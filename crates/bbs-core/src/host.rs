//! Concrete [`Host`] implementation backed by the bbs-core [`Database`].
//!
//! [`BbsHost`] is the single canonical implementation of the
//! [`bbs_plugin_api::Host`] trait. The supervisor in `main.rs` constructs one
//! `BbsHost`, wraps it in an `Arc`, and passes that `Arc<dyn Host>` to every
//! plugin at `init` time.
//!
//! ## Session lifecycle
//!
//! Sessions are held in memory; they are not persisted across restarts. The
//! session map is a `RwLock<HashMap<SessionId, SessionRecord>>` so concurrent
//! reads (from multiple transport plugins) don't block each other. The write
//! lock is taken only on `create_session` / `end_session`.
//!
//! ## Command processing
//!
//! For now only the basic meta-commands (Help, Whoami, Logout, Unknown) are
//! implemented. The authentication flow (Register / Login / WorkflowReply)
//! will be wired in once the credential store is exposed via the public API.
//!
//! ## Event bus
//!
//! A `broadcast::Sender<DomainEvent>` is created at construction. Every
//! caller of `Host::events` gets a fresh receiver; missed events (slow
//! consumers) produce `RecvError::Lagged`, which plugins must handle.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use bbs_plugin_api::{
    Command, DomainEvent, HostError, PermissionCtx, PermissionLevel, Response, SessionId,
};
use bbs_plugin_api::host::Host;
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, info};

use crate::db::Database;

// ── Session record ────────────────────────────────────────────────────────────

struct SessionRecord {
    #[allow(dead_code)]
    transport: String,
    username: Option<bbs_plugin_api::Username>,
    level: PermissionLevel,
}

// ── BbsHost ───────────────────────────────────────────────────────────────────

/// Concrete `Host` implementation backed by the bbs-core database.
///
/// Construct via [`BbsHost::new`] and wrap in an `Arc` before passing to
/// plugins:
///
/// ```rust,ignore
/// let host: Arc<dyn Host> = Arc::new(BbsHost::new(db));
/// ```
pub struct BbsHost {
    /// Persistence handle (Clone + Send + Sync).
    #[allow(dead_code)]
    db: Database,
    /// Event fanout channel. Capacity 256: slow consumers get `Lagged`.
    events_tx: broadcast::Sender<DomainEvent>,
    /// In-memory session map.
    sessions: RwLock<HashMap<SessionId, SessionRecord>>,
    /// Monotonically increasing session ID counter.
    next_id: AtomicU64,
}

impl BbsHost {
    /// Construct a new `BbsHost` backed by `db`.
    ///
    /// The event broadcast channel is created here with a capacity of 256
    /// events. Plugins that cannot keep up will receive `RecvError::Lagged`
    /// and should log the miss and continue.
    pub fn new(db: Database) -> Self {
        let (events_tx, _) = broadcast::channel(256);
        Self {
            db,
            events_tx,
            sessions: RwLock::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }
}

// ── Host impl ─────────────────────────────────────────────────────────────────

#[async_trait]
impl Host for BbsHost {
    async fn process_command(
        &self,
        session: SessionId,
        cmd: Command,
    ) -> Result<Response, HostError> {
        debug!(%session, ?cmd, "processing command");

        match cmd {
            Command::Help { topic: None } => Ok(Response::Text(
                "Available commands: help, whoami, logout\n\
                 Type 'help <topic>' for more information on a command."
                    .into(),
            )),

            Command::Help { topic: Some(t) } => Ok(Response::Text(format!(
                "No help available for '{t}' yet. Try 'help' for the command list."
            ))),

            Command::Whoami => {
                let sessions = self.sessions.read().await;
                let text = sessions
                    .get(&session)
                    .map(|r| {
                        r.username
                            .as_ref()
                            .map(|u| format!("Logged in as {u} ({}).", r.level))
                            .unwrap_or_else(|| "Not logged in.".into())
                    })
                    .unwrap_or_else(|| "Unknown session.".into());
                Ok(Response::Text(text))
            }

            Command::Logout => {
                self.end_session(session).await?;
                Ok(Response::LoggedOut)
            }

            // Auth flow — placeholder until credential store is wired in.
            Command::Register { .. } | Command::Login { .. } | Command::WorkflowReply { .. } => {
                Ok(Response::Error(
                    "Authentication not yet implemented.".into(),
                ))
            }

            Command::Unknown { raw } => Ok(Response::Text(format!(
                "Unknown command: '{raw}'. Type 'help' for the command list."
            ))),

            // Non-exhaustive catch-all: new Command variants land here.
            _ => Ok(Response::Error(
                "Command not yet supported.".into(),
            )),
        }
    }

