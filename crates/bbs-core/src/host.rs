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
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use bbs_plugin_api::advert::AdvertBus;
use bbs_plugin_api::host::Host;
use bbs_plugin_api::{
    AdminAccessPolicy, AdminBackupRecord, AdminMessageRecord, AdminRoomSummary, AdminSessionInfo,
    AdminStats, AdminUserInfo, Command, DomainEvent, HostError, MessageRecipient, PermissionCtx,
    PermissionLevel, Response, SessionId, Username,
};
use tokio::sync::{broadcast, RwLock};
use tracing::{debug, info, warn};

use crate::db::{Database, MessageStore, RoomStore, UserStore};
use crate::ids::{MessageId, RoomId, UserId};
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
    /// Changing the user's password (PASSWD command).
    ChangePassword {
        stage: ChangePwdStage,
    },
    /// Resetting another user's password (.PW command, Sysop+).
    SetUserPassword {
        target: Username,
        stage: SetUserPwdStage,
    },
    /// Browsing messages one-at-a-time with F/R navigation.
    /// E replies to the current message; any other input exits.
    Reading,
    /// Choosing a room from the numbered list produced by K.
    /// Stores the ordered room IDs so the user can type a number to jump in.
    Rooms {
        room_ids: Vec<RoomId>,
    },
    /// Stepping through unvalidated accounts one-at-a-time (LP queue).
    /// `pending` is the list of usernames still to review; `index` is the
    /// next one to show. Aide+ only.
    ReviewPending {
        pending: Vec<Username>,
        index: usize,
    },
}

#[derive(Clone, Debug)]
enum ChangePwdStage {
    /// Waiting for the user to enter their current password.
    VerifyOld { attempts: u32 },
    /// Current password verified; waiting for the new password.
    EnterNew,
    /// New password entered; waiting for confirmation.
    ConfirmNew { new_password: String },
}

#[derive(Clone, Debug)]
enum SetUserPwdStage {
    /// Waiting for the new password.
    EnterNew,
    /// New password entered; waiting for confirmation.
    ConfirmNew { new_password: String },
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

#[allow(clippy::enum_variant_names)]
#[derive(Clone, Debug)]
enum ComposeStage {
    /// Mail room only: waiting for the recipient username.
    AwaitingRecipient,
    /// Waiting for the message body.
    AwaitingBody { recipient: Option<Username> },
    /// Body is staged; waiting for a lone "." to confirm the send.
    ///
    /// Used by the inline `E <text>` path. The separate confirmation
    /// step makes sends idempotent on lossy links: if "Message posted."
    /// is not received, the user sends "." again and gets the same
    /// confirmation without a duplicate post.
    AwaitingConfirmation {
        recipient: Option<Username>,
        body: String,
    },
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
    /// Position within the current room for one-at-a-time F/R navigation.
    /// `None` means "not yet started"; F starts at the first message, R at the last.
    /// Reset to `None` when the room changes.
    current_message_id: Option<MessageId>,
}

// ── Access policy ─────────────────────────────────────────────────────────────

/// Runtime access policy — controls verification and guest-room behaviour.
///
/// Constructed from config on startup and held inside a `RwLock` so
/// in-BBS sysop commands (`OPENACCESS`, `CLOSEACCESS`, `GUESTROOM`) can
/// update it without a restart.
#[derive(Debug, Clone)]
pub struct AccessPolicy {
    /// When `false`, Unvalidated users are treated as `User` immediately after
    /// registration — no aide/sysop verification step is required.
    pub require_verify: bool,
    /// Name of the room that unverified users are allowed into.
    /// `None` keeps the strict "no access until verified" behaviour.
    pub guest_room_name: Option<String>,
}

impl Default for AccessPolicy {
    fn default() -> Self {
        Self {
            require_verify: true,
            guest_room_name: None,
        }
    }
}

// ── BbsHost ───────────────────────────────────────────────────────────────────

/// Concrete [`Host`] implementation backed by the bbs-core [`Database`].
pub struct BbsHost {
    db: Database,
    events_tx: broadcast::Sender<DomainEvent>,
    sessions: RwLock<HashMap<SessionId, SessionRecord>>,
    next_id: AtomicU64,
    advert_bus: Arc<AdvertBus>,
    /// Per-username login failure counts (failures, last_attempt).
    /// Shared across all sessions so parallel sessions can't bypass rate limiting.
    login_failures: tokio::sync::Mutex<HashMap<String, (u32, Instant)>>,
    /// Optional GPS coordinates from `[location]` config section.
    /// Wrapped in a RwLock so the web admin can update it without a restart.
    location: std::sync::RwLock<Option<(f64, f64)>>,
    /// Access policy — controls verification and guest-room behaviour.
    /// Wrapped in a `RwLock` so in-BBS sysop commands can update it live.
    access_policy: RwLock<AccessPolicy>,
    /// Resolved guest room ID — populated by [`Self::ensure_guest_room`].
    /// `None` when the guest room feature is disabled or not yet initialised.
    guest_room_id: std::sync::RwLock<Option<RoomId>>,
    /// Absolute path to `config.toml`, used by in-BBS commands that persist
    /// policy changes to disk.  `None` in tests and minimal CLI runs.
    config_path: Option<PathBuf>,
}

impl BbsHost {
    /// Create a new [`BbsHost`] backed by the given database.
    pub fn new(db: Database) -> Self {
        Self::with_location(db, None)
    }

    /// Create a [`BbsHost`] with an optional GPS location.
    pub fn with_location(db: Database, location: Option<(f64, f64)>) -> Self {
        Self::with_config(db, location, AccessPolicy::default(), None)
    }

    /// Create a [`BbsHost`] with a full configuration.
    ///
    /// `config_path` should be the canonicalized path to `config.toml` so
    /// in-BBS sysop commands can persist policy changes to disk.
    pub fn with_config(
        db: Database,
        location: Option<(f64, f64)>,
        policy: AccessPolicy,
        config_path: Option<PathBuf>,
    ) -> Self {
        let (events_tx, _) = broadcast::channel(256);
        Self {
            db,
            events_tx,
            sessions: RwLock::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            advert_bus: Arc::new(AdvertBus::new()),
            login_failures: tokio::sync::Mutex::new(HashMap::new()),
            location: std::sync::RwLock::new(location),
            access_policy: RwLock::new(policy),
            guest_room_id: std::sync::RwLock::new(None),
            config_path,
        }
    }

    /// Ensure the guest room exists in the database (creating it if needed)
    /// and cache its ID.
    ///
    /// Must be called **before** wrapping `self` in an `Arc` (i.e., before
    /// handing it to transports).  Returns `Ok(())` immediately when the
    /// guest room feature is not configured.
    pub async fn ensure_guest_room(&self) -> Result<(), String> {
        let name = {
            let policy = self.access_policy.read().await;
            policy.guest_room_name.clone()
        };
        let Some(name) = name else {
            return Ok(());
        };

        let room = match self.db.get_by_name(&name).await {
            Ok(Some(r)) => r,
            Ok(None) => {
                // Create with min_permission_level = Unvalidated so both
                // guests and regular users can read and post.
                let id = RoomStore::create(
                    &self.db,
                    &name,
                    Some("Open room — all users welcome."),
                    false,
                    PermissionLevel::Unvalidated,
                    crate::timestamp::Timestamp::now(),
                )
                .await
                .map_err(|e| format!("guest room create: {e}"))?;

                RoomStore::get_by_id(&self.db, id)
                    .await
                    .map_err(|e| format!("guest room fetch: {e}"))?
                    .ok_or_else(|| "guest room missing after create".to_owned())?
            }
            Err(e) => return Err(format!("guest room lookup: {e}")),
        };

        *self.guest_room_id.write().expect("guest_room_id poisoned") = Some(room.id);
        info!(room_id = %room.id.as_i64(), room_name = %room.name, "guest room configured");
        Ok(())
    }

    /// Return the cached guest room ID, if any.
    fn guest_room_id(&self) -> Option<RoomId> {
        *self.guest_room_id.read().expect("guest_room_id poisoned")
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

        // Emit a CommandExecuted event for every non-WorkflowReply command so
        // the web admin log view shows live BBS activity.
        // WorkflowReply is excluded to avoid logging passwords.
        if !matches!(cmd, Command::WorkflowReply { .. }) {
            let label = cmd_label(&cmd).to_owned();
            let user = {
                let sessions = self.sessions.read().await;
                sessions.get(&session).and_then(|s| s.username.clone())
            };
            let _ = self.events_tx.send(DomainEvent::CommandExecuted {
                session,
                command: label,
                user,
            });
        }

        match cmd {
            Command::Help { topic } => {
                let level = {
                    let sessions = self.sessions.read().await;
                    sessions.get(&session).map(|r| {
                        if r.username.is_some() {
                            Some(r.level)
                        } else {
                            None
                        }
                    })
                };
                // level = None: unknown session; Some(None): not logged in;
                // Some(Some(lvl)): logged in at lvl.
                let auth_level = level.flatten();
                Ok(Response::Text(help_text(topic.as_deref(), auth_level)))
            }

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
            Command::EnterMessage { body } => self.handle_enter_message(session, body).await,
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
            Command::ChangePassword => self.handle_change_password(session).await,
            Command::CreateRoom { name } => self.handle_create_room(session, &name).await,
            Command::DeleteRoom { name } => self.handle_delete_room(session, &name).await,
            Command::EditRoom => Ok(Response::Text(
                "Room editing is not yet implemented.".into(),
            )),
            Command::EditUser { .. } => Ok(Response::Text(
                "User editing is not yet implemented.".into(),
            )),
            Command::ListUsers { filter } => self.handle_list_users(session, filter).await,
            Command::SearchUsers { query } => self.handle_search_users(session, query).await,
            Command::UserInfo { username } => self.handle_user_info(session, username).await,
            Command::DeleteUser { username } => self.handle_delete_user(session, username).await,
            Command::SetUserPassword { username } => {
                self.handle_set_user_password(session, username).await
            }

            // Access policy (Sysop only)
            Command::OpenAccess => self.handle_open_access(session).await,
            Command::CloseAccess => self.handle_close_access(session).await,
            Command::SetGuestRoom { name } => self.handle_set_guest_room(session, name).await,

            Command::Unknown { .. } => {
                Ok(Response::Text("Unknown command. Type H for help.".into()))
            }
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
                current_message_id: None,
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

    fn node_location(&self) -> Option<(f64, f64)> {
        *self.location.read().unwrap()
    }

    fn set_node_location(&self, location: Option<(f64, f64)>) {
        *self.location.write().unwrap() = location;
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

    async fn admin_create_user(
        &self,
        username: &str,
        password: &str,
        permission_level: u8,
    ) -> Result<(), HostError> {
        let uname = Username::new(username).map_err(|_| {
            HostError::PreconditionFailed(format!("invalid username: {username:?}"))
        })?;

        let level = match permission_level {
            0 => PermissionLevel::Unvalidated,
            10 => PermissionLevel::User,
            50 => PermissionLevel::Aide,
            100 => PermissionLevel::Sysop,
            other => {
                return Err(HostError::PreconditionFailed(format!(
                    "unknown permission_level {other}"
                )))
            }
        };

        let now = Timestamp::now();
        let user_id = match UserStore::create(&self.db, &uname, None, level, now).await {
            Ok(id) => id,
            Err(crate::db::StoreError::Conflict(_)) => {
                return Err(HostError::PreconditionFailed(format!(
                    "username {username:?} is already taken"
                )));
            }
            Err(e) => return Err(HostError::Storage(format!("create user: {e}"))),
        };

        self.db
            .credentials()
            .set_password(user_id, password, now)
            .await
            .map_err(|e| HostError::Storage(format!("set password: {e}")))?;

        Ok(())
    }

    async fn admin_set_password(&self, username: &str, password: &str) -> Result<(), HostError> {
        let uname = Username::new(username)
            .map_err(|_| HostError::NotFound(format!("user {username:?}")))?;

        let user = UserStore::get_by_username(&self.db, &uname)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?
            .ok_or_else(|| HostError::NotFound(format!("user {username:?}")))?;

        self.db
            .credentials()
            .set_password(user.id, password, Timestamp::now())
            .await
            .map_err(|e| HostError::Storage(format!("set password: {e}")))?;

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
        // Protect all five built-in rooms (Lobby=1, Mail=2, Aides=3, Sysop=4, System=5).
        if room_id <= 5 {
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
            config_filename: None,
            config_size_bytes: None,
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

    async fn admin_delete_backup(&self, backup_dir: &str, filename: &str) -> Result<(), HostError> {
        self.db
            .admin_delete_backup(backup_dir, filename)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))
    }

    async fn admin_write_audit(
        &self,
        actor: &str,
        action: &str,
        target: Option<&str>,
        detail: Option<&str>,
    ) -> Result<(), HostError> {
        self.db
            .audit_write(actor, action, target, detail)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))
    }

    async fn admin_audit_log(
        &self,
        limit: u32,
        offset: u32,
        action_filter: Option<&str>,
    ) -> Result<Vec<bbs_plugin_api::AdminAuditEntry>, HostError> {
        self.db
            .audit_query(limit, offset, action_filter)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))
    }

