//! Meshtastic transport plugin for Supply Drop BBS.
//!
//! Supports both USB serial radios and TCP connections to `meshtasticd`.

mod command;
mod proto;
mod session;
mod stream;

use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use async_trait::async_trait;
use bbs_plugin_api::{
    error::{HostError, PluginError, TransportError},
    event::{DomainEvent, Notification, NotifyOutcome},
    identity::SessionId,
    transport::TransportEngine,
    Host, PermissionLevel, Plugin, Response,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use bbs_plugin_api::MeshtasticAdminRequest;

use crate::{
    command::{format_response, parse_command, render_notification, truncate_utf8},
    proto::{
        admin_get_lora_config, admin_get_owner, admin_get_security_config, admin_get_session_key,
        admin_message, admin_reboot, admin_remove_fixed_position, admin_set_device_config,
        admin_set_fixed_position, admin_set_lora_config, admin_set_owner, admin_set_time,
        direct_text_packet, from_radio, mesh_packet, mt_config, node_key, synthetic_pubkey,
        AdminMessage, Data, LoRaConfig, MeshPacket, MtConfig, NodeInfo, BROADCAST_ADDR,
        PORT_ADMIN_APP, PORT_NODEINFO_APP, PORT_TEXT_MESSAGE_APP,
    },
    session::SessionState,
    stream::{ClientEvent, MeshtasticClient, SerialConfig, TcpConfig},
};

const TRANSPORT_NAME: &str = "meshtastic";

// ── Connection type ───────────────────────────────────────────────────────────

/// How the Meshtastic transport connects to the radio device.
#[derive(Debug, Clone, Copy, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MeshtasticConnectionType {
    /// Connect directly to a USB radio running Meshtastic firmware.
    #[default]
    Serial,
    /// Connect via TCP to `meshtasticd` or another Meshtastic TCP stream.
    Tcp,
    /// Connect to a Meshtastic-firmware Pi HAT via GPIO UART (`/dev/ttyAMA0`).
    Hat,
}

// ── Config ────────────────────────────────────────────────────────────────────

/// Radio parameter configuration stored in `[plugins.meshtastic.radio]`.
///
/// These values are pushed to the device automatically once config sync
/// completes after the transport connects (see `apply_config_on_connect`), so
/// changes made in setup or the web admin UI take effect the next time the BBS
/// connects.  The device persists the settings in its own flash.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MeshtasticRadioConfig {
    /// Meshtastic region code (e.g. `"US"`, `"EU_868"`, `"ANZ"`).
    ///
    /// Sets the legal frequency range for the device.  See the Meshtastic
    /// documentation for the full list of region codes.
    #[serde(default)]
    pub region: Option<String>,

    /// Meshtastic modem preset (e.g. `"LONG_FAST"`, `"MEDIUM_SLOW"`).
    ///
    /// Predefined LoRa parameter profiles.  `"LONG_FAST"` is the Meshtastic
    /// default and works well for most outdoor deployments.
    #[serde(default)]
    pub modem_preset: Option<String>,

    /// Enable SX126x RX boosted gain — improves receive sensitivity. Default on.
    #[serde(default = "default_true")]
    pub rx_boosted_gain: bool,

    /// Maximum number of hops for packets originated by this node. Default 3.
    #[serde(default = "default_radio_hops")]
    pub hops: u32,

    /// Ignore packets that arrived over MQTT. Default on.
    #[serde(default = "default_true")]
    pub ignore_mqtt: bool,

    /// Whether the radio transmitter is enabled. Default on.
    #[serde(default = "default_true")]
    pub tx_enabled: bool,
}

impl Default for MeshtasticRadioConfig {
    fn default() -> Self {
        Self {
            region: None,
            modem_preset: None,
            rx_boosted_gain: true,
            hops: default_radio_hops(),
            ignore_mqtt: true,
            tx_enabled: true,
        }
    }
}

/// Configuration for the Meshtastic transport (`[plugins.meshtastic]`).
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MeshtasticConfig {
    /// Set to `false` to disable this transport without removing the section.
    #[serde(default = "default_enabled")]
    pub enabled: bool,

    /// How to connect to the radio.
    #[serde(default)]
    pub connection_type: MeshtasticConnectionType,

    /// OS path to the USB serial device.
    #[serde(default)]
    pub serial_port: Option<String>,

    /// Baud rate for the serial connection.
    #[serde(default = "default_baud_rate")]
    pub baud_rate: u32,

    /// Address of a `meshtasticd` TCP listener.
    #[serde(default = "default_addr")]
    pub addr: SocketAddr,

    /// Optional single-character prefix that marks a message as a BBS command.
    #[serde(default)]
    pub command_prefix: Option<char>,

    /// Greeting sent to a node the first time it contacts the BBS.
    #[serde(default = "default_welcome_message")]
    pub welcome_message: String,

    /// Maximum bytes sent in one Meshtastic text payload.
    #[serde(default = "default_max_payload_bytes")]
    pub max_payload_bytes: usize,

    /// Days a radio-node credential remains valid. `0` disables auto-login.
    #[serde(default = "default_node_credential_ttl_days")]
    pub node_credential_ttl_days: u32,

    /// Hop limit for outbound direct-message replies.
    #[serde(default = "default_hop_limit")]
    pub hop_limit: u32,

    /// Request radio-layer acknowledgements for replies and notifications.
    #[serde(default = "default_want_ack")]
    pub want_ack: bool,

    /// Initial reconnect delay after a serial/TCP disconnect.
    #[serde(default = "default_reconnect_delay_initial_ms")]
    pub reconnect_delay_initial_ms: u64,

    /// Maximum reconnect delay after repeated failures.
    #[serde(default = "default_reconnect_delay_max_ms")]
    pub reconnect_delay_max_ms: u64,

    /// Radio parameter configuration.
    ///
    /// Applied to the device automatically when the BBS connects.
    ///
    /// Example:
    /// ```toml
    /// [plugins.meshtastic.radio]
    /// region       = "US"
    /// modem_preset = "LONG_FAST"
    /// ```
    #[serde(default)]
    pub radio: Option<MeshtasticRadioConfig>,

    /// Short node name shown on OLED maps and mesh UIs (≤ 4 chars).
    ///
    /// Applied to the device automatically when the BBS connects.
    #[serde(default)]
    pub short_name: Option<String>,

    /// Full node display name shown in Meshtastic apps.
    ///
    /// Applied to the device automatically when the BBS connects.
    #[serde(default)]
    pub long_name: Option<String>,
}

impl Default for MeshtasticConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            connection_type: MeshtasticConnectionType::default(),
            serial_port: None,
            baud_rate: default_baud_rate(),
            addr: default_addr(),
            command_prefix: None,
            welcome_message: default_welcome_message(),
            max_payload_bytes: default_max_payload_bytes(),
            node_credential_ttl_days: default_node_credential_ttl_days(),
            hop_limit: default_hop_limit(),
            want_ack: default_want_ack(),
            reconnect_delay_initial_ms: default_reconnect_delay_initial_ms(),
            reconnect_delay_max_ms: default_reconnect_delay_max_ms(),
            radio: None,
            short_name: None,
            long_name: None,
        }
    }
}

impl MeshtasticConfig {
    fn reconnect_delay_initial(&self) -> Duration {
        Duration::from_millis(self.reconnect_delay_initial_ms.max(1))
    }

    fn reconnect_delay_max(&self) -> Duration {
        Duration::from_millis(
            self.reconnect_delay_max_ms
                .max(self.reconnect_delay_initial_ms.max(1)),
        )
    }
}

fn default_enabled() -> bool {
    false
}
fn default_true() -> bool {
    true
}
fn default_radio_hops() -> u32 {
    3
}
fn default_baud_rate() -> u32 {
    115_200
}
fn default_addr() -> SocketAddr {
    "127.0.0.1:4403"
        .parse()
        .expect("hard-coded address is valid")
}
fn default_welcome_message() -> String {
    "Welcome to Supply Drop BBS. LOGIN <user>, REGISTER <user>, or H for help.".to_owned()
}
fn default_max_payload_bytes() -> usize {
    220
}
fn default_node_credential_ttl_days() -> u32 {
    14
}
fn default_hop_limit() -> u32 {
    3
}
fn default_want_ack() -> bool {
    true
}
fn default_reconnect_delay_initial_ms() -> u64 {
    1_000
}
fn default_reconnect_delay_max_ms() -> u64 {
    60_000
}

// ── Transport ─────────────────────────────────────────────────────────────────

/// Meshtastic transport plugin handle.
pub struct MeshtasticTransport {
    host: Arc<dyn Host>,
    cmd_tx: mpsc::Sender<proto::ToRadio>,
    state: Arc<Mutex<SessionState>>,
    client_slot: Mutex<Option<MeshtasticClient>>,
    shutdown_tx: watch::Sender<bool>,
    command_prefix: Option<char>,
    welcome_message: String,
    max_payload_bytes: usize,
    node_credential_ttl_days: u32,
    hop_limit: u32,
    want_ack: bool,
    packet_counter: Arc<AtomicU32>,
    /// Admin channel sender — stored so `start()` can register it with the host.
    admin_tx: mpsc::Sender<MeshtasticAdminRequest>,
    /// Admin channel receiver — taken by `start()` into the event loop.
    admin_rx: Mutex<Option<mpsc::Receiver<MeshtasticAdminRequest>>>,
    /// Radio config from `[plugins.meshtastic.radio]`, applied to the device
    /// automatically once config sync completes after connecting.
    radio_config: Option<MeshtasticRadioConfig>,
    /// Short node name from `[plugins.meshtastic]`, applied on connect.
    short_name: Option<String>,
    /// Long node name from `[plugins.meshtastic]`, applied on connect.
    long_name: Option<String>,
}

