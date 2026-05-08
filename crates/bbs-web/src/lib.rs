//! # bbs-web
//!
//! The web admin UI plugin for Supply Drop BBS. Serves an HTTP
//! admin interface for the BBS sysop. Built with `axum`. Embeds a
//! Vue 3 frontend into the binary via `rust-embed`. Speaks a JSON
//! API documented as OpenAPI (generated at build time via `utoipa`).
//!
//! See [ADR-0003](https://github.com/Mesh-America/supply-drop-bbs/blob/main/docs/adr/0003-web-ui-as-plugin.md)
//! for why this is structured as a plugin rather than a first-class
//! feature of the host.
//!
//! ## What this plugin is for
//!
//! - Sysop maintenance: managing users, rooms, messages, backups
//! - System observability: health, metrics, logs, audit trail
//! - Anything the operator wants to do without driving it through
//!   mesh DMs or the CLI socket
//!
//! ## What this plugin is NOT for
//!
//! - End-user message reading. Mesh users use the mesh.
//! - Public-facing service. Default bind is `127.0.0.1`; remote
//!   access is via a TLS-terminating reverse proxy.
//! - Always-on. Default OFF: must be enabled with the `admin-web`
//!   cargo feature, then enabled in config.
//!
//! ## Status
//!
//! Placeholder. Real implementation lands in subsequent commits.

/// Internal placeholder so the crate has at least one item to
/// compile. Removed when real types land.
pub fn placeholder() {}
