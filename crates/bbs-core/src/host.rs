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
//! lock is taken only on mutations.
//!
//! ## Auth workflow
//!
//! Registration and login are multi-step workflows tracked per-session in the
//! `Workflow` enum stored alongside each `SessionRecord`. The mesh transport's
//! `awaiting_reply` flag is driven naturally by `Response::Prompt` returns.
//!
//! ## Event bus
//!
//! A `broadcast::Sender<DomainEvent>` is created at construction. Every
//! caller of `Host::events` gets a fresh receiver; missed events (slow
//! consumers) produce `RecvError::Lagged`, which plugins must handle.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use bbs_plugin_api::advert::AdvertBus;
use bbs_plugin_api::host::Host;
use bbs_plugin_api::{
    Command, DomainEvent, HostError, PermissionCtx, PermissionLevel, Response, SessionId,
};
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, info, warn};

use crate::db::{Database, UserStore};
use crate::timestamp::Timestamp;
use crate::user::UserStatus;

// ── Workflow state ────────────────────────────────────────────────────────────

/// Multi-step auth workflow in progress for a session.
#[derive(Clone, Debug, Default)]
enum Workflow {
    #[default]
    None,
    Register {
        username: bbs_plugin_api::Username,
        stage: RegisterStage,
    },
    Login {
        username: bbs_plugin_api::Username,
        attempts: u32,
    },
}

#[derive(Clone, Debug)]
enum RegisterStage {
    /// Awaiting the user's chosen display name.
    DisplayName,
    /// Awaiting the user's chosen password.
    Password { display_name: Option<String> },
    /// Awaiting password confirmation.
    Confirm {
        display_name: Option<String>,
        password: String,
    },
}

// ── Session record ────────────────────────────────────────────────────────────

struct SessionRecord {
    #[allow(dead_code)]
    transport: String,
    username: Option<bbs_plugin_api::Username>,
    level: PermissionLevel,
    workflow: Workflow,
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
    db: Database,
    /// Event fanout channel. Capacity 256: slow consumers get `Lagged`.
    events_tx: broadcast::Sender<DomainEvent>,
    /// In-memory session map.
    sessions: RwLock<HashMap<SessionId, SessionRecord>>,
    /// Monotonically increasing session ID counter.
    next_id: AtomicU64,
    /// Shared mesh advertisement store + send-request bus.
    advert_bus: Arc<AdvertBus>,
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
            advert_bus: Arc::new(AdvertBus::new()),
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
                "Commands: register <username>, login <username>, logout, whoami, help".into(),
            )),

            Command::Help { topic: Some(t) } => {
                Ok(Response::Text(format!("No help available for '{t}' yet.")))
            }

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

            Command::Register { username } => self.handle_register(session, username).await,
            Command::Login { username } => self.handle_login(session, username).await,
            Command::WorkflowReply { reply } => self.handle_workflow_reply(session, reply).await,

            Command::Unknown { raw } => Ok(Response::Text(format!(
                "Unknown command: '{raw}'. Type 'help' for the command list."
            ))),

            _ => Ok(Response::Error("Command not yet supported.".into())),
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
                workflow: Workflow::None,
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

    fn advert_bus(&self) -> Arc<AdvertBus> {
        Arc::clone(&self.advert_bus)
    }
}

// ── Auth helpers ──────────────────────────────────────────────────────────────

impl BbsHost {
    async fn handle_register(
        &self,
        session: SessionId,
        username: bbs_plugin_api::Username,
    ) -> Result<Response, HostError> {
        // Reject if already logged in.
        {
            let sessions = self.sessions.read().await;
            if let Some(r) = sessions.get(&session) {
                if r.username.is_some() {
                    return Ok(Response::Error(
                        "Already logged in. Use 'logout' first.".into(),
                    ));
                }
            }
        }

        // Check availability.
        let existing = self
            .db
            .get_by_username(&username)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        if existing.is_some() {
            return Ok(Response::Error(format!(
                "Username '{username}' is already taken. Choose another."
            )));
        }

        // Start registration workflow.
        {
            let mut sessions = self.sessions.write().await;
            if let Some(r) = sessions.get_mut(&session) {
                r.workflow = Workflow::Register {
                    username,
                    stage: RegisterStage::DisplayName,
                };
            }
        }

        Ok(Response::Prompt {
            text: "Choose a display name (or press Enter to use your username):".into(),
            hide_input: false,
        })
    }

    async fn handle_login(
        &self,
        session: SessionId,
        username: bbs_plugin_api::Username,
    ) -> Result<Response, HostError> {
        // Reject if already logged in.
        {
            let sessions = self.sessions.read().await;
            if let Some(r) = sessions.get(&session) {
                if r.username.is_some() {
                    return Ok(Response::Error(
                        "Already logged in. Use 'logout' first.".into(),
                    ));
                }
            }
        }

        // Check that the account exists and is active.
        let user = self
            .db
            .get_by_username(&username)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        match &user {
            None => {
                return Ok(Response::Error(format!(
                    "No account found for '{username}'."
                )))
            }
            Some(u) if u.status != UserStatus::Active => {
                return Ok(Response::Error("Account is not active.".into()))
            }
            Some(_) => {}
        }

        // Start login workflow.
        {
            let mut sessions = self.sessions.write().await;
            if let Some(r) = sessions.get_mut(&session) {
                r.workflow = Workflow::Login {
                    username,
                    attempts: 0,
                };
            }
        }

        Ok(Response::Prompt {
            text: "Enter your password:".into(),
            hide_input: true,
        })
    }

