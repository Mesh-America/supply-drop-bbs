//! Async client for the MeshCore companion-frame protocol.
//!
//! Supports two transports:
//!
//! - **TCP** ([`CompanionClient::connect`]) — connects to a
//!   `CompanionFrameServer`, typically `pymc_core`'s TCP bridge.  Used in
//!   HAT and standalone TCP deployments.
//!
//! - **Serial** ([`CompanionClient::connect_serial`]) — opens a local USB
//!   serial port (e.g. a Heltec V3 or T-Beam).  The companion-frame protocol
//!   is byte-stream-agnostic; the same codec runs over both transports.
//!
//! # Connection model
//!
//! [`CompanionClient`] is a channel-based handle.  The actual I/O runs in a
//! background Tokio task that owns the stream and handles reconnection.
//!
//! ```text
//! ┌─────────────────────────────────────────────┐
//! │               CompanionClient               │
//! │  cmd_tx ──► [channel] ──► background task  │
//! │  event_rx ◄── [channel] ◄── background task │
//! └─────────────────────────────────────────────┘
//!                │                  │
//!              write               read
//!                └──► stream (TCP or serial) ◄┘
//! ```
//!
//! # Lifecycle
//!
//! 1. Call [`CompanionClient::connect`] or [`CompanionClient::connect_serial`]
//!    — spawns the background task and returns immediately.
//! 2. Poll [`CompanionClient::recv`] to consume [`ClientEvent`]s.
//! 3. Send outbound frames via [`CompanionClient::send`] or
//!    [`CompanionClient::sender`].
//! 4. Drop the client to signal a clean shutdown.
//!
//! # Reconnection
//!
//! On any I/O error the worker emits [`ClientEvent::Disconnected`] with
//! `will_retry: true`, sleeps for a backoff period (exponential, capped), then
//! reconnects.  The backoff parameters are set on [`ClientConfig`] and
//! [`SerialConfig`].
//!
//! # Handshake
//!
//! After each successful connection the worker sends
//! [`OutboundFrame::AppStart`] and expects [`InboundFrame::SelfInfo`] as the
//! first response.  A [`ClientEvent::Connected`] carrying the [`SelfInfo`] is
//! emitted once the handshake completes.

use std::{io, net::SocketAddr, time::Duration};

use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    sync::mpsc,
    time::sleep,
};
use tokio_serial::SerialPortBuilderExt;
use tracing::{debug, info, warn};

use crate::{
    constants::{FRAME_OUTBOUND_PREFIX, MAX_PAYLOAD_SIZE},
    decode_inbound, encode_outbound,
    error::FrameDecodeError,
    frame::{InboundFrame, OutboundFrame},
    types::SelfInfo,
};

// ── Public types ──────────────────────────────────────────────────────────────

/// Configuration for a TCP [`CompanionClient`].
#[derive(Debug, Clone)]
pub struct ClientConfig {
    /// Address of the `CompanionFrameServer` TCP listener.
    pub addr: SocketAddr,

    /// Protocol version code sent in the [`OutboundFrame::AppStart`] handshake.
    /// Use [`crate::constants::APP_TARGET_VER_V3`] unless you have a specific
    /// reason to request an older format.
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

/// Configuration for a serial [`CompanionClient`].
#[derive(Debug, Clone)]
pub struct SerialConfig {
    /// OS path to the serial device.
    ///
    /// Examples: `/dev/ttyACM0` (Linux), `/dev/tty.usbmodem*` (macOS),
    /// `COM3` (Windows).
    pub port: String,

    /// Baud rate. MeshCore USB companion devices default to 115 200.
    pub baud_rate: u32,

    /// Protocol version code sent in the [`OutboundFrame::AppStart`] handshake.
    pub app_target_version: u8,

    /// Delay before the first reconnect attempt after a port error.
    pub reconnect_delay_initial: Duration,

