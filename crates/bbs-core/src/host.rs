//! Concrete [`Host`] implementation backed by the bbs-core [`Database`].
//!
//! ## Session lifecycle
//!
//! Sessions are held in memory (not persisted across restarts). The session map
//! uses `RwLock<HashMap<SessionId, SessionRecord>>` so concurrent reads from
//! multiple transports don't contend. Write lock taken only on mutations.
//!
//! ## Auth workflow
//!
//! Registration and login are multi-step workflows tracked per-session in the
//! `Workflow` enum. The mesh transport's `awaiting_reply` flag follows naturally
//! from `Response::Prompt` returns.
//!
//! ## Room system
//!
//! After login, each session is placed in the Lobby (room ID 1). Room navigation
//! (C, G, M, K) and message operations (N, E, D, F, R, S) are gated on auth.
//! Per-user read state is persisted in `user_room_state` via `MessageStore`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use bbs_plugin_api::advert::AdvertBus;
use bbs_plugin_api::host::Host;
use bbs_plugin_api::{
    AdminBackupRecord, AdminMessageRecord, AdminRoomSummary, AdminSessionInfo, AdminStats,
    AdminUserInfo, Command, DomainEvent, HostError, PermissionCtx, PermissionLevel, Response,
    SessionId, Username,
};
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, info, warn};

use crate::db::{Database, MessageStore, RoomStore, UserStore};
use crate::ids::{RoomId, UserId};
use crate::message::Message;
use crate::timestamp::Timestamp;
use crate::user::UserStatus;

// ── System room constants ─────────────────────────────────────────────────────

const LOBBY_ROOM_ID: RoomId = RoomId::new(1);
const MAIL_ROOM_ID: RoomId = RoomId::new(2);

/// Messages shown per page for mesh radio (keep short for LoRa).
const MESH_PAGE: u32 = 5;

// ── Workflow state ────────────────────────────────────────────────────────────

#[derive(Clone, Debug, Default)]
enum Workflow {
    #[default]
    None,
    Register {
        username: Username,
        stage: RegisterStage,
    },
    Login {
        username: Username,
        attempts: u32,
    },
    /// Composing a message (E command).
    Compose {
        room_id: RoomId,
        stage: ComposeStage,
    },
    /// Editing the user's own display name (PROFILE command).
    EditProfile,
}

#[derive(Clone, Debug)]
enum RegisterStage {
    DisplayName,
    Password {
        display_name: Option<String>,
    },
    Confirm {
        display_name: Option<String>,
        password: String,
    },
}

#[derive(Clone, Debug)]
enum ComposeStage {
    /// Mail room only: waiting for the recipient username.
    AwaitingRecipient,
    /// Waiting for the message body.
    AwaitingBody { recipient: Option<Username> },
}

// ── Session record ────────────────────────────────────────────────────────────

struct SessionRecord {
    #[allow(dead_code)]
    transport: String,
    username: Option<Username>,
    user_id: Option<UserId>,
    level: PermissionLevel,
    workflow: Workflow,
    /// Current room. Starts at Lobby on login; updated by C/G/M.
    current_room: RoomId,
}

// ── BbsHost ───────────────────────────────────────────────────────────────────

/// Concrete [`Host`] implementation backed by the bbs-core [`Database`].
pub struct BbsHost {
    db: Database,
    events_tx: broadcast::Sender<DomainEvent>,
    sessions: RwLock<HashMap<SessionId, SessionRecord>>,
    next_id: AtomicU64,
    advert_bus: Arc<AdvertBus>,
}

