/// `>` — prefix byte on frames sent from the radio bridge to the app.
pub const FRAME_OUTBOUND_PREFIX: u8 = 0x3E;
/// `<` — prefix byte on frames sent from the app to the radio bridge.
pub const FRAME_INBOUND_PREFIX: u8 = 0x3C;

/// Maximum total frame size for frames we *send* (prefix + 2-byte length + payload).
///
/// Used by the transport layer to compute [`MAX_REPLY_BYTES`] so that outgoing
/// `SendTxtMsg` frames fit within the radio bridge's expected frame size.
///
/// [`MAX_REPLY_BYTES`]: bbs_mesh::transport::MAX_REPLY_BYTES
pub const MAX_FRAME_SIZE: usize = 172;

/// Maximum payload size for frames we *receive* from the radio bridge.
///
/// This is a sanity guard against a malformed length field causing a
/// large allocation or memory-limit panic. It is intentionally larger
/// than `MAX_FRAME_SIZE - 3` because the firmware can send structured
/// response frames (contact lists, advert records, etc.) whose payload
/// exceeds the outgoing text-message limit.
///
/// Setting this to 256 covers every valid MeshCore companion-protocol
/// frame while still catching obviously corrupt length values.
pub const MAX_PAYLOAD_SIZE: usize = 256;

pub const PUB_KEY_SIZE: usize = 32;
pub const MAX_PATH_SIZE: usize = 64;
pub const CONTACT_NAME_SIZE: usize = 32;

/// Protocol version code reported in RESP_CODE_DEVICE_INFO.
/// Version 10+ enables multi-byte path hashes.
pub const FIRMWARE_VER_CODE: u8 = 10;

// ── Commands (app → radio) ──────────────────────────────────────────────────
pub const CMD_APP_START: u8 = 1;
pub const CMD_SEND_TXT_MSG: u8 = 2;
pub const CMD_SEND_CHANNEL_TXT_MSG: u8 = 3;
pub const CMD_GET_CONTACTS: u8 = 4;
pub const CMD_GET_DEVICE_TIME: u8 = 5;
pub const CMD_SET_DEVICE_TIME: u8 = 6;
pub const CMD_SEND_SELF_ADVERT: u8 = 7;
pub const CMD_SET_ADVERT_NAME: u8 = 8;
pub const CMD_ADD_UPDATE_CONTACT: u8 = 9;
pub const CMD_SYNC_NEXT_MESSAGE: u8 = 10;
pub const CMD_SET_RADIO_PARAMS: u8 = 11;
pub const CMD_SET_RADIO_TX_POWER: u8 = 12;
pub const CMD_RESET_PATH: u8 = 13;
pub const CMD_SET_ADVERT_LATLON: u8 = 14;
pub const CMD_REMOVE_CONTACT: u8 = 15;
pub const CMD_SHARE_CONTACT: u8 = 16;
pub const CMD_EXPORT_CONTACT: u8 = 17;
pub const CMD_IMPORT_CONTACT: u8 = 18;
pub const CMD_REBOOT: u8 = 19;
pub const CMD_GET_BATT_AND_STORAGE: u8 = 20;
pub const CMD_SET_TUNING_PARAMS: u8 = 21;
pub const CMD_DEVICE_QUERY: u8 = 22;
pub const CMD_EXPORT_PRIVATE_KEY: u8 = 23;
pub const CMD_IMPORT_PRIVATE_KEY: u8 = 24;
pub const CMD_SEND_RAW_DATA: u8 = 25;
pub const CMD_SEND_LOGIN: u8 = 26;
pub const CMD_SEND_STATUS_REQ: u8 = 27;
pub const CMD_HAS_CONNECTION: u8 = 28;
pub const CMD_LOGOUT: u8 = 29;
pub const CMD_GET_CONTACT_BY_KEY: u8 = 30;
pub const CMD_GET_CHANNEL: u8 = 31;
pub const CMD_SET_CHANNEL: u8 = 32;
pub const CMD_SIGN_START: u8 = 33;
pub const CMD_SIGN_DATA: u8 = 34;
pub const CMD_SIGN_FINISH: u8 = 35;
pub const CMD_SEND_TRACE_PATH: u8 = 36;
pub const CMD_SET_DEVICE_PIN: u8 = 37;
pub const CMD_SET_OTHER_PARAMS: u8 = 38;
pub const CMD_SEND_TELEMETRY_REQ: u8 = 39;
pub const CMD_GET_CUSTOM_VARS: u8 = 40;
pub const CMD_SET_CUSTOM_VAR: u8 = 41;
pub const CMD_GET_ADVERT_PATH: u8 = 42;
pub const CMD_GET_TUNING_PARAMS: u8 = 43;
pub const CMD_SEND_BINARY_REQ: u8 = 50;
pub const CMD_FACTORY_RESET: u8 = 51;
pub const CMD_SEND_PATH_DISCOVERY_REQ: u8 = 52;
pub const CMD_SET_FLOOD_SCOPE: u8 = 54;
pub const CMD_SEND_CONTROL_DATA: u8 = 55;
pub const CMD_GET_STATS: u8 = 56;
pub const CMD_SEND_ANON_REQ: u8 = 57;
pub const CMD_SET_AUTOADD_CONFIG: u8 = 58;
pub const CMD_GET_AUTOADD_CONFIG: u8 = 59;
pub const CMD_SET_PATH_HASH_MODE: u8 = 61;