    async fn handle_workflow_reply(
        &self,
        session: SessionId,
        reply: String,
    ) -> Result<Response, HostError> {
        // Clone the current workflow so we can release the lock before async DB ops.
        let workflow = {
            let sessions = self.sessions.read().await;
            sessions
                .get(&session)
                .map(|r| r.workflow.clone())
                .unwrap_or(Workflow::None)
        };

        match workflow {
            Workflow::None => Ok(Response::Error(
                "No active workflow. Type 'help' for the command list.".into(),
            )),

            Workflow::Register {
                username,
                stage: RegisterStage::DisplayName,
            } => {
                let display_name = if reply.trim().is_empty() {
                    None
                } else {
                    Some(reply.trim().to_owned())
                };
                let mut sessions = self.sessions.write().await;
                if let Some(r) = sessions.get_mut(&session) {
                    r.workflow = Workflow::Register {
                        username,
                        stage: RegisterStage::Password { display_name },
                    };
                }
                Ok(Response::Prompt {
                    text: "Choose a password (min 8 characters):".into(),
                    hide_input: true,
                })
            }

            Workflow::Register {
                username,
                stage: RegisterStage::Password { display_name },
            } => {
                if reply.len() < 8 {
                    // Keep stage at Password; re-prompt.
                    let mut sessions = self.sessions.write().await;
                    if let Some(r) = sessions.get_mut(&session) {
                        r.workflow = Workflow::Register {
                            username,
                            stage: RegisterStage::Password { display_name },
                        };
                    }
                    return Ok(Response::Prompt {
                        text: "Password must be at least 8 characters. Try again:".into(),
                        hide_input: true,
                    });
                }
                let mut sessions = self.sessions.write().await;
                if let Some(r) = sessions.get_mut(&session) {
                    r.workflow = Workflow::Register {
                        username,
                        stage: RegisterStage::Confirm {
                            display_name,
                            password: reply,
                        },
                    };
                }
                Ok(Response::Prompt {
                    text: "Confirm your password:".into(),
                    hide_input: true,
                })
            }

            Workflow::Register {
                username,
                stage:
                    RegisterStage::Confirm {
                        display_name,
                        password,
                    },
            } => {
                if reply != password {
                    // Passwords don't match — restart from password entry.
                    let mut sessions = self.sessions.write().await;
                    if let Some(r) = sessions.get_mut(&session) {
                        r.workflow = Workflow::Register {
                            username,
                            stage: RegisterStage::Password { display_name },
                        };
                    }
                    return Ok(Response::Prompt {
                        text: "Passwords don't match. Choose a password:".into(),
                        hide_input: true,
                    });
                }

                let now = Timestamp::now();
                let user_id = self
                    .db
                    .create(
                        &username,
                        display_name.as_deref(),
                        PermissionLevel::User,
                        now,
                    )
                    .await
                    .map_err(|e| HostError::Storage(format!("create user: {e}")))?;

                self.db
                    .credentials()
                    .set_password(user_id, &password, now)
                    .await
                    .map_err(|e| HostError::Storage(format!("set password: {e}")))?;

                {
                    let mut sessions = self.sessions.write().await;
                    if let Some(r) = sessions.get_mut(&session) {
                        r.username = Some(username.clone());
                        r.level = PermissionLevel::User;
                        r.workflow = Workflow::None;
                    }
                }

                info!(%session, %username, "registration complete");
                Ok(Response::LoggedIn { user: username })
            }

            Workflow::Login { username, attempts } => {
                let user = self
                    .db
                    .get_by_username(&username)
                    .await
                    .map_err(|e| HostError::Storage(format!("{e}")))?;

                let user = match user {
                    None => {
                        // Account deleted between Login command and password entry.
                        let mut sessions = self.sessions.write().await;
                        if let Some(r) = sessions.get_mut(&session) {
                            r.workflow = Workflow::None;
                        }
                        return Ok(Response::Error("Account no longer exists.".into()));
                    }
                    Some(u) => u,
                };

                let ok = self
                    .db
                    .credentials()
                    .verify_password(user.id, &reply, Timestamp::now())
                    .await
                    .map_err(|e| HostError::Storage(format!("verify password: {e}")))?;

                if ok {
                    self.db
                        .update(user.id, None, None, None, Some(Timestamp::now()))
                        .await
                        .map_err(|e| HostError::Storage(format!("update last_login: {e}")))?;

                    {
                        let mut sessions = self.sessions.write().await;
                        if let Some(r) = sessions.get_mut(&session) {
                            r.username = Some(username.clone());
                            r.level = user.permission_level;
                            r.workflow = Workflow::None;
                        }
                    }

                    info!(%session, %username, "login successful");
                    Ok(Response::LoggedIn { user: username })
                } else {
                    let new_attempts = attempts + 1;
                    if new_attempts >= 3 {
                        warn!(%session, %username, "login failed: too many attempts");
                        let mut sessions = self.sessions.write().await;
                        if let Some(r) = sessions.get_mut(&session) {
                            r.workflow = Workflow::None;
                        }
                        Ok(Response::Error(
                            "Too many failed attempts. Type 'login <username>' to try again."
                                .into(),
                        ))
                    } else {
                        let remaining = 3 - new_attempts;
                        let mut sessions = self.sessions.write().await;
                        if let Some(r) = sessions.get_mut(&session) {
                            r.workflow = Workflow::Login {
                                username,
                                attempts: new_attempts,
                            };
                        }
                        Ok(Response::Prompt {
                            text: format!(
                                "Incorrect password ({remaining} attempt(s) remaining). Try again:"
                            ),
                            hide_input: true,
                        })
                    }
                }
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use bbs_plugin_api::{Command, Username};
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
        let Response::Text(text) = resp else {
            panic!("expected Text")
        };
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

    #[tokio::test]
    async fn register_and_login_full_flow() {
        let host = make_host().await;
        let sid = host.create_session("test").await.unwrap();
        let uname = Username::new("alice").unwrap();

        // Step 1: register
        let r = host
            .process_command(
                sid,
                Command::Register {
                    username: uname.clone(),
                },
            )
            .await
            .unwrap();
        assert!(
            matches!(r, Response::Prompt { .. }),
            "expected display-name prompt"
        );

        // Step 2: display name
        let r = host
            .process_command(
                sid,
                Command::WorkflowReply {
                    reply: "Alice".into(),
                },
            )
            .await
            .unwrap();
        assert!(
            matches!(
                r,
                Response::Prompt {
                    hide_input: true,
                    ..
                }
            ),
            "expected password prompt"
        );

        // Step 3: password
        let r = host
            .process_command(
                sid,
                Command::WorkflowReply {
                    reply: "s3cr3t!!".into(),
                },
            )
            .await
            .unwrap();
        assert!(
            matches!(
                r,
                Response::Prompt {
                    hide_input: true,
                    ..
                }
            ),
            "expected confirm prompt"
        );

        // Step 4: confirm
        let r = host
            .process_command(
                sid,
                Command::WorkflowReply {
                    reply: "s3cr3t!!".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            r,
            Response::LoggedIn {
                user: uname.clone()
            }
        );

        // Should be logged in now
        let ctx = host.permission_ctx(sid).await.unwrap();
        assert_eq!(ctx.username.as_ref(), Some(&uname));
        assert_eq!(ctx.level, PermissionLevel::User);

        // Logout and log back in
        host.process_command(sid, Command::Logout).await.unwrap();

        let sid2 = host.create_session("test").await.unwrap();
        let r = host
            .process_command(
                sid2,
                Command::Login {
                    username: uname.clone(),
                },
            )
            .await
            .unwrap();
        assert!(matches!(
            r,
            Response::Prompt {
                hide_input: true,
                ..
            }
        ));

        let r = host
            .process_command(
                sid2,
                Command::WorkflowReply {
                    reply: "s3cr3t!!".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(
            r,
            Response::LoggedIn {
                user: uname.clone()
            }
        );
    }

    #[tokio::test]
    async fn login_wrong_password_lockout() {
        let host = make_host().await;
        let sid = host.create_session("test").await.unwrap();
        let uname = Username::new("bob").unwrap();

        // Register bob
        host.process_command(
            sid,
            Command::Register {
                username: uname.clone(),
            },
        )
        .await
        .unwrap();
        host.process_command(
            sid,
            Command::WorkflowReply {
                reply: String::new(),
            },
        )
        .await
        .unwrap();
        host.process_command(
            sid,
            Command::WorkflowReply {
                reply: "password1".into(),
            },
        )
        .await
        .unwrap();
        host.process_command(
            sid,
            Command::WorkflowReply {
                reply: "password1".into(),
            },
        )
        .await
        .unwrap();
        host.process_command(sid, Command::Logout).await.unwrap();

        // Login with wrong password 3 times
        let sid2 = host.create_session("test").await.unwrap();
        host.process_command(
            sid2,
            Command::Login {
                username: uname.clone(),
            },
        )
        .await
        .unwrap();

        for _ in 0..2 {
            let r = host
                .process_command(
                    sid2,
                    Command::WorkflowReply {
                        reply: "wrong".into(),
                    },
                )
                .await
                .unwrap();
            assert!(matches!(r, Response::Prompt { .. }), "should re-prompt");
        }

        let r = host
            .process_command(
                sid2,
                Command::WorkflowReply {
                    reply: "wrong".into(),
                },
            )
            .await
            .unwrap();
        assert!(
            matches!(r, Response::Error(_)),
            "should error after 3 failures"
        );
    }
}
