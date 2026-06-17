//! # bbs-mesh
//!
//! The MeshCore transport plugin for Supply Drop BBS.  Connects to
//! `pymc_core`'s `CompanionFrameServer` over TCP and translates between
//! MeshCore direct messages and the BBS `Command` / `Response` types
//! defined in `bbs_plugin_api`.
//!
//! ## Architecture overview
//!
//! ```text
//! ┌──────────────────────────────────────────────────────────────┐
//! │  pymc_core CompanionFrameServer  (radio bridge, TCP :5000)   │
//! └──────────────────────┬───────────────────────────────────────┘
//!                        │ companion-frame TCP protocol
//! ┌──────────────────────▼───────────────────────────────────────┐
//! │  meshcore-companion  CompanionClient                         │
//! │  (frame codec + reconnecting TCP client)                     │
//! └──────────────────────┬───────────────────────────────────────┘
//!                        │ ClientEvent channel
//! ┌──────────────────────▼───────────────────────────────────────┐
//! │  bbs-mesh  MeshTransport                                     │
//! │  ┌─────────────────────────────────────────────────────────┐ │
//! │  │  event_loop task                                        │ │
//! │  │  ContactMsgRecv → parse_command → host.process_command  │ │
//! │  │  → format_response → SendTxtMsg                         │ │
//! │  └─────────────────────────────────────────────────────────┘ │
//! │  notify() → render_notification → SendTxtMsg                 │
//! └──────────────────────┬───────────────────────────────────────┘
//!                        │ Host trait
//! ┌──────────────────────▼───────────────────────────────────────┐
//! │  bbs-core  (domain logic, persistence)                       │
//! └──────────────────────────────────────────────────────────────┘
//! ```
//!
//! ## Identity mapping
//!
//! Per [ADR-0011], the mapping between MeshCore public-key prefixes and BBS
//! session IDs lives entirely within this crate's [`SessionState`].  No
//! MeshCore-specific identifiers appear in `bbs-core`'s schema.
//!
//! [ADR-0011]: https://github.com/Mesh-America/supply-drop-bbs/blob/main/docs/adr/0011-transport-protocol-agnostic-core.md
//!
//! ## Configuration
//!
//! See [`MeshConfig`] for all available options and their defaults.

// Suppress missing-docs for this internal crate until the API stabilises.
#![allow(missing_docs)]

pub mod command;
pub mod config;
pub mod metrics;
mod send_tracker;
pub mod session;
pub mod transport;

pub use config::{ConnectionType, MeshConfig};
pub use metrics::{DeliveryStats, DeliveryStatsSnapshot};
pub use session::{SessionEntry, SessionState};
pub use transport::MeshTransport;
