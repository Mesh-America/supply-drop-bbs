//! Frame encoding and decoding for the companion-frame protocol.
//!
//! # Wire format
//!
//! Every frame on the wire is:
//! ```text
//! [prefix:1][length:u16-LE][payload:length]
//! ```
//!
//! - **Outbound** (radio → app): prefix = `0x3E` (`>`)
//! - **Inbound**  (app → radio): prefix = `0x3C` (`<`)
//!
//! `payload[0]` is always the command/response/push type byte.
//!
//! # Entry points
//!
//! - [`decode_inbound`] — parse a raw payload (no prefix/length) into [`InboundFrame`].
//! - [`encode_outbound`] — serialize an [`OutboundFrame`] into complete wire bytes
//!   (prefix + length + payload).
//! - [`strip_frame_header`] — validate and remove the 3-byte header from raw wire bytes.

use crate::{
    constants::*,
    error::FrameDecodeError,
    types::{
        BattAndStorage, ChannelInfo, ChannelMsg, Contact, ContactMsg, DeviceInfo, ExportedContact,
        LoginSuccess, SelfInfo, SentResult,
    },
};

// ── Inbound frame enum ────────────────────────────────────────────────────────

/// A frame received from the radio bridge (radio → app).
///
/// Solicited responses and unsolicited push notifications share this enum.
/// Variants for less-common frame types whose body layout is not yet
/// fully specified carry the raw body bytes for callers that need them.
#[derive(Debug, Clone, PartialEq)]
pub enum InboundFrame {
    // ─ Solicited responses ─────────────────────────────────────────────────
    Ok,
    Err {
        error_code: u8,
    },
    ContactsStart {
        count: u32,
    },
    Contact(Contact),
    EndOfContacts {
        most_recent_lastmod: u32,
    },
    SelfInfo(SelfInfo),
    Sent(SentResult),
    ContactMsgRecv(ContactMsg),
    ChannelMsgRecv(ChannelMsg),
    CurrTime {
        unix_time: u32,
    },
    NoMoreMessages,
    ExportContact(ExportedContact),
    BattAndStorage(BattAndStorage),
    DeviceInfo(DeviceInfo),
    PrivateKey {
        key: Box<[u8]>,
    },
    Disabled,
    ContactMsgRecvV3(ContactMsg),
    ChannelMsgRecvV3(ChannelMsg),
    ChannelInfo(ChannelInfo),
    AutoaddConfig {
        config: u8,
    },
    // Raw body for less-common resp codes we parse on demand:
    Stats {
        stats_type: u8,
        raw: Vec<u8>,
    },
    AdvertPath {
        raw: Vec<u8>,
    },
    TuningParams {
        raw: Vec<u8>,
    },
    CustomVars {
        csv: String,
    },

    // ─ Unsolicited push notifications ──────────────────────────────────────
    /// Short advertisement — just the 32-byte public key.
    Advert {
        pubkey: [u8; 32],
    },
    /// Full advertisement — same fields as a Contact entry.
    NewAdvert(Contact),
    /// A contact's outbound path was updated.
    PathUpdated {
        pubkey: [u8; 32],
    },
    /// Delivery acknowledgement for a sent message.
    SendConfirmed {
        crc: u32,
    },
    /// A message is waiting; call CMD_SYNC_NEXT_MESSAGE.
    MsgWaiting,
    RawData {
        snr_byte: i8,
        rssi_byte: i8,
        data: Vec<u8>,
    },
    LoginSuccess(LoginSuccess),
    LoginFail {
        pubkey_prefix: [u8; 6],
    },
    StatusResponse {
        pubkey_prefix: [u8; 6],
        raw: Vec<u8>,
    },
    LogRxData {
        snr_byte: i8,
        rssi_byte: i8,
        raw: Vec<u8>,
    },
    TelemetryResponse {
        pubkey_prefix: [u8; 6],
        raw: Vec<u8>,
    },
    BinaryResponse {
        tag: [u8; 4],
        raw: Vec<u8>,
    },
    PathDiscoveryResponse {
        raw: Vec<u8>,
    },
    ControlData {
        raw: Vec<u8>,
    },
    ContactDeleted {
        pubkey: [u8; 32],
    },
    ContactsFull,
    TraceData {
        raw: Vec<u8>,
    },

