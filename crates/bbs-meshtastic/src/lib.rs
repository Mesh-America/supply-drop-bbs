//! Meshtastic transport plugin for Supply Drop BBS.
//!
//! This crate is a **protocol stub**.  The config types, feature flag, setup
//! wizard, and installer are all wired up so that MeshCore and Meshtastic are
//! treated as independent, co-equal transports.  The Meshtastic radio codec
//! (protobuf over serial/TCP) has not been implemented yet — starting this
//! transport logs a clear error and exits.
//!
//! When the codec is ready, replace the `init` / `start` / `stop` bodies in
//! [`MeshtasticTransport`] with the real implementation.  Everything else
//! (config, feature gate, wizard, installer) stays the same.

use std::net::SocketAddr;

use async_trait::async_trait;
use bbs_plugin_api::{Host, Plugin, PluginError};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

// ── Connection type ───────────────────────────────────────────────────────────

/// How the Meshtastic transport connects to the radio device.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MeshtasticConnectionType {
    /// Connect directly to a USB radio (Heltec V3, T-Beam, RAK4631, etc.)
    /// running Meshtastic firmware via serial port.
    #[default]
    Serial,

    /// Connect via TCP to a running `meshtasticd` instance or any Meshtastic
    /// node that exposes a TCP stream (default port 4403).
    Tcp,
}

// ── Config ────────────────────────────────────────────────────────────────────

/// Configuration for the Meshtastic transport (`[plugins.meshtastic]`).
///
/// # Minimal TOML examples
///
/// USB serial (most common):
/// ```toml
/// [plugins.meshtastic]
/// connection_type = "serial"
/// serial_port     = "/dev/ttyACM0"
/// ```
///
/// TCP to meshtasticd:
/// ```toml
/// [plugins.meshtastic]
/// connection_type = "tcp"
/// addr            = "127.0.0.1:4403"
/// ```
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MeshtasticConfig {
    /// Set to `false` to disable this transport without removing the section.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// How to connect to the radio.
    #[serde(default)]
    pub connection_type: MeshtasticConnectionType,

    /// OS path to the USB serial device.
    /// Required when `connection_type = "serial"`.
    /// Examples: `/dev/ttyACM0` (Linux), `COM3` (Windows).
    #[serde(default)]
    pub serial_port: Option<String>,

    /// Baud rate for the serial connection. Meshtastic defaults to 115 200.
    #[serde(default = "default_baud_rate")]
    pub baud_rate: u32,

    /// Address of a `meshtasticd` TCP listener.
    /// Used when `connection_type = "tcp"`. Defaults to `127.0.0.1:4403`.
    #[serde(default = "default_addr")]
    pub addr: SocketAddr,

    /// Optional single-character prefix that marks a message as a BBS command.
    /// When `None` every direct message is treated as a potential command.
    #[serde(default)]
    pub command_prefix: Option<char>,
}

impl Default for MeshtasticConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            connection_type: MeshtasticConnectionType::default(),
            serial_port: None,
            baud_rate: default_baud_rate(),
            addr: default_addr(),
            command_prefix: None,
        }
    }
}

fn default_true() -> bool {
    true
}
fn default_baud_rate() -> u32 {
    115_200
}
fn default_addr() -> SocketAddr {
    "127.0.0.1:4403"
        .parse()
        .expect("hard-coded address is valid")
}

// ── Transport ─────────────────────────────────────────────────────────────────

/// Meshtastic transport plugin handle.
///
/// Currently a stub — the Meshtastic protobuf codec is not yet implemented.
/// Starting this transport will log an error and exit.
pub struct MeshtasticTransport {
    _config: MeshtasticConfig,
    _host: Arc<dyn Host>,
}

#[async_trait]
impl Plugin for MeshtasticTransport {
    fn name(&self) -> &'static str {
        "meshtastic"
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    async fn init(config: Self::Config, host: Arc<dyn Host>) -> Result<Self, PluginError>
    where
        Self: Sized,
    {
        Ok(Self {
            _config: config,
            _host: host,
        })
    }

    async fn start(&self) -> Result<(), PluginError> {
        Err(PluginError::Other(
            "Meshtastic transport is not yet implemented. \
             The codec (protobuf over serial/TCP) is pending. \
             Disable [plugins.meshtastic] in your config until it is ready."
                .into(),
        ))
    }

    async fn stop(&self) -> Result<(), PluginError> {
        Ok(())
    }

    type Config = MeshtasticConfig;
}
