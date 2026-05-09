//! # bbs-cli
//!
//! The CLI transport plugin for Supply Drop BBS.
//!
//! Listens on a Unix-domain socket and translates line-based text input from
//! local clients into BBS `Command`s, routing
//! responses and host-initiated `Notification`s
//! back to the same connection.
//!
//! ## Wire protocol
//!
//! The protocol is intentionally minimal — one UTF-8 line per message in each
//! direction:
//!
//! 1. On connect, the server sends a one-line banner ending in `\n`.
//! 2. The client sends command lines terminated by `\n`.
//! 3. The server sends one response line per command, terminated by `\n`.
//!    Multi-line responses are split into multiple lines by the host before
//!    reaching the transport.
//! 4. `Response::Prompt` with `hide_input = true` is prefixed with
//!    `\x00HIDDEN\x00` so a smart client can suppress terminal echo;
//!    dumb clients simply display the prompt text.
//! 5. Host-initiated notifications (from [`TransportEngine::notify`]) are
//!    interleaved as `\x00NOTIFY\x00<text>` lines so the client can
//!    distinguish them from command responses.
//! 6. EOF on the client side ends the session.
//!
//! ## Platform support
//!
//! Unix-domain sockets are a Unix primitive.  On non-Unix hosts this crate
//! compiles to a no-op stub that logs a warning and never accepts connections.
//! All CI and deployment targets (Linux on aarch64 / armv7 / x86-64) are Unix.
//!
//! ## Configuration
//!
//! See [`CliConfig`] for all available options.

#![allow(missing_docs)]

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use bbs_plugin_api::{
    error::{PluginError, TransportError},
    event::{Notification, NotifyOutcome},
    identity::SessionId,
    plugin::Plugin,
    transport::TransportEngine,
    Host,
};
use serde::{Deserialize, Serialize};

// `warn` is used in both the Unix impl and the non-Unix stub.
// `debug` and `info` are Unix-only.
#[cfg(not(unix))]
use tracing::warn;
#[cfg(unix)]
use tracing::{debug, info, warn};

// Unix-only: command/response types needed by the session implementation.
#[cfg(unix)]
use bbs_plugin_api::{identity::Username, Command, Response};

// ── Config ────────────────────────────────────────────────────────────────────

/// Configuration for the CLI transport plugin.
///
/// Deserialized from `[plugins.cli]` in the operator's TOML config.
/// All fields have defaults; an empty `[plugins.cli]` section is valid.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CliConfig {
    /// Whether to start the CLI listener.  Set `false` to disable at
    /// runtime without recompiling.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Unix socket path.  Defaults to `<data_dir>/cli.sock` (resolved
    /// by the BBS config loader at startup).
    #[serde(default)]
    pub socket: Option<PathBuf>,

    /// Octal permission mode applied to the socket file (e.g. `"0600"`).
    ///
    /// `"0600"` means only the BBS process owner can connect — appropriate
    /// for a single-operator setup.  `"0660"` lets members of the BBS group
    /// connect, which is useful when running the CLI under a different user.
    #[serde(default = "default_socket_mode")]
    pub socket_mode: String,

    /// Username or UID to `chown` the socket to after creation.
    /// Defaults to the BBS process user.
    #[serde(default)]
    pub socket_owner: Option<String>,
}

impl Default for CliConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            socket: None,
            socket_mode: default_socket_mode(),
            socket_owner: None,
        }
    }
}

fn default_enabled() -> bool {
    true
}

fn default_socket_mode() -> String {
    "0600".to_owned()
}

// ── Wire-protocol framing constants (Unix only) ───────────────────────────────

/// Prefix that marks a server-side notification line (host-initiated push).
///
/// Clients that understand the protocol can strip this prefix and present
/// the notification distinctly from command responses.
#[cfg(unix)]
const NOTIFY_PREFIX: &str = "\x00NOTIFY\x00";

/// Prefix added to prompt lines when `hide_input = true`.
///
/// Smart clients (e.g. the `supply-drop-bbs connect` subcommand) suppress
/// terminal echo when they see this prefix.
#[cfg(unix)]
const HIDDEN_PREFIX: &str = "\x00HIDDEN\x00";

// ── Session state (Unix only) ─────────────────────────────────────────────────