    /// Maximum delay between reconnect attempts.
    pub reconnect_delay_max: Duration,
}

/// Events emitted by [`CompanionClient`].
///
/// Callers should handle all variants; unrecognised frame types surface as
/// [`InboundFrame::Unknown`] inside [`ClientEvent::Frame`].
#[derive(Debug)]
pub enum ClientEvent {
    /// The connection is up and the AppStart handshake succeeded.
    ///
    /// Carries the [`SelfInfo`] returned by the device, which includes node
    /// identity, radio parameters, and GPS coordinates.
    Connected { self_info: SelfInfo },

    /// The connection was lost or the handshake failed.
    ///
    /// When `will_retry` is `true` the worker is sleeping before the next
    /// reconnect attempt.  When `false` the client is shutting down (caller
    /// dropped the handle).
    Disconnected { will_retry: bool },

    /// A frame received from the device.
    Frame(InboundFrame),
}

/// Error returned by [`CompanionClient::send`] when the background worker has
/// exited.
#[derive(Debug, thiserror::Error)]
#[error("companion client worker has exited; cannot send frame")]
pub struct SendError(pub OutboundFrame);

/// Async handle to a persistent MeshCore companion connection.
///
/// Construct via [`CompanionClient::connect`] (TCP) or
/// [`CompanionClient::connect_serial`] (USB serial).  The type is
/// transport-agnostic after construction: both transports produce the same
/// [`ClientEvent`] stream and accept the same [`OutboundFrame`] commands.
///
/// # Example (TCP)
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
///                 println!("connected: {}", self_info.node_name);
///                 client.send(OutboundFrame::GetBattAndStorage).await.ok();
///             }
///             ClientEvent::Frame(frame) => println!("{frame:?}"),
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
    /// Spawn a TCP background worker and return a client handle.
    ///
    /// The worker begins connecting immediately.  This call never blocks; the
    /// first [`ClientEvent::Connected`] or [`ClientEvent::Disconnected`]
    /// arrives via [`Self::recv`] once the connection attempt completes.
    pub fn connect(config: ClientConfig) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let (event_tx, event_rx) = mpsc::channel(64);
        tokio::spawn(run_tcp_worker(config, cmd_rx, event_tx));
        Self { cmd_tx, event_rx }
    }

    /// Spawn a USB serial background worker and return a client handle.
    ///
    /// The worker opens the serial port immediately.  If the port is
    /// unavailable, it retries with exponential backoff (same model as the TCP
    /// reconnect loop).
    pub fn connect_serial(config: SerialConfig) -> Self {
        let (cmd_tx, cmd_rx) = mpsc::channel(32);
        let (event_tx, event_rx) = mpsc::channel(64);
        tokio::spawn(run_serial_worker(config, cmd_rx, event_tx));
        Self { cmd_tx, event_rx }
    }

    /// Send a command to the device.
    ///
    /// Returns `Ok(())` once the command is queued; transmission is
    /// asynchronous.  Returns [`SendError`] if the worker has exited.
    pub async fn send(&self, frame: OutboundFrame) -> Result<(), SendError> {
        self.cmd_tx.send(frame).await.map_err(|e| SendError(e.0))
    }

    /// Receive the next [`ClientEvent`].
    ///
    /// Returns `None` when the background worker has exited.
    pub async fn recv(&mut self) -> Option<ClientEvent> {
        self.event_rx.recv().await
    }

    /// Non-blocking variant of [`Self::recv`].
    pub fn try_recv(&mut self) -> Result<ClientEvent, mpsc::error::TryRecvError> {
        self.event_rx.try_recv()
    }

    /// Clone the outbound command sender.
    ///
    /// Useful when the receiving side is moved into a background task while
    /// the caller still needs to enqueue commands from a different code path
    /// (e.g. a plugin's `notify()` method).
    pub fn sender(&self) -> mpsc::Sender<OutboundFrame> {
        self.cmd_tx.clone()
    }
}

// ── TCP worker ────────────────────────────────────────────────────────────────

