//! Deliverability repro harness.
//!
//! Stands up a fake pyMC-style companion bridge on TCP loopback, runs the real
//! MeshCore [`MeshTransport`] against it, feeds crafted frames, and prints the
//! inbound delivery counters after each scenario — so you can watch them move
//! without a radio or a real user.
//!
//! ```text
//! cargo run -p bbs-mesh --example deliverability_repro
//! ```
//!
//! It exercises the three inbound signatures this instrumentation surfaces:
//!   1. a normal DM → `inbound_received`.
//!   2. a same-timestamp resend → `dedup_dropped_timestamp` (a coarse-clock bridge like pyMC collides two same-second sends).
//!   3. a reconnect backlog → fresh processed, clearly-stale → `reconnect_discarded`.
//!
//! This is a diagnostic/demo harness, not a test — it prints, it doesn't assert.
//! The equivalent assertions live in `tests/transport.rs`.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bbs_mesh::{MeshConfig, MeshTransport};
use bbs_plugin_api::{plugin::Plugin, testing::MockHost, Response};
use meshcore_companion::constants::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

// ── companion-frame builders (mirror crates/bbs-mesh/tests/transport.rs) ────────

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

fn contact_msg_frame_ts(sender_prefix: [u8; 6], text: &str, timestamp: u32) -> Vec<u8> {
    let mut payload = vec![RESP_CODE_CONTACT_MSG_RECV];
    payload.extend_from_slice(&sender_prefix);
    payload.push(0u8); // path_len
    payload.push(TXT_TYPE_PLAIN);
    payload.extend_from_slice(&timestamp.to_le_bytes());
    payload.extend_from_slice(text.as_bytes());
    radio_frame(&payload)
}

/// The fake bridge: the TCP peer the transport connects to.
struct Bridge {
    stream: TcpStream,
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

    /// Read one outbound frame payload (strips the 3-byte wire header).
    async fn read_command(&mut self) -> Vec<u8> {
        let header = self.recv_n(3).await;
        let len = u16::from_le_bytes([header[1], header[2]]) as usize;
        self.recv_n(len).await
    }

    /// Read outbound frames until a `CMD_SYNC_NEXT_MESSAGE` appears, skipping
    /// replies, path resets, and connect-time bookkeeping.
    async fn read_until_sync(&mut self) {
        loop {
            if self.read_command().await[0] == CMD_SYNC_NEXT_MESSAGE {
                return;
            }
        }
    }

    /// The normal on-connect handshake: AppStart → SelfInfo, answer the drain
    /// with NoMoreMessages, then GET_CONTACTS / GET_AUTOADD_CONFIG.
    async fn complete_handshake(&mut self, name: &str) {
        let _app_start = self.recv_n(11).await;
        self.send(&self_info_frame(name)).await;
        self.read_until_sync().await;
        self.send(&radio_frame(&[RESP_CODE_NO_MORE_MESSAGES])).await;
        // GET_CONTACTS, then GET_AUTOADD_CONFIG (reply: already enabled).
        let _get_contacts = self.read_command().await;
        let _get_autoadd = self.read_command().await;
        self.send(&radio_frame(&[RESP_CODE_AUTOADD_CONFIG, 1]))
            .await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn now_secs() -> u32 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs() as u32
}

fn print_counters(label: &str, transport: &MeshTransport) {
    let s = transport.delivery_stats().snapshot();
    println!(
        "    {label:<26} inbound_received={}  dedup_dropped_timestamp={}  dedup_dropped_text={}  reconnect_discarded={}  sends_total={}",
        s.inbound_received,
        s.dedup_dropped_timestamp,
        s.dedup_dropped_text,
        s.reconnect_discarded,
        s.sends_total,
    );
}

#[tokio::main]
async fn main() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("ok".to_owned()));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let config = MeshConfig {
        addr,
        welcome_message: String::new(),
        reconnect_delay_initial_ms: 20,
        reconnect_delay_max_ms: 50,
        ..MeshConfig::default()
    };
    let transport = MeshTransport::init(config, host).await.unwrap();
    transport.start().await.unwrap();

    let (stream, _) = listener.accept().await.unwrap();
    let mut bridge = Bridge { stream };
    bridge.complete_handshake("ReproBridge").await;

    println!("\n=== Deliverability repro — watch the inbound counters move ===\n");
    print_counters("baseline", &transport);

    let user = [0x42u8; 6];

    // ── Scenario 1: a normal DM ────────────────────────────────────────────────
    println!("\n[1] normal DM  →  expect inbound_received +1");
    bridge
        .send(&contact_msg_frame_ts(user, "hello", 1_700_000_100))
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    print_counters("after normal DM", &transport);

    // ── Scenario 2: a resend reusing the SAME timestamp ─────────────────────────
    // This is the coarse-clock collision: a whole-second bridge (pyMC) stamps two
    // sends in the same second identically, so the second collides on the
    // (timestamp, text) key and is dropped. Note `inbound_received` counts
    // *pre-dedup*, so it also increments — net-processed is
    // `inbound_received - dedup_dropped_*`, and `dedup_dropped / inbound_received`
    // is the drop ratio the OPERATIONS.md triage table reads.
    println!("\n[2] resend with the SAME timestamp  →  expect dedup_dropped_timestamp +1 (and inbound_received +1, counted pre-dedup)");
    bridge
        .send(&contact_msg_frame_ts(user, "hello", 1_700_000_100))
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    print_counters("after same-ts resend", &transport);

    // ── Scenario 3: a reconnect backlog (fresh processed, stale discarded) ──────
    println!("\n[3] reconnect with a FRESH and a STALE queued message  →  expect inbound_received +1, reconnect_discarded +1");
    drop(bridge); // close the link → the transport reconnects
    let (stream2, _) = listener.accept().await.unwrap();
    let mut bridge = Bridge { stream: stream2 };

    // Manual drain handshake so we can inject the backlog during the drain.
    let _app_start = bridge.recv_n(11).await;
    bridge.send(&self_info_frame("ReproBridge")).await;
    let now = now_secs();

    bridge.read_until_sync().await;
    bridge
        .send(&contact_msg_frame_ts(user, "reconnect-fresh", now))
        .await;

    bridge.read_until_sync().await;
    bridge
        .send(&contact_msg_frame_ts(
            user,
            "reconnect-stale",
            now.saturating_sub(3600),
        ))
        .await;

    bridge.read_until_sync().await;
    bridge
        .send(&radio_frame(&[RESP_CODE_NO_MORE_MESSAGES]))
        .await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    print_counters("after reconnect drain", &transport);

    println!("\n=== done ===\n");
    let _ = transport.stop().await;
}
