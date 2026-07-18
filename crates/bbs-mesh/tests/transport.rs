//! Integration tests for [`MeshTransport`].
//!
//! Uses [`MockHost`] from `bbs-plugin-api` and an in-process TCP loopback
//! (ephemeral port on 127.0.0.1) to exercise the full
//! `init → start → frame → response` path without a real `pymc_core` process.
//!
//! # Bridge protocol reminder
//!
//! The companion client sends AppStart (11 bytes: prefix + 2-byte len + 8-byte
//! payload padded to firmware minimum) and expects SelfInfo back before
//! entering the event loop.  The test bridge must read all 11 bytes before
//! sending SelfInfo to avoid leaving stale bytes in the TCP buffer.

use std::{
    sync::{
        atomic::{AtomicU32, Ordering},
        Arc,
    },
    time::Duration,
};

use bbs_mesh::{MeshConfig, MeshTransport};
use bbs_plugin_api::{
    event::{Notification, NotifyOutcome},
    plugin::Plugin,
    testing::MockHost,
    transport::TransportEngine,
    Command, Response,
};
use meshcore_companion::constants::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// ── Wire-frame helpers ────────────────────────────────────────────────────────

fn radio_frame(payload: &[u8]) -> Vec<u8> {
    let mut v = vec![FRAME_OUTBOUND_PREFIX];
    v.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    v.extend_from_slice(payload);
    v
}

fn self_info_frame(name: &str) -> Vec<u8> {
    let mut body = Vec::new();
    body.push(ADV_TYPE_CHAT);
    body.push(20u8);
    body.push(22u8);
    body.extend_from_slice(&[0xAAu8; 32]);
    body.extend_from_slice(&0i32.to_le_bytes());
    body.extend_from_slice(&0i32.to_le_bytes());
    body.push(0u8);
    body.push(ADVERT_LOC_NONE);
    body.push(0u8);
    body.push(0u8);
    body.extend_from_slice(&915_000u32.to_le_bytes());
    body.extend_from_slice(&125_000u32.to_le_bytes());
    body.push(10u8);
    body.push(5u8);
    body.extend_from_slice(name.as_bytes());

    let mut payload = vec![RESP_CODE_SELF_INFO];
    payload.extend_from_slice(&body);
    radio_frame(&payload)
}

/// Build an error frame (RESP_CODE_ERR + error_code).
fn err_frame(error_code: u8) -> Vec<u8> {
    radio_frame(&[RESP_CODE_ERR, error_code])
}

/// Monotonic source of distinct sender timestamps so each `contact_msg_frame`
/// models a genuinely new message. A real retransmission reuses a timestamp —
/// use [`contact_msg_frame_ts`] with a repeated value for that.
static NEXT_TS: AtomicU32 = AtomicU32::new(1_700_000_000);

fn contact_msg_frame(sender_prefix: [u8; 6], text: &str) -> Vec<u8> {
    let ts = NEXT_TS.fetch_add(1, Ordering::Relaxed);
    contact_msg_frame_ts(sender_prefix, text, ts)
}

/// Like [`contact_msg_frame`] but with an explicit sender timestamp. Reuse the
/// same `timestamp` across two frames to model a retransmission of one message;
/// use distinct values to model two separate messages.
fn contact_msg_frame_ts(sender_prefix: [u8; 6], text: &str, timestamp: u32) -> Vec<u8> {
    let mut payload = vec![RESP_CODE_CONTACT_MSG_RECV];
    payload.extend_from_slice(&sender_prefix);
    payload.push(0u8); // path_len
    payload.push(TXT_TYPE_PLAIN);
    payload.extend_from_slice(&timestamp.to_le_bytes());
    payload.extend_from_slice(text.as_bytes());
    radio_frame(&payload)
}

/// RESP_CODE_CHANNEL_MSG_RECV: a channel (group) message popped from the device's
/// offline queue — the same queue that holds DMs, so the sync chain must be able
/// to continue past one. Layout: [code][channel_idx][path_len][txt_type][ts×4][text].
fn channel_msg_frame(channel_idx: u8, text: &str) -> Vec<u8> {
    let ts = NEXT_TS.fetch_add(1, Ordering::Relaxed);
    let mut payload = vec![RESP_CODE_CHANNEL_MSG_RECV];
    payload.push(channel_idx);
    payload.push(0u8); // path_len
    payload.push(TXT_TYPE_PLAIN);
    payload.extend_from_slice(&ts.to_le_bytes());
    payload.extend_from_slice(text.as_bytes());
    radio_frame(&payload)
}

/// RESP_CODE_SENT: the device's reply to a send. `send_type` 0=failed, 1=flood,
/// 2=direct; `crc` is the expected-ack identifier; `timeout_ms` the delivery hint.
fn sent_frame(send_type: u8, crc: u32, timeout_ms: u32) -> Vec<u8> {
    let mut payload = vec![RESP_CODE_SENT, send_type];
    payload.extend_from_slice(&crc.to_le_bytes());
    payload.extend_from_slice(&timeout_ms.to_le_bytes());
    radio_frame(&payload)
}

/// PUSH_CODE_SEND_CONFIRMED: destination acknowledged receipt of `crc`.
fn send_confirmed_frame(crc: u32) -> Vec<u8> {
    let mut payload = vec![PUSH_CODE_SEND_CONFIRMED];
    payload.extend_from_slice(&crc.to_le_bytes());
    payload.extend_from_slice(&0u32.to_le_bytes());
    radio_frame(&payload)
}

/// PUSH_CODE_PATH_UPDATED: the device learned a route to a node and reports its
/// full 32-byte pubkey.
fn path_updated_frame(pubkey: [u8; 32]) -> Vec<u8> {
    let mut payload = vec![PUSH_CODE_PATH_UPDATED];
    payload.extend_from_slice(&pubkey);
    radio_frame(&payload)
}

// ── Test harness ──────────────────────────────────────────────────────────────

struct Bridge {
    stream: tokio::net::TcpStream,
}

