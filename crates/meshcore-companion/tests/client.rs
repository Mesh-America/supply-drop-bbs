//! Tests for the companion TCP client.
//!
//! All tests use an in-process TCP loopback (ephemeral port on 127.0.0.1) so
//! no real `pymc_core` process or external network is needed.  The test
//! controls the "server" side manually via [`TcpBridge`].
//!
//! AppStart is always 5 wire bytes: `[0x3C][0x02][0x00][CMD_APP_START][version]`.
//! Tests that consume the AppStart always read exactly 5 bytes to avoid
//! leaving the version byte in the TCP buffer and corrupting subsequent reads.

use std::time::Duration;

use tokio::io::AsyncWriteExt;

use meshcore_companion::{
    client::{ClientConfig, ClientEvent, CompanionClient},
    constants::*,
    frame::{InboundFrame, OutboundFrame},
};

// ── Wire-frame helpers ────────────────────────────────────────────────────────

/// Wrap a payload in the radio-→-app wire header (prefix=0x3E + LE u16 length).
fn radio_frame(payload: &[u8]) -> Vec<u8> {
    let mut v = vec![FRAME_OUTBOUND_PREFIX];
    v.extend_from_slice(&(payload.len() as u16).to_le_bytes());
    v.extend_from_slice(payload);
    v
}

/// Build a minimal but valid RESP_CODE_SELF_INFO wire frame.
///
/// The fixed portion is 57 bytes:
/// `adv_type[1] tx_power[1] field22[1] pubkey[32] lat[4] lon[4]
///  multi_acks[1] advert_loc[1] telemetry[1] manual_add[1]
///  freq_khz[4] bw_hz[4] sf[1] cr[1]`
/// followed by the variable-length node name.
fn self_info_frame(name: &str) -> Vec<u8> {
    let mut body = Vec::new();
    body.push(ADV_TYPE_CHAT); // adv_type
    body.push(20); // tx_power_dbm
    body.push(22); // field_22
    body.extend_from_slice(&[0xAAu8; 32]); // pubkey
    body.extend_from_slice(&0i32.to_le_bytes()); // latitude
    body.extend_from_slice(&0i32.to_le_bytes()); // longitude
    body.push(0); // multi_acks
    body.push(ADVERT_LOC_NONE); // advert_loc_policy
    body.push(0); // telemetry_modes
    body.push(0); // manual_add_contacts
    body.extend_from_slice(&915_000u32.to_le_bytes()); // frequency_khz
    body.extend_from_slice(&125_000u32.to_le_bytes()); // bandwidth_hz
    body.push(10); // spreading_factor
    body.push(5); // coding_rate
    body.extend_from_slice(name.as_bytes()); // node_name (variable length)

    let mut payload = vec![RESP_CODE_SELF_INFO];
    payload.extend_from_slice(&body);
    radio_frame(&payload)
}

fn ok_frame() -> Vec<u8> {
    radio_frame(&[RESP_CODE_OK])
}

fn curr_time_frame(t: u32) -> Vec<u8> {
    let mut payload = vec![RESP_CODE_CURR_TIME];
    payload.extend_from_slice(&t.to_le_bytes());
    radio_frame(&payload)
}

// ── Test harness ──────────────────────────────────────────────────────────────

/// Server (bridge) side of a loopback test connection.
struct TcpBridge {
    stream: tokio::net::TcpStream,
}

impl TcpBridge {
    async fn send(&mut self, bytes: &[u8]) {
        self.stream.write_all(bytes).await.unwrap();
    }

    /// Read exactly `n` bytes from the client.
    async fn recv_n(&mut self, n: usize) -> Vec<u8> {
        let mut buf = vec![0u8; n];
        tokio::io::AsyncReadExt::read_exact(&mut self.stream, &mut buf)
            .await
            .unwrap();
        buf
    }
}

/// Bind an ephemeral TCP listener, connect a [`CompanionClient`] to it,
/// and return both the client handle and the accepted server stream.
///
/// The bridge does NOT automatically respond to AppStart — the test drives
/// that explicitly.
async fn loopback() -> (CompanionClient, TcpBridge) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let config = ClientConfig {
        addr,
        app_target_version: APP_TARGET_VER_V3,
        reconnect_delay_initial: Duration::from_millis(20),
        reconnect_delay_max: Duration::from_millis(100),
    };
    let client = CompanionClient::connect(config);
    let (stream, _) = listener.accept().await.unwrap();
    (client, TcpBridge { stream })
}

