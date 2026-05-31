//! Minimal Meshtastic protobuf model used by the transport.
//!
//! These structs intentionally cover only the fields Supply Drop needs for
//! direct text messages, node adverts, config startup, and outbound replies.
//! Unknown protobuf fields are ignored by `prost`, so newer firmware can add
//! fields without breaking this transport.

use prost::{Message, Oneof};

pub const BROADCAST_ADDR: u32 = u32::MAX;
pub const PORT_TEXT_MESSAGE_APP: i32 = 1;
pub const PORT_NODEINFO_APP: i32 = 4;
pub const PRIORITY_RELIABLE: i32 = 70;

#[derive(Clone, PartialEq, Message)]
pub struct ToRadio {
    #[prost(oneof = "to_radio::PayloadVariant", tags = "1, 3, 4, 7")]
    pub payload_variant: Option<to_radio::PayloadVariant>,
}

pub mod to_radio {
    use super::*;

    #[derive(Clone, PartialEq, Oneof)]
    pub enum PayloadVariant {
        #[prost(message, tag = "1")]
        Packet(super::MeshPacket),
        #[prost(uint32, tag = "3")]
        WantConfigId(u32),
        #[prost(bool, tag = "4")]
        Disconnect(bool),
        #[prost(message, tag = "7")]
        Heartbeat(super::Heartbeat),
    }
}

#[derive(Clone, PartialEq, Message)]
pub struct FromRadio {
    #[prost(uint32, tag = "1")]
    pub id: u32,
    #[prost(oneof = "from_radio::PayloadVariant", tags = "2, 3, 4, 5, 7, 8")]
    pub payload_variant: Option<from_radio::PayloadVariant>,
}

pub mod from_radio {
    use super::*;

    #[derive(Clone, PartialEq, Oneof)]
    pub enum PayloadVariant {
        #[prost(message, tag = "2")]
        Packet(super::MeshPacket),
        #[prost(message, tag = "3")]
        MyInfo(super::MyNodeInfo),
        #[prost(message, tag = "4")]
        NodeInfo(super::NodeInfo),
        /// `Config` section streamed during the initial `want_config` sync
        /// (e.g. the current LoRa config). Lets us read the device's live
        /// settings without an explicit admin GET.
        #[prost(message, tag = "5")]
        Config(super::MtConfig),
        #[prost(uint32, tag = "7")]
        ConfigCompleteId(u32),
        #[prost(bool, tag = "8")]
        Rebooted(bool),
    }
}

#[derive(Clone, PartialEq, Message)]
pub struct MeshPacket {
    #[prost(fixed32, tag = "1")]
    pub from: u32,
    #[prost(fixed32, tag = "2")]
    pub to: u32,
    #[prost(uint32, tag = "3")]
    pub channel: u32,
    #[prost(oneof = "mesh_packet::PayloadVariant", tags = "4, 5")]
    pub payload_variant: Option<mesh_packet::PayloadVariant>,
    #[prost(fixed32, tag = "6")]
    pub id: u32,
    #[prost(fixed32, tag = "7")]
    pub rx_time: u32,
    #[prost(float, tag = "8")]
    pub rx_snr: f32,
    #[prost(uint32, tag = "9")]
    pub hop_limit: u32,
    #[prost(bool, tag = "10")]
    pub want_ack: bool,
    #[prost(enumeration = "MeshPacketPriority", tag = "11")]
    pub priority: i32,
    #[prost(int32, tag = "12")]
    pub rx_rssi: i32,
    #[prost(bool, tag = "14")]
    pub via_mqtt: bool,
    #[prost(uint32, tag = "15")]
    pub hop_start: u32,
    #[prost(bytes = "vec", tag = "16")]
    pub public_key: Vec<u8>,
    /// Marks the packet as using public-key cryptography. The Meshtastic app
    /// sets this on admin messages to the local node; firmware with PKC enabled
    /// and the legacy admin channel disabled requires it, otherwise the admin
    /// request is silently dropped.
    #[prost(bool, tag = "17")]
    pub pki_encrypted: bool,
}

pub mod mesh_packet {
    use super::*;