#[async_trait]
impl Plugin for MeshtasticTransport {
    type Config = MeshtasticConfig;

    fn name(&self) -> &'static str {
        TRANSPORT_NAME
    }

    fn version(&self) -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    async fn init(config: Self::Config, host: Arc<dyn Host>) -> Result<Self, PluginError> {
        let client = match config.connection_type {
            MeshtasticConnectionType::Tcp => {
                info!(addr = %config.addr, "meshtastic transport: connecting via TCP");
                MeshtasticClient::connect_tcp(TcpConfig {
                    addr: config.addr,
                    reconnect_delay_initial: config.reconnect_delay_initial(),
                    reconnect_delay_max: config.reconnect_delay_max(),
                })
            }
            MeshtasticConnectionType::Serial | MeshtasticConnectionType::Hat => {
                let port = config.serial_port.clone().ok_or_else(|| {
                    PluginError::InvalidConfig(
                        "connection_type 'serial'/'hat' requires serial_port to be set".into(),
                    )
                })?;
                let conn_label = if config.connection_type == MeshtasticConnectionType::Hat {
                    "via GPIO UART HAT"
                } else {
                    "via USB serial"
                };
                info!(port = %port, baud = config.baud_rate, "meshtastic transport: connecting {conn_label}");
                MeshtasticClient::connect_serial(SerialConfig {
                    port,
                    baud_rate: config.baud_rate,
                    reconnect_delay_initial: config.reconnect_delay_initial(),
                    reconnect_delay_max: config.reconnect_delay_max(),
                })
            }
        };

        let cmd_tx = client.sender();
        let (shutdown_tx, _) = watch::channel(false);
        let (admin_tx, admin_rx) = mpsc::channel::<MeshtasticAdminRequest>(4);

        Ok(Self {
            host,
            cmd_tx,
            state: Arc::new(Mutex::new(SessionState::default())),
            client_slot: Mutex::new(Some(client)),
            shutdown_tx,
            command_prefix: config.command_prefix,
            welcome_message: config.welcome_message,
            max_payload_bytes: config.max_payload_bytes,
            node_credential_ttl_days: config.node_credential_ttl_days,
            hop_limit: config.hop_limit,
            want_ack: config.want_ack,
            packet_counter: Arc::new(AtomicU32::new(1)),
            admin_tx,
            admin_rx: Mutex::new(Some(admin_rx)),
            radio_config: config.radio,
            short_name: config.short_name,
            long_name: config.long_name,
        })
    }

    async fn start(&self) -> Result<(), PluginError> {
        let client = self
            .client_slot
            .lock()
            .expect("client_slot mutex poisoned")
            .take()
            .ok_or_else(|| {
                PluginError::StartFailed("meshtastic transport already started".into())
            })?;

        let admin_rx = self
            .admin_rx
            .lock()
            .expect("admin_rx mutex poisoned")
            .take()
            .ok_or_else(|| {
                PluginError::StartFailed("meshtastic admin channel already taken".into())
            })?;

        // Register admin channel with the host before spawning the event loop.
        self.host
            .register_meshtastic_admin_ops(self.admin_tx.clone());

        let host = Arc::clone(&self.host);
        let cmd_tx = self.cmd_tx.clone();
        let state = Arc::clone(&self.state);
        let shutdown_rx = self.shutdown_tx.subscribe();
        let prefix = self.command_prefix;
        let welcome = self.welcome_message.clone();
        let ttl_days = self.node_credential_ttl_days;
        let max_payload = self.max_payload_bytes;
        let hop_limit = self.hop_limit;
        let want_ack = self.want_ack;
        let packet_counter = Arc::clone(&self.packet_counter);
        let auto_apply = AutoApplyConfig {
            radio: self.radio_config.clone(),
            short_name: self.short_name.clone(),
            long_name: self.long_name.clone(),
        };

        tokio::spawn(event_loop(
            client,
            host,
            cmd_tx,
            state,
            shutdown_rx,
            prefix,
            welcome,
            ttl_days,
            max_payload,
            hop_limit,
            want_ack,
            packet_counter,
            admin_rx,
            auto_apply,
        ));

        // Subscribe to advisory domain events, mirroring the MeshCore transport.
        let mut domain_rx = self.host.events();
        let notif_host = Arc::clone(&self.host);
        let notif_cmd_tx = self.cmd_tx.clone();
        let notif_state = Arc::clone(&self.state);
        let notif_max_payload = self.max_payload_bytes;
        let notif_hop_limit = self.hop_limit;
        let notif_want_ack = self.want_ack;
        let notif_counter = Arc::clone(&self.packet_counter);
        let mut notif_shutdown_rx = self.shutdown_tx.subscribe();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    result = domain_rx.recv() => match result {
                        Ok(event) => {
                            push_domain_notification(
                                event,
                                &notif_host,
                                &notif_cmd_tx,
                                &notif_state,
                                notif_max_payload,
                                notif_hop_limit,
                                notif_want_ack,
                                &notif_counter,
                            ).await;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            warn!("meshtastic: domain event stream lagged by {n}");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    },
                    _ = notif_shutdown_rx.changed() => break,
                }
            }
        });

        // NB: the meshtastic transport intentionally does NOT subscribe to the
        // advert-send bus. Meshtastic NodeInfo is only ever *originated* by the
        // device firmware (on boot, on a periodic timer, on hearing an unknown
        // node, or when answering a NodeInfo request) — a client-injected
        // NODEINFO_APP packet is not retransmitted by the firmware, so a manual
        // "send advert" can't make us appear. Discoverability is handled by
        // lowering `node_info_broadcast_secs` on connect (see enqueue_auto_apply).

        info!("meshtastic transport started");
        Ok(())
    }

    async fn stop(&self) -> Result<(), PluginError> {
        let _ = self.shutdown_tx.send(true);
        let sessions = self.state.lock().expect("state mutex poisoned").sessions();
        for session in sessions {
            let _ = self.host.end_session(session).await;
        }
        info!("meshtastic transport stop requested");
        Ok(())
    }
}

#[async_trait]
impl TransportEngine for MeshtasticTransport {
    async fn notify(
        &self,
        session: SessionId,
        payload: Notification,
    ) -> Result<NotifyOutcome, TransportError> {
        let node_num = {
            self.state
                .lock()
                .expect("state mutex poisoned")
                .node_for_session(session)
        };
        let Some(node_num) = node_num else {
            return Ok(NotifyOutcome::Dropped);
        };

        let text = truncate_utf8(&render_notification(&payload), self.max_payload_bytes);
        let packet_id = self.packet_counter.fetch_add(1, Ordering::Relaxed);
        self.cmd_tx
            .send(direct_text_packet(
                node_num,
                text,
                packet_id,
                self.hop_limit,
                self.want_ack,
            ))
            .await
            .map_err(|_| TransportError::ConnectionLost("meshtastic client closed".into()))?;
        Ok(NotifyOutcome::Queued)
    }
}

// ── Event loop ────────────────────────────────────────────────────────────────

/// Settings from config.toml that are pushed to the device automatically once
/// config sync completes after each (re)connection.  This is what makes
/// "configure in setup or web UI → it just works" true: the operator never has
/// to run a CLI command to apply settings.
#[derive(Clone, Default)]
struct AutoApplyConfig {
    radio: Option<MeshtasticRadioConfig>,
    short_name: Option<String>,
    long_name: Option<String>,
}

/// Pending admin request in the Meshtastic event loop.
///
/// Only GET operations are tracked here — they wait for the device's response.
/// SET operations (LoRa config, owner) are fire-and-forget: Meshtastic does not
/// send a reliable admin response for config writes — it applies them silently
/// (and reboots only when a LoRa parameter actually changes). Waiting for an ACK
/// would always time out, so SET requests reply success as soon as the command
/// is sent to the device.
#[allow(clippy::enum_variant_names)] // all are Get* by design; Set* are fire-and-forget
enum PendingMeshtasticAdmin {
    GetLora {
        request_id: u32,
        reply: tokio::sync::oneshot::Sender<Result<bbs_plugin_api::MeshtasticLoRaConfig, String>>,
    },
    GetOwner {
        request_id: u32,
        reply: tokio::sync::oneshot::Sender<Result<bbs_plugin_api::MeshtasticOwnerInfo, String>>,
    },
    GetSecurity {
        request_id: u32,
        reply: tokio::sync::oneshot::Sender<Result<bbs_plugin_api::MeshtasticSecurityInfo, String>>,
    },
}

/// True when the caller waiting on a pending GET has dropped its receiver
/// (i.e. timed out and given up), so the pending op can be safely discarded.
fn pending_reply_abandoned(op: &PendingMeshtasticAdmin) -> bool {
    match op {
        PendingMeshtasticAdmin::GetLora { reply, .. } => reply.is_closed(),
        PendingMeshtasticAdmin::GetOwner { reply, .. } => reply.is_closed(),
        PendingMeshtasticAdmin::GetSecurity { reply, .. } => reply.is_closed(),
    }
}