#[cfg(unix)]
#[derive(Default)]
struct CliState {
    /// `SessionId` → sender for pushing text lines to the connected client.
    ///
    /// The receiver lives inside each [`session_loop`] task; dropping the
    /// sender causes `notify_rx.recv()` to return `None`, which closes the
    /// connection cleanly.
    sessions: std::collections::HashMap<SessionId, tokio::sync::mpsc::Sender<String>>,
}

// ── CliTransport (Unix) ───────────────────────────────────────────────────────

/// The CLI transport plugin.
///
/// Listens on a Unix-domain socket; each connection becomes an interactive
/// BBS session.  Implements [`Plugin`] (lifecycle) and [`TransportEngine`]
/// (inbound command processing + outbound `notify()`).
///
/// # Construction
///
/// Always constructed via [`Plugin::init`].
///
/// # Shutdown
///
/// Call [`Plugin::stop`] or drop the value.  The accept loop detects the
/// watch-channel signal and exits; open sessions drain naturally as their
/// connections close.
#[cfg(unix)]
pub struct CliTransport {
    host: Arc<dyn Host>,
    state: Arc<std::sync::Mutex<CliState>>,
    shutdown_tx: tokio::sync::watch::Sender<bool>,
    /// Socket path — kept for cleanup on `stop()`.
    socket_path: PathBuf,
    /// Listener is held here until `start()` moves it into the accept loop.
    listener_slot: std::sync::Mutex<Option<tokio::net::UnixListener>>,
}

/// No-op stub for non-Unix platforms (Windows, etc.).
#[cfg(not(unix))]
pub struct CliTransport;

// ── Plugin impl (Unix) ────────────────────────────────────────────────────────

#[cfg(unix)]
#[async_trait]
impl Plugin for CliTransport {
    type Config = CliConfig;

    fn name(&self) -> &'static str {
        "cli"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    /// Bind the Unix socket and apply configured permissions.
    ///
    /// Any stale socket file from a previous run is removed before binding.
    /// Returns [`PluginError::InvalidConfig`] if `config.socket` was not
    /// resolved (i.e. it is still `None`).
    async fn init(config: Self::Config, host: Arc<dyn Host>) -> Result<Self, PluginError> {
        let socket_path = config.socket.clone().ok_or_else(|| {
            PluginError::InvalidConfig(
                "cli.socket path not resolved — check that data_dir is set".into(),
            )
        })?;

        // Remove a stale socket from a previous (unclean) shutdown.
        if socket_path.exists() {
            std::fs::remove_file(&socket_path).map_err(|e| {
                PluginError::InvalidConfig(format!(
                    "could not remove stale socket {}: {e}",
                    socket_path.display()
                ))
            })?;
        }

        let listener = tokio::net::UnixListener::bind(&socket_path).map_err(|e| {
            PluginError::StartFailed(format!("bind {}: {e}", socket_path.display()))
        })?;

        // Apply the configured octal permission mode.
        apply_socket_mode(&socket_path, &config.socket_mode);

        let (shutdown_tx, _) = tokio::sync::watch::channel(false);

        info!(socket = %socket_path.display(), "cli transport: socket bound");

        Ok(Self {
            host,
            state: Arc::new(std::sync::Mutex::new(CliState::default())),
            shutdown_tx,
            socket_path,
            listener_slot: std::sync::Mutex::new(Some(listener)),
        })
    }

    /// Move the listener into the accept-loop task and begin serving.
    async fn start(&self) -> Result<(), PluginError> {
        let listener = self
            .listener_slot
            .lock()
            .expect("listener_slot mutex poisoned")
            .take()
            .ok_or_else(|| PluginError::StartFailed("cli transport already started".into()))?;

        tokio::spawn(accept_loop(
            listener,
            Arc::clone(&self.host),
            Arc::clone(&self.state),
            self.shutdown_tx.subscribe(),
        ));

        info!(socket = %self.socket_path.display(), "cli transport started");
        Ok(())
    }

    /// Signal the accept loop to stop and remove the socket file.
    async fn stop(&self) -> Result<(), PluginError> {
        let _ = self.shutdown_tx.send(true);
        if let Err(e) = std::fs::remove_file(&self.socket_path) {
            // Not fatal — the file may have already been cleaned up.
            warn!(path = %self.socket_path.display(), "cli: could not remove socket on stop: {e}");
        }
        info!("cli transport stop requested");
        Ok(())
    }
}

// ── Plugin impl (non-Unix stub) ───────────────────────────────────────────────