impl Bridge {
    async fn send(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).await.unwrap();
    }

    async fn recv_n(&mut self, n: usize) -> Vec<u8> {
        let mut buf = vec![0u8; n];
        self.stream.read_exact(&mut buf).await.unwrap();
        buf
    }

    async fn complete_handshake(&mut self, name: &str) {
        // AppStart = 11 bytes: prefix(1) + len(2) + CMD_APP_START(1) + version(1) + zeros(6)
        // Must read all 11 to avoid leaving stale bytes that corrupt read_command().
        let app_start = self.recv_n(11).await;
        assert_eq!(app_start[3], CMD_APP_START, "expected CMD_APP_START");
        self.send(&self_info_frame(name)).await;
        // Transport immediately drains the queue on connect: read the
        // SyncNextMessage it sends and reply with NoMoreMessages so the
        // draining flag clears before tests send real commands.
        let drain_cmd = self.read_command().await;
        assert_eq!(
            drain_cmd[0], CMD_SYNC_NEXT_MESSAGE,
            "expected initial SyncNextMessage drain"
        );
        let no_more = radio_frame(&[RESP_CODE_NO_MORE_MESSAGES]);
        self.send(&no_more).await;
        // Transport sends CMD_GET_CONTACTS immediately after connect to populate
        // the advert bus. Read and discard it — the mock bridge sends no contacts.
        // This is a strict single read: no advert frames should appear here because
        // every caller runs with adverts disabled (see make_transport_with and the
        // real_host configs). `no_self_advert_when_disabled` guards that invariant.
        let get_contacts = self.read_command().await;
        assert_eq!(
            get_contacts[0], CMD_GET_CONTACTS,
            "expected CMD_GET_CONTACTS after drain"
        );
        // Transport queries autoadd config at startup to ensure auto-pruning is
        // enabled on the radio. Reply with config=1 (already enabled) so the
        // transport does not emit a follow-up SetAutoaddConfig command.
        let get_autoadd = self.read_command().await;
        assert_eq!(
            get_autoadd[0], CMD_GET_AUTOADD_CONFIG,
            "expected CMD_GET_AUTOADD_CONFIG after CMD_GET_CONTACTS"
        );
        let autoadd_reply = radio_frame(&[RESP_CODE_AUTOADD_CONFIG, 1]);
        self.send(&autoadd_reply).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    /// Simulate older firmware that returns UNSUPPORTED_CMD for CMD_APP_START
    /// and also for CMD_SYNC_NEXT_MESSAGE (the drain command).
    ///
    /// This exercises the drain-lock fix: without it, the draining flag would
    /// stay true forever and all subsequent messages would be silently dropped.
    async fn complete_handshake_unsupported_app_start(&mut self) {
        let app_start = self.recv_n(11).await;
        assert_eq!(app_start[3], CMD_APP_START, "expected CMD_APP_START");
        // Return UNSUPPORTED_CMD instead of SelfInfo.
        self.send(&err_frame(ERR_CODE_UNSUPPORTED_CMD)).await;
        // Transport still sends SyncNextMessage for the drain.
        let drain_cmd = self.read_command().await;
        assert_eq!(
            drain_cmd[0], CMD_SYNC_NEXT_MESSAGE,
            "expected SyncNextMessage drain even without SelfInfo"
        );
        // Return UNSUPPORTED_CMD — the fixed code must clear the drain flag.
        self.send(&err_frame(ERR_CODE_UNSUPPORTED_CMD)).await;
        // Transport sends CMD_GET_CONTACTS. Return error (unsupported) — that
        // is also silently tolerable; the advert bus just won't be pre-populated.
        let get_contacts = self.read_command().await;
        assert_eq!(
            get_contacts[0], CMD_GET_CONTACTS,
            "expected CMD_GET_CONTACTS"
        );
        self.send(&err_frame(ERR_CODE_UNSUPPORTED_CMD)).await;
        // Transport also queries autoadd config. Old firmware may not support
        // it — return UNSUPPORTED_CMD so the transport ignores it gracefully.
        let get_autoadd = self.read_command().await;
        assert_eq!(
            get_autoadd[0], CMD_GET_AUTOADD_CONFIG,
            "expected CMD_GET_AUTOADD_CONFIG"
        );
        self.send(&err_frame(ERR_CODE_UNSUPPORTED_CMD)).await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    /// Read one outbound frame payload (strips the 3-byte wire header).
    async fn read_command(&mut self) -> Vec<u8> {
        let header = self.recv_n(3).await;
        assert_eq!(header[0], FRAME_INBOUND_PREFIX);
        let len = u16::from_le_bytes([header[1], header[2]]) as usize;
        self.recv_n(len).await
    }

    /// Read outbound frames until a text send (`CMD_SEND_TXT_MSG`) appears,
    /// skipping path resets and other bookkeeping frames. Returns its payload.
    async fn read_text_send(&mut self) -> Vec<u8> {
        loop {
            let cmd = self.read_command().await;
            if cmd[0] == CMD_SEND_TXT_MSG {
                return cmd;
            }
        }
    }

    /// Read outbound frames until a `CMD_SYNC_NEXT_MESSAGE` appears, skipping
    /// replies, path resets, and connect-time bookkeeping (GET_CONTACTS /
    /// GET_AUTOADD_CONFIG).
    async fn read_until_sync(&mut self) {
        loop {
            let cmd = self.read_command().await;
            if cmd[0] == CMD_SYNC_NEXT_MESSAGE {
                return;
            }
        }
    }

    /// Read outbound frames until one with command byte `want` appears, skipping
    /// any others. Returns its payload.
    async fn read_until_cmd(&mut self, want: u8) -> Vec<u8> {
        loop {
            let cmd = self.read_command().await;
            if cmd[0] == want {
                return cmd;
            }
        }
    }
}

/// Spin up a [`MeshTransport`] against an in-process loopback listener.
///
/// `host` is an `Arc<MockHost>` so the caller can keep a clone for inspection.
async fn make_transport(host: Arc<MockHost>, prefix: Option<char>) -> (MeshTransport, Bridge) {
    make_transport_with(host, |cfg| cfg.command_prefix = prefix).await
}

/// Like [`make_transport`] but lets the caller tweak the `MeshConfig` before the
/// transport starts — e.g. to enable reply retransmission
/// (`reply_max_attempts > 1`), which is off by default.
async fn make_transport_with(
    host: Arc<MockHost>,
    customize: impl FnOnce(&mut MeshConfig),
) -> (MeshTransport, Bridge) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let mut config = MeshConfig {
        addr,
        welcome_message: String::new(), // suppressed in tests
        reconnect_delay_initial_ms: 20,
        reconnect_delay_max_ms: 50,
        // Adverts are on by default in production, but they add frames to the
        // connect/idle command stream that the handshake assertions don't expect.
        // Tests that exercise adverts opt in explicitly via `customize`.
        advert_on_connect: false,
        advert_interval_secs: 0,
        ..MeshConfig::default()
    };
    customize(&mut config);

    // Arc<MockHost> coerces to Arc<dyn Host>.
    let transport = MeshTransport::init(config, host).await.unwrap();
    transport.start().await.unwrap();

    let (stream, _) = listener.accept().await.unwrap();
    (transport, Bridge { stream })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// After the handshake, a `help` DM reaches the host as `Command::Help`.
#[tokio::test]
async fn help_command_reaches_host() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("help text".to_owned()));

    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;
    bridge.complete_handshake("TestNode").await;

    let sender = [0x01u8, 0x02, 0x03, 0x04, 0x05, 0x06];
    bridge.send(&contact_msg_frame(sender, "help")).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let received = host.commands_received();
    assert_eq!(received.len(), 1, "expected exactly 1 command");
    assert!(
        matches!(received[0].1, Command::Help { topic: None }),
        "expected Help{{None}}, got {:?}",
        received[0].1
    );

    transport.stop().await.unwrap();
}

/// The response text from the host is sent back to the sender.
#[tokio::test]
async fn response_text_returned_to_sender() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("Supply Drop BBS".to_owned()));

    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;
    bridge.complete_handshake("TestNode").await;

    let sender = [0xAAu8; 6];
    bridge.send(&contact_msg_frame(sender, "help")).await;

    // Payload layout: [CMD_SEND_TXT_MSG][txt_type][attempt][reserved×4][prefix×6][text…]
    // read_text_send skips the drain SyncNextMessage the transport now emits after
    // each processed inbound message (drain-to-NoMoreMessages hardening).
    let cmd_payload = tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
        .await
        .expect("timed out waiting for reply");

    assert_eq!(cmd_payload[0], CMD_SEND_TXT_MSG);
    let text = std::str::from_utf8(&cmd_payload[13..]).unwrap();
    assert_eq!(text, "Supply Drop BBS");

    transport.stop().await.unwrap();
}

/// A `Response::Prompt` sets `awaiting_reply`; the next message is dispatched
/// as `Command::WorkflowReply`.
#[tokio::test]
async fn prompt_sets_workflow_state() {
    let host = Arc::new(MockHost::new());
    host.set_response_for(
        |cmd| matches!(cmd, Command::Help { .. }),
        Response::Prompt {
            text: "Enter password:".to_owned(),
            hide_input: true,
        },
    );
    host.set_response_for(
        |cmd| matches!(cmd, Command::WorkflowReply { .. }),
        Response::Text("OK".to_owned()),
    );

    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;
    bridge.complete_handshake("Node").await;

    let sender = [0x11u8; 6];
    bridge.send(&contact_msg_frame(sender, "help")).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    bridge.send(&contact_msg_frame(sender, "mypassword")).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let received = host.commands_received();
    assert_eq!(received.len(), 2);
    assert!(matches!(received[0].1, Command::Help { .. }));
    assert!(
        matches!(&received[1].1, Command::WorkflowReply { reply } if reply == "mypassword"),
        "expected WorkflowReply, got {:?}",
        received[1].1
    );

    transport.stop().await.unwrap();
}

/// Workflow idle-timeout: a node stranded in a workflow because its prompt reply
/// was lost (every message it sends is consumed as workflow input it never sees a
/// response to) is freed after `workflow_timeout_secs`. The message that trips the
/// timeout is Cancelled out of the workflow and re-processed as a fresh command.
#[tokio::test]
async fn workflow_idle_timeout_frees_stranded_user() {
    let host = Arc::new(MockHost::new());
    // A workflow whose reply always re-prompts with the SAME text — models a stage
    // stuck because the user never sees the prompt (its reply keeps getting lost).
    host.set_response_for(
        |cmd| matches!(cmd, Command::Help { .. }),
        Response::Prompt {
            text: "Enter code:".to_owned(),
            hide_input: false,
        },
    );
    host.set_response_for(
        |cmd| matches!(cmd, Command::WorkflowReply { .. }),
        Response::Prompt {
            text: "Enter code:".to_owned(),
            hide_input: false,
        },
    );
    host.set_default_response(Response::Text("ok".to_owned())); // Cancel etc.

    let (transport, mut bridge) =
        make_transport_with(Arc::clone(&host), |cfg| cfg.workflow_timeout_secs = 1).await;
    bridge.complete_handshake("Node").await;

    let sender = [0x51u8; 6];
    // Enter the workflow.
    bridge.send(&contact_msg_frame(sender, "help")).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
        .await
        .expect("initial prompt");

    // Wait past the 1s idle window, then send another message. It should trip the
    // timeout: Cancel the stale workflow and re-parse "help" as a fresh Help.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    bridge.send(&contact_msg_frame(sender, "help")).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
        .await
        .expect("reply after timeout");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let received = host.commands_received();
    let cmds: Vec<_> = received.iter().map(|(_, c)| c).collect();
    assert!(
        received.iter().any(|(_, c)| matches!(c, Command::Cancel)),
        "the idle-timeout must cancel the stale workflow, got: {cmds:?}"
    );
    assert_eq!(
        received
            .iter()
            .filter(|(_, c)| matches!(c, Command::WorkflowReply { .. }))
            .count(),
        0,
        "the message that tripped the timeout must NOT be consumed as a workflow reply, got: {cmds:?}"
    );
    assert_eq!(
        received
            .iter()
            .filter(|(_, c)| matches!(c, Command::Help { .. }))
            .count(),
        2,
        "both messages reached the host as Help (the 2nd re-parsed after the timeout), got: {cmds:?}"
    );

    transport.stop().await.unwrap();
}

