//! Codec tests for the companion-frame protocol.
//!
//! Each test section corresponds to a decoded frame type or an encoding
//! command. Wire bytes are constructed by hand from the documented layouts
//! (confirmed against `pymc_core/frame_server.py`).

use meshcore_companion::{
    constants::*,
    decode_inbound, encode_outbound,
    error::FrameDecodeError,
    frame::{InboundFrame, OutboundFrame},
    strip_frame_header,
    types::{BattAndStorage, ChannelMsg, Contact, ContactMsg, LoginSuccess, SentResult},
};

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Build a raw wire frame (prefix + LE-length + payload).
fn wire(prefix: u8, payload: &[u8]) -> Vec<u8> {
    let mut v = vec![prefix];
    v.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    v.extend_from_slice(payload);
    v
}

// ── strip_frame_header ────────────────────────────────────────────────────────

#[test]
fn strip_header_happy_path() {
    let payload = &[RESP_CODE_OK];
    let raw = wire(FRAME_OUTBOUND_PREFIX, payload);
    assert_eq!(strip_frame_header(&raw).unwrap(), payload);
}

#[test]
fn strip_header_wrong_prefix() {
    let raw = wire(0x00, &[RESP_CODE_OK]);
    let err = strip_frame_header(&raw).unwrap_err();
    assert_eq!(
        err,
        FrameDecodeError::WrongPrefix {
            expected: FRAME_OUTBOUND_PREFIX,
            got: 0x00
        }
    );
}

#[test]
fn strip_header_empty_rejects() {
    let err = strip_frame_header(&[]).unwrap_err();
    assert!(matches!(err, FrameDecodeError::WrongPrefix { .. }));
}

#[test]
fn strip_header_payload_too_large() {
    // Claim 300 bytes (> MAX_PAYLOAD_SIZE = 256). Use little-endian u16.
    let len: u16 = 300;
    let mut raw = vec![FRAME_OUTBOUND_PREFIX, (len & 0xFF) as u8, (len >> 8) as u8];
    raw.extend(vec![0u8; 300]);
    let err = strip_frame_header(&raw).unwrap_err();
    assert_eq!(err, FrameDecodeError::PayloadTooLarge(300));
}

#[test]
fn strip_header_payload_truncated() {
    // Header says 10 bytes, buffer has only 5 after the 3-byte header.
    // Must return BodyTooShort rather than panicking (SYN-36).
    let len: u16 = 10;
    let mut raw = vec![FRAME_OUTBOUND_PREFIX, (len & 0xFF) as u8, (len >> 8) as u8];
    raw.extend(vec![0u8; 5]); // only 5 bytes, not 10
    let err = strip_frame_header(&raw).unwrap_err();
    assert_eq!(
        err,
        FrameDecodeError::BodyTooShort {
            type_byte: 0,
            needed: 13,
            got: 8,
        }
    );
}

// ── decode_inbound — zero-body frames ─────────────────────────────────────────

#[test]
fn decode_ok() {
    let frame = decode_inbound(&[RESP_CODE_OK]).unwrap();
    assert_eq!(frame, InboundFrame::Ok);
}

#[test]
fn decode_disabled() {
    let frame = decode_inbound(&[RESP_CODE_DISABLED]).unwrap();
    assert_eq!(frame, InboundFrame::Disabled);
}

#[test]
fn decode_no_more_messages() {
    let frame = decode_inbound(&[RESP_CODE_NO_MORE_MESSAGES]).unwrap();
    assert_eq!(frame, InboundFrame::NoMoreMessages);
}

#[test]
fn decode_msg_waiting() {
    let frame = decode_inbound(&[PUSH_CODE_MSG_WAITING]).unwrap();
    assert_eq!(frame, InboundFrame::MsgWaiting);
}

#[test]
fn decode_contacts_full() {
    let frame = decode_inbound(&[PUSH_CODE_CONTACTS_FULL]).unwrap();
    assert_eq!(frame, InboundFrame::ContactsFull);
}

// ── decode_inbound — small structured frames ──────────────────────────────────