/// An admin WRITE waiting for a fresh session passkey before it can be sent.
///
/// Meshtastic gates admin writes on an 8-byte `session_passkey` that the device
/// only hands out in a get-response. So a write is deferred: we send a
/// session-key request, and once the passkey arrives we build and send the
/// write with it echoed in.
enum DeferredWrite {
    Lora {
        lora: proto::LoRaConfig,
        reply: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
    },
    Owner {
        user: proto::User,
        reply: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
    },
    /// Write the merged DeviceConfig (e.g. node_info_broadcast_secs). Reboots.
    Device { device: proto::DeviceConfig },
    /// Reboot the radio after `secs` (forces a boot-time NodeInfo broadcast).
    Reboot {
        secs: i32,
        reply: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
    },
    /// Sync the device clock to the host's current time. No reply, no reboot.
    Time,
    /// Set a fixed GPS position (decimal degrees). No reply, no reboot.
    SetFixedPosition { lat: f64, lon: f64 },
    /// Clear any fixed GPS position. No reply, no reboot.
    RemoveFixedPosition,
}

/// Extract the `session_passkey` (AdminMessage field 101) from an inbound admin
/// packet, if present. Every Meshtastic admin get-response carries a fresh
/// 8-byte passkey; harvesting it lets us authorize subsequent writes.
fn harvest_session_passkey(msg: &proto::FromRadio) -> Option<Vec<u8>> {
    use prost::Message as _;
    let Some(from_radio::PayloadVariant::Packet(p)) = &msg.payload_variant else {
        return None;
    };
    let Some(mesh_packet::PayloadVariant::Decoded(d)) = &p.payload_variant else {
        return None;
    };
    if d.portnum != PORT_ADMIN_APP {
        return None;
    }
    let admin = AdminMessage::decode(d.payload.as_slice()).ok()?;
    (!admin.session_passkey.is_empty()).then_some(admin.session_passkey)
}

/// Send all deferred writes now that a session passkey is available.
async fn flush_deferred_writes(
    cmd_tx: &mpsc::Sender<proto::ToRadio>,
    node: u32,
    passkey: &[u8],
    deferred: &mut Vec<DeferredWrite>,
) {
    for w in deferred.drain(..) {
        let rid = random_packet_id();
        match w {
            DeferredWrite::Lora { lora, reply } => {
                let ok = cmd_tx
                    .send(admin_set_lora_config(node, rid, lora, passkey.to_vec()))
                    .await
                    .is_ok();
                if let Some(r) = reply {
                    let _ = r.send(if ok {
                        Ok(())
                    } else {
                        Err("meshtastic client disconnected".into())
                    });
                }
                if ok {
                    info!("meshtastic: applied LoRa config to device");
                }
            }
            DeferredWrite::Owner { user, reply } => {
                let ok = cmd_tx
                    .send(admin_set_owner(node, rid, user, passkey.to_vec()))
                    .await
                    .is_ok();
                if let Some(r) = reply {
                    let _ = r.send(if ok {
                        Ok(())
                    } else {
                        Err("meshtastic client disconnected".into())
                    });
                }
                if ok {
                    info!("meshtastic: applied node owner info to device");
                }
            }
            DeferredWrite::Device { device } => {
                if cmd_tx
                    .send(admin_set_device_config(node, rid, device, passkey.to_vec()))
                    .await
                    .is_ok()
                {
                    info!("meshtastic: applied device config (node_info_broadcast_secs) to device");
                }
            }
            DeferredWrite::Reboot { secs, reply } => {
                let ok = cmd_tx
                    .send(admin_reboot(node, rid, secs, passkey.to_vec()))
                    .await
                    .is_ok();
                if let Some(r) = reply {
                    let _ = r.send(if ok {
                        Ok(())
                    } else {
                        Err("meshtastic client disconnected".into())
                    });
                }
                if ok {
                    info!(secs, "meshtastic: requested radio reboot");
                }
            }
            DeferredWrite::Time => {
                let secs = unix_now_secs();
                if cmd_tx
                    .send(admin_set_time(node, rid, secs, passkey.to_vec()))
                    .await
                    .is_ok()
                {
                    info!(unix_secs = secs, "meshtastic: synced device time");
                }
            }
            DeferredWrite::SetFixedPosition { lat, lon } => {
                if cmd_tx
                    .send(admin_set_fixed_position(
                        node,
                        rid,
                        lat,
                        lon,
                        passkey.to_vec(),
                    ))
                    .await
                    .is_ok()
                {
                    info!(lat, lon, "meshtastic: set fixed position on device");
                }
            }
            DeferredWrite::RemoveFixedPosition => {
                if cmd_tx
                    .send(admin_remove_fixed_position(node, rid, passkey.to_vec()))
                    .await
                    .is_ok()
                {
                    info!("meshtastic: cleared fixed position on device");
                }
            }
        }
    }
}

/// Current Unix time in seconds (truncated to u32, the wire field width).
fn unix_now_secs() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0)
}

