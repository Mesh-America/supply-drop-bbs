//! Configuration for the MeshCore transport plugin.
//!
//! Deserialized from the `[plugins.mesh]` section of the operator's
//! TOML config file.  All fields have sensible defaults so an
//! operator running `pymc_core` on the same machine with default
//! settings needs zero configuration.
//!
//! # Connection types
//!
//! | `connection_type` | Transport        | pymc_core needed? |
//! |-------------------|------------------|-------------------|
//! | `tcp`             | TCP socket       | yes (default)     |
//! | `hat`             | TCP socket       | yes (Pi HAT)      |
//! | `serial`          | USB serial port  | no                |
//!
//! Both `tcp` and `hat` connect to a `CompanionFrameServer` over TCP.
//! `hat` is operationally identical to `tcp` at the BBS level; the
//! distinction is that the setup wizard offers Pi HAT GPIO / SPI setup
//! only for `hat`.  `serial` bypasses `pymc_core` entirely and speaks
//! the companion-frame protocol directly to the USB device.

use std::{net::SocketAddr, time::Duration};

use meshcore_companion::constants::APP_TARGET_VER_V3;
use serde::{Deserialize, Serialize};

/// How the mesh transport connects to the radio device.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionType {
    /// Connect via TCP to a `pymc_core` `CompanionFrameServer`.
    /// The default — works for standalone Pi + USB radio setups managed
    /// by `pymc_core`, or for any networked bridge.
    #[default]
    Tcp,

    /// Connect via TCP to a `pymc_core` `CompanionFrameServer` that
    /// manages a Pi HAT radio (GPIO / SPI).  Operationally identical to
    /// `tcp` at the BBS level; the setup wizard uses this to offer HAT-
    /// specific configuration (pin presets, UART setup, service install).
    Hat,

    /// Connect directly to a USB companion device (e.g. Heltec V3,
    /// T-Beam) via a local serial port.  `pymc_core` is not required;
    /// the BBS speaks the companion-frame protocol directly.  See
    /// ADR-0013 for the rationale.
    Serial,
}

/// Configuration for [`MeshTransport`](crate::MeshTransport).
///
/// # Minimal TOML examples
///
/// TCP (pymc_core on the same host, default port):
/// ```toml
/// [plugins.mesh]
/// # Nothing required — defaults connect to 127.0.0.1:5000
/// ```
///
/// USB serial (no pymc_core):
/// ```toml
/// [plugins.mesh]
/// connection_type = "serial"
/// serial_port     = "/dev/ttyACM0"
/// ```
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MeshConfig {
    /// How to reach the radio.
    #[serde(default)]
    pub connection_type: ConnectionType,

    // ── TCP / HAT fields ──────────────────────────────────────────────

    /// Address of the `CompanionFrameServer` TCP listener.
    ///
    /// Used when `connection_type` is `tcp` or `hat`.
    /// Defaults to `127.0.0.1:5000`, which is the `pymc_core` default
    /// when both processes run on the same host.
    #[serde(default = "default_addr")]
    pub addr: SocketAddr,

    // ── Serial fields ─────────────────────────────────────────────────

    /// OS path to the USB serial device.
    ///
    /// Required when `connection_type = "serial"`.
    /// Examples: `/dev/ttyACM0` (Linux), `COM3` (Windows).
    ///
    /// When `None` and `connection_type = "serial"`, the BBS will fail
    /// at startup with a clear error message.
    #[serde(default)]
    pub serial_port: Option<String>,

    /// Baud rate for the serial connection.
    ///
    /// MeshCore USB companion devices default to 115 200.
    #[serde(default = "default_baud_rate")]
    pub baud_rate: u32,

    // ── Common fields ─────────────────────────────────────────────────

    /// Optional single-character prefix that marks a message as a BBS command.
    ///
    /// When set, only messages beginning with this character are interpreted
    /// as commands; all others continue a multi-step workflow (registration,
    /// login, etc.).
    ///
    /// When `None` (the default) every direct message is a potential command.
    ///
    /// Example: `"!"` — users send `!help`, `!rooms`, etc.
    #[serde(default)]
    pub command_prefix: Option<char>,

    /// MeshCore companion-frame protocol version to request in the AppStart
    /// handshake.
    ///
    /// Defaults to [`APP_TARGET_VER_V3`].  Lower this only if you know the
    /// device does not support v3.
    #[serde(default = "default_app_ver")]
    pub app_target_version: u8,

    /// Initial backoff before the first reconnect / reopen attempt after a
    /// disconnect, in milliseconds.  Doubles on each successive failure up to
    /// [`reconnect_delay_max_ms`](Self::reconnect_delay_max_ms).
    #[serde(default = "default_reconnect_initial_ms")]
    pub reconnect_delay_initial_ms: u64,

    /// Maximum reconnect / reopen backoff, in milliseconds.
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
            connection_type: ConnectionType::default(),
            addr: default_addr(),
            serial_port: None,
            baud_rate: default_baud_rate(),
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

fn default_baud_rate() -> u32 {
    115_200
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
