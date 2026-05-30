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
        admin_get_lora_config, admin_get_owner, admin_get_security_config, admin_message,
        admin_set_lora_config, admin_set_owner, direct_text_packet, from_radio, mesh_packet,
        mt_config, node_key, synthetic_pubkey, AdminMessage, Data, LoRaConfig, MeshPacket,
        MtConfig, NodeInfo, User, BROADCAST_ADDR, PORT_ADMIN_APP, PORT_NODEINFO_APP,
        PORT_TEXT_MESSAGE_APP,
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
/// These values are **not** pushed to the device automatically on connect.
/// Apply them on demand via the web admin UI (Settings → Meshtastic radio)
/// or `supply-drop-bbs node set-meshtastic-radio` once that command is added.
/// Once applied the device persists the settings in its own flash.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
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
    /// Stored here for reference and applied on demand via the web admin UI
    /// (Settings → Meshtastic radio).  **Not** pushed automatically on every
    /// connect — the device persists radio settings in its own flash.
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
    /// Stored here for reference.  Push to the device via the web admin UI
    /// (Settings → Meshtastic device → Node name → Save to device) or by
    /// running `supply-drop-bbs node set-meshtastic-owner` once that CLI
    /// command is available.
    #[serde(default)]
    pub short_name: Option<String>,

    /// Full node display name shown in Meshtastic apps.
    ///
    /// Stored alongside `short_name`; applied on demand via the web admin UI.
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

