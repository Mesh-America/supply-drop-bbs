//! [`MeshTransport`] — the MeshCore [`Plugin`] + [`TransportEngine`] impl.
//!
//! # Lifecycle
//!
//! ```text
//! Plugin::init()   → connects CompanionClient, wires channels, returns Self
//! Plugin::start()  → moves client into event-loop task; task runs until stop()
//! Plugin::stop()   → signals watch channel; event loop drains and exits
//! ```
//!
//! # Thread / task model
//!
//! ```text
//!   ┌──────────────────┐      ┌─────────────────────────────────────┐
//!   │  MeshTransport   │      │          event_loop task             │
//!   │                  │      │                                      │
//!   │  cmd_tx ─────────┼─────►│  CompanionClient  ──► handle_frame  │
//!   │  state (Arc<Mutex>)─────►│  SessionState                       │
//!   │  shutdown_tx ────┼─────►│  watch::Receiver                    │
//!   └──────────────────┘      └─────────────────────────────────────┘
//! ```
//!
//! `notify()` uses the stored `cmd_tx` to push `SendTxtMsg` frames without
//! touching the event-loop task.  The loop is the sole consumer of
//! [`ClientEvent`]s; `notify()` is the sole producer of outbound commands
//! from the `MeshTransport` side.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use bbs_plugin_api::{
    error::{HostError, PluginError, TransportError},
    event::{DomainEvent, Notification, NotifyOutcome},
    identity::SessionId,
    plugin::Plugin,
    transport::TransportEngine,
    Command, Host, PermissionLevel, Response,
};
use meshcore_companion::{
    client::{ClientConfig, ClientEvent, CompanionClient, SerialConfig},
    constants::{MAX_FRAME_SIZE, TXT_TYPE_PLAIN},
    frame::OutboundFrame,
};

/// Maximum bytes of plain text that fit in one `SendTxtMsg` companion frame.
///
/// Wire layout: `[prefix:1][len:2][CMD:1][txt_type:1][attempt:1][timestamp:4][prefix:6][text:N]`
/// = 16 bytes of overhead.  Total frame must not exceed `MAX_FRAME_SIZE`.
const MAX_REPLY_BYTES: usize = MAX_FRAME_SIZE - 16;

/// Transport name recorded on advert records so the web UI can show which
/// radio network each node was heard on.
const TRANSPORT_NAME: &str = "meshcore";
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, warn};

use crate::{
    command::{format_response, parse_command, render_notification},
    config::{ConnectionType, MeshConfig},
    metrics::DeliveryStats,
    send_tracker::{RetryConfig, SendTracker, SentOutcome},
    session::SessionState,
};

/// Floor for how long to wait for a reply's end-to-end ACK before retransmitting.
const REPLY_ACK_MIN_WAIT: Duration = Duration::from_secs(4);
/// Ceiling for the ACK wait (guards against an absurd device timeout hint).
const REPLY_ACK_MAX_WAIT: Duration = Duration::from_secs(30);
/// How often the event loop checks for replies that timed out awaiting an ACK.
const RETRY_TICK: Duration = Duration::from_millis(500);
/// How often the event loop appends a delivery-history sample for trend display.
const SAMPLE_TICK: Duration = Duration::from_secs(60);
/// How far back to seed the in-memory trend from persisted samples on startup
/// (matches the in-memory ring's ~8h capacity).
const HISTORY_SEED_SECS: u64 = 8 * 60 * 60;

/// Depth of the queue feeding the command worker. Inbound LoRa traffic is slow
/// (airtime-limited), so this is rarely above 1; the headroom only matters if
/// frames arrive faster than the host can drain them, in which case the event
/// loop blocks on `send` (backpressure) rather than dropping work.
const COMMAND_QUEUE_DEPTH: usize = 64;

/// How fresh a queued message must be to be processed when draining the bridge
/// backlog on reconnect. Messages whose sender timestamp is older than this are
/// discarded rather than acted on, so recovering from a long outage does not
/// unleash a burst of replies to DMs the user sent long ago. Sized generously
/// (10 minutes) so it cleanly separates a brief link blip (process the backlog)
/// from a real outage (drop it), tolerating modest sender/BBS clock skew. A
/// message with no sender timestamp (0) is treated as fresh — we cannot prove it
/// stale, and dropping a real DM is worse than a late reply.
const DRAIN_STALE_AFTER_SECS: u32 = 600;

/// Enqueue a plain-text reply to the companion client and, when retransmission
/// is enabled, record it in `tracker` for delivery tracking.
///
/// Record + enqueue happen together under the tracker lock with a *non-blocking*
/// `try_send`, so the tracker's send-order FIFO matches the wire order even when
/// called from several tasks (the event loop, `notify`, domain-event pushes).
/// No `.await` is held across the lock. A full command channel drops the reply
/// (logged) rather than blocking the caller; depth is generous and this is rare.
fn enqueue_text(
    tracker: &Mutex<SendTracker>,
    stats: &DeliveryStats,
    cmd_tx: &mpsc::Sender<OutboundFrame>,
    prefix: [u8; 6],
    text: String,
    attempt: u8,
) {
    let frame = OutboundFrame::SendTxtMsg {
        txt_type: TXT_TYPE_PLAIN,
        // The wire `attempt` field is 0-based (0 = first send); the tracker
        // counts transmissions 1-based.
        attempt: attempt.saturating_sub(1),
        timestamp: now_unix_secs(),
        pubkey_prefix: prefix,
        text: text.clone(),
    };
    let mut t = tracker.lock().expect("send tracker mutex poisoned");
    match cmd_tx.try_send(frame) {
        Ok(()) => {
            // Count every frame that reaches the wire, independent of whether
            // retransmission (and hence the tracker's record) is enabled.
            stats.on_send(prefix, attempt);
            if t.retries_enabled() {
                t.record(prefix, text, TXT_TYPE_PLAIN, attempt, Instant::now());
            }
        }
        Err(e) => {
            stats.on_dropped();
            warn!(error = %e, "mesh: command channel full/closed — reply dropped");
        }
    }
}

/// Retransmit any replies whose end-to-end ACK deadline has passed, and log the
/// ones that have exhausted their attempt budget. Called on the retry tick from
/// the event loop. On retry the node's stored path is reset first (when flooding
/// is enabled) so a stale direct route doesn't keep failing the same way.
fn retransmit_due_replies(
    cmd_tx: &mpsc::Sender<OutboundFrame>,
    state: &Arc<Mutex<SessionState>>,
    tracker: &Arc<Mutex<SendTracker>>,
    stats: &DeliveryStats,
    flood_after_send: bool,
) {
    let due = {
        let mut t = tracker.lock().expect("send tracker mutex poisoned");
        if !t.retries_enabled() {
            return;
        }
        t.collect_due(Instant::now())
    };
    for rec in due.gave_up {
        stats.on_gave_up();
        stats.on_node_gave_up(rec.prefix);
        warn!(
            attempts = rec.attempt,
            "mesh: reply undelivered after all retries — giving up"
        );
    }
    for rec in due.to_retry {
        if flood_after_send {
            let pubkey = state
                .lock()
                .expect("state mutex poisoned")
                .get_full_pubkey(&rec.prefix);
            if let Some(pubkey) = pubkey {
                let _ = cmd_tx.try_send(OutboundFrame::ResetPath { pubkey });
            }
        }
        debug!(
            next_attempt = rec.attempt + 1,
            "mesh: retransmitting unacknowledged reply"
        );
        enqueue_text(
            tracker,
            stats,
            cmd_tx,
            rec.prefix,
            rec.text,
            rec.attempt + 1,
        );
    }
}

/// Current Unix time in seconds, truncated to u32 (the wire format's field width).
/// Returns 0 if the system clock is before the epoch, which should never happen
/// on a configured host.
pub fn now_unix_secs() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0)
}

// ── MeshTransport ─────────────────────────────────────────────────────────────

