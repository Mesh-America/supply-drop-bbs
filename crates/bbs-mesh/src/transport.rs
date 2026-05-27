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
use std::time::{SystemTime, UNIX_EPOCH};

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
use tokio::sync::{mpsc, watch};
use tracing::{debug, error, info, warn};

use crate::{
    command::{format_response, parse_command, render_notification},
    config::{ConnectionType, MeshConfig},
    session::SessionState,
};

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
}

#[async_trait]
impl Plugin for MeshTransport {
    type Config = MeshConfig;

    fn name(&self) -> &'static str {
        "mesh"
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
                                if advert_cmd_tx
                                    .send(OutboundFrame::SendSelfAdvert { flood })
                                    .await
                                    .is_err()
                                {
                                    warn!("mesh: could not enqueue SendSelfAdvert — cmd channel closed");
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
        self.cmd_tx
            .send(OutboundFrame::SendTxtMsg {
                txt_type: TXT_TYPE_PLAIN,
                attempt: 0,
                timestamp: now_unix_secs(),
                pubkey_prefix,
                text,
            })
            .await
            .map_err(|_| TransportError::ConnectionLost("companion client closed".into()))?;

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
                if ctx.username.as_ref() != Some(&user) {
                    continue;
                }
                let prefix = state
                    .lock()
                    .expect("state mutex poisoned")
                    .by_session
                    .get(&sid)
                    .copied();
                if let Some(prefix) = prefix {
                    let _ = cmd_tx
                        .send(OutboundFrame::SendTxtMsg {
                            txt_type: TXT_TYPE_PLAIN,
                            attempt: 0,
                            timestamp: now_unix_secs(),
                            pubkey_prefix: prefix,
                            text: "Your account has been validated. \
                                   You now have full access. Type 'H'."
                                .to_owned(),
                        })
                        .await;
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
                if ctx.level < PermissionLevel::Aide {
                    continue;
                }
                let prefix = state
                    .lock()
                    .expect("state mutex poisoned")
                    .by_session
                    .get(&sid)
                    .copied();
                if let Some(prefix) = prefix {
                    let _ = cmd_tx
                        .send(OutboundFrame::SendTxtMsg {
                            txt_type: TXT_TYPE_PLAIN,
                            attempt: 0,
                            timestamp: now_unix_secs(),
                            pubkey_prefix: prefix,
                            text: format!(
                                "New registration: {} — type PENDING to review.",
                                user.as_str()
                            ),
                        })
                        .await;
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
    mut key_rx: tokio::sync::mpsc::Receiver<bbs_plugin_api::MeshKeyRequest>,
) {
    // Pending one-shot key operation. At most one at a time.
    let mut pending_key_op: Option<PendingKeyOp> = None;

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
                            );
                            // Record our own pubkey so the NewAdvert handler can
                            // detect self-advert echoes and preserve configured GPS.
                            state.lock().expect("state mutex poisoned").self_pubkey =
                                Some(info.pubkey);
                            // Publish pubkey to Host so the web UI can display it.
                            let pubkey_hex: String = info.pubkey.iter().map(|b| format!("{b:02x}")).collect();
                            host.set_node_pubkey(pubkey_hex);
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
                    }
                    Some(ClientEvent::Disconnected { will_retry }) => {
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
                                if let Some(PendingKeyOp::Import { reply }) = pending_key_op.take() {
                                    let _ = reply.send(Ok(()));
                                    true
                                } else {
                                    false
                                }
                            }
                            _ => false,
                        };
                        if !consumed {
                            handle_frame(frame, &host, &cmd_tx, &state, command_prefix, &welcome_message, node_credential_ttl_days, &draining, flood_after_send).await;
                        }
                    }
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
                }
            }
            _ = shutdown_rx.changed() => {
                info!("mesh: shutdown signal received — event loop exiting");
                break;
            }
        }
    }
}