    /// Catch-all for type bytes we don't recognise.
    Unknown {
        type_byte: u8,
        payload: Vec<u8>,
    },
}

// ── Outbound frame enum ───────────────────────────────────────────────────────

/// A command frame sent from the app to the radio bridge (app → radio).
#[derive(Debug, Clone, PartialEq)]
pub enum OutboundFrame {
    /// Handshake — must be the first command after connecting.
    AppStart {
        app_target_version: u8,
    },
    /// Query firmware version and capabilities.
    DeviceQuery {
        app_target_version: u8,
    },
    /// Fetch all contacts modified since `since` (0 = all).
    GetContacts {
        since: u32,
    },
    /// Pop the next queued inbound message.
    SyncNextMessage,
    /// Send a direct text message to a contact.
    SendTxtMsg {
        txt_type: u8,
        attempt: u8,
        timestamp: u32,
        pubkey_prefix: [u8; 6],
        text: String,
    },
    /// Send a message to a channel (group).
    SendChannelTxtMsg {
        txt_type: u8,
        channel_idx: u8,
        text: String,
    },
    /// Send a login packet to a repeater contact.
    SendLogin {
        pubkey: [u8; 32],
        password: String,
    },
    GetDeviceTime,
    SetDeviceTime {
        unix_time: u32,
    },
    SendSelfAdvert {
        /// true = flood, false = direct.
        flood: bool,
    },
    SetAdvertName {
        name: String,
    },
    SetAdvertLatlon {
        lat_1e6: i32,
        lon_1e6: i32,
    },
    AddUpdateContact(crate::types::Contact),
    RemoveContact {
        pubkey: [u8; 32],
    },
    ResetPath {
        pubkey: [u8; 32],
    },
    GetContactByKey {
        pubkey: [u8; 32],
    },
    ShareContact {
        pubkey: [u8; 32],
    },
    ExportContact {
        pubkey: Option<[u8; 32]>,
    },
    ImportContact {
        data: Vec<u8>,
    },
    GetBattAndStorage,
    GetChannel {
        channel_idx: Option<u8>,
    },
    SetChannel {
        channel_idx: u8,
        name: String,
        /// 32-byte channel secret.
        secret: [u8; 32],
    },
    Logout {
        pubkey: [u8; 32],
    },
    GetStats {
        stats_type: u8,
    },
    SetFloodScope {
        key: Option<[u8; 16]>,
    },
    SetOtherParams {
        manual_add: u8,
        telemetry_modes: u8,
        advert_loc_policy: u8,
        multi_acks: u8,
    },
    SetAutoaddConfig {
        config: u8,
    },
    GetAutoaddConfig,
    SetPathHashMode {
        mode: u8,
    },
    /// Escape hatch for commands not yet modelled above.
    Raw {
        code: u8,
        body: Vec<u8>,
    },
}

// ── Low-level framing ─────────────────────────────────────────────────────────

/// Strip and validate the 3-byte wire header from raw bytes read off the socket.
///
/// The `raw` slice must start with a `0x3E` prefix byte followed by a 2-byte
/// little-endian payload length, followed by exactly that many payload bytes.
/// Returns a reference to the payload (starting with the type byte) on success.
pub fn strip_frame_header(raw: &[u8]) -> Result<&[u8], FrameDecodeError> {
    if raw.is_empty() {
        return Err(FrameDecodeError::WrongPrefix {
            expected: FRAME_OUTBOUND_PREFIX,
            got: 0,
        });
    }
    if raw[0] != FRAME_OUTBOUND_PREFIX {
        return Err(FrameDecodeError::WrongPrefix {
            expected: FRAME_OUTBOUND_PREFIX,
            got: raw[0],
        });
    }
    if raw.len() < 3 {
        return Err(FrameDecodeError::BodyTooShort {
            type_byte: 0,
            needed: 3,
            got: raw.len(),
        });
    }
    let len = u16::from_le_bytes([raw[1], raw[2]]) as usize;
    if len > MAX_PAYLOAD_SIZE {
        return Err(FrameDecodeError::PayloadTooLarge(len));
    }
    Ok(&raw[3..3 + len])
}

