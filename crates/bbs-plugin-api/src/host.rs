//! The `Host` trait — what plugins call into.
//!
//! `bbs-core` provides the canonical implementation of this trait.
//! Plugins receive an `Arc<dyn Host>` at `init` time and use it for
//! everything they want the BBS to do: process commands, manage
//! sessions, subscribe to events.
//!
//! ## Permission gating
//!
//! Methods that touch user-visible state take a [`PermissionCtx`]
//! argument. The host enforces that the context's tier satisfies
//! the operation's requirement; transports cannot synthesise
//! contexts of arbitrary authority because the only way to mint
//! a `PermissionCtx` is through the host itself
//! (see [`PermissionCtx::__internal_new`]).
//!
//! ## What's NOT here yet
//!
//! Domain accessors — `host.users()`, `host.rooms()`,
//! `host.messages()` — are referenced in the architecture doc but
//! not yet on the trait. They'll land alongside the corresponding
//! types in `bbs-core`. For now `process_command` is the route
//! through which transport plugins manipulate domain state.

use std::sync::Arc;

use crate::admin::{
    AdminAuditEntry, AdminBackupRecord, AdminMessageRecord, AdminReports, AdminRoomSummary,
    AdminSessionInfo, AdminStats, AdminUserInfo,
};
use crate::advert::AdvertBus;
use crate::command::{Command, Response};
use crate::error::HostError;
use crate::event::DomainEvent;
use crate::identity::SessionId;
use crate::permissions::{PermissionCtx, PermissionLevel};
use async_trait::async_trait;
use tokio::sync::broadcast;

/// What the host exposes to plugins.
///
/// Implementations of this trait are produced by `bbs-core` and
/// passed to each plugin at `init`. Plugins hold an
/// `Arc<dyn Host>` for their lifetime.
#[async_trait]
pub trait Host: Send + Sync {
    // ── Command processing ──────────────────────────────────────

    /// Process a command from a session.
    ///
    /// Permission checks happen inside the host based on the
    /// session's currently-bound user (or the unvalidated
    /// pre-auth tier if the session isn't bound yet). Transports
    /// cannot bypass these checks.
    ///
    /// The session may not be bound to a user — registration and
    /// login flow through this method on a pre-auth session.
    async fn process_command(
        &self,
        session: SessionId,
        cmd: Command,
    ) -> Result<Response, HostError>;

    // ── Sessions ────────────────────────────────────────────────