/// Dispatch a single inbound frame from the radio bridge.
#[allow(clippy::too_many_arguments)]
async fn handle_frame(
    frame: meshcore_companion::frame::InboundFrame,
    host: &Arc<dyn Host>,
    cmd_tx: &mpsc::Sender<OutboundFrame>,
    state: &Arc<Mutex<SessionState>>,
    command_prefix: Option<char>,
    welcome_message: &str,
    node_credential_ttl_days: u32,
    draining: &Arc<AtomicBool>,
    flood_after_send: bool,
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
            // While draining, discard queued messages and request the next one
            // so we flush the entire backlog before serving live traffic.
            if draining.load(Ordering::Relaxed) {
                info!(
                    prefix = msg.sender_key_prefix[0],
                    "mesh: discarding stale queued message (draining)"
                );
                let _ = cmd_tx.send(OutboundFrame::SyncNextMessage).await;
                return;
            }

            // Only handle plain-text messages; CLI data and signed frames are
            // not BBS commands.
            if msg.txt_type != meshcore_companion::constants::TXT_TYPE_PLAIN {
                info!(
                    txt_type = msg.txt_type,
                    "mesh: ignoring non-plain-text ContactMsg"
                );
                return;
            }

            info!(
                prefix = msg.sender_key_prefix[0],
                txt_type = msg.txt_type,
                len = msg.text.len(),
                "mesh: inbound message received"
            );
            dispatch_message(
                msg.sender_key_prefix,
                &msg.text,
                host,
                cmd_tx,
                state,
                command_prefix,
                welcome_message,
                node_credential_ttl_days,
                flood_after_send,
            )
            .await;
        }

        // ── Queued message notification ───────────────────────────────────────
        // The bridge has a message waiting; fetch it with SyncNextMessage.
        InboundFrame::MsgWaiting => {
            info!("mesh: MsgWaiting — fetching next queued message");
            if cmd_tx.send(OutboundFrame::SyncNextMessage).await.is_err() {
                warn!("mesh: could not enqueue SyncNextMessage — cmd channel closed");
            }
        }

        // ── Contact advertisements ────────────────────────────────────────────
        // Record in the shared advert bus so the web UI can display them.
        // Sessions are minted on first DM, not on advert.
        InboundFrame::Advert { pubkey } => {
            host.advert_bus().upsert_short(pubkey);
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
            host.advert_bus().upsert(
                contact.pubkey,
                contact.name.clone(),
                contact.adv_type,
                contact.gps_lat,
                contact.gps_lon,
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

        // ── Everything else ───────────────────────────────────────────────────
        other => {
            debug!("mesh: ignoring frame {other:?}");
        }
    }
}