/// The device pushes `PathUpdated` with a node's full 32-byte pubkey after learning
/// a route. Capturing it enables `flood_after_send`: the post-reply `ResetPath`
/// (which needs the full key — an inbound DM carries only the 6-byte prefix) then
/// fires, so the next reply to that node floods instead of dying on a stale direct
/// path. Without the capture, `get_full_pubkey` is `None` and `ResetPath` is skipped.
#[tokio::test]
async fn path_updated_enables_flood_after_send() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("ok".to_owned()));
    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;
    bridge.complete_handshake("Node").await;

    let sender = [0x55u8; 6];
    // Establish a session first (set_full_pubkey no-ops without one).
    bridge.send(&contact_msg_frame(sender, "help")).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
        .await
        .expect("first reply");

    // Device reports the full pubkey (its first 6 bytes are the sender's prefix).
    let mut pubkey = [0u8; 32];
    pubkey[..6].copy_from_slice(&sender);
    pubkey[6] = 0xAB;
    bridge.send(&path_updated_frame(pubkey)).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    // The next reply should now be followed by a ResetPath (flood_after_send).
    bridge.send(&contact_msg_frame(sender, "help")).await;
    let mut saw_reset = false;
    for _ in 0..8 {
        match tokio::time::timeout(Duration::from_secs(2), bridge.read_command()).await {
            Ok(cmd) if cmd[0] == CMD_RESET_PATH => {
                saw_reset = true;
                break;
            }
            Ok(_) => continue,
            Err(_) => break,
        }
    }
    assert!(
        saw_reset,
        "flood_after_send must emit ResetPath once PathUpdated supplied the full pubkey"
    );

    transport.stop().await.unwrap();
}

/// Two distinct messages that produce the SAME reply text in the same second must
/// still be stamped with DIFFERENT outbound timestamps, so the radio's outbound
/// dedup doesn't collapse them into one byte-identical frame and silently drop the
/// second reply (the mirror of the inbound whole-second collision). The SendTxtMsg
/// timestamp is bytes [3..7] of the payload.
#[tokio::test]
async fn same_second_identical_replies_get_distinct_timestamps() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("ok".to_owned()));
    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;
    bridge.complete_handshake("Node").await;

    let sender = [0x61u8; 6];
    // Two messages back-to-back (same wall-clock second), same canned reply.
    bridge.send(&contact_msg_frame(sender, "a")).await;
    bridge.send(&contact_msg_frame(sender, "b")).await;

    let r1 = tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
        .await
        .expect("first reply");
    let r2 = tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
        .await
        .expect("second reply");
    let ts1 = u32::from_le_bytes([r1[3], r1[4], r1[5], r1[6]]);
    let ts2 = u32::from_le_bytes([r2[3], r2[4], r2[5], r2[6]]);
    assert!(
        ts2 > ts1,
        "outbound reply timestamps must be strictly increasing so same-second \
         identical replies aren't dedup-collapsed by the radio (ts1={ts1}, ts2={ts2})"
    );

    transport.stop().await.unwrap();
}

/// Issue #104: two identical workflow replies in a row (e.g. the password and
/// its matching confirmation) must BOTH reach the host. The general
/// retransmission dedup must not silently drop the second one just because its
/// text equals the first — a new prompt between them starts a fresh reply turn.
#[tokio::test]
async fn identical_workflow_replies_after_prompt_both_reach_host() {
    let host = Arc::new(MockHost::new());
    // The initial command and every workflow reply return a Prompt, so the
    // session stays in awaiting-reply mode across all three messages (mirrors
    // "Choose a password:" → "Confirm your password:").
    host.set_response_for(
        |cmd| matches!(cmd, Command::Help { .. }),
        Response::Prompt {
            text: "Choose a password:".to_owned(),
            hide_input: true,
        },
    );
    host.set_response_for(
        |cmd| matches!(cmd, Command::WorkflowReply { .. }),
        Response::Prompt {
            text: "Confirm your password:".to_owned(),
            hide_input: true,
        },
    );

    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;
    bridge.complete_handshake("Node").await;

    let sender = [0x33u8; 6];
    bridge.send(&contact_msg_frame(sender, "help")).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    // Password, then the SAME text again as confirmation.
    bridge.send(&contact_msg_frame(sender, "hunter2pw")).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    bridge.send(&contact_msg_frame(sender, "hunter2pw")).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let received = host.commands_received();
    let reply_count = received
        .iter()
        .filter(|(_, c)| matches!(c, Command::WorkflowReply { reply } if reply == "hunter2pw"))
        .count();
    assert_eq!(
        reply_count,
        2,
        "both identical confirmations must reach the host, got commands: {:?}",
        received.iter().map(|(_, c)| c).collect::<Vec<_>>()
    );

    transport.stop().await.unwrap();
}

/// A true retransmission — the same message re-sent by the radio or a client
/// that did not hear its ACK, reusing the sender's per-message timestamp — must
/// reach the host only once, even though the text and session state are
/// unchanged. This is the "Error: Already logged in" hardening: a resend can no
/// longer be reprocessed, including copies that arrive after the text-only
/// window or out of step with the workflow state.
#[tokio::test]
async fn retransmitted_command_with_same_timestamp_reaches_host_once() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("ok".to_owned()));

    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;
    bridge.complete_handshake("Node").await;

    let sender = [0x77u8; 6];
    let ts = 1_700_000_900u32;
    // The same message arrives three times reusing the sender's timestamp.
    for _ in 0..3 {
        bridge.send(&contact_msg_frame_ts(sender, "help", ts)).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let help_count = host
        .commands_received()
        .iter()
        .filter(|(_, c)| matches!(c, Command::Help { .. }))
        .count();
    assert_eq!(
        help_count, 1,
        "a retransmitted command (same sender timestamp) must be processed once"
    );

    transport.stop().await.unwrap();
}