// ── Decode ────────────────────────────────────────────────────────────────────

/// Parse an inbound payload into an [`InboundFrame`].
///
/// `payload` must be the raw payload bytes (no prefix/length header).
/// `payload[0]` is the type byte.
pub fn decode_inbound(payload: &[u8]) -> Result<InboundFrame, FrameDecodeError> {
    if payload.is_empty() {
        return Err(FrameDecodeError::BodyTooShort {
            type_byte: 0,
            needed: 1,
            got: 0,
        });
    }
    let type_byte = payload[0];
    let body = &payload[1..];

    macro_rules! need {
        ($n:expr) => {
            if body.len() < $n {
                return Err(FrameDecodeError::BodyTooShort {
                    type_byte,
                    needed: $n,
                    got: body.len(),
                });
            }
        };
    }

    match type_byte {
        RESP_CODE_OK => Ok(InboundFrame::Ok),
        RESP_CODE_ERR => {
            need!(1);
            Ok(InboundFrame::Err {
                error_code: body[0],
            })
        }
        RESP_CODE_CONTACTS_START => {
            need!(4);
            Ok(InboundFrame::ContactsStart {
                count: r_u32(body, 0),
            })
        }
        RESP_CODE_CONTACT => {
            need!(147);
            Ok(InboundFrame::Contact(parse_contact(body)?))
        }
        RESP_CODE_END_OF_CONTACTS => {
            need!(4);
            Ok(InboundFrame::EndOfContacts {
                most_recent_lastmod: r_u32(body, 0),
            })
        }
        RESP_CODE_SELF_INFO => {
            need!(57);
            Ok(InboundFrame::SelfInfo(parse_self_info(body)?))
        }
        RESP_CODE_SENT => {
            need!(9);
            Ok(InboundFrame::Sent(parse_sent_result(body)))
        }
        RESP_CODE_CONTACT_MSG_RECV => {
            need!(12);
            Ok(InboundFrame::ContactMsgRecv(parse_contact_msg(body, None)?))
        }
        RESP_CODE_CHANNEL_MSG_RECV => {
            need!(7);
            Ok(InboundFrame::ChannelMsgRecv(parse_channel_msg(body, None)?))
        }
        RESP_CODE_CURR_TIME => {
            need!(4);
            Ok(InboundFrame::CurrTime {
                unix_time: r_u32(body, 0),
            })
        }
        RESP_CODE_NO_MORE_MESSAGES => Ok(InboundFrame::NoMoreMessages),
        RESP_CODE_EXPORT_CONTACT => {
            need!(73);
            Ok(InboundFrame::ExportContact(parse_exported_contact(body)?))
        }
        RESP_CODE_BATT_AND_STORAGE => {
            need!(10);
            Ok(InboundFrame::BattAndStorage(parse_batt_and_storage(body)))
        }
        RESP_CODE_DEVICE_INFO => {
            need!(81);
            Ok(InboundFrame::DeviceInfo(parse_device_info(body)?))
        }
        RESP_CODE_PRIVATE_KEY => {
            // Server exports 64-byte expanded key; accept any length ≥ 32.
            need!(32);
            Ok(InboundFrame::PrivateKey {
                key: body.to_vec().into_boxed_slice(),
            })
        }
        RESP_CODE_DISABLED => Ok(InboundFrame::Disabled),
        RESP_CODE_CONTACT_MSG_RECV_V3 => {
            // [snr_byte][reserved][reserved][sender_prefix×6][path_len][txt_type][ts×4][text]
            need!(15);
            let snr = (body[0] as i8) as f32 / 4.0;
            Ok(InboundFrame::ContactMsgRecvV3(parse_contact_msg(
                &body[3..],
                Some(snr),
            )?))
        }
        RESP_CODE_CHANNEL_MSG_RECV_V3 => {
            // [snr_byte][reserved][reserved][channel_idx][path_len][txt_type][ts×4][text]
            need!(10);
            let snr = (body[0] as i8) as f32 / 4.0;
            Ok(InboundFrame::ChannelMsgRecvV3(parse_channel_msg(
                &body[3..],
                Some(snr),
            )?))
        }
        RESP_CODE_CHANNEL_INFO => {
            // [channel_idx][name×32][secret×16]
            need!(49);
            Ok(InboundFrame::ChannelInfo(parse_channel_info(body)?))
        }
        RESP_CODE_AUTOADD_CONFIG => {
            need!(1);
            Ok(InboundFrame::AutoaddConfig { config: body[0] })
        }
        RESP_CODE_STATS => {
            need!(1);
            Ok(InboundFrame::Stats {
                stats_type: body[0],
                raw: body[1..].to_vec(),
            })
        }
        RESP_CODE_ADVERT_PATH => Ok(InboundFrame::AdvertPath { raw: body.to_vec() }),
        RESP_CODE_TUNING_PARAMS => Ok(InboundFrame::TuningParams { raw: body.to_vec() }),
        RESP_CODE_CUSTOM_VARS => {
            let s = std::str::from_utf8(body).map_err(|_| FrameDecodeError::InvalidUtf8)?;
            Ok(InboundFrame::CustomVars { csv: s.to_owned() })
        }

        PUSH_CODE_ADVERT => {
            need!(32);
            Ok(InboundFrame::Advert {
                pubkey: copy32(body),
            })
        }
        PUSH_CODE_NEW_ADVERT => {
            need!(147);
            Ok(InboundFrame::NewAdvert(parse_contact(body)?))
        }
        PUSH_CODE_PATH_UPDATED => {
            need!(32);
            Ok(InboundFrame::PathUpdated {
                pubkey: copy32(body),
            })
        }
        PUSH_CODE_SEND_CONFIRMED => {
            // [crc×4][zero×4]
            need!(8);
            Ok(InboundFrame::SendConfirmed {
                crc: r_u32(body, 0),
            })
        }
        PUSH_CODE_MSG_WAITING => Ok(InboundFrame::MsgWaiting),
        PUSH_CODE_RAW_DATA => {
            // [snr_byte][rssi_byte][0xFF][payload…]
            need!(3);
            Ok(InboundFrame::RawData {
                snr_byte: body[0] as i8,
                rssi_byte: body[1] as i8,
                data: body[3..].to_vec(),
            })
        }
        PUSH_CODE_LOGIN_SUCCESS => {
            // [is_admin][pubkey_prefix×6][tag×4][acl_permissions][fw_level]
            need!(13);
            let mut prefix = [0u8; 6];
            prefix.copy_from_slice(&body[1..7]);
            Ok(InboundFrame::LoginSuccess(LoginSuccess {
                is_admin: body[0] != 0,
                pubkey_prefix: prefix,
                tag: r_u32(body, 7),
                acl_permissions: body[11],
                firmware_ver_level: body[12],
            }))
        }
        PUSH_CODE_LOGIN_FAIL => {
            // [reserved][pubkey_prefix×6]
            need!(7);
            let mut prefix = [0u8; 6];
            prefix.copy_from_slice(&body[1..7]);
            Ok(InboundFrame::LoginFail {
                pubkey_prefix: prefix,
            })
        }
        PUSH_CODE_STATUS_RESPONSE => {
            // [reserved][pubkey_prefix×6][raw…]
            need!(7);
            let mut prefix = [0u8; 6];
            prefix.copy_from_slice(&body[1..7]);
            Ok(InboundFrame::StatusResponse {
                pubkey_prefix: prefix,
                raw: body[7..].to_vec(),
            })
        }
        PUSH_CODE_LOG_RX_DATA => {
            // [snr_byte][rssi_byte][raw…]
            need!(2);
            Ok(InboundFrame::LogRxData {
                snr_byte: body[0] as i8,
                rssi_byte: body[1] as i8,
                raw: body[2..].to_vec(),
            })
        }
        PUSH_CODE_TELEMETRY_RESPONSE => {
            // [reserved][pubkey_prefix×6][lpp…]
            need!(7);
            let mut prefix = [0u8; 6];
            prefix.copy_from_slice(&body[1..7]);
            Ok(InboundFrame::TelemetryResponse {
                pubkey_prefix: prefix,
                raw: body[7..].to_vec(),
            })
        }
        PUSH_CODE_BINARY_RESPONSE => {
            // [reserved][tag×4][response_data…]
            need!(5);
            let mut tag = [0u8; 4];
            tag.copy_from_slice(&body[1..5]);
            Ok(InboundFrame::BinaryResponse {
                tag,
                raw: body[5..].to_vec(),
            })
        }
        PUSH_CODE_PATH_DISCOVERY_RESPONSE => {
            Ok(InboundFrame::PathDiscoveryResponse { raw: body.to_vec() })
        }
        PUSH_CODE_CONTROL_DATA => Ok(InboundFrame::ControlData { raw: body.to_vec() }),
        PUSH_CODE_CONTACT_DELETED => {
            need!(32);
            Ok(InboundFrame::ContactDeleted {
                pubkey: copy32(body),
            })
        }
        PUSH_CODE_CONTACTS_FULL => Ok(InboundFrame::ContactsFull),
        PUSH_CODE_TRACE_DATA => Ok(InboundFrame::TraceData { raw: body.to_vec() }),

        _ => {
            tracing::warn!("unknown companion frame type: 0x{:02X}", type_byte);
            Ok(InboundFrame::Unknown {
                type_byte,
                payload: payload.to_vec(),
            })
        }
    }
}