#[cfg(not(unix))]
#[async_trait]
impl Plugin for CliTransport {
    type Config = CliConfig;

    fn name(&self) -> &'static str {
        "cli"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    async fn init(_config: Self::Config, _host: Arc<dyn Host>) -> Result<Self, PluginError> {
        warn!("CLI transport (Unix socket) is not supported on this platform — skipping");
        Ok(Self)
    }

    async fn start(&self) -> Result<(), PluginError> {
        Ok(())
    }

    async fn stop(&self) -> Result<(), PluginError> {
        Ok(())
    }
}

// ── TransportEngine impl (Unix) ───────────────────────────────────────────────

#[cfg(unix)]
#[async_trait]
impl TransportEngine for CliTransport {
    /// Push a notification to an active CLI session.
    ///
    /// Looks up the session's outbound sender and enqueues a prefixed line.
    /// Returns [`NotifyOutcome::Dropped`] if the session has no connected
    /// client (unknown session or the client already disconnected).
    async fn notify(
        &self,
        session: SessionId,
        payload: Notification,
    ) -> Result<NotifyOutcome, TransportError> {
        let tx = {
            let state = self.state.lock().expect("cli state mutex poisoned");
            state.sessions.get(&session).cloned()
        };

        let Some(tx) = tx else {
            debug!(?session, "cli notify: no active session — dropping");
            return Ok(NotifyOutcome::Dropped);
        };

        let line = format!("{}{}", NOTIFY_PREFIX, render_notification(&payload));
        match tx.send(line).await {
            Ok(()) => Ok(NotifyOutcome::Queued),
            // Receiver dropped — client disconnected between lookup and send.
            Err(_) => Ok(NotifyOutcome::Dropped),
        }
    }
}

// ── TransportEngine impl (non-Unix stub) ─────────────────────────────────────

#[cfg(not(unix))]
#[async_trait]
impl TransportEngine for CliTransport {
    async fn notify(
        &self,
        _session: SessionId,
        _payload: Notification,
    ) -> Result<NotifyOutcome, TransportError> {
        Ok(NotifyOutcome::Dropped)
    }
}

// ── Accept loop ───────────────────────────────────────────────────────────────

/// Background task: accept new connections and spawn a session for each.
#[cfg(unix)]
async fn accept_loop(
    listener: tokio::net::UnixListener,
    host: Arc<dyn Host>,
    state: Arc<std::sync::Mutex<CliState>>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        tokio::select! {
            result = listener.accept() => {
                match result {
                    Ok((stream, _addr)) => {
                        tokio::spawn(session_loop(
                            stream,
                            Arc::clone(&host),
                            Arc::clone(&state),
                        ));
                    }
                    Err(e) => {
                        // Transient accept errors (e.g. EMFILE) — log and
                        // continue; the loop will recover on the next accept.
                        warn!("cli: accept error: {e}");
                    }
                }
            }
            _ = shutdown_rx.changed() => {
                info!("cli: shutdown signal — accept loop exiting");
                break;
            }
        }
    }
}

// ── Session loop ──────────────────────────────────────────────────────────────