/// Dispatch runs on a single off-loop command worker; this guards the invariant
/// the offload relies on — a node's messages reach the host in arrival order. A
/// per-message spawn instead of one FIFO worker would let workflow input race
/// and reorder.
#[tokio::test]
async fn commands_from_same_node_processed_in_order() {
    let host = Arc::new(MockHost::new());
    // Every command returns a Prompt, so the session stays in awaiting-reply
    // mode and each later message is delivered verbatim as a WorkflowReply.
    host.set_default_response(Response::Prompt {
        text: "?".to_owned(),
        hide_input: false,
    });

    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;
    bridge.complete_handshake("Node").await;

    let sender = [0x44u8; 6];
    bridge.send(&contact_msg_frame(sender, "help")).await; // enters awaiting-reply
    for reply in ["one", "two", "three"] {
        tokio::time::sleep(Duration::from_millis(30)).await;
        bridge.send(&contact_msg_frame(sender, reply)).await;
    }
    tokio::time::sleep(Duration::from_millis(60)).await;

    let replies: Vec<String> = host
        .commands_received()
        .iter()
        .filter_map(|(_, c)| match c {
            Command::WorkflowReply { reply } => Some(reply.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(
        replies,
        vec!["one", "two", "three"],
        "messages must reach the host in arrival order"
    );

    transport.stop().await.unwrap();
}

/// Two different nodes interleaving through the single FIFO worker: each node's
/// messages reach the host in that node's own arrival order, and the per-node
/// dedup rings don't cross-contaminate.
#[tokio::test]
async fn two_nodes_interleaved_preserve_per_node_order() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Prompt {
        text: "?".to_owned(),
        hide_input: false,
    });

    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;
    bridge.complete_handshake("Node").await;

    let a = [0xA1u8; 6];
    let b = [0xB2u8; 6];
    // Each node enters awaiting-reply, then sends ordered replies, interleaved.
    bridge.send(&contact_msg_frame(a, "help")).await;
    bridge.send(&contact_msg_frame(b, "help")).await;
    for (ra, rb) in [("a1", "b1"), ("a2", "b2"), ("a3", "b3")] {
        tokio::time::sleep(Duration::from_millis(20)).await;
        bridge.send(&contact_msg_frame(a, ra)).await;
        bridge.send(&contact_msg_frame(b, rb)).await;
    }
    tokio::time::sleep(Duration::from_millis(80)).await;

    let replies: Vec<String> = host
        .commands_received()
        .iter()
        .filter_map(|(_, c)| match c {
            Command::WorkflowReply { reply } => Some(reply.clone()),
            _ => None,
        })
        .collect();
    let a_order: Vec<&str> = replies
        .iter()
        .filter(|r| r.starts_with('a'))
        .map(String::as_str)
        .collect();
    let b_order: Vec<&str> = replies
        .iter()
        .filter(|r| r.starts_with('b'))
        .map(String::as_str)
        .collect();
    assert_eq!(a_order, vec!["a1", "a2", "a3"], "node A in arrival order");
    assert_eq!(b_order, vec!["b1", "b2", "b3"], "node B in arrival order");

    transport.stop().await.unwrap();
}

/// A burst larger than the worker queue depth applies backpressure (the event
/// loop blocks on `send`) rather than dropping work: with a deliberately slow
/// host, every message still reaches the host exactly once, in order.
#[tokio::test]
async fn burst_beyond_queue_depth_is_not_dropped_and_stays_ordered() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Prompt {
        text: "?".to_owned(),
        hide_input: false,
    });
    // Slow host so the worker drains slower than the burst arrives, forcing the
    // bounded channel (COMMAND_QUEUE_DEPTH = 64) to fill and backpressure.
    host.set_process_delay(Duration::from_millis(10));

    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;
    bridge.complete_handshake("Node").await;

    let sender = [0xCCu8; 6];
    bridge.send(&contact_msg_frame(sender, "help")).await; // enters awaiting-reply
    let n = 80usize; // > COMMAND_QUEUE_DEPTH
    for i in 0..n {
        bridge
            .send(&contact_msg_frame(sender, &format!("m{i}")))
            .await;
    }
    // Wait for the slow host to drain the whole burst (~n * delay + margin).
    tokio::time::sleep(Duration::from_millis(n as u64 * 10 + 800)).await;

    let replies: Vec<String> = host
        .commands_received()
        .iter()
        .filter_map(|(_, c)| match c {
            Command::WorkflowReply { reply } => Some(reply.clone()),
            _ => None,
        })
        .collect();
    let expected: Vec<String> = (0..n).map(|i| format!("m{i}")).collect();
    assert_eq!(
        replies, expected,
        "every burst message must reach the host once, in order, with none dropped"
    );

    transport.stop().await.unwrap();
}

/// A sender that supplies no per-message timestamp (`timestamp == 0`) still gets
/// retransmission dedup via the text-only fallback window — the path that guards
/// legacy/no-clock devices. Exercises the `timestamp == 0` branch end-to-end.
#[tokio::test]
async fn zero_timestamp_retransmission_deduped_via_text_window() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("ok".to_owned()));

    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;
    bridge.complete_handshake("Node").await;

    let sender = [0x91u8; 6];
    // Same command twice with timestamp 0, inside the text-dedup window.
    bridge.send(&contact_msg_frame_ts(sender, "help", 0)).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    bridge.send(&contact_msg_frame_ts(sender, "help", 0)).await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let help_count = host
        .commands_received()
        .iter()
        .filter(|(_, c)| matches!(c, Command::Help { .. }))
        .count();
    assert_eq!(
        help_count, 1,
        "a zero-timestamp retransmission must be dropped by the text-only fallback"
    );

    transport.stop().await.unwrap();
}

/// With `timestamp == 0`, a retransmitted workflow reply that lands after the
/// workflow completed is still dropped by the text-only fallback (the
/// `dedup_message` / `is_recent_workflow_reply` guards), so the host never
/// reprocesses it.
#[tokio::test]
async fn zero_timestamp_workflow_reply_retransmission_after_completion_dropped() {
    let host = Arc::new(MockHost::new());
    host.set_response_for(
        |cmd| matches!(cmd, Command::Help { .. }),
        Response::Prompt {
            text: "enter code:".to_owned(),
            hide_input: false,
        },
    );
    host.set_response_for(
        |cmd| matches!(cmd, Command::WorkflowReply { .. }),
        Response::Text("done".to_owned()),
    );

    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;
    bridge.complete_handshake("Node").await;

    let sender = [0x92u8; 6];
    bridge.send(&contact_msg_frame_ts(sender, "help", 0)).await; // → Prompt
    tokio::time::sleep(Duration::from_millis(50)).await;
    bridge.send(&contact_msg_frame_ts(sender, "1234", 0)).await; // reply → completes
    tokio::time::sleep(Duration::from_millis(50)).await;
    bridge.send(&contact_msg_frame_ts(sender, "1234", 0)).await; // retransmit after completion
    tokio::time::sleep(Duration::from_millis(50)).await;

    let reply_count = host
        .commands_received()
        .iter()
        .filter(|(_, c)| matches!(c, Command::WorkflowReply { reply } if reply == "1234"))
        .count();
    assert_eq!(
        reply_count, 1,
        "a zero-timestamp workflow-reply retransmission after completion must be dropped"
    );

    transport.stop().await.unwrap();
}

/// Boundary/documentation test for the dedup invariant: two identical workflow
/// replies that reuse ONE timestamp are treated as a retransmission, so the
/// second is dropped. This is exactly why issue #104 (password + identical
/// confirmation) relies on the client stamping each distinct send with a DISTINCT
/// timestamp — see `identical_workflow_replies_after_prompt_both_reach_host` for
/// the distinct-timestamp (real-client) path where both reach the host.
#[tokio::test]
async fn identical_workflow_replies_with_same_timestamp_dedup_second() {
    let host = Arc::new(MockHost::new());
    host.set_response_for(
        |cmd| matches!(cmd, Command::Help { .. }),
        Response::Prompt {
            text: "Choose a password:".to_owned(),
            hide_input: true,
        },
    );
    host.set_response_for(
        |cmd| matches!(cmd, Command::WorkflowReply { .. }),
        Response::Prompt {
            text: "Confirm your password:".to_owned(),
            hide_input: true,
        },
    );

    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;
    bridge.complete_handshake("Node").await;

    let sender = [0x93u8; 6];
    bridge.send(&contact_msg_frame(sender, "help")).await; // distinct ts, enters workflow
    tokio::time::sleep(Duration::from_millis(50)).await;
    let pw_ts = 1_700_050_000u32;
    bridge
        .send(&contact_msg_frame_ts(sender, "hunter2pw", pw_ts))
        .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    bridge
        .send(&contact_msg_frame_ts(sender, "hunter2pw", pw_ts))
        .await; // SAME timestamp ⇒ a retransmission
    tokio::time::sleep(Duration::from_millis(50)).await;

    let reply_count = host
        .commands_received()
        .iter()
        .filter(|(_, c)| matches!(c, Command::WorkflowReply { reply } if reply == "hunter2pw"))
        .count();
    assert_eq!(
        reply_count, 1,
        "a confirmation reusing the same timestamp is a retransmission and is dropped"
    );

    transport.stop().await.unwrap();
}

/// After a prompt, the next message must reach the host as a `WorkflowReply` —
/// it must NOT be re-parsed as a standalone command at the transport layer. The
/// host owns the decision of how to interpret a reply (including whether a
/// REGISTER/LOGIN reply should escape the workflow); the transport must deliver
/// the text verbatim so that decision is possible.
#[tokio::test]
async fn reply_after_prompt_reaches_host_as_workflow_reply() {
    let host = Arc::new(MockHost::new());
    host.set_response_for(
        |cmd| matches!(cmd, Command::Register { .. }),
        Response::Prompt {
            text: "Registering 'qatester1'. Choose a password (min 8 characters):".to_owned(),
            hide_input: true,
        },
    );
    host.set_response_for(
        |cmd| matches!(cmd, Command::WorkflowReply { .. }),
        Response::Prompt {
            text: "Confirm the password for 'qatester1':".to_owned(),
            hide_input: true,
        },
    );

    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;
    bridge.complete_handshake("Node").await;

    let sender = [0x44u8; 6];
    bridge
        .send(&contact_msg_frame(sender, "register qatester1"))
        .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    // A reply that happens to look like a command keyword must still be delivered
    // as a WorkflowReply (the host decides whether it escapes the workflow).
    bridge
        .send(&contact_msg_frame(sender, "register qatester2"))
        .await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let received = host.commands_received();
    assert!(
        received.iter().any(
            |(_, c)| matches!(c, Command::WorkflowReply { reply } if reply == "register qatester2")
        ),
        "input after a prompt must reach the host as a WorkflowReply, got: {:?}",
        received.iter().map(|(_, c)| c).collect::<Vec<_>>()
    );

    transport.stop().await.unwrap();
}