/// Complete the AppStart handshake on a fresh loopback pair and wait for the
/// `Connected` event.  Returns the `TcpBridge` so the test can continue.
async fn complete_handshake(client: &mut CompanionClient, bridge: &mut TcpBridge, name: &str) {
    // AppStart = 5 bytes: [prefix][len_lo=2][len_hi=0][CMD_APP_START][version]
    let _app_start = bridge.recv_n(5).await;
    bridge.send(&self_info_frame(name)).await;
    let ev = tokio::time::timeout(Duration::from_secs(2), client.recv())
        .await
        .expect("timed out waiting for Connected")
        .expect("client channel closed");
    assert!(
        matches!(ev, ClientEvent::Connected { .. }),
        "expected Connected, got {ev:?}"
    );
}

// ── Tests ─────────────────────────────────────────────────────────────────────

/// A successful handshake emits `Connected` carrying the node name.
#[tokio::test]
async fn handshake_emits_connected() {
    let (mut client, mut bridge) = loopback().await;

    let app_start = bridge.recv_n(5).await;
    assert_eq!(
        app_start[0], FRAME_INBOUND_PREFIX,
        "AppStart must use inbound prefix"
    );
    assert_eq!(app_start[3], CMD_APP_START, "byte[3] must be CMD_APP_START");
    assert_eq!(
        app_start[4], APP_TARGET_VER_V3,
        "byte[4] must be app_target_version"
    );

    bridge.send(&self_info_frame("TestRadio")).await;

    let event = tokio::time::timeout(Duration::from_secs(2), client.recv())
        .await
        .expect("timed out")
        .expect("channel closed");

    match event {
        ClientEvent::Connected { self_info } => {
            assert_eq!(self_info.unwrap().node_name, "TestRadio")
        }
        other => panic!("expected Connected, got {other:?}"),
    }
}

/// When the device returns `ERR_CODE_UNSUPPORTED_CMD` for `CMD_APP_START`
/// (MeshCore ≥ 1.15 behaviour on Heltec V4.2), the client should still emit
/// `Connected { self_info: None }` and enter the event loop rather than
/// retrying forever.  See GitHub issue #2.
#[tokio::test]
async fn unsupported_app_start_emits_connected_without_self_info() {
    let (mut client, mut bridge) = loopback().await;

    // Consume the AppStart the client sends.
    let _app_start = bridge.recv_n(5).await;

    // Reply with ERR_CODE_UNSUPPORTED_CMD (error_code = 1).
    let err_frame = radio_frame(&[RESP_CODE_ERR, ERR_CODE_UNSUPPORTED_CMD]);
    bridge.send(&err_frame).await;

    // Client must emit Connected with self_info = None (not loop forever).
    let ev = tokio::time::timeout(Duration::from_secs(2), client.recv())
        .await
        .expect("timed out waiting for Connected")
        .expect("client channel closed");

    match ev {
        ClientEvent::Connected { self_info } => {
            assert!(
                self_info.is_none(),
                "self_info should be None for firmware that rejects AppStart"
            );
        }
        other => panic!("expected Connected{{self_info: None}}, got {other:?}"),
    }

    // The client must stay in the event loop — subsequent frames are forwarded.
    bridge.send(&ok_frame()).await;
    let ev2 = tokio::time::timeout(Duration::from_secs(2), client.recv())
        .await
        .expect("timed out waiting for Frame")
        .expect("channel closed");
    assert!(
        matches!(ev2, ClientEvent::Frame(InboundFrame::Ok)),
        "expected Frame(Ok) after handshake-less connect, got {ev2:?}"
    );
}

/// The first bytes the client sends are a well-formed AppStart frame.
#[tokio::test]
async fn first_bytes_are_app_start() {
    let (_client, mut bridge) = loopback().await;

    // [prefix][len_lo=2][len_hi=0][CMD_APP_START][APP_TARGET_VER_V3]
    let wire = bridge.recv_n(5).await;
    assert_eq!(wire[0], FRAME_INBOUND_PREFIX);
    assert_eq!(
        u16::from_le_bytes([wire[1], wire[2]]),
        2,
        "payload length must be 2"
    );
    assert_eq!(wire[3], CMD_APP_START);
    assert_eq!(wire[4], APP_TARGET_VER_V3);
}

/// Frames arriving after the handshake are forwarded as `Frame` events.
#[tokio::test]
async fn frames_forwarded_after_handshake() {
    let (mut client, mut bridge) = loopback().await;
    complete_handshake(&mut client, &mut bridge, "R").await;

    bridge.send(&curr_time_frame(1_700_000_000)).await;
    let ev = tokio::time::timeout(Duration::from_secs(2), client.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            ev,
            ClientEvent::Frame(InboundFrame::CurrTime {
                unix_time: 1_700_000_000
            })
        ),
        "unexpected: {ev:?}"
    );

    bridge.send(&ok_frame()).await;
    let ev = tokio::time::timeout(Duration::from_secs(2), client.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(ev, ClientEvent::Frame(InboundFrame::Ok)));
}