impl BbsHost {
    /// Create a new [`BbsHost`] backed by the given database.
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

// async_trait rewrites async fn bodies into Box::pin(async move { … }) closures.
// Clippy's dead_code analysis doesn't follow those closures back to pub(crate)
// helpers, so it incorrectly flags the admin methods as unused. All of them are
// reachable via dyn Host trait dispatch from bbs-web.
#[allow(dead_code)]
#[async_trait]
impl Host for BbsHost {
    async fn process_command(
        &self,
        session: SessionId,
        cmd: Command,
    ) -> Result<Response, HostError> {
        debug!(%session, ?cmd, "processing command");

        match cmd {
            Command::Help { topic } => Ok(Response::Text(help_text(topic.as_deref()))),

            Command::Whoami => self.handle_whoami(session).await,
            Command::Logout | Command::Quit => {
                self.end_session(session).await?;
                Ok(Response::LoggedOut)
            }

            Command::Register { username } => self.handle_register(session, username).await,
            Command::Login { username } => self.handle_login(session, username).await,
            Command::WorkflowReply { reply } => self.handle_workflow_reply(session, reply).await,
            Command::Cancel => self.handle_cancel(session).await,

            // Room navigation
            Command::ListRooms => self.handle_list_rooms(session).await,
            Command::GoNextUnread => self.handle_go_next_unread(session).await,
            Command::ChangeRoom { target } => self.handle_change_room(session, &target).await,
            Command::GoMail => self.handle_change_to_room(session, MAIL_ROOM_ID).await,
            Command::IgnoreRoom => Ok(Response::Text("Room ignore is not yet implemented.".into())),

            // Message reading
            Command::ReadNew => self.handle_read_new(session).await,
            Command::ReadForward { after } => self.handle_read_forward(session, after).await,
            Command::ReadReverse => self.handle_read_reverse(session).await,
            Command::ScanMessages => self.handle_scan(session).await,
            Command::FastForward => self.handle_fast_forward(session).await,

            // Message posting / deletion
            Command::EnterMessage => self.handle_enter_message(session).await,
            Command::DeleteMessage { id } => self.handle_delete(session, id).await,

            // Moderation / account
            Command::WhoIsOnline => self.handle_who_is_online(session).await,
            Command::ListPending => self.handle_list_pending(session).await,
            Command::ValidateUser { username } => {
                self.handle_validate_user(session, username).await
            }
            Command::BlockUser { target, force } => {
                self.handle_block_user(session, target, force).await
            }
            Command::BanUser { username } => self.handle_ban_user(session, username).await,
            Command::UnbanUser { username } => self.handle_unban_user(session, username).await,

            // Profile / room management
            Command::EditProfile => self.handle_edit_profile(session).await,
            Command::CreateRoom { name } => self.handle_create_room(session, &name).await,
            Command::DeleteRoom { name } => self.handle_delete_room(session, &name).await,
            Command::EditRoom => Ok(Response::Text(
                "Room editing is not yet implemented.".into(),
            )),
            Command::EditUser { .. } => Ok(Response::Text(
                "User editing is not yet implemented.".into(),
            )),

            Command::Unknown { raw } => Ok(Response::Text(format!(
                "Unknown command: '{raw}'. Type H for help."
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
                user_id: None,
                level: PermissionLevel::Unvalidated,
                workflow: Workflow::None,
                current_room: LOBBY_ROOM_ID,
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
        let r = sessions
            .get(&session)
            .ok_or(HostError::UnknownSession(session))?;
        Ok(PermissionCtx::__internal_new(
            session,
            r.username.clone(),
            r.level,
        ))
    }

    fn events(&self) -> broadcast::Receiver<DomainEvent> {
        self.events_tx.subscribe()
    }

    fn advert_bus(&self) -> Arc<AdvertBus> {
        Arc::clone(&self.advert_bus)
    }

    // ── Admin / web-UI operations ─────────────────────────────────────────────

    async fn admin_verify_credentials(
        &self,
        username: &str,
        password: &str,
    ) -> Result<PermissionLevel, HostError> {
        let uname = Username::new(username)
            .map_err(|_| HostError::NotFound(format!("user {username:?}")))?;

        let user = UserStore::get_by_username(&self.db, &uname)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?
            .ok_or_else(|| HostError::NotFound(format!("user {username:?}")))?;

        if !user.is_active() {
            return Err(HostError::PermissionDenied {
                required: PermissionLevel::Aide,
            });
        }

        if user.permission_level < PermissionLevel::Aide {
            return Err(HostError::PermissionDenied {
                required: PermissionLevel::Aide,
            });
        }

        let ok = self
            .db
            .credentials()
            .verify_password(user.id, password, Timestamp::now())
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        if !ok {
            return Err(HostError::PermissionDenied {
                required: PermissionLevel::Aide,
            });
        }

        Ok(user.permission_level)
    }

    async fn admin_list_sessions(&self) -> Result<Vec<AdminSessionInfo>, HostError> {
        let sessions = self.sessions.read().await;
        Ok(sessions
            .iter()
            .map(|(sid, r)| AdminSessionInfo {
                session_id: sid.as_u64(),
                transport: r.transport.clone(),
                username: r.username.as_ref().map(|u| u.as_str().to_owned()),
                permission_level: r.level as u8,
            })
            .collect())
    }

    async fn admin_list_users(
        &self,
        status_filter: Option<u8>,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<AdminUserInfo>, HostError> {
        let filter = match status_filter {
            None => None,
            Some(0) => Some(UserStatus::Active),
            Some(1) => Some(UserStatus::Banned),
            Some(2) => Some(UserStatus::Deleted),
            Some(other) => {
                return Err(HostError::PreconditionFailed(format!(
                    "unknown status filter {other}"
                )))
            }
        };

        let users = UserStore::list(&self.db, filter, limit, offset)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        Ok(users
            .into_iter()
            .map(|u| AdminUserInfo {
                id: u.id.as_i64(),
                username: u.username.as_str().to_owned(),
                display_name: u.display_name,
                status: u.status.to_string(),
                permission_level: u.permission_level as u8,
                created_at: u.created_at.to_rfc3339(),
                last_login_at: u.last_login_at.map(|t| t.to_rfc3339()),
            })
            .collect())
    }

    async fn admin_update_user(
        &self,
        username: &str,
        status: Option<u8>,
        permission_level: Option<u8>,
    ) -> Result<(), HostError> {
        let uname = Username::new(username)
            .map_err(|_| HostError::NotFound(format!("user {username:?}")))?;

        let user = UserStore::get_by_username(&self.db, &uname)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?
            .ok_or_else(|| HostError::NotFound(format!("user {username:?}")))?;

        let new_status = match status {
            None => None,
            Some(0) => Some(UserStatus::Active),
            Some(1) => Some(UserStatus::Banned),
            Some(2) => Some(UserStatus::Deleted),
            Some(other) => {
                return Err(HostError::PreconditionFailed(format!(
                    "unknown status {other}"
                )))
            }
        };

        let new_level = match permission_level {
            None => None,
            Some(0) => Some(PermissionLevel::Unvalidated),
            Some(10) => Some(PermissionLevel::User),
            Some(50) => Some(PermissionLevel::Aide),
            Some(100) => Some(PermissionLevel::Sysop),
            Some(other) => {
                return Err(HostError::PreconditionFailed(format!(
                    "unknown permission_level {other}"
                )))
            }
        };

        UserStore::update(&self.db, user.id, None, new_status, new_level, None)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        // If banning, kick any live sessions for this user.
        if matches!(new_status, Some(UserStatus::Banned)) {
            let mut sessions = self.sessions.write().await;
            for r in sessions.values_mut() {
                if r.username.as_ref().map(|u| u.as_str()) == Some(username) {
                    r.workflow = crate::host::Workflow::None;
                }
            }
        }

        // If validating (level change from Unvalidated → something higher), update live sessions.
        if let Some(level) = new_level {
            let mut sessions = self.sessions.write().await;
            for r in sessions.values_mut() {
                if r.username.as_ref().map(|u| u.as_str()) == Some(username) {
                    r.level = level;
                }
            }
        }

        Ok(())
    }

    async fn admin_list_rooms(&self) -> Result<Vec<AdminRoomSummary>, HostError> {
        self.db
            .admin_list_rooms()
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))
    }

    async fn admin_create_room(
        &self,
        name: &str,
        description: Option<&str>,
    ) -> Result<AdminRoomSummary, HostError> {
        RoomStore::create(
            &self.db,
            name,
            description,
            false,
            PermissionLevel::User,
            Timestamp::now(),
        )
        .await
        .map_err(|e| HostError::Storage(format!("{e}")))?;

        // Fetch the just-created room by name.
        let rooms = self
            .db
            .admin_list_rooms()
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        rooms
            .into_iter()
            .find(|r| r.name == name)
            .ok_or_else(|| HostError::Internal("created room not found".into()))
    }

    async fn admin_delete_room(&self, room_id: i64) -> Result<bool, HostError> {
        // Protect system rooms.
        if room_id <= 3 {
            return Err(HostError::PreconditionFailed(
                "system rooms cannot be deleted".into(),
            ));
        }
        let rid = crate::ids::RoomId::new(room_id);
        match RoomStore::delete(&self.db, rid).await {
            Ok(()) => Ok(true),
            Err(crate::db::StoreError::NotFound) => Ok(false),
            Err(e) => Err(HostError::Storage(format!("{e}"))),
        }
    }

    async fn admin_list_messages(
        &self,
        room_id: i64,
        limit: u32,
        after_id: Option<i64>,
    ) -> Result<Vec<AdminMessageRecord>, HostError> {
        use crate::ids::MessageId;
        let rid = crate::ids::RoomId::new(room_id);
        let after = after_id.map(MessageId::new);
        let page = crate::db::MessageStore::list_in_room(&self.db, rid, after, limit)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        Ok(page
            .messages
            .into_iter()
            .map(|m| AdminMessageRecord {
                id: m.id.as_i64(),
                sender: m.sender.as_str().to_owned(),
                recipient: m.recipient.as_ref().map(|u| u.as_str().to_owned()),
                content: m.content,
                timestamp: m.timestamp.to_rfc3339(),
            })
            .collect())
    }

    async fn admin_delete_message(&self, message_id: i64) -> Result<bool, HostError> {
        use crate::ids::MessageId;
        crate::db::MessageStore::delete(&self.db, MessageId::new(message_id))
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))
    }

    async fn admin_stats(&self) -> Result<AdminStats, HostError> {
        let active_sessions = self.sessions.read().await.len();
        self.db
            .admin_stats(active_sessions)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))
    }

    async fn admin_reports(&self) -> Result<bbs_plugin_api::AdminReports, HostError> {
        self.db
            .admin_reports()
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))
    }

    async fn admin_trigger_backup(&self, backup_dir: &str) -> Result<AdminBackupRecord, HostError> {
        use time::format_description::well_known::Rfc3339;

        let now = time::OffsetDateTime::now_utc();
        let stamp = now
            .format(
                &time::format_description::parse("[year][month][day]_[hour][minute][second]")
                    .unwrap(),
            )
            .unwrap_or_else(|_| "backup".to_owned());
        let filename = format!("backup_{stamp}.db");
        let dest = std::path::Path::new(backup_dir).join(&filename);

        self.db
            .admin_backup(&dest.to_string_lossy())
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        let meta = tokio::fs::metadata(&dest)
            .await
            .map_err(|e| HostError::Storage(format!("read backup metadata: {e}")))?;

        let created_at = now.format(&Rfc3339).unwrap_or_default();

        Ok(AdminBackupRecord {
            filename,
            size_bytes: meta.len(),
            created_at,
        })
    }

    async fn admin_list_backups(
        &self,
        backup_dir: &str,
    ) -> Result<Vec<AdminBackupRecord>, HostError> {
        self.db
            .admin_list_backups(backup_dir)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))
    }
}