/// With a prefix configured, messages without the prefix are silently ignored.
#[tokio::test]
async fn prefix_filters_non_prefixed_messages() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("ok".to_owned()));

    let (transport, mut bridge) = make_transport(Arc::clone(&host), Some('!')).await;
    bridge.complete_handshake("Node").await;

    let sender = [0x22u8; 6];

    bridge.send(&contact_msg_frame(sender, "help")).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        host.commands_received().len(),
        0,
        "unprefixed message should be ignored"
    );

    bridge.send(&contact_msg_frame(sender, "!help")).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let cmds = host.commands_received();
    assert_eq!(cmds.len(), 1);
    assert!(matches!(cmds[0].1, Command::Help { .. }));

    transport.stop().await.unwrap();
}

/// With `advert_on_connect` enabled, the transport broadcasts a self-advert as
/// part of the connect command burst, so the mesh (re)learns a route to the BBS
/// right away. The advert is flooded (flood byte = 1).
#[tokio::test]
async fn self_advert_broadcast_on_connect() {
    let host = Arc::new(MockHost::new());
    let (transport, mut bridge) = make_transport_with(Arc::clone(&host), |cfg| {
        cfg.advert_on_connect = true;
        cfg.advert_interval_secs = 0; // isolate the on-connect advert
    })
    .await;

    // Manual handshake: after SelfInfo, the connect burst carries a self-advert.
    let app_start = bridge.recv_n(11).await;
    assert_eq!(app_start[3], CMD_APP_START);
    bridge.send(&self_info_frame("Node")).await;

    let advert = tokio::time::timeout(
        Duration::from_secs(2),
        bridge.read_until_cmd(CMD_SEND_SELF_ADVERT),
    )
    .await
    .expect("expected a self-advert in the connect command burst");
    assert_eq!(
        advert[1], 1,
        "on-connect advert should flood (flood byte = 1)"
    );

    transport.stop().await.unwrap();
}

/// With `advert_interval_secs` set, the transport broadcasts a self-advert on the
/// periodic tick. Uses a 1s interval so the test is fast; `advert_on_connect` is
/// off so the only advert seen is the periodic one.
#[tokio::test]
async fn periodic_self_advert_fires() {
    let host = Arc::new(MockHost::new());
    let (transport, mut bridge) = make_transport_with(Arc::clone(&host), |cfg| {
        cfg.advert_on_connect = false; // isolate the periodic advert
        cfg.advert_interval_secs = 1;
    })
    .await;
    // No connect advert (advert_on_connect = false), so the fixed handshake holds.
    bridge.complete_handshake("Node").await;

    // The immediate first tick is skipped, so the first periodic advert lands
    // ~1s after start. Generous timeout to avoid CI flake.
    let advert = tokio::time::timeout(
        Duration::from_secs(3),
        bridge.read_until_cmd(CMD_SEND_SELF_ADVERT),
    )
    .await
    .expect("expected a periodic self-advert within the interval window");
    assert_eq!(
        advert[1], 1,
        "periodic advert should flood (flood byte = 1)"
    );

    transport.stop().await.unwrap();
}

/// With both advert knobs off, NO self-advert is ever emitted — not in the
/// connect burst, not on a timer. Guards the invariant the strict handshake
/// assertions rely on (and the production off-switch).
#[tokio::test]
async fn no_self_advert_when_disabled() {
    let host = Arc::new(MockHost::new());
    let (transport, mut bridge) = make_transport_with(Arc::clone(&host), |cfg| {
        cfg.advert_on_connect = false;
        cfg.advert_interval_secs = 0;
    })
    .await;

    // Manual handshake so we can inspect every frame of the connect burst.
    let app_start = bridge.recv_n(11).await;
    assert_eq!(app_start[3], CMD_APP_START);
    bridge.send(&self_info_frame("Node")).await;

    // Read through the whole connect burst (ends at GetAutoaddConfig), asserting
    // no self-advert appears anywhere in it.
    loop {
        let cmd = bridge.read_command().await;
        assert_ne!(
            cmd[0], CMD_SEND_SELF_ADVERT,
            "no self-advert must be emitted when adverts are disabled"
        );
        if cmd[0] == CMD_SYNC_NEXT_MESSAGE {
            bridge
                .send(&radio_frame(&[RESP_CODE_NO_MORE_MESSAGES]))
                .await;
        }
        if cmd[0] == CMD_GET_AUTOADD_CONFIG {
            break;
        }
    }

    transport.stop().await.unwrap();
}

/// `notify()` delivers a `SendTxtMsg` to the correct node.
#[tokio::test]
async fn notify_sends_text_to_node() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("ok".to_owned()));

    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;
    bridge.complete_handshake("Node").await;

    // Establish a session by sending a message first.
    let sender = [0x33u8; 6];
    bridge.send(&contact_msg_frame(sender, "whoami")).await;
    // read_text_send skips the post-message drain SyncNextMessage.
    let _ = tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
        .await
        .expect("timed out waiting for whoami reply");

    let session = host.commands_received()[0].0;
    let outcome = transport
        .notify(session, Notification::Text("You have mail!".to_owned()))
        .await
        .unwrap();
    assert!(matches!(outcome, NotifyOutcome::Queued));

    let cmd_payload = tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
        .await
        .expect("timed out waiting for notification");

    assert_eq!(cmd_payload[0], CMD_SEND_TXT_MSG);
    let text = std::str::from_utf8(&cmd_payload[13..]).unwrap();
    assert_eq!(text, "You have mail!");

    transport.stop().await.unwrap();
}

/// `notify()` for an unknown session returns `NotifyOutcome::Dropped`.
#[tokio::test]
async fn notify_unknown_session_drops() {
    use bbs_plugin_api::identity::SessionId;

    let host = Arc::new(MockHost::new());
    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;
    bridge.complete_handshake("Node").await;

    let bogus = SessionId::__internal_new(0xDEAD_BEEF);
    let outcome = transport
        .notify(bogus, Notification::Text("hello".to_owned()))
        .await
        .unwrap();
    assert!(matches!(outcome, NotifyOutcome::Dropped));

    transport.stop().await.unwrap();
}

/// When the device returns UNSUPPORTED_CMD for CMD_APP_START *and*
/// CMD_SYNC_NEXT_MESSAGE, the drain flag must be cleared so subsequent
/// ContactMsgRecv frames are processed normally.
///
/// Regression test for the drain-forever bug: without the fix an
/// UNSUPPORTED_CMD error during drain falls to the catch-all debug log,
/// leaving `draining = true` permanently and silently dropping every
/// message the user sends.
#[tokio::test]
async fn unsupported_app_start_and_sync_still_processes_messages() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("pong".to_owned()));

    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;
    // Simulate firmware that rejects CMD_APP_START and CMD_SYNC_NEXT_MESSAGE.
    bridge.complete_handshake_unsupported_app_start().await;

    // Send a user message — it should reach the host (drain was cleared).
    let sender = [0x55u8; 6];
    bridge.send(&contact_msg_frame(sender, "help")).await;

    // The transport should send back a reply (from MockHost). read_text_send skips
    // the post-message drain SyncNextMessage.
    let cmd_payload = tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
        .await
        .expect("timed out — drain flag was not cleared; message was silently dropped");

    assert_eq!(
        cmd_payload[0], CMD_SEND_TXT_MSG,
        "expected SendTxtMsg reply to user"
    );
    let text = std::str::from_utf8(&cmd_payload[13..]).unwrap();
    assert_eq!(text, "pong");

    transport.stop().await.unwrap();
}

/// Same as above but after UNSUPPORTED_CMD for AppStart the *drain* command
/// succeeds (NoMoreMessages). Verifies the normal-but-no-SelfInfo path still
/// works and messages are dispatched.
#[tokio::test]
async fn unsupported_app_start_with_successful_drain_processes_messages() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("ok".to_owned()));

    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;

    // Handshake: AppStart → UNSUPPORTED_CMD, then drain completes normally.
    let app_start = bridge.recv_n(11).await;
    assert_eq!(app_start[3], CMD_APP_START);
    bridge.send(&err_frame(ERR_CODE_UNSUPPORTED_CMD)).await;

    let drain_cmd = bridge.read_command().await;
    assert_eq!(drain_cmd[0], CMD_SYNC_NEXT_MESSAGE);
    bridge
        .send(&radio_frame(&[RESP_CODE_NO_MORE_MESSAGES]))
        .await;

    let get_contacts = bridge.read_command().await;
    assert_eq!(get_contacts[0], CMD_GET_CONTACTS);
    // Transport queries autoadd config; reply with config=1 (already enabled).
    let get_autoadd = bridge.read_command().await;
    assert_eq!(get_autoadd[0], CMD_GET_AUTOADD_CONFIG);
    bridge
        .send(&radio_frame(&[RESP_CODE_AUTOADD_CONFIG, 1]))
        .await;
    tokio::time::sleep(Duration::from_millis(20)).await;

    // Send a message — should be processed.
    let sender = [0x66u8; 6];
    bridge.send(&contact_msg_frame(sender, "whoami")).await;

    // read_text_send skips the post-message drain SyncNextMessage.
    let cmd_payload = tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
        .await
        .expect("timed out waiting for reply");
    assert_eq!(cmd_payload[0], CMD_SEND_TXT_MSG);

    transport.stop().await.unwrap();
}

