//! [`ProcessTransport`] — a single externally-spawned transport plugin.
//!
//! Spawns a child process and bridges the IPC protocol to the Supply Drop
//! [`Plugin`] + [`TransportEngine`] interface.  The child process owns its
//! network connections; this struct translates between JSON IPC frames and
//! `Command`/`Response`/`Notification` types.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use bbs_plugin_api::event::Notification;
use bbs_plugin_api::registry::ProcessPluginConfig;
use bbs_plugin_api::{
    Command, Host, PluginError, Response, SessionId, TransportEngine, TransportError,
};
use bbs_plugin_api::{NotifyOutcome, Plugin};
use serde_json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command as TokioCommand};
use tokio::sync::{mpsc, watch, Mutex};
use tracing::{debug, info, warn};

use crate::ipc::{HostMsg, PluginMsg};

// -- Exit event ----------------------------------------------------------------

/// Signals sent from the process-watcher task to the manager's exit-handling loop.
///
/// The channel always exists (regardless of `restart_on_crash`) so the manager
/// can learn about *any* process exit and keep `PluginState` accurate.
#[derive(Debug)]
pub(crate) enum ExitEvent {
    /// The process exited with a non-zero code and `restart_on_crash` is true.
    /// The manager should restart the plugin.
    Crash,
    /// The process exited cleanly (code 0) **or** `restart_on_crash` is false.
    /// The manager should transition the plugin state to `Stopped`.
    Stopped,
}

// ── Session state ─────────────────────────────────────────────────────────────

struct SessionEntry {
    session_id: SessionId,
    /// True when the last Response was a Prompt — next recv is WorkflowReply.
    awaiting_reply: bool,
}

struct SessionMap {
    /// plugin conn id → session entry
    by_id: HashMap<String, SessionEntry>,
    /// SessionId → plugin conn id (reverse lookup for notify)
    by_session: HashMap<SessionId, String>,
}

impl SessionMap {
    fn new() -> Self {
        Self {
            by_id: HashMap::new(),
            by_session: HashMap::new(),
        }
    }
}

// ── ProcessTransport ──────────────────────────────────────────────────────────

/// Manages a single externally-spawned transport plugin process.
pub struct ProcessTransport {
    /// Stable name, leaked to `&'static str` for `host.create_session()`.
    transport_name: &'static str,
    config: ProcessPluginConfig,
    host: Arc<dyn Host>,
    /// Sender for JSON lines to the child's stdin.  Set in `start()`.
    ///
    /// Unbounded so that callers are never blocked waiting for the child's
    /// stdin writer to drain.  Back-pressure from a slow process manifests as
    /// growing memory rather than a blocked async task.
    stdin_tx: OnceLock<mpsc::UnboundedSender<String>>,
    sessions: Arc<Mutex<SessionMap>>,
    /// 0 = unlimited; set from the plugin's `ready` message.
    payload_limit: Arc<AtomicUsize>,
    /// Version string reported by the plugin in its `ready` message.
    plugin_version: Arc<std::sync::Mutex<Option<String>>>,
    shutdown_tx: watch::Sender<bool>,
    /// Fires an [`ExitEvent`] whenever the child process exits.
    /// Always populated so both crash and clean exits are reported to the manager.
    exit_tx: mpsc::Sender<ExitEvent>,
    /// Taken once by the manager to wire up the exit-handling loop.
    exit_rx: std::sync::Mutex<Option<mpsc::Receiver<ExitEvent>>>,
}

impl ProcessTransport {
    pub(crate) fn new(config: ProcessPluginConfig, host: Arc<dyn Host>) -> Self {
        // Leak the name string once to get a &'static str.
        // This is a deliberate one-time allocation per plugin name.
        let transport_name: &'static str = Box::leak(config.name.clone().into_boxed_str());
        let (exit_tx, exit_rx) = mpsc::channel(4);
        Self {
            transport_name,
            config,
            host,
            stdin_tx: OnceLock::new(),
            sessions: Arc::new(Mutex::new(SessionMap::new())),
            payload_limit: Arc::new(AtomicUsize::new(0)),
            plugin_version: Arc::new(std::sync::Mutex::new(None)),
            shutdown_tx: watch::channel(false).0,
            exit_tx,
            exit_rx: std::sync::Mutex::new(Some(exit_rx)),
        }
    }