#[test]
fn decode_err() {
    let frame = decode_inbound(&[RESP_CODE_ERR, ERR_CODE_NOT_FOUND]).unwrap();
    assert_eq!(
        frame,
        InboundFrame::Err {
            error_code: ERR_CODE_NOT_FOUND
        }
    );
}

#[test]
fn decode_contacts_start() {
    let count: u32 = 42;
    let mut payload = vec![RESP_CODE_CONTACTS_START];
    payload.extend_from_slice(&count.to_le_bytes());
    let frame = decode_inbound(&payload).unwrap();
    assert_eq!(frame, InboundFrame::ContactsStart { count: 42 });
}

#[test]
fn decode_end_of_contacts() {
    let ts: u32 = 0xDEAD_BEEF;
    let mut payload = vec![RESP_CODE_END_OF_CONTACTS];
    payload.extend_from_slice(&ts.to_le_bytes());
    let frame = decode_inbound(&payload).unwrap();
    assert_eq!(
        frame,
        InboundFrame::EndOfContacts {
            most_recent_lastmod: 0xDEAD_BEEF
        }
    );
}

#[test]
fn decode_curr_time() {
    let t: u32 = 1_700_000_000;
    let mut payload = vec![RESP_CODE_CURR_TIME];
    payload.extend_from_slice(&t.to_le_bytes());
    let frame = decode_inbound(&payload).unwrap();
    assert_eq!(
        frame,
        InboundFrame::CurrTime {
            unix_time: 1_700_000_000
        }
    );
}

#[test]
fn decode_batt_and_storage() {
    let mut payload = vec![RESP_CODE_BATT_AND_STORAGE];
    payload.extend_from_slice(&3800u16.to_le_bytes()); // millivolts
    payload.extend_from_slice(&512u32.to_le_bytes()); // used_kb
    payload.extend_from_slice(&4096u32.to_le_bytes()); // total_kb
    let frame = decode_inbound(&payload).unwrap();
    assert_eq!(
        frame,
        InboundFrame::BattAndStorage(BattAndStorage {
            millivolts: 3800,
            used_kb: 512,
            total_kb: 4096
        })
    );
}

#[test]
fn decode_sent_result() {
    let mut payload = vec![RESP_CODE_SENT];
    payload.push(MSG_SEND_SENT_DIRECT); // is_flood = 0 (direct, not flood)
    payload.extend_from_slice(&0x0000_1234u32.to_le_bytes()); // expected_ack
    payload.extend_from_slice(&30_000u32.to_le_bytes()); // timeout_ms
    let frame = decode_inbound(&payload).unwrap();
    assert_eq!(
        frame,
        InboundFrame::Sent(SentResult {
            is_flood: false,
            expected_ack: 0x1234,
            timeout_ms: 30_000
        })
    );
}

#[test]
fn decode_send_confirmed() {
    let mut payload = vec![PUSH_CODE_SEND_CONFIRMED];
    payload.extend_from_slice(&0xCAFE_BABEu32.to_le_bytes()); // crc
    payload.extend_from_slice(&[0u8; 4]); // zero padding
    let frame = decode_inbound(&payload).unwrap();
    assert_eq!(frame, InboundFrame::SendConfirmed { crc: 0xCAFE_BABE });
}

#[test]
fn decode_advert() {
    let pubkey = [0xABu8; 32];
    let mut payload = vec![PUSH_CODE_ADVERT];
    payload.extend_from_slice(&pubkey);
    let frame = decode_inbound(&payload).unwrap();
    assert_eq!(frame, InboundFrame::Advert { pubkey });
}

#[test]
fn decode_path_updated() {
    let pubkey = [0x11u8; 32];
    let mut payload = vec![PUSH_CODE_PATH_UPDATED];
    payload.extend_from_slice(&pubkey);
    let frame = decode_inbound(&payload).unwrap();
    assert_eq!(frame, InboundFrame::PathUpdated { pubkey });
}

#[test]
fn decode_contact_deleted() {
    let pubkey = [0x22u8; 32];
    let mut payload = vec![PUSH_CODE_CONTACT_DELETED];
    payload.extend_from_slice(&pubkey);
    let frame = decode_inbound(&payload).unwrap();
    assert_eq!(frame, InboundFrame::ContactDeleted { pubkey });
}