    async fn create_session(&self, transport: &'static str) -> Result<SessionId, HostError> {
        let id = SessionId::__internal_new(self.next_id.fetch_add(1, Ordering::Relaxed));

        self.sessions.write().await.insert(
            id,
            SessionRecord {
                transport: transport.to_owned(),
                username: None,
                level: PermissionLevel::Unvalidated,
            },
        );

        let _ = self.events_tx.send(DomainEvent::SessionCreated {
            session: id,
            transport: transport.to_owned(),
        });

        info!(%id, transport, "session created");
        Ok(id)
    }

    async fn end_session(&self, session: SessionId) -> Result<(), HostError> {
        self.sessions.write().await.remove(&session);

        let _ = self.events_tx.send(DomainEvent::SessionEnded {
            session,
            reason: "end_session".into(),
        });

        info!(%session, "session ended");
        Ok(())
    }

    async fn permission_ctx(&self, session: SessionId) -> Result<PermissionCtx, HostError> {
        let sessions = self.sessions.read().await;
        let record = sessions
            .get(&session)
            .ok_or(HostError::UnknownSession(session))?;

        Ok(PermissionCtx::__internal_new(
            session,
            record.username.clone(),
            record.level,
        ))
    }

    fn events(&self) -> broadcast::Receiver<DomainEvent> {
        self.events_tx.subscribe()
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use bbs_plugin_api::Command;
    use tempfile::NamedTempFile;

    async fn make_host() -> Arc<BbsHost> {
        let f = NamedTempFile::new().unwrap();
        let db = Database::open(&f.path().to_string_lossy())
            .await
            .expect("db open");
        Arc::new(BbsHost::new(db))
    }

    #[tokio::test]
    async fn create_and_end_session() {
        let host = make_host().await;
        let sid = host.create_session("test").await.unwrap();
        assert_eq!(host.sessions.read().await.len(), 1);
        host.end_session(sid).await.unwrap();
        assert_eq!(host.sessions.read().await.len(), 0);
    }

    #[tokio::test]
    async fn end_unknown_session_is_ok() {
        let host = make_host().await;
        let fake = SessionId::__internal_new(9999);
        // end_session is documented as idempotent — must not return Err.
        host.end_session(fake).await.unwrap();
    }

    #[tokio::test]
    async fn permission_ctx_unknown_session_errors() {
        let host = make_host().await;
        let fake = SessionId::__internal_new(9999);
        assert!(matches!(
            host.permission_ctx(fake).await,
            Err(HostError::UnknownSession(_))
        ));
    }

    #[tokio::test]
    async fn permission_ctx_pre_auth_is_unvalidated() {
        let host = make_host().await;
        let sid = host.create_session("test").await.unwrap();
        let ctx = host.permission_ctx(sid).await.unwrap();
        assert_eq!(ctx.level, PermissionLevel::Unvalidated);
        assert!(ctx.username.is_none());
    }

    #[tokio::test]
    async fn help_command_returns_text() {
        let host = make_host().await;
        let sid = host.create_session("test").await.unwrap();
        let resp = host
            .process_command(sid, Command::Help { topic: None })
            .await
            .unwrap();
        assert!(matches!(resp, Response::Text(_)));
    }

    #[tokio::test]
    async fn whoami_pre_auth() {
        let host = make_host().await;
        let sid = host.create_session("test").await.unwrap();
        let resp = host.process_command(sid, Command::Whoami).await.unwrap();
        let Response::Text(text) = resp else { panic!("expected Text") };
        assert!(text.contains("Not logged in"));
    }

    #[tokio::test]
    async fn logout_ends_session() {
        let host = make_host().await;
        let sid = host.create_session("test").await.unwrap();
        let resp = host.process_command(sid, Command::Logout).await.unwrap();
        assert_eq!(resp, Response::LoggedOut);
        assert_eq!(host.sessions.read().await.len(), 0);
    }

    #[tokio::test]
    async fn events_broadcasts_session_created() {
        let host = make_host().await;
        let mut rx = host.events();
        let sid = host.create_session("test").await.unwrap();
        let ev = rx.recv().await.unwrap();
        assert!(matches!(ev, DomainEvent::SessionCreated { session, .. } if session == sid));
    }
}