// ── Encode ────────────────────────────────────────────────────────────────────

/// Serialize an outbound command into complete wire bytes (prefix + length + payload).
///
/// The returned `Vec<u8>` can be written directly to the TCP socket.
pub fn encode_outbound(frame: &OutboundFrame) -> Vec<u8> {
    let payload = build_payload(frame);
    wrap_payload(&payload)
}

fn wrap_payload(payload: &[u8]) -> Vec<u8> {
    assert!(
        payload.len() <= u16::MAX as usize,
        "wrap_payload: payload length {} exceeds u16::MAX (65535); \
         the frame header can only encode a 2-byte length",
        payload.len()
    );
    let mut wire = Vec::with_capacity(3 + payload.len());
    wire.push(FRAME_INBOUND_PREFIX);
    wire.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    wire.extend_from_slice(payload);
    wire
}

fn build_payload(frame: &OutboundFrame) -> Vec<u8> {
    let mut p: Vec<u8> = Vec::new();
    match frame {
        OutboundFrame::AppStart { app_target_version } => {
            p.push(CMD_APP_START);
            p.push(*app_target_version);
        }
        OutboundFrame::DeviceQuery { app_target_version } => {
            p.push(CMD_DEVICE_QUERY);
            p.push(*app_target_version);
        }
        OutboundFrame::GetContacts { since } => {
            p.push(CMD_GET_CONTACTS);
            p.extend_from_slice(&since.to_le_bytes());
        }
        OutboundFrame::SyncNextMessage => {
            p.push(CMD_SYNC_NEXT_MESSAGE);
        }
        OutboundFrame::SendTxtMsg {
            txt_type,
            attempt,
            timestamp,
            pubkey_prefix,
            text,
        } => {
            // Layout confirmed from frame_server._cmd_send_txt_msg:
            // data[0]=txt_type, data[1]=attempt, data[2..6]=timestamp(4), data[6..12]=prefix, data[12..]=text
            p.push(CMD_SEND_TXT_MSG);
            p.push(*txt_type);
            p.push(*attempt);
            p.extend_from_slice(&timestamp.to_le_bytes());
            p.extend_from_slice(pubkey_prefix);
            p.extend_from_slice(text.as_bytes());
        }
        OutboundFrame::SendChannelTxtMsg {
            txt_type,
            channel_idx,
            text,
        } => {
            // data[0]=txt_type, data[1]=channel_idx, data[2..6]=reserved(4), data[6..]=text
            p.push(CMD_SEND_CHANNEL_TXT_MSG);
            p.push(*txt_type);
            p.push(*channel_idx);
            p.extend_from_slice(&[0u8; 4]); // reserved
            p.extend_from_slice(text.as_bytes());
        }
        OutboundFrame::SendLogin { pubkey, password } => {
            p.push(CMD_SEND_LOGIN);
            p.extend_from_slice(pubkey);
            p.extend_from_slice(password.as_bytes());
            p.push(0); // null terminator
        }
        OutboundFrame::GetDeviceTime => {
            p.push(CMD_GET_DEVICE_TIME);
        }
        OutboundFrame::SetDeviceTime { unix_time } => {
            p.push(CMD_SET_DEVICE_TIME);
            p.extend_from_slice(&unix_time.to_le_bytes());
        }
        OutboundFrame::SendSelfAdvert { flood } => {
            p.push(CMD_SEND_SELF_ADVERT);
            p.push(if *flood { 1 } else { 0 });
        }
        OutboundFrame::SetAdvertName { name } => {
            p.push(CMD_SET_ADVERT_NAME);
            let b = name.as_bytes();
            p.extend_from_slice(&b[..b.len().min(31)]);
            p.push(0);
        }
        OutboundFrame::SetAdvertLatlon { lat_1e6, lon_1e6 } => {
            p.push(CMD_SET_ADVERT_LATLON);
            p.extend_from_slice(&lat_1e6.to_le_bytes());
            p.extend_from_slice(&lon_1e6.to_le_bytes());
        }
        OutboundFrame::AddUpdateContact(c) => {
            p.push(CMD_ADD_UPDATE_CONTACT);
            encode_contact_body(&mut p, c);
        }
        OutboundFrame::RemoveContact { pubkey } => {
            p.push(CMD_REMOVE_CONTACT);
            p.extend_from_slice(pubkey);
        }
        OutboundFrame::ResetPath { pubkey } => {
            p.push(CMD_RESET_PATH);
            p.extend_from_slice(pubkey);
        }
        OutboundFrame::GetContactByKey { pubkey } => {
            p.push(CMD_GET_CONTACT_BY_KEY);
            p.extend_from_slice(pubkey);
        }
        OutboundFrame::ShareContact { pubkey } => {
            p.push(CMD_SHARE_CONTACT);
            p.extend_from_slice(pubkey);
        }
        OutboundFrame::ExportContact { pubkey } => {
            p.push(CMD_EXPORT_CONTACT);
            if let Some(key) = pubkey {
                p.extend_from_slice(key);
            }
        }
        OutboundFrame::ImportContact { data } => {
            p.push(CMD_IMPORT_CONTACT);
            p.extend_from_slice(data);
        }
        OutboundFrame::GetBattAndStorage => {
            p.push(CMD_GET_BATT_AND_STORAGE);
        }
        OutboundFrame::GetChannel { channel_idx } => {
            p.push(CMD_GET_CHANNEL);
            if let Some(idx) = channel_idx {
                p.push(*idx);
            }
        }
        OutboundFrame::SetChannel {
            channel_idx,
            name,
            secret,
        } => {
            p.push(CMD_SET_CHANNEL);
            p.push(*channel_idx);
            let nb = name.as_bytes();
            let nlen = nb.len().min(32);
            p.extend_from_slice(&nb[..nlen]);
            p.resize(p.len() + (32 - nlen), 0);
            p.extend_from_slice(secret);
        }
        OutboundFrame::Logout { pubkey } => {
            p.push(CMD_LOGOUT);
            p.extend_from_slice(pubkey);
        }
        OutboundFrame::GetStats { stats_type } => {
            p.push(CMD_GET_STATS);
            p.push(*stats_type);
        }
        OutboundFrame::SetFloodScope { key } => {
            p.push(CMD_SET_FLOOD_SCOPE);
            p.push(0); // reserved byte
            if let Some(k) = key {
                p.extend_from_slice(k);
            }
        }
        OutboundFrame::SetOtherParams {
            manual_add,
            telemetry_modes,
            advert_loc_policy,
            multi_acks,
        } => {
            p.push(CMD_SET_OTHER_PARAMS);
            p.push(*manual_add);
            p.push(*telemetry_modes);
            p.push(*advert_loc_policy);
            p.push(*multi_acks);
        }
        OutboundFrame::SetAutoaddConfig { config } => {
            p.push(CMD_SET_AUTOADD_CONFIG);
            p.push(*config);
        }
        OutboundFrame::GetAutoaddConfig => {
            p.push(CMD_GET_AUTOADD_CONFIG);
        }
        OutboundFrame::SetPathHashMode { mode } => {
            p.push(CMD_SET_PATH_HASH_MODE);
            p.push(0); // subtype byte must be 0
            p.push(*mode);
        }
        OutboundFrame::Raw { code, body } => {
            p.push(*code);
            p.extend_from_slice(body);
        }
    }
    p
}