    /// Mint a new, unbound session for the given transport.
    /// Returns the freshly-allocated `SessionId`. The transport
    /// is responsible for binding this session ID to its
    /// connection (e.g., setting a cookie, recording a node->id
    /// mapping).
    ///
    /// The transport name must match a `TransportEngine::name()`
    /// of a loaded transport plugin. The host records this for
    /// audit.
    async fn create_session(&self, transport: &'static str) -> Result<SessionId, HostError>;

    /// End a session. Idempotent: ending an already-ended or
    /// unknown session returns Ok. The host emits a
    /// [`DomainEvent::SessionEnded`] for downstream consumers.
    async fn end_session(&self, session: SessionId) -> Result<(), HostError>;

    /// Look up the permission context for a session. Plugins use
    /// this when they need to check authority before doing
    /// anything optimistic — the host's own methods do the gating
    /// internally, so most plugins don't call this directly.
    async fn permission_ctx(&self, session: SessionId) -> Result<PermissionCtx, HostError>;

    // ── Domain events ───────────────────────────────────────────

    /// Subscribe to the domain-event stream. Each call returns a
    /// new `broadcast::Receiver`; events fan out to all active
    /// subscribers.
    ///
    /// The capacity of the broadcast channel is set by the host;
    /// slow consumers may receive `RecvError::Lagged` and miss
    /// events. Plugins should be ready to handle this — for most
    /// notifications, missing one is fine because the next
    /// trigger fires another. Plugins that need durable delivery
    /// (audit log, reports) should subscribe directly to the
    /// audit log via `process_command`-driven flows, not events.
    fn events(&self) -> broadcast::Receiver<DomainEvent>;

    // ── Mesh adverts ────────────────────────────────────────────

    /// Access the shared advert bus.
    ///
    /// `MeshTransport` writes records here when adverts are heard
    /// over the air and subscribes to the send-request channel.
    /// `WebPlugin` reads the list and triggers sends on behalf of
    /// the sysop.
    fn advert_bus(&self) -> Arc<AdvertBus>;

    // ── Admin / web-UI operations ────────────────────────────────────────────
    //
    // These methods are called by the web admin plugin.  Minimal `Host`
    // implementations (e.g. `MockHost` for plugin unit tests) can rely on the
    // default impls, which return `HostError::NotSupported`.

    /// Verify a BBS username + password for web-UI login.
    ///
    /// Returns the user's `PermissionLevel` on success.  Returns
    /// `HostError::NotFound` when the username is unknown,
    /// `HostError::PermissionDenied` when the credentials are wrong or the
    /// account is inactive, and `HostError::NotSupported` in minimal impls.
    async fn admin_verify_credentials(
        &self,
        username: &str,
        password: &str,
    ) -> Result<PermissionLevel, HostError> {
        let _ = (username, password);
        Err(HostError::NotSupported("admin_verify_credentials".into()))
    }

    /// Return info about every currently-live BBS session.
    async fn admin_list_sessions(&self) -> Result<Vec<AdminSessionInfo>, HostError> {
        Err(HostError::NotSupported("admin_list_sessions".into()))
    }

    /// List user accounts.
    ///
    /// `status_filter`: `None` = all; `Some(0)` = Active; `Some(1)` = Banned.
    async fn admin_list_users(
        &self,
        status_filter: Option<u8>,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<AdminUserInfo>, HostError> {
        let _ = (status_filter, limit, offset);
        Err(HostError::NotSupported("admin_list_users".into()))
    }

    /// Create a new user account directly, without going through the
    /// interactive registration workflow.
    ///
    /// Intended for CLI bootstrap (e.g. creating the first sysop when there is
    /// no existing account to log in with).
    ///
    /// `permission_level`: 0 = Unvalidated, 10 = User, 50 = Aide, 100 = Sysop.
    /// The account is created with `status = Active`.
    ///
    /// Returns `HostError::PreconditionFailed` if the username is already taken
    /// or is syntactically invalid.
    async fn admin_create_user(
        &self,
        username: &str,
        password: &str,
        permission_level: u8,
    ) -> Result<(), HostError> {
        let _ = (username, password, permission_level);
        Err(HostError::NotSupported("admin_create_user".into()))
    }

    /// Update a user's status and/or permission level.
    ///
    /// Pass `None` for either field to leave it unchanged.
    async fn admin_update_user(
        &self,
        username: &str,
        status: Option<u8>,
        permission_level: Option<u8>,
    ) -> Result<(), HostError> {
        let _ = (username, status, permission_level);
        Err(HostError::NotSupported("admin_update_user".into()))
    }

    /// List all rooms with message counts.
    async fn admin_list_rooms(&self) -> Result<Vec<AdminRoomSummary>, HostError> {
        Err(HostError::NotSupported("admin_list_rooms".into()))
    }

    /// Create a new room and return its summary.
    async fn admin_create_room(
        &self,
        name: &str,
        description: Option<&str>,
    ) -> Result<AdminRoomSummary, HostError> {
        let _ = (name, description);
        Err(HostError::NotSupported("admin_create_room".into()))
    }

    /// Delete a room by ID.  Returns `false` if the room did not exist or is
    /// a protected system room.
    async fn admin_delete_room(&self, room_id: i64) -> Result<bool, HostError> {
        let _ = room_id;
        Err(HostError::NotSupported("admin_delete_room".into()))
    }

    /// List messages in a room, cursor-paginated.
    async fn admin_list_messages(
        &self,
        room_id: i64,
        limit: u32,
        after_id: Option<i64>,
    ) -> Result<Vec<AdminMessageRecord>, HostError> {
        let _ = (room_id, limit, after_id);
        Err(HostError::NotSupported("admin_list_messages".into()))
    }

    /// Delete a message by ID.  Returns `false` if it did not exist.
    async fn admin_delete_message(&self, message_id: i64) -> Result<bool, HostError> {
        let _ = message_id;
        Err(HostError::NotSupported("admin_delete_message".into()))
    }

    /// Return aggregate BBS statistics.
    async fn admin_stats(&self) -> Result<AdminStats, HostError> {
        Err(HostError::NotSupported("admin_stats".into()))
    }

    /// Return analytics reports: top senders, top rooms, daily volume, stale rooms.
    async fn admin_reports(&self) -> Result<AdminReports, HostError> {
        Err(HostError::NotSupported("admin_reports".into()))
    }

    /// Trigger a `VACUUM INTO` backup written to `backup_dir`.
    ///
    /// The filename is auto-generated with a UTC timestamp.
    async fn admin_trigger_backup(&self, backup_dir: &str) -> Result<AdminBackupRecord, HostError> {
        let _ = backup_dir;
        Err(HostError::NotSupported("admin_trigger_backup".into()))
    }

    /// List `.db` backup files found in `backup_dir`.
    async fn admin_list_backups(
        &self,
        backup_dir: &str,
    ) -> Result<Vec<AdminBackupRecord>, HostError> {
        let _ = backup_dir;
        Err(HostError::NotSupported("admin_list_backups".into()))
    }

    /// Delete a backup file (and its associated config snapshot) from `backup_dir`.
    /// Returns `HostError::NotFound` if the file does not exist.
    async fn admin_delete_backup(&self, backup_dir: &str, filename: &str) -> Result<(), HostError> {
        let _ = (backup_dir, filename);
        Err(HostError::NotSupported("admin_delete_backup".into()))
    }

    // ── Audit log ────────────────────────────────────────────────────────────────

    /// Append one entry to the durable audit log.
    ///
    /// Called by the host after every privileged action (via command path) and
    /// by the web plugin after admin UI actions (with `actor = "web:<username>"`).
    ///
    /// Failures are logged but not propagated — audit writes must not block or
    /// fail the action they are recording.
    async fn admin_write_audit(
        &self,
        actor: &str,
        action: &str,
        target: Option<&str>,
        detail: Option<&str>,
    ) -> Result<(), HostError> {
        let _ = (actor, action, target, detail);
        Err(HostError::NotSupported("admin_write_audit".into()))
    }

    /// Return paginated audit log entries, newest first.
    ///
    /// `action_filter`: when `Some`, only entries with that `action` value are
    /// returned.  `limit` caps the result count; `offset` skips that many rows.
    async fn admin_audit_log(
        &self,
        limit: u32,
        offset: u32,
        action_filter: Option<&str>,
    ) -> Result<Vec<AdminAuditEntry>, HostError> {
        let _ = (limit, offset, action_filter);
        Err(HostError::NotSupported("admin_audit_log".into()))
    }

    // ── Mesh node credentials ────────────────────────────────────────────────────
    //
    // These methods implement the persistent node → user binding that lets mesh
    // nodes auto-login without re-entering credentials across server restarts.
    // The binding is stored in the `node_credentials` table (migration 0006).
    //
    // Minimal `Host` implementations can rely on the default no-op impls.

    /// Check whether a mesh node has a valid stored credential and, if so,
    /// auto-bind the session to that user.
    ///
    /// `prefix` is the first 6 bytes of the node's Ed25519 public key.
    /// `ttl_days` is the maximum age of a valid credential.
    ///
    /// Returns `Some(username)` on a successful restore (the session is now
    /// authenticated as that user).  Returns `None` when no binding exists,
    /// it has expired, or the bound user no longer exists / is banned.
    async fn mesh_node_restore(
        &self,
        session: SessionId,
        prefix: [u8; 6],
        ttl_days: u32,
    ) -> Result<Option<crate::identity::Username>, HostError> {
        let _ = (session, prefix, ttl_days);
        Ok(None)
    }

    /// Persist a node → current-session-user binding after a successful login.
    ///
    /// Called by the mesh transport immediately after it receives
    /// `Response::LoggedIn`.  Idempotent: calling again refreshes `last_auth`.
    async fn mesh_node_bind(&self, session: SessionId, prefix: [u8; 6]) -> Result<(), HostError> {
        let _ = (session, prefix);
        Ok(())
    }

    /// Remove the stored binding for a node prefix on explicit logout.
    ///
    /// Called by the mesh transport when it receives `Response::LoggedOut`.
    async fn mesh_node_unbind(&self, prefix: [u8; 6]) -> Result<(), HostError> {
        let _ = prefix;
        Ok(())
    }
}