/// Pending admin request in the Meshtastic event loop.
enum PendingMeshtasticAdmin {
    GetLora {
        request_id: u32,
        reply: tokio::sync::oneshot::Sender<Result<bbs_plugin_api::MeshtasticLoRaConfig, String>>,
    },
    SetLora {
        request_id: u32,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    GetOwner {
        request_id: u32,
        reply: tokio::sync::oneshot::Sender<Result<bbs_plugin_api::MeshtasticOwnerInfo, String>>,
    },
    SetOwner {
        request_id: u32,
        reply: tokio::sync::oneshot::Sender<Result<(), String>>,
    },
    GetSecurity {
        request_id: u32,
        reply: tokio::sync::oneshot::Sender<Result<bbs_plugin_api::MeshtasticSecurityInfo, String>>,
    },
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
) {
    let mut pending_admin: Option<PendingMeshtasticAdmin> = None;
    // Session passkey received from the last GET response; echo it back in SET
    // commands as a replay-attack guard (Meshtastic 2.5+, field 101).
    let mut last_session_passkey: Vec<u8> = Vec::new();

    loop {
        tokio::select! {
            event = client.recv() => match event {
                Some(ClientEvent::Connected) => {
                    info!("meshtastic: connected to radio");
                }
                Some(ClientEvent::Disconnected { will_retry }) => {
                    // Fail any in-flight admin request.
                    if let Some(op) = pending_admin.take() {
                        let err = "device disconnected".to_owned();
                        match op {
                            PendingMeshtasticAdmin::GetLora { reply, .. } => { let _ = reply.send(Err(err)); }
                            PendingMeshtasticAdmin::SetLora { reply, .. } => { let _ = reply.send(Err(err)); }
                            PendingMeshtasticAdmin::GetOwner { reply, .. } => { let _ = reply.send(Err(err)); }
                            PendingMeshtasticAdmin::SetOwner { reply, .. } => { let _ = reply.send(Err(err)); }
                            PendingMeshtasticAdmin::GetSecurity { reply, .. } => { let _ = reply.send(Err(err)); }
                        }
                    }
                    if will_retry {
                        info!("meshtastic: radio disconnected, will retry");
                    } else {
                        info!("meshtastic: radio client shut down");
                        break;
                    }
                }
                Some(ClientEvent::FromRadio(msg)) => {
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
                        }
                    }
                    reject_no_num(req);
                    continue;
                };
                if pending_admin.is_some() {
                    // Another admin operation is in flight — reject immediately.
                    fn reject_busy(req: MeshtasticAdminRequest) {
                        let e = "another admin operation is already in progress".to_owned();
                        match req {
                            MeshtasticAdminRequest::GetLoRaConfig { reply } => { let _ = reply.send(Err(e)); }
                            MeshtasticAdminRequest::SetLoRaConfig { reply, .. } => { let _ = reply.send(Err(e)); }
                            MeshtasticAdminRequest::GetOwner { reply } => { let _ = reply.send(Err(e)); }
                            MeshtasticAdminRequest::SetOwner { reply, .. } => { let _ = reply.send(Err(e)); }
                            MeshtasticAdminRequest::GetSecurity { reply } => { let _ = reply.send(Err(e)); }
                        }
                    }
                    reject_busy(req);
                    continue;
                }
                match req {
                    MeshtasticAdminRequest::GetLoRaConfig { reply } => {
                        let rid = packet_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if cmd_tx.send(admin_get_lora_config(my_node_num, rid)).await.is_err() {
                            let _ = reply.send(Err("meshtastic client disconnected".into()));
                        } else {
                            pending_admin = Some(PendingMeshtasticAdmin::GetLora { request_id: rid, reply });
                        }
                    }
                    MeshtasticAdminRequest::SetLoRaConfig { config, reply } => {
                        let rid = packet_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
                        };
                        let passkey = last_session_passkey.clone();
                        if cmd_tx.send(admin_set_lora_config(my_node_num, rid, lora, passkey)).await.is_err() {
                            let _ = reply.send(Err("meshtastic client disconnected".into()));
                        } else {
                            pending_admin = Some(PendingMeshtasticAdmin::SetLora { request_id: rid, reply });
                        }
                    }
                    MeshtasticAdminRequest::GetOwner { reply } => {
                        let rid = packet_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if cmd_tx.send(admin_get_owner(my_node_num, rid)).await.is_err() {
                            let _ = reply.send(Err("meshtastic client disconnected".into()));
                        } else {
                            pending_admin = Some(PendingMeshtasticAdmin::GetOwner { request_id: rid, reply });
                        }
                    }
                    MeshtasticAdminRequest::SetOwner { long_name, short_name, reply } => {
                        // For SetOwner we need the current values to merge — do a
                        // GetOwner first and then set. For simplicity, send SetOwner
                        // with only the provided fields; the device keeps others.
                        // We synthesize a User with empty fields for what we don't change.
                        let rid = packet_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        let user = User {
                            id: String::new(),
                            long_name: long_name.unwrap_or_default(),
                            short_name: short_name.unwrap_or_default(),
                            public_key: Vec::new(),
                        };
                        let passkey = last_session_passkey.clone();
                        if cmd_tx.send(admin_set_owner(my_node_num, rid, user, passkey)).await.is_err() {
                            let _ = reply.send(Err("meshtastic client disconnected".into()));
                        } else {
                            pending_admin = Some(PendingMeshtasticAdmin::SetOwner { request_id: rid, reply });
                        }
                    }
                    MeshtasticAdminRequest::GetSecurity { reply } => {
                        let rid = packet_counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        if cmd_tx.send(admin_get_security_config(my_node_num, rid)).await.is_err() {
                            let _ = reply.send(Err("meshtastic client disconnected".into()));
                        } else {
                            pending_admin = Some(PendingMeshtasticAdmin::GetSecurity { request_id: rid, reply });
                        }
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
        Some(PendingMeshtasticAdmin::SetLora { request_id, reply }) => {
            if data.reply_id != request_id && data.request_id != request_id {
                *pending_admin = Some(PendingMeshtasticAdmin::SetLora { request_id, reply });
                return false;
            }
            // Any matching admin packet is the ACK for a SET command.
            let _ = reply.send(Ok(()));
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
        Some(PendingMeshtasticAdmin::SetOwner { request_id, reply }) => {
            if data.reply_id != request_id && data.request_id != request_id {
                *pending_admin = Some(PendingMeshtasticAdmin::SetOwner { request_id, reply });
                return false;
            }
            let _ = reply.send(Ok(()));
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
            record_node_advert(host, node);
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

    host.advert_bus().upsert(pubkey, name, 0, lat_1e6, lon_1e6);
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
        };
        assert_eq!(text_payload(&packet), Some("hello".to_owned()));
    }
}