// ── Private helpers ───────────────────────────────────────────────────────────

fn r_u16(b: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([b[off], b[off + 1]])
}

fn r_u32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn r_i32(b: &[u8], off: usize) -> i32 {
    i32::from_le_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

fn copy32(b: &[u8]) -> [u8; 32] {
    let mut a = [0u8; 32];
    a.copy_from_slice(&b[..32]);
    a
}

fn read_cstr(b: &[u8]) -> Result<String, FrameDecodeError> {
    let end = b.iter().position(|&x| x == 0).unwrap_or(b.len());
    std::str::from_utf8(&b[..end])
        .map(|s| s.to_owned())
        .map_err(|_| FrameDecodeError::InvalidUtf8)
}

fn parse_contact(body: &[u8]) -> Result<Contact, FrameDecodeError> {
    // pubkey[32] + adv_type[1] + flags[1] + out_path_len[1] + out_path[64]
    // + name[32] + last_advert[4] + lat[4] + lon[4] + lastmod[4] = 147 bytes
    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&body[..32]);
    let adv_type = body[32];
    let flags = body[33];
    let opl_byte = body[34];
    let out_path_len = if opl_byte == 0xFF {
        -1i8
    } else {
        opl_byte as i8
    };
    let mut out_path = [0u8; 64];
    out_path.copy_from_slice(&body[35..99]);
    let name = read_cstr(&body[99..131])?;
    let last_advert_timestamp = r_u32(body, 131);
    let gps_lat = r_i32(body, 135);
    let gps_lon = r_i32(body, 139);
    let lastmod = r_u32(body, 143);
    Ok(Contact {
        pubkey,
        adv_type,
        flags,
        out_path_len,
        out_path,
        name,
        last_advert_timestamp,
        gps_lat,
        gps_lon,
        lastmod,
    })
}

