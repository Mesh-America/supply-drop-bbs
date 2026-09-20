//! Async client for the MeshCore companion-frame protocol.
//!
//! Supports two transports:
//!
//! - **TCP** ([`CompanionClient::connect`]) — connects to a
//!   `CompanionFrameServer`, typically `openhop_core`'s TCP bridge.  Used in
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
    time::{sleep, timeout},
};
use tokio_serial::SerialPortBuilderExt;
use tracing::{debug, info, trace, warn};

use crate::{
    constants::{ERR_CODE_UNSUPPORTED_CMD, FRAME_OUTBOUND_PREFIX, MAX_PAYLOAD_SIZE},
    decode_inbound, encode_outbound,
    error::FrameDecodeError,
    frame::{InboundFrame, OutboundFrame},
    scope::FloodScope,
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

    /// A flood scope to set as the radio's default before [`ClientEvent::Connected`]
    /// is emitted, so the first thing the application does on a connection (an
    /// advert, say) already uses it. Sent once per radio (by public key) while
    /// the client runs; see [`FloodScope`]. `None` leaves the radio's scope alone.
    pub default_flood_scope: Option<FloodScope>,
}

impl ClientConfig {
    /// Create a config with default reconnect timings for the given address.
    pub fn new(addr: SocketAddr) -> Self {
        Self {
            addr,
            app_target_version: crate::constants::APP_TARGET_VER_V3,
            reconnect_delay_initial: Duration::from_secs(1),
            reconnect_delay_max: Duration::from_secs(60),
            default_flood_scope: None,
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

    /// See [`ClientConfig::default_flood_scope`].
    pub default_flood_scope: Option<FloodScope>,
}

/// Events emitted by [`CompanionClient`].
///
/// Callers should handle all variants; unrecognised frame types surface as
/// [`InboundFrame::Unknown`] inside [`ClientEvent::Frame`].
#[derive(Debug)]
pub enum ClientEvent {
    /// The connection is up and the AppStart handshake succeeded (or the
    /// device indicated it does not support `CMD_APP_START`).
    ///
    /// `self_info` is `Some` when the device responded to `CMD_APP_START`
    /// with a valid `SelfInfo` frame.  It is `None` when the device returned
    /// `ERR_CODE_UNSUPPORTED_CMD` for `CMD_APP_START`; in that case node
    /// identity and radio parameters are unavailable until the device pushes
    /// an advert.
    Connected { self_info: Option<SelfInfo> },

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
///                 let name = self_info.as_ref().map(|i| i.node_name.as_str()).unwrap_or("unknown");
///                 println!("connected: {name}");
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
    // The radio and scope the default flood scope was last settled for, kept
    // across reconnects so a link blip does not rewrite the radio's flash.
    let mut scope_memory = ScopeMemory::default();

    loop {
        debug!(addr = %config.addr, "companion/tcp: connecting");
        match attempt_tcp_session(&config, &mut cmd_rx, &event_tx, &mut scope_memory).await {
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
    scope_memory: &mut ScopeMemory,
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

    let (reader, mut writer) = stream.into_split();
    match run_session(
        reader,
        &mut writer,
        config.app_target_version,
        cmd_rx,
        event_tx,
        config.default_flood_scope.as_ref(),
        scope_memory,
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
    // The radio and scope the default flood scope was last settled for, kept
    // across reconnects so a link blip does not rewrite the radio's flash.
    let mut scope_memory = ScopeMemory::default();

    loop {
        debug!(port = %config.port, baud = config.baud_rate, "companion/serial: opening port");
        match attempt_serial_session(&config, &mut cmd_rx, &event_tx, &mut scope_memory).await {
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

/// Map a serial-open error to an `io::Error`, upgrading a permission-denied
/// failure to an actionable message. Access-denied on the radio's tty is a
/// common Pi/Debian gotcha: the device is group-owned by `plugdev` (or
/// `dialout`, depending on the distro) and the `supply-drop` service user is
/// not in that group.
fn serial_open_error(port: &str, e: tokio_serial::Error) -> io::Error {
    if matches!(
        e.kind(),
        tokio_serial::ErrorKind::Io(io::ErrorKind::PermissionDenied)
    ) {
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "permission denied opening serial port {port}: the service user is not in the \
                 group that owns the device. Run `ls -lL {port}` to find the owning group \
                 (commonly `dialout` or `plugdev`), add the user to it \
                 (`sudo usermod -aG <group> supply-drop`), then restart the service. \
                 See docs/OPERATIONS.md — \"Serial port: Permission denied\"."
            ),
        )
    } else {
        io::Error::other(format!("could not open serial port {port}: {e}"))
    }
}

async fn attempt_serial_session(
    config: &SerialConfig,
    cmd_rx: &mut mpsc::Receiver<OutboundFrame>,
    event_tx: &mpsc::Sender<ClientEvent>,
    scope_memory: &mut ScopeMemory,
) -> SessionOutcome {
    let stream = match tokio_serial::new(&config.port, config.baud_rate).open_native_async() {
        Ok(s) => s,
        Err(e) => {
            return SessionOutcome::IoError(serial_open_error(&config.port, e), false);
        }
    };
    info!(port = %config.port, baud = config.baud_rate, "companion/serial: port opened");

    // Give the nRF52840 firmware time to complete setup() before we send
    // AppStart.  The Linux cdc-acm driver sends SET_CONTROL_LINE_STATE(DTR=1)
    // automatically on port open (acm_port_activate), so no explicit DTR
    // assertion is needed.  What *is* needed is this delay: radio init,
    // filesystem mount, and mesh init together take ~1-2 seconds, and if we
    // send AppStart before setup() finishes the frame sits in the TinyUSB FIFO
    // safely — but it is processed correctly only once serial_interface.begin()
    // and startInterface() have been called.  2 s matches the delay used by the
    // Meshtastic serial transport for the same class of devices.
    sleep(Duration::from_secs(2)).await;

    let (reader, mut writer) = tokio::io::split(stream);
    match run_session(
        reader,
        &mut writer,
        config.app_target_version,
        cmd_rx,
        event_tx,
        config.default_flood_scope.as_ref(),
        scope_memory,
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

/// The radio and flood scope the default-scope exchange last ran for, whatever
/// its outcome. A radio that refused, or reported back a different scope, is not
/// asked again while the client runs, so it is not rewritten on every reconnect.
type ScopeMemory = Option<([u8; 32], FloodScope)>;

/// How long to wait for the radio's reply to each of the two scope commands.
const SCOPE_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// Set the radio's default flood scope and read it back, during the handshake.
///
/// This is the one place the replies can be told from anything else: nothing
/// but `AppStart` has been sent on the connection and the command queue is not
/// being read yet, so the next reply frame after each command is its answer
/// (replies carry no request id). Doing it here, before `Connected`, also means
/// the scope is set before the application sends its first command.
///
/// A radio that rejects the command (firmware without it) is logged and left as
/// it is; only I/O errors and a silent radio end the session.
async fn apply_default_flood_scope<R, W>(
    reader: &mut R,
    writer: &mut W,
    radio: [u8; 32],
    scope: &FloodScope,
    memory: &mut ScopeMemory,
) -> io::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let done = Some((radio, scope.clone()));
    if *memory == done {
        return Ok(());
    }
    info!(region = %scope.name, "companion: setting the radio's default flood scope");

    // The Set.
    writer
        .write_all(&encode_outbound(&OutboundFrame::SetDefaultFloodScope {
            scope: scope.clone(),
        }))
        .await?;
    writer.flush().await?;
    let Some(set_reply) = next_scope_reply(reader, memory, &done).await? else {
        warn!(
            region = %scope.name,
            "companion: could not read the radio's reply to the default flood scope, so \
             it is unknown whether adverts are scoped; not asked again until the BBS restarts"
        );
        *memory = done;
        return Ok(());
    };
    match set_reply {
        InboundFrame::Ok => {}
        InboundFrame::Err { error_code } => {
            warn!(
                region = %scope.name,
                error_code,
                "companion: the radio rejected the default flood scope (firmware without \
                 the command?), so its adverts are not scoped as configured; not asked \
                 again until the BBS restarts"
            );
            *memory = done;
            return Ok(());
        }
        other => {
            debug!("companion: unexpected reply to the default flood scope: {other:?}");
        }
    }

    // The Get: what the radio kept, and its key.
    writer
        .write_all(&encode_outbound(&OutboundFrame::GetDefaultFloodScope))
        .await?;
    writer.flush().await?;
    let reply = next_scope_reply(reader, memory, &done).await?;
    *memory = done;
    let Some(reply) = reply else {
        debug!("companion: could not decode the radio's default flood scope read-back");
        return Ok(());
    };
    match reply {
        InboundFrame::DefaultFloodScope(Some(seen)) if seen == *scope => {
            let key: String = seen.key.iter().map(|b| format!("{b:02x}")).collect();
            info!(region = %seen.name, key = %key, "companion: radio default flood scope set");
        }
        InboundFrame::DefaultFloodScope(seen) => warn!(
            sent = %scope.name,
            radio = seen.as_ref().map_or("(none)", |s| s.name.as_str()),
            "companion: the radio did not keep the default flood scope that was set, \
             so its adverts are not scoped as configured; not asked again until the \
             BBS restarts"
        ),
        other => debug!(
            "companion: could not read the default flood scope back ({other:?}); \
             the Set was accepted"
        ),
    }
    Ok(())
}

/// A frame that arrived whole but did not decode. Kept apart from other
/// `InvalidData` errors (a bad length) because the reader is still in step after
/// it, so a caller that can carry on may.
#[derive(Debug)]
struct UndecodableFrame(String);

impl std::fmt::Display for UndecodableFrame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UndecodableFrame {}

fn is_undecodable_frame(e: &io::Error) -> bool {
    e.get_ref()
        .is_some_and(|inner| inner.is::<UndecodableFrame>())
}

/// The next solicited reply (`Ok`, `Err` or the scope answer), discarding the
/// unsolicited push frames a busy radio interleaves, as the handshake does.
/// `None` if a frame came in that could not be decoded: the reader is still in
/// step, but whether it was the reply is unknown, so the caller stops the
/// exchange rather than guess. If the radio stays silent the exchange is
/// recorded as settled (so the next connection does not repeat it) and the
/// session ends with a timeout: the reader may be part-way through a frame and
/// cannot safely be reused.
async fn next_scope_reply<R>(
    reader: &mut R,
    memory: &mut ScopeMemory,
    done: &ScopeMemory,
) -> io::Result<Option<InboundFrame>>
where
    R: AsyncRead + Unpin,
{
    let read = timeout(SCOPE_REPLY_TIMEOUT, async {
        loop {
            match read_frame(reader).await {
                Ok(
                    f @ (InboundFrame::Ok
                    | InboundFrame::Err { .. }
                    | InboundFrame::DefaultFloodScope(_)),
                ) => return Ok(Some(f)),
                // Discarded, as in the AppStart handshake. Visible at debug in
                // case a message frame is among them.
                Ok(other) => {
                    debug!("companion: discarding frame during flood scope setup: {other:?}");
                }
                Err(e) if is_undecodable_frame(&e) => return Ok(None),
                Err(e) => return Err(e),
            }
        }
    })
    .await;
    match read {
        Ok(result) => result,
        Err(_) => {
            warn!(
                "companion: the radio did not answer the default flood scope command; \
                 giving up on it until the BBS restarts"
            );
            *memory = done.clone();
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "no reply to the default flood scope command",
            ))
        }
    }
}

/// Handshake + event loop shared by TCP and serial sessions.
///
/// Works for any `AsyncRead`/`AsyncWrite` pair.  Returns when the session
/// ends for any reason.
///
/// If the device responds to `CMD_APP_START` with `ERR_CODE_UNSUPPORTED_CMD`
/// the handshake is considered successful with no `SelfInfo`;
/// [`ClientEvent::Connected`] carries `None`.
async fn run_session<R, W>(
    reader: R,
    writer: &mut W,
    app_target_version: u8,
    cmd_rx: &mut mpsc::Receiver<OutboundFrame>,
    event_tx: &mpsc::Sender<ClientEvent>,
    default_flood_scope: Option<&FloodScope>,
    scope_memory: &mut ScopeMemory,
) -> SessionOutcome
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    // ── AppStart handshake ────────────────────────────────────────────────────
    let handshake = encode_outbound(&OutboundFrame::AppStart { app_target_version });
    trace!(
        "companion: tx AppStart ({} bytes): {:02X?}",
        handshake.len(),
        handshake
    );
    if let Err(e) = writer.write_all(&handshake).await {
        return SessionOutcome::IoError(e, false);
    }
    // Flush to ensure the frame reaches the device before we start reading.
    // USB CDC drivers may batch small writes; an explicit flush forces immediate
    // transmission, which matters for devices that won't respond until they
    // receive the full AppStart frame.
    if let Err(e) = writer.flush().await {
        warn!("companion: flush after AppStart failed: {e}");
    }

    // Wait up to 10 seconds for SelfInfo, discarding any unsolicited push
    // frames (LogRxData, Advert, etc.) that arrive before the device processes
    // AppStart.  Active nodes relay mesh traffic continuously, so several
    // frames may arrive before SelfInfo is queued in the firmware's TX buffer.
    //
    // The handshake loop uses a sequential await inside `timeout`, NOT
    // `tokio::select!` — so it is cancel-safe as-is.  We borrow reader
    // mutably here and take ownership for the spawned task after.
    let mut reader = reader;
    const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
    let self_info: Option<SelfInfo> = match timeout(HANDSHAKE_TIMEOUT, async {
        loop {
            match read_frame(&mut reader).await {
                Err(e) => return Err(e),
                Ok(InboundFrame::SelfInfo(info)) => return Ok(Some(info)),
                // Some devices return UNSUPPORTED_CMD for CMD_APP_START.  Treat
                // this as a handshake-less connection and proceed without SelfInfo.
                Ok(InboundFrame::Err { error_code }) if error_code == ERR_CODE_UNSUPPORTED_CMD => {
                    warn!(
                        "companion: device returned UNSUPPORTED_CMD for CMD_APP_START \
                         — proceeding without SelfInfo; \
                         node identity will be unavailable until the first advert is received"
                    );
                    return Ok(None);
                }
                // Any other frame (LogRxData, Advert, PathUpdated, …) arrived
                // before SelfInfo — the device is busy relaying mesh traffic.
                // Discard and keep waiting.
                Ok(other) => {
                    trace!("companion: discarding pre-handshake frame: {other:?}");
                }
            }
        }
    })
    .await
    {
        Ok(Ok(info)) => info,
        Ok(Err(e)) => return SessionOutcome::IoError(e, false),
        Err(_) => {
            warn!(
                "companion: handshake timeout ({HANDSHAKE_TIMEOUT:?}) — \
                 device did not respond to AppStart; \
                 check connection type, baud rate, and that the device is \
                 running the MeshCore USB companion firmware"
            );
            return SessionOutcome::IoError(
                io::Error::new(io::ErrorKind::TimedOut, "AppStart handshake timeout"),
                false,
            );
        }
    };

    if let Some(ref info) = self_info {
        info!(node = %info.node_name, "companion: handshake complete");
    }
    // Set the default flood scope before the application hears `Connected` (and
    // so before it sends anything, the on-connect advert included). Skipped on a
    // radio that gave no SelfInfo, since the exchange is keyed by its public key.
    if let (Some(info), Some(scope)) = (&self_info, default_flood_scope) {
        if let Err(e) =
            apply_default_flood_scope(&mut reader, writer, info.pubkey, scope, scope_memory).await
        {
            return SessionOutcome::IoError(e, false);
        }
    }

    if default_flood_scope.is_some() && self_info.is_none() {
        warn!(
            "companion: the radio gave no SelfInfo, so the default flood scope was not \
             set: its adverts are not scoped as configured"
        );
    }
    if event_tx
        .send(ClientEvent::Connected { self_info })
        .await
        .is_err()
    {
        return SessionOutcome::Shutdown;
    }

    // ── Event loop ────────────────────────────────────────────────────────────
    //
    // read_frame calls read_exact multiple times and is not cancel-safe.
    // When another tokio::select! branch wins, partially-consumed frame bytes
    // are abandoned, desynchronising the stream and corrupting all subsequent
    // frames (SYN-38).
    //
    // Fix: a dedicated task owns the reader and delivers complete frames over
    // an mpsc channel.  mpsc::Receiver::recv() IS cancel-safe, so the event
    // loop can safely select over `frame_rx`.
    let (frame_tx, mut frame_rx) = mpsc::channel::<io::Result<InboundFrame>>(8);
    tokio::spawn(async move {
        loop {
            let frame = read_frame(&mut reader).await;
            let is_err = frame.is_err();
            if frame_tx.send(frame).await.is_err() {
                break; // event loop exited; stop reading
            }
            if is_err {
                break; // I/O error forwarded; let the event loop reconnect
            }
        }
    });

    loop {
        tokio::select! {
            result = frame_rx.recv() => {
                match result {
                    Some(Ok(frame)) => {
                        trace!("companion: rx {frame:?}");
                        if event_tx.send(ClientEvent::Frame(frame)).await.is_err() {
                            return SessionOutcome::Shutdown;
                        }
                    }
                    Some(Err(e)) => return SessionOutcome::IoError(e, false),
                    None => return SessionOutcome::IoError(
                        io::Error::new(io::ErrorKind::BrokenPipe, "reader task exited unexpectedly"),
                        false,
                    ),
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
/// Scans the byte stream for the device→host prefix (`0x3E`, `>`), skipping
/// any bytes that don't match.  This tolerates startup banners or stray bytes
/// that some companion devices emit before the companion-frame protocol is
/// ready.  Once the prefix is found, reads the 2-byte LE payload length, then
/// the payload, then decodes.
///
/// Returns `io::Error` on any I/O failure or payload decode error.
async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<InboundFrame> {
    // Scan for the outbound-frame prefix, skipping any non-matching bytes.
    // This handles startup banners, CRLF noise, or any bytes the device sends
    // before entering companion-frame mode.
    loop {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte).await?;
        if byte[0] == FRAME_OUTBOUND_PREFIX {
            break;
        }
        // Log at trace so routine operation is quiet but debug builds show the noise.
        trace!(
            "companion: skipping non-prefix byte 0x{:02X} (expected 0x{FRAME_OUTBOUND_PREFIX:02X})",
            byte[0]
        );
    }

    let mut len_bytes = [0u8; 2];
    reader.read_exact(&mut len_bytes).await?;
    let payload_len = u16::from_le_bytes(len_bytes) as usize;

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

    trace!(
        "companion: rx {} bytes payload: {:02X?}",
        payload_len,
        payload
    );

    decode_inbound(&payload).map_err(|e: FrameDecodeError| {
        io::Error::new(io::ErrorKind::InvalidData, UndecodableFrame(e.to_string()))
    })
}

#[cfg(test)]
mod serial_open_error_tests {
    use super::*;

    #[test]
    fn permission_denied_serial_error_is_actionable() {
        let e = tokio_serial::Error::new(
            tokio_serial::ErrorKind::Io(io::ErrorKind::PermissionDenied),
            "permission denied",
        );
        let mapped = serial_open_error("/dev/ttyACM0", e);
        assert_eq!(mapped.kind(), io::ErrorKind::PermissionDenied);
        assert!(mapped.to_string().contains("usermod"));
        assert!(mapped.to_string().contains("ls -lL"));
    }

    #[test]
    fn other_serial_error_keeps_generic_message() {
        let e = tokio_serial::Error::new(tokio_serial::ErrorKind::NoDevice, "nope");
        let mapped = serial_open_error("/dev/ttyACM0", e);
        assert_ne!(mapped.kind(), io::ErrorKind::PermissionDenied);
        assert!(mapped.to_string().contains("could not open serial port"));
    }
}