/// The MeshCore transport plugin.
///
/// Connects Supply Drop BBS to a `pymc_core` radio bridge over the companion-
/// frame TCP protocol.  Implements both [`Plugin`] (lifecycle) and
/// [`TransportEngine`] (inbound command processing + outbound notifications).
///
/// # Construction
///
/// Always constructed via [`Plugin::init`]; do not call `new` directly.
///
/// # Shutdown
///
/// Drop the value or call [`Plugin::stop`].  The event-loop task detects the
/// watch-channel signal and exits; the companion client's reconnect loop then
/// receives a `Shutdown` outcome and exits cleanly.
pub struct MeshTransport {
    /// Host handle — all domain operations go through this.
    host: Arc<dyn Host>,
    /// Clone of the companion client's outbound sender.  Used in `notify()` to
    /// push `SendTxtMsg` frames without going through the event-loop task.
    cmd_tx: mpsc::Sender<OutboundFrame>,
    /// Bi-directional session map, shared with the event-loop task.
    state: Arc<Mutex<SessionState>>,
    /// Holds the newly-constructed `CompanionClient` until `start()` moves it
    /// into the event-loop task.  Wrapped in `Option` so `start()` can `take`
    /// it out; `Mutex` so `start(&self)` can mutate through a shared ref.
    client_slot: Mutex<Option<CompanionClient>>,
    /// Sending half of the shutdown watch channel.  `stop()` sends `true`;
    /// the event-loop task watches for the change and exits.
    shutdown_tx: watch::Sender<bool>,
    /// Optional command prefix from config (e.g. `'!'`).
    command_prefix: Option<char>,
    /// Greeting sent to a node the first time it contacts the BBS.
    welcome_message: String,
    /// How many days a stored node credential stays valid (0 = disabled).
    node_credential_ttl_days: u32,
    /// Set to `true` while draining stale queued messages on (re)connect.
    /// Cleared when the bridge signals `NoMoreMessages`.
    draining: Arc<AtomicBool>,
    /// When `true`, queue a `ResetPath` immediately after every outbound
    /// `SendTxtMsg` so the next send to that node floods rather than using
    /// a potentially-stale direct path.  Can be disabled in config to
    /// restore pre-v0.2.4 direct-path-only behaviour.
    flood_after_send: bool,
    /// Seconds a node may await a workflow reply before the transport cancels the
    /// stale workflow and treats the node's next message as a fresh command.
    /// A lost prompt reply otherwise strands the node in an invisible workflow.
    /// `0` disables the timeout.
    workflow_timeout_secs: u64,
    /// Tracks in-flight replies and drives retransmission on missing ACKs.
    /// Shared with the event-loop task (which owns the retry timer and the
    /// `Sent`/`SendConfirmed` correlation). See [`crate::send_tracker`].
    send_tracker: Arc<Mutex<SendTracker>>,
    /// Cumulative reply-delivery counters, surfaced to the admin UI. Shared with
    /// the event-loop and notification tasks; lock-free. See [`crate::metrics`].
    delivery_stats: Arc<DeliveryStats>,
}

impl MeshTransport {
    /// Shared handle to this transport's reply-delivery counters.
    ///
    /// The host binary clones this and registers it with the web admin (as an
    /// `Arc<dyn TransportStats>`) so the operator can see round-trip link health.
    pub fn delivery_stats(&self) -> Arc<DeliveryStats> {
        Arc::clone(&self.delivery_stats)
    }
}

#[async_trait]
impl Plugin for MeshTransport {
    type Config = MeshConfig;

    fn name(&self) -> &'static str {
        "meshcore"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    /// Connect the companion client and wire internal channels.
    ///
    /// Dispatches to TCP or serial based on [`MeshConfig::connection_type`].
    /// The actual I/O begins in a background task; `start()` moves the client
    /// into the event-loop task to begin processing frames.
    async fn init(config: Self::Config, host: Arc<dyn Host>) -> Result<Self, PluginError> {
        let client = match config.connection_type {
            ConnectionType::Tcp | ConnectionType::Hat => {
                let client_config = ClientConfig {
                    addr: config.addr,
                    app_target_version: config.app_target_version,
                    reconnect_delay_initial: config.reconnect_delay_initial(),
                    reconnect_delay_max: config.reconnect_delay_max(),
                };
                info!(
                    addr = %config.addr,
                    mode = ?config.connection_type,
                    "mesh transport: connecting via TCP"
                );
                CompanionClient::connect(client_config)
            }

            ConnectionType::Serial => {
                let port = config.serial_port.clone().ok_or_else(|| {
                    PluginError::InvalidConfig(
                        "connection_type = 'serial' requires serial_port to be set".into(),
                    )
                })?;
                let serial_config = SerialConfig {
                    port: port.clone(),
                    baud_rate: config.baud_rate,
                    app_target_version: config.app_target_version,
                    reconnect_delay_initial: config.reconnect_delay_initial(),
                    reconnect_delay_max: config.reconnect_delay_max(),
                };
                info!(
                    port = %port,
                    baud = config.baud_rate,
                    "mesh transport: connecting via serial"
                );
                CompanionClient::connect_serial(serial_config)
            }
        };

        let cmd_tx = client.sender();
        let (shutdown_tx, _) = watch::channel(false);

        Ok(Self {
            host,
            cmd_tx,
            state: Arc::new(Mutex::new(SessionState::default())),
            client_slot: Mutex::new(Some(client)),
            shutdown_tx,
            command_prefix: config.command_prefix,
            welcome_message: config.welcome_message,
            node_credential_ttl_days: config.node_credential_ttl_days,
            draining: Arc::new(AtomicBool::new(false)),
            flood_after_send: config.flood_after_send,
            workflow_timeout_secs: config.workflow_timeout_secs,
            send_tracker: Arc::new(Mutex::new(SendTracker::new(RetryConfig {
                max_attempts: config.reply_max_attempts.max(1),
                min_timeout: REPLY_ACK_MIN_WAIT,
                max_timeout: REPLY_ACK_MAX_WAIT,
            }))),
            delivery_stats: Arc::new(DeliveryStats::default()),
        })
    }

    /// Move the companion client into the event-loop task and begin serving.
    ///
    /// Returns immediately after spawning the task.  Errors if `start()` is
    /// called a second time (the client slot is consumed on the first call).
    async fn start(&self) -> Result<(), PluginError> {
        let client = self
            .client_slot
            .lock()
            .expect("client_slot mutex poisoned")
            .take()
            .ok_or_else(|| PluginError::StartFailed("mesh transport already started".into()))?;

        let host = Arc::clone(&self.host);
        let cmd_tx = self.cmd_tx.clone();
        let state = Arc::clone(&self.state);
        let shutdown_rx = self.shutdown_tx.subscribe();
        let prefix = self.command_prefix;
        let welcome = self.welcome_message.clone();
        let ttl_days = self.node_credential_ttl_days;
        let flood_after_send = self.flood_after_send;
        let workflow_timeout_secs = self.workflow_timeout_secs;

        // Admin channel: web UI → event loop for key operations.
        let (key_tx, key_rx) = tokio::sync::mpsc::channel::<bbs_plugin_api::MeshKeyRequest>(4);
        self.host.register_mesh_key_ops(key_tx);

        // Watch for advert-send requests from the web UI.
        let mut advert_send_rx = host.advert_bus().subscribe_send();
        let advert_cmd_tx = self.cmd_tx.clone();
        let advert_host = Arc::clone(&host);
        let mut advert_shutdown_rx = self.shutdown_tx.subscribe();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    result = advert_send_rx.recv() => {
                        match result {
                            Ok(flood) => {
                                // Refresh the radio's stored location before broadcasting
                                // so manual sends include GPS just like the on-connect push.
                                if let Some((lat, lon)) = advert_host.node_location() {
                                    let lat_1e6 = (lat * 1_000_000.0) as i32;
                                    let lon_1e6 = (lon * 1_000_000.0) as i32;
                                    let _ = advert_cmd_tx
                                        .send(OutboundFrame::SetAdvertLatlon { lat_1e6, lon_1e6 })
                                        .await;
                                }
                                // Set the advert name before broadcasting too, so manual
                                // sends carry the configured node name even on devices that
                                // return no SelfInfo on AppStart (where the on-connect push
                                // is skipped). Issue #101.
                                if let Some(node_name) = advert_host.mesh_node_name() {
                                    if !node_name.is_empty() {
                                        info!(node_name = %node_name, "mesh: setting advert name before send");
                                        let _ = advert_cmd_tx
                                            .send(OutboundFrame::SetAdvertName { name: node_name })
                                            .await;
                                    }
                                }
                                if advert_cmd_tx
                                    .send(OutboundFrame::SendSelfAdvert { flood })
                                    .await
                                    .is_err()
                                {
                                    warn!("mesh: could not enqueue SendSelfAdvert — cmd channel closed");
                                } else {
                                    info!(flood, "mesh: sending self-advert");
                                }
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                warn!("mesh: advert send requests lagged by {n}");
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                    _ = advert_shutdown_rx.changed() => break,
                }
            }
        });

        // Subscribe to domain events and push notifications to online nodes.
        let mut domain_rx = host.events();
        let notif_state = Arc::clone(&self.state);
        let notif_cmd_tx = self.cmd_tx.clone();
        let notif_host = Arc::clone(&self.host);
        let notif_tracker = Arc::clone(&self.send_tracker);
        let notif_stats = Arc::clone(&self.delivery_stats);
        let mut notif_shutdown_rx = self.shutdown_tx.subscribe();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    result = domain_rx.recv() => {
                        match result {
                            Ok(event) => {
                                push_domain_notification(
                                    event,
                                    &notif_host,
                                    &notif_cmd_tx,
                                    &notif_state,
                                    &notif_tracker,
                                    &notif_stats,
                                    flood_after_send,
                                )
                                .await;
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                warn!("mesh: domain event stream lagged by {n}");
                            }
                            Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                        }
                    }
                    _ = notif_shutdown_rx.changed() => break,
                }
            }
        });

        let draining = Arc::clone(&self.draining);
        let send_tracker = Arc::clone(&self.send_tracker);
        let delivery_stats = Arc::clone(&self.delivery_stats);
        tokio::spawn(event_loop(
            client,
            host,
            cmd_tx,
            state,
            shutdown_rx,
            prefix,
            welcome,
            ttl_days,
            draining,
            flood_after_send,
            workflow_timeout_secs,
            send_tracker,
            delivery_stats,
            key_rx,
        ));

        info!("mesh transport started");
        Ok(())
    }

    /// Signal the event-loop task to stop.
    ///
    /// The task will exit after processing any in-flight frame; it may take up
    /// to one frame's processing time to fully stop.  The companion client's
    /// reconnect loop will also exit shortly after.
    async fn stop(&self) -> Result<(), PluginError> {
        // Ignore errors: the receiver may already be gone if the task exited.
        let _ = self.shutdown_tx.send(true);
        info!("mesh transport stop requested");
        Ok(())
    }
}

