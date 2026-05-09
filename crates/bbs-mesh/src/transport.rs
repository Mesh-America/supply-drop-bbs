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

use async_trait::async_trait;
use bbs_plugin_api::{
    error::{HostError, PluginError, TransportError},
    event::{DomainEvent, Notification, NotifyOutcome},
    identity::SessionId,
    plugin::Plugin,
    transport::TransportEngine,
    Host, PermissionLevel, Response,
};
use meshcore_companion::{
    client::{ClientConfig, ClientEvent, CompanionClient, SerialConfig},
    constants::TXT_TYPE_PLAIN,
    frame::OutboundFrame,
};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::{
    command::{format_response, parse_command, render_notification},
    config::{ConnectionType, MeshConfig},
    session::SessionState,
};

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

        // Watch for advert-send requests from the web UI.
        let mut advert_send_rx = host.advert_bus().subscribe_send();
        let advert_cmd_tx = self.cmd_tx.clone();
        let mut advert_shutdown_rx = self.shutdown_tx.subscribe();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    result = advert_send_rx.recv() => {
                        match result {
                            Ok(flood) => {
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
                pubkey_prefix,
                text,
            })
            .await
            .map_err(|_| TransportError::ConnectionLost("companion client closed".into()))?;

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
                            pubkey_prefix: prefix,
                            text: "Your account has been validated. \
                                   You now have full access. Type 'H'."
                                .to_owned(),
                        })
                        .await;
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
                            pubkey_prefix: prefix,
                            text: format!(
                                "New registration: {} — type PENDING to review.",
                                user.as_str()
                            ),
                        })
                        .await;
                }
            }
        }
        _ => {}
    }
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
) {
    loop {
        tokio::select! {
            event = client.recv() => {
                match event {
                    None => {
                        info!("mesh: companion client channel closed — event loop exiting");
                        break;
                    }
                    Some(ClientEvent::Connected { self_info }) => {
                        info!(
                            node = %self_info.node_name,
                            freq_khz = self_info.frequency_khz,
                            "mesh: radio bridge connected — draining stale queue"
                        );
                        // Drain any messages that queued while we were offline so
                        // they don't corrupt in-progress workflows.
                        draining.store(true, Ordering::Relaxed);
                        let _ = cmd_tx.send(OutboundFrame::SyncNextMessage).await;
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
                        handle_frame(frame, &host, &cmd_tx, &state, command_prefix, &welcome_message, node_credential_ttl_days, &draining).await;
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
                debug!(
                    "mesh: discarding stale queued message from {}",
                    msg.sender_key_prefix[0]
                );
                let _ = cmd_tx.send(OutboundFrame::SyncNextMessage).await;
                return;
            }

            // Only handle plain-text messages; CLI data and signed frames are
            // not BBS commands.
            if msg.txt_type != meshcore_companion::constants::TXT_TYPE_PLAIN {
                debug!(
                    "mesh: ignoring non-plain-text ContactMsg (txt_type={})",
                    msg.txt_type
                );
                return;
            }
            dispatch_message(
                msg.sender_key_prefix,
                &msg.text,
                host,
                cmd_tx,
                state,
                command_prefix,
                welcome_message,
                node_credential_ttl_days,
            )
            .await;
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
            host.advert_bus().upsert_short(pubkey);
            debug!(prefix = ?&pubkey[..6], "mesh: short advert received");
        }
        InboundFrame::NewAdvert(contact) => {
            host.advert_bus().upsert(
                contact.pubkey,
                contact.name.clone(),
                contact.adv_type,
                contact.gps_lat,
                contact.gps_lon,
            );
            debug!(name = %contact.name, "mesh: full advert (new contact) received");
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
) {
    // ── Get or create a session for this node ─────────────────────────────────
    let (session, is_new) = get_or_create_session(sender_prefix, host, state).await;

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

        if !greeting.is_empty()
            && cmd_tx
                .send(OutboundFrame::SendTxtMsg {
                    txt_type: TXT_TYPE_PLAIN,
                    attempt: 0,
                    pubkey_prefix: sender_prefix,
                    text: greeting,
                })
                .await
                .is_err()
        {
            warn!(
                ?session,
                "mesh: could not enqueue welcome — cmd channel closed"
            );
        }
    }

    // ── Determine if we're awaiting a workflow reply ──────────────────────────
    let awaiting_reply = state
        .lock()
        .expect("state mutex poisoned")
        .is_awaiting_reply(&sender_prefix);

    // ── Dedup mesh retransmissions of workflow replies ────────────────────────
    // The mesh protocol retransmits packets; a retransmitted password can arrive
    // after login completes (awaiting_reply=false). Drop it silently if it
    // matches the most recently processed WorkflowReply within the dedup window.
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

    debug!(?session, ?cmd, "mesh: dispatching command");

    // ── Process through the host ──────────────────────────────────────────────
    // On UnknownSession (e.g. server restarted while the transport retained a
    // stale session ID), evict the stale mapping, mint a fresh session, and
    // retry the command once with the new ID.
    let response = match host.process_command(session, cmd.clone()).await {
        Ok(r) => r,
        Err(HostError::UnknownSession(stale)) => {
            debug!(?stale, "mesh: stale session — refreshing");
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
            // Attempt auto-login on the refreshed session before replaying the command.
            if node_credential_ttl_days > 0 {
                if let Err(e) = host
                    .mesh_node_restore(fresh_sid, sender_prefix, node_credential_ttl_days)
                    .await
                {
                    warn!(?fresh_sid, "mesh: node_restore on refresh error: {e}");
                }
            }
            match host.process_command(fresh_sid, cmd).await {
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
                if let Err(e) = host.mesh_node_bind(session, sender_prefix).await {
                    warn!(?session, "mesh: node_bind error: {e}");
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

    // ── Send the response back to the node ────────────────────────────────────
    let Some(reply_text) = format_response(&response) else {
        return;
    };

    if cmd_tx
        .send(OutboundFrame::SendTxtMsg {
            txt_type: TXT_TYPE_PLAIN,
            attempt: 0,
            pubkey_prefix: sender_prefix,
            text: reply_text,
        })
        .await
        .is_err()
    {
        warn!(
            ?session,
            "mesh: could not enqueue reply — cmd channel closed"
        );
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
) -> (SessionId, bool) {
    // Fast path: session already exists.
    if let Some(sid) = state.lock().expect("state mutex poisoned").lookup(&prefix) {
        return (sid, false);
    }

    // Slow path: mint a new session from the host.
    let new_id = match host.create_session("mesh").await {
        Ok(id) => id,
        Err(e) => {
            // This should not happen in normal operation; log and use a dummy.
            warn!("mesh: host.create_session failed: {e}");
            // Re-check state in case another concurrent message beat us here.
            if let Some(sid) = state.lock().expect("state mutex poisoned").lookup(&prefix) {
                return (sid, false);
            }
            // Fallback: can't proceed — caller will produce an error response.
            panic!("mesh: host.create_session failed and no fallback: {e}");
        }
    };

    let (sid, is_new) = state
        .lock()
        .expect("state mutex poisoned")
        .get_or_insert(prefix, new_id);

    (sid, is_new)
}