// ── decode_inbound — contact message v1/v2 ────────────────────────────────────

#[test]
fn decode_contact_msg_recv() {
    let sender = [1u8, 2, 3, 4, 5, 6];
    let path_len: u8 = 2;
    let txt_type = TXT_TYPE_PLAIN;
    let timestamp: u32 = 1_700_000_100;
    let text = b"Hello world";

    let mut payload = vec![RESP_CODE_CONTACT_MSG_RECV];
    payload.extend_from_slice(&sender);
    payload.push(path_len);
    payload.push(txt_type);
    payload.extend_from_slice(&timestamp.to_le_bytes());
    payload.extend_from_slice(text);

    let frame = decode_inbound(&payload).unwrap();
    assert_eq!(
        frame,
        InboundFrame::ContactMsgRecv(ContactMsg {
            sender_key_prefix: sender,
            path_len: 2,
            txt_type: TXT_TYPE_PLAIN,
            timestamp: 1_700_000_100,
            text: "Hello world".to_owned(),
            snr: None,
        })
    );
}

// ── decode_inbound — channel message v1/v2 ───────────────────────────────────

#[test]
fn decode_channel_msg_recv() {
    let channel_idx: u8 = 1;
    let path_len: u8 = 0;
    let txt_type = TXT_TYPE_PLAIN;
    let timestamp: u32 = 1_700_000_200;
    let text = b"Alice: hi there";

    let mut payload = vec![RESP_CODE_CHANNEL_MSG_RECV];
    payload.push(channel_idx);
    payload.push(path_len);
    payload.push(txt_type);
    payload.extend_from_slice(&timestamp.to_le_bytes());
    payload.extend_from_slice(text);

    let frame = decode_inbound(&payload).unwrap();
    assert_eq!(
        frame,
        InboundFrame::ChannelMsgRecv(ChannelMsg {
            channel_idx: 1,
            path_len: 0,
            txt_type: TXT_TYPE_PLAIN,
            timestamp: 1_700_000_200,
            text: "Alice: hi there".to_owned(),
            snr: None,
        })
    );
}

// ── decode_inbound — contact message v3 (with SNR) ───────────────────────────

#[test]
fn decode_contact_msg_recv_v3() {
    // snr_byte encodes as (dB * 4); −6 dB → i8 = −24 → byte 0xE8
    let snr_byte: i8 = -24; // −24 / 4 = −6.0 dB
    let sender = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];
    let path_len: u8 = 1;
    let txt_type = TXT_TYPE_PLAIN;
    let timestamp: u32 = 1_700_000_300;
    let text = b"v3 msg";

    let mut payload = vec![RESP_CODE_CONTACT_MSG_RECV_V3];
    payload.push(snr_byte as u8); // snr_byte at body[0]
    payload.push(0); // reserved
    payload.push(0); // reserved
                     // after stripping 3 bytes, the msg starts at body[3]:
    payload.extend_from_slice(&sender);
    payload.push(path_len);
    payload.push(txt_type);
    payload.extend_from_slice(&timestamp.to_le_bytes());
    payload.extend_from_slice(text);

    let frame = decode_inbound(&payload).unwrap();
    if let InboundFrame::ContactMsgRecvV3(msg) = frame {
        assert_eq!(msg.sender_key_prefix, sender);
        assert_eq!(msg.text, "v3 msg");
        let snr = msg.snr.unwrap();
        assert!((snr - (-6.0f32)).abs() < 0.01, "snr mismatch: {snr}");
    } else {
        panic!("expected ContactMsgRecvV3, got {frame:?}");
    }
}

// ── decode_inbound — login success ───────────────────────────────────────────