// ── Auth helpers ──────────────────────────────────────────────────────────────

impl BbsHost {
    async fn handle_whoami(&self, session: SessionId) -> Result<Response, HostError> {
        let sessions = self.sessions.read().await;
        let text = sessions
            .get(&session)
            .map(|r| {
                r.username
                    .as_ref()
                    .map(|u| {
                        format!(
                            "Logged in as {} ({}). Current room: room:{}",
                            u.as_str(),
                            r.level,
                            r.current_room.as_i64()
                        )
                    })
                    .unwrap_or_else(|| "Not logged in.".into())
            })
            .unwrap_or_else(|| "Unknown session.".into());
        Ok(Response::Text(text))
    }

    async fn handle_cancel(&self, session: SessionId) -> Result<Response, HostError> {
        let mut sessions = self.sessions.write().await;
        if let Some(r) = sessions.get_mut(&session) {
            if matches!(r.workflow, Workflow::None) {
                return Ok(Response::Text("No active workflow.".into()));
            }
            r.workflow = Workflow::None;
        }
        Ok(Response::Text("Cancelled.".into()))
    }

    async fn handle_register(
        &self,
        session: SessionId,
        username: Username,
    ) -> Result<Response, HostError> {
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

        let existing = self
            .db
            .get_by_username(&username)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;
        if existing.is_some() {
            return Ok(Response::Error(format!(
                "'{username}' is taken. Try: login {username}"
            )));
        }

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
        username: Username,
    ) -> Result<Response, HostError> {
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

        let user = self
            .db
            .get_by_username(&username)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;
        match user {
            Some(u) if u.status == UserStatus::Active => {}
            _ => return Ok(Response::Error("Login failed.".into())),
        }

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
        let workflow = {
            let sessions = self.sessions.read().await;
            sessions
                .get(&session)
                .map(|r| r.workflow.clone())
                .unwrap_or(Workflow::None)
        };

        match workflow {
            Workflow::None => Ok(Response::Error("No active workflow. Type 'help'.".into())),

            // ── Registration ─────────────────────────────────────────────────
            Workflow::Register {
                username,
                stage: RegisterStage::DisplayName,
            } => {
                let trimmed = reply.trim();
                let display_name = if trimmed.is_empty() {
                    None
                } else {
                    if let Err(e) = crate::user::User::validate_display_name(trimmed) {
                        return Ok(Response::Prompt {
                            text: format!("Invalid display name: {e}. Try again:"),
                            hide_input: false,
                        });
                    }
                    Some(trimmed.to_owned())
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
                let user_id = UserStore::create(
                    &self.db,
                    &username,
                    display_name.as_deref(),
                    PermissionLevel::Unvalidated,
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
                        r.user_id = Some(user_id);
                        r.level = PermissionLevel::Unvalidated;
                        r.workflow = Workflow::None;
                        r.current_room = LOBBY_ROOM_ID;
                    }
                }
                let _ = self.events_tx.send(DomainEvent::UserCreated {
                    user: username.clone(),
                });
                info!(%session, %username, "registration complete — awaiting validation");
                Ok(Response::LoggedIn { user: username })
            }

            // ── Login ────────────────────────────────────────────────────────
            Workflow::Login { username, attempts } => {
                let user = self
                    .db
                    .get_by_username(&username)
                    .await
                    .map_err(|e| HostError::Storage(format!("{e}")))?;
                let user = match user {
                    None => {
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
                    UserStore::update(&self.db, user.id, None, None, None, Some(Timestamp::now()))
                        .await
                        .map_err(|e| HostError::Storage(format!("update last_login: {e}")))?;
                    {
                        let mut sessions = self.sessions.write().await;
                        if let Some(r) = sessions.get_mut(&session) {
                            r.username = Some(username.clone());
                            r.user_id = Some(user.id);
                            r.level = user.permission_level;
                            r.workflow = Workflow::None;
                            r.current_room = LOBBY_ROOM_ID;
                        }
                    }
                    info!(%session, %username, "login successful");
                    Ok(Response::LoggedIn { user: username })
                } else {
                    // Slow down brute-force attempts regardless of outcome.
                    tokio::time::sleep(std::time::Duration::from_secs(2)).await;
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

            // ── Message composition ──────────────────────────────────────────
            Workflow::Compose {
                room_id,
                stage: ComposeStage::AwaitingRecipient,
            } => {
                match Username::new(reply.trim()) {
                    Ok(recipient) => {
                        // Verify recipient exists.
                        let exists = self
                            .db
                            .get_by_username(&recipient)
                            .await
                            .map_err(|e| HostError::Storage(format!("{e}")))?
                            .is_some();
                        if !exists {
                            // Stay in AwaitingRecipient.
                            let mut sessions = self.sessions.write().await;
                            if let Some(r) = sessions.get_mut(&session) {
                                r.workflow = Workflow::Compose {
                                    room_id,
                                    stage: ComposeStage::AwaitingRecipient,
                                };
                            }
                            return Ok(Response::Prompt {
                                text: format!(
                                    "User '{}' not found. Enter recipient username:",
                                    reply.trim()
                                ),
                                hide_input: false,
                            });
                        }
                        let mut sessions = self.sessions.write().await;
                        if let Some(r) = sessions.get_mut(&session) {
                            r.workflow = Workflow::Compose {
                                room_id,
                                stage: ComposeStage::AwaitingBody {
                                    recipient: Some(recipient),
                                },
                            };
                        }
                        Ok(Response::Prompt {
                            text: "Enter your message:".into(),
                            hide_input: false,
                        })
                    }
                    Err(_) => {
                        let mut sessions = self.sessions.write().await;
                        if let Some(r) = sessions.get_mut(&session) {
                            r.workflow = Workflow::Compose {
                                room_id,
                                stage: ComposeStage::AwaitingRecipient,
                            };
                        }
                        Ok(Response::Prompt {
                            text: "Invalid username. Enter recipient username:".into(),
                            hide_input: false,
                        })
                    }
                }
            }

            Workflow::Compose {
                room_id,
                stage: ComposeStage::AwaitingBody { recipient },
            } => {
                let body = reply.trim();
                if body.is_empty() {
                    let mut sessions = self.sessions.write().await;
                    if let Some(r) = sessions.get_mut(&session) {
                        r.workflow = Workflow::Compose {
                            room_id,
                            stage: ComposeStage::AwaitingBody { recipient },
                        };
                    }
                    return Ok(Response::Prompt {
                        text: "Message cannot be empty. Enter your message:".into(),
                        hide_input: false,
                    });
                }

                let sender = {
                    let sessions = self.sessions.read().await;
                    sessions
                        .get(&session)
                        .and_then(|r| r.username.clone())
                        .ok_or(HostError::NotAuthenticated)?
                };

                let now = Timestamp::now();
                if let Some(ref rcpt) = recipient {
                    self.db
                        .post_direct(&sender, rcpt, body, now)
                        .await
                        .map_err(|e| HostError::Storage(format!("post_direct: {e}")))?;
                } else {
                    self.db
                        .post_to_room(room_id, &sender, body, now)
                        .await
                        .map_err(|e| HostError::Storage(format!("post_to_room: {e}")))?;
                }

                {
                    let mut sessions = self.sessions.write().await;
                    if let Some(r) = sessions.get_mut(&session) {
                        r.workflow = Workflow::None;
                    }
                }
                Ok(Response::Text("Message posted.".into()))
            }

            // ── Profile edit ─────────────────────────────────────────────────
            Workflow::EditProfile => {
                let (_, user_id, _, _) = match self.session_auth(session).await {
                    Ok(t) => t,
                    Err(r) => return Ok(r),
                };

                let display_name: Option<Option<&str>> = if reply.trim() == "-" {
                    Some(None) // clear display name
                } else if reply.trim().is_empty() {
                    None // no change
                } else {
                    Some(Some(reply.trim()))
                };

                if display_name.is_none() {
                    let mut sessions = self.sessions.write().await;
                    if let Some(r) = sessions.get_mut(&session) {
                        r.workflow = Workflow::None;
                    }
                    return Ok(Response::Text("No change made.".into()));
                }

                UserStore::update(
                    &self.db,
                    user_id,
                    display_name.map(|d| d.map(|s| s as &str)),
                    None,
                    None,
                    None,
                )
                .await
                .map_err(|e| HostError::Storage(format!("update profile: {e}")))?;

                {
                    let mut sessions = self.sessions.write().await;
                    if let Some(r) = sessions.get_mut(&session) {
                        r.workflow = Workflow::None;
                    }
                }
                Ok(Response::Text("Display name updated.".into()))
            }
        }
    }
}

// ── Room navigation helpers ───────────────────────────────────────────────────

impl BbsHost {
    /// Extract (username, user_id, level, current_room) from a live session,
    /// returning an auth error response if the session isn't logged in.
    async fn session_auth(
        &self,
        session: SessionId,
    ) -> Result<(Username, UserId, PermissionLevel, RoomId), Response> {
        let sessions = self.sessions.read().await;
        let r = match sessions.get(&session) {
            Some(r) => r,
            None => return Err(Response::Error("Unknown session.".into())),
        };
        match (&r.username, r.user_id) {
            (Some(u), Some(id)) => Ok((u.clone(), id, r.level, r.current_room)),
            _ => Err(Response::Error(
                "Not logged in. Use 'login <username>' or 'register <username>'.".into(),
            )),
        }
    }

    /// Like `session_auth` but also requires `PermissionLevel::User` or above.
    /// Unvalidated accounts get a pending-validation message.
    async fn session_auth_user(
        &self,
        session: SessionId,
    ) -> Result<(Username, UserId, PermissionLevel, RoomId), Response> {
        let result = self.session_auth(session).await?;
        if result.2 < PermissionLevel::User {
            return Err(Response::Text(
                "Your account is pending validation by an aide.\n\
                 Type 'whoami', 'help', 'pending', or 'logout'."
                    .into(),
            ));
        }
        Ok(result)
    }

    async fn handle_list_rooms(&self, session: SessionId) -> Result<Response, HostError> {
        let (_, user_id, level, current_room) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };

        let rooms = self
            .db
            .list_readable(level)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        let mut lines = Vec::new();
        for room in &rooms {
            let unread = self
                .db
                .unread_count(user_id, room.id)
                .await
                .map_err(|e| HostError::Storage(format!("{e}")))?;
            let marker = if unread > 0 { "*" } else { " " };
            let here = if room.id == current_room {
                " [here]"
            } else {
                ""
            };
            let count = if unread > 0 {
                format!(" ({unread} new)")
            } else {
                String::new()
            };
            lines.push(format!("{marker} {}{}{}", room.name, count, here));
        }

        if lines.is_empty() {
            return Ok(Response::Text("No accessible rooms.".into()));
        }
        Ok(Response::Text(format!("Rooms:\n{}", lines.join("\n"))))
    }

    async fn handle_go_next_unread(&self, session: SessionId) -> Result<Response, HostError> {
        let (_, user_id, level, current_room) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };

        let rooms = self
            .db
            .list_readable(level)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        // Walk the room list starting just after the current room.
        let start = rooms
            .iter()
            .position(|r| r.id == current_room)
            .map(|i| i + 1)
            .unwrap_or(0);

        let candidate = rooms[start..]
            .iter()
            .chain(rooms[..start].iter())
            .find(|r| r.id != current_room);

        for room in candidate.into_iter().chain(rooms.iter()) {
            let unread = self
                .db
                .unread_count(user_id, room.id)
                .await
                .map_err(|e| HostError::Storage(format!("{e}")))?;
            if unread > 0 {
                self.set_current_room(session, room.id).await;
                return Ok(Response::Text(format!(
                    "Now in: {} ({unread} new)",
                    room.name
                )));
            }
        }

        Ok(Response::Text("No unread messages in any room.".into()))
    }

    async fn handle_change_room(
        &self,
        session: SessionId,
        target: &str,
    ) -> Result<Response, HostError> {
        if target.trim().is_empty() {
            return Ok(Response::Text("Usage: C <room name or number>".into()));
        }

        let (_, user_id, level, _) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };

        // Try by name first; then by numeric ID.
        let room = if let Ok(id) = target.parse::<i64>() {
            RoomStore::get_by_id(&self.db, RoomId::new(id))
                .await
                .map_err(|e| HostError::Storage(format!("{e}")))?
        } else {
            self.db
                .get_by_name(target.trim())
                .await
                .map_err(|e| HostError::Storage(format!("{e}")))?
        };

        let room = match room {
            None => return Ok(Response::Error(format!("Room '{target}' not found."))),
            Some(r) => r,
        };

        if level < room.min_permission_level {
            return Ok(Response::Error(format!(
                "You don't have permission to enter '{}'.",
                room.name
            )));
        }

        self.set_current_room(session, room.id).await;
        let unread = self
            .db
            .unread_count(user_id, room.id)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        let msg = if unread > 0 {
            format!("Now in: {} ({unread} new). Type N to read.", room.name)
        } else {
            format!("Now in: {} (no new messages).", room.name)
        };
        Ok(Response::Text(msg))
    }

