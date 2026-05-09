//! # bbs-plugin-api
//!
//! The contract every Supply Drop BBS plugin compiles against.
//! This crate is intentionally small: it defines traits and shared
//! types, nothing else. Plugin authors depend on this crate (and
//! optionally `bbs-core` for domain types) and implement the
//! [`Plugin`] trait plus any capability traits the plugin uses.
//!
//! ## Module map
//!
//! - [`identity`]    — `SessionId`, `Username`
//! - [`permissions`] — `PermissionLevel`, `PermissionCtx`
//! - [`plugin`]      — the `Plugin` trait and lifecycle
//! - [`host`]        — the `Host` trait (implemented by `bbs-core`)
//! - [`transport`]   — the `TransportEngine` capability trait
//! - [`event`]       — `DomainEvent`, `Notification`
//! - [`command`]     — `Command`, `Response` (still placeholders;
//!   these grow with feature work)
//! - [`error`]       — `PluginError`, `HostError`, `TransportError`
//! - [`testing`]     — fake `Host` for plugin unit tests
//!
//! ## Status
//!
//! Pre-1.0. Trait shapes will evolve. See
//! [`docs/PLUGIN_API.md`](https://github.com/Mesh-America/supply-drop-bbs/blob/main/docs/PLUGIN_API.md)
//! for the prose introduction and
//! [`docs/adr/0011-transport-protocol-agnostic-core.md`](https://github.com/Mesh-America/supply-drop-bbs/blob/main/docs/adr/0011-transport-protocol-agnostic-core.md)
//! for the protocol-agnosticism rules.

pub mod admin;
pub mod advert;
pub mod command;
pub mod error;
pub mod event;
pub mod host;
pub mod identity;
pub mod permissions;
pub mod plugin;
pub mod testing;
pub mod transport;

// ── Re-exports of the most commonly used items ───────────────────
//
// Plugin authors should be able to write
//
//     use bbs_plugin_api::{Plugin, Host, SessionId, ...};
//
// without spelunking the module tree for every type.

pub use admin::{
    AdminAuditEntry, AdminBackupRecord, AdminDailyVolume, AdminMessageRecord, AdminReports,
    AdminRoomSummary, AdminSessionInfo, AdminStaleRoom, AdminStats, AdminTopRoom, AdminTopSender,
    AdminUserInfo,
};
pub use advert::{AdvertBus, AdvertRecord};
pub use command::{Command, Response};
pub use error::{HostError, PluginError, TransportError};
pub use event::{DomainEvent, MessageRecipient, Notification, NotifyOutcome};
pub use host::Host;
pub use identity::{SessionId, Username};
pub use permissions::{PermissionCtx, PermissionLevel};
pub use plugin::Plugin;
pub use transport::TransportEngine;
