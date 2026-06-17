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

/// Radio parameter configuration stored in `[plugins.mesh.radio]`.
///
/// These values are **not** pushed to the device automatically on connect.
/// Apply them explicitly via `supply-drop-bbs node set-radio` or during
/// the setup wizard. Once applied the device (T114, Heltec V3, etc.)
/// persists the settings in its own flash.
///
/// Either specify a named `preset` (which sets all parameters at once) or
/// supply individual fields. Individual fields take precedence over the preset.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RadioConfig {
    /// Named region preset (e.g. `"USA/Canada"`).
    ///
    /// Run `supply-drop-bbs node set-radio --list-presets` to see all names.
    #[serde(default)]
    pub preset: Option<String>,

    /// Carrier frequency in Hz (e.g. `910_525_000` for 910.525 MHz).
    ///
    /// Overrides the preset value when set.
    #[serde(default)]
    pub frequency_hz: Option<u64>,

    /// Channel bandwidth in Hz (e.g. `62_500` for 62.5 kHz).
    ///
    /// Overrides the preset value when set.
    #[serde(default)]
    pub bandwidth_hz: Option<u32>,

    /// LoRa spreading factor (7–12). Overrides the preset value when set.
    #[serde(default)]
    pub spreading_factor: Option<u8>,

    /// LoRa coding rate denominator (5–8, representing 4/5 through 4/8).
    ///
    /// Overrides the preset value when set.
    #[serde(default)]
    pub coding_rate: Option<u8>,

    /// Transmit power in dBm. Overrides the preset value when set.
    #[serde(default)]
    pub tx_power_dbm: Option<i32>,
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
    /// Set to `false` to disable the MeshCore transport at runtime without
    /// removing the config section.  Defaults to `true`.
    #[serde(default = "default_true")]
    pub enabled: bool,

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
    /// Message sent to a node the first time it contacts the BBS.
    ///
    /// Defaults to a standard welcome prompt. Set to an empty string to disable.
    #[serde(default = "default_welcome")]
    pub welcome_message: String,

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

    /// How many days a stored node credential remains valid.
    ///
    /// After this many days without a successful login the binding expires
    /// and the node must re-authenticate with a password.  Set to `0` to
    /// disable persistent node credentials entirely.
    #[serde(default = "default_node_credential_ttl_days")]
    pub node_credential_ttl_days: u32,

    /// Reset a node's stored path immediately after sending it a message,
    /// so that the next outbound message (e.g. a mail notification) is
    /// delivered via flood rather than a potentially-stale direct path.
    ///
    /// Flood mode rebroadcasts hop-by-hop across the mesh and reaches the
    /// destination regardless of whether the BBS's stored route is still
    /// valid.  Disabling this restores the previous direct-path-only
    /// behaviour.  Defaults to `true`.
    #[serde(default = "default_flood_after_send")]
    pub flood_after_send: bool,

    /// Total transmissions for an outbound reply, including the first.
    ///
    /// Defaults to `1` (retransmission disabled — record-and-forget). When set
    /// greater than `1`, the transport tracks each reply's delivery (via the
    /// device's `RESP_CODE_SENT` CRC and `PUSH_CODE_SEND_CONFIRMED`) and
    /// retransmits — up to this many attempts — if no end-to-end confirmation
    /// arrives before the device's timeout hint. On a multi-hop mesh the return
    /// path is lossy, so a reply (or its ACK) can be dropped and the BBS appears
    /// unresponsive; retransmission recovers those cases on links that actually
    /// confirm delivery.
    ///
    /// ⚠️ Only raise this above `1` on a link whose confirm rate is non-zero
    /// (check the mesh "link health" metrics first). Retransmission depends on
    /// the radio returning `PUSH_CODE_SEND_CONFIRMED`; a link that never does —
    /// some multi-hop / bridge setups never surface one — cannot tell a
    /// delivered reply from a lost one, so it retransmits *every* reply to
    /// exhaustion, duplicating it `reply_max_attempts` times. That is why the
    /// default is `1`.
    ///
    /// Even on a healthy link delivery is at-least-once: a confirmation lost on
    /// the return path can produce one duplicate reply — preferable to silence,
    /// and inbound commands are deduplicated separately.
    #[serde(default = "default_reply_max_attempts")]
    pub reply_max_attempts: u8,

    /// Radio parameter configuration.
    ///
    /// Stored here for reference and applied on demand via
    /// `supply-drop-bbs node set-radio`.  **Not** pushed automatically on
    /// every connect — the device persists radio settings in its own flash.
    ///
    /// Example:
    /// ```toml
    /// [plugins.mesh.radio]
    /// preset = "USA/Canada"
    /// ```
    #[serde(default)]
    pub radio: Option<RadioConfig>,
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
            enabled: true,
            connection_type: ConnectionType::default(),
            addr: default_addr(),
            serial_port: None,
            baud_rate: default_baud_rate(),
            welcome_message: default_welcome(),
            command_prefix: None,
            app_target_version: default_app_ver(),
            reconnect_delay_initial_ms: default_reconnect_initial_ms(),
            reconnect_delay_max_ms: default_reconnect_max_ms(),
            node_credential_ttl_days: default_node_credential_ttl_days(),
            flood_after_send: default_flood_after_send(),
            reply_max_attempts: default_reply_max_attempts(),
            radio: None,
        }
    }
}

fn default_addr() -> SocketAddr {
    "127.0.0.1:5000"
        .parse()
        .expect("hard-coded address is valid")
}

fn default_baud_rate() -> u32 {
    115_200
}

fn default_app_ver() -> u8 {
    APP_TARGET_VER_V3
}

fn default_welcome() -> String {
    "Welcome to Supply Drop BBS!\nType 'register <username>' to create an account\nor 'login <username>' if you already have one.\nType 'H' for a list of commands.".into()
}

fn default_reconnect_initial_ms() -> u64 {
    1_000
}

fn default_reconnect_max_ms() -> u64 {
    60_000
}

fn default_node_credential_ttl_days() -> u32 {
    14
}

fn default_flood_after_send() -> bool {
    true
}

fn default_reply_max_attempts() -> u8 {
    1
}

fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reply retransmission is opt-in: the default must stay `1` so a link that
    /// never returns an end-to-end delivery confirmation can't duplicate every
    /// reply (see the `reply_max_attempts` field docs). Regression guard.
    #[test]
    fn reply_retransmission_is_off_by_default() {
        assert_eq!(MeshConfig::default().reply_max_attempts, 1);

        // An omitted key must resolve to the off default via the serde wiring.
        let cfg: MeshConfig = serde_json::from_str("{}").unwrap();
        assert_eq!(cfg.reply_max_attempts, 1);
    }
}