#[allow(clippy::too_many_arguments)]
async fn event_loop(
    mut client: MeshtasticClient,
    host: Arc<dyn Host>,
    cmd_tx: mpsc::Sender<proto::ToRadio>,
    state: Arc<Mutex<SessionState>>,
    mut shutdown_rx: watch::Receiver<bool>,
    command_prefix: Option<char>,
    welcome_message: String,
    node_credential_ttl_days: u32,
    max_payload_bytes: usize,
    hop_limit: u32,
    want_ack: bool,
    packet_counter: Arc<AtomicU32>,
    mut admin_rx: mpsc::Receiver<MeshtasticAdminRequest>,
    auto_apply: AutoApplyConfig,
) {
    let mut pending_admin: Option<PendingMeshtasticAdmin> = None;
    // Session passkey received from the last admin get-response; required to
    // authorize admin writes (Meshtastic AdminMessage field 101).
    let mut last_session_passkey: Vec<u8> = Vec::new();
    // Admin writes waiting for a fresh session passkey before they can be sent.
    let mut deferred_writes: Vec<DeferredWrite> = Vec::new();
    // True once we've sent a session-key request and are awaiting the passkey.
    let mut session_requested = false;
    // Whether we've queued the config.toml auto-apply on this connection.
    // Reset on disconnect so settings re-apply after a reconnect.
    let mut auto_applied = false;
    // Periodically reap a pending GET whose caller has given up (its oneshot
    // receiver was dropped after the caller's own timeout). Without this, a GET
    // that never gets a device response would leave `pending_admin` set forever
    // and block every later admin operation with "another operation in progress".
    let mut reap = tokio::time::interval(Duration::from_secs(2));
    reap.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            _ = reap.tick() => {
                if pending_admin.as_ref().is_some_and(pending_reply_abandoned) {
                    warn!("meshtastic: clearing abandoned pending admin GET (no device response)");
                    pending_admin = None;
                }
            }
            event = client.recv() => match event {
                Some(ClientEvent::Connected) => {
                    info!("meshtastic: connected to radio");
                    auto_applied = false;
                }
                Some(ClientEvent::Disconnected { will_retry }) => {
                    // Fail any in-flight admin request.
                    if let Some(op) = pending_admin.take() {
                        let err = "device disconnected".to_owned();
                        match op {
                            PendingMeshtasticAdmin::GetLora { reply, .. } => { let _ = reply.send(Err(err)); }
                            PendingMeshtasticAdmin::GetOwner { reply, .. } => { let _ = reply.send(Err(err)); }
                            PendingMeshtasticAdmin::GetSecurity { reply, .. } => { let _ = reply.send(Err(err)); }
                        }
                    }
                    // Fail any deferred writes whose caller is waiting.
                    for w in deferred_writes.drain(..) {
                        let r = match w {
                            DeferredWrite::Lora { reply, .. } => reply,
                            DeferredWrite::Owner { reply, .. } => reply,
                            DeferredWrite::Reboot { reply, .. } => reply,
                            DeferredWrite::Device { .. }
                            | DeferredWrite::Time
                            | DeferredWrite::SetFixedPosition { .. }
                            | DeferredWrite::RemoveFixedPosition => None,
                        };
                        if let Some(r) = r {
                            let _ = r.send(Err("device disconnected".into()));
                        }
                    }
                    session_requested = false;
                    auto_applied = false;
                    if will_retry {
                        info!("meshtastic: radio disconnected, will retry");
                    } else {
                        info!("meshtastic: radio client shut down");
                        break;
                    }
                }
                Some(ClientEvent::FromRadio(msg)) => {
                    // Auto-apply config.toml settings once config sync completes.
                    // ConfigCompleteId marks the end of the initial sync, by which
                    // point MyInfo (node number) has already been received. We
                    // enqueue the writes and request a session key; the writes are
                    // sent once the passkey arrives (below).
                    if !auto_applied
                        && matches!(
                            msg.payload_variant,
                            Some(from_radio::PayloadVariant::ConfigCompleteId(_))
                        )
                    {
                        let (node, device_lora, device_owner, device_config) = {
                            let s = state.lock().expect("state poisoned");
                            (
                                s.my_node_num,
                                s.device_lora.clone(),
                                s.device_owner.clone(),
                                s.device_config.clone(),
                            )
                        };
                        if let Some(node) = node {
                            // Always sync the device clock to system time on connect.
                            deferred_writes.push(DeferredWrite::Time);
                            // Manage fixed position from the host's configured GPS:
                            // set it when a location is configured, clear it otherwise.
                            match host.node_location() {
                                Some((lat, lon)) => deferred_writes
                                    .push(DeferredWrite::SetFixedPosition { lat, lon }),
                                None => {
                                    deferred_writes.push(DeferredWrite::RemoveFixedPosition)
                                }
                            }
                            // Push configured radio params, node name, and the
                            // node-info broadcast interval (all skip-if-unchanged).
                            enqueue_auto_apply(
                                &auto_apply,
                                device_lora.as_ref(),
                                device_owner.as_ref(),
                                device_config.as_ref(),
                                &mut deferred_writes,
                            );
                            request_session_key(
                                &cmd_tx,
                                node,
                                &mut session_requested,
                                &deferred_writes,
                            )
                            .await;
                            auto_applied = true;
                        }
                    }

                    // Harvest a session passkey from any admin response and, once
                    // we have one, flush queued writes.
                    if let Some(pk) = harvest_session_passkey(&msg) {
                        last_session_passkey = pk;
                    }
                    if !deferred_writes.is_empty() && !last_session_passkey.is_empty() {
                        let node = state.lock().expect("state poisoned").my_node_num;
                        if let Some(node) = node {
                            flush_deferred_writes(
                                &cmd_tx,
                                node,
                                &last_session_passkey,
                                &mut deferred_writes,
                            )
                            .await;
                            session_requested = false;
                        }
                    }

                    // Check if this is an admin response packet before general dispatch.
                    let consumed = try_handle_admin_response(
                        &msg,
                        &mut pending_admin,
                        &mut last_session_passkey,
                    );
                    if !consumed {
                        handle_from_radio(
                            msg,
                            &host,
                            &cmd_tx,
                            &state,
                            command_prefix,
                            &welcome_message,
                            node_credential_ttl_days,
                            max_payload_bytes,
                            hop_limit,
                            want_ack,
                            &packet_counter,
                        )
                        .await;
                    }
                }
                None => break,
            },
            Some(req) = admin_rx.recv() => {
                let my_node_num = state.lock().expect("state mutex poisoned").my_node_num;
                let Some(my_node_num) = my_node_num else {
                    // Node number not yet received — reject all admin requests.
                    fn reject_no_num(req: MeshtasticAdminRequest) {
                        let e = "node num not yet received from radio".to_owned();
                        match req {
                            MeshtasticAdminRequest::GetLoRaConfig { reply } => { let _ = reply.send(Err(e)); }
                            MeshtasticAdminRequest::SetLoRaConfig { reply, .. } => { let _ = reply.send(Err(e)); }
                            MeshtasticAdminRequest::GetOwner { reply } => { let _ = reply.send(Err(e)); }
                            MeshtasticAdminRequest::SetOwner { reply, .. } => { let _ = reply.send(Err(e)); }
                            MeshtasticAdminRequest::GetSecurity { reply } => { let _ = reply.send(Err(e)); }
                            // Not connected yet → empty snapshot rather than an error.
                            MeshtasticAdminRequest::GetSnapshot { reply } => { let _ = reply.send(Ok(Default::default())); }
                            MeshtasticAdminRequest::Reboot { reply, .. } => { let _ = reply.send(Err(e)); }
                        }
                    }
                    reject_no_num(req);
                    continue;
                };
                // Only GET operations use `pending_admin` (one in flight at a
                // time). SET operations are queued as deferred writes, so they
                // are not blocked by a pending GET.
                if pending_admin.is_some()
                    && matches!(
                        req,
                        MeshtasticAdminRequest::GetLoRaConfig { .. }
                            | MeshtasticAdminRequest::GetOwner { .. }
                            | MeshtasticAdminRequest::GetSecurity { .. }
                    )
                {
                    let e = "another admin operation is already in progress".to_owned();
                    match req {
                        MeshtasticAdminRequest::GetLoRaConfig { reply } => { let _ = reply.send(Err(e)); }
                        MeshtasticAdminRequest::GetOwner { reply } => { let _ = reply.send(Err(e)); }
                        MeshtasticAdminRequest::GetSecurity { reply } => { let _ = reply.send(Err(e)); }
                        _ => unreachable!(),
                    }
                    continue;
                }
                match req {
                    MeshtasticAdminRequest::GetLoRaConfig { reply } => {
                        // Use a varied packet id, not the sequential counter: the
                        // device dedups by (from, id) and silently drops admin
                        // requests whose id it has seen before, so a low/repeating
                        // id yields no response (the GET then reaps as abandoned).
                        let rid = random_packet_id();
                        if cmd_tx.send(admin_get_lora_config(my_node_num, rid)).await.is_err() {
                            let _ = reply.send(Err("meshtastic client disconnected".into()));
                        } else {
                            pending_admin = Some(PendingMeshtasticAdmin::GetLora { request_id: rid, reply });
                        }
                    }
                    MeshtasticAdminRequest::SetLoRaConfig { config, reply } => {
                        let lora = LoRaConfig {
                            use_preset: config.use_preset,
                            modem_preset: config.modem_preset,
                            bandwidth: config.bandwidth,
                            spread_factor: config.spread_factor,
                            coding_rate: config.coding_rate,
                            frequency_offset: config.frequency_offset,
                            region: config.region,
                            hop_limit: config.hop_limit,
                            tx_enabled: config.tx_enabled,
                            tx_power: config.tx_power,
                            channel_num: config.channel_num,
                            override_frequency: config.override_frequency,
                            sx126x_rx_boosted_gain: config.sx126x_rx_boosted_gain,
                            ignore_mqtt: config.ignore_mqtt,
                        };
                        // Queue the write and request a session key. The write is
                        // sent (and `reply` completed) once the passkey arrives.
                        deferred_writes.push(DeferredWrite::Lora { lora, reply: Some(reply) });
                        request_session_key(&cmd_tx, my_node_num, &mut session_requested, &deferred_writes).await;
                    }
                    MeshtasticAdminRequest::GetOwner { reply } => {
                        let rid = random_packet_id();
                        if cmd_tx.send(admin_get_owner(my_node_num, rid)).await.is_err() {
                            let _ = reply.send(Err("meshtastic client disconnected".into()));
                        } else {
                            pending_admin = Some(PendingMeshtasticAdmin::GetOwner { request_id: rid, reply });
                        }
                    }
                    MeshtasticAdminRequest::SetOwner { long_name, short_name, reply } => {
                        let user = owner_user(long_name.as_deref(), short_name.as_deref());
                        deferred_writes.push(DeferredWrite::Owner { user, reply: Some(reply) });
                        request_session_key(&cmd_tx, my_node_num, &mut session_requested, &deferred_writes).await;
                    }
                    MeshtasticAdminRequest::GetSecurity { reply } => {
                        let rid = random_packet_id();
                        if cmd_tx.send(admin_get_security_config(my_node_num, rid)).await.is_err() {
                            let _ = reply.send(Err("meshtastic client disconnected".into()));
                        } else {
                            pending_admin = Some(PendingMeshtasticAdmin::GetSecurity { request_id: rid, reply });
                        }
                    }
                    MeshtasticAdminRequest::GetSnapshot { reply } => {
                        // Served instantly from the sync cache — no device round-trip.
                        let snapshot = {
                            let st = state.lock().expect("state mutex poisoned");
                            device_snapshot(
                                st.device_lora.as_ref(),
                                st.device_owner.as_ref(),
                                st.device_security.as_ref(),
                            )
                        };
                        let _ = reply.send(Ok(snapshot));
                    }
                    MeshtasticAdminRequest::Reboot { seconds, reply } => {
                        // Admin write → needs a session passkey; queue it and
                        // request the key (sent once the passkey arrives).
                        deferred_writes.push(DeferredWrite::Reboot {
                            secs: seconds,
                            reply: Some(reply),
                        });
                        request_session_key(&cmd_tx, my_node_num, &mut session_requested, &deferred_writes).await;
                    }
                }
            },
            _ = shutdown_rx.changed() => {
                info!("meshtastic: shutdown signal received");
                break;
            }
        }
    }
}

/// Target NodeInfo broadcast interval pushed to the device so it re-announces
/// itself periodically (firmware minimum is 3600s / 1h — lower values are
/// clamped by the firmware). This is the only firmware-supported way to keep a
/// node discoverable; client-injected NodeInfo packets are not retransmitted.
const NODE_INFO_BROADCAST_SECS: u32 = 3600;

