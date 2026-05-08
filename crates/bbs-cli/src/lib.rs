//! # bbs-cli
//!
//! The CLI transport plugin for Supply Drop BBS. Listens on a
//! Unix-domain socket, accepts connections from local CLI clients,
//! and translates between the line-based CLI protocol and the
//! BBS-core's `Command` / `Response` types.
//!
//! Used for:
//!
//! - Sysop administration scripts that don't want to go through
//!   the web UI
//! - Local development against the BBS without a mesh radio
//! - Smoke tests in CI (stand up a BBS, talk to it through the
//!   CLI socket, verify responses)
//!
//! Default-on (cargo feature `transport-cli`).
//!
//! ## Status
//!
//! Placeholder. Real implementation lands in subsequent commits.

/// Internal placeholder so the crate has at least one item to
/// compile. Removed when real types land.
pub fn placeholder() {}
