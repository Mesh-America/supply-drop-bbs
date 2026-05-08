//! Configuration for the MeshCore transport plugin.
//!
//! Deserialized from the `[plugins.mesh]` section of the operator's
//! TOML config file.  All fields have sensible defaults so an
//! operator running `pymc_core` on the same machine with default
//! settings needs zero configuration.

use std::{net::SocketAddr, time::Duration};

use meshcore_companion::constants::APP_TARGET_VER_V3;
use serde::{Deserialize, Serialize};

/// Configuration for [`MeshTransport`](crate::MeshTransport).
///
/// # Minimal TOML example
///
/// ```toml
/// # No config needed if pymc_core runs on 127.0.0.1:5000.
/// [plugins.mesh]
/// ```
///
/// # Full TOML example
///
/// ```toml
/// [plugins.mesh]
/// addr                     = "192.168.1.10:5000"
/// command_prefix           = "!"
/// reconnect_delay_initial_ms = 2000
/// reconnect_delay_max_ms     = 120000
/// ```
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MeshConfig {
    /// Address of the `CompanionFrameServer` TCP listener.
    ///
    /// Defaults to `127.0.0.1:5000`, which is the `pymc_core` default when
    /// both processes run on the same host (e.g. a Raspberry Pi).
    #[serde(default = "default_addr")]
    pub addr: SocketAddr,

    /// Optional single-character prefix that marks a message as a BBS command.
    ///
    /// When set, only messages that begin with this character are interpreted as
    /// commands; all other messages are treated as workflow replies (continuing a
    /// multi-step flow such as registration or login).
    ///
    /// When `None` (the default) every direct message is a potential command.
    ///
    /// Example: `"!"` — users send `!help`, `!rooms`, etc.
    #[serde(default)]
    pub command_prefix: Option<char>,

    /// MeshCore companion-frame protocol version to request in the AppStart
    /// handshake.
    ///
    /// Defaults to [`APP_TARGET_VER_V3`] (the highest supported version, which
    /// enables per-frame SNR reporting).  Lower this only if you know the radio
    /// bridge does not support v3.
    #[serde(default = "default_app_ver")]
    pub app_target_version: u8,

    /// Initial backoff before the first reconnect attempt after a disconnect,
    /// in milliseconds.  Doubles on each successive failure up to
    /// [`reconnect_delay_max_ms`](Self::reconnect_delay_max_ms).
    #[serde(default = "default_reconnect_initial_ms")]
    pub reconnect_delay_initial_ms: u64,

    /// Maximum reconnect backoff, in milliseconds.
    #[serde(default = "default_reconnect_max_ms")]
    pub reconnect_delay_max_ms: u64,
}

impl MeshConfig {
    /// Return the initial reconnect delay as a [`Duration`].
    pub fn reconnect_delay_initial(&self) -> Duration {
        Duration::from_millis(self.reconnect_delay_initial_ms)
    }

    /// Return the maximum reconnect delay as a [`Duration`].
    pub fn reconnect_delay_max(&self) -> Duration {
        Duration::from_millis(self.reconnect_delay_max_ms)
    }
}

impl Default for MeshConfig {
    fn default() -> Self {
        Self {
            addr: default_addr(),
            command_prefix: None,
            app_target_version: default_app_ver(),
            reconnect_delay_initial_ms: default_reconnect_initial_ms(),
            reconnect_delay_max_ms: default_reconnect_max_ms(),
        }
    }
}

fn default_addr() -> SocketAddr {
    "127.0.0.1:5000".parse().expect("hard-coded address is valid")
}

fn default_app_ver() -> u8 {
    APP_TARGET_VER_V3
}

fn default_reconnect_initial_ms() -> u64 {
    1_000
}

fn default_reconnect_max_ms() -> u64 {
    60_000
}