/// Drain-to-NoMoreMessages: after processing a normal (non-draining) inbound
/// message, the transport emits a follow-up `SyncNextMessage` so it keeps pulling
/// until the bridge reports the queue empty. This makes delivery robust to a
/// dropped `MsgWaiting` push (or a bridge/firmware that notifies once per
/// empty→non-empty transition) instead of stranding a backlog until reconnect.
#[tokio::test]
async fn processed_message_emits_followup_sync() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("ok".to_owned()));
    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;
    bridge.complete_handshake("Node").await;

    let sender = [0x21u8; 6];
    bridge.send(&contact_msg_frame(sender, "help")).await;

    // Collect outbound frames until we've seen BOTH the text reply and the drain
    // SyncNextMessage (their relative order is not guaranteed).
    let mut saw_sync = false;
    let mut saw_reply = false;
    for _ in 0..6 {
        let cmd = tokio::time::timeout(Duration::from_secs(2), bridge.read_command())
            .await
            .expect("timed out waiting for outbound frames");
        match cmd[0] {
            CMD_SYNC_NEXT_MESSAGE => saw_sync = true,
            CMD_SEND_TXT_MSG => saw_reply = true,
            _ => {}
        }
        if saw_sync && saw_reply {
            break;
        }
    }
    assert!(saw_reply, "the text reply must be sent");
    assert!(
        saw_sync,
        "a follow-up SyncNextMessage must be emitted to drain the bridge queue"
    );

    transport.stop().await.unwrap();
}

/// On reconnect the transport drains the bridge's queued backlog and processes
/// EVERY message — including one whose sender clock looks old. The timestamp is
/// stamped by the sender, whose RTC may never have been synced (firmware's
/// unsynced default is a 2024 epoch), so an old-looking stamp is not evidence of
/// staleness; age-based discarding silently ate real commands from clockless
/// nodes. The backlog is bounded (the firmware offline queue holds 16 frames),
/// so processing everything cannot burst-reply meaningfully.
#[tokio::test]
async fn reconnect_drain_processes_entire_backlog() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("ok".to_owned()));
    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;

    // Manual handshake so queued messages can be injected during the on-connect
    // drain (before NoMoreMessages clears the draining flag).
    let app_start = bridge.recv_n(11).await;
    assert_eq!(app_start[3], CMD_APP_START);
    bridge.send(&self_info_frame("Node")).await;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs() as u32;
    let sender = [0x31u8; 6];

    // First drain sync → feed a FRESH queued message (age ~0).
    let drain1 = bridge.read_command().await;
    assert_eq!(drain1[0], CMD_SYNC_NEXT_MESSAGE);
    bridge
        .send(&contact_msg_frame_ts(sender, "fresh", now))
        .await;

    // Next drain sync → feed a message whose sender clock reads an hour behind
    // (a never-synced RTC can be years behind — must still be processed).
    bridge.read_until_sync().await;
    bridge
        .send(&contact_msg_frame_ts(
            sender,
            "oldclock",
            now.saturating_sub(3600),
        ))
        .await;

    // Next drain sync → end the drain.
    bridge.read_until_sync().await;
    bridge
        .send(&radio_frame(&[RESP_CODE_NO_MORE_MESSAGES]))
        .await;

    tokio::time::sleep(Duration::from_millis(100)).await;

    let received = host.commands_received();
    let has = |t: &str| {
        received
            .iter()
            .any(|(_, c)| matches!(c, Command::Unknown { raw } if raw == t))
    };
    assert!(
        has("fresh"),
        "a fresh queued message must be processed on reconnect, got: {:?}",
        received.iter().map(|(_, c)| c).collect::<Vec<_>>()
    );
    assert!(
        has("oldclock"),
        "a queued message with an old sender clock must be processed too, got: {:?}",
        received.iter().map(|(_, c)| c).collect::<Vec<_>>()
    );

    transport.stop().await.unwrap();
}

/// Channel (group) messages share the device's offline queue with DMs, so a
/// `CMD_SYNC_NEXT_MESSAGE` pop can return one. The drain chain must continue
/// past it: without the fix, a channel message popped during the reconnect
/// drain pinned `draining=true` and stranded any DM queued behind it.
#[tokio::test]
async fn channel_message_during_drain_does_not_stall_the_chain() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("ok".to_owned()));
    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;

    // Manual handshake so queue pops can be injected during the on-connect drain.
    let app_start = bridge.recv_n(11).await;
    assert_eq!(app_start[3], CMD_APP_START);
    bridge.send(&self_info_frame("Node")).await;

    let sender = [0x35u8; 6];

    // First drain sync → the queue pop returns a CHANNEL message.
    let drain1 = bridge.read_command().await;
    assert_eq!(drain1[0], CMD_SYNC_NEXT_MESSAGE);
    bridge.send(&channel_msg_frame(0, "group chatter")).await;

    // The chain must continue: another sync pops the DM queued BEHIND it.
    bridge.read_until_sync().await;
    bridge.send(&contact_msg_frame(sender, "behind-chan")).await;

    // And on to the end of the queue.
    bridge.read_until_sync().await;
    bridge
        .send(&radio_frame(&[RESP_CODE_NO_MORE_MESSAGES]))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    let received = host.commands_received();
    assert!(
        received
            .iter()
            .any(|(_, c)| matches!(c, Command::Unknown { raw } if raw == "behind-chan")),
        "a DM queued behind a channel message must still be drained and processed, got: {:?}",
        received.iter().map(|(_, c)| c).collect::<Vec<_>>()
    );

    // Draining must have cleared: a LIVE message is processed normally.
    bridge.send(&contact_msg_frame(sender, "live-after")).await;
    let _ = tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
        .await
        .expect("live message after drain must be answered (draining stuck?)");

    transport.stop().await.unwrap();
}

/// In NORMAL mode the drain-to-NoMoreMessages recovery must also continue past a
/// channel message: a DM whose MsgWaiting tickle was lost can sit queued behind
/// one, and only the continued sync chain recovers it.
#[tokio::test]
async fn channel_message_in_normal_mode_continues_queue_sync() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("ok".to_owned()));
    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;
    bridge.complete_handshake("Node").await;

    let sender = [0x36u8; 6];

    // A MsgWaiting tickle whose pop returns a CHANNEL message; the DM behind it
    // never got a tickle (dropped). The continued chain must recover it.
    bridge.send(&radio_frame(&[PUSH_CODE_MSG_WAITING])).await;
    bridge.read_until_sync().await;
    bridge.send(&channel_msg_frame(0, "group chatter")).await;

    // Continuation sync → pops the tickle-less DM.
    bridge.read_until_sync().await;
    bridge
        .send(&contact_msg_frame(sender, "tickleless-dm"))
        .await;

    // The DM's own continuation → queue is empty.
    bridge.read_until_sync().await;
    bridge
        .send(&radio_frame(&[RESP_CODE_NO_MORE_MESSAGES]))
        .await;
    tokio::time::sleep(Duration::from_millis(100)).await;

    assert!(
        host.commands_received()
            .iter()
            .any(|(_, c)| matches!(c, Command::Unknown { raw } if raw == "tickleless-dm")),
        "a tickle-less DM behind a channel message must be recovered by the sync chain"
    );

    transport.stop().await.unwrap();
}