async fn run_tcp_worker(
    config: ClientConfig,
    mut cmd_rx: mpsc::Receiver<OutboundFrame>,
    event_tx: mpsc::Sender<ClientEvent>,
) {
    let mut backoff = config.reconnect_delay_initial;

    loop {
        debug!(addr = %config.addr, "companion/tcp: connecting");
        match attempt_tcp_session(&config, &mut cmd_rx, &event_tx).await {
            SessionOutcome::Shutdown => {
                info!("companion/tcp: clean shutdown");
                break;
            }
            SessionOutcome::IoError(e, session_ran) => {
                warn!("companion/tcp: session error: {e}");
                let _ = event_tx
                    .send(ClientEvent::Disconnected { will_retry: true })
                    .await;
                if session_ran {
                    // A real session ran before this error; reset the backoff
                    // so a brief hiccup doesn't impose the saturated maximum
                    // delay on the very next reconnect attempt.
                    backoff = config.reconnect_delay_initial;
                }
                debug!("companion/tcp: reconnecting in {backoff:?}");
                sleep(backoff).await;
                backoff = (backoff * 2).min(config.reconnect_delay_max);
            }
        }
    }

    let _ = event_tx
        .send(ClientEvent::Disconnected { will_retry: false })
        .await;
}

async fn attempt_tcp_session(
    config: &ClientConfig,
    cmd_rx: &mut mpsc::Receiver<OutboundFrame>,
    event_tx: &mpsc::Sender<ClientEvent>,
) -> SessionOutcome {
    let stream = match TcpStream::connect(config.addr).await {
        Ok(s) => s,
        Err(e) => return SessionOutcome::IoError(e, false),
    };
    info!(addr = %config.addr, "companion/tcp: connected");

    // Disable Nagle: companion frames are small, latency matters more than
    // throughput.
    if let Err(e) = stream.set_nodelay(true) {
        warn!("companion/tcp: could not set TCP_NODELAY: {e}");
    }

    let (mut reader, mut writer) = stream.into_split();
    match run_session(
        &mut reader,
        &mut writer,
        config.app_target_version,
        cmd_rx,
        event_tx,
    )
    .await
    {
        SessionOutcome::IoError(e, _) => SessionOutcome::IoError(e, true),
        other => other,
    }
}

// ── Serial worker ─────────────────────────────────────────────────────────────

async fn run_serial_worker(
    config: SerialConfig,
    mut cmd_rx: mpsc::Receiver<OutboundFrame>,
    event_tx: mpsc::Sender<ClientEvent>,
) {
    let mut backoff = config.reconnect_delay_initial;

    loop {
        debug!(port = %config.port, baud = config.baud_rate, "companion/serial: opening port");
        match attempt_serial_session(&config, &mut cmd_rx, &event_tx).await {
            SessionOutcome::Shutdown => {
                info!("companion/serial: clean shutdown");
                break;
            }
            SessionOutcome::IoError(e, session_ran) => {
                warn!("companion/serial: session error: {e}");
                let _ = event_tx
                    .send(ClientEvent::Disconnected { will_retry: true })
                    .await;
                if session_ran {
                    // A real session ran before this error; reset the backoff
                    // so a brief hiccup doesn't impose the saturated maximum
                    // delay on the very next reconnect attempt.
                    backoff = config.reconnect_delay_initial;
                }
                debug!("companion/serial: reopening in {backoff:?}");
                sleep(backoff).await;
                backoff = (backoff * 2).min(config.reconnect_delay_max);
            }
        }
    }

    let _ = event_tx
        .send(ClientEvent::Disconnected { will_retry: false })
        .await;
}