// ── Response codes (radio → app, solicited) ─────────────────────────────────
pub const RESP_CODE_OK: u8 = 0;
pub const RESP_CODE_ERR: u8 = 1;
pub const RESP_CODE_CONTACTS_START: u8 = 2;
pub const RESP_CODE_CONTACT: u8 = 3;
pub const RESP_CODE_END_OF_CONTACTS: u8 = 4;
pub const RESP_CODE_SELF_INFO: u8 = 5;
pub const RESP_CODE_SENT: u8 = 6;
pub const RESP_CODE_CONTACT_MSG_RECV: u8 = 7;
pub const RESP_CODE_CHANNEL_MSG_RECV: u8 = 8;
pub const RESP_CODE_CURR_TIME: u8 = 9;
pub const RESP_CODE_NO_MORE_MESSAGES: u8 = 10;
pub const RESP_CODE_EXPORT_CONTACT: u8 = 11;
pub const RESP_CODE_BATT_AND_STORAGE: u8 = 12;
pub const RESP_CODE_DEVICE_INFO: u8 = 13;
pub const RESP_CODE_PRIVATE_KEY: u8 = 14;
pub const RESP_CODE_DISABLED: u8 = 15;
pub const RESP_CODE_CONTACT_MSG_RECV_V3: u8 = 16;
pub const RESP_CODE_CHANNEL_MSG_RECV_V3: u8 = 17;
pub const RESP_CODE_CHANNEL_INFO: u8 = 18;
pub const RESP_CODE_SIGN_START: u8 = 19;
pub const RESP_CODE_SIGNATURE: u8 = 20;
pub const RESP_CODE_CUSTOM_VARS: u8 = 21;
pub const RESP_CODE_ADVERT_PATH: u8 = 22;
pub const RESP_CODE_TUNING_PARAMS: u8 = 23;
pub const RESP_CODE_STATS: u8 = 24;
pub const RESP_CODE_AUTOADD_CONFIG: u8 = 25;

// ── Push codes (radio → app, unsolicited) ───────────────────────────────────
pub const PUSH_CODE_ADVERT: u8 = 0x80;
pub const PUSH_CODE_PATH_UPDATED: u8 = 0x81;
pub const PUSH_CODE_SEND_CONFIRMED: u8 = 0x82;
pub const PUSH_CODE_MSG_WAITING: u8 = 0x83;
pub const PUSH_CODE_RAW_DATA: u8 = 0x84;
pub const PUSH_CODE_LOGIN_SUCCESS: u8 = 0x85;
pub const PUSH_CODE_LOGIN_FAIL: u8 = 0x86;
pub const PUSH_CODE_STATUS_RESPONSE: u8 = 0x87;
pub const PUSH_CODE_LOG_RX_DATA: u8 = 0x88;
pub const PUSH_CODE_TRACE_DATA: u8 = 0x89;
pub const PUSH_CODE_NEW_ADVERT: u8 = 0x8A;
pub const PUSH_CODE_TELEMETRY_RESPONSE: u8 = 0x8B;
pub const PUSH_CODE_BINARY_RESPONSE: u8 = 0x8C;
pub const PUSH_CODE_PATH_DISCOVERY_RESPONSE: u8 = 0x8D;
pub const PUSH_CODE_CONTROL_DATA: u8 = 0x8E;
pub const PUSH_CODE_CONTACT_DELETED: u8 = 0x8F;
pub const PUSH_CODE_CONTACTS_FULL: u8 = 0x90;

// ── Error codes ─────────────────────────────────────────────────────────────
pub const ERR_CODE_UNSUPPORTED_CMD: u8 = 1;
pub const ERR_CODE_NOT_FOUND: u8 = 2;
pub const ERR_CODE_TABLE_FULL: u8 = 3;
pub const ERR_CODE_BAD_STATE: u8 = 4;
pub const ERR_CODE_FILE_IO_ERROR: u8 = 5;
pub const ERR_CODE_ILLEGAL_ARG: u8 = 6;

// ── ADV types ───────────────────────────────────────────────────────────────
pub const ADV_TYPE_CHAT: u8 = 1;
pub const ADV_TYPE_REPEATER: u8 = 2;
pub const ADV_TYPE_ROOM: u8 = 3;
pub const ADV_TYPE_SENSOR: u8 = 4;

// ── Text types ──────────────────────────────────────────────────────────────
pub const TXT_TYPE_PLAIN: u8 = 0;
pub const TXT_TYPE_CLI_DATA: u8 = 1;
pub const TXT_TYPE_SIGNED_PLAIN: u8 = 2;

// ── Message send results ─────────────────────────────────────────────────────
pub const MSG_SEND_FAILED: u8 = 0;
pub const MSG_SEND_SENT_FLOOD: u8 = 1;
pub const MSG_SEND_SENT_DIRECT: u8 = 2;

// ── Advert location policy ───────────────────────────────────────────────────
pub const ADVERT_LOC_NONE: u8 = 0;
pub const ADVERT_LOC_SHARE: u8 = 1;

// ── App target version sent in CMD_APP_START / CMD_DEVICE_QUERY ─────────────
/// Requesting v3 message format (ContactMsgRecvV3 / ChannelMsgRecvV3 with SNR).
pub const APP_TARGET_VER_V3: u8 = 3;