/// Per-connection task: read lines, dispatch commands, write responses.
#[cfg(unix)]
async fn session_loop(
    stream: tokio::net::UnixStream,
    host: Arc<dyn Host>,
    state: Arc<std::sync::Mutex<CliState>>,
) {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    // ── Create a BBS session ──────────────────────────────────────────────────
    let session_id = match host.create_session("cli").await {
        Ok(id) => id,
        Err(e) => {
            warn!("cli: could not create session: {e}");
            return;
        }
    };

    // ── Outbound channel for host-initiated notifications ─────────────────────
    let (notify_tx, mut notify_rx) = tokio::sync::mpsc::channel::<String>(16);
    state
        .lock()
        .expect("cli state mutex poisoned")
        .sessions
        .insert(session_id, notify_tx);

    // ── Split stream into read/write halves ───────────────────────────────────
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let mut awaiting_reply = false;

    // ── Welcome banner ────────────────────────────────────────────────────────
    let banner = format!(
        "Supply Drop BBS v{} — type 'help' for commands\n",
        env!("CARGO_PKG_VERSION")
    );
    if writer.write_all(banner.as_bytes()).await.is_err() {
        end_session(session_id, &host, &state).await;
        return;
    }

    debug!(?session_id, "cli: session started");

    // ── Event loop ────────────────────────────────────────────────────────────
    loop {
        tokio::select! {
            // Inbound: text line from the connected client.
            line_result = lines.next_line() => {
                let text = match line_result {
                    Ok(Some(t)) => t,
                    _ => {
                        // EOF or I/O error — the client disconnected.
                        debug!(?session_id, "cli: client disconnected");
                        break;
                    }
                };

                let cmd = parse_command(&text, awaiting_reply);
                debug!(?session_id, ?cmd, "cli: dispatching command");

                let response = match host.process_command(session_id, cmd).await {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(?session_id, "cli: host error: {e}");
                        Response::Error(format!("{e}"))
                    }
                };

                awaiting_reply = matches!(response, Response::Prompt { .. });

                if let Some(line) = format_response(&response) {
                    let line = format!("{line}\n");
                    if writer.write_all(line.as_bytes()).await.is_err() {
                        break;
                    }
                }
            }

            // Outbound: notification pushed by the host via notify().
            notification = notify_rx.recv() => {
                match notification {
                    Some(line) => {
                        let line = format!("{line}\n");
                        if writer.write_all(line.as_bytes()).await.is_err() {
                            break;
                        }
                    }
                    // Sender dropped (transport stopped) — exit cleanly.
                    None => break,
                }
            }
        }
    }

    end_session(session_id, &host, &state).await;
}

/// Remove the session from state and notify the host.
#[cfg(unix)]
async fn end_session(
    session_id: SessionId,
    host: &Arc<dyn Host>,
    state: &Arc<std::sync::Mutex<CliState>>,
) {
    state
        .lock()
        .expect("cli state mutex poisoned")
        .sessions
        .remove(&session_id);
    if let Err(e) = host.end_session(session_id).await {
        warn!(?session_id, "cli: end_session error: {e}");
    }
    debug!(?session_id, "cli: session ended");
}

// ── Command parsing ───────────────────────────────────────────────────────────

/// Parse a raw CLI input line into a [`Command`].
///
/// Unlike the mesh transport there is no command prefix — CLI users type
/// commands directly.  Workflow replies take priority when `awaiting_reply`
/// is set.
#[cfg(unix)]
fn parse_command(text: &str, awaiting_reply: bool) -> Command {
    let text = text.trim();

    if awaiting_reply {
        return Command::WorkflowReply {
            reply: text.to_owned(),
        };
    }

    if text.is_empty() {
        return Command::Unknown { raw: String::new() };
    }

    let (word, rest) = split_first_word(text);
    let keyword = word.to_ascii_lowercase();

    match keyword.as_str() {
        "h" | "help" | "?" => Command::Help {
            topic: rest.map(str::to_owned),
        },
        "register" => match rest.and_then(|s| Username::new(s).ok()) {
            Some(u) => Command::Register { username: u },
            None => Command::Help {
                topic: Some("register".to_owned()),
            },
        },
        "login" => match rest.and_then(|s| Username::new(s).ok()) {
            Some(u) => Command::Login { username: u },
            None => Command::Help {
                topic: Some("login".to_owned()),
            },
        },
        "logout" | "q" => Command::Logout,
        _ => Command::Unknown {
            raw: text.to_owned(),
        },
    }
}

#[cfg(unix)]
fn split_first_word(s: &str) -> (&str, Option<&str>) {
    match s.find(|c: char| c.is_ascii_whitespace()) {
        None => (s, None),
        Some(i) => {
            let rest = s[i..].trim_start();
            (&s[..i], if rest.is_empty() { None } else { Some(rest) })
        }
    }
}

// ── Response rendering ────────────────────────────────────────────────────────

/// Render a [`Response`] into the text line sent back to the client.
///
/// Returns `None` for response variants that carry no user-visible content.
#[cfg(unix)]
fn format_response(response: &Response) -> Option<String> {
    match response {
        Response::Text(t) => Some(t.clone()),
        Response::Prompt { text, hide_input } => {
            if *hide_input {
                Some(format!("{HIDDEN_PREFIX}{text}"))
            } else {
                Some(text.clone())
            }
        }
        Response::LoggedIn { user } => Some(format!(
            "Welcome, {}. Type 'help' for commands.",
            user.as_str()
        )),
        Response::LoggedOut => Some("Goodbye. Your session has ended.".to_owned()),
        Response::Error(e) => Some(format!("Error: {e}")),
        _ => None,
    }
}