    /// Take the exit receiver so the manager can drive the exit-handling loop.
    ///
    /// Returns `None` when already taken.
    /// Must be called before [`Plugin::start`].
    pub(crate) fn take_exit_receiver(&self) -> Option<mpsc::Receiver<ExitEvent>> {
        self.exit_rx.lock().expect("exit_rx poisoned").take()
    }

    /// Version string reported by the plugin in its `ready` message.
    /// Returns `None` if the plugin has not yet sent `ready` or omitted the field.
    pub(crate) fn reported_version(&self) -> Option<String> {
        self.plugin_version
            .lock()
            .expect("plugin_version poisoned")
            .clone()
    }

    /// Spawn the child and return it along with the stdin sender channel.
    fn spawn_child(
        config: &ProcessPluginConfig,
    ) -> Result<(Child, mpsc::UnboundedSender<String>), PluginError> {
        let mut cmd = TokioCommand::new(&config.command);
        cmd.args(&config.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| {
            PluginError::InvalidConfig(format!(
                "process transport '{}': failed to spawn '{}': {e}",
                config.name, config.command
            ))
        })?;

        let stdin = child.stdin.take().expect("stdin piped");
        // Use an unbounded channel so the caller is never blocked waiting for
        // the child's stdin writer to drain (BUG-11).  The writer task owns
        // the receiver and drains it at the speed of the child process.
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();

        // Dedicated writer task: reads from the channel and writes to stdin.
        tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(line) = rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
                if stdin.write_all(b"\n").await.is_err() {
                    break;
                }
            }
        });

        Ok((child, tx))
    }
}

fn render_notification(n: &Notification) -> String {
    match n {
        Notification::Text(t) => t.clone(),
        Notification::MailWaiting { count } => format!(
            "You have {} unread message{}. Type 'mail' to read.",
            count,
            if *count == 1 { "" } else { "s" }
        ),
        Notification::SystemEvent(s) => format!("[system] {s}"),
        _ => "[notification]".to_owned(),
    }
}

fn truncate_to_limit(text: String, limit: usize) -> String {
    if limit == 0 || text.len() <= limit {
        return text;
    }
    let suffix = "…";
    let cut = limit.saturating_sub(suffix.len());
    // Cut at a char boundary.
    let safe_cut = text
        .char_indices()
        .take_while(|(i, _)| *i < cut)
        .last()
        .map(|(i, c)| i + c.len_utf8())
        .unwrap_or(0);
    format!("{}{suffix}", &text[..safe_cut])
}

#[async_trait]
impl Plugin for ProcessTransport {
    type Config = ProcessPluginConfig;

    fn name(&self) -> &'static str {
        self.transport_name
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    async fn init(config: Self::Config, host: Arc<dyn Host>) -> Result<Self, PluginError> {
        Ok(Self::new(config, host))
    }

