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

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bbs_plugin_api::{
    event::{Notification, NotifyOutcome},
    error::{PluginError, TransportError},
    identity::SessionId,
    plugin::Plugin,
    transport::TransportEngine,
    Host, Response,
};
use meshcore_companion::{
    client::{ClientConfig, ClientEvent, CompanionClient},
    constants::TXT_TYPE_PLAIN,
    frame::OutboundFrame,
};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::{
    command::{format_response, parse_command, render_notification},
    config::MeshConfig,
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
    /// This is where the TCP connection attempt begins (in a background task
    /// spawned by [`CompanionClient::connect`]).  The `start()` call is what
    /// actually begins processing events from that connection.
    async fn init(config: Self::Config, host: Arc<dyn Host>) -> Result<Self, PluginError> {
        let client_config = ClientConfig {
            addr: config.addr,
            app_target_version: config.app_target_version,
            reconnect_delay_initial: config.reconnect_delay_initial(),
            reconnect_delay_max: config.reconnect_delay_max(),
        };

        let client = CompanionClient::connect(client_config);
        let cmd_tx = client.sender();
        let (shutdown_tx, _) = watch::channel(false);

        info!(addr = %config.addr, "mesh transport initialised");

        Ok(Self {
            host,
            cmd_tx,
            state: Arc::new(Mutex::new(SessionState::default())),
            client_slot: Mutex::new(Some(client)),
            shutdown_tx,
            command_prefix: config.command_prefix,
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

        tokio::spawn(event_loop(client, host, cmd_tx, state, shutdown_rx, prefix));

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
            debug!(?session, "notify: no mesh node mapped to session — dropping");
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

/// Background task: receive [`ClientEvent`]s and dispatch them.
///
/// Runs until the shutdown watch fires or the companion client channel closes.
async fn event_loop(
    mut client: CompanionClient,
    host: Arc<dyn Host>,
    cmd_tx: mpsc::Sender<OutboundFrame>,
    state: Arc<Mutex<SessionState>>,
    mut shutdown_rx: watch::Receiver<bool>,
    command_prefix: Option<char>,
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
                            "mesh: radio bridge connected"
                        );
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
                        handle_frame(frame, &host, &cmd_tx, &state, command_prefix).await;
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
async fn handle_frame(
    frame: meshcore_companion::frame::InboundFrame,
    host: &Arc<dyn Host>,
    cmd_tx: &mpsc::Sender<OutboundFrame>,
    state: &Arc<Mutex<SessionState>>,
    command_prefix: Option<char>,
) {
    use meshcore_companion::frame::InboundFrame;

    match frame {
        // ── Direct text messages (v1/v2 and v3 with SNR) ─────────────────────
        InboundFrame::ContactMsgRecv(msg) | InboundFrame::ContactMsgRecvV3(msg) => {
            // Only handle plain-text messages; CLI data and signed frames are
            // not BBS commands.
            if msg.txt_type != meshcore_companion::constants::TXT_TYPE_PLAIN {
                debug!("mesh: ignoring non-plain-text ContactMsg (txt_type={})", msg.txt_type);
                return;
            }
            dispatch_message(msg.sender_key_prefix, &msg.text, host, cmd_tx, state, command_prefix).await;
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
        // Nodes that advertise themselves but haven't messaged yet.  We log
        // them but don't create sessions (sessions are minted on first DM).
        InboundFrame::Advert { pubkey } => {
            debug!(prefix = ?&pubkey[..6], "mesh: short advert received");
        }
        InboundFrame::NewAdvert(contact) => {
            debug!(name = %contact.name, "mesh: full advert (new contact) received");
        }

        // ── Everything else ───────────────────────────────────────────────────
        other => {
            debug!("mesh: ignoring frame {other:?}");
        }
    }
}

/// Parse a direct message text, route it through the host, and send the reply.
async fn dispatch_message(
    sender_prefix: [u8; 6],
    text: &str,
    host: &Arc<dyn Host>,
    cmd_tx: &mpsc::Sender<OutboundFrame>,
    state: &Arc<Mutex<SessionState>>,
    command_prefix: Option<char>,
) {
    // ── Get or create a session for this node ─────────────────────────────────
    let (session, is_new) = get_or_create_session(sender_prefix, host, state).await;

    if is_new {
        debug!(?session, prefix = ?sender_prefix, "mesh: new session minted");
    }

    // ── Determine if we're awaiting a workflow reply ──────────────────────────
    let awaiting_reply = state
        .lock()
        .expect("state mutex poisoned")
        .is_awaiting_reply(&sender_prefix);

    // ── Parse the command ─────────────────────────────────────────────────────
    let Some(cmd) = parse_command(text, command_prefix, awaiting_reply) else {
        // Message doesn't match prefix and no workflow active — silently ignore.
        debug!("mesh: message ignored (no prefix match, no active workflow)");
        return;
    };

    debug!(?session, ?cmd, "mesh: dispatching command");

    // ── Process through the host ──────────────────────────────────────────────
    let response = match host.process_command(session, cmd).await {
        Ok(r) => r,
        Err(e) => {
            warn!(?session, "mesh: host returned error: {e}");
            Response::Error(format!("{e}"))
        }
    };

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
        warn!(?session, "mesh: could not enqueue reply — cmd channel closed");
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
