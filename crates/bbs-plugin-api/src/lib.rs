//! # bbs-plugin-api
//!
//! The contract every Supply Drop BBS plugin compiles against.
//! This crate is intentionally small: it defines traits and shared
//! types, nothing else. Plugin authors depend on this crate (and
//! optionally `bbs-core` for domain types) and implement the
//! `Plugin` trait plus any capability traits the plugin uses.
//!
//! ## What's here
//!
//! - **`Plugin`** — the base trait every plugin implements.
//! - **`Host`** — the trait that `bbs-core` implements; plugins
//!   call into it to drive the BBS.
//! - **Capability traits** — `TransportEngine`, `RouteContributor`,
//!   `StaticFileMount`, `ScheduledTask`, `EventConsumer`,
//!   `MetricsContributor`, `HealthCheck`. A plugin opts into any
//!   combination.
//! - **Shared types** — `SessionId`, `Notification`, `DomainEvent`,
//!   `PluginError`, etc.
//!
//! ## What's NOT here
//!
//! - Concrete logic. The `Host` *implementation* lives in
//!   `bbs-core`. The plugin *implementations* live in their own
//!   crates.
//! - Protocol-specific types. Transport-specific identifiers
//!   (MeshCore public keys, Meshtastic node IDs, IRC nicks) live
//!   in their respective transport plugins, not here. See
//!   [ADR-0011](https://github.com/Mesh-America/supply-drop-bbs/blob/main/docs/adr/0011-transport-protocol-agnostic-core.md).
//!
//! ## Status
//!
//! Placeholder. Real traits land in subsequent commits. The shape
//! sketches in `docs/PLUGIN_API.md` are the design target.

/// Internal placeholder so the crate has at least one item to
/// compile. Removed when real traits land.
pub fn placeholder() {}
