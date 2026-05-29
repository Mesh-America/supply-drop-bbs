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
    #[prost(oneof = "from_radio::PayloadVariant", tags = "2, 3, 4, 7, 8")]
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

#[derive(Clone, PartialEq, Message)]
pub struct User {
    #[prost(string, tag = "1")]
    pub id: String,
    #[prost(string, tag = "2")]
    pub long_name: String,
    #[prost(string, tag = "3")]
    pub short_name: String,
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

pub const PORT_ADMIN_APP: i32 = 67;

#[derive(Clone, PartialEq, Message)]
pub struct AdminMessage {
    #[prost(oneof = "admin_message::PayloadVariant", tags = "6, 11, 13")]
    pub payload_variant: Option<admin_message::PayloadVariant>,
}

pub mod admin_message {
    use super::*;
    #[derive(Clone, PartialEq, Oneof)]
    pub enum PayloadVariant {
        #[prost(message, tag = "6")]
        GetConfigResponse(super::MtConfig),
        #[prost(message, tag = "11")]
        SetConfig(super::MtConfig),
        #[prost(int32, tag = "13")]
        GetConfigRequest(i32),
    }
}

#[derive(Clone, PartialEq, Message)]
pub struct MtConfig {
    #[prost(oneof = "mt_config::PayloadVariant", tags = "3")]
    pub payload_variant: Option<mt_config::PayloadVariant>,
}

pub mod mt_config {
    use super::*;
    #[derive(Clone, PartialEq, Oneof)]
    pub enum PayloadVariant {
        #[prost(message, tag = "3")]
        Lora(super::LoRaConfig),
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
    #[prost(float, tag = "14")]
    pub override_frequency: f32,
}

/// CONFIG_TYPE_LORA = 5
pub const CONFIG_TYPE_LORA: i32 = 5;

pub fn admin_get_lora_config(to_node: u32, request_id: u32) -> ToRadio {
    use prost::Message as _;
    let admin = AdminMessage {
        payload_variant: Some(admin_message::PayloadVariant::GetConfigRequest(
            CONFIG_TYPE_LORA,
        )),
    };
    ToRadio {
        payload_variant: Some(to_radio::PayloadVariant::Packet(MeshPacket {
            from: 0,
            to: to_node,
            channel: 0,
            payload_variant: Some(mesh_packet::PayloadVariant::Decoded(Data {
                portnum: PORT_ADMIN_APP,
                payload: admin.encode_to_vec(),
                want_response: true,
                dest: to_node,
                source: to_node,
                request_id,
                reply_id: 0,
            })),
            id: request_id,
            rx_time: 0,
            rx_snr: 0.0,
            hop_limit: 0,
            want_ack: false,
            priority: PRIORITY_RELIABLE,
            rx_rssi: 0,
            via_mqtt: false,
            hop_start: 0,
            public_key: Vec::new(),
        })),
    }
}

pub fn admin_set_lora_config(to_node: u32, request_id: u32, config: LoRaConfig) -> ToRadio {
    use prost::Message as _;
    let admin = AdminMessage {
        payload_variant: Some(admin_message::PayloadVariant::SetConfig(MtConfig {
            payload_variant: Some(mt_config::PayloadVariant::Lora(config)),
        })),
    };
    ToRadio {
        payload_variant: Some(to_radio::PayloadVariant::Packet(MeshPacket {
            from: 0,
            to: to_node,
            channel: 0,
            payload_variant: Some(mesh_packet::PayloadVariant::Decoded(Data {
                portnum: PORT_ADMIN_APP,
                payload: admin.encode_to_vec(),
                want_response: true,
                dest: to_node,
                source: to_node,
                request_id,
                reply_id: 0,
            })),
            id: request_id,
            rx_time: 0,
            rx_snr: 0.0,
            hop_limit: 0,
            want_ack: false,
            priority: PRIORITY_RELIABLE,
            rx_rssi: 0,
            via_mqtt: false,
            hop_start: 0,
            public_key: Vec::new(),
        })),
    }
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