#[async_trait]
impl TransportEngine for MeshTransport {
    /// Push a notification to an active mesh session.
    ///
    /// Looks up the recipient's pubkey prefix from [`SessionState`], encodes
    /// the notification as plain text, and enqueues a `SendTxtMsg` frame.
    ///
    /// Returns [`NotifyOutcome::Queued`] on success (the frame is in the
    /// companion client's command channel; over-the-air delivery is not
    /// confirmed synchronously).  Returns [`NotifyOutcome::Dropped`] if the
    /// session has no associated pubkey prefix (unknown or already ended).
    async fn notify(
        &self,
        session: SessionId,
        payload: Notification,
    ) -> Result<NotifyOutcome, TransportError> {
        let pubkey_prefix = {
            let state = self.state.lock().expect("state mutex poisoned");
            state.by_session.get(&session).copied()
        };

        let Some(pubkey_prefix) = pubkey_prefix else {
            debug!(
                ?session,
                "notify: no mesh node mapped to session — dropping"
            );
            return Ok(NotifyOutcome::Dropped);
        };

        let text = render_notification(&payload);
        enqueue_text(
            &self.send_tracker,
            &self.delivery_stats,
            &self.cmd_tx,
            pubkey_prefix,
            text,
            1,
        );

        if self.flood_after_send {
            let full_pubkey = self
                .state
                .lock()
                .expect("state mutex poisoned")
                .get_full_pubkey(&pubkey_prefix);
            if let Some(pubkey) = full_pubkey {
                let _ = self.cmd_tx.send(OutboundFrame::ResetPath { pubkey }).await;
            }
        }

        Ok(NotifyOutcome::Queued)
    }
}

// ── Event loop ────────────────────────────────────────────────────────────────