/// Queue the operator's configured radio and node-name settings as deferred
/// writes, to be sent once a session passkey is obtained. Called once per
/// connection after config sync completes — this is what makes settings from
/// setup or the web UI take effect automatically on connect.
fn enqueue_auto_apply(
    cfg: &AutoApplyConfig,
    device_lora: Option<&proto::LoRaConfig>,
    device_owner: Option<&proto::User>,
    device_config: Option<&proto::DeviceConfig>,
    deferred: &mut Vec<DeferredWrite>,
) {
    // Device config: lower node_info_broadcast_secs so the firmware re-announces
    // the node hourly. Merge onto the captured DeviceConfig so role and every
    // other field are preserved; skip the write (and its reboot) when unchanged.
    if let Some(current) = device_config {
        if current.node_info_broadcast_secs != NODE_INFO_BROADCAST_SECS {
            let mut desired = current.clone();
            desired.node_info_broadcast_secs = NODE_INFO_BROADCAST_SECS;
            info!(
                from = current.node_info_broadcast_secs,
                to = NODE_INFO_BROADCAST_SECS,
                "meshtastic: queuing device config — node_info_broadcast_secs change"
            );
            deferred.push(DeferredWrite::Device { device: desired });
        } else {
            info!("meshtastic: node_info_broadcast_secs already matches, skipping (no reboot)");
        }
    }
    if let Some(radio) = &cfg.radio {
        // Build the desired config by *overlaying* our settings onto the
        // device's current LoRa config — never from a zeroed default. A bare
        // `LoRaConfig { .. ..Default::default() }` would set tx_enabled=false and
        // hop_limit=0, clobbering the radio every write. Region/preset are only
        // touched when explicitly configured (so we never blank a set region).
        //
        // Writing LoRa config reboots the radio even when unchanged, so we skip
        // the write entirely when the merged result equals what's on the device.
        if let Some(current) = device_lora {
            let mut desired = current.clone();
            desired.use_preset = true;
            if let Some(p) = radio.modem_preset.as_deref() {
                desired.modem_preset = preset_str_to_int(p);
            }
            if let Some(r) = radio.region.as_deref() {
                desired.region = region_str_to_int(r);
            }
            desired.hop_limit = radio.hops;
            desired.tx_enabled = radio.tx_enabled;
            desired.sx126x_rx_boosted_gain = radio.rx_boosted_gain;
            desired.ignore_mqtt = radio.ignore_mqtt;
            if desired == *current {
                info!(
                    "meshtastic: radio config already matches device, skipping write (no reboot)"
                );
            } else {
                // Log exactly which fields differ so a spurious reboot-on-every-
                // connect can be diagnosed (the device may not report a field we
                // force, making the compare always fail).
                info!(
                    use_preset = ?(current.use_preset, desired.use_preset),
                    region = ?(current.region, desired.region),
                    modem_preset = ?(current.modem_preset, desired.modem_preset),
                    hop_limit = ?(current.hop_limit, desired.hop_limit),
                    tx_enabled = ?(current.tx_enabled, desired.tx_enabled),
                    rx_boosted_gain = ?(current.sx126x_rx_boosted_gain, desired.sx126x_rx_boosted_gain),
                    ignore_mqtt = ?(current.ignore_mqtt, desired.ignore_mqtt),
                    "meshtastic: queuing radio config — fields differ (current, desired)"
                );
                deferred.push(DeferredWrite::Lora {
                    lora: desired,
                    reply: None,
                });
            }
        } else {
            warn!(
                "meshtastic: device LoRa config not captured during sync; \
                 skipping radio apply to avoid clobbering device settings"
            );
        }
    }
    if cfg.short_name.is_some() || cfg.long_name.is_some() {
        let user = owner_user(cfg.long_name.as_deref(), cfg.short_name.as_deref());
        // SetOwner also reboots the radio on current firmware. Skip the write
        // when the device already carries this exact (clamped) name — otherwise
        // every connect reboots the device and resets its periodic NodeInfo
        // broadcast timer, so neighbours never learn about us.
        let already_set = device_owner
            .is_some_and(|u| u.long_name == user.long_name && u.short_name == user.short_name);
        if already_set {
            info!(
                long_name = %user.long_name,
                short_name = %user.short_name,
                "meshtastic: node name already matches device, skipping write (no reboot)"
            );
        } else {
            info!(
                captured = device_owner.is_some(),
                device_long = device_owner.map(|u| u.long_name.as_str()).unwrap_or("<not captured>"),
                device_short = device_owner.map(|u| u.short_name.as_str()).unwrap_or("<not captured>"),
                config_long = %user.long_name,
                config_short = %user.short_name,
                "meshtastic: queuing node name — differs from device (or device owner not captured)"
            );
            deferred.push(DeferredWrite::Owner { user, reply: None });
        }
    }
}

/// Request a session key from the device (to authorize queued writes), unless a
/// request is already outstanding or there is nothing to write.
async fn request_session_key(
    cmd_tx: &mpsc::Sender<proto::ToRadio>,
    node: u32,
    session_requested: &mut bool,
    deferred: &[DeferredWrite],
) {
    if *session_requested || deferred.is_empty() {
        return;
    }
    if cmd_tx
        .send(admin_get_session_key(node, random_packet_id()))
        .await
        .is_ok()
    {
        *session_requested = true;
    }
}

/// Try to handle an admin-channel response packet.
///
/// Returns `true` if the message was consumed (caller should skip general dispatch).
/// Updates `session_passkey` with any passkey found in the decoded `AdminMessage`.
fn try_handle_admin_response(
    msg: &proto::FromRadio,
    pending_admin: &mut Option<PendingMeshtasticAdmin>,
    session_passkey: &mut Vec<u8>,
) -> bool {
    use prost::Message as _;

    let Some(from_radio::PayloadVariant::Packet(packet)) = &msg.payload_variant else {
        return false;
    };
    let Some(mesh_packet::PayloadVariant::Decoded(data)) = &packet.payload_variant else {
        return false;
    };
    if data.portnum != PORT_ADMIN_APP {
        return false;
    }

    match pending_admin.take() {
        Some(PendingMeshtasticAdmin::GetLora { request_id, reply }) => {
            if data.reply_id != request_id && data.request_id != request_id {
                *pending_admin = Some(PendingMeshtasticAdmin::GetLora { request_id, reply });
                return false;
            }
            match AdminMessage::decode(data.payload.as_slice()) {
                Ok(msg) => {
                    // Capture session passkey for subsequent SET commands.
                    if !msg.session_passkey.is_empty() {
                        *session_passkey = msg.session_passkey.clone();
                    }
                    match msg.payload_variant {
                        Some(admin_message::PayloadVariant::GetConfigResponse(MtConfig {
                            payload_variant: Some(mt_config::PayloadVariant::Lora(lora)),
                        })) => {
                            let config = bbs_plugin_api::MeshtasticLoRaConfig {
                                use_preset: lora.use_preset,
                                modem_preset: lora.modem_preset,
                                bandwidth: lora.bandwidth,
                                spread_factor: lora.spread_factor,
                                coding_rate: lora.coding_rate,
                                frequency_offset: lora.frequency_offset,
                                region: lora.region,
                                hop_limit: lora.hop_limit,
                                tx_enabled: lora.tx_enabled,
                                tx_power: lora.tx_power,
                                channel_num: lora.channel_num,
                                override_frequency: lora.override_frequency,
                                sx126x_rx_boosted_gain: lora.sx126x_rx_boosted_gain,
                                ignore_mqtt: lora.ignore_mqtt,
                            };
                            let _ = reply.send(Ok(config));
                        }
                        _ => {
                            let _ = reply.send(Err(
                                "unexpected admin response (expected LoRa config)".into(),
                            ));
                        }
                    }
                }
                Err(e) => {
                    let _ = reply.send(Err(format!("failed to decode admin response: {e}")));
                }
            }
            true
        }
        Some(PendingMeshtasticAdmin::GetOwner { request_id, reply }) => {
            if data.reply_id != request_id && data.request_id != request_id {
                *pending_admin = Some(PendingMeshtasticAdmin::GetOwner { request_id, reply });
                return false;
            }
            match AdminMessage::decode(data.payload.as_slice()) {
                Ok(msg) => {
                    if !msg.session_passkey.is_empty() {
                        *session_passkey = msg.session_passkey.clone();
                    }
                    match msg.payload_variant {
                        Some(admin_message::PayloadVariant::GetOwnerResponse(user)) => {
                            let info = bbs_plugin_api::MeshtasticOwnerInfo {
                                id: user.id,
                                long_name: user.long_name,
                                short_name: user.short_name,
                                public_key_hex: hex_encode(&user.public_key),
                            };
                            let _ = reply.send(Ok(info));
                        }
                        _ => {
                            let _ = reply.send(Err(
                                "unexpected admin response (expected owner info)".into(),
                            ));
                        }
                    }
                }
                Err(e) => {
                    let _ = reply.send(Err(format!("failed to decode admin response: {e}")));
                }
            }
            true
        }
        Some(PendingMeshtasticAdmin::GetSecurity { request_id, reply }) => {
            if data.reply_id != request_id && data.request_id != request_id {
                *pending_admin = Some(PendingMeshtasticAdmin::GetSecurity { request_id, reply });
                return false;
            }
            match AdminMessage::decode(data.payload.as_slice()) {
                Ok(msg) => {
                    if !msg.session_passkey.is_empty() {
                        *session_passkey = msg.session_passkey.clone();
                    }
                    match msg.payload_variant {
                        Some(admin_message::PayloadVariant::GetConfigResponse(MtConfig {
                            payload_variant: Some(mt_config::PayloadVariant::Security(sec)),
                        })) => {
                            let info = bbs_plugin_api::MeshtasticSecurityInfo {
                                public_key_hex: hex_encode(&sec.public_key),
                                admin_channel_enabled: sec.admin_channel_enabled,
                            };
                            let _ = reply.send(Ok(info));
                        }
                        _ => {
                            let _ = reply.send(Err(
                                "unexpected admin response (expected security config)".into(),
                            ));
                        }
                    }
                }
                Err(e) => {
                    let _ = reply.send(Err(format!("failed to decode admin response: {e}")));
                }
            }
            true
        }
        None => false,
    }
}

/// Hex-encode a byte slice (lowercase, no prefix).
fn hex_encode(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            use std::fmt::Write as _;
            write!(s, "{b:02x}").unwrap();
            s
        })
}

// ── CLI helpers — direct device config push ───────────────────────────────────