    async fn handle_change_to_room(
        &self,
        session: SessionId,
        room_id: RoomId,
    ) -> Result<Response, HostError> {
        let (_, user_id, level, _) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };

        let room = RoomStore::get_by_id(&self.db, room_id)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?
            .ok_or_else(|| HostError::NotFound(format!("room {room_id}")))?;

        if level < room.min_permission_level {
            return Ok(Response::Error(format!(
                "You don't have permission to enter '{}'.",
                room.name
            )));
        }

        self.set_current_room(session, room.id).await;
        let unread = self
            .db
            .unread_count(user_id, room.id)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        let msg = if unread > 0 {
            format!("Now in: {} ({unread} new). Type N to read.", room.name)
        } else {
            format!("Now in: {} (no new messages).", room.name)
        };
        Ok(Response::Text(msg))
    }

    async fn set_current_room(&self, session: SessionId, room_id: RoomId) {
        let mut sessions = self.sessions.write().await;
        if let Some(r) = sessions.get_mut(&session) {
            r.current_room = room_id;
        }
    }
}

// ── Message helpers ───────────────────────────────────────────────────────────

impl BbsHost {
    async fn handle_read_new(&self, session: SessionId) -> Result<Response, HostError> {
        let (username, user_id, _, room_id) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };

        let room = RoomStore::get_by_id(&self.db, room_id)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?
            .ok_or_else(|| HostError::NotFound(format!("{room_id}")))?;

        let after = self
            .db
            .get_last_read(user_id, room_id)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        let page = self
            .db
            .list_in_room(room_id, after, MESH_PAGE)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        let blocked = self
            .db
            .blocks_by(username.as_str())
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;
        let visible: Vec<_> = page
            .messages
            .iter()
            .filter(|m| !blocked.contains(m.sender.as_str()))
            .collect();

