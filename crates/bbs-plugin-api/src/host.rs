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

/// Request sent from [`Host`] to the mesh transport's admin channel.
pub enum MeshKeyRequest {
    /// Fetch the device's private key hex (64 chars).
    ExportKey {
        /// One-shot channel to deliver the result back to the caller.
        reply: tokio::sync::oneshot::Sender<Result<String, String>>,
    },
    /// Push a new private key (32 raw bytes) to the device.
    ImportKey {
        /// Raw 32-byte private key to install on the device.
        key: [u8; 32],
        /// One-shot channel to deliver the result back to the caller.
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// Apply LoRa radio parameters to the companion device.
    ApplyRadio {
        /// The radio parameters to apply.
        params: crate::admin::MeshRadioParams,
        /// One-shot channel to deliver the result back to the caller.
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
}

/// Request sent from [`Host`] to the Meshtastic transport's admin channel.
pub enum MeshtasticAdminRequest {
    /// Fetch the current LoRa radio config from the device.
    GetLoRaConfig {
        /// One-shot channel to deliver the result back to the caller.
        reply: tokio::sync::oneshot::Sender<Result<crate::admin::MeshtasticLoRaConfig, String>>,
    },
    /// Push a new LoRa radio config to the device.
    SetLoRaConfig {
        /// The LoRa config to write to the device.
        config: crate::admin::MeshtasticLoRaConfig,
        /// One-shot channel to deliver the result back to the caller.
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// Fetch the node's owner/user info (long name, short name, public key).
    GetOwner {
        /// One-shot channel to deliver the result back to the caller.
        reply: tokio::sync::oneshot::Sender<Result<crate::admin::MeshtasticOwnerInfo, String>>,
    },
    /// Update the node's owner/user info.
    SetOwner {
        /// New long name, or `None` to leave unchanged.
        long_name: Option<String>,
        /// New short name (≤ 4 chars), or `None` to leave unchanged.
        short_name: Option<String>,
        /// One-shot channel to deliver the result back to the caller.
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    /// Fetch the device's security / PKC configuration.
    GetSecurity {
        /// One-shot channel to deliver the result back to the caller.
        reply: tokio::sync::oneshot::Sender<Result<crate::admin::MeshtasticSecurityInfo, String>>,
    },
    /// Fetch a combined snapshot (LoRa + owner + security) from the cache
    /// captured during the last connect-time sync. Answered instantly without a
    /// device round-trip.
    GetSnapshot {
        /// One-shot channel to deliver the snapshot back to the caller.
        reply: tokio::sync::oneshot::Sender<Result<crate::admin::MeshtasticDeviceSnapshot, String>>,
    },
}

use crate::admin::{
    AdminAccessPolicy, AdminAuditEntry, AdminBackupRecord, AdminMessageRecord, AdminReports,
    AdminRoomSummary, AdminSessionInfo, AdminStats, AdminUserInfo, MeshRadioParams,
    MeshtasticLoRaConfig, MeshtasticOwnerInfo, MeshtasticSecurityInfo,
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

    /// Reset a user's password without requiring the old password.
    ///
    /// Sysop-only operation.  Returns `HostError::NotFound` when the username is
    /// unknown and `HostError::NotSupported` in minimal implementations.
    async fn admin_set_password(&self, username: &str, password: &str) -> Result<(), HostError> {
        let _ = (username, password);
        Err(HostError::NotSupported("admin_set_password".into()))
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

    /// Forcibly terminate a live session by its numeric ID.
    ///
    /// Returns `true` when a session was found and removed, `false` when no
    /// matching session existed (already gone).
    async fn admin_kill_session(&self, session_id: u64) -> Result<bool, HostError> {
        let _ = session_id;
        Err(HostError::NotSupported("admin_kill_session".into()))
    }

    /// Update mutable fields of a room.
    ///
    /// All parameters are optional; `None` = leave unchanged.
    /// `description = Some(None)` clears the description to NULL.
    ///
    /// Returns the updated `AdminRoomSummary` on success.
    async fn admin_update_room(
        &self,
        room_id: i64,
        description: Option<Option<String>>,
        read_only: Option<bool>,
        min_permission_level: Option<u8>,
    ) -> Result<AdminRoomSummary, HostError> {
        let _ = (room_id, description, read_only, min_permission_level);
        Err(HostError::NotSupported("admin_update_room".into()))
    }

    /// Search room messages (excludes private Mail DMs).
    ///
    /// `sender`: filter by exact username; `None` = all senders.
    /// `query`: substring match against message content; `None` = no text filter.
    /// `limit`: maximum rows to return (capped server-side at 200).
    async fn admin_search_messages(
        &self,
        sender: Option<&str>,
        query: Option<&str>,
        limit: u32,
    ) -> Result<Vec<AdminMessageRecord>, HostError> {
        let _ = (sender, query, limit);
        Err(HostError::NotSupported("admin_search_messages".into()))
    }

    // ── Access policy ────────────────────────────────────────────────────────────

    /// Return the current access policy.
    ///
    /// Returns `HostError::NotSupported` in minimal implementations.
    async fn admin_get_access_policy(&self) -> Result<AdminAccessPolicy, HostError> {
        Err(HostError::NotSupported("admin_get_access_policy".into()))
    }

    /// Enable or disable the verification requirement.
    ///
    /// When `require_verify = false`, newly-registered accounts are treated
    /// as `User` immediately without aide/sysop validation.
    ///
    /// Takes effect immediately and is persisted to `config.toml`.
    async fn admin_set_require_verify(&self, require_verify: bool) -> Result<(), HostError> {
        let _ = require_verify;
        Err(HostError::NotSupported("admin_set_require_verify".into()))
    }

    /// Set or clear the guest room.
    ///
    /// `name = Some("RoomName")` enables the guest room (created if needed).
    /// `name = None` disables the feature.
    ///
    /// Takes effect immediately and is persisted to `config.toml`.
    async fn admin_set_guest_room(&self, name: Option<String>) -> Result<(), HostError> {
        let _ = name;
        Err(HostError::NotSupported("admin_set_guest_room".into()))
    }

    // ── Node location ────────────────────────────────────────────────────────────

    /// Return the configured GPS coordinates for this BBS node, if any.
    ///
    /// The mesh transport calls this on `Connected` and sends `SetAdvertLatlon`
    /// to the radio so the node's location appears in LoRa adverts.
    /// Returns `None` when no location is configured (the radio default is used).
    fn node_location(&self) -> Option<(f64, f64)> {
        None
    }

    /// Update the in-memory GPS location without a restart.
    /// Called by the web admin after saving a `[location]` config change.
    /// The mesh transport reads this on the next reconnect.
    fn set_node_location(&self, _location: Option<(f64, f64)>) {}

    /// Called by the mesh transport when AppStart SelfInfo is received.
    /// Stores the node's public key hex so it can be displayed in the web UI.
    fn set_node_pubkey(&self, _pubkey_hex: String) {}

    /// Return the current node public key hex (set by the mesh transport on connect).
    fn node_pubkey(&self) -> Option<String> {
        None
    }

    /// Register the mesh transport's admin command channel.
    /// Called by MeshTransport on start; the sender is stored so Host methods
    /// can route key operations through the live transport.
    fn register_mesh_key_ops(&self, _sender: tokio::sync::mpsc::Sender<MeshKeyRequest>) {}

    /// Export the device's private key as a 64-char hex string.
    /// Requires the mesh transport to be connected.
    async fn admin_export_node_key(&self) -> Result<String, HostError> {
        Err(HostError::NotSupported("admin_export_node_key".into()))
    }

    /// Import a new private key from a 64-char hex string.
    /// Requires the mesh transport to be connected.
    async fn admin_import_node_key(&self, hex: String) -> Result<(), HostError> {
        let _ = hex;
        Err(HostError::NotSupported("admin_import_node_key".into()))
    }

    /// Apply LoRa radio parameters to the MeshCore companion device.
    /// Requires the mesh transport to be connected.
    async fn admin_apply_mesh_radio(&self, params: MeshRadioParams) -> Result<(), HostError> {
        let _ = params;
        Err(HostError::NotSupported("admin_apply_mesh_radio".into()))
    }

    /// Register the Meshtastic transport's admin command channel.
    fn register_meshtastic_admin_ops(
        &self,
        _sender: tokio::sync::mpsc::Sender<MeshtasticAdminRequest>,
    ) {
    }

    /// Fetch the current LoRa radio config from the Meshtastic device.
    async fn admin_get_meshtastic_lora(&self) -> Result<MeshtasticLoRaConfig, HostError> {
        Err(HostError::NotSupported("admin_get_meshtastic_lora".into()))
    }

    /// Push a new LoRa radio config to the Meshtastic device.
    async fn admin_set_meshtastic_lora(
        &self,
        config: MeshtasticLoRaConfig,
    ) -> Result<(), HostError> {
        let _ = config;
        Err(HostError::NotSupported("admin_set_meshtastic_lora".into()))
    }

    /// Fetch the Meshtastic device's owner/user info (long name, short name,
    /// public key).
    async fn admin_get_meshtastic_owner(&self) -> Result<MeshtasticOwnerInfo, HostError> {
        Err(HostError::NotSupported("admin_get_meshtastic_owner".into()))
    }

    /// Update the Meshtastic device's owner/user info.
    ///
    /// Pass `None` for any field that should not be changed.  The implementation
    /// fetches the current owner first, merges the new values, then writes back.
    async fn admin_set_meshtastic_owner(
        &self,
        long_name: Option<String>,
        short_name: Option<String>,
    ) -> Result<(), HostError> {
        let _ = (long_name, short_name);
        Err(HostError::NotSupported("admin_set_meshtastic_owner".into()))
    }

    /// Fetch the Meshtastic device's security / PKC configuration.
    async fn admin_get_meshtastic_security(&self) -> Result<MeshtasticSecurityInfo, HostError> {
        Err(HostError::NotSupported(
            "admin_get_meshtastic_security".into(),
        ))
    }

    /// Fetch a combined snapshot of the Meshtastic device settings from the
    /// cache captured during the last connect-time sync (no device round-trip).
    async fn admin_get_meshtastic_snapshot(
        &self,
    ) -> Result<crate::admin::MeshtasticDeviceSnapshot, HostError> {
        Err(HostError::NotSupported(
            "admin_get_meshtastic_snapshot".into(),
        ))
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
