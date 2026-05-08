//! Async TCP client for the MeshCore companion-frame protocol.
//!
//! # Connection model
//!
//! [`CompanionClient`] maintains a persistent connection to a
//! `CompanionFrameServer` (typically `pymc_core`'s TCP bridge).  Internally
//! it runs a background Tokio task that owns the socket and handles
//! reconnection automatically.
//!
//! ```text
//! ┌─────────────────────────────────────────┐
//! │             CompanionClient              │
//! │  cmd_tx ──► [channel] ──► worker task   │
//! │  event_rx ◄── [channel] ◄── worker task │
//! └─────────────────────────────────────────┘
//!              │                │
//!            write            read
//!              └──► TCP socket ◄┘
//! ```
//!
//! # Lifecycle
//!
//! 1. Call [`CompanionClient::connect`] — spawns the background task and
//!    returns immediately.  The task begins connecting in the background.
//! 2. Poll [`CompanionClient::recv`] (or integrate the event stream into a
//!    `select!` loop) to consume [`ClientEvent`]s.
//! 3. Call [`CompanionClient::send`] to dispatch commands.  Sends are
//!    fire-and-forget from the caller's perspective; the worker serialises and
//!    writes them over TCP.
//! 4. Drop the client to trigger a clean shutdown.  The background task exits
//!    once it detects that the command channel is closed.
//!
//! # Reconnection
//!
//! On any TCP error the worker emits [`ClientEvent::Disconnected`] with
//! `will_retry: true`, sleeps for a backoff period, then reconnects.
//! The backoff starts at [`ClientConfig::reconnect_delay_initial`] and
//! doubles after each failed attempt, capped at
//! [`ClientConfig::reconnect_delay_max`].
//!
//! # Handshake
//!
//! After each successful TCP connect the worker sends
//! [`OutboundFrame::AppStart`] and expects [`InboundFrame::SelfInfo`] as the
//! very first response.  A [`ClientEvent::Connected`] carrying the
//! [`SelfInfo`] is emitted once the handshake succeeds.  Subsequent frames
//! are forwarded as [`ClientEvent::Frame`].

use std::{io, net::SocketAddr, time::Duration};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
    time::sleep,
};
use tracing::{debug, info, warn};

use crate::{
    constants::{FRAME_OUTBOUND_PREFIX, MAX_PAYLOAD_SIZE},
    decode_inbound,
    encode_outbound,
    error::FrameDecodeError,
    frame::{InboundFrame, OutboundFrame},
    types::SelfInfo,
};

// ── Public types ──────────────────────────────────────────────────────────────

/// Configuration for [`CompanionClient`].
///
/// Construct via [`ClientConfig::new`] or use [`Default`] for the standard
/// BBS defaults.
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Address of the `CompanionFrameServer` TCP listener.
    pub addr: SocketAddr,

    /// Protocol version code sent in the [`OutboundFrame::AppStart`]
    /// handshake.  Use [`crate::constants::APP_TARGET_VER_V3`] unless you
    /// have a specific reason to request an older format.
    pub app_target_version: u8,

    /// Delay before the first reconnect attempt after a disconnect.
    ///
    /// Subsequent attempts double this value, capped at
    /// [`Self::reconnect_delay_max`].
    pub reconnect_delay_initial: Duration,

    /// Maximum delay between reconnect attempts.
    pub reconnect_delay_max: Duration,
}

impl ClientConfig {
    /// Create a config with default reconnect timings for the given address.
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            app_target_version: crate::constants::APP_TARGET_VER_V3,
            reconnect_delay_initial: Duration::from_secs(1),
            reconnect_delay_max: Duration::from_secs(60),
        }
    }
}

/// Events emitted by [`CompanionClient`].
///
/// Callers should handle all variants; unrecognised frame types are surfaced
/// as [`InboundFrame::Unknown`] inside [`ClientEvent::Frame`] rather than
/// being silently dropped.
#[derive(Debug)]
pub enum ClientEvent {
    /// The TCP connection is up and the AppStart handshake succeeded.
    ///
    /// Carries the [`SelfInfo`] returned by the radio bridge, which includes
    /// node identity, radio parameters, and GPS coordinates.
    Connected { self_info: SelfInfo },

    /// The TCP connection was lost or the handshake failed.
    ///
    /// When `will_retry` is `true` the worker is sleeping before the next
    /// reconnect attempt.  When `false` the client is shutting down
    /// (caller dropped the handle).
    Disconnected { will_retry: bool },

    /// A frame received from the radio bridge.
    Frame(InboundFrame),
}

/// Error returned by [`CompanionClient::send`] when the background worker has
/// exited (i.e. the client was dropped or the runtime shut down).
#[derive(Debug, thiserror::Error)]
#[error("companion client worker has exited; cannot send frame")]
pub struct SendError(pub OutboundFrame);

