use crate::constants::MAX_PATH_SIZE;

/// Information about the local companion node, returned by CMD_APP_START.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelfInfo {
    pub adv_type: u8,
    pub tx_power_dbm: u8,
    pub pubkey: [u8; 32],
    /// Degrees × 1_000_000 (little-endian i32 on the wire).
    pub latitude: i32,
    /// Degrees × 1_000_000.
    pub longitude: i32,
    pub multi_acks: u8,
    pub advert_loc_policy: u8,
    /// Packed: bits 0-1 = base telemetry mode, 2-3 = location, 4-5 = environment.
    pub telemetry_modes: u8,
    pub manual_add_contacts: u8,
    /// Frequency in kHz (sent on wire in kHz, not Hz).
    pub frequency_khz: u32,
    pub bandwidth_hz: u32,
    pub spreading_factor: u8,
    pub coding_rate: u8,
    /// Variable-length UTF-8, max 31 bytes (not null-padded on wire).
    pub node_name: String,
}

/// Firmware/capability info returned by CMD_DEVICE_QUERY.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub firmware_ver: u8,
    /// max_contacts_div_2 × 2 (bridge caps at 510; true limit stored elsewhere).
    pub max_contacts: u16,
    pub max_channels: u8,
    pub ble_pin: u32,
    pub build_date: String,
    pub manufacturer: String,
    pub version: String,
    pub client_repeat: u8,
    /// 0=1-byte path hashes, 1=2-byte, 2=3-byte (firmware v10+).
    pub path_hash_mode: u8,
}

/// A contact/node entry, returned in CMD_GET_CONTACTS or pushed on advert.
///
/// `out_path_len` stores the raw wire byte interpreted as `i8`: −1 (0xFF)
/// means the path is unknown; 0 means direct; positive = hop count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contact {
    pub pubkey: [u8; 32],
    pub adv_type: u8,
    /// Bit 0 = favourite; other bits reserved.
    pub flags: u8,
    /// Wire byte 0xFF decoded as −1 (unknown/no path).
    pub out_path_len: i8,
    pub out_path: [u8; MAX_PATH_SIZE],
    pub name: String,
    pub last_advert_timestamp: u32,
    /// Degrees × 1_000_000.
    pub gps_lat: i32,
    pub gps_lon: i32,
    pub lastmod: u32,
}

/// A direct message received from a contact (v1/v2 or v3 format).
///
/// Both `RESP_CODE_CONTACT_MSG_RECV` and `RESP_CODE_CONTACT_MSG_RECV_V3`
/// decode into this type; `snr` is `None` for v1/v2 frames.
#[derive(Debug, Clone, PartialEq)]
pub struct ContactMsg {
    pub sender_key_prefix: [u8; 6],
    pub path_len: u8,
    pub txt_type: u8,
    pub timestamp: u32,
    pub text: String,
    /// SNR in dB; only present in v3 frames.
    pub snr: Option<f32>,
}

/// A channel (group) message received (v1/v2 or v3 format).
///
/// The `text` field follows the MeshCore convention "SenderName: MessageText".
#[derive(Debug, Clone, PartialEq)]
pub struct ChannelMsg {
    pub channel_idx: u8,
    pub path_len: u8,
    pub txt_type: u8,
    pub timestamp: u32,
    pub text: String,
    /// SNR in dB; only present in v3 frames.
    pub snr: Option<f32>,
}

/// Result of a sent-message command (RESP_CODE_SENT).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentResult {
    pub is_flood: bool,
    pub expected_ack: u32,
    pub timeout_ms: u32,
}

/// Battery and storage info (RESP_CODE_BATT_AND_STORAGE).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BattAndStorage {
    pub millivolts: u16,
    pub used_kb: u32,
    pub total_kb: u32,
}

/// Exported contact data (RESP_CODE_EXPORT_CONTACT), 73 bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedContact {
    pub pubkey: [u8; 32],
    pub adv_type: u8,
    pub name: String,
    pub gps_lat: i32,
    pub gps_lon: i32,
}

/// Channel configuration (RESP_CODE_CHANNEL_INFO).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelInfo {
    pub channel_idx: u8,
    pub name: String,
    /// First 16 bytes of the channel secret (PSK).
    pub secret: [u8; 16],
}

/// Successful login response pushed after CMD_SEND_LOGIN.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginSuccess {
    pub is_admin: bool,
    pub pubkey_prefix: [u8; 6],
    pub tag: u32,
    pub acl_permissions: u8,
    pub firmware_ver_level: u8,
}