#[test]
fn decode_login_success() {
    let prefix = [1u8, 2, 3, 4, 5, 6];
    let tag: u32 = 0xDEAD_C0DE;
    let acl: u8 = 0b0000_0011;
    let fw: u8 = 10;

    let mut payload = vec![PUSH_CODE_LOGIN_SUCCESS];
    payload.push(1u8); // is_admin = true
    payload.extend_from_slice(&prefix);
    payload.extend_from_slice(&tag.to_le_bytes());
    payload.push(acl);
    payload.push(fw);

    let frame = decode_inbound(&payload).unwrap();
    assert_eq!(
        frame,
        InboundFrame::LoginSuccess(LoginSuccess {
            is_admin: true,
            pubkey_prefix: prefix,
            tag: 0xDEAD_C0DE,
            acl_permissions: acl,
            firmware_ver_level: 10,
        })
    );
}

// ── decode_inbound — unknown type byte ───────────────────────────────────────

#[test]
fn decode_unknown_type_byte() {
    let payload = vec![0xFE, 0x01, 0x02];
    let frame = decode_inbound(&payload).unwrap();
    assert_eq!(
        frame,
        InboundFrame::Unknown {
            type_byte: 0xFE,
            payload: payload.clone()
        }
    );
}

// ── decode_inbound — error cases ─────────────────────────────────────────────

#[test]
fn decode_empty_payload_errors() {
    let err = decode_inbound(&[]).unwrap_err();
    assert_eq!(
        err,
        FrameDecodeError::BodyTooShort {
            type_byte: 0,
            needed: 1,
            got: 0
        }
    );
}

#[test]
fn decode_body_too_short_for_type() {
    // RESP_CODE_ERR needs at least 1 body byte, give 0.
    let err = decode_inbound(&[RESP_CODE_ERR]).unwrap_err();
    assert_eq!(
        err,
        FrameDecodeError::BodyTooShort {
            type_byte: RESP_CODE_ERR,
            needed: 1,
            got: 0
        }
    );
}

#[test]
fn decode_tolerates_invalid_utf8() {
    // RESP_CODE_CHANNEL_MSG_RECV: [chan_idx][path_len][txt_type][ts×4][invalid utf8]
    // read_cstr now uses from_utf8_lossy so invalid bytes become U+FFFD rather
    // than returning an error and killing the session.
    let mut payload = vec![RESP_CODE_CHANNEL_MSG_RECV];
    payload.push(0); // channel_idx
    payload.push(0); // path_len
    payload.push(0); // txt_type
    payload.extend_from_slice(&0u32.to_le_bytes()); // timestamp
    payload.push(0xFF); // invalid UTF-8 byte → replaced with U+FFFD
    let frame = decode_inbound(&payload).unwrap();
    match frame {
        InboundFrame::ChannelMsgRecv(msg) => {
            assert_eq!(msg.text, "\u{FFFD}", "invalid byte should become U+FFFD");
        }
        other => panic!("expected ChannelMsgRecv, got {other:?}"),
    }
}

// ── decode_inbound — contact struct ──────────────────────────────────────────

fn build_contact_body() -> (Vec<u8>, Contact) {
    let pubkey = [0x55u8; 32];
    let adv_type = ADV_TYPE_CHAT;
    let flags: u8 = 0;
    let out_path_len_byte: u8 = 0xFF; // unknown → -1
    let out_path = [0u8; 64];
    let mut name_buf = [0u8; 32];
    let name = b"TestNode";
    name_buf[..name.len()].copy_from_slice(name);
    let last_advert: u32 = 100;
    let lat: i32 = 37_422_160; // ≈37.42216°N
    let lon: i32 = -122_084_058; // ≈122.08406°W
    let lastmod: u32 = 200;

    let mut body = Vec::new();
    body.extend_from_slice(&pubkey);
    body.push(adv_type);
    body.push(flags);
    body.push(out_path_len_byte);
    body.extend_from_slice(&out_path);
    body.extend_from_slice(&name_buf);
    body.extend_from_slice(&last_advert.to_le_bytes());
    body.extend_from_slice(&lat.to_le_bytes());
    body.extend_from_slice(&lon.to_le_bytes());
    body.extend_from_slice(&lastmod.to_le_bytes());

    let contact = Contact {
        pubkey,
        adv_type: ADV_TYPE_CHAT,
        flags: 0,
        out_path_len: -1,
        out_path: [0u8; 64],
        name: "TestNode".to_owned(),
        last_advert_timestamp: 100,
        gps_lat: 37_422_160,
        gps_lon: -122_084_058,
        lastmod: 200,
    };
    (body, contact)
}

