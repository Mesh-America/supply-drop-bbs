//! # bbs-process-transport
//!
//! Externally-spawned transport plugin support for Supply Drop BBS.
//!
//! Operators can run any executable as a BBS transport: Supply Drop spawns it,
//! speaks a simple JSON IPC protocol over stdin/stdout, and bridges the result
//! to the standard `Command`/`Response`/`Notification` model.
//!
//! ## Quick start
//!
//! In `config.toml`:
//!
//! ```toml
//! [[plugins.process]]
//! name    = "my-transport"
//! command = "/usr/local/bin/my-transport"
//! args    = ["--port", "2323"]
//! enabled = true
//! ```
//!
//! The executable must speak the IPC protocol documented in [`ipc`].
//!
//! ## Modules
//!
//! - [`ipc`] — wire protocol types (`PluginMsg`, `HostMsg`)
//! - [`transport`] — [`ProcessTransport`] (implements `Plugin + TransportEngine`)
//! - [`manager`] — [`ProcessPluginManager`] (implements `PluginRegistryApi`)

#![warn(missing_docs)]

pub mod ipc;
pub mod manager;
pub mod transport;

pub use ipc::{HostMsg, PluginMsg};
pub use manager::ProcessPluginManager;
pub use transport::ProcessTransport;