/// A reply that is accepted by the device (RESP_CODE_SENT with a CRC) but never
/// confirmed (no PUSH_CODE_SEND_CONFIRMED) is retransmitted after the ack-wait
/// floor — this is the core reply-reliability fix for the lossy return path.
#[tokio::test]
async fn unacked_reply_is_retransmitted() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("hi there".to_owned()));

    // Retransmission is opt-in (default 1); this test exercises it.
    let (transport, mut bridge) =
        make_transport_with(Arc::clone(&host), |cfg| cfg.reply_max_attempts = 3).await;
    bridge.complete_handshake("Node").await;

    let sender = [0x55u8; 6];
    bridge.send(&contact_msg_frame(sender, "help")).await;

    // First transmission of the reply (wire attempt 0).
    let first = tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
        .await
        .expect("first reply not sent");
    assert_eq!(first[0], CMD_SEND_TXT_MSG);
    assert_eq!(first[2], 0, "first send is attempt 0");
    assert_eq!(std::str::from_utf8(&first[13..]).unwrap(), "hi there");

    // Device accepts it (assigns CRC) but the destination never acknowledges.
    bridge.send(&sent_frame(2, 0xDEAD_BEEF, 100)).await;

    // After the ack-wait floor the BBS retransmits the same text (wire attempt 1).
    let retry = tokio::time::timeout(Duration::from_secs(8), bridge.read_text_send())
        .await
        .expect("reply was not retransmitted");
    assert_eq!(retry[0], CMD_SEND_TXT_MSG);
    assert_eq!(std::str::from_utf8(&retry[13..]).unwrap(), "hi there");
    assert_eq!(retry[2], 1, "retransmission is attempt 1 (0-based wire)");

    transport.stop().await.unwrap();
}

/// A reply that is confirmed delivered (PUSH_CODE_SEND_CONFIRMED) is NOT
/// retransmitted — delivery is at-least-once, not blindly duplicated.
#[tokio::test]
async fn confirmed_reply_is_not_retransmitted() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("hi there".to_owned()));

    // Retransmission is opt-in (default 1); enable it so "confirmed ⇒ no retry"
    // is actually under test.
    let (transport, mut bridge) =
        make_transport_with(Arc::clone(&host), |cfg| cfg.reply_max_attempts = 3).await;
    bridge.complete_handshake("Node").await;

    let sender = [0x56u8; 6];
    bridge.send(&contact_msg_frame(sender, "help")).await;

    let first = tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
        .await
        .expect("first reply not sent");
    assert_eq!(first[0], CMD_SEND_TXT_MSG);

    // Device accepts AND the destination confirms delivery.
    let crc = 0xAABB_CCDD;
    bridge.send(&sent_frame(2, crc, 100)).await;
    bridge.send(&send_confirmed_frame(crc)).await;

    // Past the ack-wait floor there must be no second text send.
    let retry = tokio::time::timeout(Duration::from_secs(6), bridge.read_text_send()).await;
    assert!(
        retry.is_err(),
        "confirmed reply must not be retransmitted (got a duplicate send)"
    );

    transport.stop().await.unwrap();
}

/// Reproduction harness: drive a **real** `BbsHost` (temp DB) through the bridge,
/// so transport `awaiting_reply` ↔ host workflow ↔ session interactions are
/// exercised end-to-end (MockHost can't model them). Regression for the QA bug
/// where the first reply to the interactive "choose a password" prompt was
/// dropped to the anonymous banner.
#[tokio::test]
async fn real_host_interactive_register_prompt_reply_advances() {
    let dbfile = tempfile::NamedTempFile::new().unwrap();
    let db = bbs_core::Database::open(&dbfile.path().to_string_lossy())
        .await
        .unwrap();
    let host: Arc<dyn bbs_plugin_api::Host> = Arc::new(bbs_core::BbsHost::new(db));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let config = MeshConfig {
        addr,
        command_prefix: None,
        welcome_message: String::new(),
        reconnect_delay_initial_ms: 20,
        reconnect_delay_max_ms: 50,
        reply_max_attempts: 1, // disable retransmission noise for this test
        advert_on_connect: false,
        advert_interval_secs: 0,
        ..MeshConfig::default()
    };
    let transport = MeshTransport::init(config, host).await.unwrap();
    transport.start().await.unwrap();
    let (stream, _) = listener.accept().await.unwrap();
    let mut bridge = Bridge { stream };
    bridge.complete_handshake("Node").await;

    let sender = [0x77u8; 6];

    // First contact from a brand-new node: interactive register.
    bridge
        .send(&contact_msg_frame(sender, "register alice2"))
        .await;
    let prompt = tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
        .await
        .expect("register reply not sent");
    let prompt_text = String::from_utf8_lossy(&prompt[13..]).to_string();
    assert!(
        prompt_text.to_lowercase().contains("password"),
        "expected a password prompt, got: {prompt_text}"
    );

    // Reply with the password — must advance to "Confirm", NOT fall back to the
    // anonymous banner.
    bridge.send(&contact_msg_frame(sender, "secretpw1")).await;
    let reply = tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
        .await
        .expect("password-reply response not sent");
    let reply_text = String::from_utf8_lossy(&reply[13..]).to_string();
    assert!(
        !reply_text.contains("omit password to be prompted"),
        "BUG: first password reply was dropped to the anonymous banner: {reply_text}"
    );
    assert!(
        reply_text.to_lowercase().contains("confirm"),
        "expected a confirm-password prompt, got: {reply_text}"
    );

    transport.stop().await.unwrap();
}

/// QA noted the MeshCore client re-sends messages (flaky automation). Reproduce
/// the interactive register flow where the triggering command arrives TWICE
/// before the password, exercising the dedup / awaiting / auth_escape path on a
/// real host — the first password reply must still reach the Confirm step.
#[tokio::test]
async fn real_host_register_double_send_then_password() {
    let dbfile = tempfile::NamedTempFile::new().unwrap();
    let db = bbs_core::Database::open(&dbfile.path().to_string_lossy())
        .await
        .unwrap();
    let host: Arc<dyn bbs_plugin_api::Host> = Arc::new(bbs_core::BbsHost::new(db));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let config = MeshConfig {
        addr,
        command_prefix: None,
        welcome_message: String::new(),
        reconnect_delay_initial_ms: 20,
        reconnect_delay_max_ms: 50,
        reply_max_attempts: 1,
        advert_on_connect: false,
        advert_interval_secs: 0,
        ..MeshConfig::default()
    };
    let transport = MeshTransport::init(config, host).await.unwrap();
    transport.start().await.unwrap();
    let (stream, _) = listener.accept().await.unwrap();
    let mut bridge = Bridge { stream };
    bridge.complete_handshake("Node").await;

    let sender = [0x88u8; 6];

    // The flaky client RE-SENDS the same REGISTER message: a true retransmit
    // reuses the sender's timestamp, so both copies carry the same one.
    let register_ts = 1_700_000_500u32;
    bridge
        .send(&contact_msg_frame_ts(sender, "register dupe", register_ts))
        .await;
    let _ = tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
        .await
        .expect("first register reply");
    bridge
        .send(&contact_msg_frame_ts(sender, "register dupe", register_ts))
        .await;
    // Second copy may or may not produce a frame depending on dedup; drain with a
    // short timeout so we don't block.
    let _ = tokio::time::timeout(Duration::from_millis(300), bridge.read_text_send()).await;

    // Now the password.
    bridge.send(&contact_msg_frame(sender, "secretpw1")).await;
    let reply = tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
        .await
        .expect("password-reply response");
    let reply_text = String::from_utf8_lossy(&reply[13..]).to_string();
    assert!(
        !reply_text.contains("omit password to be prompted"),
        "BUG: password reply dropped to the anonymous banner after double-send: {reply_text}"
    );
    assert!(
        reply_text.to_lowercase().contains("confirm"),
        "expected confirm-password prompt, got: {reply_text}"
    );

    transport.stop().await.unwrap();
}