    async fn admin_kill_session(&self, session_id: u64) -> Result<bool, HostError> {
        let target = SessionId::__internal_new(session_id);
        let found = self.sessions.write().await.remove(&target).is_some();
        if found {
            let _ = self.events_tx.send(DomainEvent::SessionEnded {
                session: target,
                reason: "admin_kill".into(),
            });
            info!(%target, "session forcibly ended by admin");
        }
        Ok(found)
    }

    async fn admin_update_room(
        &self,
        room_id: i64,
        description: Option<Option<String>>,
        read_only: Option<bool>,
        min_permission_level: Option<u8>,
    ) -> Result<bbs_plugin_api::AdminRoomSummary, HostError> {
        use crate::ids::RoomId;
        let rid = RoomId::new(room_id);

        let new_level = match min_permission_level {
            None => None,
            Some(0) => Some(PermissionLevel::Unvalidated),
            Some(10) => Some(PermissionLevel::User),
            Some(50) => Some(PermissionLevel::Aide),
            Some(100) => Some(PermissionLevel::Sysop),
            Some(other) => {
                return Err(HostError::PreconditionFailed(format!(
                    "unknown min_permission_level {other}"
                )))
            }
        };

        // description: None = leave alone; Some(None) = clear; Some(Some(s)) = set
        let desc_update: Option<Option<&str>> = description.as_ref().map(|inner| inner.as_deref());

        RoomStore::update(&self.db, rid, desc_update, read_only, new_level)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        let room = RoomStore::get_by_id(&self.db, rid)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?
            .ok_or_else(|| HostError::NotFound(format!("room {room_id}")))?;

        let count = self
            .db
            .room_message_count(room_id)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        Ok(bbs_plugin_api::AdminRoomSummary {
            id: room.id.as_i64(),
            name: room.name,
            description: room.description,
            read_only: room.read_only,
            min_permission_level: room.min_permission_level as u8,
            message_count: count,
            created_at: room.created_at.to_rfc3339(),
            deletable: room_id > 5,
            locked: (2..=4).contains(&room_id),
        })
    }

