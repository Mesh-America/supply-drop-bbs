//! `bbs-hello-transport` — a minimal TCP transport plugin for Supply Drop BBS.
//!
//! This crate is a **reference implementation** that demonstrates every
//! integration point for a native-Rust transport plugin.  Fork it as a
//! starting point for your own transport.
//!
//! ## What it demonstrates
//!
//! | Pattern | Where |
//! |---------|-------|
//! | Serde config with defaults | [`HelloConfig`] |
//! | `Plugin` lifecycle (init → start → stop) | [`HelloTransport`] |
//! | Spawning workers from `start()`, shutdown via `watch` | [`HelloTransport::start`] |
//! | Accepting TCP connections | `handle_connection` |
//! | Session lifecycle (create → commands → end) | `handle_connection` |
//! | `awaiting_reply` state machine | `handle_connection` |
//! | Push notifications via `TransportEngine::notify` | [`HelloTransport`] impl |
//! | `MockHost` in tests | inline `tests` module |
//!
//! ## Usage
//!
//! ```toml
//! # config.toml
//! [plugins.hello]
//! bind = "0.0.0.0:2323"
//! ```
//!
//! Register in `main.rs` alongside other transport plugins:
//!
//! ```ignore
//! let hello = HelloTransport::init(hello_config, Arc::clone(&host) as Arc<dyn Host>).await?;
//! hello.start().await?;
//! ```

use std::sync::Arc;

use async_trait::async_trait;
use bbs_plugin_api::{
    event::Notification, Command, Host, NotifyOutcome, Plugin, PluginError, Response, SessionId,
    TransportEngine, TransportError,
};
use serde::Deserialize;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch, Mutex},
};
use tracing::{info, warn};

// ── Config ─────────────────────────────────────────────────────────────────────

/// Configuration for the hello transport, loaded from `config.toml`.
///
/// ```toml
/// [plugins.hello]
/// bind = "0.0.0.0:2323"   # optional; defaults to 127.0.0.1:2323
/// ```
#[derive(Debug, Clone, Deserialize)]
pub struct HelloConfig {
    /// TCP address to listen on.
    ///
    /// Defaults to `127.0.0.1:2323`.  Use `0.0.0.0:<port>` to accept
    /// connections from any interface.  Specify port `0` to let the OS
    /// assign a free port (useful in tests).
    #[serde(default = "HelloConfig::default_bind")]
    pub bind: String,
}

impl HelloConfig {
    fn default_bind() -> String {
        "127.0.0.1:2323".to_owned()
    }
}

impl Default for HelloConfig {
    fn default() -> Self {
        Self {
            bind: Self::default_bind(),
        }
    }
}

// ── Internal state ─────────────────────────────────────────────────────────────

struct ActiveSession {
    session_id: SessionId,
    /// Sends outgoing text lines to the connection handler task.
    tx: mpsc::Sender<String>,
}

// ── HelloTransport ─────────────────────────────────────────────────────────────

/// Minimal single-connection TCP transport plugin.
///
/// Binds a port, accepts one client at a time, and bridges each line to
/// [`Host::process_command`].  When a client disconnects, the BBS session is
/// torn down and the transport is ready for the next connection.
///
/// [`TransportEngine::notify`] delivers push messages to the connected
/// client; if no client is currently connected the message is dropped.
///
/// # Intentional limitations
///
/// This is a *reference* crate — readable over clever:
///
/// - One concurrent connection.  Real transports accept many; see `bbs-mesh`
///   for a multi-session example.
/// - No TLS, no authentication, no input masking.
/// - Not registered as a Cargo feature in the host binary.  Fork and wire it
///   in yourself.
pub struct HelloTransport {
    config: HelloConfig,
    host: Arc<dyn Host>,
    /// The single active connection slot.  `None` when no client is connected.
    session: Arc<Mutex<Option<ActiveSession>>>,
    shutdown_tx: watch::Sender<bool>,
}

#[async_trait]
impl Plugin for HelloTransport {
    type Config = HelloConfig;

    fn name(&self) -> &'static str {
        "hello-transport"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    async fn init(config: Self::Config, host: Arc<dyn Host>) -> Result<Self, PluginError> {
        Ok(Self {
            config,
            host,
            session: Arc::new(Mutex::new(None)),
            shutdown_tx: watch::channel(false).0,
        })
    }

    async fn start(&self) -> Result<(), PluginError> {
        let listener = TcpListener::bind(&self.config.bind).await.map_err(|e| {
            PluginError::InvalidConfig(format!(
                "hello-transport: could not bind '{}': {e}",
                self.config.bind
            ))
        })?;

        info!(bind = %self.config.bind, "hello-transport listening");

        let host = Arc::clone(&self.host);
        let session = Arc::clone(&self.session);
        let mut shutdown_rx = self.shutdown_tx.subscribe();

        // Accept loop: runs until stop() fires the shutdown watch.
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    accept = listener.accept() => match accept {
                        Ok((stream, addr)) => {
                            info!(%addr, "hello-transport: new connection");
                            tokio::spawn(handle_connection(
                                stream,
                                Arc::clone(&host),
                                Arc::clone(&session),
                            ));
                        }
                        Err(e) => warn!("hello-transport: accept error: {e}"),
                    },
                    _ = shutdown_rx.changed() => break,
                }
            }
        });

        Ok(())
    }

    async fn stop(&self) -> Result<(), PluginError> {
        let _ = self.shutdown_tx.send(true);
        // End the active session.  handle_connection also calls end_session
        // on disconnect; end_session is idempotent so the double-call is safe.
        if let Some(active) = self.session.lock().await.take() {
            let _ = self.host.end_session(active.session_id).await;
        }
        Ok(())
    }
}