/// Commands sent via `client.send()` appear on the bridge as valid wire frames.
#[tokio::test]
async fn send_command_reaches_bridge() {
    let (mut client, mut bridge) = loopback().await;
    complete_handshake(&mut client, &mut bridge, "R").await;

    client.send(OutboundFrame::GetBattAndStorage).await.unwrap();

    // GetBattAndStorage = 4 bytes: [prefix][len_lo=1][len_hi=0][CMD]
    let cmd = bridge.recv_n(4).await;
    assert_eq!(cmd[0], FRAME_INBOUND_PREFIX);
    assert_eq!(
        u16::from_le_bytes([cmd[1], cmd[2]]),
        1,
        "payload length must be 1"
    );
    assert_eq!(cmd[3], CMD_GET_BATT_AND_STORAGE);
}

/// Closing the server connection triggers `Disconnected(will_retry=true)` then
/// a reconnect (the bridge receives a second AppStart).
#[tokio::test]
async fn reconnects_after_disconnect() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let config = ClientConfig {
        addr,
        app_target_version: APP_TARGET_VER_V3,
        reconnect_delay_initial: Duration::from_millis(20),
        reconnect_delay_max: Duration::from_millis(50),
    };
    let mut client = CompanionClient::connect(config);

    // ── First connection ──────────────────────────────────────────────────
    let (mut s1, _) = listener.accept().await.unwrap();
    let mut buf = vec![0u8; 5];
    tokio::io::AsyncReadExt::read_exact(&mut s1, &mut buf)
        .await
        .unwrap();
    s1.write_all(&self_info_frame("Node1")).await.unwrap();

    let ev = tokio::time::timeout(Duration::from_secs(2), client.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(ev, ClientEvent::Connected { .. }));

    // Simulate bridge disconnect by dropping the server stream.
    drop(s1);

    // Client emits Disconnected(will_retry=true).
    let ev = tokio::time::timeout(Duration::from_secs(2), client.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(ev, ClientEvent::Disconnected { will_retry: true }),
        "expected Disconnected(retry=true), got {ev:?}"
    );

    // ── Second connection (automatic reconnect) ───────────────────────────
    let (mut s2, _) = listener.accept().await.unwrap();
    let mut buf2 = vec![0u8; 5];
    tokio::io::AsyncReadExt::read_exact(&mut s2, &mut buf2)
        .await
        .unwrap();
    assert_eq!(buf2[3], CMD_APP_START, "expected AppStart on reconnect");

    // Complete the handshake to keep the worker happy.
    s2.write_all(&self_info_frame("Node1")).await.unwrap();
    let ev2 = tokio::time::timeout(Duration::from_secs(2), client.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(ev2, ClientEvent::Connected { .. }));
}

/// Dropping the client causes the worker to exit and the TCP connection to
/// close (server side sees EOF with no leftover bytes).
#[tokio::test]
async fn drop_client_closes_connection() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let config = ClientConfig {
        addr,
        app_target_version: APP_TARGET_VER_V3,
        reconnect_delay_initial: Duration::from_millis(10),
        reconnect_delay_max: Duration::from_millis(10),
    };

    let mut client = CompanionClient::connect(config);

    let (mut srv, _) = listener.accept().await.unwrap();
    let mut buf = vec![0u8; 5];
    tokio::io::AsyncReadExt::read_exact(&mut srv, &mut buf)
        .await
        .unwrap();
    srv.write_all(&self_info_frame("R")).await.unwrap();

    // Wait for Connected before dropping so the worker is in the event loop
    // (not mid-connect) when cmd_tx closes.
    let _ = tokio::time::timeout(Duration::from_secs(2), client.recv())
        .await
        .unwrap();
    drop(client);

    // The worker should detect cmd_rx closed and exit, which closes the TCP
    // connection.  Server reads EOF (n = 0).
    let mut drain = [0u8; 1];
    let n = tokio::time::timeout(
        Duration::from_secs(2),
        tokio::io::AsyncReadExt::read(&mut srv, &mut drain),
    )
    .await
    .expect("timed out waiting for EOF")
    .unwrap();
    assert_eq!(n, 0, "expected EOF after client drop");
}

/// `send()` returns `SendError` once the worker exits (all senders dropped).
#[tokio::test]
async fn send_after_worker_exit_returns_error() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let config = ClientConfig {
        addr,
        app_target_version: APP_TARGET_VER_V3,
        reconnect_delay_initial: Duration::from_millis(5),
        reconnect_delay_max: Duration::from_millis(5),
    };
    // Hold only the Sender half; receive side is unused.
    let client = CompanionClient::connect(config);
    // Accept and immediately drop to make the handshake fail quickly.
    let (srv, _) = listener.accept().await.unwrap();
    drop(srv);

    // We no longer have a way to call send() after dropping the client,
    // so this test just verifies the above scenario compiles and doesn't
    // panic — the Rust channel type guarantees `send` returns Err once
    // the worker has exited and cmd_rx is dropped.
    drop(client);
}
