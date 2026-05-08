//! # bbs-mesh
//!
//! The MeshCore transport plugin for Supply Drop BBS. Connects to
//! `pymc_core`'s `CompanionFrameServer` (the radio bridge process)
//! over TCP using the companion-frame protocol, and translates
//! between MeshCore packets and BBS-core's `Command` / `Response`
//! types.
//!
//! See [docs/PROTOCOL.md](https://github.com/Mesh-America/supply-drop-bbs/blob/main/docs/PROTOCOL.md)
//! for the wire format and command vocabulary.
//!
//! ## Identity mapping
//!
//! Per [ADR-0011](https://github.com/Mesh-America/supply-drop-bbs/blob/main/docs/adr/0011-transport-protocol-agnostic-core.md),
//! the mapping between MeshCore identities (cryptographic public
//! keys) and BBS usernames lives in this crate's own
//! `meshcore_identities` table — not in `bbs-core`'s schema. A
//! sibling transport for Meshtastic would maintain its own
//! analogous table without touching this one.
//!
//! Default-on (cargo feature `transport-mesh`).
//!
//! ## Status
//!
//! Placeholder. Real implementation lands in subsequent commits.

/// Internal placeholder so the crate has at least one item to
/// compile. Removed when real types land.
pub fn placeholder() {}