fn region_str_to_int(name: &str) -> i32 {
    match name {
        "US" => 1,
        "EU_433" => 2,
        "EU_868" => 3,
        "CN" => 4,
        "JP" => 5,
        "ANZ" => 6,
        "KR" => 7,
        "TW" => 8,
        "RU" => 9,
        "IN" => 10,
        "NZ_865" => 11,
        "TH" => 12,
        "LORA_24" => 13,
        "UA_433" => 14,
        "UA_868" => 15,
        "MY_433" => 16,
        "MY_919" => 17,
        "SG_923" => 18,
        _ => 0, // UNSET
    }
}

fn preset_str_to_int(name: &str) -> i32 {
    match name {
        "LONG_FAST" => 0,
        "LONG_SLOW" => 1,
        "VERY_LONG_SLOW" => 2,
        "MEDIUM_SLOW" => 3,
        "MEDIUM_FAST" => 4,
        "SHORT_SLOW" => 5,
        "SHORT_FAST" => 6,
        "LONG_MODERATE" => 7,
        "SHORT_TURBO" => 8,
        "LONG_TURBO" => 9,
        "LITE_FAST" => 10,
        "LITE_SLOW" => 11,
        "NARROW_FAST" => 12,
        "NARROW_SLOW" => 13,
        _ => 0, // LONG_FAST
    }
}

/// Connect to the Meshtastic device and push the LoRa radio config stored in
/// `config`.  Returns `Ok(())` on success or an error string.
///
/// Resolves the connection parameters in order: flag overrides → config values.
/// The BBS must **not** be running on the same port when this is called.
pub async fn apply_radio_from_config(
    config: &MeshtasticConfig,
    port_override: Option<String>,
    baud_override: Option<u32>,
    addr_override: Option<String>,
) -> Result<(), String> {
    use proto::{admin_set_lora_config, LoRaConfig};

    let radio_cfg = config.radio.as_ref().ok_or_else(|| {
        "no [plugins.meshtastic.radio] in config.toml — \
         run 'supply-drop-bbs setup' to configure"
            .to_owned()
    })?;
    let region = region_str_to_int(radio_cfg.region.as_deref().unwrap_or("UNSET"));
    let preset = preset_str_to_int(radio_cfg.modem_preset.as_deref().unwrap_or("LONG_FAST"));
    eprintln!(
        "Pushing radio config: region={} ({}), modem_preset={} ({})",
        region,
        radio_cfg.region.as_deref().unwrap_or("UNSET"),
        preset,
        radio_cfg.modem_preset.as_deref().unwrap_or("LONG_FAST"),
    );

    let mut client = make_client(config, port_override, baud_override, addr_override)?;
    run_admin_write(&mut client, move |node, passkey| {
        let lora = LoRaConfig {
            use_preset: true,
            modem_preset: preset,
            region,
            ..Default::default()
        };
        admin_set_lora_config(node, random_packet_id(), lora, passkey)
    })
    .await
}

/// Connect to the Meshtastic device and push the node name stored in `config`.
///
/// Fetches the existing owner first to preserve the PKC public key, then
/// merges in `long_name` and `short_name` from config and sends `SetOwner`.
pub async fn apply_owner_from_config(
    config: &MeshtasticConfig,
    port_override: Option<String>,
    baud_override: Option<u32>,
    addr_override: Option<String>,
) -> Result<(), String> {
    use proto::admin_set_owner;

    if config.long_name.is_none() && config.short_name.is_none() {
        return Err(
            "neither long_name nor short_name is set in [plugins.meshtastic] — \
             run 'supply-drop-bbs setup' or add them manually to config.toml"
                .to_owned(),
        );
    }

    let mut client = make_client(config, port_override, baud_override, addr_override)?;
    let user = owner_user(config.long_name.as_deref(), config.short_name.as_deref());
    eprintln!(
        "Pushing owner: long_name={:?}, short_name={:?}",
        user.long_name, user.short_name
    );
    run_admin_write(&mut client, move |node, passkey| {
        admin_set_owner(node, random_packet_id(), user.clone(), passkey)
    })
    .await
}