/// React to a host [`DomainEvent`] by pushing a notification to affected nodes.
///
/// - `UserValidated`: tells the validated user their account is active.
/// - `UserCreated`: alerts all online aides and sysops of the new registration.
async fn push_domain_notification(
    event: DomainEvent,
    host: &Arc<dyn Host>,
    cmd_tx: &mpsc::Sender<OutboundFrame>,
    state: &Arc<Mutex<SessionState>>,
    send_tracker: &Arc<Mutex<SendTracker>>,
    stats: &Arc<DeliveryStats>,
    flood_after_send: bool,
) {
    let sessions: Vec<SessionId> = state
        .lock()
        .expect("state mutex poisoned")
        .by_session
        .keys()
        .copied()
        .collect();

    match event {
        DomainEvent::UserValidated { user } => {
            for sid in sessions {
                let Ok(ctx) = host.permission_ctx(sid).await else {
                    continue;
                };
                if ctx.username() != Some(&user) {
                    continue;
                }
                let prefix = state
                    .lock()
                    .expect("state mutex poisoned")
                    .by_session
                    .get(&sid)
                    .copied();
                if let Some(prefix) = prefix {
                    enqueue_text(
                        send_tracker,
                        stats,
                        cmd_tx,
                        prefix,
                        "Your account has been validated. You now have full access. Type 'H'."
                            .to_owned(),
                        1,
                    );
                    if flood_after_send {
                        let pubkey = state
                            .lock()
                            .expect("state mutex poisoned")
                            .get_full_pubkey(&prefix);
                        if let Some(pubkey) = pubkey {
                            let _ = cmd_tx.send(OutboundFrame::ResetPath { pubkey }).await;
                        }
                    }
                }
            }
        }
        DomainEvent::UserCreated { user } => {
            for sid in sessions {
                let Ok(ctx) = host.permission_ctx(sid).await else {
                    continue;
                };
                if ctx.level() < PermissionLevel::Aide {
                    continue;
                }
                let prefix = state
                    .lock()
                    .expect("state mutex poisoned")
                    .by_session
                    .get(&sid)
                    .copied();
                if let Some(prefix) = prefix {
                    enqueue_text(
                        send_tracker,
                        stats,
                        cmd_tx,
                        prefix,
                        format!(
                            "New registration: {} — type PENDING to review.",
                            user.as_str()
                        ),
                        1,
                    );
                    if flood_after_send {
                        let pubkey = state
                            .lock()
                            .expect("state mutex poisoned")
                            .get_full_pubkey(&prefix);
                        if let Some(pubkey) = pubkey {
                            let _ = cmd_tx.send(OutboundFrame::ResetPath { pubkey }).await;
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// Pending one-shot key operation in the event loop.
enum PendingKeyOp {
    Export {
        reply: tokio::sync::oneshot::Sender<Result<String, String>>,
    },
    Import {
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    ApplyRadio {
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
        /// true = waiting for SetRadioParams Ok; false = waiting for SetRadioTxPower Ok
        waiting_for_params: bool,
        /// tx_power to send after params Ok arrives
        tx_power_dbm: i8,
    },
}

/// Background task: receive [`ClientEvent`]s and dispatch them.
///
/// Runs until the shutdown watch fires or the companion client channel closes.
#[allow(clippy::too_many_arguments)]
async fn event_loop(
    mut client: CompanionClient,
    host: Arc<dyn Host>,
    cmd_tx: mpsc::Sender<OutboundFrame>,
    state: Arc<Mutex<SessionState>>,
    mut shutdown_rx: watch::Receiver<bool>,
    command_prefix: Option<char>,
    welcome_message: String,
    node_credential_ttl_days: u32,
    draining: Arc<AtomicBool>,
    flood_after_send: bool,
    workflow_timeout_secs: u64,
    send_tracker: Arc<Mutex<SendTracker>>,
    delivery_stats: Arc<DeliveryStats>,
    mut key_rx: tokio::sync::mpsc::Receiver<bbs_plugin_api::MeshKeyRequest>,
) {
    // Pending one-shot key operation. At most one at a time.
    let mut pending_key_op: Option<PendingKeyOp> = None;

    // Periodically retransmit replies that timed out awaiting an end-to-end ACK.
    let mut retry_tick = tokio::time::interval(RETRY_TICK);
    retry_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Periodically snapshot the delivery counters into the trend history.
    let mut sample_tick = tokio::time::interval(SAMPLE_TICK);
    sample_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // Seed the in-memory trend from persisted samples so the confirm-rate chart
    // survives a restart. Best-effort: an empty/erroring store just starts fresh.
    let since = (now_unix_secs() as u64).saturating_sub(HISTORY_SEED_SECS);
    match host.delivery_samples(TRANSPORT_NAME, since).await {
        Ok(samples) if !samples.is_empty() => {
            info!(
                count = samples.len(),
                "mesh: seeded delivery trend from storage"
            );
            delivery_stats.load_history(samples);
        }
        Ok(_) => {}
        Err(e) => debug!("mesh: loading delivery history failed: {e}"),
    }

    // Offload command processing to a single FIFO worker so handling a command
    // (host DB I/O) never blocks the event loop from reading the next frame —
    // delivery confirmations, queued-message notifications, and the next
    // `SyncNextMessage` stay responsive. One worker preserves per-node ordering
    // and the dedup check-and-record exactly as inline dispatch did. On
    // shutdown the loop drops `cmd_worker_tx` and awaits the worker (bounded
    // below) so any queued commands drain before the task exits.
    let (cmd_worker_tx, cmd_worker_rx) = mpsc::channel::<InboundCommand>(COMMAND_QUEUE_DEPTH);
    let worker = tokio::spawn(command_worker(
        cmd_worker_rx,
        Arc::clone(&host),
        cmd_tx.clone(),
        Arc::clone(&state),
        command_prefix,
        welcome_message,
        node_credential_ttl_days,
        flood_after_send,
        workflow_timeout_secs,
        Arc::clone(&send_tracker),
        Arc::clone(&delivery_stats),
    ));

    loop {
        tokio::select! {
            event = client.recv() => {
                match event {
                    None => {
                        info!("mesh: companion client channel closed — event loop exiting");
                        break;
                    }
                    Some(ClientEvent::Connected { self_info }) => {
                        // Drain any messages that queued while we were offline so
                        // they don't corrupt in-progress workflows.  Always done
                        // regardless of whether SelfInfo is available.
                        draining.store(true, Ordering::Relaxed);
                        let _ = cmd_tx.send(OutboundFrame::SyncNextMessage).await;

                        if let Some(info) = self_info {
                            info!(
                                node = %info.node_name,
                                freq_khz = info.frequency_khz,
                                adv_type = info.adv_type,
                                "mesh: radio bridge connected — draining stale queue"
                            );
                            // Register the BBS node in the advert bus so it appears in
                            // the web UI immediately (using whatever GPS the radio reports).
                            host.advert_bus().upsert(
                                info.pubkey,
                                info.node_name.clone(),
                                info.adv_type,
                                info.latitude,
                                info.longitude,
                                TRANSPORT_NAME,
                            );
                            // Record our own pubkey so the NewAdvert handler can
                            // detect self-advert echoes and preserve configured GPS.
                            state.lock().expect("state mutex poisoned").self_pubkey =
                                Some(info.pubkey);
                            // Publish pubkey to Host so the web UI can display it.
                            let pubkey_hex: String = info.pubkey.iter().map(|b| format!("{b:02x}")).collect();
                            host.set_node_pubkey(pubkey_hex);
                            // Push the configured node name to the radio so the BBS
                            // advertises with a human name instead of its key-derived
                            // fallback (issue #101).  The host has already truncated it
                            // to a MeshCore-safe length; the frame encoder also caps at
                            // 31 bytes as a final guard.
                            if let Some(node_name) = host.mesh_node_name() {
                                if !node_name.is_empty() {
                                    info!(node_name = %node_name, "mesh: setting advert name");
                                    let _ = cmd_tx
                                        .send(OutboundFrame::SetAdvertName { name: node_name })
                                        .await;
                                }
                            }
                            // Push GPS coordinates to the radio if configured, and
                            // refresh the advert bus entry so the web UI shows
                            // the config GPS.
                            if let Some((lat, lon)) = host.node_location() {
                                let lat_1e6 = (lat * 1_000_000.0) as i32;
                                let lon_1e6 = (lon * 1_000_000.0) as i32;
                                info!(lat_1e6, lon_1e6, "mesh: setting radio location");
                                let _ = cmd_tx
                                    .send(OutboundFrame::SetAdvertLatlon { lat_1e6, lon_1e6 })
                                    .await;
                                host.advert_bus().upsert(
                                    info.pubkey,
                                    info.node_name.clone(),
                                    info.adv_type,
                                    lat_1e6,
                                    lon_1e6,
                                    TRANSPORT_NAME,
                                );
                            }
                        } else {
                            // Device did not return SelfInfo (CMD_APP_START
                            // was unsupported) — node identity is unavailable
                            // until the device pushes an advert.
                            info!(
                                "mesh: radio bridge connected (no SelfInfo — \
                                 CMD_APP_START unsupported by device) \
                                 — draining stale queue"
                            );
                        }

                        // Fetch the full contact list so the advert bus is populated
                        // with names, types, and locations. Without this, nodes already
                        // in the device's contact table arrive only as short adverts
                        // (pubkey-only stubs) via PUSH_CODE_ADVERT (0x80).
                        let _ = cmd_tx.send(OutboundFrame::GetContacts { since: 0 }).await;

                        // Query the radio's autoadd config so we can ensure
                        // auto-pruning is enabled.  When the contact table is full
                        // and autoadd is active the firmware evicts the oldest entry
                        // to make room for newly-heard nodes.  Without this, a full
                        // table (PUSH_CODE_CONTACTS_FULL) prevents new contacts from
                        // being stored, causing outbound DMs to those nodes to fail.
                        // The response (InboundFrame::AutoaddConfig) is handled in
                        // handle_frame, which sets config if it is currently disabled.
                        let _ = cmd_tx.send(OutboundFrame::GetAutoaddConfig).await;
                    }
                    Some(ClientEvent::Disconnected { will_retry }) => {
                        // If a key operation is in flight, fail it immediately so
                        // the caller's oneshot receiver is not left hanging
                        // indefinitely waiting for a reply that will never come.
                        if let Some(op) = pending_key_op.take() {
                            let err = "device disconnected".to_owned();
                            match op {
                                PendingKeyOp::Export { reply } => { let _ = reply.send(Err(err)); }
                                PendingKeyOp::Import { reply } => { let _ = reply.send(Err(err)); }
                                PendingKeyOp::ApplyRadio { reply, .. } => { let _ = reply.send(Err(err)); }
                            }
                        }
                        if will_retry {
                            info!("mesh: radio bridge disconnected, will retry");
                        } else {
                            info!("mesh: radio bridge shut down — event loop exiting");
                            break;
                        }
                    }
                    Some(ClientEvent::Frame(frame)) => {
                        use meshcore_companion::frame::InboundFrame;
                        // Intercept key op responses before general frame dispatch.
                        let consumed = match &frame {
                            InboundFrame::PrivateKey { key } => {
                                if let Some(PendingKeyOp::Export { reply }) = pending_key_op.take() {
                                    let hex: String = key.iter().map(|b| format!("{b:02x}")).collect();
                                    let _ = reply.send(Ok(hex));
                                    true
                                } else {
                                    false
                                }
                            }
                            InboundFrame::Ok => {
                                match pending_key_op.take() {
                                    Some(PendingKeyOp::Import { reply }) => {
                                        let _ = reply.send(Ok(()));
                                        true
                                    }
                                    Some(PendingKeyOp::ApplyRadio { reply, waiting_for_params: true, tx_power_dbm }) => {
                                        // Params acknowledged — now send TX power
                                        let _ = cmd_tx.send(OutboundFrame::SetRadioTxPower { power_dbm: tx_power_dbm }).await;
                                        pending_key_op = Some(PendingKeyOp::ApplyRadio {
                                            reply,
                                            waiting_for_params: false,
                                            tx_power_dbm,
                                        });
                                        true
                                    }
                                    Some(PendingKeyOp::ApplyRadio { reply, waiting_for_params: false, .. }) => {
                                        // TX power acknowledged — done
                                        let _ = reply.send(Ok(()));
                                        true
                                    }
                                    other => {
                                        pending_key_op = other;
                                        false
                                    }
                                }
                            }
                            // Device returned an error frame while a key op was in
                            // flight — propagate it so the caller doesn't hang.
                            InboundFrame::Err { error_code } => {
                                if let Some(op) = pending_key_op.take() {
                                    let msg = format!("device error (code {error_code:#04x})");
                                    match op {
                                        PendingKeyOp::Export { reply } => { let _ = reply.send(Err(msg)); }
                                        PendingKeyOp::Import { reply } => { let _ = reply.send(Err(msg)); }
                                        PendingKeyOp::ApplyRadio { reply, .. } => { let _ = reply.send(Err(msg)); }
                                    }
                                    true
                                } else {
                                    false
                                }
                            }
                            _ => false,
                        };
                        if !consumed {
                            handle_frame(frame, &host, &cmd_tx, &state, &draining, &send_tracker, &delivery_stats, &cmd_worker_tx).await;
                        }
                    }
                }
            }
            _ = retry_tick.tick() => {
                retransmit_due_replies(&cmd_tx, &state, &send_tracker, &delivery_stats, flood_after_send);
            }
            _ = sample_tick.tick() => {
                let s = delivery_stats.sample(now_unix_secs() as u64);
                // Persist durably so the trend survives a restart. Best-effort:
                // a host without metrics storage just no-ops, and a write error
                // must not disturb the event loop.
                if let Err(e) = host.record_delivery_sample(TRANSPORT_NAME, s).await {
                    debug!("mesh: persisting delivery sample failed: {e}");
                }
            }
            Some(req) = key_rx.recv() => {
                use bbs_plugin_api::MeshKeyRequest;
                match req {
                    MeshKeyRequest::ExportKey { reply } => {
                        if pending_key_op.is_some() {
                            let _ = reply.send(Err("another key operation is already in progress".into()));
                        } else {
                            let _ = cmd_tx.send(OutboundFrame::ExportPrivateKey).await;
                            pending_key_op = Some(PendingKeyOp::Export { reply });
                        }
                    }
                    MeshKeyRequest::ImportKey { key, reply } => {
                        if pending_key_op.is_some() {
                            let _ = reply.send(Err("another key operation is already in progress".into()));
                        } else {
                            let _ = cmd_tx.send(OutboundFrame::ImportPrivateKey { key }).await;
                            pending_key_op = Some(PendingKeyOp::Import { reply });
                        }
                    }
                    MeshKeyRequest::ApplyRadio { params, reply } => {
                        if pending_key_op.is_some() {
                            let _ = reply.send(Err("another device operation is already in progress".into()));
                        } else {
                            let _ = cmd_tx.send(OutboundFrame::SetRadioParams {
                                frequency_hz: params.frequency_hz,
                                bandwidth_hz: params.bandwidth_hz,
                                spreading_factor: params.spreading_factor,
                                coding_rate: params.coding_rate,
                            }).await;
                            pending_key_op = Some(PendingKeyOp::ApplyRadio {
                                reply,
                                waiting_for_params: true,
                                tx_power_dbm: params.tx_power_dbm,
                            });
                        }
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                info!("mesh: shutdown signal received — event loop exiting");
                break;
            }
        }
    }

    // Drain the command worker before the task ends: dropping the sender closes
    // the channel, the worker finishes any queued commands, then `recv` returns
    // `None` and it exits. Bounded so a stuck host command can't hang shutdown —
    // on timeout the worker is left to finish (or be torn down with the runtime).
    drop(cmd_worker_tx);
    let _ = tokio::time::timeout(Duration::from_secs(5), worker).await;
}

/// Dispatch a single inbound frame from the radio bridge.
#[allow(clippy::too_many_arguments)]
async fn handle_frame(
    frame: meshcore_companion::frame::InboundFrame,
    host: &Arc<dyn Host>,
    cmd_tx: &mpsc::Sender<OutboundFrame>,
    state: &Arc<Mutex<SessionState>>,
    draining: &Arc<AtomicBool>,
    send_tracker: &Arc<Mutex<SendTracker>>,
    delivery_stats: &Arc<DeliveryStats>,
    cmd_worker_tx: &mpsc::Sender<InboundCommand>,
) {
    use meshcore_companion::frame::InboundFrame;

    match frame {
        // ── Drain complete ────────────────────────────────────────────────────
        // Bridge has no more queued messages; resume normal processing.
        InboundFrame::NoMoreMessages if draining.load(Ordering::Relaxed) => {
            draining.store(false, Ordering::Relaxed);
            info!("mesh: stale queue drained — resuming normal processing");
        }

        // ── Direct text messages (v1/v2 and v3 with SNR) ─────────────────────
        InboundFrame::ContactMsgRecv(msg) | InboundFrame::ContactMsgRecvV3(msg) => {
            let is_draining = draining.load(Ordering::Relaxed);

            // Only handle plain-text messages; CLI data and signed frames are
            // not BBS commands. Keep draining past a non-command frame so the
            // backlog still flushes to NoMoreMessages.
            if msg.txt_type != meshcore_companion::constants::TXT_TYPE_PLAIN {
                debug!(
                    txt_type = msg.txt_type,
                    "mesh: ignoring non-plain-text ContactMsg"
                );
                if is_draining {
                    let _ = cmd_tx.send(OutboundFrame::SyncNextMessage).await;
                }
                return;
            }

            // On reconnect we drain whatever queued while we were offline. Unlike
            // before — when the whole backlog was discarded — we now *process*
            // fresh messages so a DM sent during a brief blip still gets a reply.
            // Clearly-stale backlog (older than DRAIN_STALE_AFTER_SECS, judged by
            // the sender's timestamp) is still discarded so recovering from a long
            // outage doesn't unleash a burst of replies to long-dead messages.
            if is_draining {
                let age = now_unix_secs().saturating_sub(msg.timestamp);
                if msg.timestamp != 0 && age > DRAIN_STALE_AFTER_SECS {
                    debug!(
                        prefix = msg.sender_key_prefix[0],
                        age_secs = age,
                        "mesh: discarding stale queued message (draining)"
                    );
                    delivery_stats.on_reconnect_discard();
                    let _ = cmd_tx.send(OutboundFrame::SyncNextMessage).await;
                    return;
                }
                debug!(
                    prefix = msg.sender_key_prefix[0],
                    timestamp = msg.timestamp,
                    age_secs = age,
                    "mesh: processing queued message from reconnect backlog"
                );
                delivery_stats.on_inbound_received();
                if cmd_worker_tx
                    .send(InboundCommand {
                        sender_prefix: msg.sender_key_prefix,
                        timestamp: msg.timestamp,
                        text: msg.text,
                    })
                    .await
                    .is_err()
                {
                    warn!("mesh: command worker stopped — dropping inbound message");
                }
                // Keep pulling the rest of the backlog.
                let _ = cmd_tx.send(OutboundFrame::SyncNextMessage).await;
                return;
            }

            debug!(
                prefix = msg.sender_key_prefix[0],
                txt_type = msg.txt_type,
                // The sender's per-message timestamp drives retransmission dedup;
                // logging it lets an operator confirm two distinct sends carry
                // distinct timestamps (e.g. a password and its confirmation).
                timestamp = msg.timestamp,
                len = msg.text.len(),
                "mesh: inbound message received"
            );
            delivery_stats.on_inbound_received();
            // Hand the message to the command worker and return immediately so
            // the event loop stays free to read the next frame instead of
            // blocking on host I/O. The bounded channel applies backpressure
            // (rarely reached at LoRa rates) rather than dropping a message.
            if cmd_worker_tx
                .send(InboundCommand {
                    sender_prefix: msg.sender_key_prefix,
                    timestamp: msg.timestamp,
                    text: msg.text,
                })
                .await
                .is_err()
            {
                warn!("mesh: command worker stopped — dropping inbound message");
            }
            // Drain-to-NoMoreMessages: keep fetching until the bridge reports the
            // queue empty, so message delivery no longer depends on a perfect 1:1
            // MsgWaiting↔message correspondence. This recovers a message whose
            // MsgWaiting push was dropped (e.g. under bridge write-queue
            // backpressure) and is robust to a bridge/firmware that notifies once
            // per empty→non-empty transition rather than per message. The
            // follow-up sync uses a blocking send so a transiently full command
            // channel backpressures the event loop rather than dropping the sync
            // (a dropped sync would re-open the stranding gap). When the queue is
            // empty the bridge replies NoMoreMessages, which — while not draining —
            // falls through to a harmless no-op and stops the loop.
            if cmd_tx.send(OutboundFrame::SyncNextMessage).await.is_err() {
                warn!("mesh: could not enqueue drain SyncNextMessage — cmd channel closed");
            }
        }

        // ── Queued message notification ───────────────────────────────────────
        // The bridge has a message waiting; fetch it with SyncNextMessage.
        InboundFrame::MsgWaiting => {
            debug!("mesh: MsgWaiting — fetching next queued message");
            if cmd_tx.send(OutboundFrame::SyncNextMessage).await.is_err() {
                warn!("mesh: could not enqueue SyncNextMessage — cmd channel closed");
            }
        }

        // ── Contact advertisements ────────────────────────────────────────────
        // Record in the shared advert bus so the web UI can display them.
        // Sessions are minted on first DM, not on advert.
        InboundFrame::Advert { pubkey } => {
            host.advert_bus().upsert_short(pubkey, TRANSPORT_NAME);
            debug!(prefix = ?&pubkey[..6], "mesh: short advert received");
        }
        InboundFrame::NewAdvert(contact) => {
            // When the radio echoes our own advert back, its GPS fields reflect
            // the radio's hardware GPS (0,0 if no lock) — not the configured
            // location we pushed via SetAdvertLatlon.  Substitute the configured
            // GPS so the web UI stays accurate regardless of hardware GPS state.
            let self_pubkey = state.lock().expect("state mutex poisoned").self_pubkey;
            let (gps_lat, gps_lon) = if self_pubkey == Some(contact.pubkey) {
                host.node_location()
                    .map(|(lat, lon)| ((lat * 1_000_000.0) as i32, (lon * 1_000_000.0) as i32))
                    .unwrap_or((contact.gps_lat, contact.gps_lon))
            } else {
                (contact.gps_lat, contact.gps_lon)
            };
            host.advert_bus().upsert(
                contact.pubkey,
                contact.name.clone(),
                contact.adv_type,
                gps_lat,
                gps_lon,
                TRANSPORT_NAME,
            );
            // Record the full pubkey so we can send ResetPath after delivers.
            let prefix: [u8; 6] = contact.pubkey[..6].try_into().expect("pubkey is 32 bytes");
            state
                .lock()
                .expect("state mutex poisoned")
                .set_full_pubkey(&prefix, contact.pubkey);
            debug!(name = %contact.name, "mesh: full advert (new contact) received");
        }

        // ── Contact list sync (response to CMD_GET_CONTACTS) ─────────────────
        InboundFrame::Contact(contact) => {
            // Populate the advert bus with full metadata from the device's
            // contact list — these arrive as RESP_CODE_CONTACT frames after
            // CMD_GET_CONTACTS and contain name, type, and location.
            //
            // Use upsert_with_timestamp so the "Last Seen" column reflects the
            // real last-advert time stored on the device rather than the moment
            // we happened to run GetContacts (which would make every row show
            // the same timestamp).
            host.advert_bus().upsert_with_timestamp(
                contact.pubkey,
                contact.name.clone(),
                contact.adv_type,
                contact.gps_lat,
                contact.gps_lon,
                contact.last_advert_timestamp as i64,
                TRANSPORT_NAME,
            );
            // Record the full pubkey mapping so ResetPath can find the node.
            let prefix: [u8; 6] = contact.pubkey[..6].try_into().expect("pubkey is 32 bytes");
            state
                .lock()
                .expect("state mutex poisoned")
                .set_full_pubkey(&prefix, contact.pubkey);
            debug!(name = %contact.name, "mesh: contact list entry → advert bus");
        }
        InboundFrame::ContactsStart { count: _ } => {
            debug!("mesh: contact list sync started");
        }
        InboundFrame::EndOfContacts {
            most_recent_lastmod: _,
        } => {
            debug!("mesh: contact list sync complete");
        }

        // ── Outbound message send result ──────────────────────────────────────
        // RESP_CODE_SENT (0x06) is the device's reply to CMD_SEND_TXT_MSG.
        // Log a warning if the device could not route the message so operators
        // can diagnose delivery failures without digging through device logs.
        InboundFrame::Sent(result) => {
            let accepted = result.is_flood || result.expected_ack != 0;
            delivery_stats.on_sent_result(accepted);
            if !accepted {
                // MSG_SEND_FAILED — device could not route the message.
                // Common causes: no path to the destination, contact not in
                // the device's table, or the destination is out of range.
                warn!(
                    "mesh: device could not send message (MSG_SEND_FAILED) — \
                     message was not delivered; \
                     check that the destination node is in the radio's contact \
                     table and that a path exists"
                );
            } else {
                debug!(
                    is_flood = result.is_flood,
                    expected_ack = result.expected_ack,
                    timeout_ms = result.timeout_ms,
                    "mesh: message accepted by device"
                );
            }
            // Correlate with the oldest tracked send (RESP_CODE_SENT is the
            // in-order reply to each CMD_SEND_TXT_MSG). A CRC of 0 (send failed)
            // pops the record without scheduling an ACK wait, so it isn't retried
            // by the timeout path.
            let outcome = send_tracker
                .lock()
                .expect("send tracker mutex poisoned")
                .on_sent(result.expected_ack, result.timeout_ms, Instant::now());
            // Attribute the device verdict to the destination node (best-effort:
            // requires the tracker to have correlated a record, i.e. retries on).
            match outcome {
                SentOutcome::Accepted(prefix) => delivery_stats.on_node_sent_result(prefix, true),
                SentOutcome::Failed(ref rec) => {
                    delivery_stats.on_node_sent_result(rec.prefix, false)
                }
                SentOutcome::Spurious => {
                    debug!(
                        "mesh: Sent frame with no tracked send (retries off or already resolved)"
                    );
                }
            }
        }

        // ── End-to-end delivery confirmation ──────────────────────────────────
        // PUSH_CODE_SEND_CONFIRMED (0x82): the destination acknowledged receipt.
        // Clear the pending retransmission for this message.
        InboundFrame::SendConfirmed { crc } => {
            delivery_stats.on_confirmed(crc);
            let confirmed = send_tracker
                .lock()
                .expect("send tracker mutex poisoned")
                .on_confirmed(crc);
            if let Some(rec) = confirmed {
                // Round-trip latency of the delivered transmission. Available
                // only when retransmission tracking kept a record for this CRC.
                let latency = Instant::now().saturating_duration_since(rec.sent_at);
                let latency_ms = latency.as_millis() as u64;
                delivery_stats.record_latency(latency_ms);
                delivery_stats.on_node_confirmed(rec.prefix, latency_ms);
                debug!(crc, latency_ms, "mesh: reply delivery confirmed");
            }
        }

        // ── Device error frames ───────────────────────────────────────────────
        // InboundFrame::Err that was not consumed by the key-op handler above
        // (i.e., there is no pending key operation — this error is a response
        // to a regular command such as CMD_SYNC_NEXT_MESSAGE).
        //
        // If this arrives while we are draining the stale-message queue it
        // most likely means CMD_SYNC_NEXT_MESSAGE is not supported by this
        // firmware build.  Without this handler the draining flag would stay
        // true forever, causing every subsequent ContactMsgRecv to be silently
        // discarded — the BBS would appear alive but no user messages would
        // ever be processed.
        InboundFrame::Err { error_code } if draining.load(Ordering::Relaxed) => {
            warn!(
                error_code,
                "mesh: device error during message-queue drain — \
                 CMD_SYNC_NEXT_MESSAGE may be unsupported by this firmware; \
                 clearing drain flag and resuming normal message processing"
            );
            draining.store(false, Ordering::Relaxed);
        }

        InboundFrame::Err { error_code } => {
            debug!(
                error_code,
                "mesh: unhandled device error frame (no pending key op, not draining)"
            );
        }

        // ── Autoadd / autoprune config ────────────────────────────────────────
        // Response to CMD_GET_AUTOADD_CONFIG sent at startup.
        // When bit 0 is clear the firmware will NOT automatically add newly-heard
        // nodes to the contact table, and will NOT prune old entries when the
        // table is full.  Enable it so the table self-manages.
        InboundFrame::AutoaddConfig { config } => {
            if config & 1 == 0 {
                warn!(
                    config,
                    "mesh: contact autoadd is disabled on the radio — \
                     enabling it so stale contacts are pruned when the table is full"
                );
                let _ = cmd_tx
                    .send(OutboundFrame::SetAutoaddConfig { config: config | 1 })
                    .await;
            } else {
                debug!(config, "mesh: contact autoadd already enabled");
            }
        }

        // ── Contact table full — proactive eviction ───────────────────────────
        // The firmware could not store a new contact because the table is at
        // capacity.  Outbound DMs to nodes not in the table fail silently
        // because the radio cannot encrypt to an unknown key.
        //
        // Response: find the least-recently-seen contact in the advert bus that
        // does NOT have an active BBS session (we do not want to evict someone
        // mid-conversation), then send CMD_REMOVE_CONTACT for it.  This frees
        // one slot so the firmware can store the next new contact it hears.
        //
        // Note: after eviction the firmware will NOT automatically retry the
        // contact that just failed.  The new node needs to send another advert
        // (MeshCore nodes re-advertise periodically) before it can be added.
        InboundFrame::ContactsFull => {
            warn!(
                "mesh: radio contact table is full — \
                 evicting stalest inactive contact to free a slot"
            );
            // Build an exclusion list: our own node + all prefixes with active sessions.
            let (active_prefixes, self_prefix) = {
                let st = state.lock().expect("state mutex poisoned");
                let active: Vec<[u8; 6]> = st.by_prefix.keys().copied().collect();
                let sp = st
                    .self_pubkey
                    .map(|pk| pk[..6].try_into().expect("pubkey is 32 bytes"));
                (active, sp)
            };
            let mut exclude = active_prefixes;
            if let Some(sp) = self_prefix {
                if !exclude.contains(&sp) {
                    exclude.push(sp);
                }
            }
            match host.advert_bus().stalest_pubkey_excluding(&exclude) {
                Some(stale_pubkey) => {
                    let prefix_hex: String = stale_pubkey[..6]
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect();
                    warn!(
                        prefix = %prefix_hex,
                        "mesh: removing stale contact from radio table to free a slot"
                    );
                    let _ = cmd_tx
                        .send(OutboundFrame::RemoveContact {
                            pubkey: stale_pubkey,
                        })
                        .await;
                }
                None => {
                    warn!(
                        "mesh: radio table full but no evictable stale contact found \
                         (all known contacts have active sessions); \
                         new nodes cannot be added until sessions close"
                    );
                }
            }
        }

        // ── Path (re)learned to a node ────────────────────────────────────────
        // The device pushes the full 32-byte pubkey when it learns or refreshes a
        // route to a contact. Capture it: forcing a reply to flood (so it isn't
        // lost on a stale multi-hop direct path) is done with `ResetPath`, which
        // needs the full key — but an inbound DM only carries the 6-byte prefix,
        // and `GetContacts` runs once on connect, before first-contact sessions
        // exist. Without this the full key stays unknown for exactly the far
        // nodes that need flooding, so `flood_after_send`'s post-reply ResetPath
        // is silently skipped (`get_full_pubkey` returns None) and every reply
        // goes direct. `set_full_pubkey` no-ops until a session exists; PathUpdated
        // arrives after the inbound DM created it, so it lands.
        InboundFrame::PathUpdated { pubkey } => {
            let prefix: [u8; 6] = pubkey[..6].try_into().expect("pubkey is 32 bytes");
            state
                .lock()
                .expect("state mutex poisoned")
                .set_full_pubkey(&prefix, pubkey);
            debug!(
                prefix = prefix[0],
                "mesh: PathUpdated — captured full pubkey (enables flood-after-send for this node)"
            );
        }

        // ── Everything else ───────────────────────────────────────────────────
        other => {
            debug!("mesh: ignoring frame {other:?}");
        }
    }
}

/// A plain-text direct message handed off from the event loop to the command
/// worker for processing. Carries everything [`dispatch_message`] needs that is
/// specific to one inbound message; the rest of its context is owned by the
/// worker.
struct InboundCommand {
    sender_prefix: [u8; 6],
    timestamp: u32,
    text: String,
}

/// Process inbound commands off the event loop, one at a time, in arrival order.
///
/// The event loop forwards each plain-text DM here and immediately returns to
/// reading frames, so handling a command (which awaits host DB I/O) never
/// blocks delivery-confirmation handling or the next queued-message fetch. A
/// single worker keeps per-node ordering — and the dedup check-and-record —
/// identical to the previous inline dispatch. Exits when the event loop drops
/// the sender and the queue drains.
#[allow(clippy::too_many_arguments)]
async fn command_worker(
    mut rx: mpsc::Receiver<InboundCommand>,
    host: Arc<dyn Host>,
    cmd_tx: mpsc::Sender<OutboundFrame>,
    state: Arc<Mutex<SessionState>>,
    command_prefix: Option<char>,
    welcome_message: String,
    node_credential_ttl_days: u32,
    flood_after_send: bool,
    workflow_timeout_secs: u64,
    send_tracker: Arc<Mutex<SendTracker>>,
    delivery_stats: Arc<DeliveryStats>,
) {
    while let Some(cmd) = rx.recv().await {
        dispatch_message(
            cmd.sender_prefix,
            cmd.timestamp,
            &cmd.text,
            &host,
            &cmd_tx,
            &state,
            command_prefix,
            &welcome_message,
            node_credential_ttl_days,
            flood_after_send,
            workflow_timeout_secs,
            &send_tracker,
            &delivery_stats,
        )
        .await;
    }
    debug!("mesh: command worker stopped (queue closed)");
}

/// Parse a direct message text, route it through the host, and send the reply.
#[allow(clippy::too_many_arguments)]
async fn dispatch_message(
    sender_prefix: [u8; 6],
    timestamp: u32,
    text: &str,
    host: &Arc<dyn Host>,
    cmd_tx: &mpsc::Sender<OutboundFrame>,
    state: &Arc<Mutex<SessionState>>,
    command_prefix: Option<char>,
    welcome_message: &str,
    node_credential_ttl_days: u32,
    flood_after_send: bool,
    workflow_timeout_secs: u64,
    send_tracker: &Arc<Mutex<SendTracker>>,
    delivery_stats: &Arc<DeliveryStats>,
) {
    // ── Get or create a session for this node ─────────────────────────────────
    let Some((session, is_new)) = get_or_create_session(sender_prefix, host, state).await else {
        // Session creation failed; the error was already logged inside
        // get_or_create_session.  Skip this message â the command worker continues
        // with the next queued command.
        return;
    };

    if is_new {
        debug!(?session, prefix = ?sender_prefix, "mesh: new session minted");
    }

    // Resolve the full 32-byte pubkey for this node (populated by advert/contact frames).
    // Credential operations require the full key; if unavailable, they are skipped.
    let full_pubkey: Option<[u8; 32]> = state
        .lock()
        .expect("state mutex poisoned")
        .get_full_pubkey(&sender_prefix);

    // ── Determine if we're awaiting a workflow reply ──────────────────────────
    let awaiting_reply = state
        .lock()
        .expect("state mutex poisoned")
        .is_awaiting_reply(&sender_prefix);

    // ── Workflow idle-timeout ─────────────────────────────────────────────────
    // If this node has been awaiting a workflow reply longer than the configured
    // window, its prompt reply was almost certainly lost on the return path and
    // the node is stranded: every message it sends is consumed as workflow input
    // whose "try again" response is also lost, and only `cancel` breaks the loop.
    // Cancel the stale workflow on BOTH sides — via Command::Cancel so the host's
    // Workflow resets in sync with the transport flag — and treat THIS message as
    // a fresh command (fall through with awaiting_reply = false). The timer is
    // stamped per workflow stage (see SessionState::update_awaiting_reply), so a
    // legitimately-progressing multi-step flow is not cut short.
    let awaiting_reply = if awaiting_reply
        && workflow_timeout_secs > 0
        && state
            .lock()
            .expect("state mutex poisoned")
            .awaiting_reply_expired(&sender_prefix, Duration::from_secs(workflow_timeout_secs))
    {
        info!(
            ?session,
            timeout_secs = workflow_timeout_secs,
            "mesh: workflow idle-timeout — cancelling stale workflow, treating message as a fresh command"
        );
        // Reset host-side workflow state in sync (handle_cancel → Workflow::None).
        // The response is discarded; the current message is re-processed below as
        // a fresh command. Cancel is infallible on a live session; on a stale one
        // it errors harmlessly and the replay below hits the UnknownSession path.
        if let Err(e) = host.process_command(session, Command::Cancel).await {
            warn!(?session, "mesh: workflow-timeout cancel error: {e}");
        }
        state
            .lock()
            .expect("state mutex poisoned")
            .update_awaiting_reply(&sender_prefix, None);
        false
    } else {
        awaiting_reply
    };

    // ── Build greeting for unauthenticated nodes ──────────────────────────────
    // Show the welcome banner to any unauthenticated node on every message so
    // they always have context for how to register or log in.  The greeting is
    // NOT sent as a separate frame here — it is prepended to the first response
    // frame below so both arrive in a single radio transmission.
    //
    // Skip during active workflows: the node is mid-flow (e.g. entering a
    // password) and seeing the banner again would be confusing.
    let already_authenticated = host
        .permission_ctx(session)
        .await
        .map(|ctx| ctx.username().is_some())
        .unwrap_or(false);

    let pending_greeting: Option<String> = if !already_authenticated && !awaiting_reply {
        // Attempt auto-login via stored node credential (new sessions only;
        // skip when TTL = 0 or full pubkey unknown).
        let auto_username = if is_new && node_credential_ttl_days > 0 {
            if let Some(pubkey) = full_pubkey {
                match host
                    .mesh_node_restore(session, pubkey, node_credential_ttl_days)
                    .await
                {
                    Ok(u) => u,
                    Err(e) => {
                        warn!(?session, "mesh: node_restore error: {e}");
                        None
                    }
                }
            } else {
                None
            }
        } else {
            None
        };

        // Resolve {name} — prefer auto-login username, then advertised display
        // name, then empty so the placeholder is always removed.
        let name = auto_username
            .as_ref()
            .map(|u| u.as_str().to_owned())
            .or_else(|| host.advert_bus().name_by_prefix(&sender_prefix))
            .unwrap_or_default();

        let welcome = welcome_message.replace("{name}", &name);

        let greeting = match &auto_username {
            Some(username) if !welcome.is_empty() => {
                format!("{welcome}\nWelcome back, {username}! Type 'H' for commands.")
            }
            Some(username) => {
                format!("Welcome back, {username}! Type 'H' for commands.")
            }
            None => welcome,
        };

        if greeting.is_empty() {
            None
        } else {
            Some(greeting)
        }
    } else {
        None
    };

    // ── Deduplicate retransmissions ───────────────────────────────────────────
    // Prefer the sender's per-message timestamp: a retransmission reuses it, so
    // matching on (timestamp, text) drops resends robustly — even ones delayed
    // past the text-only window or arriving after the workflow state changed
    // (the "Error: Already logged in" symptom). When the sender supplies no
    // timestamp (0), fall back to the text-only window, which additionally
    // guards mesh retransmissions of a workflow reply that land after the
    // workflow has completed.
    //
    // The `on_dedup_drop_*` counters here are DIAGNOSTIC ONLY — they do not
    // change behaviour. They let an operator see, from the field, how often the
    // dedup fires on a given node's traffic. This matters because a coarse-clock
    // bridge (the pyMC bridge stamps whole-second `int(time.time())` and discards
    // the client's own timestamp) makes two distinct sends in the same second
    // collide on the (timestamp, text) key. Whether that ever drops *legitimate*
    // traffic in practice is what these counters measure; the behavioural fix (if
    // the data shows one is warranted) is deliberately deferred, because a
    // same-second confirmation is on-the-wire indistinguishable from a stale
    // retransmission of the command that triggered the prompt, so no
    // transport-layer rule can tell them apart (see the tests
    // `identical_workflow_replies_with_same_timestamp_dedup_second` and
    // `real_host_register_double_send_then_password`, which demand opposite
    // outcomes for byte-identical input).
    if timestamp != 0 {
        if state
            .lock()
            .expect("state mutex poisoned")
            .dedup_by_timestamp(&sender_prefix, timestamp, text)
        {
            debug!(
                timestamp,
                "mesh: dropping retransmitted message (timestamp dedup)"
            );
            delivery_stats.on_dedup_drop_timestamp();
            return;
        }
    } else {
        if state
            .lock()
            .expect("state mutex poisoned")
            .dedup_message(&sender_prefix, text)
        {
            debug!("mesh: dropping retransmitted message (text dedup)");
            delivery_stats.on_dedup_drop_text();
            return;
        }
        if !awaiting_reply
            && state
                .lock()
                .expect("state mutex poisoned")
                .is_recent_workflow_reply(&sender_prefix, text)
        {
            debug!("mesh: dropping retransmitted workflow reply (text dedup)");
            delivery_stats.on_dedup_drop_text();
            return;
        }
    }

    // ── Parse the command ─────────────────────────────────────────────────────
    let Some(cmd) = parse_command(text, command_prefix, awaiting_reply) else {
        // Message doesn't match prefix and no workflow active — silently ignore.
        debug!("mesh: message ignored (no prefix match, no active workflow)");
        return;
    };

    // Record workflow reply text for retransmission deduplication.
    if awaiting_reply {
        state
            .lock()
            .expect("state mutex poisoned")
            .set_last_workflow_reply(&sender_prefix, text.to_owned());
    }

    debug!(?session, ?cmd, "mesh: dispatching command");

    // ── Process through the host ──────────────────────────────────────────────
    // On UnknownSession (e.g. server restarted while the transport retained a
    // stale session ID), evict the stale mapping, mint a fresh session, and
    // retry the command once with the new ID.
    //
    // `active_sid` tracks whichever session ID was actually used to process the
    // command so that subsequent credential operations (mesh_node_bind) target
    // the correct — potentially refreshed — session.
    let mut active_sid = session;
    let response = match host.process_command(session, cmd.clone()).await {
        Ok(r) => r,
        Err(HostError::UnknownSession(stale)) => {
            info!(?stale, "mesh: stale session — refreshing");
            state
                .lock()
                .expect("state mutex poisoned")
                .remove_by_prefix(&sender_prefix);
            let fresh = match host.create_session("meshcore").await {
                Ok(id) => id,
                Err(e) => {
                    warn!("mesh: session refresh failed: {e}");
                    return;
                }
            };
            let (fresh_sid, _) = state
                .lock()
                .expect("state mutex poisoned")
                .get_or_insert(sender_prefix, fresh);
            // Track the fresh session so credential operations below use it.
            active_sid = fresh_sid;
            // Attempt auto-login on the refreshed session before replaying the command.
            if node_credential_ttl_days > 0 {
                if let Some(pubkey) = full_pubkey {
                    if let Err(e) = host
                        .mesh_node_restore(fresh_sid, pubkey, node_credential_ttl_days)
                        .await
                    {
                        warn!(?fresh_sid, "mesh: node_restore on refresh error: {e}");
                    }
                }
            }
            // The original command was parsed with the stale transport state
            // (which may have had `awaiting_reply = true`).  If it became a
            // WorkflowReply but the fresh session has no active workflow, re-parse
            // with awaiting_reply = false so the user's intent is honoured.
            //
            // Example: user had K's room list open; BBS session expired; user
            // sends "N" → parsed as WorkflowReply.  After eviction the fresh
            // session has Workflow::None, so we re-parse "N" as Command::ReadNew.
            let retry_cmd = if matches!(cmd, Command::WorkflowReply { .. }) {
                parse_command(text, command_prefix, false).unwrap_or_else(|| cmd.clone())
            } else {
                cmd.clone()
            };
            match host.process_command(fresh_sid, retry_cmd).await {
                Ok(r) => r,
                Err(e) => {
                    warn!(?fresh_sid, "mesh: error after session refresh: {e}");
                    Response::Error(format!("{e}"))
                }
            }
        }
        Err(e) => {
            warn!(?session, "mesh: host returned error: {e}");
            Response::Error(format!("{e}"))
        }
    };

    // ── Persist / clear node credential on auth state changes ────────────────
    // Only operate when the full 32-byte pubkey is known; skip silently otherwise.
    if node_credential_ttl_days > 0 {
        if let Some(pubkey) = full_pubkey {
            match &response {
                Response::LoggedIn { .. } => {
                    if let Err(e) = host.mesh_node_bind(active_sid, pubkey).await {
                        warn!(?active_sid, "mesh: node_bind error: {e}");
                    }
                }
                Response::LoggedOut => {
                    if let Err(e) = host.mesh_node_unbind(pubkey).await {
                        warn!("mesh: node_unbind error: {e}");
                    }
                }
                _ => {}
            }
        }
    }

    // ── Update workflow-reply state ───────────────────────────────────────────
    // A Prompt response means the node's next message continues a workflow; any
    // other response ends it. update_awaiting_reply also stamps the idle-timeout
    // clock (per workflow stage — only when the prompt text changes), so a node
    // stranded by a lost prompt reply can be freed (see the workflow idle-timeout
    // near the top of dispatch_message).
    let prompt_text: Option<&str> = match &response {
        Response::Prompt { text, .. } => Some(text.as_str()),
        _ => None,
    };
    {
        let mut state = state.lock().expect("state mutex poisoned");
        state.update_awaiting_reply(&sender_prefix, prompt_text);
        // A new prompt begins a fresh reply turn, so the user's next message is
        // genuine input even if it repeats the previous reply verbatim. Clear
        // the message-dedup baseline so the general retransmission dedup can't
        // silently drop, for example, the matching password entered again at
        // "Confirm your password:". (#104)
        //
        // This only matters for the timestamp==0 fallback path. On the timestamp
        // path, #104 is handled naturally: the re-typed confirmation is a new
        // send with a new per-message timestamp, so dedup_by_timestamp's
        // (timestamp, text) key already treats it as distinct — the dedup hinges
        // on the client stamping each distinct send with a distinct timestamp.
        // Do NOT also clear `recent_msgs` here: that would let a delayed
        // retransmission of the previous reply through after a prompt, re-opening
        // the very "Error: Already logged in" reprocessing this dedup prevents.
        if prompt_text.is_some() {
            state.clear_last_message(&sender_prefix);
        }
    }

    // ── Collect frames to send back ───────────────────────────────────────────
    // MultiText delivers each element as a separate radio frame.
    // All other variants produce a single frame via format_response.
    //
    // If a greeting was built above, prepend it to the first frame so the
    // welcome banner and the command response arrive as one radio transmission
    // rather than two.  When the response carries no text (format_response
    // returns None) the greeting is sent on its own.
    let mut frames: Vec<String> = if let Response::MultiText(parts) = &response {
        parts.clone()
    } else {
        match format_response(&response) {
            Some(t) => vec![t],
            None => vec![],
        }
    };

    if let Some(greeting) = pending_greeting {
        if let Some(first) = frames.first_mut() {
            *first = format!("{greeting}\n{first}");
        } else {
            frames.push(greeting);
        }
    }

    if frames.is_empty() {
        return;
    }

    let frame_count = frames.len();
    for (i, reply_text) in frames.into_iter().enumerate() {
        let is_last = i + 1 == frame_count;

        // Guard against oversized frames: truncate to the maximum that fits in
        // one companion frame.  Truncation is a last resort — source strings
        // should stay under MAX_REPLY_BYTES.  Walk back from the limit to
        // preserve valid UTF-8.
        let reply_text = if reply_text.len() > MAX_REPLY_BYTES {
            warn!(
                ?session,
                original_len = reply_text.len(),
                max_len = MAX_REPLY_BYTES,
                "mesh: reply too long for one frame — truncating"
            );
            let mut end = MAX_REPLY_BYTES;
            while !reply_text.is_char_boundary(end) {
                end -= 1;
            }
            reply_text[..end].to_owned()
        } else {
            reply_text
        };

        debug!(
            ?session,
            len = reply_text.len(),
            frame = i + 1,
            total = frame_count,
            "mesh: sending reply to node"
        );

        enqueue_text(
            send_tracker,
            delivery_stats,
            cmd_tx,
            sender_prefix,
            reply_text,
            1,
        );
        // Reset path only after the last frame so intermediate frames travel
        // the same (possibly direct) route as the first.
        if is_last && flood_after_send {
            let pubkey = state
                .lock()
                .expect("state mutex poisoned")
                .get_full_pubkey(&sender_prefix);
            if let Some(pubkey) = pubkey {
                let _ = cmd_tx.send(OutboundFrame::ResetPath { pubkey }).await;
            }
        }
    }
}

/// Ensure a BBS session exists for `prefix`, creating one via the host if not.
///
/// Holds the state mutex only briefly; does not hold it across the async
/// `create_session` call.
async fn get_or_create_session(
    prefix: [u8; 6],
    host: &Arc<dyn Host>,
    state: &Arc<Mutex<SessionState>>,
) -> Option<(SessionId, bool)> {
    // Fast path: session already exists.
    if let Some(sid) = state.lock().expect("state mutex poisoned").lookup(&prefix) {
        return Some((sid, false));
    }

    // Slow path: mint a new session from the host.
    let new_id = match host.create_session("meshcore").await {
        Ok(id) => id,
        Err(e) => {
            // This should not happen in normal operation; log and use a dummy.
            warn!("mesh: host.create_session failed: {e}");
            // Re-check state in case another concurrent message beat us here.
            if let Some(sid) = state.lock().expect("state mutex poisoned").lookup(&prefix) {
                return Some((sid, false));
            }
            // We cannot proceed without a session, but we must NOT panic here.
            // This runs on the detached command_worker task; a panic would kill
            // it permanently and silently, halting all mesh command processing
            // with no visible error. Instead, log the failure and return None so
            // the caller skips this message and the worker stays alive for the
            // next queued command.
            error!(
                "mesh: host.create_session failed and no fallback: {e} — skipping this message to keep the command worker alive"
            );
            return None;
        }
    };

    let (sid, is_new) = state
        .lock()
        .expect("state mutex poisoned")
        .get_or_insert(prefix, new_id);

    Some((sid, is_new))
}