// ── Notification rendering ────────────────────────────────────────────────────

#[cfg(unix)]
fn render_notification(notification: &Notification) -> String {
    match notification {
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

// ── Socket helpers ────────────────────────────────────────────────────────────

#[cfg(unix)]
fn apply_socket_mode(path: &std::path::Path, mode_str: &str) {
    use std::os::unix::fs::PermissionsExt;
    // Strip leading "0" (octal literal style like "0600").
    let digits = mode_str.trim_start_matches('0');
    if let Ok(mode) = u32::from_str_radix(digits, 8) {
        if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)) {
            warn!(path = %path.display(), "cli: could not set socket mode: {e}");
        }
    } else {
        warn!(mode = %mode_str, "cli: invalid socket_mode — must be octal string like '0600'");
    }
}

// ── Unit tests (Unix only — helpers are Unix-gated) ───────────────────────────

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn cmd(text: &str) -> Command {
        parse_command(text, false)
    }

    #[test]
    fn help_no_topic() {
        assert!(matches!(cmd("help"), Command::Help { topic: None }));
        assert!(matches!(cmd("?"), Command::Help { topic: None }));
        assert!(matches!(cmd("HELP"), Command::Help { topic: None }));
    }

    #[test]
    fn help_with_topic() {
        assert!(matches!(
            cmd("help rooms"),
            Command::Help { topic: Some(t) } if t == "rooms"
        ));
    }

    #[test]
    fn logout_and_quit() {
        assert!(matches!(cmd("logout"), Command::Logout));
        assert!(matches!(cmd("LOGOUT"), Command::Logout));
        assert!(matches!(cmd("q"), Command::Logout));
        assert!(matches!(cmd("Q"), Command::Logout));
    }

    #[test]
    fn whoami_is_unknown_on_cli() {
        assert!(matches!(cmd("whoami"), Command::Unknown { .. }));
    }

    #[test]
    fn h_is_help() {
        assert!(matches!(cmd("h"), Command::Help { topic: None }));
        assert!(matches!(cmd("H"), Command::Help { topic: None }));
    }

    #[test]
    fn unknown_keyword() {
        assert!(matches!(
            cmd("rooms"),
            Command::Unknown { raw } if raw == "rooms"
        ));
    }

    #[test]
    fn whitespace_trimmed() {
        assert!(matches!(cmd("  help  "), Command::Help { topic: None }));
    }

    #[test]
    fn awaiting_reply_wraps_any_text() {
        assert!(matches!(
            parse_command("help", true),
            Command::WorkflowReply { reply } if reply == "help"
        ));
        assert!(matches!(
            parse_command("s3cr3t!", true),
            Command::WorkflowReply { reply } if reply == "s3cr3t!"
        ));
    }

    #[test]
    fn format_text() {
        assert_eq!(
            format_response(&Response::Text("hello".into())),
            Some("hello".into())
        );
    }

    #[test]
    fn format_prompt_visible() {
        assert_eq!(
            format_response(&Response::Prompt {
                text: "Username:".into(),
                hide_input: false,
            }),
            Some("Username:".into())
        );
    }

    #[test]
    fn format_prompt_hidden_has_prefix() {
        let out = format_response(&Response::Prompt {
            text: "Password:".into(),
            hide_input: true,
        })
        .unwrap();
        assert!(out.starts_with(HIDDEN_PREFIX));
        assert!(out.contains("Password:"));
    }

    #[test]
    fn format_logged_in_contains_username() {
        let user = Username::new("alice").unwrap();
        let out = format_response(&Response::LoggedIn { user }).unwrap();
        assert!(out.contains("alice"));
    }

    #[test]
    fn format_error() {
        let out = format_response(&Response::Error("boom".into())).unwrap();
        assert!(out.contains("boom"));
    }

    #[test]
    fn render_mail_singular() {
        let t = render_notification(&Notification::MailWaiting { count: 1 });
        assert!(t.contains('1') && !t.contains("messages"));
    }

    #[test]
    fn render_mail_plural() {
        let t = render_notification(&Notification::MailWaiting { count: 3 });
        assert!(t.contains('3') && t.contains("messages"));
    }

    #[test]
    fn notify_prefix_in_rendered_output() {
        let t = render_notification(&Notification::Text("hi".into()));
        // render_notification itself doesn't add the prefix; notify() does.
        assert_eq!(t, "hi");
    }
}
