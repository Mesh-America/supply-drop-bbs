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
    /// Whether this room can be deleted. False for the five built-in system
    /// rooms (Lobby, Mail, Aides, Sysop, System).
    pub deletable: bool,
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

/// One entry in the top-senders report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminTopSender {
    /// BBS username of the sender.
    pub username: String,
    /// Total messages sent by this user.
    pub message_count: i64,
}

/// One entry in the top-rooms report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminTopRoom {
    /// Stable room row ID.
    pub room_id: i64,
    /// Room name.
    pub room_name: String,
    /// Total messages posted to this room.
    pub message_count: i64,
}

/// Message count for a single calendar day (YYYY-MM-DD).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminDailyVolume {
    /// Calendar day in `YYYY-MM-DD` format.
    pub day: String,
    /// Number of messages posted on this day.
    pub count: i64,
}

/// A room that has had no messages recently (or ever).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminStaleRoom {
    /// Stable room row ID.
    pub room_id: i64,
    /// Room name.
    pub room_name: String,
    /// RFC 3339 timestamp of the last message, or `None` if the room is empty.
    pub last_message_at: Option<String>,
}

/// Bundled analytics returned by [`crate::host::Host::admin_reports`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminReports {
    /// Top 10 users by total message count.
    pub top_senders: Vec<AdminTopSender>,
    /// Top 10 rooms by total message count.
    pub top_rooms: Vec<AdminTopRoom>,
    /// Daily message counts for the last 30 days (ascending).
    pub daily_volume: Vec<AdminDailyVolume>,
    /// Rooms with no messages in the last 30 days (or ever), oldest-first.
    pub stale_rooms: Vec<AdminStaleRoom>,
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
    /// Config file saved alongside this database backup, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_filename: Option<String>,
    /// Size of the config file in bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_size_bytes: Option<u64>,
}