#[test]
fn decode_contact() {
    let (body, expected) = build_contact_body();
    let mut payload = vec![RESP_CODE_CONTACT];
    payload.extend_from_slice(&body);
    let frame = decode_inbound(&payload).unwrap();
    assert_eq!(frame, InboundFrame::Contact(expected));
}

#[test]
fn decode_out_path_len_direct() {
    // out_path_len = 0 means direct (not 0xFF).
    let (mut body, _) = build_contact_body();
    body[34] = 0; // out_path_len byte: direct
    let mut payload = vec![RESP_CODE_CONTACT];
    payload.extend_from_slice(&body);
    if let InboundFrame::Contact(c) = decode_inbound(&payload).unwrap() {
        assert_eq!(c.out_path_len, 0);
    } else {
        panic!("expected Contact");
    }
}

// ── encode_outbound ───────────────────────────────────────────────────────────

#[test]
fn encode_app_start() {
    let wire_bytes = encode_outbound(&OutboundFrame::AppStart {
        app_target_version: APP_TARGET_VER_V3,
    });
    // Wire: [FRAME_INBOUND_PREFIX][len_lo][len_hi][CMD_APP_START][APP_TARGET_VER_V3][0×6]
    // Payload is padded to 8 bytes minimum as required by the MeshCore companion firmware.
    assert_eq!(wire_bytes[0], FRAME_INBOUND_PREFIX);
    let len = u16::from_le_bytes([wire_bytes[1], wire_bytes[2]]) as usize;
    let payload = &wire_bytes[3..3 + len];
    assert_eq!(payload[0], CMD_APP_START);
    assert_eq!(payload[1], APP_TARGET_VER_V3);
    // Remaining bytes are zero padding.
    assert_eq!(
        &payload[2..],
        &[0u8; 6],
        "AppStart must be padded to 8-byte minimum payload"
    );
    assert_eq!(
        len, 8,
        "AppStart payload must be 8 bytes (firmware minimum)"
    );
}

#[test]
fn encode_sync_next_message() {
    let wire_bytes = encode_outbound(&OutboundFrame::SyncNextMessage);
    let len = u16::from_le_bytes([wire_bytes[1], wire_bytes[2]]) as usize;
    assert_eq!(len, 1);
    assert_eq!(wire_bytes[3], CMD_SYNC_NEXT_MESSAGE);
}

#[test]
fn encode_get_contacts() {
    let since: u32 = 1_700_000_000;
    let wire_bytes = encode_outbound(&OutboundFrame::GetContacts { since });
    let len = u16::from_le_bytes([wire_bytes[1], wire_bytes[2]]) as usize;
    assert_eq!(len, 5);
    assert_eq!(wire_bytes[3], CMD_GET_CONTACTS);
    let decoded_since =
        u32::from_le_bytes([wire_bytes[4], wire_bytes[5], wire_bytes[6], wire_bytes[7]]);
    assert_eq!(decoded_since, since);
}

#[test]
fn encode_send_txt_msg_layout() {
    // Confirm timestamp bytes at positions 2-5 (after txt_type and attempt).
    // Wire payload: [CMD][txt_type][attempt][timestamp×4][prefix×6][text…]
    let prefix = [0x01, 0x02, 0x03, 0x04, 0x05, 0x06];
    let ts: u32 = 0x1234_5678;
    let wire_bytes = encode_outbound(&OutboundFrame::SendTxtMsg {
        txt_type: TXT_TYPE_PLAIN,
        attempt: 1,
        timestamp: ts,
        pubkey_prefix: prefix,
        text: "hi".to_owned(),
    });
    let payload = &wire_bytes[3..];
    assert_eq!(payload[0], CMD_SEND_TXT_MSG);
    assert_eq!(payload[1], TXT_TYPE_PLAIN); // txt_type
    assert_eq!(payload[2], 1u8); // attempt
    assert_eq!(&payload[3..7], &ts.to_le_bytes()); // timestamp (little-endian)
    assert_eq!(&payload[7..13], &prefix); // pubkey_prefix
    assert_eq!(&payload[13..15], b"hi");
}