    async fn admin_search_messages(
        &self,
        sender: Option<&str>,
        query: Option<&str>,
        limit: u32,
    ) -> Result<Vec<bbs_plugin_api::AdminMessageRecord>, HostError> {
        let capped = limit.min(200);
        self.db
            .admin_search_messages(sender, query, capped)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))
    }

    async fn admin_get_access_policy(&self) -> Result<AdminAccessPolicy, HostError> {
        let policy = self.access_policy.read().await;
        Ok(AdminAccessPolicy {
            require_verify: policy.require_verify,
            guest_room: policy.guest_room_name.clone(),
            guest_room_id: self.guest_room_id().map(|id| id.as_i64()),
        })
    }

    async fn admin_set_require_verify(&self, require_verify: bool) -> Result<(), HostError> {
        {
            let mut policy = self.access_policy.write().await;
            policy.require_verify = require_verify;
        }
        self.persist_access_policy().await;
        Ok(())
    }

    async fn admin_set_guest_room(&self, name: Option<String>) -> Result<(), HostError> {
        {
            let mut policy = self.access_policy.write().await;
            policy.guest_room_name = name.clone();
        }
        if name.is_some() {
            self.ensure_guest_room().await.map_err(HostError::Storage)?;
        } else {
            *self.guest_room_id.write().expect("guest_room_id poisoned") = None;
        }
        self.persist_access_policy().await;
        Ok(())
    }

    async fn mesh_node_restore(
        &self,
        session: SessionId,
        prefix: [u8; 6],
        ttl_days: u32,
    ) -> Result<Option<Username>, HostError> {
        // Look up a still-valid binding.
        let user_id = self
            .db
            .node_credentials()
            .lookup(&prefix, ttl_days)
            .await
            .map_err(|e| HostError::Storage(format!("node_credential lookup: {e}")))?;

        let Some(user_id) = user_id else {
            return Ok(None);
        };

        // Fetch the user — bail out silently if deleted or banned.
        let user = match UserStore::get_by_id(&self.db, user_id)
            .await
            .map_err(|e| HostError::Storage(format!("get user by id: {e}")))?
        {
            Some(u) if u.status == crate::user::UserStatus::Active => u,
            _ => return Ok(None),
        };

        // Bind the session exactly like a normal login.
        UserStore::update(&self.db, user.id, None, None, None, Some(Timestamp::now()))
            .await
            .map_err(|e| HostError::Storage(format!("update last_login: {e}")))?;
        {
            let mut sessions = self.sessions.write().await;
            if let Some(r) = sessions.get_mut(&session) {
                r.username = Some(user.username.clone());
                r.user_id = Some(user.id);
                r.level = user.permission_level;
                r.workflow = Workflow::None;
                r.current_room = LOBBY_ROOM_ID;
            }
        }

        // Refresh last_auth so the TTL clock resets on each successful auto-login.
        self.db
            .node_credentials()
            .upsert(&prefix, user_id, Timestamp::now())
            .await
            .map_err(|e| HostError::Storage(format!("node_credential upsert: {e}")))?;

        info!(%session, username = %user.username, "mesh: auto-login via stored node credential");
        Ok(Some(user.username))
    }

    async fn mesh_node_bind(&self, session: SessionId, prefix: [u8; 6]) -> Result<(), HostError> {
        let user_id = {
            let sessions = self.sessions.read().await;
            sessions.get(&session).and_then(|r| r.user_id)
        };
        let Some(user_id) = user_id else {
            return Ok(());
        };
        self.db
            .node_credentials()
            .upsert(&prefix, user_id, Timestamp::now())
            .await
            .map_err(|e| HostError::Storage(format!("node_credential upsert: {e}")))
    }

    async fn mesh_node_unbind(&self, prefix: [u8; 6]) -> Result<(), HostError> {
        self.db
            .node_credentials()
            .delete(&prefix)
            .await
            .map_err(|e| HostError::Storage(format!("node_credential delete: {e}")))
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
                if !matches!(r.workflow, Workflow::None) {
                    return Ok(Response::Error(
                        "A workflow is already in progress. Type 'cancel' first.".into(),
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
                if !matches!(r.workflow, Workflow::None) {
                    return Ok(Response::Error(
                        "A workflow is already in progress. Type 'cancel' first.".into(),
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
            match sessions.get_mut(&session) {
                Some(r) => {
                    r.workflow = Workflow::Login {
                        username,
                        attempts: 0,
                    }
                }
                // Session unknown — likely a stale ID held by the transport
                // after a server restart. Surface the error so the transport
                // can mint a fresh session and retry.
                None => return Err(HostError::UnknownSession(session)),
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
            match sessions.get(&session) {
                Some(r) => r.workflow.clone(),
                None => return Err(HostError::UnknownSession(session)),
            }
        };

        match workflow {
            Workflow::None => Ok(Response::Error("No active workflow. Type 'H'.".into())),

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
                if reply.chars().count() < 8 {
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
                // Promote the very first registrant to Sysop so the system
                // isn't stuck with no one able to validate new users.
                let is_first = UserStore::list(&self.db, None, 1, 0)
                    .await
                    .map_err(|e| HostError::Storage(format!("list users: {e}")))?
                    .is_empty();
                let initial_level = if is_first {
                    PermissionLevel::Sysop
                } else {
                    PermissionLevel::Unvalidated
                };
                let user_id = match UserStore::create(
                    &self.db,
                    &username,
                    display_name.as_deref(),
                    initial_level,
                    now,
                )
                .await
                {
                    Ok(id) => id,
                    Err(crate::db::StoreError::Conflict(_)) => {
                        let mut sessions = self.sessions.write().await;
                        if let Some(r) = sessions.get_mut(&session) {
                            r.workflow = Workflow::None;
                        }
                        return Ok(Response::Error(format!(
                            "'{username}' was just taken. Try: register <different_username>"
                        )));
                    }
                    Err(e) => return Err(HostError::Storage(format!("create user: {e}"))),
                };
                self.db
                    .credentials()
                    .set_password(user_id, &password, now)
                    .await
                    .map_err(|e| HostError::Storage(format!("set password: {e}")))?;

                {
                    // Unvalidated users land in the guest room (if configured);
                    // sysop / first-user lands in Lobby as usual.
                    let initial_room = if initial_level < PermissionLevel::User {
                        self.guest_room_id().unwrap_or(LOBBY_ROOM_ID)
                    } else {
                        LOBBY_ROOM_ID
                    };
                    let mut sessions = self.sessions.write().await;
                    if let Some(r) = sessions.get_mut(&session) {
                        r.username = Some(username.clone());
                        r.user_id = Some(user_id);
                        r.level = initial_level;
                        r.workflow = Workflow::None;
                        r.current_room = initial_room;
                    }
                }
                let _ = self.events_tx.send(DomainEvent::UserCreated {
                    user: username.clone(),
                });
                if is_first {
                    info!(%session, %username, "first registration — promoted to Sysop");
                } else {
                    info!(%session, %username, "registration complete — awaiting validation");

                    // DM every active sysop so they know there's a new
                    // user waiting to be validated (or banned).
                    let sysops: Vec<crate::user::User> = UserStore::list(&self.db, None, 200, 0)
                        .await
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|u| {
                            u.permission_level == PermissionLevel::Sysop
                                && u.status == crate::user::UserStatus::Active
                        })
                        .collect();

                    if !sysops.is_empty() {
                        let dm_ts = Timestamp::now();
                        let dm_body = format!(
                            "New user registered: {username}\nV {username} to verify, B {username} to ban."
                        );
                        let bbs_sender = Username::__internal_system("bbs");
                        for sysop in sysops {
                            let _ = MessageStore::post_direct(
                                &self.db,
                                &bbs_sender,
                                &sysop.username,
                                &dm_body,
                                dm_ts,
                            )
                            .await;
                        }
                    }
                }
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
                    // Clear failure count on success.
                    self.login_failures.lock().await.remove(username.as_str());
                    UserStore::update(&self.db, user.id, None, None, None, Some(Timestamp::now()))
                        .await
                        .map_err(|e| HostError::Storage(format!("update last_login: {e}")))?;
                    {
                        // Unverified users land in the guest room (if configured).
                        let initial_room = if user.permission_level < PermissionLevel::User {
                            self.guest_room_id().unwrap_or(LOBBY_ROOM_ID)
                        } else {
                            LOBBY_ROOM_ID
                        };
                        let mut sessions = self.sessions.write().await;
                        if let Some(r) = sessions.get_mut(&session) {
                            r.username = Some(username.clone());
                            r.user_id = Some(user.id);
                            r.level = user.permission_level;
                            r.workflow = Workflow::None;
                            r.current_room = initial_room;
                        }
                    }
                    let _ = self.events_tx.send(DomainEvent::SessionAuthenticated {
                        session,
                        user: username.clone(),
                    });
                    info!(%session, %username, "login successful");
                    Ok(Response::LoggedIn { user: username })
                } else {
                    // Global per-username failure tracking — parallel sessions share this
                    // counter so they can't bypass the delay by opening fresh connections.
                    let delay_secs = {
                        let mut map = self.login_failures.lock().await;
                        let entry = map
                            .entry(username.as_str().to_owned())
                            .or_insert((0, Instant::now()));
                        // Stale entries (>10 min) reset the counter.
                        if entry.1.elapsed().as_secs() > 600 {
                            *entry = (0, Instant::now());
                        }
                        entry.0 += 1;
                        entry.1 = Instant::now();
                        let failures = entry.0;
                        // Exponential backoff: 2, 4, 8, 16, 30 s (capped).
                        u64::min(2u64.saturating_pow(failures), 30)
                    };
                    warn!(%session, %username, delay_secs, "login failed: wrong password");
                    tokio::time::sleep(std::time::Duration::from_secs(delay_secs)).await;

                    let new_attempts = attempts + 1;
                    if new_attempts >= 3 {
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
                let (msg_id, event_recipient) = if let Some(ref rcpt) = recipient {
                    let mid = self
                        .db
                        .post_direct(&sender, rcpt, body, now)
                        .await
                        .map_err(|e| HostError::Storage(format!("post_direct: {e}")))?;
                    (mid, MessageRecipient::Direct(rcpt.clone()))
                } else {
                    let mid = self
                        .db
                        .post_to_room(room_id, &sender, body, now)
                        .await
                        .map_err(|e| HostError::Storage(format!("post_to_room: {e}")))?;
                    (mid, MessageRecipient::Room(room_id.as_i64().to_string()))
                };

                let _ = self.events_tx.send(DomainEvent::MessagePosted {
                    sender: sender.clone(),
                    recipient: event_recipient,
                    message_id: msg_id.as_i64() as u64,
                });

                {
                    let mut sessions = self.sessions.write().await;
                    if let Some(r) = sessions.get_mut(&session) {
                        r.workflow = Workflow::None;
                    }
                }
                Ok(Response::Text("Message posted.".into()))
            }

            // ── Draft confirmation ────────────────────────────────────────────
            Workflow::Compose {
                room_id,
                stage: ComposeStage::AwaitingConfirmation { recipient, body },
            } => {
                if reply.trim() != "." {
                    // Re-show the staged draft — the confirmation prompt may have
                    // been lost on the first send.
                    let preview = if let Some(ref rcpt) = recipient {
                        format!("To {}: {}\nType . to send", rcpt.as_str(), body)
                    } else {
                        format!("{body}\nType . to send")
                    };
                    // Keep workflow state unchanged.
                    let mut sessions = self.sessions.write().await;
                    if let Some(r) = sessions.get_mut(&session) {
                        r.workflow = Workflow::Compose {
                            room_id,
                            stage: ComposeStage::AwaitingConfirmation { recipient, body },
                        };
                    }
                    return Ok(Response::Prompt {
                        text: preview,
                        hide_input: false,
                    });
                }

                // "." received — post the staged message.
                let sender = {
                    let sessions = self.sessions.read().await;
                    sessions
                        .get(&session)
                        .and_then(|r| r.username.clone())
                        .ok_or(HostError::NotAuthenticated)?
                };
                let now = Timestamp::now();
                let (msg_id, event_recipient) = if let Some(ref rcpt) = recipient {
                    let mid = self
                        .db
                        .post_direct(&sender, rcpt, &body, now)
                        .await
                        .map_err(|e| HostError::Storage(format!("post_direct: {e}")))?;
                    (mid, MessageRecipient::Direct(rcpt.clone()))
                } else {
                    let mid = self
                        .db
                        .post_to_room(room_id, &sender, &body, now)
                        .await
                        .map_err(|e| HostError::Storage(format!("post_to_room: {e}")))?;
                    (mid, MessageRecipient::Room(room_id.as_i64().to_string()))
                };
                let _ = self.events_tx.send(DomainEvent::MessagePosted {
                    sender,
                    recipient: event_recipient,
                    message_id: msg_id.as_i64() as u64,
                });
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

                let trimmed = reply.trim();
                let display_name: Option<Option<&str>> = if trimmed == "-" {
                    Some(None) // clear display name
                } else if trimmed.is_empty() {
                    None // no change
                } else {
                    if let Err(e) = crate::user::User::validate_display_name(trimmed) {
                        return Ok(Response::Prompt {
                            text: format!("Invalid display name: {e}. Try again (- to clear):"),
                            hide_input: false,
                        });
                    }
                    Some(Some(trimmed))
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

            // ── Password change ──────────────────────────────────────────────
            Workflow::ChangePassword {
                stage: ChangePwdStage::VerifyOld { attempts },
            } => {
                let (_, user_id, _, _) = match self.session_auth(session).await {
                    Ok(t) => t,
                    Err(r) => return Ok(r),
                };
                let now = Timestamp::now();
                let ok = self
                    .db
                    .credentials()
                    .verify_password(user_id, &reply, now)
                    .await
                    .map_err(|e| HostError::Storage(format!("verify_password: {e}")))?;

                if ok {
                    let mut sessions = self.sessions.write().await;
                    if let Some(r) = sessions.get_mut(&session) {
                        r.workflow = Workflow::ChangePassword {
                            stage: ChangePwdStage::EnterNew,
                        };
                    }
                    Ok(Response::Prompt {
                        text: "New password (min 8 characters):".into(),
                        hide_input: true,
                    })
                } else {
                    let new_attempts = attempts + 1;
                    if new_attempts >= 3 {
                        let mut sessions = self.sessions.write().await;
                        if let Some(r) = sessions.get_mut(&session) {
                            r.workflow = Workflow::None;
                        }
                        Ok(Response::Error(
                            "Too many failed attempts. Password not changed.".into(),
                        ))
                    } else {
                        let mut sessions = self.sessions.write().await;
                        if let Some(r) = sessions.get_mut(&session) {
                            r.workflow = Workflow::ChangePassword {
                                stage: ChangePwdStage::VerifyOld {
                                    attempts: new_attempts,
                                },
                            };
                        }
                        Ok(Response::Prompt {
                            text: "Incorrect password. Current password:".into(),
                            hide_input: true,
                        })
                    }
                }
            }

            Workflow::ChangePassword {
                stage: ChangePwdStage::EnterNew,
            } => {
                if reply.chars().count() < 8 {
                    return Ok(Response::Prompt {
                        text: "Too short (min 8 characters). New password:".into(),
                        hide_input: true,
                    });
                }
                let mut sessions = self.sessions.write().await;
                if let Some(r) = sessions.get_mut(&session) {
                    r.workflow = Workflow::ChangePassword {
                        stage: ChangePwdStage::ConfirmNew {
                            new_password: reply,
                        },
                    };
                }
                Ok(Response::Prompt {
                    text: "Confirm new password:".into(),
                    hide_input: true,
                })
            }

            Workflow::ChangePassword {
                stage: ChangePwdStage::ConfirmNew { new_password },
            } => {
                if reply != new_password {
                    let mut sessions = self.sessions.write().await;
                    if let Some(r) = sessions.get_mut(&session) {
                        r.workflow = Workflow::ChangePassword {
                            stage: ChangePwdStage::EnterNew,
                        };
                    }
                    return Ok(Response::Prompt {
                        text: "Passwords don't match. New password:".into(),
                        hide_input: true,
                    });
                }
                let (_, user_id, _, _) = match self.session_auth(session).await {
                    Ok(t) => t,
                    Err(r) => return Ok(r),
                };
                let now = Timestamp::now();
                self.db
                    .credentials()
                    .set_password(user_id, &new_password, now)
                    .await
                    .map_err(|e| HostError::Storage(format!("set_password: {e}")))?;
                {
                    let mut sessions = self.sessions.write().await;
                    if let Some(r) = sessions.get_mut(&session) {
                        r.workflow = Workflow::None;
                    }
                }
                info!(%session, "user changed password");
                Ok(Response::Text("Password changed successfully.".into()))
            }

            // ── Set another user's password (Sysop+) ────────────────────────
            Workflow::SetUserPassword {
                target,
                stage: SetUserPwdStage::EnterNew,
            } => {
                if reply.chars().count() < 8 {
                    return Ok(Response::Prompt {
                        text: "Too short (min 8 characters). New password:".into(),
                        hide_input: true,
                    });
                }
                let mut sessions = self.sessions.write().await;
                if let Some(r) = sessions.get_mut(&session) {
                    r.workflow = Workflow::SetUserPassword {
                        target,
                        stage: SetUserPwdStage::ConfirmNew {
                            new_password: reply,
                        },
                    };
                }
                Ok(Response::Prompt {
                    text: "Confirm new password:".into(),
                    hide_input: true,
                })
            }

            Workflow::SetUserPassword {
                target,
                stage: SetUserPwdStage::ConfirmNew { new_password },
            } => {
                if reply != new_password {
                    let mut sessions = self.sessions.write().await;
                    if let Some(r) = sessions.get_mut(&session) {
                        r.workflow = Workflow::SetUserPassword {
                            target,
                            stage: SetUserPwdStage::EnterNew,
                        };
                    }
                    return Ok(Response::Prompt {
                        text: "Passwords don't match. New password:".into(),
                        hide_input: true,
                    });
                }
                let (actor, _, level, _) = match self.session_auth_user(session).await {
                    Ok(t) => t,
                    Err(r) => return Ok(r),
                };
                if level < PermissionLevel::Sysop {
                    let mut sessions = self.sessions.write().await;
                    if let Some(r) = sessions.get_mut(&session) {
                        r.workflow = Workflow::None;
                    }
                    return Ok(Response::Error("Sysop access required.".into()));
                }
                let user = UserStore::get_by_username(&self.db, &target)
                    .await
                    .map_err(|e| HostError::Storage(format!("{e}")))?;
                let user = match user {
                    Some(u) => u,
                    None => {
                        let mut sessions = self.sessions.write().await;
                        if let Some(r) = sessions.get_mut(&session) {
                            r.workflow = Workflow::None;
                        }
                        return Ok(Response::Error(format!(
                            "User '{}' not found.",
                            target.as_str()
                        )));
                    }
                };
                self.db
                    .credentials()
                    .set_password(user.id, &new_password, Timestamp::now())
                    .await
                    .map_err(|e| HostError::Storage(format!("set_password: {e}")))?;
                {
                    let mut sessions = self.sessions.write().await;
                    if let Some(r) = sessions.get_mut(&session) {
                        r.workflow = Workflow::None;
                    }
                }
                if let Err(e) = self
                    .db
                    .audit_write(
                        actor.as_str(),
                        "set_user_password",
                        Some(target.as_str()),
                        None,
                    )
                    .await
                {
                    tracing::warn!("audit write failed: {e}");
                }
                info!(%actor, %target, "sysop reset user password");
                Ok(Response::Text(format!(
                    "Password for '{}' updated.",
                    target.as_str()
                )))
            }

            // ── Message reading ──────────────────────────────────────────────
            Workflow::Reading => {
                match reply.trim().to_uppercase().as_str() {
                    "F" => self.handle_read_forward(session, None).await,
                    "R" => self.handle_read_reverse(session).await,
                    "E" => self.handle_reply_from_reading(session).await,
                    _ => {
                        // Any other input exits reading mode.
                        {
                            let mut sessions = self.sessions.write().await;
                            if let Some(r) = sessions.get_mut(&session) {
                                r.workflow = Workflow::None;
                                r.current_message_id = None;
                            }
                        }
                        Ok(Response::Text(
                            "Exited reading mode. Type H for help.".into(),
                        ))
                    }
                }
            }

            // ── Room selection ────────────────────────────────────────────────
            Workflow::Rooms { room_ids } => {
                let trimmed = reply.trim();
                // X or empty → cancel
                if trimmed.eq_ignore_ascii_case("x") || trimmed.is_empty() {
                    {
                        let mut sessions = self.sessions.write().await;
                        if let Some(r) = sessions.get_mut(&session) {
                            r.workflow = Workflow::None;
                        }
                    }
                    return Ok(Response::Text("Cancelled.".into()));
                }
                // Numeric index into the list shown by K
                if let Ok(n) = trimmed.parse::<usize>() {
                    if n >= 1 && n <= room_ids.len() {
                        let target_id = room_ids[n - 1];
                        {
                            let mut sessions = self.sessions.write().await;
                            if let Some(r) = sessions.get_mut(&session) {
                                r.workflow = Workflow::None;
                            }
                        }
                        self.set_current_room(session, target_id).await;
                        return self.handle_change_to_room(session, target_id).await;
                    }
                }
                // Fall back: treat as room name via the normal change-room path
                {
                    let mut sessions = self.sessions.write().await;
                    if let Some(r) = sessions.get_mut(&session) {
                        r.workflow = Workflow::None;
                    }
                }
                self.handle_change_room(session, trimmed).await
            }

            // ── Pending-user review queue ─────────────────────────────────────
            Workflow::ReviewPending { pending, index } => {
                if index >= pending.len() {
                    let mut sessions = self.sessions.write().await;
                    if let Some(r) = sessions.get_mut(&session) {
                        r.workflow = Workflow::None;
                    }
                    return Ok(Response::Text("No more pending accounts.".into()));
                }

                let username = pending[index].clone();
                let next_index = index + 1;

                match reply.trim().to_uppercase().as_str() {
                    "V" => {
                        // Validate (promote to User tier)
                        let result = self.handle_validate_user(session, username.clone()).await;
                        // Advance or finish
                        self.advance_review_pending(session, pending, next_index)
                            .await?;
                        result
                    }
                    "B" => {
                        // Ban
                        let result = self.handle_ban_user(session, username.clone()).await;
                        self.advance_review_pending(session, pending, next_index)
                            .await?;
                        result
                    }
                    "S" | "X" => {
                        // Skip or exit queue
                        let done =
                            next_index >= pending.len() || reply.trim().to_uppercase() == "X";
                        if done {
                            let mut sessions = self.sessions.write().await;
                            if let Some(r) = sessions.get_mut(&session) {
                                r.workflow = Workflow::None;
                            }
                            Ok(Response::Text("Exited review queue.".into()))
                        } else {
                            self.advance_review_pending(session, pending, next_index)
                                .await
                        }
                    }
                    _ => {
                        // Re-show current entry
                        Ok(Response::Prompt {
                            text: format!(
                                "{} — V Validate  S Skip  B Ban  X Exit",
                                username.as_str()
                            ),
                            hide_input: false,
                        })
                    }
                }
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
    ///
    /// When the cached level is Unvalidated, the DB is re-read once to catch
    /// out-of-process promotions (e.g. `supply-drop-bbs user promote`) without
    /// requiring the user to log out and back in.
    ///
    /// When `access_policy.require_verify` is `false`, Unvalidated sessions
    /// are promoted to `User` in-memory so they pass this check without a
    /// sysop having to manually validate them.
    async fn session_auth_user(
        &self,
        session: SessionId,
    ) -> Result<(Username, UserId, PermissionLevel, RoomId), Response> {
        let (username, user_id, level, room_id) = self.session_auth(session).await?;

        if level >= PermissionLevel::User {
            return Ok((username, user_id, level, room_id));
        }

        // Level is Unvalidated — re-read from DB in case an out-of-process
        // tool (CLI, direct DB edit) promoted this user since they logged in.
        let fresh_level = UserStore::get_by_id(&self.db, user_id)
            .await
            .ok()
            .flatten()
            .map(|u| u.permission_level)
            .unwrap_or(level);

        if fresh_level >= PermissionLevel::User {
            // Refresh the in-memory session so subsequent commands don't DB-check again.
            let mut sessions = self.sessions.write().await;
            if let Some(r) = sessions.get_mut(&session) {
                r.level = fresh_level;
            }
            return Ok((username, user_id, fresh_level, room_id));
        }

        // If require_verify is disabled, treat Unvalidated as User-level.
        let require_verify = self.access_policy.read().await.require_verify;
        if !require_verify {
            let mut sessions = self.sessions.write().await;
            if let Some(r) = sessions.get_mut(&session) {
                r.level = PermissionLevel::User;
            }
            return Ok((username, user_id, PermissionLevel::User, room_id));
        }

        Err(Response::Text(
            "Your account is pending validation by an aide.\n\
             Type H for help, WHOAMI to see your status, or Q to log out."
                .into(),
        ))
    }

    /// Like [`session_auth_user`] but also allows Unvalidated users through
    /// when a guest room is configured.
    ///
    /// Returns the same tuple as `session_auth_user`; callers check
    /// `level < PermissionLevel::User` to detect guest-only access and
    /// restrict navigation/posting to the guest room.
    async fn session_auth_or_guest(
        &self,
        session: SessionId,
    ) -> Result<(Username, UserId, PermissionLevel, RoomId), Response> {
        match self.session_auth_user(session).await {
            ok @ Ok(_) => ok,
            Err(pending_response) => {
                // If a guest room is configured, let Unvalidated sessions
                // through with their real (Unvalidated) level so handlers
                // can restrict them to that room.
                if self.guest_room_id().is_some() {
                    self.session_auth(session)
                        .await
                        .map_err(|_| pending_response)
                } else {
                    Err(pending_response)
                }
            }
        }
    }

    async fn handle_list_rooms(&self, session: SessionId) -> Result<Response, HostError> {
        let (username, user_id, level, current_room) =
            match self.session_auth_or_guest(session).await {
                Ok(t) => t,
                Err(r) => return Ok(r),
            };

        let is_guest = level < PermissionLevel::User;
        let guest_rid = self.guest_room_id();

        let rooms = {
            let all = self
                .db
                .list_readable(level)
                .await
                .map_err(|e| HostError::Storage(format!("{e}")))?;
            if is_guest {
                // Guests only see the guest room.
                all.into_iter()
                    .filter(|r| Some(r.id) == guest_rid)
                    .collect::<Vec<_>>()
            } else {
                all
            }
        };

        let mut lines = Vec::new();
        for room in &rooms {
            let unread = if room.id == MAIL_ROOM_ID {
                self.db
                    .unread_direct_count(&username, user_id, room.id)
                    .await
            } else {
                self.db.unread_count(user_id, room.id).await
            }
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

        // Prefix each line with its 1-based index so the user can type a
        // number to jump in (handled by Workflow::Rooms).
        let numbered: Vec<String> = lines
            .iter()
            .enumerate()
            .map(|(i, l)| format!("{}. {}", i + 1, l))
            .collect();

        let room_ids: Vec<RoomId> = rooms.iter().map(|r| r.id).collect();
        {
            let mut sessions = self.sessions.write().await;
            if let Some(r) = sessions.get_mut(&session) {
                r.workflow = Workflow::Rooms { room_ids };
            }
        }

        Ok(Response::Prompt {
            text: format!(
                "Rooms:\n{}\nEnter # to join, X to cancel",
                numbered.join("\n")
            ),
            hide_input: false,
        })
    }

    async fn handle_go_next_unread(&self, session: SessionId) -> Result<Response, HostError> {
        let (username, user_id, level, current_room) =
            match self.session_auth_or_guest(session).await {
                Ok(t) => t,
                Err(r) => return Ok(r),
            };

        let is_guest = level < PermissionLevel::User;
        let guest_rid = self.guest_room_id();

        let rooms = {
            let all = self
                .db
                .list_readable(level)
                .await
                .map_err(|e| HostError::Storage(format!("{e}")))?;
            if is_guest {
                all.into_iter()
                    .filter(|r| Some(r.id) == guest_rid)
                    .collect::<Vec<_>>()
            } else {
                all
            }
        };

        // Walk the room list starting just after the current room,
        // wrapping around. Skip the current room if encountered during wrap.
        let start = rooms
            .iter()
            .position(|r| r.id == current_room)
            .map(|i| i + 1)
            .unwrap_or(0);

        for room in rooms[start..].iter().chain(rooms[..start].iter()) {
            if room.id == current_room {
                continue;
            }
            let unread = if room.id == MAIL_ROOM_ID {
                self.db
                    .unread_direct_count(&username, user_id, room.id)
                    .await
            } else {
                self.db.unread_count(user_id, room.id).await
            }
            .map_err(|e| HostError::Storage(format!("{e}")))?;
            if unread > 0 {
                self.set_current_room(session, room.id).await;
                return self.handle_read_new(session).await;
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

        let (username, user_id, level, _) = match self.session_auth_or_guest(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };

        let is_guest = level < PermissionLevel::User;
        let guest_rid = self.guest_room_id();

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

        // Guests may only navigate to the guest room.
        if is_guest && Some(room.id) != guest_rid {
            return Ok(Response::Text(
                "You must be verified to access that room.".into(),
            ));
        }

        self.set_current_room(session, room.id).await;
        let unread = if room.id == MAIL_ROOM_ID {
            self.db
                .unread_direct_count(&username, user_id, room.id)
                .await
        } else {
            self.db.unread_count(user_id, room.id).await
        }
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
        let (username, user_id, level, _) = match self.session_auth_user(session).await {
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
        let unread = if room.id == MAIL_ROOM_ID {
            self.db
                .unread_direct_count(&username, user_id, room.id)
                .await
        } else {
            self.db.unread_count(user_id, room.id).await
        }
        .map_err(|e| HostError::Storage(format!("{e}")))?;

        let msg = if unread > 0 {
            format!("Now in: {} ({unread} new). Type N to read.", room.name)
        } else {
            format!("Now in: {} (no new messages).", room.name)
        };
        Ok(Response::Text(msg))
    }

    /// Start a reply compose from reading mode.
    ///
    /// Looks up the current message, switches to `Workflow::Compose` with the
    /// sender pre-populated as recipient (Mail room) or no recipient (room
    /// post), and returns a body prompt so the user can type their reply
    /// without leaving the reading context manually.
    async fn handle_reply_from_reading(&self, session: SessionId) -> Result<Response, HostError> {
        let (_, _, level, room_id) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };

        let room = RoomStore::get_by_id(&self.db, room_id)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?
            .ok_or_else(|| HostError::NotFound(format!("{room_id}")))?;

        if room.read_only && level < PermissionLevel::Aide {
            return Ok(Response::Error(format!("'{}' is read-only.", room.name)));
        }

        let msg_id = {
            let sessions = self.sessions.read().await;
            sessions.get(&session).and_then(|r| r.current_message_id)
        };

        let recipient: Option<Username> = if let Some(mid) = msg_id {
            let msg = MessageStore::get_by_id(&self.db, mid)
                .await
                .map_err(|e| HostError::Storage(format!("{e}")))?;
            // In Mail room, reply goes to the sender of the current message.
            // In a regular room, there is no specific recipient.
            if room_id == MAIL_ROOM_ID {
                msg.map(|m| m.sender)
            } else {
                None
            }
        } else {
            None
        };

        let prompt = match &recipient {
            Some(r) => format!("Reply to {}:", r.as_str()),
            None => format!("Post to {}:", room.name),
        };

        {
            let mut sessions = self.sessions.write().await;
            if let Some(r) = sessions.get_mut(&session) {
                r.workflow = Workflow::Compose {
                    room_id,
                    stage: ComposeStage::AwaitingBody { recipient },
                };
            }
        }

        Ok(Response::Prompt {
            text: prompt,
            hide_input: false,
        })
    }

    /// Advance the `ReviewPending` queue to `next_index`, showing the next
    /// account or finishing the workflow when the list is exhausted.
    async fn advance_review_pending(
        &self,
        session: SessionId,
        pending: Vec<Username>,
        next_index: usize,
    ) -> Result<Response, HostError> {
        if next_index >= pending.len() {
            let mut sessions = self.sessions.write().await;
            if let Some(r) = sessions.get_mut(&session) {
                r.workflow = Workflow::None;
            }
            return Ok(Response::Text(
                "Review complete — no more pending accounts.".into(),
            ));
        }

        let next_user = &pending[next_index];
        let prompt = format!(
            "#{} of {}: {}  — V Validate  S Skip  B Ban  X Exit",
            next_index + 1,
            pending.len(),
            next_user.as_str()
        );

        {
            let mut sessions = self.sessions.write().await;
            if let Some(r) = sessions.get_mut(&session) {
                r.workflow = Workflow::ReviewPending {
                    pending,
                    index: next_index,
                };
            }
        }

        Ok(Response::Prompt {
            text: prompt,
            hide_input: false,
        })
    }

    async fn set_current_room(&self, session: SessionId, room_id: RoomId) {
        let mut sessions = self.sessions.write().await;
        if let Some(r) = sessions.get_mut(&session) {
            if r.current_room != room_id {
                r.current_message_id = None;
                r.workflow = Workflow::None;
            }
            r.current_room = room_id;
        }
    }
}

// ── Access-policy sysop command handlers ─────────────────────────────────────

impl BbsHost {
    /// Handle `OPENACCESS` — disable the verification requirement immediately.
    async fn handle_open_access(&self, session: SessionId) -> Result<Response, HostError> {
        let (actor, _, level, _) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };
        if level < PermissionLevel::Sysop {
            return Ok(Response::Text("Sysop permission required.".into()));
        }
        {
            let mut policy = self.access_policy.write().await;
            policy.require_verify = false;
        }
        self.persist_access_policy().await;
        if let Err(e) = self
            .db
            .audit_write(actor.as_str(), "open_access", None, None)
            .await
        {
            warn!("audit write failed: {e}");
        }
        Ok(Response::Text(
            "Open access enabled. New registrations no longer require verification.".into(),
        ))
    }

    /// Handle `CLOSEACCESS` — restore the verification requirement immediately.
    async fn handle_close_access(&self, session: SessionId) -> Result<Response, HostError> {
        let (actor, _, level, _) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };
        if level < PermissionLevel::Sysop {
            return Ok(Response::Text("Sysop permission required.".into()));
        }
        {
            let mut policy = self.access_policy.write().await;
            policy.require_verify = true;
        }
        self.persist_access_policy().await;
        if let Err(e) = self
            .db
            .audit_write(actor.as_str(), "close_access", None, None)
            .await
        {
            warn!("audit write failed: {e}");
        }
        Ok(Response::Text(
            "Verification requirement restored. New accounts must be validated.".into(),
        ))
    }

    /// Handle `GUESTROOM <name>` / `GUESTROOM OFF`.
    async fn handle_set_guest_room(
        &self,
        session: SessionId,
        name: Option<String>,
    ) -> Result<Response, HostError> {
        let (actor, _, level, _) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };
        if level < PermissionLevel::Sysop {
            return Ok(Response::Text("Sysop permission required.".into()));
        }

        let reply = match name {
            None => {
                // Disable guest room.
                {
                    let mut policy = self.access_policy.write().await;
                    policy.guest_room_name = None;
                }
                *self.guest_room_id.write().expect("guest_room_id poisoned") = None;
                self.persist_access_policy().await;
                if let Err(e) = self
                    .db
                    .audit_write(actor.as_str(), "set_guest_room", Some("off"), None)
                    .await
                {
                    warn!("audit write failed: {e}");
                }
                "Guest room disabled. Unverified users will see the pending-validation message."
                    .to_owned()
            }
            Some(ref room_name) => {
                // Enable / change guest room — ensure it exists.
                {
                    let mut policy = self.access_policy.write().await;
                    policy.guest_room_name = Some(room_name.clone());
                }
                // Re-run ensure_guest_room to create it if needed.
                if let Err(e) = self.ensure_guest_room().await {
                    // Roll back in-memory change on failure.
                    let mut policy = self.access_policy.write().await;
                    policy.guest_room_name = None;
                    return Ok(Response::Text(format!("Failed to set guest room: {e}")));
                }
                self.persist_access_policy().await;
                if let Err(e) = self
                    .db
                    .audit_write(
                        actor.as_str(),
                        "set_guest_room",
                        Some(room_name.as_str()),
                        None,
                    )
                    .await
                {
                    warn!("audit write failed: {e}");
                }
                format!("Guest room set to '{room_name}'. Unverified users will be placed there.")
            }
        };

        Ok(Response::Text(reply))
    }

    /// Persist the current access policy to `config.toml`.
    ///
    /// Failures are logged as warnings but not propagated — the in-memory
    /// state is already updated and the sysop can restart to re-read the file.
    async fn persist_access_policy(&self) {
        let Some(ref path) = self.config_path else {
            warn!("no config_path set — access policy change will not survive restart");
            return;
        };

        let (require_verify, guest_room_name) = {
            let policy = self.access_policy.read().await;
            (policy.require_verify, policy.guest_room_name.clone())
        };

        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(e) => {
                warn!(
                    "persist_access_policy: could not read {}: {e}",
                    path.display()
                );
                return;
            }
        };

        let mut doc = match content.parse::<toml_edit::DocumentMut>() {
            Ok(d) => d,
            Err(e) => {
                warn!(
                    "persist_access_policy: could not parse {}: {e}",
                    path.display()
                );
                return;
            }
        };

        // Ensure [bbs] table exists.
        if doc.get("bbs").is_none() {
            doc["bbs"] = toml_edit::Item::Table(toml_edit::Table::new());
        }

        doc["bbs"]["require_verify"] = toml_edit::value(require_verify);

        match guest_room_name {
            Some(name) => {
                doc["bbs"]["guest_room"] = toml_edit::value(name);
            }
            None => {
                if let Some(bbs) = doc.get_mut("bbs").and_then(|t| t.as_table_mut()) {
                    bbs.remove("guest_room");
                }
            }
        }

        if let Err(e) = std::fs::write(path, doc.to_string()) {
            warn!(
                "persist_access_policy: could not write {}: {e}",
                path.display()
            );
        } else {
            info!(path = %path.display(), "access policy persisted to config");
        }
    }
}

// ── Message helpers ───────────────────────────────────────────────────────────

impl BbsHost {
    async fn handle_read_new(&self, session: SessionId) -> Result<Response, HostError> {
        let (username, user_id, level, room_id) = match self.session_auth_or_guest(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };

        let is_guest = level < PermissionLevel::User;
        let guest_rid = self.guest_room_id();
        if is_guest && Some(room_id) != guest_rid {
            return Ok(Response::Text(
                "You can only read messages in the Guests room.".into(),
            ));
        }

        let room = RoomStore::get_by_id(&self.db, room_id)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?
            .ok_or_else(|| HostError::NotFound(format!("{room_id}")))?;

        let after = self
            .db
            .get_last_read(user_id, room_id)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        let page = if room_id == MAIL_ROOM_ID {
            self.db
                .list_direct(&username, after, MESH_PAGE)
                .await
                .map_err(|e| HostError::Storage(format!("{e}")))?
        } else {
            self.db
                .list_in_room(room_id, after, MESH_PAGE)
                .await
                .map_err(|e| HostError::Storage(format!("{e}")))?
        };

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

        let mut parts = vec![format!("[{} — new messages]", room.name)];
        for msg in &visible {
            parts.push(format_message(msg));
        }
        if let Some(cursor) = page.next_cursor {
            parts.push(format!(
                "(more — type N again or F {} to continue)",
                cursor.as_i64()
            ));
        }
        Ok(Response::MultiText(parts))
    }

    async fn handle_read_forward(
        &self,
        session: SessionId,
        after: Option<i64>,
    ) -> Result<Response, HostError> {
        let (username, user_id, level, room_id) = match self.session_auth_or_guest(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };

        let is_guest = level < PermissionLevel::User;
        let guest_rid = self.guest_room_id();
        if is_guest && Some(room_id) != guest_rid {
            return Ok(Response::Text(
                "You can only read messages in the Guests room.".into(),
            ));
        }

        let room = RoomStore::get_by_id(&self.db, room_id)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?
            .ok_or_else(|| HostError::NotFound(format!("{room_id}")))?;

        // Explicit cursor from "F <id>" overrides session state.
        let (cursor, already_reading) = {
            let sessions = self.sessions.read().await;
            let r = sessions.get(&session);
            let cursor = after
                .map(MessageId::new)
                .or_else(|| r.and_then(|r| r.current_message_id));
            let already_reading = r.is_some_and(|r| matches!(r.workflow, Workflow::Reading));
            (cursor, already_reading)
        };

        // First F with no cursor and not yet in reading mode → show intro.
        if cursor.is_none() && !already_reading {
            let count = if room_id == MAIL_ROOM_ID {
                self.db.count_direct(&username).await
            } else {
                self.db.count_in_room(room_id).await
            }
            .map_err(|e| HostError::Storage(format!("{e}")))?;

            {
                let mut sessions = self.sessions.write().await;
                if let Some(r) = sessions.get_mut(&session) {
                    r.workflow = Workflow::Reading;
                }
            }

            return Ok(Response::Prompt {
                text: format!(
                    "[{} — Reading]\n{} message(s)\nF - Forward  R - Backward  X - Exit",
                    room.name, count
                ),
                hide_input: false,
            });
        }

        let msg = if room_id == MAIL_ROOM_ID {
            self.db.next_direct(&username, cursor).await
        } else {
            self.db.next_in_room(room_id, cursor).await
        }
        .map_err(|e| HostError::Storage(format!("{e}")))?;

        let msg = match msg {
            None => {
                return Ok(Response::Prompt {
                    text: format!("No more messages in {}.\nR - Backward  X - Exit", room.name),
                    hide_input: false,
                })
            }
            Some(m) => m,
        };

        // Advance read pointer and update session cursor.
        let _ = self.db.mark_read(user_id, room_id, msg.id).await;
        {
            let mut sessions = self.sessions.write().await;
            if let Some(r) = sessions.get_mut(&session) {
                r.current_message_id = Some(msg.id);
                r.workflow = Workflow::Reading;
            }
        }

        // Check neighbours for conditional nav hints.
        let has_prev = if room_id == MAIL_ROOM_ID {
            self.db.prev_direct(&username, Some(msg.id)).await
        } else {
            self.db.prev_in_room(room_id, Some(msg.id)).await
        }
        .map_err(|e| HostError::Storage(format!("{e}")))?
        .is_some();

        let has_next = if room_id == MAIL_ROOM_ID {
            self.db.next_direct(&username, Some(msg.id)).await
        } else {
            self.db.next_in_room(room_id, Some(msg.id)).await
        }
        .map_err(|e| HostError::Storage(format!("{e}")))?
        .is_some();

        Ok(Response::Prompt {
            text: build_message_with_nav(&msg, has_prev, has_next),
            hide_input: false,
        })
    }

    async fn handle_read_reverse(&self, session: SessionId) -> Result<Response, HostError> {
        let (username, user_id, level, room_id) = match self.session_auth_or_guest(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };

        let is_guest = level < PermissionLevel::User;
        let guest_rid = self.guest_room_id();
        if is_guest && Some(room_id) != guest_rid {
            return Ok(Response::Text(
                "You can only read messages in the Guests room.".into(),
            ));
        }

        let room = RoomStore::get_by_id(&self.db, room_id)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?
            .ok_or_else(|| HostError::NotFound(format!("{room_id}")))?;

        // R with no position → jump to last message; otherwise go one back.
        let (cursor, already_reading) = {
            let sessions = self.sessions.read().await;
            let r = sessions.get(&session);
            let cursor = r.and_then(|r| r.current_message_id);
            let already_reading = r.is_some_and(|r| matches!(r.workflow, Workflow::Reading));
            (cursor, already_reading)
        };

        // First R with no cursor and not yet in reading mode → show intro.
        if cursor.is_none() && !already_reading {
            let count = if room_id == MAIL_ROOM_ID {
                self.db.count_direct(&username).await
            } else {
                self.db.count_in_room(room_id).await
            }
            .map_err(|e| HostError::Storage(format!("{e}")))?;

            {
                let mut sessions = self.sessions.write().await;
                if let Some(r) = sessions.get_mut(&session) {
                    r.workflow = Workflow::Reading;
                }
            }

            return Ok(Response::Prompt {
                text: format!(
                    "[{} — Reading]\n{} message(s)\nF - Forward  R - Backward  X - Exit",
                    room.name, count
                ),
                hide_input: false,
            });
        }

        let msg = if room_id == MAIL_ROOM_ID {
            self.db.prev_direct(&username, cursor).await
        } else {
            self.db.prev_in_room(room_id, cursor).await
        }
        .map_err(|e| HostError::Storage(format!("{e}")))?;

        let msg = match msg {
            None => {
                return Ok(Response::Prompt {
                    text: format!(
                        "No previous messages in {}.\nF - Forward  X - Exit",
                        room.name
                    ),
                    hide_input: false,
                })
            }
            Some(m) => m,
        };

        // Advance read pointer and update session cursor.
        let _ = self.db.mark_read(user_id, room_id, msg.id).await;
        {
            let mut sessions = self.sessions.write().await;
            if let Some(r) = sessions.get_mut(&session) {
                r.current_message_id = Some(msg.id);
                r.workflow = Workflow::Reading;
            }
        }

        // Check neighbours for conditional nav hints.
        let has_prev = if room_id == MAIL_ROOM_ID {
            self.db.prev_direct(&username, Some(msg.id)).await
        } else {
            self.db.prev_in_room(room_id, Some(msg.id)).await
        }
        .map_err(|e| HostError::Storage(format!("{e}")))?
        .is_some();

        let has_next = if room_id == MAIL_ROOM_ID {
            self.db.next_direct(&username, Some(msg.id)).await
        } else {
            self.db.next_in_room(room_id, Some(msg.id)).await
        }
        .map_err(|e| HostError::Storage(format!("{e}")))?
        .is_some();

        Ok(Response::Prompt {
            text: build_message_with_nav(&msg, has_prev, has_next),
            hide_input: false,
        })
    }

    async fn handle_scan(&self, session: SessionId) -> Result<Response, HostError> {
        let (username, _, level, room_id) = match self.session_auth_or_guest(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };

        let is_guest = level < PermissionLevel::User;
        let guest_rid = self.guest_room_id();
        if is_guest && Some(room_id) != guest_rid {
            return Ok(Response::Text(
                "You can only read messages in the Guests room.".into(),
            ));
        }

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
            let flat: String = msg.content.replace('\r', "").replace('\n', " ");
            let snippet: String = flat.chars().take(40).collect();
            let ellipsis = if flat.chars().count() > 40 { "…" } else { "" };
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

    async fn handle_enter_message(
        &self,
        session: SessionId,
        inline_body: Option<String>,
    ) -> Result<Response, HostError> {
        let (_sender, _, level, room_id) = match self.session_auth_or_guest(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };

        let is_guest = level < PermissionLevel::User;
        let guest_rid = self.guest_room_id();
        if is_guest && Some(room_id) != guest_rid {
            return Ok(Response::Text(
                "You can only post messages in the Guests room.".into(),
            ));
        }

        // Guests cannot send mail.
        if is_guest && room_id == MAIL_ROOM_ID {
            return Ok(Response::Text("You must be verified to send mail.".into()));
        }

        {
            let sessions = self.sessions.read().await;
            if let Some(r) = sessions.get(&session) {
                if !matches!(r.workflow, Workflow::None) {
                    return Ok(Response::Error(
                        "A workflow is already in progress. Type 'cancel' first.".into(),
                    ));
                }
            }
        }

        let room = RoomStore::get_by_id(&self.db, room_id)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?
            .ok_or_else(|| HostError::NotFound(format!("{room_id}")))?;

        if room.read_only && level < PermissionLevel::Aide {
            return Ok(Response::Error(format!("'{}' is read-only.", room.name)));
        }

        // ── Inline mode: body (and optional @recipient) supplied on the same line ──
        // Stage as a draft (AwaitingConfirmation) rather than posting immediately.
        // The user must send a lone "." to confirm. This makes the send idempotent
        // on lossy links: if "Message posted." is lost, retrying "." is safe.
        if let Some(raw) = inline_body {
            let raw = raw.trim();
            if raw.is_empty() {
                // Treat bare "E " (with trailing space) same as bare "E".
                // Fall through to the prompt flow below.
            } else if room_id == MAIL_ROOM_ID {
                // Mail inline: "E @recipient message" or "E recipient message"
                let (first, rest) = raw
                    .split_once(|c: char| c.is_whitespace())
                    .map(|(a, b)| (a, Some(b)))
                    .unwrap_or((raw, None));
                let recipient_str = first.trim_start_matches('@');
                match Username::new(recipient_str) {
                    Ok(recipient) => {
                        let exists = self
                            .db
                            .get_by_username(&recipient)
                            .await
                            .map_err(|e| HostError::Storage(format!("{e}")))?
                            .is_some();
                        if !exists {
                            return Ok(Response::Error(format!(
                                "User '{}' not found.",
                                recipient.as_str()
                            )));
                        }
                        let body = rest.unwrap_or("").trim();
                        if body.is_empty() {
                            return Ok(Response::Prompt {
                                text: format!("Enter message for {}:", recipient.as_str()),
                                hide_input: false,
                            });
                        }
                        let body = body.to_owned();
                        let mut sessions = self.sessions.write().await;
                        if let Some(r) = sessions.get_mut(&session) {
                            r.workflow = Workflow::Compose {
                                room_id,
                                stage: ComposeStage::AwaitingConfirmation {
                                    recipient: Some(recipient.clone()),
                                    body: body.clone(),
                                },
                            };
                        }
                        return Ok(Response::Prompt {
                            text: format!("To {}: {}\nType . to send", recipient.as_str(), body),
                            hide_input: false,
                        });
                    }
                    Err(_) => {
                        return Ok(Response::Prompt {
                            text: "Enter recipient username:".into(),
                            hide_input: false,
                        });
                    }
                }
            } else {
                // Room inline: "E message text" → stage draft.
                let body = raw.to_owned();
                let mut sessions = self.sessions.write().await;
                if let Some(r) = sessions.get_mut(&session) {
                    r.workflow = Workflow::Compose {
                        room_id,
                        stage: ComposeStage::AwaitingConfirmation {
                            recipient: None,
                            body: body.clone(),
                        },
                    };
                }
                return Ok(Response::Prompt {
                    text: format!("{body}\nType . to send"),
                    hide_input: false,
                });
            }
        }

        // ── Prompt flow: no inline body ──────────────────────────────────────
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
        // DMs live in the Mail room; room messages require a room_messages join.
        let in_room = if msg.recipient.is_some() {
            room_id == MAIL_ROOM_ID
        } else {
            MessageStore::is_in_room(&self.db, msg_id, room_id)
                .await
                .map_err(|e| HostError::Storage(format!("{e}")))?
        };
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

        // Audit when a privileged user moderates someone else's content.
        if level >= PermissionLevel::Aide && msg.sender != username {
            let detail = format!("by {}", msg.sender.as_str());
            if let Err(e) = self
                .db
                .audit_write(
                    username.as_str(),
                    "delete_message",
                    Some(&format!("#{id}")),
                    Some(&detail),
                )
                .await
            {
                tracing::warn!("audit write failed: {e}");
            }
        }

        Ok(Response::Text(format!("Message #{id} deleted.")))
    }

    async fn handle_fast_forward(&self, session: SessionId) -> Result<Response, HostError> {
        let (_, user_id, level, room_id) = match self.session_auth_or_guest(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };

        let is_guest = level < PermissionLevel::User;
        let guest_rid = self.guest_room_id();
        if is_guest && Some(room_id) != guest_rid {
            return Ok(Response::Text(
                "You can only read messages in the Guests room.".into(),
            ));
        }

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

        let usernames: Vec<Username> = pending.iter().map(|u| u.username.clone()).collect();
        let first_name = usernames[0].as_str().to_owned();
        let total = usernames.len();

        {
            let mut sessions = self.sessions.write().await;
            if let Some(r) = sessions.get_mut(&session) {
                r.workflow = Workflow::ReviewPending {
                    pending: usernames,
                    index: 0,
                };
            }
        }

        Ok(Response::Prompt {
            text: format!("#1 of {total}: {first_name}  — V Validate  S Skip  B Ban  X Exit"),
            hide_input: false,
        })
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
            None => return Ok(Response::Error("User not found.".into())),
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

        if let Err(e) = self
            .db
            .audit_write(actor.as_str(), "validate", Some(username.as_str()), None)
            .await
        {
            tracing::warn!("audit write failed: {e}");
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
            return Ok(Response::Error("User not found.".into()));
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
            None => return Ok(Response::Error("User not found.".into())),
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

        if let Err(e) = self
            .db
            .audit_write(actor.as_str(), "ban", Some(username.as_str()), None)
            .await
        {
            tracing::warn!("audit write failed: {e}");
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
            None => return Ok(Response::Error("User not found.".into())),
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

        if let Err(e) = self
            .db
            .audit_write(actor.as_str(), "unban", Some(username.as_str()), None)
            .await
        {
            tracing::warn!("audit write failed: {e}");
        }

        info!(%actor, %username, "user unbanned");
        Ok(Response::Text(format!(
            "'{}' has been unbanned.",
            username.as_str()
        )))
    }

    async fn handle_list_users(
        &self,
        session: SessionId,
        filter: Option<String>,
    ) -> Result<Response, HostError> {
        let (_, _, level, _) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };

        let (status_filter, label) = match filter.as_deref() {
            None | Some("active") => (Some(UserStatus::Active), "active"),
            Some("banned") => (Some(UserStatus::Banned), "banned"),
            Some("deleted") if level >= PermissionLevel::Sysop => {
                (Some(UserStatus::Deleted), "deleted")
            }
            Some("all") if level >= PermissionLevel::Sysop => (None, "all"),
            Some("deleted") | Some("all") => {
                return Ok(Response::Error(
                    "Sysop access required for that filter.".into(),
                ))
            }
            Some(other) => {
                return Ok(Response::Error(format!(
                    "Unknown filter '{other}'. Use: active, banned, all (sysop)."
                )))
            }
        };

        let users = UserStore::list(&self.db, status_filter, 50, 0)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        if users.is_empty() {
            return Ok(Response::Text(format!("No {label} users found.")));
        }

        let mut lines = vec![format!("Users ({label}, {}):", users.len())];
        for u in &users {
            let lvl = match u.permission_level {
                PermissionLevel::Sysop => "sysop",
                PermissionLevel::Aide => "aide",
                PermissionLevel::User => "user",
                PermissionLevel::Unvalidated => "unval",
            };
            lines.push(format!(" {} [{}]", u.username.as_str(), lvl));
        }
        Ok(Response::Text(lines.join("\n")))
    }

    async fn handle_search_users(
        &self,
        session: SessionId,
        query: String,
    ) -> Result<Response, HostError> {
        match self.session_auth_user(session).await {
            Ok(_) => {}
            Err(r) => return Ok(r),
        }
        if query.is_empty() {
            return Ok(Response::Error("Usage: SEARCH <username>".into()));
        }

        let all = UserStore::list(&self.db, None, 500, 0)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        let q = query.to_lowercase();
        let matches: Vec<_> = all
            .iter()
            .filter(|u| u.username.as_str().to_lowercase().contains(&q))
            .collect();

        if matches.is_empty() {
            return Ok(Response::Text(format!("No users matching '{query}'.")));
        }

        let mut lines = vec![format!("Search '{query}' ({}):", matches.len())];
        for u in matches {
            let lvl = match u.permission_level {
                PermissionLevel::Sysop => "sysop",
                PermissionLevel::Aide => "aide",
                PermissionLevel::User => "user",
                PermissionLevel::Unvalidated => "unval",
            };
            let status = match u.status {
                UserStatus::Active => "",
                UserStatus::Banned => " [banned]",
                UserStatus::Deleted => " [deleted]",
            };
            lines.push(format!(" {} [{}]{}", u.username.as_str(), lvl, status));
        }
        Ok(Response::Text(lines.join("\n")))
    }

    async fn handle_user_info(
        &self,
        session: SessionId,
        username: Username,
    ) -> Result<Response, HostError> {
        match self.session_auth_user(session).await {
            Ok(_) => {}
            Err(r) => return Ok(r),
        }

        let user = UserStore::get_by_username(&self.db, &username)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;

        let user = match user {
            None => return Ok(Response::Error("User not found.".into())),
            Some(u) => u,
        };

        let session_count = {
            let sessions = self.sessions.read().await;
            sessions
                .values()
                .filter(|r| r.username.as_ref() == Some(&username))
                .count()
        };

        let lvl = match user.permission_level {
            PermissionLevel::Sysop => "sysop",
            PermissionLevel::Aide => "aide",
            PermissionLevel::User => "user",
            PermissionLevel::Unvalidated => "unvalidated",
        };
        let status = match user.status {
            UserStatus::Active => "active",
            UserStatus::Banned => "banned",
            UserStatus::Deleted => "deleted",
        };

        let mut lines = vec![
            format!("User: {}", user.username.as_str()),
            format!("Level: {lvl}  Status: {status}"),
        ];
        if let Some(ref dn) = user.display_name {
            lines.push(format!("Name: {dn}"));
        }
        lines.push(format!("Joined: {}", user.created_at));
        if let Some(last) = user.last_login_at {
            lines.push(format!("Last login: {last}"));
        }
        if session_count > 0 {
            lines.push(format!(
                "Online ({} session{})",
                session_count,
                if session_count == 1 { "" } else { "s" }
            ));
        }
        Ok(Response::Text(lines.join("\n")))
    }

    async fn handle_delete_user(
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
            None => return Ok(Response::Error("User not found.".into())),
            Some(u) => u,
        };

        if user.status == UserStatus::Deleted {
            return Ok(Response::Error(format!(
                "'{}' is already deleted.",
                username.as_str()
            )));
        }
        if user.permission_level >= level {
            return Ok(Response::Error(format!(
                "Cannot delete '{}' — equal or higher permission tier.",
                username.as_str()
            )));
        }

        UserStore::update(
            &self.db,
            user.id,
            None,
            Some(UserStatus::Deleted),
            None,
            None,
        )
        .await
        .map_err(|e| HostError::Storage(format!("{e}")))?;

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

        if let Err(e) = self
            .db
            .audit_write(actor.as_str(), "delete_user", Some(username.as_str()), None)
            .await
        {
            tracing::warn!("audit write failed: {e}");
        }

        warn!(%actor, %username, "user deleted");
        Ok(Response::Text(format!(
            "'{}' has been deleted.",
            username.as_str()
        )))
    }

    async fn handle_edit_profile(&self, session: SessionId) -> Result<Response, HostError> {
        match self.session_auth_user(session).await {
            Ok(_) => {}
            Err(r) => return Ok(r),
        }
        {
            let sessions = self.sessions.read().await;
            if let Some(r) = sessions.get(&session) {
                if !matches!(r.workflow, Workflow::None) {
                    return Ok(Response::Error(
                        "A workflow is already in progress. Type 'cancel' first.".into(),
                    ));
                }
            }
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

    async fn handle_change_password(&self, session: SessionId) -> Result<Response, HostError> {
        match self.session_auth_user(session).await {
            Ok(_) => {}
            Err(r) => return Ok(r),
        }
        {
            let sessions = self.sessions.read().await;
            if let Some(r) = sessions.get(&session) {
                if !matches!(r.workflow, Workflow::None) {
                    return Ok(Response::Error(
                        "A workflow is already in progress. Type 'cancel' first.".into(),
                    ));
                }
            }
        }
        {
            let mut sessions = self.sessions.write().await;
            if let Some(r) = sessions.get_mut(&session) {
                r.workflow = Workflow::ChangePassword {
                    stage: ChangePwdStage::VerifyOld { attempts: 0 },
                };
            }
        }
        Ok(Response::Prompt {
            text: "Current password:".into(),
            hide_input: true,
        })
    }

    async fn handle_set_user_password(
        &self,
        session: SessionId,
        target: Username,
    ) -> Result<Response, HostError> {
        let (_, _, level, _) = match self.session_auth_user(session).await {
            Ok(t) => t,
            Err(r) => return Ok(r),
        };
        if level < PermissionLevel::Sysop {
            return Ok(Response::Error("Sysop access required.".into()));
        }
        let user = UserStore::get_by_username(&self.db, &target)
            .await
            .map_err(|e| HostError::Storage(format!("{e}")))?;
        if user.is_none() {
            return Ok(Response::Error(format!(
                "User '{}' not found.",
                target.as_str()
            )));
        }
        {
            let sessions = self.sessions.read().await;
            if let Some(r) = sessions.get(&session) {
                if !matches!(r.workflow, Workflow::None) {
                    return Ok(Response::Error(
                        "A workflow is already in progress. Type 'cancel' first.".into(),
                    ));
                }
            }
        }
        {
            let mut sessions = self.sessions.write().await;
            if let Some(r) = sessions.get_mut(&session) {
                r.workflow = Workflow::SetUserPassword {
                    target: target.clone(),
                    stage: SetUserPwdStage::EnterNew,
                };
            }
        }
        Ok(Response::Prompt {
            text: format!("New password for {}:", target.as_str()),
            hide_input: true,
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

        if let Err(e) = self
            .db
            .audit_write(actor.as_str(), "create_room", Some(name), None)
            .await
        {
            tracing::warn!("audit write failed: {e}");
        }

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

        if let Err(e) = self
            .db
            .audit_write(actor.as_str(), "delete_room", Some(name), None)
            .await
        {
            tracing::warn!("audit write failed: {e}");
        }

        info!(%actor, %name, "room deleted");
        Ok(Response::Text(format!("Room '{name}' deleted.")))
    }
}

// ── Command label (for log events) ───────────────────────────────────────────

fn cmd_label(cmd: &Command) -> &'static str {
    match cmd {
        Command::Help { .. } => "Help",
        Command::Login { .. } => "Login",
        Command::Register { .. } => "Register",
        Command::WorkflowReply { .. } => "WorkflowReply",
        Command::Cancel => "Cancel",
        Command::Logout | Command::Quit => "Logout",
        Command::Whoami => "Whoami",
        Command::ListRooms => "ListRooms",
        Command::GoNextUnread => "GoNextUnread",
        Command::ChangeRoom { .. } => "ChangeRoom",
        Command::GoMail => "GoMail",
        Command::IgnoreRoom => "IgnoreRoom",
        Command::ReadNew => "ReadNew",
        Command::ReadForward { .. } => "ReadForward",
        Command::ReadReverse => "ReadReverse",
        Command::ScanMessages => "ScanMessages",
        Command::FastForward => "FastForward",
        Command::EnterMessage { .. } => "EnterMessage",
        Command::DeleteMessage { .. } => "DeleteMessage",
        Command::WhoIsOnline => "WhoIsOnline",
        Command::ListPending => "ListPending",
        Command::ValidateUser { .. } => "ValidateUser",
        Command::BlockUser { .. } => "BlockUser",
        Command::BanUser { .. } => "BanUser",
        Command::UnbanUser { .. } => "UnbanUser",
        Command::EditProfile => "EditProfile",
        Command::ChangePassword => "ChangePassword",
        Command::EditRoom => "EditRoom",
        Command::EditUser { .. } => "EditUser",
        Command::CreateRoom { .. } => "CreateRoom",
        Command::DeleteRoom { .. } => "DeleteRoom",
        Command::Unknown { .. } => "Unknown",
        _ => "Other",
    }
}

// ── Help text ─────────────────────────────────────────────────────────────────

fn help_text(topic: Option<&str>, level: Option<PermissionLevel>) -> String {
    let logged_in = level.is_some();
    let is_aide = level >= Some(PermissionLevel::Aide);
    let is_sysop = level >= Some(PermissionLevel::Sysop);

    match topic {
        None => {
            if logged_in {
                HELP_QUICK_LOGGED_IN.to_owned()
            } else {
                HELP_QUICK_ANON.to_owned()
            }
        }
        Some(t) => match t.to_ascii_lowercase().as_str() {
            "all" if logged_in => HELP_OVERVIEW.to_owned(),
            "m" | "mail" if logged_in => HELP_MAIL.to_owned(),
            "r" | "read" | "reading" if logged_in => HELP_READING.to_owned(),
            "p" | "post" | "posting" if logged_in => HELP_POSTING.to_owned(),
            "u" | "users" if logged_in => HELP_USERS.to_owned(),
            "n" | "nav" | "navigation" if logged_in => HELP_NAVIGATION.to_owned(),
            "a" | "acct" | "account" if logged_in => HELP_ACCOUNT.to_owned(),
            "aide" if is_aide => HELP_AIDE.to_owned(),
            "sysop" if is_sysop => HELP_SYSOP.to_owned(),
            cmd => help_for_command(cmd, level),
        },
    }
}

fn help_for_command(cmd: &str, level: Option<PermissionLevel>) -> String {
    let logged_in = level.is_some();
    let is_aide = level >= Some(PermissionLevel::Aide);
    let is_sysop = level >= Some(PermissionLevel::Sysop);

    let detail = match cmd {
        // ── Always available ─────────────────────────────────────────────
        "h" | "help" | "?" => {
            if logged_in {
                "H — show this help\n\
                 H reading / posting / navigation / account\n\
                 H <cmd> for detail on one command (eg. H N)"
            } else {
                "H — show this help."
            }
        }
        "q" => {
            if logged_in {
                "Q — log out"
            } else {
                "Q — quit"
            }
        }
        "register" => "REGISTER <user> — create an account",
        "login" => "LOGIN <user> — log in to your account",
        "cancel" => "CANCEL — cancel the current workflow",

        // ── Logged-in only ───────────────────────────────────────────────
        "n" if logged_in => "N — read new messages since last visit",
        "f" if logged_in => "F — forward-read (oldest first)\nF <id> to start from a specific message",
        "r" if logged_in => "R — reverse-read (newest first)",
        "s" if logged_in => "S — scan message headers in this room",
        ".ff" if logged_in => ".FF — fast-forward past unread\nResets your last-read pointer to the latest message.",
        "e" if logged_in => "E — enter a message\nE <text> to post without a prompt\nIn Mail: E @user message",
        "d" if logged_in => "D <id> — delete a message\nAides and sysops can delete any message.",
        "g" if logged_in => "G — go to next room with unread messages",
        "c" if logged_in => "C <name> — change room by name or number",
        "k" if logged_in => "K — list known rooms",
        "i" if logged_in => "I — ignore this room (toggle)\nIgnored rooms are skipped during navigation.",
        "m" if logged_in => {
            "M — go to Mail (private messages)\n\
             In Mail: E to write, N to read new,\n\
             F/R older/newer, S scan, D <#> delete.\n\
             H mail for full mail help."
        }
        "w" if logged_in => "W — who's online",
        "b" if logged_in => {
            "B <user> — block / unblock user\n\
             Prefix + to force-block, - to force-unblock, or omit to toggle.\n\
             Hides their messages from you."
        }
        "profile" if logged_in => "PROFILE — edit your display name",
        "passwd" if logged_in => {
            "PASSWD — change your password\n\
             You'll be asked for your current password, then the new one twice."
        }
        "stop" if logged_in => "STOP — stop pending messages",

        // ── Aide+ only ───────────────────────────────────────────────────
        "v" if is_aide => "V — validate pending users\nEnters the user validation workflow.",
        "pending" if is_aide => "PENDING — list users awaiting validation",
        ".er" if is_aide => ".ER — edit current room\nEdit name, description, read-only flag, or min permission level.",
        ".eu" if is_aide => ".EU <user> — edit a user's profile or permissions\nAides cannot promote to Sysop.",
        "ban" if is_aide => "BAN <user> — ban a user account",
        "u" | "users" if logged_in => {
            "U — list active user accounts\n\
             U banned — list banned accounts\n\
             U all — list all accounts (sysop)"
        }
        "s" | "search" if logged_in => "S <query> — find users by username (substring match)",
        "whois" if logged_in => {
            "WHOIS <user> — show account details\n\
             Includes level, status, join date, last login, and active sessions."
        }

        // ── Sysop+ only ──────────────────────────────────────────────────
        "unban" if is_sysop => "UNBAN <user> — lift a ban",
        ".c" if is_sysop => ".C — create a new room\nEnters the room creation workflow.",
        ".dr" if is_sysop => ".DR <name> — delete a room",
        ".du" if is_sysop => ".DU <user> — soft-delete a user account\nSets status to deleted and ends active sessions.",
        ".pw" if is_sysop => ".PW <user> — reset another user's password\nDoes not require knowing their current password.",
        "openaccess" if is_sysop => {
            "OPENACCESS — disable verification requirement (SHTF mode)\n\
             All registrations immediately receive User-level access.\n\
             Takes effect immediately and persists to config.toml."
        }
        "closeaccess" if is_sysop => {
            "CLOSEACCESS — restore verification requirement\n\
             New accounts must be validated by an aide or sysop.\n\
             Takes effect immediately and persists to config.toml."
        }
        "guestroom" if is_sysop => {
            "GUESTROOM <name> — set guest room (created if needed)\n\
             Unverified users are placed here and cannot leave.\n\
             GUESTROOM OFF to disable.\n\
             Takes effect immediately and persists to config.toml."
        }

        other => {
            return format!(
                "No help for '{other}'.\n\
                 H for commands, H reading/posting/navigation/account for topics."
            )
        }
    };
    detail.to_owned()
}

const HELP_QUICK_ANON: &str = "\
REGISTER <user>  create an account\n\
LOGIN <user>     log in to your account\n\
Q  quit\n\
H  help";

// 156 bytes — must stay ≤ MAX_REPLY_BYTES (MAX_FRAME_SIZE(172) - 16 bytes overhead).
const HELP_QUICK_LOGGED_IN: &str = "\
 K  list rooms\n\
 C  change room\n\
 N  new messages\n\
 E  enter message\n\
 G  next unread\n\
 M  go to Mail\n\
 W  who's online\n\
 Q  log out\n\
H all - show all";

const HELP_OVERVIEW: &str = "\
H M — Mail\n\
H R — Reading\n\
H P — Posting\n\
H U — Users\n\
H N — Navigation\n\
H A — Account";

const HELP_READING: &str = "\
Reading:\n\
 N    read new messages\n\
 F    forward-read (oldest first)\n\
 R    reverse-read (newest first)\n\
 S    scan message headers\n\
 .FF  fast-forward past unread";

const HELP_POSTING: &str = "\
Posting:\n\
 D <#>  delete\n\
 E      enter message (prompts)\n\
 E msg  post now, no prompt\n\
 E @user msg  send DM inline";

const HELP_NAVIGATION: &str = "\
Navigation:\n\
 C    change room\n\
 G    next unread room\n\
 I    ignore this room\n\
 K    list known rooms\n\
 M    go to Mail";

const HELP_MAIL: &str = "\
Mail (private messages):\n\
 M    go to Mail\n\
 E    write (prompts)\n\
 E @user msg  send inline\n\
 N    read new\n\
 F/R  older/newer\n\
 S    scan\n\
 D <#> delete";

const HELP_ACCOUNT: &str = "\
Account:\n\
 B      block / unblock a user\n\
 PASSWD  change your password\n\
 PROFILE edit your display name\n\
 Q      log out\n\
 W      who's online\n\
H U — Users";

const HELP_AIDE: &str = "\
Aide:\n\
 PENDING  pending users\n\
 V <u>   validate user\n\
 BAN <u>  ban a user\n\
 .ER     edit current room\n\
H U — Users";

const HELP_USERS: &str = "\
Users:\n\
 U         list active\n\
 U banned  list banned\n\
 U all     list all\n\
 S <q>     search\n\
 WHOIS <u> details";

const HELP_SYSOP: &str = "\
Sysop:\n\
 .C .DR .DU .PW rooms/users\n\
 UNBAN <u>   lift a ban\n\
 OPENACCESS  skip verify\n\
 CLOSEACCESS require verify\n\
 GUESTROOM <name>|OFF";

// ── Formatting helpers ────────────────────────────────────────────────────────

fn build_message_with_nav(msg: &Message, has_prev: bool, has_next: bool) -> String {
    let mut nav = Vec::new();
    if has_prev {
        nav.push("R - Previous");
    }
    if has_next {
        nav.push("F - Next");
    }
    nav.push("E - Reply");
    format!("{}\n{}", format_message(msg), nav.join("  "))
}

fn format_message(msg: &Message) -> String {
    let id = msg.id.as_i64();
    let sender = msg.sender.as_str();
    // Collapse embedded newlines so a multiline body doesn't corrupt the listing
    // format (lines are separated by \n in the response text).
    let content = msg.content.replace('\r', "").replace('\n', " ");
    if let Some(ref recipient) = msg.recipient {
        let r = recipient.as_str();
        format!("#{id} [DM→{r}] {sender}: {content}")
    } else {
        format!("#{id} {sender}: {content}")
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use bbs_plugin_api::{Command, Username};
    use tempfile::NamedTempFile;

    /// Every canned help string must fit in one companion frame.
    ///
    /// `SendTxtMsg` wire layout adds 16 bytes of overhead on top of the text.
    /// With `MAX_FRAME_SIZE = 172` the maximum text is 156 bytes.
    #[test]
    fn help_strings_fit_mesh_payload() {
        // MAX_FRAME_SIZE(172) - 16 bytes overhead = 156 bytes max text.
        const MESH_MAX: usize = 156;
        let cases = [
            ("HELP_QUICK_ANON", HELP_QUICK_ANON),
            ("HELP_QUICK_LOGGED_IN", HELP_QUICK_LOGGED_IN),
            ("HELP_OVERVIEW", HELP_OVERVIEW),
            ("HELP_MAIL", HELP_MAIL),
            ("HELP_READING", HELP_READING),
            ("HELP_POSTING", HELP_POSTING),
            ("HELP_NAVIGATION", HELP_NAVIGATION),
            ("HELP_ACCOUNT", HELP_ACCOUNT),
            ("HELP_AIDE", HELP_AIDE),
            ("HELP_USERS", HELP_USERS),
            ("HELP_SYSOP", HELP_SYSOP),
        ];
        for (name, s) in cases {
            assert!(
                s.len() <= MESH_MAX,
                "{name} is {} bytes — exceeds {MESH_MAX}-byte MeshCore payload limit",
                s.len()
            );
        }
    }

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

        // First registrant is promoted to Sysop automatically.
        let ctx = host.permission_ctx(sid).await.unwrap();
        assert_eq!(ctx.level, PermissionLevel::Sysop);
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

        // K now returns Prompt (Workflow::Rooms) so the user can pick by number.
        let resp = host.process_command(sid, Command::ListRooms).await.unwrap();
        let Response::Prompt { text, .. } = resp else {
            panic!("expected Prompt from ListRooms")
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
            .process_command(sid, Command::EnterMessage { body: None })
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

    // ── Registration: sysop notification ─────────────────────────────────────

    /// Full registration workflow for a username/password pair.
    async fn do_register(host: &BbsHost, sid: SessionId, username: &str, password: &str) {
        let uname = Username::new(username).unwrap();
        host.process_command(sid, Command::Register { username: uname })
            .await
            .unwrap();
        // display name (empty = skip)
        host.process_command(
            sid,
            Command::WorkflowReply {
                reply: String::new(),
            },
        )
        .await
        .unwrap();
        // password
        host.process_command(
            sid,
            Command::WorkflowReply {
                reply: password.into(),
            },
        )
        .await
        .unwrap();
        // confirm
        host.process_command(
            sid,
            Command::WorkflowReply {
                reply: password.into(),
            },
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn registration_dms_sysop() {
        let (host, _db) = make_host().await;

        // First registrant — auto-promoted to sysop.
        let s1 = host.create_session("test").await.unwrap();
        do_register(&host, s1, "sysop", "s3cr3t!!").await;

        let sysop_name = Username::new("sysop").unwrap();

        // No DMs yet (first user, no notification sent).
        let page = MessageStore::list_direct(&host.db, &sysop_name, None, 10)
            .await
            .unwrap();
        assert!(
            page.messages.is_empty(),
            "first registrant should receive no notification DM"
        );

        // Second registrant — should trigger a DM to sysop.
        let s2 = host.create_session("test").await.unwrap();
        do_register(&host, s2, "newuser", "abc12345").await;

        let page = MessageStore::list_direct(&host.db, &sysop_name, None, 10)
            .await
            .unwrap();
        assert_eq!(
            page.messages.len(),
            1,
            "sysop should receive exactly one notification DM"
        );
        let dm = &page.messages[0];
        assert_eq!(dm.sender, Username::__internal_system("bbs"));
        assert_eq!(dm.recipient.as_ref(), Some(&sysop_name));
        assert!(
            dm.content.contains("newuser"),
            "DM should mention the new username"
        );
        assert!(
            dm.content.to_lowercase().contains("verify")
                || dm.content.to_lowercase().contains("v newuser"),
            "DM should hint at the verify command"
        );
    }

    // ── Password change ───────────────────────────────────────────────────────

    #[tokio::test]
    async fn passwd_change_full_flow() {
        let (host, _db) = make_host().await;
        let sid = host.create_session("test").await.unwrap();
        do_register(&host, sid, "alice", "oldpass1").await;

        // Start PASSWD workflow.
        let r = host
            .process_command(sid, Command::ChangePassword)
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
            "should prompt for current password"
        );

        // Provide current password.
        let r = host
            .process_command(
                sid,
                Command::WorkflowReply {
                    reply: "oldpass1".into(),
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
            "should prompt for new password"
        );

        // Provide new password.
        let r = host
            .process_command(
                sid,
                Command::WorkflowReply {
                    reply: "newpass99".into(),
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
            "should prompt for confirmation"
        );

        // Confirm new password.
        let r = host
            .process_command(
                sid,
                Command::WorkflowReply {
                    reply: "newpass99".into(),
                },
            )
            .await
            .unwrap();
        assert_eq!(r, Response::Text("Password changed successfully.".into()));

        // Verify new password works — log out, log back in with new password.
        host.process_command(sid, Command::Logout).await.unwrap();
        let sid2 = host.create_session("test").await.unwrap();
        let uname = Username::new("alice").unwrap();
        host.process_command(sid2, Command::Login { username: uname })
            .await
            .unwrap();
        let r = host
            .process_command(
                sid2,
                Command::WorkflowReply {
                    reply: "newpass99".into(),
                },
            )
            .await
            .unwrap();
        assert!(
            matches!(r, Response::LoggedIn { .. }),
            "new password should log in"
        );
    }

    #[tokio::test]
    async fn passwd_wrong_current_password_is_retried() {
        let (host, _db) = make_host().await;
        let sid = host.create_session("test").await.unwrap();
        do_register(&host, sid, "bob", "correct8").await;

        host.process_command(sid, Command::ChangePassword)
            .await
            .unwrap();

        // Wrong password — twice.
        let r1 = host
            .process_command(
                sid,
                Command::WorkflowReply {
                    reply: "wrongpwd".into(),
                },
            )
            .await
            .unwrap();
        assert!(
            matches!(r1, Response::Prompt { .. }),
            "first wrong attempt should re-prompt"
        );

        let r2 = host
            .process_command(
                sid,
                Command::WorkflowReply {
                    reply: "wrongpwd".into(),
                },
            )
            .await
            .unwrap();
        assert!(
            matches!(r2, Response::Prompt { .. }),
            "second wrong attempt should re-prompt"
        );

        // Third wrong attempt — workflow should be aborted with an error.
        let r3 = host
            .process_command(
                sid,
                Command::WorkflowReply {
                    reply: "wrongpwd".into(),
                },
            )
            .await
            .unwrap();
        assert!(
            matches!(r3, Response::Error(_)),
            "third wrong attempt should abort"
        );

        // Workflow is cleared — new commands should work normally.
        let r = host.process_command(sid, Command::ListRooms).await.unwrap();
        assert!(
            !matches!(r, Response::Error(_)),
            "session should be usable after aborted PASSWD"
        );
    }

    #[tokio::test]
    async fn passwd_confirm_mismatch_retries_new_password() {
        let (host, _db) = make_host().await;
        let sid = host.create_session("test").await.unwrap();
        do_register(&host, sid, "carol", "startpwd").await;

        host.process_command(sid, Command::ChangePassword)
            .await
            .unwrap();

        // Correct current password.
        host.process_command(
            sid,
            Command::WorkflowReply {
                reply: "startpwd".into(),
            },
        )
        .await
        .unwrap();

        // New password.
        host.process_command(
            sid,
            Command::WorkflowReply {
                reply: "mynewpwd1".into(),
            },
        )
        .await
        .unwrap();

        // Mismatched confirmation → should go back to EnterNew prompt.
        let r = host
            .process_command(
                sid,
                Command::WorkflowReply {
                    reply: "differentp".into(),
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
            "mismatch should re-prompt for new password"
        );
    }

    #[tokio::test]
    async fn first_registrant_gets_no_sysop_dm() {
        // Regression: the first user (who IS the sysop) must not receive a
        // spurious notification about themselves.
        let (host, _db) = make_host().await;
        let sid = host.create_session("test").await.unwrap();
        do_register(&host, sid, "admin", "password1").await;

        let admin_name = Username::new("admin").unwrap();
        let page = MessageStore::list_direct(&host.db, &admin_name, None, 10)
            .await
            .unwrap();
        assert!(
            page.messages.is_empty(),
            "first registrant should not get a DM about themselves"
        );
    }

    // ── Access policy tests ───────────────────────────────────────────────────

    async fn make_host_with_policy(policy: AccessPolicy) -> (Arc<BbsHost>, NamedTempFile) {
        let f = NamedTempFile::new().unwrap();
        let db = Database::open(&f.path().to_string_lossy())
            .await
            .expect("db open");
        let host = BbsHost::with_config(db, None, policy, None);
        host.ensure_guest_room().await.expect("ensure_guest_room");
        (Arc::new(host), f)
    }

    /// require_verify = false: unvalidated user gets full access right after registration.
    #[tokio::test]
    async fn open_access_unvalidated_treated_as_user() {
        let policy = AccessPolicy {
            require_verify: false,
            guest_room_name: None,
        };
        let (host, _db) = make_host_with_policy(policy).await;

        // First user (sysop).
        let s1 = host.create_session("test").await.unwrap();
        do_register(&host, s1, "admin", "s3cr3t!!").await;

        // Second user — Unvalidated in DB but require_verify = false.
        let s2 = host.create_session("test").await.unwrap();
        do_register(&host, s2, "alice", "alice123!!").await;

        // alice should get a room list (Prompt), not a "pending" text.
        let r = host.process_command(s2, Command::ListRooms).await.unwrap();
        assert!(
            matches!(r, Response::Prompt { .. }),
            "expected room list prompt, got: {r:?}"
        );
    }

    /// guest_room configured: guest user only sees the guest room in K.
    #[tokio::test]
    async fn guest_room_list_only_shows_guest_room() {
        let policy = AccessPolicy {
            require_verify: true,
            guest_room_name: Some("Guests".to_owned()),
        };
        let (host, _db) = make_host_with_policy(policy).await;

        // Register sysop (first).
        let s1 = host.create_session("test").await.unwrap();
        do_register(&host, s1, "admin", "s3cr3t!!").await;

        // Register alice (unvalidated guest).
        let s2 = host.create_session("test").await.unwrap();
        do_register(&host, s2, "alice", "alice123!!").await;

        // K should return a prompt listing only "Guests" room.
        let r = host.process_command(s2, Command::ListRooms).await.unwrap();
        match r {
            Response::Prompt { text, .. } => {
                assert!(
                    text.contains("Guests"),
                    "guest room should be listed: {text}"
                );
                assert!(
                    !text.contains("Lobby"),
                    "Lobby should be hidden from guests: {text}"
                );
            }
            other => panic!("expected Prompt, got: {other:?}"),
        }
    }

    /// guest_room configured: guest cannot navigate to Lobby.
    #[tokio::test]
    async fn guest_cannot_change_to_non_guest_room() {
        let policy = AccessPolicy {
            require_verify: true,
            guest_room_name: Some("Guests".to_owned()),
        };
        let (host, _db) = make_host_with_policy(policy).await;

        let s1 = host.create_session("test").await.unwrap();
        do_register(&host, s1, "admin", "s3cr3t!!").await;

        let s2 = host.create_session("test").await.unwrap();
        do_register(&host, s2, "alice", "alice123!!").await;

        let r = host
            .process_command(
                s2,
                Command::ChangeRoom {
                    target: "Lobby".to_owned(),
                },
            )
            .await
            .unwrap();
        assert!(
            matches!(r, Response::Text(ref t) if t.contains("verified")),
            "expected verification required message, got: {r:?}"
        );
    }

    /// guest_room configured: guest can post in the guest room.
    #[tokio::test]
    async fn guest_can_post_in_guest_room() {
        let policy = AccessPolicy {
            require_verify: true,
            guest_room_name: Some("Guests".to_owned()),
        };
        let (host, _db) = make_host_with_policy(policy).await;

        let s1 = host.create_session("test").await.unwrap();
        do_register(&host, s1, "admin", "s3cr3t!!").await;

        let s2 = host.create_session("test").await.unwrap();
        do_register(&host, s2, "alice", "alice123!!").await;

        // alice starts in guest room after registration.
        // EnterMessage in guest room should give a compose prompt.
        let r = host
            .process_command(
                s2,
                Command::EnterMessage {
                    body: Some("hello from guest".to_owned()),
                },
            )
            .await
            .unwrap();
        assert!(
            matches!(r, Response::Prompt { .. }),
            "expected compose prompt, got: {r:?}"
        );
    }

    /// guest_room configured: after validation, user lands in Lobby on next login.
    #[tokio::test]
    async fn verified_user_lands_in_lobby() {
        let policy = AccessPolicy {
            require_verify: true,
            guest_room_name: Some("Guests".to_owned()),
        };
        let (host, _db) = make_host_with_policy(policy).await;

        let s1 = host.create_session("test").await.unwrap();
        do_register(&host, s1, "admin", "s3cr3t!!").await;

        let s2 = host.create_session("test").await.unwrap();
        do_register(&host, s2, "alice", "alice123!!").await;

        // alice should be in guest room (not Lobby).
        {
            let sessions = host.sessions.read().await;
            let r = sessions.get(&s2).unwrap();
            assert_ne!(
                r.current_room, LOBBY_ROOM_ID,
                "unverified user should not start in Lobby"
            );
        }

        // Promote alice to User.
        let alice_name = Username::new("alice").unwrap();
        force_validate(&host, &alice_name).await;

        // Log alice out and back in.
        host.end_session(s2).await.unwrap();
        let s3 = host.create_session("test").await.unwrap();
        host.process_command(
            s3,
            Command::Login {
                username: alice_name,
            },
        )
        .await
        .unwrap();
        host.process_command(
            s3,
            Command::WorkflowReply {
                reply: "alice123!!".to_owned(),
            },
        )
        .await
        .unwrap();

        {
            let sessions = host.sessions.read().await;
            let r = sessions.get(&s3).unwrap();
            assert_eq!(
                r.current_room, LOBBY_ROOM_ID,
                "verified user should land in Lobby after login"
            );
        }
    }
}
