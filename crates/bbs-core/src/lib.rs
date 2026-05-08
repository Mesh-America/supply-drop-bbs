//! # bbs-core
//!
//! The domain heart of Supply Drop BBS. This commit lays down the
//! pure-data foundation; persistence and business logic land in
//! subsequent commits.
//!
//! ## Module map
//!
//! - [`ids`]       — `UserId`, `RoomId`, `MessageId` newtypes
//! - [`timestamp`] — `Timestamp` (UTC-only, RFC 3339 wire format)
//! - [`user`]      — `User`, `UserStatus`, display-name validation
//! - [`room`]      — `Room`, name + description validation,
//!   linked-list neighbour fields
//! - [`message`]   — `Message`, content validation
//!
//! ## Boundaries this crate respects
//!
//! Per [ADR-0011](https://github.com/Mesh-America/supply-drop-bbs/blob/main/docs/adr/0011-transport-protocol-agnostic-core.md):
//!
//! - No protocol-specific identifiers on domain types. A
//!   `User` doesn't carry a `mesh_node_id` or a `meshtastic_node_id`
//!   field; those mappings live in the relevant transport plugin.
//! - All timestamps are UTC at storage. Local-time conversion
//!   happens at the rendering boundary (operator UI), not here.
//!
//! ## Status
//!
//! Domain types + persistence layer. Command processing and the
//! concrete `Host` implementation come in subsequent commits.

pub mod db;
pub mod ids;
pub mod message;
pub mod room;
pub mod timestamp;
pub mod user;

// ── Re-exports of the most-used items ────────────────────────────

pub use db::{Database, DbOpenError, MessagePage, MessageStore, RoomStore, StoreError, UserStore};
pub use ids::{MessageId, RoomId, UserId};
pub use message::{InvalidMessageContent, Message};
pub use room::{InvalidRoomDescription, InvalidRoomName, Room};
pub use timestamp::{ParseTimestampError, Timestamp};
pub use user::{InvalidDisplayName, User, UserStatus};