/// Async handle to a persistent MeshCore companion connection.
///
/// Cheaply cloneable via the inner channels.  Dropping the last clone shuts
/// down the background worker.
///
/// # Example
///
/// ```no_run
/// use std::net::SocketAddr;
/// use meshcore_companion::client::{ClientConfig, ClientEvent, CompanionClient};
/// use meshcore_companion::frame::OutboundFrame;
///
/// #[tokio::main]
/// async fn main() {
///     let addr: SocketAddr = "127.0.0.1:5000".parse().unwrap();
///     let mut client = CompanionClient::connect(ClientConfig::new(addr));
///
///     while let Some(event) = client.recv().await {
///         match event {
///             ClientEvent::Connected { self_info } => {
///                 println!("connected as {}", self_info.node_name);
///                 client.send(OutboundFrame::GetBattAndStorage).await.ok();
///             }
///             ClientEvent::Frame(frame) => println!("frame: {frame:?}"),
///             ClientEvent::Disconnected { will_retry } => {
///                 println!("disconnected (retry={will_retry})");
///             }
///         }
///     }
/// }
/// ```
pub struct CompanionClient {
    cmd_tx: mpsc::Sender<OutboundFrame>,
    event_rx: mpsc::Receiver<ClientEvent>,
}

impl CompanionClient {
    /// Spawn the background worker and return a client handle.
    ///
    /// The worker begins connecting immediately.  This call never blocks; the
    /// first [`ClientEvent::Connected`] or [`ClientEvent::Disconnected`]
    /// arrives via [`Self::recv`] once the connection attempt completes.
    pub fn connect(config: ClientConfig) -> Self {
        // 32-command buffer: generous for BBS use cases; back-pressure kicks
        // in if the socket stalls (e.g. during a reconnect).
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        // 64-event buffer: radio can burst frames (contact sync, messages).
        let (event_tx, event_rx) = mpsc::channel(64);

        tokio::spawn(run_worker(config, cmd_rx, event_tx));

        Self { cmd_tx, event_rx }
    }

    /// Send a command to the radio bridge.
    ///
    /// Returns `Ok(())` once the command is queued; actual transmission
    /// happens asynchronously.  If the worker has exited (client dropped or
    /// runtime shut down) returns [`SendError`] carrying the frame back.
    ///
    /// Back-pressure: if the command channel is full this awaits until space
    /// is available.
    pub async fn send(&self, frame: OutboundFrame) -> Result<(), SendError> {
        self.cmd_tx.send(frame).await.map_err(|e| SendError(e.0))
    }

    /// Receive the next [`ClientEvent`] from the radio bridge.
    ///
    /// Returns `None` when the background worker has exited (all senders
    /// dropped), signalling that the client is shut down.
    pub async fn recv(&mut self) -> Option<ClientEvent> {
        self.event_rx.recv().await
    }

    /// Non-blocking variant of [`Self::recv`].
    ///
    /// Returns `Ok(event)` if one is immediately available, or an
    /// [`mpsc::error::TryRecvError`] otherwise.
    pub fn try_recv(&mut self) -> Result<ClientEvent, mpsc::error::TryRecvError> {
        self.event_rx.try_recv()
    }

    /// Clone the outbound command sender.
    ///
    /// Useful when the event-receiving side (i.e. `recv()`) is moved into a
    /// background task while the caller still needs to enqueue commands — for
    /// example in a plugin that holds `CompanionClient` in a worker task but
    /// needs to send frames from a `notify()` call on a different code path.
    ///
    /// Senders are cheap to clone and share; they do not need to be exclusive.
    pub fn sender(&self) -> mpsc::Sender<OutboundFrame> {
        self.cmd_tx.clone()
    }
}

// ── Background worker ─────────────────────────────────────────────────────────

/// Root task: reconnect loop with exponential backoff.
///
/// Exits cleanly when `cmd_rx` is closed (caller dropped the client) or when
/// `event_tx` is closed (caller dropped the receiver — unusual but handled).
async fn run_worker(
    config: ClientConfig,
    mut cmd_rx: mpsc::Receiver<OutboundFrame>,
    event_tx: mpsc::Sender<ClientEvent>,
) {
    let mut backoff = config.reconnect_delay_initial;

    loop {
        debug!(addr = %config.addr, "companion: attempting TCP connect");
        match attempt_session(&config, &mut cmd_rx, &event_tx).await {
            SessionOutcome::Shutdown => {
                info!("companion: clean shutdown");
                break;
            }
            SessionOutcome::IoError(e) => {
                warn!("companion: session ended with error: {e}");
                // Notify callers; ignore send error (they may have dropped recv).
                let _ = event_tx.send(ClientEvent::Disconnected { will_retry: true }).await;
                debug!("companion: reconnecting in {backoff:?}");
                sleep(backoff).await;
                backoff = (backoff * 2).min(config.reconnect_delay_max);
            }
        }
    }

    // Tell callers we're done (will_retry=false distinguishes clean exit from
    // a transient disconnect).
    let _ = event_tx.send(ClientEvent::Disconnected { will_retry: false }).await;
}