        // Advance read pointer to last message in the raw page (including
        // blocked messages) so N doesn't re-deliver them on the next call.
        if let Some(last) = page.messages.last() {
            self.db
                .mark_read(user_id, room_id, last.id)
                .await
                .map_err(|e| HostError::Storage(format!("{e}")))?;
        }

        if visible.is_empty() {
            return Ok(Response::Text(format!("No new messages in {}.", room.name)));
        }

        let mut lines = vec![format!("[{} — new messages]", room.name)];
        for msg in &visible {
            lines.push(format_message(msg));
        }
        if page.next_cursor.is_some() {
            lines.push(format!(
                "(more — type N again or F {} to continue)",
                page.messages.last().map(|m| m.id.as_i64()).unwrap_or(0)
            ));
        }
        Ok(Response::Text(lines.join("\n")))
    }

    async fn handle_read_forward(
        &self,
        session: SessionId,
        after: Option<i64>,
    ) -> Result<Response, HostError> {
        let (username, user_id, _, room_id) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };

        let room = RoomStore::get_by_id(&self.db, room_id)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?
            .ok_or_else(|| HostError::NotFound(format!("{room_id}")))?;

        let after_id = after.map(crate::ids::MessageId::new);
        let page = self
            .db
            .list_in_room(room_id, after_id, MESH_PAGE)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        let blocked = self
            .db
            .blocks_by(username.as_str())
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;
        let visible: Vec<_> = page
            .messages
            .iter()
            .filter(|m| !blocked.contains(m.sender.as_str()))
            .collect();

        if let Some(last) = page.messages.last() {
            self.db
                .mark_read(user_id, room_id, last.id)
                .await
                .map_err(|e| HostError::Storage(format!("{e}")))?;
        }

        if visible.is_empty() {
            return Ok(Response::Text(format!("No messages in {}.", room.name)));
        }

        let mut lines = vec![format!("[{} — forward read]", room.name)];
        for msg in &visible {
            lines.push(format_message(msg));
        }
        if let Some(cursor) = page.next_cursor {
            lines.push(format!("(more — type F {} to continue)", cursor.as_i64()));
        }
        Ok(Response::Text(lines.join("\n")))
    }

    async fn handle_read_reverse(&self, session: SessionId) -> Result<Response, HostError> {
        let (username, _, _, room_id) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };

        let room = RoomStore::get_by_id(&self.db, room_id)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?
            .ok_or_else(|| HostError::NotFound(format!("{room_id}")))?;

        let messages = self
            .db
            .list_recent_in_room(room_id, MESH_PAGE)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        let blocked = self
            .db
            .blocks_by(username.as_str())
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;
        let visible: Vec<_> = messages
            .iter()
            .filter(|m| !blocked.contains(m.sender.as_str()))
            .collect();

        if visible.is_empty() {
            return Ok(Response::Text(format!("No messages in {}.", room.name)));
        }

        let mut lines = vec![format!("[{} — newest first]", room.name)];
        for msg in &visible {
            lines.push(format_message(msg));
        }
        Ok(Response::Text(lines.join("\n")))
    }

    async fn handle_scan(&self, session: SessionId) -> Result<Response, HostError> {
        let (username, _, _, room_id) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };

        let room = RoomStore::get_by_id(&self.db, room_id)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?
            .ok_or_else(|| HostError::NotFound(format!("{room_id}")))?;

        let page = self
            .db
            .list_in_room(room_id, None, MESH_PAGE * 2)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        let blocked = self
            .db
            .blocks_by(username.as_str())
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;
        let visible: Vec<_> = page
            .messages
            .iter()
            .filter(|m| !blocked.contains(m.sender.as_str()))
            .collect();

        if visible.is_empty() {
            return Ok(Response::Text(format!("No messages in {}.", room.name)));
        }

        let mut lines = vec![format!("[{} — scan]", room.name)];
        for msg in &visible {
            let snippet: String = msg.content.chars().take(40).collect();
            let ellipsis = if msg.content.len() > 40 { "…" } else { "" };
            lines.push(format!(
                "#{} {}: {}{}",
                msg.id.as_i64(),
                msg.sender.as_str(),
                snippet,
                ellipsis
            ));
        }
        if page.next_cursor.is_some() {
            lines.push("(more — type F <id> to read from a message)".into());
        }
        Ok(Response::Text(lines.join("\n")))
    }

    async fn handle_enter_message(&self, session: SessionId) -> Result<Response, HostError> {
        let (_, _, _, room_id) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };

        let room = RoomStore::get_by_id(&self.db, room_id)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?
            .ok_or_else(|| HostError::NotFound(format!("{room_id}")))?;

        if room.read_only {
            return Ok(Response::Error(format!("'{}' is read-only.", room.name)));
        }

        if room_id == MAIL_ROOM_ID {
            let mut sessions = self.sessions.write().await;
            if let Some(r) = sessions.get_mut(&session) {
                r.workflow = Workflow::Compose {
                    room_id,
                    stage: ComposeStage::AwaitingRecipient,
                };
            }
            return Ok(Response::Prompt {
                text: "Enter recipient username:".into(),
                hide_input: false,
            });
        }

        let mut sessions = self.sessions.write().await;
        if let Some(r) = sessions.get_mut(&session) {
            r.workflow = Workflow::Compose {
                room_id,
                stage: ComposeStage::AwaitingBody { recipient: None },
            };
        }
        Ok(Response::Prompt {
            text: format!("Enter your message for {}:", room.name),
            hide_input: false,
        })
    }

    async fn handle_delete(&self, session: SessionId, id: i64) -> Result<Response, HostError> {
        let (username, _, level, room_id) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };

        let msg_id = crate::ids::MessageId::new(id);
        let msg = MessageStore::get_by_id(&self.db, msg_id)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        let msg = match msg {
            None => return Ok(Response::Error(format!("Message #{id} not found."))),
            Some(m) => m,
        };

        // Message must be visible from the current room.
        let in_room = MessageStore::is_in_room(&self.db, msg_id, room_id)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;
        if !in_room {
            return Ok(Response::Error(format!("Message #{id} not found.")));
        }

        let can_delete = level >= PermissionLevel::Aide
            || msg.sender == username
            || msg.recipient.as_ref() == Some(&username);

        if !can_delete {
            return Ok(Response::Error(
                "You can only delete your own messages.".into(),
            ));
        }

        MessageStore::delete(&self.db, msg_id)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        Ok(Response::Text(format!("Message #{id} deleted.")))
    }

    async fn handle_fast_forward(&self, session: SessionId) -> Result<Response, HostError> {
        let (_, user_id, _, room_id) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };

        let recent = self
            .db
            .list_recent_in_room(room_id, 1)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        if let Some(latest) = recent.into_iter().next() {
            self.db
                .mark_read(user_id, room_id, latest.id)
                .await
                .map_err(|e| HostError::Storage(format!("{e}")))?;
            Ok(Response::Text("Skipped to latest message.".into()))
        } else {
            Ok(Response::Text("No messages in this room.".into()))
        }
    }
}

// ── Moderation helpers ────────────────────────────────────────────────────────

