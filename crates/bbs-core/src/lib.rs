//! # bbs-core
//!
//! The domain heart of Supply Drop BBS. This crate owns:
//!
//! - **Domain types** — `User`, `Room`, `Message`, `Session`,
//!   `Workflow`, `PermissionLevel`, and the newtypes that wrap
//!   their identifiers. No protocol-specific fields. See
//!   [ADR-0011](https://github.com/Mesh-America/supply-drop-bbs/blob/main/docs/adr/0011-transport-protocol-agnostic-core.md).
//! - **Persistence** — `sqlx`-backed access to a single SQLite
//!   database in WAL mode. Compile-time-checked queries; no raw
//!   SQL escapes from this crate. See
//!   [ADR-0005](https://github.com/Mesh-America/supply-drop-bbs/blob/main/docs/adr/0005-db-strategy.md).
//! - **Business logic** — command processing, permission checks,
//!   workflow state-machine transitions, audit-log writes.
//! - **The `Host` implementation** — the concrete type that
//!   transport plugins drive the BBS through. The `Host` *trait*
//!   lives in `bbs-plugin-api`; the impl lives here.
//!
//! ## Boundaries
//!
//! Things this crate does NOT do:
//!
//! - I/O concerns beyond the DB. No HTTP, no sockets, no radio.
//! - Transport-specific identifiers. Mapping a MeshCore public key
//!   to a username happens in `bbs-mesh`, not here.
//! - Plugin lifecycle management. That's the host binary's job.
//!
//! ## Status
//!
//! Placeholder. Real types land in subsequent commits.

/// Internal placeholder so the crate has at least one item to
/// compile. Removed when real types land.
pub fn placeholder() {}