    async fn start(&self) -> Result<(), PluginError> {
        if !self.config.enabled {
            info!(
                plugin = self.config.name,
                "process transport disabled — skipping"
            );
            return Ok(());
        }

        let (mut child, tx) = Self::spawn_child(&self.config)?;
        self.stdin_tx
            .set(tx.clone())
            .map_err(|_| PluginError::StartFailed("already started".into()))?;

        let stdout = child.stdout.take().expect("stdout piped");
        let stderr = child.stderr.take().expect("stderr piped");

        let sessions = Arc::clone(&self.sessions);
        let host = Arc::clone(&self.host);
        let transport_name = self.transport_name;
        let payload_limit = Arc::clone(&self.payload_limit);
        let plugin_version = Arc::clone(&self.plugin_version);
        let stdin_tx = tx.clone();
        let mut shutdown_rx = self.shutdown_tx.subscribe();
        let plugin_name = self.config.name.clone();

        // Stdout reader task — main IPC dispatch loop.
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                tokio::select! {
                    line = lines.next_line() => {
                        match line {
                            Ok(Some(raw)) => {
                                handle_plugin_msg(
                                    &raw, &sessions, &host, transport_name,
                                    &payload_limit, &plugin_version, &stdin_tx,
                                ).await;
                            }
                            _ => {
                                info!(plugin = %plugin_name, "process transport stdout closed");
                                break;
                            }
                        }
                    }
                    _ = shutdown_rx.changed() => break,
                }
            }
            // Clean up all sessions when the process exits.
            let mut map = sessions.lock().await;
            let session_ids: Vec<SessionId> = map.by_id.values().map(|e| e.session_id).collect();
            map.by_id.clear();
            map.by_session.clear();
            drop(map);
            for sid in session_ids {
                let _ = host.end_session(sid).await;
            }
        });

        // Stderr reader task — captures plugin logs.
        let plugin_name2 = self.config.name.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                warn!(plugin = %plugin_name2, "{line}");
            }
        });

        // Process watcher task — detects exit and notifies the manager.
        //
        // Always fires an ExitEvent so the manager can update PluginState:
        //   - non-zero exit + restart_on_crash=true  -> ExitEvent::Crash
        //   - all other exits (clean or no-restart)  -> ExitEvent::Stopped
        let plugin_name3 = self.config.name.clone();
        let restart = self.config.restart_on_crash;
        let exit_tx = self.exit_tx.clone();
        tokio::spawn(async move {
            match child.wait().await {
                Ok(status) => {
                    if restart && !status.success() {
                        warn!(
                            plugin = %plugin_name3,
                            "process exited with {status} — notifying manager for restart"
                        );
                        let _ = exit_tx.send(ExitEvent::Crash).await;
                    } else {
                        info!(plugin = %plugin_name3, "process exited with {status}");
                        let _ = exit_tx.send(ExitEvent::Stopped).await;
                    }
                }
                Err(e) => {
                    warn!(plugin = %plugin_name3, "wait() error: {e}");
                    // Treat a wait error as a clean stop so the manager does not
                    // leave the plugin stuck in Running state forever.
                    let _ = exit_tx.send(ExitEvent::Stopped).await;
                }
            }
        });

        info!(plugin = self.config.name, "process transport started");
        Ok(())
    }

    async fn stop(&self) -> Result<(), PluginError> {
        let _ = self.shutdown_tx.send(true);
        if let Some(tx) = self.stdin_tx.get() {
            if let Ok(msg) = serde_json::to_string(&HostMsg::Shutdown) {
                let _ = tx.send(msg);
            }
        }
        Ok(())
    }
}

#[async_trait]
impl TransportEngine for ProcessTransport {
    async fn notify(
        &self,
        session: SessionId,
        payload: Notification,
    ) -> Result<NotifyOutcome, TransportError> {
        let map = self.sessions.lock().await;
        let Some(plugin_id) = map.by_session.get(&session).cloned() else {
            return Ok(NotifyOutcome::Dropped);
        };
        drop(map);

        let text = render_notification(&payload);
        let Some(tx) = self.stdin_tx.get() else {
            return Ok(NotifyOutcome::PermanentFailure(
                "transport not started".into(),
            ));
        };

        let msg = HostMsg::Send {
            id: plugin_id,
            text,
            hide_input: false,
        };
        match serde_json::to_string(&msg) {
            Ok(line) => match tx.send(line) {
                Ok(()) => Ok(NotifyOutcome::Delivered),
                // UnboundedSender::send only fails when the receiver is dropped
                // (i.e. the writer task exited because the process died).
                Err(_) => Ok(NotifyOutcome::Dropped),
            },
            Err(e) => Ok(NotifyOutcome::PermanentFailure(e.to_string())),
        }
    }
}

// ── Message dispatch ──────────────────────────────────────────────────────────

