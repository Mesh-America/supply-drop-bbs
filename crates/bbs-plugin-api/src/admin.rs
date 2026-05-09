//! Admin-layer data transfer types.
//!
//! These are flat DTOs returned by the admin methods on [`Host`](crate::Host)
//! and serialised by the web admin plugin. Keeping them here (rather than in
//! `bbs-core`) lets any plugin call admin methods via `Arc<dyn Host>` without
//! taking a dependency on `bbs-core`'s internal types.

use serde::{Deserialize, Serialize};

/// A live BBS session as seen by the admin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminSessionInfo {
    /// The session's numeric ID.
    pub session_id: u64,
    /// The transport that created this session (e.g. `"mesh"`, `"cli"`).
    pub transport: String,
    /// The BBS username bound to this session, or `None` for pre-auth.
    pub username: Option<String>,
    /// The caller's current permission level as the raw `u8` discriminant.
    pub permission_level: u8,
}

/// A BBS user account as seen by the admin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminUserInfo {
    /// Stable row ID.
    pub id: i64,
    /// Login username.
    pub username: String,
    /// Optional display name.
    pub display_name: Option<String>,
    /// Lifecycle status: `"active"`, `"banned"`, or `"deleted"`.
    pub status: String,
    /// Permission level as `u8` discriminant (0/10/50/100).
    pub permission_level: u8,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
    /// RFC 3339 last login timestamp, or `None` if never logged in.
    pub last_login_at: Option<String>,
}

/// A BBS room with message count, as seen by the admin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminRoomSummary {
    /// Stable row ID.
    pub id: i64,
    /// Short room name.
    pub name: String,
    /// Optional description.
    pub description: Option<String>,
    /// Whether the room is read-only for non-sysops.
    pub read_only: bool,
    /// Minimum permission level to access this room (`u8` discriminant).
    pub min_permission_level: u8,
    /// Total number of messages posted to this room.
    pub message_count: i64,
    /// RFC 3339 creation timestamp.
    pub created_at: String,
}

/// A message as seen by the admin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminMessageRecord {
    /// Stable message ID.
    pub id: i64,
    /// Username of the sender.
    pub sender: String,
    /// DM recipient username, or `None` for room posts.
    pub recipient: Option<String>,
    /// Message content (may be truncated in list views).
    pub content: String,
    /// RFC 3339 post timestamp.
    pub timestamp: String,
}

/// Aggregate BBS statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminStats {
    /// Users with `status = Active` and `permission_level > 0`.
    pub active_users: i64,
    /// Users with `status = Active` and `permission_level = 0` (pending validation).
    pub pending_users: i64,
    /// Users with `status = Banned`.
    pub banned_users: i64,
    /// Total message rows (room + DM combined).
    pub total_messages: i64,
    /// Total room rows.
    pub total_rooms: i64,
    /// Count of currently live BBS sessions.
    pub active_sessions: usize,
}

/// A database backup file record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminBackupRecord {
    /// File name only (not a full path).
    pub filename: String,
    /// File size in bytes.
    pub size_bytes: u64,
    /// RFC 3339 file modification timestamp.
    pub created_at: String,
}