async fn attempt_serial_session(
    config: &SerialConfig,
    cmd_rx: &mut mpsc::Receiver<OutboundFrame>,
    event_tx: &mpsc::Sender<ClientEvent>,
) -> SessionOutcome {
    let stream = match tokio_serial::new(&config.port, config.baud_rate).open_native_async() {
        Ok(s) => s,
        Err(e) => {
            return SessionOutcome::IoError(
                io::Error::other(format!("could not open serial port {}: {e}", config.port)),
                false,
            );
        }
    };
    info!(port = %config.port, baud = config.baud_rate, "companion/serial: port opened");

    let (mut reader, mut writer) = tokio::io::split(stream);
    match run_session(
        &mut reader,
        &mut writer,
        config.app_target_version,
        cmd_rx,
        event_tx,
    )
    .await
    {
        SessionOutcome::IoError(e, _) => SessionOutcome::IoError(e, true),
        other => other,
    }
}

// ── Shared session logic ──────────────────────────────────────────────────────

/// Outcome of a single connection attempt + session.
enum SessionOutcome {
    /// Command channel or event channel closed — exit the reconnect loop.
    Shutdown,
    /// I/O or protocol error — reconnect after backoff.
    ///
    /// The `bool` is `true` when the transport was successfully opened before
    /// the error occurred (i.e. a real session ran), and `false` when the
    /// connection attempt itself failed.  The reconnect loop resets the backoff
    /// counter to its initial value in the former case so that a brief hiccup
    /// after a long-lived session does not impose the maximum retry delay.
    IoError(io::Error, bool),
}

/// Handshake + event loop shared by TCP and serial sessions.
///
/// Works for any `AsyncRead`/`AsyncWrite` pair.  Returns when the session
/// ends for any reason.
async fn run_session<R, W>(
    reader: &mut R,
    writer: &mut W,
    app_target_version: u8,
    cmd_rx: &mut mpsc::Receiver<OutboundFrame>,
    event_tx: &mpsc::Sender<ClientEvent>,
) -> SessionOutcome
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // ── AppStart handshake ────────────────────────────────────────────────────
    let handshake = encode_outbound(&OutboundFrame::AppStart { app_target_version });
    if let Err(e) = writer.write_all(&handshake).await {
        return SessionOutcome::IoError(e, false);
    }

    let self_info = match read_frame(reader).await {
        Err(e) => return SessionOutcome::IoError(e, false),
        Ok(InboundFrame::SelfInfo(info)) => info,
        Ok(other) => {
            warn!("companion: expected SelfInfo after AppStart, got {other:?}");
            return SessionOutcome::IoError(
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "no SelfInfo after AppStart handshake",
                ),
                false,
            );
        }
    };

    info!(node = %self_info.node_name, "companion: handshake complete");
    if event_tx
        .send(ClientEvent::Connected { self_info })
        .await
        .is_err()
    {
        return SessionOutcome::Shutdown;
    }

    // ── Event loop ────────────────────────────────────────────────────────────
    loop {
        tokio::select! {
            result = read_frame(reader) => {
                match result {
                    Ok(frame) => {
                        debug!("companion: rx {frame:?}");
                        if event_tx.send(ClientEvent::Frame(frame)).await.is_err() {
                            return SessionOutcome::Shutdown;
                        }
                    }
                    Err(e) => return SessionOutcome::IoError(e, false),
                }
            }

            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(frame) => {
                        debug!("companion: tx {frame:?}");
                        let wire = encode_outbound(&frame);
                        if let Err(e) = writer.write_all(&wire).await {
                            return SessionOutcome::IoError(e, false);
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
/// reads the payload, then decodes.  Returns `io::Error` on any failure so
/// the caller can treat all session errors uniformly.
async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<InboundFrame> {
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
            format!(
                "companion: payload length {payload_len} exceeds MAX_PAYLOAD_SIZE ({MAX_PAYLOAD_SIZE})"
            ),
        ));
    }

    let mut payload = vec![0u8; payload_len];
    reader.read_exact(&mut payload).await?;

    decode_inbound(&payload)
        .map_err(|e: FrameDecodeError| io::Error::new(io::ErrorKind::InvalidData, e.to_string()))
}