#[test]
fn encode_send_channel_txt_msg_layout() {
    // Wire payload: [CMD][txt_type][channel_idx][0x00×4][text…]
    let wire_bytes = encode_outbound(&OutboundFrame::SendChannelTxtMsg {
        txt_type: TXT_TYPE_PLAIN,
        channel_idx: 2,
        text: "group msg".to_owned(),
    });
    let payload = &wire_bytes[3..];
    assert_eq!(payload[0], CMD_SEND_CHANNEL_TXT_MSG);
    assert_eq!(payload[1], TXT_TYPE_PLAIN);
    assert_eq!(payload[2], 2u8); // channel_idx
    assert_eq!(&payload[3..7], &[0u8; 4]); // 4 reserved bytes
    assert_eq!(&payload[7..16], b"group msg");
}

#[test]
fn encode_set_device_time() {
    let t: u32 = 1_234_567_890;
    let wire_bytes = encode_outbound(&OutboundFrame::SetDeviceTime { unix_time: t });
    let payload = &wire_bytes[3..];
    assert_eq!(payload[0], CMD_SET_DEVICE_TIME);
    let decoded = u32::from_le_bytes([payload[1], payload[2], payload[3], payload[4]]);
    assert_eq!(decoded, t);
}

#[test]
fn encode_set_advert_name_null_terminated() {
    let wire_bytes = encode_outbound(&OutboundFrame::SetAdvertName {
        name: "BBS".to_owned(),
    });
    let payload = &wire_bytes[3..];
    assert_eq!(payload[0], CMD_SET_ADVERT_NAME);
    assert_eq!(&payload[1..4], b"BBS");
    assert_eq!(payload[4], 0u8); // null terminator
}

#[test]
fn encode_set_advert_name_truncates_at_31() {
    // 40-char name should be truncated to 31 bytes + 1 null = 32 bytes of name field.
    let name = "A".repeat(40);
    let wire_bytes = encode_outbound(&OutboundFrame::SetAdvertName { name });
    let payload = &wire_bytes[3..];
    // payload[1..32] = 31 'A' bytes, payload[32] = null
    assert_eq!(payload.len(), 1 + 31 + 1); // cmd + name + null
    assert_eq!(payload[32], 0u8);
}

#[test]
fn encode_remove_contact() {
    let pubkey = [0xFFu8; 32];
    let wire_bytes = encode_outbound(&OutboundFrame::RemoveContact { pubkey });
    let payload = &wire_bytes[3..];
    assert_eq!(payload[0], CMD_REMOVE_CONTACT);
    assert_eq!(&payload[1..33], &pubkey);
}

// ── strip + decode round-trip ─────────────────────────────────────────────────

#[test]
fn strip_then_decode_ok() {
    let payload = &[RESP_CODE_OK];
    let raw = wire(FRAME_OUTBOUND_PREFIX, payload);
    let stripped = strip_frame_header(&raw).unwrap();
    let frame = decode_inbound(stripped).unwrap();
    assert_eq!(frame, InboundFrame::Ok);
}

#[test]
fn strip_then_decode_curr_time() {
    let t: u32 = 1_700_000_999;
    let mut payload = vec![RESP_CODE_CURR_TIME];
    payload.extend_from_slice(&t.to_le_bytes());
    let raw = wire(FRAME_OUTBOUND_PREFIX, &payload);
    let stripped = strip_frame_header(&raw).unwrap();
    let frame = decode_inbound(stripped).unwrap();
    assert_eq!(frame, InboundFrame::CurrTime { unix_time: t });
}

// ── SetRadioParams / SetRadioTxPower ─────────────────────────────────────────