fn parse_self_info(body: &[u8]) -> Result<SelfInfo, FrameDecodeError> {
    // adv_type[1] + tx_power[1] + field_22[1] + pubkey[32] + lat[4] + lon[4]
    // + multi_acks[1] + advert_loc[1] + telem[1] + manual_add[1]
    // + freq_khz[4] + bw_hz[4] + sf[1] + cr[1] + name[variable] = 57+ bytes
    let adv_type = body[0];
    let tx_power_dbm = body[1];
    // body[2] = field_22, always 22; skip
    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&body[3..35]);
    let latitude = r_i32(body, 35);
    let longitude = r_i32(body, 39);
    let multi_acks = body[43];
    let advert_loc_policy = body[44];
    let telemetry_modes = body[45];
    let manual_add_contacts = body[46];
    let frequency_khz = r_u32(body, 47);
    let bandwidth_hz = r_u32(body, 51);
    let spreading_factor = body[55];
    let coding_rate = body[56];
    // Name is variable length, not null-padded; just strip any trailing nulls.
    let node_name = read_cstr(&body[57..])?;
    Ok(SelfInfo {
        adv_type,
        tx_power_dbm,
        pubkey,
        latitude,
        longitude,
        multi_acks,
        advert_loc_policy,
        telemetry_modes,
        manual_add_contacts,
        frequency_khz,
        bandwidth_hz,
        spreading_factor,
        coding_rate,
        node_name,
    })
}