/// Outcome of a single connection attempt + session.
enum SessionOutcome {
    /// `cmd_rx` or `event_tx` closed — time to exit the reconnect loop.
    Shutdown,
    /// TCP or protocol error — reconnect after backoff.
    IoError(io::Error),
}

/// Run one full session: connect → handshake → event loop.
///
/// Returns when the session ends for any reason.
async fn attempt_session(
    config: &ClientConfig,
    cmd_rx: &mut mpsc::Receiver<OutboundFrame>,
    event_tx: &mpsc::Sender<ClientEvent>,
) -> SessionOutcome {
    // ── TCP connect ──────────────────────────────────────────────────────────
    let stream = match TcpStream::connect(config.addr).await {
        Ok(s) => s,
        Err(e) => return SessionOutcome::IoError(e),
    };
    info!(addr = %config.addr, "companion: TCP connected");

    // Disable Nagle: companion frames are small, latency matters more than
    // throughput.
    if let Err(e) = stream.set_nodelay(true) {
        warn!("companion: could not set TCP_NODELAY: {e}");
    }

    let (mut reader, mut writer) = stream.into_split();

    // ── Handshake ────────────────────────────────────────────────────────────
    let handshake = encode_outbound(&OutboundFrame::AppStart {
        app_target_version: config.app_target_version,
    });
    if let Err(e) = writer.write_all(&handshake).await {
        return SessionOutcome::IoError(e);
    }

    let self_info = match read_frame(&mut reader).await {
        Err(e) => return SessionOutcome::IoError(e),
        Ok(InboundFrame::SelfInfo(info)) => info,
        Ok(other) => {
            warn!("companion: expected SelfInfo after AppStart, got {other:?}");
            return SessionOutcome::IoError(io::Error::new(
                io::ErrorKind::InvalidData,
                "no SelfInfo after AppStart handshake",
            ));
        }
    };

    info!(node = %self_info.node_name, "companion: handshake complete");
    if event_tx.send(ClientEvent::Connected { self_info }).await.is_err() {
        return SessionOutcome::Shutdown;
    }

    // ── Event loop ───────────────────────────────────────────────────────────
    loop {
        tokio::select! {
            // Inbound: frame from radio bridge.
            result = read_frame(&mut reader) => {
                match result {
                    Ok(frame) => {
                        debug!("companion: rx {frame:?}");
                        if event_tx.send(ClientEvent::Frame(frame)).await.is_err() {
                            return SessionOutcome::Shutdown;
                        }
                    }
                    Err(e) => return SessionOutcome::IoError(e),
                }
            }

            // Outbound: command from the caller.
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(frame) => {
                        debug!("companion: tx {frame:?}");
                        let wire = encode_outbound(&frame);
                        if let Err(e) = writer.write_all(&wire).await {
                            return SessionOutcome::IoError(e);
                        }
                    }
                    None => return SessionOutcome::Shutdown,
                }
            }
        }
    }
}

// ── Frame reader ──────────────────────────────────────────────────────────────

/// Read one complete frame from `reader`.
///
/// Reads the 3-byte header (prefix + LE u16 length), validates the prefix,
/// reads the payload, then decodes it.  Returns an `io::Error` on any TCP
/// error or protocol violation so the caller can treat all failures uniformly.
async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<InboundFrame> {
    // Read and validate the 3-byte header.
    let mut header = [0u8; 3];
    reader.read_exact(&mut header).await?;

    if header[0] != FRAME_OUTBOUND_PREFIX {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "companion: bad frame prefix 0x{:02X} (expected 0x{FRAME_OUTBOUND_PREFIX:02X})",
                header[0]
            ),
        ));
    }

    let payload_len = u16::from_le_bytes([header[1], header[2]]) as usize;
    if payload_len > MAX_PAYLOAD_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("companion: payload length {payload_len} exceeds MAX_PAYLOAD_SIZE ({MAX_PAYLOAD_SIZE})"),
        ));
    }

    // Read the payload.
    let mut payload = vec![0u8; payload_len];
    reader.read_exact(&mut payload).await?;

    // Decode.
    decode_inbound(&payload).map_err(|e: FrameDecodeError| {
        io::Error::new(io::ErrorKind::InvalidData, e.to_string())
    })
}
