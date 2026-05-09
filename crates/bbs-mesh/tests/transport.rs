//! Integration tests for [`MeshTransport`].
//!
//! Uses [`MockHost`] from `bbs-plugin-api` and an in-process TCP loopback
//! (ephemeral port on 127.0.0.1) to exercise the full
//! `init → start → frame → response` path without a real `pymc_core` process.
//!
//! # Bridge protocol reminder
//!
//! The companion client sends AppStart (5 bytes) and expects SelfInfo back
//! before entering the event loop.  The test bridge must complete this
//! handshake before sending any other frames.

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

fn contact_msg_frame(sender_prefix: [u8; 6], text: &str) -> Vec<u8> {
    let mut payload = vec![RESP_CODE_CONTACT_MSG_RECV];
    payload.extend_from_slice(&sender_prefix);
    payload.push(0u8); // path_len
    payload.push(TXT_TYPE_PLAIN);
    payload.extend_from_slice(&1_700_000_000u32.to_le_bytes());
    payload.extend_from_slice(text.as_bytes());
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
        let app_start = self.recv_n(5).await;
        assert_eq!(app_start[3], CMD_APP_START, "expected CMD_APP_START");
        self.send(&self_info_frame(name)).await;
    }

    /// Read one outbound frame payload (strips the 3-byte wire header).
    async fn read_command(&mut self) -> Vec<u8> {
        let header = self.recv_n(3).await;
        assert_eq!(header[0], FRAME_INBOUND_PREFIX);
        let len = u16::from_le_bytes([header[1], header[2]]) as usize;
        self.recv_n(len).await
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