    #[derive(Clone, PartialEq, Oneof)]
    pub enum PayloadVariant {
        #[prost(message, tag = "4")]
        Decoded(super::Data),
        #[prost(bytes, tag = "5")]
        Encrypted(Vec<u8>),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum MeshPacketPriority {
    Unset = 0,
    Background = 10,
    Default = 64,
    Reliable = 70,
    Response = 80,
    High = 100,
    Alert = 110,
    Ack = 120,
}

#[derive(Clone, PartialEq, Message)]
pub struct Data {
    #[prost(enumeration = "PortNum", tag = "1")]
    pub portnum: i32,
    #[prost(bytes = "vec", tag = "2")]
    pub payload: Vec<u8>,
    #[prost(bool, tag = "3")]
    pub want_response: bool,
    #[prost(fixed32, tag = "4")]
    pub dest: u32,
    #[prost(fixed32, tag = "5")]
    pub source: u32,
    #[prost(fixed32, tag = "6")]
    pub request_id: u32,
    #[prost(fixed32, tag = "7")]
    pub reply_id: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, prost::Enumeration)]
#[repr(i32)]
pub enum PortNum {
    Unknown = 0,
    TextMessage = 1,
    Position = 3,
    Nodeinfo = 4,
}

#[derive(Clone, PartialEq, Message)]
pub struct MyNodeInfo {
    #[prost(uint32, tag = "1")]
    pub my_node_num: u32,
    #[prost(uint32, tag = "8")]
    pub reboot_count: u32,
    #[prost(uint32, tag = "11")]
    pub min_app_version: u32,
    #[prost(bytes = "vec", tag = "12")]
    pub device_id: Vec<u8>,
    #[prost(string, tag = "13")]
    pub pio_env: String,
    #[prost(uint32, tag = "15")]
    pub nodedb_count: u32,
}

#[derive(Clone, PartialEq, Message)]
pub struct NodeInfo {
    #[prost(uint32, tag = "1")]
    pub num: u32,
    #[prost(message, optional, tag = "2")]
    pub user: Option<User>,
    #[prost(message, optional, tag = "3")]
    pub position: Option<Position>,
    #[prost(float, tag = "4")]
    pub snr: f32,
    #[prost(fixed32, tag = "5")]
    pub last_heard: u32,
}

/// Meshtastic node owner/user info (`mesh.proto User`).
///
/// Used in `NodeInfo` (received during config sync) and in admin owner
/// get/set operations (`AdminMessage` fields 3/4/32).
#[derive(Clone, PartialEq, Message)]
pub struct User {
    /// Unique node ID string, e.g. `"!aabbccdd"` (hex of the 32-bit node number).
    #[prost(string, tag = "1")]
    pub id: String,
    /// Full display name (~39 chars max in firmware).
    #[prost(string, tag = "2")]
    pub long_name: String,
    /// Short display name shown on OLED/mesh maps. Firmware enforces ≤ 4 chars.
    #[prost(string, tag = "3")]
    pub short_name: String,
    /// Device role (`Config.DeviceConfig.Role`): 0=CLIENT, 1=CLIENT_MUTE,
    /// 2=ROUTER, 3=ROUTER_CLIENT, 4=REPEATER, 5=TRACKER, 6=SENSOR, 7=TAK,
    /// 8=CLIENT_HIDDEN, 9=LOST_AND_FOUND, 10=TAK_TRACKER, 11=ROUTER_LATE.
    #[prost(int32, tag = "7")]
    pub role: i32,
    /// Node's Curve25519 public key (32 bytes), broadcast on the mesh for PKC DMs.
    #[prost(bytes = "vec", tag = "8")]
    pub public_key: Vec<u8>,
}

#[derive(Clone, PartialEq, Message)]
pub struct Position {
    #[prost(sfixed32, optional, tag = "1")]
    pub latitude_i: Option<i32>,
    #[prost(sfixed32, optional, tag = "2")]
    pub longitude_i: Option<i32>,
    #[prost(int32, tag = "3")]
    pub altitude: i32,
    #[prost(fixed32, tag = "4")]
    pub time: u32,
}

#[derive(Clone, PartialEq, Message)]
pub struct Heartbeat {
    #[prost(uint32, tag = "1")]
    pub nonce: u32,
}

pub fn want_config(id: u32) -> ToRadio {
    ToRadio {
        payload_variant: Some(to_radio::PayloadVariant::WantConfigId(id)),
    }
}

pub fn heartbeat(nonce: u32) -> ToRadio {
    ToRadio {
        payload_variant: Some(to_radio::PayloadVariant::Heartbeat(Heartbeat { nonce })),
    }
}

pub fn disconnect() -> ToRadio {
    ToRadio {
        payload_variant: Some(to_radio::PayloadVariant::Disconnect(true)),
    }
}

pub fn direct_text_packet(
    to: u32,
    text: String,
    packet_id: u32,
    hop_limit: u32,
    want_ack: bool,
) -> ToRadio {
    ToRadio {
        payload_variant: Some(to_radio::PayloadVariant::Packet(MeshPacket {
            from: 0,
            to,
            channel: 0,
            payload_variant: Some(mesh_packet::PayloadVariant::Decoded(Data {
                portnum: PORT_TEXT_MESSAGE_APP,
                payload: text.into_bytes(),
                want_response: false,
                dest: 0,
                source: 0,
                request_id: 0,
                reply_id: 0,
            })),
            id: packet_id,
            rx_time: 0,
            rx_snr: 0.0,
            hop_limit,
            want_ack,
            priority: PRIORITY_RELIABLE,
            rx_rssi: 0,
            via_mqtt: false,
            hop_start: 0,
            public_key: Vec::new(),
            pki_encrypted: false,
        })),
    }
}

/// Broadcast our own node info (owner `User`) to the whole mesh.
///
/// This is the Meshtastic equivalent of MeshCore's self-advert: it makes the
/// radio transmit a `NODEINFO_APP` packet so other nodes add us to their node
/// list. The firmware otherwise only broadcasts node info on boot and on a slow
/// periodic timer (~3 h by default), so without this a freshly-configured BBS
/// node can take hours to appear on neighbouring devices.
///
/// `from` is left 0 (local origin); the firmware stamps it with our node
/// number before transmit. `want_response` is false — this is an announcement,
/// not a request for replies.
pub fn nodeinfo_broadcast(packet_id: u32, user: User, hop_limit: u32, want_ack: bool) -> ToRadio {
    use prost::Message as _;
    ToRadio {
        payload_variant: Some(to_radio::PayloadVariant::Packet(MeshPacket {
            from: 0,
            to: BROADCAST_ADDR,
            channel: 0,
            payload_variant: Some(mesh_packet::PayloadVariant::Decoded(Data {
                portnum: PORT_NODEINFO_APP,
                payload: user.encode_to_vec(),
                want_response: false,
                dest: 0,
                source: 0,
                request_id: 0,
                reply_id: 0,
            })),
            id: packet_id,
            rx_time: 0,
            rx_snr: 0.0,
            hop_limit,
            want_ack,
            priority: PRIORITY_RELIABLE,
            rx_rssi: 0,
            via_mqtt: false,
            hop_start: 0,
            public_key: Vec::new(),
            pki_encrypted: false,
        })),
    }
}

pub fn node_key(node_num: u32) -> [u8; 6] {
    let n = node_num.to_be_bytes();
    [b'M', b'T', n[0], n[1], n[2], n[3]]
}

pub fn synthetic_pubkey(node_num: u32) -> [u8; 32] {
    let mut key = [0u8; 32];
    key[0] = b'M';
    key[1] = b'T';
    key[2..6].copy_from_slice(&node_num.to_be_bytes());
    key
}

/// Meshtastic `PortNum::ADMIN_APP`. This is **6**, not 67 — 67 is
/// `TELEMETRY_APP`. Sending admin messages on 67 makes the device's telemetry
/// module consume them and the admin module never processes them.
pub const PORT_ADMIN_APP: i32 = 6;
/// `config_type` for `GetConfigRequest` — LoRa radio config (`Config.lora`, field 6).
pub const CONFIG_TYPE_LORA: i32 = 5;
/// `config_type` for `GetConfigRequest` — Security / PKC config (`Config.security`, field 8).
pub const CONFIG_TYPE_SECURITY: i32 = 7;
/// `config_type` for `GetConfigRequest` — session key. Requesting this opens the
/// admin session for the connection; current Meshtastic firmware silently drops
/// admin *writes* that aren't preceded by it. The Meshtastic app sends this
/// before every admin set.
pub const CONFIG_TYPE_SESSIONKEY: i32 = 8;

// ── Admin proto types (admin.proto + mesh.proto, subset) ──────────────────────
// Note: `User` is already defined above (re-used for NodeInfo; same fields).

/// Meshtastic security config (`config.proto Config.SecurityConfig`, subset).
#[derive(Clone, PartialEq, Message)]
pub struct SecurityConfig {
    /// Node's Curve25519 public key — shared with mesh peers.
    #[prost(bytes = "vec", tag = "1")]
    pub public_key: Vec<u8>,
    /// Node's Curve25519 private key (on-device; only visible if serial is enabled).
    #[prost(bytes = "vec", tag = "2")]
    pub private_key: Vec<u8>,
    /// Allow admin commands over the legacy unencrypted admin channel.
    #[prost(bool, tag = "8")]
    pub admin_channel_enabled: bool,
}

/// Meshtastic admin message (`admin.proto AdminMessage`).
///
/// Field numbers match the current Meshtastic admin.proto.  The `session_passkey`
/// (field 101) is a replay-attack guard: the device returns it in every GET
/// response and the client must echo it back in SET commands within 300 s.
#[derive(Clone, PartialEq, Message)]
pub struct AdminMessage {
    #[prost(oneof = "admin_message::PayloadVariant", tags = "3, 4, 5, 6, 32, 34")]
    pub payload_variant: Option<admin_message::PayloadVariant>,
    /// Replay-attack guard — echo back in all SET commands.
    #[prost(bytes = "vec", tag = "101")]
    pub session_passkey: Vec<u8>,
}

pub mod admin_message {
    use super::*;
    #[derive(Clone, PartialEq, Oneof)]
    pub enum PayloadVariant {
        /// Request the node's owner/user info (send `true`).
        #[prost(bool, tag = "3")]
        GetOwnerRequest(bool),
        /// Node's owner/user info (response to `GetOwnerRequest`).
        #[prost(message, tag = "4")]
        GetOwnerResponse(super::User),
        /// Request a config type (`ConfigType` enum: 5 = LoRa, 7 = Security).
        #[prost(int32, tag = "5")]
        GetConfigRequest(i32),
        /// Config payload (response to `GetConfigRequest`).
        #[prost(message, tag = "6")]
        GetConfigResponse(super::MtConfig),
        /// Set a fixed GPS position on the node (enables fixed-position mode).
        #[prost(message, tag = "41")]
        SetFixedPosition(super::Position),
        /// Clear any fixed position and disable fixed-position mode.
        #[prost(bool, tag = "42")]
        RemoveFixedPosition(bool),
        /// Set the node's clock (Unix seconds). Does not reboot the device.
        #[prost(uint32, tag = "43")]
        SetTimeOnly(u32),
        /// Update the node's owner/user info.
        #[prost(message, tag = "32")]
        SetOwner(super::User),
        /// Update a config section.
        #[prost(message, tag = "34")]
        SetConfig(super::MtConfig),
    }
}

/// Subset of Meshtastic `Config` (`config.proto Config`).
#[derive(Clone, PartialEq, Message)]
pub struct MtConfig {
    #[prost(oneof = "mt_config::PayloadVariant", tags = "6, 8")]
    pub payload_variant: Option<mt_config::PayloadVariant>,
}

pub mod mt_config {
    use super::*;
    #[derive(Clone, PartialEq, Oneof)]
    pub enum PayloadVariant {
        /// `Config.lora` — LoRa radio parameters (field 6 in Config oneof).
        #[prost(message, tag = "6")]
        Lora(super::LoRaConfig),
        /// `Config.security` — PKC / key settings (field 8 in Config oneof).
        #[prost(message, tag = "8")]
        Security(super::SecurityConfig),
    }
}

#[derive(Clone, PartialEq, Message)]
pub struct LoRaConfig {
    #[prost(bool, tag = "1")]
    pub use_preset: bool,
    #[prost(int32, tag = "2")]
    pub modem_preset: i32,
    #[prost(uint32, tag = "3")]
    pub bandwidth: u32,
    #[prost(uint32, tag = "4")]
    pub spread_factor: u32,
    #[prost(uint32, tag = "5")]
    pub coding_rate: u32,
    #[prost(float, tag = "6")]
    pub frequency_offset: f32,
    #[prost(int32, tag = "7")]
    pub region: i32,
    #[prost(uint32, tag = "8")]
    pub hop_limit: u32,
    #[prost(bool, tag = "9")]
    pub tx_enabled: bool,
    #[prost(int32, tag = "10")]
    pub tx_power: i32,
    #[prost(uint32, tag = "11")]
    pub channel_num: u32,
    /// SX126x RX boosted gain — improves receive sensitivity on SX126x radios.
    #[prost(bool, tag = "13")]
    pub sx126x_rx_boosted_gain: bool,
    #[prost(float, tag = "14")]
    pub override_frequency: f32,
    /// Ignore packets that arrived over MQTT.
    #[prost(bool, tag = "104")]
    pub ignore_mqtt: bool,
}

// ── Admin packet helpers ──────────────────────────────────────────────────────

/// Wrap an encoded `AdminMessage` in a `ToRadio` admin packet.
fn admin_packet(to_node: u32, request_id: u32, admin: AdminMessage) -> ToRadio {
    use prost::Message as _;
    ToRadio {
        payload_variant: Some(to_radio::PayloadVariant::Packet(MeshPacket {
            from: 0,
            to: to_node,
            channel: 0,
            payload_variant: Some(mesh_packet::PayloadVariant::Decoded(Data {
                portnum: PORT_ADMIN_APP,
                payload: admin.encode_to_vec(),
                want_response: true,
                // dest/source/request_id/reply_id must be 0 on an outbound
                // request — they are only populated on *responses*. Setting
                // source/dest to the node's own number makes the firmware treat
                // the packet as self-originated and silently ignore it (no admin
                // response, and SET commands never apply). The response is
                // correlated via the MeshPacket `id` below, which the device
                // echoes back in the response's `request_id`.
                dest: 0,
                source: 0,
                request_id: 0,
                reply_id: 0,
            })),
            id: request_id,
            rx_time: 0,
            rx_snr: 0.0,
            hop_limit: 3,
            want_ack: true,
            priority: PRIORITY_RELIABLE,
            rx_rssi: 0,
            via_mqtt: false,
            hop_start: 0,
            public_key: Vec::new(),
            // Plaintext local-serial admin. Must be false: with it set, the
            // firmware tries to PKI-decrypt our plaintext payload into garbage
            // ("Can't decode protobuf"). The local trusted path (from==0) needs
            // no encryption.
            pki_encrypted: false,
        })),
    }
}

/// Build a `GetConfigRequest` for the LoRa config.
pub fn admin_get_lora_config(to_node: u32, request_id: u32) -> ToRadio {
    admin_packet(
        to_node,
        request_id,
        AdminMessage {
            payload_variant: Some(admin_message::PayloadVariant::GetConfigRequest(
                CONFIG_TYPE_LORA,
            )),
            session_passkey: Vec::new(),
        },
    )
}

/// Build a `SetConfig` for the LoRa config, echoing back the session passkey.
pub fn admin_set_lora_config(
    to_node: u32,
    request_id: u32,
    config: LoRaConfig,
    session_passkey: Vec<u8>,
) -> ToRadio {
    admin_packet(
        to_node,
        request_id,
        AdminMessage {
            payload_variant: Some(admin_message::PayloadVariant::SetConfig(MtConfig {
                payload_variant: Some(mt_config::PayloadVariant::Lora(config)),
            })),
            session_passkey,
        },
    )
}

/// Build a `GetOwnerRequest`.
pub fn admin_get_owner(to_node: u32, request_id: u32) -> ToRadio {
    admin_packet(
        to_node,
        request_id,
        AdminMessage {
            payload_variant: Some(admin_message::PayloadVariant::GetOwnerRequest(true)),
            session_passkey: Vec::new(),
        },
    )
}

/// Build a `SetOwner` command, echoing back the session passkey.
pub fn admin_set_owner(
    to_node: u32,
    request_id: u32,
    user: User,
    session_passkey: Vec<u8>,
) -> ToRadio {
    admin_packet(
        to_node,
        request_id,
        AdminMessage {
            payload_variant: Some(admin_message::PayloadVariant::SetOwner(user)),
            session_passkey,
        },
    )
}

/// Build a `SetTimeOnly` command to sync the device clock, echoing the passkey.
pub fn admin_set_time(
    to_node: u32,
    request_id: u32,
    unix_secs: u32,
    session_passkey: Vec<u8>,
) -> ToRadio {
    admin_packet(
        to_node,
        request_id,
        AdminMessage {
            payload_variant: Some(admin_message::PayloadVariant::SetTimeOnly(unix_secs)),
            session_passkey,
        },
    )
}

/// Build a `SetFixedPosition` command from decimal-degree coordinates.
///
/// Meshtastic stores latitude/longitude as integers in units of 1e-7 degrees.
pub fn admin_set_fixed_position(
    to_node: u32,
    request_id: u32,
    lat_deg: f64,
    lon_deg: f64,
    session_passkey: Vec<u8>,
) -> ToRadio {
    let position = Position {
        latitude_i: Some((lat_deg * 1e7) as i32),
        longitude_i: Some((lon_deg * 1e7) as i32),
        altitude: 0,
        time: 0,
    };
    admin_packet(
        to_node,
        request_id,
        AdminMessage {
            payload_variant: Some(admin_message::PayloadVariant::SetFixedPosition(position)),
            session_passkey,
        },
    )
}

/// Build a `RemoveFixedPosition` command (clears fixed position).
pub fn admin_remove_fixed_position(
    to_node: u32,
    request_id: u32,
    session_passkey: Vec<u8>,
) -> ToRadio {
    admin_packet(
        to_node,
        request_id,
        AdminMessage {
            payload_variant: Some(admin_message::PayloadVariant::RemoveFixedPosition(true)),
            session_passkey,
        },
    )
}

/// Build a `GetConfigRequest` for the Security config.
pub fn admin_get_security_config(to_node: u32, request_id: u32) -> ToRadio {
    admin_packet(
        to_node,
        request_id,
        AdminMessage {
            payload_variant: Some(admin_message::PayloadVariant::GetConfigRequest(
                CONFIG_TYPE_SECURITY,
            )),
            session_passkey: Vec::new(),
        },
    )
}

/// Build a session-key `GetConfigRequest`. Send this immediately before an
/// admin write to open the admin session — firmware drops writes that aren't
/// preceded by it.
pub fn admin_get_session_key(to_node: u32, request_id: u32) -> ToRadio {
    admin_packet(
        to_node,
        request_id,
        AdminMessage {
            payload_variant: Some(admin_message::PayloadVariant::GetConfigRequest(
                CONFIG_TYPE_SESSIONKEY,
            )),
            session_passkey: Vec::new(),
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_key_is_transport_namespaced() {
        assert_eq!(node_key(0x1234_5678), [b'M', b'T', 0x12, 0x34, 0x56, 0x78]);
    }

    #[test]
    fn direct_text_encodes_as_to_radio_packet() {
        let packet = direct_text_packet(0x0102_0304, "H".to_owned(), 42, 3, true);
        let bytes = packet.encode_to_vec();
        let decoded = ToRadio::decode(bytes.as_slice()).unwrap();
        let Some(to_radio::PayloadVariant::Packet(mesh)) = decoded.payload_variant else {
            panic!("expected packet");
        };
        assert_eq!(mesh.to, 0x0102_0304);
        assert_eq!(mesh.id, 42);
        assert!(mesh.want_ack);
        let Some(mesh_packet::PayloadVariant::Decoded(data)) = mesh.payload_variant else {
            panic!("expected decoded payload");
        };
        assert_eq!(data.portnum, PORT_TEXT_MESSAGE_APP);
        assert_eq!(data.payload, b"H");
    }
}