/// Drive a Meshtastic admin WRITE against a freshly-connected device, handling
/// the full handshake the firmware requires:
///
/// 1. `want_config` and wait for `ConfigCompleteId` (the device ignores admin
///    packets received mid-sync).
/// 2. Send `get_config_request: SESSIONKEY_CONFIG` and harvest the 8-byte
///    `session_passkey` the device returns (AdminMessage field 101).
/// 3. Build the write with that passkey echoed in and send it.
///
/// The passkey is what authorizes the write; without it the firmware drops the
/// admin message. The passkey is valid for ~300 s and must come from a recent
/// get-response.
async fn run_admin_write<F>(client: &mut stream::MeshtasticClient, build: F) -> Result<(), String>
where
    F: FnOnce(u32, Vec<u8>) -> proto::ToRadio,
{
    use prost::Message as _;
    use proto::{
        admin_get_session_key, admin_message, from_radio, mesh_packet, want_config, AdminMessage,
        PORT_ADMIN_APP,
    };
    use std::time::Duration;
    use stream::ClientEvent;
    use tokio::time::timeout;

    timeout(Duration::from_secs(20), async move {
        let cmd_tx = client.sender();
        let mut my_node_num: Option<u32> = None;
        let mut requested_key = false;
        let sess_req_id = random_packet_id();

        cmd_tx
            .send(want_config(42))
            .await
            .map_err(|_| "send failed".to_owned())?;

        while let Some(event) = client.recv().await {
            match event {
                ClientEvent::Disconnected { .. } => return Err("device disconnected".to_owned()),
                ClientEvent::Connected => {}
                ClientEvent::FromRadio(msg) => {
                    match &msg.payload_variant {
                        Some(from_radio::PayloadVariant::MyInfo(info)) => {
                            my_node_num = Some(info.my_node_num);
                        }
                        Some(from_radio::PayloadVariant::ConfigCompleteId(_)) if !requested_key => {
                            let Some(node) = my_node_num else {
                                return Err("config sync completed without a node number".into());
                            };
                            // Step 2: request a session key to authorize the write.
                            cmd_tx
                                .send(admin_get_session_key(node, sess_req_id))
                                .await
                                .map_err(|_| "send failed".to_owned())?;
                            requested_key = true;
                        }
                        Some(from_radio::PayloadVariant::Packet(packet)) if requested_key => {
                            // Look for the admin response carrying the session passkey.
                            if let Some(mesh_packet::PayloadVariant::Decoded(d)) =
                                &packet.payload_variant
                            {
                                if d.portnum == PORT_ADMIN_APP {
                                    if let Ok(adm) = AdminMessage::decode(d.payload.as_slice()) {
                                        // Only act on a get-response (it carries the
                                        // passkey); ignore our own echoed requests.
                                        let is_resp = matches!(
                                            adm.payload_variant,
                                            Some(admin_message::PayloadVariant::GetConfigResponse(
                                                _
                                            ))
                                        );
                                        if is_resp || !adm.session_passkey.is_empty() {
                                            let node = my_node_num.unwrap();
                                            let pkt = build(node, adm.session_passkey);
                                            cmd_tx
                                                .send(pkt)
                                                .await
                                                .map_err(|_| "send failed".to_owned())?;
                                            // Flush before dropping the connection.
                                            tokio::time::sleep(Duration::from_millis(2000)).await;
                                            return Ok(());
                                        }
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        Err("connection closed unexpectedly".to_owned())
    })
    .await
    .map_err(|_| "timed out (20s) — is the device connected and not in use?".to_owned())?
}

/// Build a combined device snapshot DTO from the cached sync config.
fn device_snapshot(
    lora: Option<&proto::LoRaConfig>,
    owner: Option<&proto::User>,
    security: Option<&proto::SecurityConfig>,
) -> bbs_plugin_api::MeshtasticDeviceSnapshot {
    bbs_plugin_api::MeshtasticDeviceSnapshot {
        lora: lora.map(|l| bbs_plugin_api::MeshtasticLoRaConfig {
            use_preset: l.use_preset,
            modem_preset: l.modem_preset,
            bandwidth: l.bandwidth,
            spread_factor: l.spread_factor,
            coding_rate: l.coding_rate,
            frequency_offset: l.frequency_offset,
            region: l.region,
            hop_limit: l.hop_limit,
            tx_enabled: l.tx_enabled,
            tx_power: l.tx_power,
            channel_num: l.channel_num,
            override_frequency: l.override_frequency,
            sx126x_rx_boosted_gain: l.sx126x_rx_boosted_gain,
            ignore_mqtt: l.ignore_mqtt,
        }),
        owner: owner.map(|u| bbs_plugin_api::MeshtasticOwnerInfo {
            id: u.id.clone(),
            long_name: u.long_name.clone(),
            short_name: u.short_name.clone(),
            public_key_hex: hex_encode(&u.public_key),
        }),
        security: security.map(|s| bbs_plugin_api::MeshtasticSecurityInfo {
            public_key_hex: hex_encode(&s.public_key),
            admin_channel_enabled: s.admin_channel_enabled,
        }),
    }
}

/// Build a Meshtastic owner `User` with firmware-safe field lengths.
///
/// The firmware decodes these into fixed buffers — `short_name` is `char[5]`
/// (max **4** chars + null) and `long_name` is `char[40]` (max **39**). A value
/// that overflows makes the device reject the *entire* admin message with
/// `Can't decode protobuf reason='string overflow'`, so we clamp here. `id` and
/// `public_key` are left empty: firmware ignores them on SetOwner and never
/// overwrites the node's PKC key from one.
fn owner_user(long_name: Option<&str>, short_name: Option<&str>) -> proto::User {
    fn clamp(s: &str, max_chars: usize) -> String {
        s.chars().take(max_chars).collect()
    }
    proto::User {
        id: String::new(),
        long_name: long_name.map(|s| clamp(s, 39)).unwrap_or_default(),
        short_name: short_name.map(|s| clamp(s, 4)).unwrap_or_default(),
        // Left 0 (CLIENT): prost omits default-value int32 from the wire, so
        // SetOwner never sends a role and can't clobber the device's real role.
        role: 0,
        public_key: Vec::new(),
    }
}

/// Generate an unpredictable, non-zero 32-bit packet id.
///
/// Meshtastic deduplicates received packets by `(from, id)`. Reusing fixed or
/// process-restart-repeating ids causes the device to silently discard our
/// admin packets as duplicates, so each packet needs a fresh, varied id —
/// exactly what the Meshtastic app does (it uses random ids).
fn random_packet_id() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos() ^ (d.as_secs() as u32))
        .unwrap_or(0);
    // Mix and ensure non-zero.
    let id = nanos.wrapping_mul(2_654_435_761).rotate_left(13) ^ 0x9E37_79B9;
    id | 1
}

/// Build a [`MeshtasticClient`] from config + optional CLI overrides.
fn make_client(
    config: &MeshtasticConfig,
    port_override: Option<String>,
    baud_override: Option<u32>,
    addr_override: Option<String>,
) -> Result<stream::MeshtasticClient, String> {
    use std::time::Duration;
    use stream::{SerialConfig, TcpConfig};

    match config.connection_type {
        MeshtasticConnectionType::Serial | MeshtasticConnectionType::Hat => {
            let port = port_override
                .or_else(|| config.serial_port.clone())
                .ok_or_else(|| {
                    "no serial port — use --port or set serial_port in \
                     [plugins.meshtastic] in config.toml"
                        .to_owned()
                })?;
            let baud = baud_override.unwrap_or(config.baud_rate);
            eprintln!("Connecting to {port} at {baud} baud…");
            Ok(stream::MeshtasticClient::connect_serial(SerialConfig {
                port,
                baud_rate: baud,
                reconnect_delay_initial: Duration::from_secs(1),
                reconnect_delay_max: Duration::from_secs(1),
            }))
        }
        MeshtasticConnectionType::Tcp => {
            let addr_str = addr_override.unwrap_or_else(|| config.addr.to_string());
            let addr: std::net::SocketAddr = addr_str
                .parse()
                .map_err(|e| format!("invalid TCP address '{addr_str}': {e}"))?;
            eprintln!("Connecting to {addr}…");
            Ok(stream::MeshtasticClient::connect_tcp(TcpConfig {
                addr,
                reconnect_delay_initial: Duration::from_secs(1),
                reconnect_delay_max: Duration::from_secs(1),
            }))
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_from_radio(
    msg: proto::FromRadio,
    host: &Arc<dyn Host>,
    cmd_tx: &mpsc::Sender<proto::ToRadio>,
    state: &Arc<Mutex<SessionState>>,
    command_prefix: Option<char>,
    welcome_message: &str,
    node_credential_ttl_days: u32,
    max_payload_bytes: usize,
    hop_limit: u32,
    want_ack: bool,
    packet_counter: &AtomicU32,
) {
    match msg.payload_variant {
        Some(from_radio::PayloadVariant::MyInfo(my_info)) => {
            state.lock().expect("state mutex poisoned").my_node_num = Some(my_info.my_node_num);
            info!(
                node = format_node_id(my_info.my_node_num),
                "meshtastic: local node info received"
            );
        }
        Some(from_radio::PayloadVariant::NodeInfo(node)) => {
            // If this is our own node, capture the current owner name so
            // apply-on-connect can skip a redundant (reboot-inducing) SetOwner.
            {
                let mut st = state.lock().expect("state mutex poisoned");
                if Some(node.num) == st.my_node_num {
                    if let Some(u) = &node.user {
                        st.device_owner = Some(u.clone());
                    }
                }
            }
            record_node_advert(host, node);
        }
        Some(from_radio::PayloadVariant::Config(cfg)) => {
            // Capture the device's current config from the sync stream so
            // apply-on-connect can skip redundant (reboot-inducing) writes and
            // the web can serve a snapshot without a live admin round-trip.
            match cfg.payload_variant {
                Some(proto::mt_config::PayloadVariant::Lora(lora)) => {
                    debug!(
                        region = lora.region,
                        modem_preset = lora.modem_preset,
                        use_preset = lora.use_preset,
                        "meshtastic: captured device LoRa config from sync"
                    );
                    state.lock().expect("state mutex poisoned").device_lora = Some(lora);
                }
                Some(proto::mt_config::PayloadVariant::Security(sec)) => {
                    debug!("meshtastic: captured device security config from sync");
                    state.lock().expect("state mutex poisoned").device_security = Some(sec);
                }
                Some(proto::mt_config::PayloadVariant::Device(dev)) => {
                    debug!(
                        node_info_broadcast_secs = dev.node_info_broadcast_secs,
                        role = dev.role,
                        "meshtastic: captured device config from sync"
                    );
                    state.lock().expect("state mutex poisoned").device_config = Some(dev);
                }
                None => {}
            }
        }
        Some(from_radio::PayloadVariant::Packet(packet)) => {
            handle_packet(
                packet,
                host,
                cmd_tx,
                state,
                command_prefix,
                welcome_message,
                node_credential_ttl_days,
                max_payload_bytes,
                hop_limit,
                want_ack,
                packet_counter,
            )
            .await;
        }
        Some(from_radio::PayloadVariant::ConfigCompleteId(id)) => {
            debug!(id, "meshtastic: config sync complete");
        }
        Some(from_radio::PayloadVariant::Rebooted(true)) => {
            info!("meshtastic: radio rebooted");
        }
        _ => {}
    }
}

fn record_node_advert(host: &Arc<dyn Host>, node: NodeInfo) {
    let pubkey = node
        .user
        .as_ref()
        .and_then(|u| <[u8; 32]>::try_from(u.public_key.as_slice()).ok())
        .unwrap_or_else(|| synthetic_pubkey(node.num));

    let name = node
        .user
        .as_ref()
        .map(|u| {
            if u.long_name.is_empty() {
                u.id.clone()
            } else {
                u.long_name.clone()
            }
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format_node_id(node.num));

    let (lat_1e6, lon_1e6) = node
        .position
        .as_ref()
        .map(|p| {
            (
                p.latitude_i.unwrap_or(0) / 10,
                p.longitude_i.unwrap_or(0) / 10,
            )
        })
        .unwrap_or((0, 0));

    // Record the device role (Config.DeviceConfig.Role) as the advert "type".
    // The web interprets this per-transport (a Meshtastic role, not a MeshCore
    // advert type).
    let role = node.user.as_ref().map(|u| u.role).unwrap_or(0);
    host.advert_bus().upsert(
        pubkey,
        name,
        role.clamp(0, u8::MAX as i32) as u8,
        lat_1e6,
        lon_1e6,
        TRANSPORT_NAME,
    );
}

#[allow(clippy::too_many_arguments)]
async fn handle_packet(
    packet: MeshPacket,
    host: &Arc<dyn Host>,
    cmd_tx: &mpsc::Sender<proto::ToRadio>,
    state: &Arc<Mutex<SessionState>>,
    command_prefix: Option<char>,
    welcome_message: &str,
    node_credential_ttl_days: u32,
    max_payload_bytes: usize,
    hop_limit: u32,
    want_ack: bool,
    packet_counter: &AtomicU32,
) {
    if let Some(mesh_packet::PayloadVariant::Decoded(data)) = &packet.payload_variant {
        if data.portnum == PORT_NODEINFO_APP {
            debug!(
                from = format_node_id(packet.from),
                "meshtastic: nodeinfo packet observed"
            );
            return;
        }
        if data.portnum == PORT_ADMIN_APP {
            // Admin packets are handled by try_handle_admin_response in the event loop.
            return;
        }
    }

    if !is_direct_to_us(&packet, state) {
        debug!(
            from = format_node_id(packet.from),
            to = packet.to,
            "meshtastic: ignoring non-DM packet"
        );
        return;
    }

    let Some(text) = text_payload(&packet) else {
        debug!("meshtastic: ignoring non-text packet");
        return;
    };

    dispatch_message(
        packet.from,
        &text,
        host,
        cmd_tx,
        state,
        command_prefix,
        welcome_message,
        node_credential_ttl_days,
        max_payload_bytes,
        hop_limit,
        want_ack,
        packet_counter,
    )
    .await;
}

fn is_direct_to_us(packet: &MeshPacket, state: &Arc<Mutex<SessionState>>) -> bool {
    if packet.to == BROADCAST_ADDR {
        return false;
    }
    let my_node = state.lock().expect("state mutex poisoned").my_node_num;
    my_node.is_none_or(|mine| packet.to == mine)
}

fn text_payload(packet: &MeshPacket) -> Option<String> {
    let Some(mesh_packet::PayloadVariant::Decoded(Data {
        portnum, payload, ..
    })) = &packet.payload_variant
    else {
        return None;
    };

    if *portnum != PORT_TEXT_MESSAGE_APP {
        return None;
    }

    std::str::from_utf8(payload).ok().map(str::to_owned)
}

#[allow(clippy::too_many_arguments)]
async fn dispatch_message(
    node_num: u32,
    text: &str,
    host: &Arc<dyn Host>,
    cmd_tx: &mpsc::Sender<proto::ToRadio>,
    state: &Arc<Mutex<SessionState>>,
    command_prefix: Option<char>,
    welcome_message: &str,
    node_credential_ttl_days: u32,
    max_payload_bytes: usize,
    hop_limit: u32,
    want_ack: bool,
    packet_counter: &AtomicU32,
) {
    let (session, is_new) = get_or_create_session(node_num, host, state).await;

    if is_new {
        let auto_username = if node_credential_ttl_days > 0 {
            match host
                .mesh_node_restore(session, node_key(node_num), node_credential_ttl_days)
                .await
            {
                Ok(username) => username,
                Err(e) => {
                    warn!(
                        ?session,
                        node = format_node_id(node_num),
                        "meshtastic: node_restore error: {e}"
                    );
                    None
                }
            }
        } else {
            None
        };

        let greeting = if let Some(ref username) = auto_username {
            format!("Welcome back, {username}! Type 'H' for commands.")
        } else {
            welcome_message.to_owned()
        };
        send_text(
            cmd_tx,
            node_num,
            greeting,
            max_payload_bytes,
            hop_limit,
            want_ack,
            packet_counter,
        )
        .await;
    }

    let awaiting_reply = state
        .lock()
        .expect("state mutex poisoned")
        .is_awaiting_reply(node_num);

    if state
        .lock()
        .expect("state mutex poisoned")
        .dedup_message(node_num, text)
    {
        debug!("meshtastic: dropping retransmitted message");
        return;
    }

    if !awaiting_reply
        && state
            .lock()
            .expect("state mutex poisoned")
            .is_recent_workflow_reply(node_num, text)
    {
        debug!("meshtastic: dropping retransmitted workflow reply");
        return;
    }

    let Some(cmd) = parse_command(text, command_prefix, awaiting_reply) else {
        debug!("meshtastic: message ignored (no prefix match, no active workflow)");
        return;
    };

    if awaiting_reply {
        state
            .lock()
            .expect("state mutex poisoned")
            .set_last_workflow_reply(node_num, text.to_owned());
    }

    let response = match host.process_command(session, cmd.clone()).await {
        Ok(r) => r,
        Err(HostError::UnknownSession(stale)) => {
            info!(?stale, "meshtastic: stale session — refreshing");
            state
                .lock()
                .expect("state mutex poisoned")
                .remove_by_node(node_num);
            let fresh = match host.create_session(TRANSPORT_NAME).await {
                Ok(id) => id,
                Err(e) => {
                    warn!("meshtastic: session refresh failed: {e}");
                    return;
                }
            };
            state
                .lock()
                .expect("state mutex poisoned")
                .get_or_insert(node_num, fresh);
            if node_credential_ttl_days > 0 {
                let _ = host
                    .mesh_node_restore(fresh, node_key(node_num), node_credential_ttl_days)
                    .await;
            }
            match host.process_command(fresh, cmd).await {
                Ok(r) => r,
                Err(e) => Response::Error(format!("{e}")),
            }
        }
        Err(e) => Response::Error(format!("{e}")),
    };

    if node_credential_ttl_days > 0 {
        match &response {
            Response::LoggedIn { .. } => {
                if let Err(e) = host.mesh_node_bind(session, node_key(node_num)).await {
                    warn!(?session, "meshtastic: node_bind error: {e}");
                }
            }
            Response::LoggedOut => {
                if let Err(e) = host.mesh_node_unbind(node_key(node_num)).await {
                    warn!("meshtastic: node_unbind error: {e}");
                }
            }
            _ => {}
        }
    }

    let is_prompt = matches!(response, Response::Prompt { .. });
    state
        .lock()
        .expect("state mutex poisoned")
        .set_awaiting_reply(node_num, is_prompt);

    let frames: Vec<String> = if let Response::MultiText(parts) = &response {
        parts.clone()
    } else {
        match format_response(&response) {
            Some(t) => vec![t],
            None => return,
        }
    };

    for frame in frames {
        send_text(
            cmd_tx,
            node_num,
            frame,
            max_payload_bytes,
            hop_limit,
            want_ack,
            packet_counter,
        )
        .await;
    }

    if matches!(response, Response::LoggedOut) {
        let removed_session = {
            state
                .lock()
                .expect("state mutex poisoned")
                .remove_by_node(node_num)
        };
        if let Some(sid) = removed_session {
            let _ = host.end_session(sid).await;
        }
    }
}

async fn get_or_create_session(
    node_num: u32,
    host: &Arc<dyn Host>,
    state: &Arc<Mutex<SessionState>>,
) -> (SessionId, bool) {
    if let Some(sid) = state.lock().expect("state mutex poisoned").lookup(node_num) {
        return (sid, false);
    }

    let new_id = match host.create_session(TRANSPORT_NAME).await {
        Ok(id) => id,
        Err(e) => {
            warn!("meshtastic: host.create_session failed: {e}");
            if let Some(sid) = state.lock().expect("state mutex poisoned").lookup(node_num) {
                return (sid, false);
            }
            panic!("meshtastic: host.create_session failed and no fallback: {e}");
        }
    };

    state
        .lock()
        .expect("state mutex poisoned")
        .get_or_insert(node_num, new_id)
}

async fn send_text(
    cmd_tx: &mpsc::Sender<proto::ToRadio>,
    node_num: u32,
    text: String,
    max_payload_bytes: usize,
    hop_limit: u32,
    want_ack: bool,
    packet_counter: &AtomicU32,
) {
    if text.is_empty() {
        return;
    }
    let text = truncate_utf8(&text, max_payload_bytes);
    let packet_id = packet_counter.fetch_add(1, Ordering::Relaxed);
    if cmd_tx
        .send(direct_text_packet(
            node_num, text, packet_id, hop_limit, want_ack,
        ))
        .await
        .is_err()
    {
        warn!(
            node = format_node_id(node_num),
            "meshtastic: could not enqueue outbound text"
        );
    }
}

#[allow(clippy::too_many_arguments)]
async fn push_domain_notification(
    event: DomainEvent,
    host: &Arc<dyn Host>,
    cmd_tx: &mpsc::Sender<proto::ToRadio>,
    state: &Arc<Mutex<SessionState>>,
    max_payload_bytes: usize,
    hop_limit: u32,
    want_ack: bool,
    packet_counter: &AtomicU32,
) {
    let sessions: Vec<(SessionId, u32)> = {
        let state = state.lock().expect("state mutex poisoned");
        state
            .by_session
            .iter()
            .map(|(sid, node)| (*sid, *node))
            .collect()
    };

    match event {
        DomainEvent::UserValidated { user } => {
            for (sid, node_num) in sessions {
                let Ok(ctx) = host.permission_ctx(sid).await else {
                    continue;
                };
                if ctx.username.as_ref() == Some(&user) {
                    send_text(
                        cmd_tx,
                        node_num,
                        "Your account has been validated. Type 'H'.".to_owned(),
                        max_payload_bytes,
                        hop_limit,
                        want_ack,
                        packet_counter,
                    )
                    .await;
                }
            }
        }
        DomainEvent::UserCreated { user } => {
            for (sid, node_num) in sessions {
                let Ok(ctx) = host.permission_ctx(sid).await else {
                    continue;
                };
                if ctx.level >= PermissionLevel::Aide {
                    send_text(
                        cmd_tx,
                        node_num,
                        format!("New registration: {} - type PENDING.", user.as_str()),
                        max_payload_bytes,
                        hop_limit,
                        want_ack,
                        packet_counter,
                    )
                    .await;
                }
            }
        }
        _ => {}
    }
}

fn format_node_id(node_num: u32) -> String {
    format!("!{node_num:08x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_is_disabled_but_serial_ready() {
        let cfg = MeshtasticConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.connection_type, MeshtasticConnectionType::Serial);
        assert_eq!(cfg.baud_rate, 115_200);
    }

    #[test]
    fn hat_connection_type_round_trips_serde() {
        let toml = r#"
enabled = true
connection_type = "hat"
serial_port = "/dev/ttyAMA0"
"#;
        let cfg: MeshtasticConfig = toml::from_str(toml).unwrap();
        assert_eq!(cfg.connection_type, MeshtasticConnectionType::Hat);
        assert_eq!(cfg.serial_port.as_deref(), Some("/dev/ttyAMA0"));
    }

    #[test]
    fn text_payload_accepts_only_text_port() {
        let packet = MeshPacket {
            from: 1,
            to: 2,
            channel: 0,
            payload_variant: Some(mesh_packet::PayloadVariant::Decoded(Data {
                portnum: PORT_TEXT_MESSAGE_APP,
                payload: b"hello".to_vec(),
                want_response: false,
                dest: 0,
                source: 0,
                request_id: 0,
                reply_id: 0,
            })),
            id: 0,
            rx_time: 0,
            rx_snr: 0.0,
            hop_limit: 0,
            want_ack: false,
            priority: 0,
            rx_rssi: 0,
            via_mqtt: false,
            hop_start: 0,
            public_key: Vec::new(),
            pki_encrypted: false,
        };
        assert_eq!(text_payload(&packet), Some("hello".to_owned()));
    }
}