#[test]
fn encode_set_radio_params_layout() {
    // USA/Canada preset: 910_525_000 Hz, 62_500 Hz BW, SF7, CR5
    //
    // NOTE: the companion-frame protocol encodes frequency in kHz on the wire
    // (matching how RESP_CODE_SELF_INFO reports it).  Our frame type stores Hz
    // for human clarity and divides by 1000 during encoding.
    let freq_hz: u32 = 910_525_000;
    let freq_khz: u32 = freq_hz / 1000; // 910_525 kHz
    let bw: u32 = 62_500;
    let sf: u8 = 7;
    let cr: u8 = 5;

    let wire_bytes = encode_outbound(&OutboundFrame::SetRadioParams {
        frequency_hz: freq_hz,
        bandwidth_hz: bw,
        spreading_factor: sf,
        coding_rate: cr,
    });

    // Wire: [prefix:1][len:2][CMD_SET_RADIO_PARAMS][freq_khz:4-LE][bw_hz:4-LE][sf:1][cr:1]
    let payload = &wire_bytes[3..]; // skip 3-byte header
    assert_eq!(payload[0], CMD_SET_RADIO_PARAMS, "command byte");
    assert_eq!(
        &payload[1..5],
        &freq_khz.to_le_bytes(),
        "frequency in kHz LE"
    );
    assert_eq!(&payload[5..9], &bw.to_le_bytes(), "bandwidth_hz LE");
    assert_eq!(payload[9], sf, "spreading_factor");
    assert_eq!(payload[10], cr, "coding_rate");
    assert_eq!(payload.len(), 11, "total payload length");
}

#[test]
fn encode_set_radio_tx_power_layout() {
    let power_dbm: i8 = 20;
    let wire_bytes = encode_outbound(&OutboundFrame::SetRadioTxPower { power_dbm });

    // Wire: [prefix:1][len:2][CMD_SET_RADIO_TX_POWER][power:1]
    let payload = &wire_bytes[3..];
    assert_eq!(payload[0], CMD_SET_RADIO_TX_POWER, "command byte");
    assert_eq!(payload[1], power_dbm as u8, "power byte");
    assert_eq!(payload.len(), 2, "total payload length");
}

#[test]
fn encode_set_radio_tx_power_negative() {
    // Negative dBm values must round-trip correctly through i8→u8→i8.
    let power_dbm: i8 = -10;
    let wire_bytes = encode_outbound(&OutboundFrame::SetRadioTxPower { power_dbm });
    let payload = &wire_bytes[3..];
    assert_eq!(payload[1] as i8, power_dbm, "negative dBm preserved");
}

// ── ExportPrivateKey / ImportPrivateKey ──────────────────────────────────────

#[test]
fn encode_export_private_key_layout() {
    let bytes = encode_outbound(&OutboundFrame::ExportPrivateKey);
    // Header: 0x3C + u16-LE payload length (1 byte for the CMD byte)
    assert_eq!(bytes[0], FRAME_INBOUND_PREFIX);
    assert_eq!(u16::from_le_bytes([bytes[1], bytes[2]]), 1);
    assert_eq!(bytes[3], CMD_EXPORT_PRIVATE_KEY);
    assert_eq!(bytes.len(), 4);
}

#[test]
fn encode_import_private_key_layout() {
    let key = [0xABu8; 32];
    let bytes = encode_outbound(&OutboundFrame::ImportPrivateKey { key });
    assert_eq!(bytes[0], FRAME_INBOUND_PREFIX);
    assert_eq!(u16::from_le_bytes([bytes[1], bytes[2]]), 33); // 1 CMD + 32 key
    assert_eq!(bytes[3], CMD_IMPORT_PRIVATE_KEY);
    assert_eq!(&bytes[4..], &[0xABu8; 32]);
}

// ── wrap_payload overflow guard ───────────────────────────────────────────────

/// `encode_outbound` (via `wrap_payload`) must panic with a clear message when
/// the serialised payload exceeds `u16::MAX` bytes rather than silently
/// truncating the length field (BUG-13).
#[test]
#[should_panic(expected = "payload length")]
fn encode_raw_oversized_payload_panics() {
    // Build a Raw frame whose body is 65_536 bytes (u16::MAX + 1).
    // The 1-byte command code + body puts the total payload at 65_537 bytes,
    // which cannot be encoded in the 2-byte length field.
    let oversized_body = vec![0u8; u16::MAX as usize + 1];
    let _ = encode_outbound(&OutboundFrame::Raw {
        code: 0xFF,
        body: oversized_body,
    });
}