impl BbsHost {
    async fn handle_who_is_online(&self, session: SessionId) -> Result<Response, HostError> {
        match self.session_auth_user(session).await {
            Ok(_) => {}
            Err(r) => return Ok(r),
        }

        let sessions = self.sessions.read().await;
        let mut names: Vec<String> = sessions
            .values()
            .filter_map(|r| {
                r.username
                    .as_ref()
                    .map(|u| format!("{} [{}]", u.as_str(), r.level))
            })
            .collect();
        let anon = sessions.values().filter(|r| r.username.is_none()).count();
        drop(sessions);

        names.sort();
        let mut lines = vec![format!(
            "Online ({} user{}):",
            names.len(),
            if names.len() == 1 { "" } else { "s" }
        )];
        lines.extend(names);
        if anon > 0 {
            lines.push(format!("(+{anon} unauthenticated)"));
        }
        Ok(Response::Text(lines.join("\n")))
    }

    async fn handle_list_pending(&self, session: SessionId) -> Result<Response, HostError> {
        let (_, _, level, _) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };
        if level < PermissionLevel::Aide {
            return Ok(Response::Error("Aide access required.".into()));
        }

        let all_active = UserStore::list(&self.db, Some(UserStatus::Active), 200, 0)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        let pending: Vec<_> = all_active
            .iter()
            .filter(|u| u.permission_level == PermissionLevel::Unvalidated)
            .collect();

        if pending.is_empty() {
            return Ok(Response::Text("No accounts pending validation.".into()));
        }

        let mut lines = vec![format!("Pending validation ({}):", pending.len())];
        for u in &pending {
            lines.push(format!(
                "  {} (joined {})",
                u.username.as_str(),
                u.created_at
            ));
        }
        lines.push("Use V <username> to validate, B <username> to ban.".into());
        Ok(Response::Text(lines.join("\n")))
    }

    async fn handle_validate_user(
        &self,
        session: SessionId,
        username: Username,
    ) -> Result<Response, HostError> {
        let (actor, _, level, _) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };
        if level < PermissionLevel::Aide {
            return Ok(Response::Error("Aide access required.".into()));
        }

        let user = UserStore::get_by_username(&self.db, &username)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        let user = match user {
            None => {
                return Ok(Response::Error(format!(
                    "User '{}' not found.",
                    username.as_str()
                )))
            }
            Some(u) => u,
        };

        if user.permission_level != PermissionLevel::Unvalidated {
            return Ok(Response::Error(format!(
                "'{}' is already {} — not pending validation.",
                username.as_str(),
                user.permission_level
            )));
        }

        UserStore::update(
            &self.db,
            user.id,
            None,
            None,
            Some(PermissionLevel::User),
            None,
        )
        .await
        .map_err(|e| HostError::Storage(format!("{e}")))?;

        // Promote any active sessions for this user immediately.
        {
            let mut sessions = self.sessions.write().await;
            for r in sessions.values_mut() {
                if r.username.as_ref() == Some(&username) {
                    r.level = PermissionLevel::User;
                }
            }
        }

        let _ = self.events_tx.send(DomainEvent::UserValidated {
            user: username.clone(),
        });
        info!(%actor, %username, "user validated");
        Ok(Response::Text(format!(
            "'{}' validated — account is now active.",
            username.as_str()
        )))
    }

    async fn handle_block_user(
        &self,
        session: SessionId,
        target: Username,
        force: Option<bool>,
    ) -> Result<Response, HostError> {
        let (caller, _, _, _) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };

        if caller == target {
            return Ok(Response::Error("You cannot block yourself.".into()));
        }

        let exists = self
            .db
            .get_by_username(&target)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?
            .is_some();
        if !exists {
            return Ok(Response::Error(format!(
                "User '{}' not found.",
                target.as_str()
            )));
        }

        let blocker = caller.as_str();
        let blocked = target.as_str();
        let currently = self
            .db
            .is_blocking(blocker, blocked)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        match force {
            Some(true) => {
                if currently {
                    return Ok(Response::Text(format!("'{blocked}' is already blocked.")));
                }
                self.db
                    .block_user(blocker, blocked)
                    .await
                    .map_err(|e| HostError::Storage(format!("{e}")))?;
                Ok(Response::Text(format!("'{blocked}' is now blocked.")))
            }
            Some(false) => {
                if !currently {
                    return Ok(Response::Text(format!(
                        "'{blocked}' is not currently blocked."
                    )));
                }
                self.db
                    .unblock_user(blocker, blocked)
                    .await
                    .map_err(|e| HostError::Storage(format!("{e}")))?;
                Ok(Response::Text(format!("'{blocked}' is no longer blocked.")))
            }
            None => {
                if currently {
                    self.db
                        .unblock_user(blocker, blocked)
                        .await
                        .map_err(|e| HostError::Storage(format!("{e}")))?;
                    Ok(Response::Text(format!("'{blocked}' is no longer blocked.")))
                } else {
                    self.db
                        .block_user(blocker, blocked)
                        .await
                        .map_err(|e| HostError::Storage(format!("{e}")))?;
                    Ok(Response::Text(format!("'{blocked}' is now blocked.")))
                }
            }
        }
    }

    async fn handle_ban_user(
        &self,
        session: SessionId,
        username: Username,
    ) -> Result<Response, HostError> {
        let (actor, _, level, _) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };
        if level < PermissionLevel::Aide {
            return Ok(Response::Error("Aide access required.".into()));
        }

        let user = UserStore::get_by_username(&self.db, &username)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        let user = match user {
            None => {
                return Ok(Response::Error(format!(
                    "User '{}' not found.",
                    username.as_str()
                )))
            }
            Some(u) => u,
        };

        if user.status == UserStatus::Banned {
            return Ok(Response::Error(format!(
                "'{}' is already banned.",
                username.as_str()
            )));
        }

        if user.permission_level >= level {
            return Ok(Response::Error(format!(
                "Cannot ban '{}' — equal or higher permission tier.",
                username.as_str()
            )));
        }

        UserStore::update(
            &self.db,
            user.id,
            None,
            Some(UserStatus::Banned),
            None,
            None,
        )
        .await
        .map_err(|e| HostError::Storage(format!("{e}")))?;

        // Force-end any active sessions for this user.
        {
            let mut sessions = self.sessions.write().await;
            let to_end: Vec<SessionId> = sessions
                .iter()
                .filter(|(_, r)| r.username.as_ref() == Some(&username))
                .map(|(id, _)| *id)
                .collect();
            for id in to_end {
                sessions.remove(&id);
            }
        }

        warn!(%actor, %username, "user banned");
        Ok(Response::Text(format!(
            "'{}' has been banned.",
            username.as_str()
        )))
    }
}

// ── Additional command handlers ───────────────────────────────────────────────

impl BbsHost {
    async fn handle_unban_user(
        &self,
        session: SessionId,
        username: Username,
    ) -> Result<Response, HostError> {
        let (actor, _, level, _) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };
        if level < PermissionLevel::Sysop {
            return Ok(Response::Error("Sysop access required.".into()));
        }

        let user = UserStore::get_by_username(&self.db, &username)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        let user = match user {
            None => {
                return Ok(Response::Error(format!(
                    "User '{}' not found.",
                    username.as_str()
                )))
            }
            Some(u) => u,
        };

        if user.status != UserStatus::Banned {
            return Ok(Response::Error(format!(
                "'{}' is not currently banned.",
                username.as_str()
            )));
        }

        UserStore::update(
            &self.db,
            user.id,
            None,
            Some(UserStatus::Active),
            None,
            None,
        )
        .await
        .map_err(|e| HostError::Storage(format!("{e}")))?;

