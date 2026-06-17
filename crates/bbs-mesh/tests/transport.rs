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

use std::{sync::Arc, time::Duration};

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

fn contact_msg_frame(sender_prefix: [u8; 6], text: &str) -> Vec<u8> {
    let mut payload = vec![RESP_CODE_CONTACT_MSG_RECV];
    payload.extend_from_slice(&sender_prefix);
    payload.push(0u8); // path_len
    payload.push(TXT_TYPE_PLAIN);
    payload.extend_from_slice(&1_700_000_000u32.to_le_bytes());
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
}

/// Spin up a [`MeshTransport`] against an in-process loopback listener.
///
/// `host` is an `Arc<MockHost>` so the caller can keep a clone for inspection.
async fn make_transport(host: Arc<MockHost>, prefix: Option<char>) -> (MeshTransport, Bridge) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let config = MeshConfig {
        addr,
        command_prefix: prefix,
        welcome_message: String::new(), // suppressed in tests
        reconnect_delay_initial_ms: 20,
        reconnect_delay_max_ms: 50,
        ..MeshConfig::default()
    };

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
    let cmd_payload = tokio::time::timeout(Duration::from_secs(2), bridge.read_command())
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
    let _ = tokio::time::timeout(Duration::from_secs(2), bridge.read_command())
        .await
        .expect("timed out waiting for whoami reply");

    let session = host.commands_received()[0].0;
    let outcome = transport
        .notify(session, Notification::Text("You have mail!".to_owned()))
        .await
        .unwrap();
    assert!(matches!(outcome, NotifyOutcome::Queued));

    let cmd_payload = tokio::time::timeout(Duration::from_secs(2), bridge.read_command())
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

    // The transport should send back a reply (from MockHost).
    let cmd_payload = tokio::time::timeout(Duration::from_secs(2), bridge.read_command())
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

    let cmd_payload = tokio::time::timeout(Duration::from_secs(2), bridge.read_command())
        .await
        .expect("timed out waiting for reply");
    assert_eq!(cmd_payload[0], CMD_SEND_TXT_MSG);

    transport.stop().await.unwrap();
}

/// A reply that is accepted by the device (RESP_CODE_SENT with a CRC) but never
/// confirmed (no PUSH_CODE_SEND_CONFIRMED) is retransmitted after the ack-wait
/// floor — this is the core reply-reliability fix for the lossy return path.
#[tokio::test]
async fn unacked_reply_is_retransmitted() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("hi there".to_owned()));

    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;
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

    let (transport, mut bridge) = make_transport(Arc::clone(&host), None).await;
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
        ..MeshConfig::default()
    };
    let transport = MeshTransport::init(config, host).await.unwrap();
    transport.start().await.unwrap();
    let (stream, _) = listener.accept().await.unwrap();
    let mut bridge = Bridge { stream };
    bridge.complete_handshake("Node").await;

    let sender = [0x88u8; 6];

    // The flaky client sends REGISTER twice in quick succession.
    bridge
        .send(&contact_msg_frame(sender, "register dupe"))
        .await;
    let _ = tokio::time::timeout(Duration::from_secs(2), bridge.read_text_send())
        .await
        .expect("first register reply");
    bridge
        .send(&contact_msg_frame(sender, "register dupe"))
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