/// Regression for the QA "logout-then-register" bug. A node logs out with `Q`,
/// which ends its BBS session, but the transport still held the prefix →
/// (now-dead) session mapping. The next `register` landed on the dead session,
/// where `handle_register` fabricated a "choose a password" prompt WITHOUT
/// storing the workflow (the session write silently no-opped on the missing
/// session). The first password reply then hit `UnknownSession`, was reparsed
/// with `awaiting_reply = false`, and fell through to the anonymous banner —
/// exactly the over-the-air symptom QA reported. `handle_register` now returns
/// `UnknownSession` on a missing session (mirroring `handle_login`, which is why
/// interactive *login* never reproduced this), so the transport mints a fresh
/// session, replays `REGISTER`, and the register→confirm flow survives the
/// logout.
#[tokio::test]
async fn real_host_logout_then_register_prompt_reply_advances() {
    let dbfile = tempfile::NamedTempFile::new().unwrap();
    let db = bbs_core::Database::open(&dbfile.path().to_string_lossy())
        .await
        .unwrap();
    let host: Arc<dyn bbs_plugin_api::Host> = Arc::new(bbs_core::BbsHost::new(db));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let config = MeshConfig {
        addr,
        command_prefix: None,
        welcome_message: String::new(),
        reconnect_delay_initial_ms: 20,
        reconnect_delay_max_ms: 50,
        reply_max_attempts: 1,
        advert_on_connect: false,
        advert_interval_secs: 0,
        ..MeshConfig::default()
    };
    let transport = MeshTransport::init(config, host).await.unwrap();
    transport.start().await.unwrap();
    let (stream, _) = listener.accept().await.unwrap();
    let mut bridge = Bridge { stream };
    bridge.complete_handshake("Node").await;

    let sender = [0x99u8; 6];

    // First contact establishes a session; `Q` immediately ends it. The host
    // drops the session while the transport keeps the prefix → session mapping.
    bridge.send(&contact_msg_frame(sender, "Q")).await;
    let bye = tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
        .await
        .expect("logout reply not sent");
    let bye_text = String::from_utf8_lossy(&bye[13..]).to_string();
    assert!(
        bye_text.to_lowercase().contains("ended") || bye_text.to_lowercase().contains("goodbye"),
        "expected a logout acknowledgement, got: {bye_text}"
    );

    // Register a new account on the SAME node after logout.
    bridge
        .send(&contact_msg_frame(sender, "register postlogout"))
        .await;
    let prompt = tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
        .await
        .expect("register reply not sent");
    let prompt_text = String::from_utf8_lossy(&prompt[13..]).to_string();
    assert!(
        prompt_text.to_lowercase().contains("password"),
        "expected a password prompt after logout-then-register, got: {prompt_text}"
    );

    // The first password reply must advance to "Confirm", NOT the anonymous banner.
    bridge.send(&contact_msg_frame(sender, "secretpw1")).await;
    let reply = tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
        .await
        .expect("password-reply response not sent");
    let reply_text = String::from_utf8_lossy(&reply[13..]).to_string();
    assert!(
        !reply_text.contains("omit password to be prompted"),
        "BUG: first password reply after logout dropped to the anonymous banner: {reply_text}"
    );
    assert!(
        reply_text.to_lowercase().contains("confirm"),
        "expected a confirm-password prompt, got: {reply_text}"
    );

    transport.stop().await.unwrap();
}

/// End-to-end regression for the one-shot session-squash bug: after a node logs
/// out (`Q`) the transport keeps its prefix → session mapping, so the next
/// one-shot `LOGIN <user> <pw>` lands on the dead session. The host must surface
/// `UnknownSession` so the transport refreshes to a live session — NOT report
/// `LoggedIn` while writing nothing, which left every subsequent command failing
/// with "Unknown session" / the anonymous banner.
#[tokio::test]
async fn real_host_oneshot_login_after_logout_yields_live_session() {
    let dbfile = tempfile::NamedTempFile::new().unwrap();
    let db = bbs_core::Database::open(&dbfile.path().to_string_lossy())
        .await
        .unwrap();
    let host: Arc<dyn bbs_plugin_api::Host> = Arc::new(bbs_core::BbsHost::new(db));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let config = MeshConfig {
        addr,
        command_prefix: None,
        welcome_message: String::new(),
        reconnect_delay_initial_ms: 20,
        reconnect_delay_max_ms: 50,
        reply_max_attempts: 1,
        advert_on_connect: false,
        advert_interval_secs: 0,
        ..MeshConfig::default()
    };
    let transport = MeshTransport::init(config, host).await.unwrap();
    transport.start().await.unwrap();
    let (stream, _) = listener.accept().await.unwrap();
    let mut bridge = Bridge { stream };
    bridge.complete_handshake("Node").await;

    let sender = [0x9Au8; 6];

    // Read the next text-send frame body, or None on a short timeout. The host
    // emits async sysop notifications (e.g. "New registration: …") on the same
    // bridge, so callers skip frames by content rather than by position.
    async fn next_text(bridge: &mut Bridge) -> Option<String> {
        tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
            .await
            .ok()
            .map(|f| String::from_utf8_lossy(&f[13..]).to_string())
    }
    // Drain frames until one contains `want` (case-insensitive); panics on timeout.
    async fn expect_text_containing(bridge: &mut Bridge, want: &str) -> String {
        loop {
            let t = next_text(bridge)
                .await
                .unwrap_or_else(|| panic!("expected a frame containing {want:?}; timed out"));
            if t.to_lowercase().contains(&want.to_lowercase()) {
                return t;
            }
        }
    }

    // One-shot register creates the account and logs in (first user → Sysop).
    bridge
        .send(&contact_msg_frame(sender, "register squashed secretpw1"))
        .await;
    expect_text_containing(&mut bridge, "welcome").await;

    // Log out — host drops the session; transport keeps the prefix mapping.
    bridge.send(&contact_msg_frame(sender, "Q")).await;
    expect_text_containing(&mut bridge, "ended").await;

    // One-shot login on the SAME node after logout (the squash trigger). With
    // the bug this returned LoggedIn against the dead session (welcome shown)
    // but wrote nothing; the fix surfaces UnknownSession so the transport
    // refreshes to a live session before replaying the login.
    bridge
        .send(&contact_msg_frame(sender, "login squashed secretpw1"))
        .await;
    expect_text_containing(&mut bridge, "welcome").await;

    // The decisive check: the NEXT command must hit a LIVE, authenticated
    // session. Accept whichever outcome frame arrives (skipping async
    // notifications) and assert it reports the logged-in user — NOT "Unknown
    // session" and NOT the anonymous register/login banner.
    bridge.send(&contact_msg_frame(sender, "whoami")).await;
    let mut who_text = String::new();
    for _ in 0..8 {
        let Some(t) = next_text(&mut bridge).await else {
            break;
        };
        let lower = t.to_lowercase();
        if lower.contains("logged in as")
            || lower.contains("unknown session")
            || lower.contains("not logged in")
            || lower.contains("register <user>")
        {
            who_text = t;
            break;
        }
    }
    assert!(
        who_text.contains("Logged in as squashed"),
        "BUG: session squashed after one-shot login — whoami got: {who_text:?}"
    );

    transport.stop().await.unwrap();
}

/// Regression guard for the literal "Already logged in" symptom on a real host: a
/// retransmitted `login` (same sender timestamp) must be deduped before reaching
/// the host, so it is never reprocessed and the host never replies "Already
/// logged in." Dedup runs before command parsing, so this also exercises the
/// `login`-specific path of the symptom that motivated the feature.
#[tokio::test]
async fn real_host_retransmitted_login_is_not_reprocessed() {
    let dbfile = tempfile::NamedTempFile::new().unwrap();
    let db = bbs_core::Database::open(&dbfile.path().to_string_lossy())
        .await
        .unwrap();
    let host: Arc<dyn bbs_plugin_api::Host> = Arc::new(bbs_core::BbsHost::new(db));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let config = MeshConfig {
        addr,
        command_prefix: None,
        welcome_message: String::new(),
        reconnect_delay_initial_ms: 20,
        reconnect_delay_max_ms: 50,
        reply_max_attempts: 1,
        advert_on_connect: false,
        advert_interval_secs: 0,
        ..MeshConfig::default()
    };
    let transport = MeshTransport::init(config, host).await.unwrap();
    transport.start().await.unwrap();
    let (stream, _) = listener.accept().await.unwrap();
    let mut bridge = Bridge { stream };
    bridge.complete_handshake("Node").await;

    async fn next_text(bridge: &mut Bridge) -> Option<String> {
        tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
            .await
            .ok()
            .map(|f| String::from_utf8_lossy(&f[13..]).to_string())
    }
    async fn expect_text_containing(bridge: &mut Bridge, want: &str) -> String {
        loop {
            let t = next_text(bridge)
                .await
                .unwrap_or_else(|| panic!("expected a frame containing {want:?}; timed out"));
            if t.to_lowercase().contains(&want.to_lowercase()) {
                return t;
            }
        }
    }

    let node_a = [0x9Bu8; 6];
    let node_b = [0x9Cu8; 6];

    // Node A registers the account (first user → Sysop, auto-verified).
    bridge
        .send(&contact_msg_frame(node_a, "register alice secretpw1"))
        .await;
    expect_text_containing(&mut bridge, "welcome").await;

    // Node B is a fresh node: its first message mints a live session, so the
    // one-shot login is processed directly (no UnknownSession refresh, which
    // would otherwise reset the dedup ring). Then the byte-identical
    // retransmission (same sender timestamp) must be deduped before the host.
    let login_frame = contact_msg_frame_ts(node_b, "login alice secretpw1", 1_700_060_000);
    bridge.send(&login_frame).await;
    expect_text_containing(&mut bridge, "welcome").await;
    bridge.send(&login_frame).await; // retransmission — must be deduped

    // Drain any frames produced after the retransmission; none may be the
    // reprocessing error.
    while let Some(t) = next_text(&mut bridge).await {
        assert!(
            !t.to_lowercase().contains("already logged in"),
            "retransmitted login was reprocessed by the host: {t:?}"
        );
    }

    transport.stop().await.unwrap();
}