fn parse_device_info(body: &[u8]) -> Result<DeviceInfo, FrameDecodeError> {
    // firmware_ver[1] + max_contacts_div_2[1] + max_channels[1] + ble_pin[4]
    // + build_date[12] + manufacturer[40] + version[20] + client_repeat[1] + path_hash_mode[1]
    // = 81 bytes
    let firmware_ver = body[0];
    let max_contacts = (body[1] as u16) * 2;
    let max_channels = body[2];
    let ble_pin = r_u32(body, 3);
    let build_date = read_cstr(&body[7..19])?;
    let manufacturer = read_cstr(&body[19..59])?;
    let version = read_cstr(&body[59..79])?;
    let client_repeat = body[79];
    let path_hash_mode = body[80];
    Ok(DeviceInfo {
        firmware_ver,
        max_contacts,
        max_channels,
        ble_pin,
        build_date,
        manufacturer,
        version,
        client_repeat,
        path_hash_mode,
    })
}

fn parse_sent_result(body: &[u8]) -> SentResult {
    // is_flood[1] + ack[4] + timeout_ms[4] = 9 bytes
    // body[0] is MSG_SEND_* (0=failed, 1=flood, 2=direct)
    SentResult {
        is_flood: body[0] == MSG_SEND_SENT_FLOOD,
        expected_ack: r_u32(body, 1),
        timeout_ms: r_u32(body, 5),
    }
}