/// Parse a direct message text, route it through the host, and send the reply.
#[allow(clippy::too_many_arguments)]
async fn dispatch_message(
    sender_prefix: [u8; 6],
    text: &str,
    host: &Arc<dyn Host>,
    cmd_tx: &mpsc::Sender<OutboundFrame>,
    state: &Arc<Mutex<SessionState>>,
    command_prefix: Option<char>,
    welcome_message: &str,
    node_credential_ttl_days: u32,
    flood_after_send: bool,
) {
    // ── Get or create a session for this node ─────────────────────────────────
    let Some((session, is_new)) = get_or_create_session(sender_prefix, host, state).await else {
        // Session creation failed; the error was already logged inside
        // get_or_create_session.  Skip this message â the event loop continues.
        return;
    };

    if is_new {
        debug!(?session, prefix = ?sender_prefix, "mesh: new session minted");

        // Attempt auto-login via stored node credential (skip when TTL = 0).
        let auto_username = if node_credential_ttl_days > 0 {
            match host
                .mesh_node_restore(session, sender_prefix, node_credential_ttl_days)
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
        };

        let greeting = if let Some(ref username) = auto_username {
            format!("Welcome back, {username}! Type 'H' for commands.")
        } else {
            welcome_message.to_owned()
        };

        let greeting_empty = greeting.is_empty();
        let welcome_sent = !greeting_empty
            && cmd_tx
                .send(OutboundFrame::SendTxtMsg {
                    txt_type: TXT_TYPE_PLAIN,
                    attempt: 0,
                    timestamp: now_unix_secs(),
                    pubkey_prefix: sender_prefix,
                    text: greeting,
                })
                .await
                .is_ok();
        if !welcome_sent && !greeting_empty {
            warn!(
                ?session,
                "mesh: could not enqueue welcome — cmd channel closed"
            );
        }
        if welcome_sent && flood_after_send {
            let pubkey = state
                .lock()
                .expect("state mutex poisoned")
                .get_full_pubkey(&sender_prefix);
            if let Some(pubkey) = pubkey {
                let _ = cmd_tx.send(OutboundFrame::ResetPath { pubkey }).await;
            }
        }
    }

    // ── Determine if we're awaiting a workflow reply ──────────────────────────
    let awaiting_reply = state
        .lock()
        .expect("state mutex poisoned")
        .is_awaiting_reply(&sender_prefix);

    // ── Dedup radio retransmissions of all inbound messages ──────────────────
    // The radio layer retransmits packets; process only the first copy within
    // the dedup window. This covers both regular commands and workflow replies.
    if state
        .lock()
        .expect("state mutex poisoned")
        .dedup_message(&sender_prefix, text)
    {
        debug!("mesh: dropping retransmitted message (dedup)");
        return;
    }

    // ── Dedup mesh retransmissions of workflow replies ────────────────────────
    // A retransmitted password can arrive after login completes
    // (awaiting_reply=false). Drop it silently if it matches the most recently
    // processed WorkflowReply within the dedup window.
    if !awaiting_reply
        && state
            .lock()
            .expect("state mutex poisoned")
            .is_recent_workflow_reply(&sender_prefix, text)
    {
        debug!("mesh: dropping retransmitted workflow reply (dedup)");
        return;
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

    info!(?session, ?cmd, "mesh: dispatching command");

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
            let fresh = match host.create_session("mesh").await {
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
                if let Err(e) = host
                    .mesh_node_restore(fresh_sid, sender_prefix, node_credential_ttl_days)
                    .await
                {
                    warn!(?fresh_sid, "mesh: node_restore on refresh error: {e}");
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
    if node_credential_ttl_days > 0 {
        match &response {
            Response::LoggedIn { .. } => {
                if let Err(e) = host.mesh_node_bind(active_sid, sender_prefix).await {
                    warn!(?active_sid, "mesh: node_bind error: {e}");
                }
            }
            Response::LoggedOut => {
                if let Err(e) = host.mesh_node_unbind(sender_prefix).await {
                    warn!("mesh: node_unbind error: {e}");
                }
            }
            _ => {}
        }
    }

    // ── Update workflow-reply flag ────────────────────────────────────────────
    let is_prompt = matches!(response, Response::Prompt { .. });
    state
        .lock()
        .expect("state mutex poisoned")
        .set_awaiting_reply(&sender_prefix, is_prompt);

    // ── Collect frames to send back ───────────────────────────────────────────
    // MultiText delivers each element as a separate radio frame.
    // All other variants produce a single frame via format_response.
    let frames: Vec<String> = if let Response::MultiText(parts) = &response {
        parts.clone()
    } else {
        match format_response(&response) {
            Some(t) => vec![t],
            None => return,
        }
    };

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

        info!(
            ?session,
            len = reply_text.len(),
            frame = i + 1,
            total = frame_count,
            "mesh: sending reply to node"
        );

        let reply_sent = cmd_tx
            .send(OutboundFrame::SendTxtMsg {
                txt_type: TXT_TYPE_PLAIN,
                attempt: 0,
                timestamp: now_unix_secs(),
                pubkey_prefix: sender_prefix,
                text: reply_text,
            })
            .await
            .is_ok();
        if !reply_sent {
            warn!(
                ?session,
                "mesh: could not enqueue reply — cmd channel closed"
            );
            break;
        }
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
    let new_id = match host.create_session("mesh").await {
        Ok(id) => id,
        Err(e) => {
            // This should not happen in normal operation; log and use a dummy.
            warn!("mesh: host.create_session failed: {e}");
            // Re-check state in case another concurrent message beat us here.
            if let Some(sid) = state.lock().expect("state mutex poisoned").lookup(&prefix) {
                return Some((sid, false));
            }
            // We cannot proceed without a session, but we must NOT panic here.
            // This function is called from a detached tokio::spawn event-loop
            // task; a panic would kill the task permanently and silently, causing
            // the BBS to stop responding to all mesh nodes with no visible error.
            // Instead, log the failure and return None so the caller can skip
            // this message and keep the event loop alive for future messages.
            error!(
                "mesh: host.create_session failed and no fallback: {e}                  skipping this message to keep the event loop alive"
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