async fn handle_plugin_msg(
    raw: &str,
    sessions: &Arc<Mutex<SessionMap>>,
    host: &Arc<dyn Host>,
    transport_name: &'static str,
    payload_limit: &Arc<AtomicUsize>,
    plugin_version: &Arc<std::sync::Mutex<Option<String>>>,
    stdin_tx: &mpsc::UnboundedSender<String>,
) {
    let msg: PluginMsg = match serde_json::from_str(raw) {
        Ok(m) => m,
        Err(e) => {
            warn!("process transport: malformed IPC message: {e} — raw: {raw}");
            return;
        }
    };

    match msg {
        PluginMsg::Ready {
            payload_limit: limit,
            version,
        } => {
            payload_limit.store(limit, Ordering::Relaxed);
            if let Some(v) = version {
                *plugin_version.lock().expect("plugin_version poisoned") = Some(v);
            }
            debug!(transport = transport_name, limit, "plugin ready");
        }

        PluginMsg::Open { id } => {
            match host.create_session(transport_name).await {
                Ok(session_id) => {
                    let mut map = sessions.lock().await;
                    map.by_id.insert(
                        id.clone(),
                        SessionEntry {
                            session_id,
                            awaiting_reply: false,
                        },
                    );
                    map.by_session.insert(session_id, id.clone());
                    debug!(transport = transport_name, conn = %id, ?session_id, "session opened");
                }
                Err(e) => {
                    warn!(transport = transport_name, conn = %id, "create_session failed: {e}");
                    // Kick the connection so the plugin doesn't expect IPC responses.
                    send_kick(stdin_tx, &id);
                }
            }
        }

        PluginMsg::Recv { id, line } => {
            let (session_id, awaiting_reply) = {
                let map = sessions.lock().await;
                match map.by_id.get(&id) {
                    Some(e) => (e.session_id, e.awaiting_reply),
                    None => {
                        warn!(transport = transport_name, conn = %id, "recv for unknown conn");
                        return;
                    }
                }
            };

            let cmd = Command::parse(&line, awaiting_reply);
            let response = match host.process_command(session_id, cmd).await {
                Ok(r) => r,
                Err(e) => Response::Error(format!("{e}")),
            };

            let new_awaiting = response.sets_awaiting_reply();
            let hide_input = response.hides_next_input();

            {
                let mut map = sessions.lock().await;
                if let Some(e) = map.by_id.get_mut(&id) {
                    e.awaiting_reply = new_awaiting;
                }
            }

            if let Some(mut text) = response.render() {
                let limit = payload_limit.load(Ordering::Relaxed);
                text = truncate_to_limit(text, limit);
                send_text(stdin_tx, &id, text, hide_input);
            }
        }

        PluginMsg::Close { id } => {
            let session_id = {
                let mut map = sessions.lock().await;
                if let Some(entry) = map.by_id.remove(&id) {
                    map.by_session.remove(&entry.session_id);
                    Some(entry.session_id)
                } else {
                    None
                }
            };
            if let Some(sid) = session_id {
                let _ = host.end_session(sid).await;
                debug!(transport = transport_name, conn = %id, "session closed");
            }
        }
    }
}

fn send_text(tx: &mpsc::UnboundedSender<String>, id: &str, text: String, hide_input: bool) {
    let msg = HostMsg::Send {
        id: id.to_owned(),
        text,
        hide_input,
    };
    if let Ok(line) = serde_json::to_string(&msg) {
        let _ = tx.send(line);
    }
}

fn send_kick(tx: &mpsc::UnboundedSender<String>, id: &str) {
    let msg = HostMsg::Kick { id: id.to_owned() };
    if let Ok(line) = serde_json::to_string(&msg) {
        let _ = tx.send(line);
    }
}

#[cfg(test)]
mod tests {
    use super::truncate_to_limit;

    #[test]
    fn zero_limit_is_noop() {
        let s = "hello world".to_owned();
        assert_eq!(truncate_to_limit(s.clone(), 0), s);
    }

    #[test]
    fn text_at_or_below_limit_is_unchanged() {
        let s = "hello".to_owned();
        assert_eq!(truncate_to_limit(s.clone(), 10), s);
        assert_eq!(truncate_to_limit(s.clone(), 5), s);
    }

    #[test]
    fn text_over_limit_gets_ellipsis() {
        let result = truncate_to_limit("hello world".to_owned(), 8);
        // "…" is 3 bytes, so cut point is 5 bytes = "hello"
        assert!(
            result.ends_with('…'),
            "result should end with ellipsis: {result:?}"
        );
        assert!(
            result.len() <= 8,
            "result must fit in limit: len={} result={result:?}",
            result.len()
        );
    }

    #[test]
    fn truncation_respects_char_boundaries_multibyte() {
        // Each char is 4 bytes (😀 is U+1F600, 4 bytes in UTF-8).
        let s = "😀😀😀".to_owned(); // 12 bytes
                                     // Limit of 7: "…" (3 bytes) leaves 4 bytes for content = exactly one 😀
        let result = truncate_to_limit(s, 7);
        assert!(
            std::str::from_utf8(result.as_bytes()).is_ok(),
            "not valid UTF-8"
        );
        assert!(result.ends_with('…'));
    }

    #[test]
    fn truncation_of_empty_string_is_noop() {
        assert_eq!(truncate_to_limit(String::new(), 5), "");
    }

    #[test]
    fn limit_smaller_than_ellipsis_produces_just_ellipsis() {
        // If the limit is so small there's no room for any content, we still
        // get a valid (though possibly short) string ending in "…".
        let result = truncate_to_limit("hello world".to_owned(), 1);
        assert!(
            std::str::from_utf8(result.as_bytes()).is_ok(),
            "not valid UTF-8"
        );
    }
}