fn parse_batt_and_storage(body: &[u8]) -> BattAndStorage {
    // millivolts[2] + used_kb[4] + total_kb[4] = 10 bytes
    BattAndStorage {
        millivolts: r_u16(body, 0),
        used_kb: r_u32(body, 2),
        total_kb: r_u32(body, 6),
    }
}

fn parse_exported_contact(body: &[u8]) -> Result<ExportedContact, FrameDecodeError> {
    // pubkey[32] + adv_type[1] + name[32] + lat[4] + lon[4] = 73 bytes
    let mut pubkey = [0u8; 32];
    pubkey.copy_from_slice(&body[..32]);
    let adv_type = body[32];
    let name = read_cstr(&body[33..65])?;
    let gps_lat = r_i32(body, 65);
    let gps_lon = r_i32(body, 69);
    Ok(ExportedContact {
        pubkey,
        adv_type,
        name,
        gps_lat,
        gps_lon,
    })
}

fn parse_contact_msg(body: &[u8], snr: Option<f32>) -> Result<ContactMsg, FrameDecodeError> {
    // sender_key_prefix[6] + path_len[1] + txt_type[1] + timestamp[4] + text[variable]
    // = 12 bytes min (empty text with no null terminator)
    let mut sender_key_prefix = [0u8; 6];
    sender_key_prefix.copy_from_slice(&body[..6]);
    let path_len = body[6];
    let txt_type = body[7];
    let timestamp = r_u32(body, 8);
    let text_bytes = &body[12..];
    let text = read_cstr(text_bytes)?;
    Ok(ContactMsg {
        sender_key_prefix,
        path_len,
        txt_type,
        timestamp,
        text,
        snr,
    })
}

fn parse_channel_msg(body: &[u8], snr: Option<f32>) -> Result<ChannelMsg, FrameDecodeError> {
    // channel_idx[1] + path_len[1] + txt_type[1] + timestamp[4] + text[variable]
    // = 7 bytes min
    let channel_idx = body[0];
    let path_len = body[1];
    let txt_type = body[2];
    let timestamp = r_u32(body, 3);
    let text_bytes = &body[7..];
    let text = read_cstr(text_bytes)?;
    Ok(ChannelMsg {
        channel_idx,
        path_len,
        txt_type,
        timestamp,
        text,
        snr,
    })
}

fn parse_channel_info(body: &[u8]) -> Result<ChannelInfo, FrameDecodeError> {
    // channel_idx[1] + name[32] + secret[16] = 49 bytes
    let channel_idx = body[0];
    let name = read_cstr(&body[1..33])?;
    let mut secret = [0u8; 16];
    secret.copy_from_slice(&body[33..49]);
    Ok(ChannelInfo {
        channel_idx,
        name,
        secret,
    })
}

fn encode_contact_body(p: &mut Vec<u8>, c: &Contact) {
    p.extend_from_slice(&c.pubkey);
    p.push(c.adv_type);
    p.push(c.flags);
    p.push(c.out_path_len as u8); // −1 becomes 0xFF
    p.extend_from_slice(&c.out_path);
    // Name: 32-byte null-padded field
    let nb = c.name.as_bytes();
    let nlen = nb.len().min(32);
    p.extend_from_slice(&nb[..nlen]);
    for _ in nlen..32 {
        p.push(0);
    }
    p.extend_from_slice(&c.last_advert_timestamp.to_le_bytes());
    p.extend_from_slice(&c.gps_lat.to_le_bytes());
    p.extend_from_slice(&c.gps_lon.to_le_bytes());
    p.extend_from_slice(&c.lastmod.to_le_bytes());
}