#[async_trait]
impl TransportEngine for HelloTransport {
    async fn notify(
        &self,
        session: SessionId,
        payload: Notification,
    ) -> Result<NotifyOutcome, TransportError> {
        let guard = self.session.lock().await;
        let Some(active) = guard.as_ref() else {
            return Ok(NotifyOutcome::Dropped);
        };
        if active.session_id != session {
            return Ok(NotifyOutcome::Dropped);
        }
        let text = render_notification(&payload);
        match active.tx.try_send(text) {
            Ok(()) => Ok(NotifyOutcome::Delivered),
            Err(_) => Ok(NotifyOutcome::Dropped),
        }
    }
}

// ── Connection handler ─────────────────────────────────────────────────────────

async fn handle_connection(
    stream: TcpStream,
    host: Arc<dyn Host>,
    session_slot: Arc<Mutex<Option<ActiveSession>>>,
) {
    // Allocate a BBS session for this TCP connection.
    let session_id = match host.create_session("hello-transport").await {
        Ok(id) => id,
        Err(e) => {
            warn!("hello-transport: create_session failed: {e}");
            return;
        }
    };

    // Channel used by notify() to push text into this connection task.
    let (notify_tx, mut notify_rx) = mpsc::channel::<String>(16);
    *session_slot.lock().await = Some(ActiveSession {
        session_id,
        tx: notify_tx,
    });

    let (read_half, mut write_half) = stream.into_split();
    let mut lines = BufReader::new(read_half).lines();
    let mut awaiting_reply = false;

    let _ = write_half
        .write_all(b"Welcome to Supply Drop BBS!\r\n")
        .await;

    loop {
        tokio::select! {
            result = lines.next_line() => match result {
                Ok(Some(raw)) => {
                    // Parse the raw line into a BBS command.  The awaiting_reply
                    // flag is set when the previous response was a Prompt — the
                    // next line is treated as a WorkflowReply rather than a
                    // fresh command.
                    let cmd = Command::parse(&raw, awaiting_reply);
                    let response = match host.process_command(session_id, cmd).await {
                        Ok(r) => r,
                        Err(e) => Response::Error(format!("{e}")),
                    };
                    awaiting_reply = response.sets_awaiting_reply();
                    if let Some(text) = response.render() {
                        let _ = write_half.write_all(text.as_bytes()).await;
                        let _ = write_half.write_all(b"\r\n").await;
                    }
                }
                _ => break, // EOF or IO error — client disconnected
            },
            Some(text) = notify_rx.recv() => {
                // Out-of-band push from notify() — deliver immediately.
                let _ = write_half.write_all(text.as_bytes()).await;
                let _ = write_half.write_all(b"\r\n").await;
            },
        }
    }

    // Tear down: clear the slot, tell the host the session is gone.
    *session_slot.lock().await = None;
    let _ = host.end_session(session_id).await;
    info!(?session_id, "hello-transport: connection closed");
}

fn render_notification(n: &Notification) -> String {
    match n {
        Notification::Text(t) => t.clone(),
        Notification::MailWaiting { count } => format!(
            "You have {count} unread message{}. Type 'mail' to read.",
            if *count == 1 { "" } else { "s" }
        ),
        Notification::SystemEvent(s) => format!("[system] {s}"),
        _ => "[notification]".to_owned(),
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bbs_plugin_api::{event::Notification, testing::MockHost, Host, Plugin};

    use super::*;

    #[test]
    fn default_config_bind() {
        assert_eq!(HelloConfig::default().bind, "127.0.0.1:2323");
    }

    #[test]
    fn render_text_notification() {
        let n = Notification::Text("hello".to_owned());
        assert_eq!(render_notification(&n), "hello");
    }

    #[test]
    fn render_mail_waiting_single() {
        let n = Notification::MailWaiting { count: 1 };
        let s = render_notification(&n);
        assert!(s.contains("1 unread message."), "got: {s}");
    }

    #[test]
    fn render_mail_waiting_plural() {
        let n = Notification::MailWaiting { count: 3 };
        let s = render_notification(&n);
        assert!(s.contains("3 unread messages."), "got: {s}");
    }

    #[test]
    fn render_system_event() {
        let n = Notification::SystemEvent("reboot in 60s".to_owned());
        assert_eq!(render_notification(&n), "[system] reboot in 60s");
    }

    /// Demonstrates MockHost usage: init → start → stop without panicking.
    ///
    /// Port `0` lets the OS assign a free port so parallel test runs never
    /// fight over the same address.
    #[tokio::test]
    async fn init_start_stop_with_mock_host() {
        let host = Arc::new(MockHost::new());
        let config = HelloConfig {
            bind: "127.0.0.1:0".to_owned(),
        };
        let transport = HelloTransport::init(config, host as Arc<dyn Host>)
            .await
            .unwrap();
        transport.start().await.unwrap();
        transport.stop().await.unwrap();
    }
}
