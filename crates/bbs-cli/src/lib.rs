//! # bbs-cli
//!
//! The CLI transport plugin for Supply Drop BBS. Listens on a
//! Unix-domain socket, accepts connections from local CLI clients,
//! and translates between the line-based CLI protocol and the
//! BBS-core's `Command` / `Response` types.
//!
//! Default-on (cargo feature `transport-cli`).
//!
//! ## Status
//!
//! Placeholder. Real implementation lands in subsequent commits.

#![allow(missing_docs)]

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Configuration for the CLI transport plugin.
///
/// Deserialized from `[plugins.cli]` in the operator's TOML config.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CliConfig {
    /// Whether to start the CLI listener. Set `false` to disable at
    /// runtime without recompiling.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// Unix socket path. Defaults to `<data_dir>/cli.sock` (resolved
    /// at startup once `data_dir` is known).
    #[serde(default)]
    pub socket: Option<PathBuf>,

    /// Octal permission mode of the socket file (e.g. `"0600"`).
    #[serde(default = "default_socket_mode")]
    pub socket_mode: String,

    /// Username or UID to `chown` the socket to after creation.
    /// Defaults to the BBS process user.
    #[serde(default)]
    pub socket_owner: Option<String>,
}

impl Default for CliConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            socket: None,
            socket_mode: default_socket_mode(),
            socket_owner: None,
        }
    }
}

fn default_enabled() -> bool {
    true
}

fn default_socket_mode() -> String {
    "0600".to_owned()
}