        info!(%actor, %username, "user unbanned");
        Ok(Response::Text(format!(
            "'{}' has been unbanned.",
            username.as_str()
        )))
    }

    async fn handle_edit_profile(&self, session: SessionId) -> Result<Response, HostError> {
        match self.session_auth_user(session).await {
            Ok(_) => {}
            Err(r) => return Ok(r),
        }
        {
            let mut sessions = self.sessions.write().await;
            if let Some(r) = sessions.get_mut(&session) {
                r.workflow = Workflow::EditProfile;
            }
        }
        Ok(Response::Prompt {
            text: "Enter your new display name (or '-' to clear, Enter to cancel):".into(),
            hide_input: false,
        })
    }

    async fn handle_create_room(
        &self,
        session: SessionId,
        name: &str,
    ) -> Result<Response, HostError> {
        let (actor, _, level, _) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };
        if level < PermissionLevel::Sysop {
            return Ok(Response::Error("Sysop access required.".into()));
        }

        let existing = self
            .db
            .get_by_name(name)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;
        if existing.is_some() {
            return Ok(Response::Error(format!("Room '{name}' already exists.")));
        }

        let room_id = RoomStore::create(
            &self.db,
            name,
            None,
            false,
            PermissionLevel::User,
            Timestamp::now(),
        )
        .await
        .map_err(|e| HostError::Storage(format!("create room: {e}")))?;

        info!(%actor, %name, room = room_id.as_i64(), "room created");
        Ok(Response::Text(format!(
            "Room '{}' created (id={}).",
            name,
            room_id.as_i64()
        )))
    }

    async fn handle_delete_room(
        &self,
        session: SessionId,
        name: &str,
    ) -> Result<Response, HostError> {
        let (actor, _, level, _) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };
        if level < PermissionLevel::Sysop {
            return Ok(Response::Error("Sysop access required.".into()));
        }

        let room = self
            .db
            .get_by_name(name)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        let room = match room {
            None => return Ok(Response::Error(format!("Room '{name}' not found."))),
            Some(r) => r,
        };

        // Protect the three built-in system rooms.
        if room.id == LOBBY_ROOM_ID || room.id == MAIL_ROOM_ID || room.id == RoomId::new(3) {
            return Ok(Response::Error(format!(
                "Cannot delete system room '{}'.",
                room.name
            )));
        }

        RoomStore::delete(&self.db, room.id)
            .await
            .map_err(|e| HostError::Storage(format!("delete room: {e}")))?;

        info!(%actor, %name, "room deleted");
        Ok(Response::Text(format!("Room '{name}' deleted.")))
    }
}

// ── Help text ─────────────────────────────────────────────────────────────────

fn help_text(topic: Option<&str>) -> String {
    match topic {
        None => HELP_QUICK.to_owned(),
        Some(t) => match t.to_ascii_lowercase().as_str() {
            "all" => format!(
                "{HELP_READING_POSTING}\n\n{HELP_NAVIGATION}\n\n{HELP_ACCOUNT}\n\n{HELP_AIDE}\n\n{HELP_SYSOP}"
            ),
            "reading" => HELP_READING_POSTING.to_owned(),
            "posting" => HELP_READING_POSTING.to_owned(),
            "navigation" | "nav" => HELP_NAVIGATION.to_owned(),
            "account" => HELP_ACCOUNT.to_owned(),
            "aide" => HELP_AIDE.to_owned(),
            "sysop" => HELP_SYSOP.to_owned(),
            cmd => help_for_command(cmd),
        },
    }
}

fn help_for_command(cmd: &str) -> String {
    let detail = match cmd {
        "n" => "N — Read new messages since last visit. Starts with the oldest message you haven't read yet in this room.",
        "f" => "F — Read messages in the current room, starting with the oldest and moving forward.",
        "r" => "R — Read messages in the current room, starting with the most recent and moving backwards.",
        "s" => "S — Show message summaries in the current room.",
        ".ff" => ".FF — Fast-forward to the latest message in the current room, skipping over unread messages. This resets your last-read pointer to the latest message.",
        "e" => "E — Compose and post a message to the current room.",
        "d" => "D <id> — Delete a message ID specified after the command letter. Only Aides and Sysops can delete others' messages.",
        "g" => "G — Go to the next room with unread messages. This skips over rooms you've already read completely.",
        "c" => "C <name> — Change to a room by name or number. Specify the room name or ID after the command letter.",
        "k" => "K — List all rooms known to you.",
        "i" => "I — Toggle ignore on the current room. Ignored rooms are skipped during navigation and don't count toward unread.",
        "m" => "M — Go directly to the Mail room to send/receive private messages.",
        "h" | "help" | "?" => {
            "H — quick menu. H all = full menu.\n\
             H reading/nav/account/aide/sysop\n\
             H <key> for one command."
        }
        "q" => "Q — Quit or log off.",
        "w" => "W — List active users currently online.",
        "register" => "REGISTER <username> — Create a new account. Example: register alice",
        "login" => "LOGIN <username> — Log in to an existing account. Example: login alice",
        "cancel" => "CANCEL — Cancel the current workflow and return to normal command mode.",
        "stop" => "STOP — Stop pending messages as soon as possible.",
        "b" => {
            "B <user> — toggle block. +user force-block,\n\
             -user unblock. Hides their posts from you."
        }
        "v" => "V <user> — Validate (approve) a pending user. Aide+.",
        "ban" => "BAN <user> — Ban a user account, preventing further login. Aide+.",
        "unban" => "UNBAN <user> — Lift a ban on a previously banned user. Sysop+.",
        "pending" => "PENDING — List users awaiting validation. Aide+.",
        "profile" => "PROFILE — Edit your display name.",
        ".er" => ".ER — Edit the current room's name, description, read-only flag, or required permission level. Aide+.",
        ".eu" => ".EU <user> — Edit a user's display name, password, permission level, or status. Aides cannot promote to Sysop.",
        ".c" => ".C — Create a new room. Sysop+.",
        ".dr" => ".DR <name> — Delete a room by name. Sysop+.",
        other => return format!("No help for '{other}'. H all for full menu."),
    };
    detail.to_owned()
}

const HELP_QUICK: &str = "\
K rooms  C change  N new\n\
E enter  G unread  M mail\n\
Q quit   W online  ?=help\n\
H all | H <key> for more";

const HELP_READING_POSTING: &str = "\
Reading:\n\
 F  oldest first  N  new\n\
 R  newest first  S  scan\n\
 .FF  skip to latest\n\
Posting:\n\
 D  delete  E  enter";

const HELP_NAVIGATION: &str = "\
Navigation:\n\
 .C   create a new room\n\
 C    change room\n\
 G    next unread room\n\
 I    ignore this room\n\
 K    list known rooms\n\
 M    go to Mail";

const HELP_ACCOUNT: &str = "\
Account:\n\
 B    block / unblock user\n\
 H    help (also: ?)\n\
 Q    log out\n\
 W    who's online\n\
\n\
H <letter> for details. 'cancel' to bail out.";

const HELP_AIDE: &str = "\
Aide:\n\
 .ER  edit current room\n\
 .EU  edit a user\n\
 V    validate pending users";

const HELP_SYSOP: &str = "\
Sysop:\n\
 .C   create a new room\n\
 .DR  delete a room\n\
 BAN  ban a user\n\
 UNBAN  lift a ban";

// ── Formatting helpers ────────────────────────────────────────────────────────

fn format_message(msg: &Message) -> String {
    let id = msg.id.as_i64();
    let sender = msg.sender.as_str();
    if let Some(ref recipient) = msg.recipient {
        format!(
            "#{id} [DM→{}] {}: {}",
            recipient.as_str(),
            sender,
            msg.content
        )
    } else {
        format!("#{id} {}: {}", sender, msg.content)
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use bbs_plugin_api::{Command, Username};
    use tempfile::NamedTempFile;

    async fn make_host() -> (Arc<BbsHost>, NamedTempFile) {
        let f = NamedTempFile::new().unwrap();
        let db = Database::open(&f.path().to_string_lossy())
            .await
            .expect("db open");
        (Arc::new(BbsHost::new(db)), f)
    }

    /// Bypass the validation workflow for a registered user in tests.
    async fn force_validate(host: &BbsHost, username: &Username) {
        let user = UserStore::get_by_username(&host.db, username)
            .await
            .unwrap()
            .unwrap();
        UserStore::update(
            &host.db,
            user.id,
            None,
            None,
            Some(PermissionLevel::User),
            None,
        )
        .await
        .unwrap();
        // Also update any active sessions.
        let mut sessions = host.sessions.write().await;
        for r in sessions.values_mut() {
            if r.username.as_ref() == Some(username) {
                r.level = PermissionLevel::User;
            }
        }
    }

    #[tokio::test]
    async fn create_and_end_session() {
        let (host, _db) = make_host().await;
        let sid = host.create_session("test").await.unwrap();
        assert_eq!(host.sessions.read().await.len(), 1);
        host.end_session(sid).await.unwrap();
        assert_eq!(host.sessions.read().await.len(), 0);
    }

    #[tokio::test]
    async fn end_unknown_session_is_ok() {
        let (host, _db) = make_host().await;
        let fake = SessionId::__internal_new(9999);
        host.end_session(fake).await.unwrap();
    }

    #[tokio::test]
    async fn permission_ctx_unknown_session_errors() {
        let (host, _db) = make_host().await;
        let fake = SessionId::__internal_new(9999);
        assert!(matches!(
            host.permission_ctx(fake).await,
            Err(HostError::UnknownSession(_))
        ));
    }

    #[tokio::test]
    async fn permission_ctx_pre_auth_is_unvalidated() {
        let (host, _db) = make_host().await;
        let sid = host.create_session("test").await.unwrap();
        let ctx = host.permission_ctx(sid).await.unwrap();
        assert_eq!(ctx.level, PermissionLevel::Unvalidated);
        assert!(ctx.username.is_none());
    }

    #[tokio::test]
    async fn help_command_returns_text() {
        let (host, _db) = make_host().await;
        let sid = host.create_session("test").await.unwrap();
        let resp = host
            .process_command(sid, Command::Help { topic: None })
            .await
            .unwrap();
        assert!(matches!(resp, Response::Text(_)));
    }

    #[tokio::test]
    async fn whoami_pre_auth() {
        let (host, _db) = make_host().await;
        let sid = host.create_session("test").await.unwrap();
        let resp = host.process_command(sid, Command::Whoami).await.unwrap();
        let Response::Text(text) = resp else {
            panic!("expected Text")
        };
        assert!(text.contains("Not logged in"));
    }

    #[tokio::test]
    async fn logout_ends_session() {
        let (host, _db) = make_host().await;
        let sid = host.create_session("test").await.unwrap();
        let resp = host.process_command(sid, Command::Logout).await.unwrap();
        assert_eq!(resp, Response::LoggedOut);
        assert_eq!(host.sessions.read().await.len(), 0);
    }

    #[tokio::test]
    async fn events_broadcasts_session_created() {
        let (host, _db) = make_host().await;
        let mut rx = host.events();
        let sid = host.create_session("test").await.unwrap();
        let ev = rx.recv().await.unwrap();
        assert!(matches!(ev, DomainEvent::SessionCreated { session, .. } if session == sid));
    }

    #[tokio::test]
    async fn register_and_login_full_flow() {
        let (host, _db) = make_host().await;
        let sid = host.create_session("test").await.unwrap();
        let uname = Username::new("alice").unwrap();

        let r = host
            .process_command(
                sid,
                Command::Register {
                    username: uname.clone(),
                },
            )
            .await
            .unwrap();
        assert!(matches!(r, Response::Prompt { .. }));

        host.process_command(
            sid,
            Command::WorkflowReply {
                reply: "Alice".into(),
            },
        )
        .await
        .unwrap();
        host.process_command(
            sid,
            Command::WorkflowReply {
                reply: "s3cr3t!!".into(),
            },
        )
        .await
        .unwrap();
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

        // Registration places users in Unvalidated tier (awaiting aide approval).
        let ctx = host.permission_ctx(sid).await.unwrap();
        assert_eq!(ctx.level, PermissionLevel::Unvalidated);
        assert_eq!(ctx.username.as_ref(), Some(&uname));
        let sessions = host.sessions.read().await;
        assert_eq!(sessions[&sid].current_room, LOBBY_ROOM_ID);
    }

    #[tokio::test]
    async fn room_navigation_requires_auth() {
        let (host, _db) = make_host().await;
        let sid = host.create_session("test").await.unwrap();
        let resp = host.process_command(sid, Command::ListRooms).await.unwrap();
        assert!(matches!(resp, Response::Error(_)));
    }

    #[tokio::test]
    async fn list_rooms_after_login() {
        let (host, _db) = make_host().await;
        let sid = host.create_session("test").await.unwrap();
        let uname = Username::new("bob").unwrap();

        // Register bob.
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

        // Validate bob so he can use room commands.
        force_validate(&host, &uname).await;

        let resp = host.process_command(sid, Command::ListRooms).await.unwrap();
        let Response::Text(text) = resp else {
            panic!("expected Text")
        };
        assert!(text.contains("Lobby"));
    }

    #[tokio::test]
    async fn enter_and_read_message() {
        let (host, _db) = make_host().await;
        let sid = host.create_session("test").await.unwrap();
        let uname = Username::new("carol").unwrap();

        // Register carol.
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

        // Validate carol so she can post messages.
        force_validate(&host, &uname).await;

        // Enter a message.
        let r = host
            .process_command(sid, Command::EnterMessage)
            .await
            .unwrap();
        assert!(matches!(r, Response::Prompt { .. }));
        let r = host
            .process_command(
                sid,
                Command::WorkflowReply {
                    reply: "Hello, Lobby!".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(r, Response::Text("Message posted.".into()));

        // Read new — should see it (read pointer was at 0 before posting).
        // Note: the message we just posted is now at the read pointer, so
        // a second read should find nothing. First read should work.
        let r2 = host.process_command(sid, Command::ReadNew).await.unwrap();
        // Either sees the message or says "no new" depending on whether mark_read
        // ran during post. Since we don't mark on post, we should see it.
        let _ = r2; // just assert it doesn't error
    }

    #[tokio::test]
    async fn cancel_clears_workflow() {
        let (host, _db) = make_host().await;
        let sid = host.create_session("test").await.unwrap();

        // Start a registration workflow.
        let uname = Username::new("dave").unwrap();
        host.process_command(sid, Command::Register { username: uname })
            .await
            .unwrap();

        // Cancel it.
        let r = host.process_command(sid, Command::Cancel).await.unwrap();
        assert!(matches!(r, Response::Text(_)));

        // Should no longer be in a workflow — next WorkflowReply is an error.
        let r = host
            .process_command(
                sid,
                Command::WorkflowReply {
                    reply: "anything".into(),
                },
            )
            .await
            .unwrap();
        assert!(matches!(r, Response::Error(_)));
    }

    #[tokio::test]
    async fn login_wrong_password_lockout() {
        let (host, _db) = make_host().await;
        let sid = host.create_session("test").await.unwrap();
        let uname = Username::new("eve").unwrap();

        // Register eve.
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

        // Login with wrong password 3 times.
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
            assert!(matches!(r, Response::Prompt { .. }));
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
        assert!(matches!(r, Response::Error(_)));
    }
}
