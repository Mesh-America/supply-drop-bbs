//! Mesh advertisement bus.
//!
//! Provides [`AdvertBus`] — a shared, in-memory store for mesh node
//! advertisements — and [`AdvertRecord`], the shape of each entry.
//!
//! ## Data flow
//!
//! ```text
//!  MeshTransport ──upsert()──► AdvertBus ◄──list()── WebPlugin (API)
//!  WebPlugin ──request_send()──► AdvertBus ──subscribe_send()──► MeshTransport
//! ```
//!
//! `BbsHost` owns the single `Arc<AdvertBus>` instance and hands it
//! out via [`Host::advert_bus`](crate::Host::advert_bus).

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

// ── AdvertRecord ──────────────────────────────────────────────────────────────

/// A single mesh node advertisement, captured from the air.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdvertRecord {
    /// Full 32-byte public key, hex-encoded.
    pub pubkey_hex: String,
    /// Human-readable node name. Empty if only a short (pubkey-only) advert
    /// has been received so far.
    pub name: String,
    /// MeshCore advertisement type byte. `0` = unknown / short-advert only.
    pub adv_type: u8,
    /// GPS latitude in decimal degrees (`0.0` if not reported).
    pub lat: f64,
    /// GPS longitude in decimal degrees (`0.0` if not reported).
    pub lon: f64,
    /// Unix timestamp (seconds) when this node was first observed.
    pub first_seen_secs: i64,
    /// Unix timestamp (seconds) when this node was most recently observed.
    pub last_seen_secs: i64,
}

// ── AdvertBus ─────────────────────────────────────────────────────────────────

/// Shared bus: stores received adverts and routes send-advert requests.
///
/// `BbsHost` creates one `AdvertBus` at startup and returns an `Arc` to it
/// via [`Host::advert_bus`](crate::Host::advert_bus).
///
/// - `MeshTransport` calls [`upsert`](AdvertBus::upsert) /
///   [`upsert_short`](AdvertBus::upsert_short) when adverts arrive and
///   subscribes to [`subscribe_send`](AdvertBus::subscribe_send).
/// - `WebPlugin` calls [`list`](AdvertBus::list) to serve the API and
///   [`request_send`](AdvertBus::request_send) when the sysop hits the
///   "send advert" button.
pub struct AdvertBus {
    records: Mutex<HashMap<[u8; 32], AdvertRecord>>,
    send_tx: broadcast::Sender<bool>,
}

impl Default for AdvertBus {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for AdvertBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdvertBus")
            .field("record_count", &self.records.lock().map_or(0, |r| r.len()))
            .finish_non_exhaustive()
    }
}

impl AdvertBus {
    /// Create a new, empty bus.
    pub fn new() -> Self {
        let (send_tx, _) = broadcast::channel(8);
        Self {
            records: Mutex::new(HashMap::new()),
            send_tx,
        }
    }

    /// Insert or update a full advertisement from a named contact.
    ///
    /// Updates all fields for an existing record; preserves `first_seen_secs`.
    pub fn upsert(&self, pubkey: [u8; 32], name: String, adv_type: u8, gps_lat: i32, gps_lon: i32) {
        let now = unix_now();
        let lat = gps_lat as f64 / 1_000_000.0;
        let lon = gps_lon as f64 / 1_000_000.0;
        let mut records = self.records.lock().expect("advert bus poisoned");
        let entry = records.entry(pubkey).or_insert_with(|| AdvertRecord {
            pubkey_hex: hex_encode(&pubkey),
            name: String::new(),
            adv_type: 0,
            lat: 0.0,
            lon: 0.0,
            first_seen_secs: now,
            last_seen_secs: now,
        });
        entry.name = name;
        entry.adv_type = adv_type;
        entry.lat = lat;
        entry.lon = lon;
        entry.last_seen_secs = now;
    }

    /// Insert or update a short advertisement (pubkey only).
    ///
    /// Updates `last_seen_secs` on an existing record without overwriting
    /// name, type, or location. Creates a minimal stub if unseen.
    pub fn upsert_short(&self, pubkey: [u8; 32]) {
        let now = unix_now();
        let mut records = self.records.lock().expect("advert bus poisoned");
        records
            .entry(pubkey)
            .and_modify(|e| e.last_seen_secs = now)
            .or_insert_with(|| AdvertRecord {
                pubkey_hex: hex_encode(&pubkey),
                name: String::new(),
                adv_type: 0,
                lat: 0.0,
                lon: 0.0,
                first_seen_secs: now,
                last_seen_secs: now,
            });
    }

    /// Return all records sorted by `last_seen_secs` descending (newest first).
    pub fn list(&self) -> Vec<AdvertRecord> {
        let records = self.records.lock().expect("advert bus poisoned");
        let mut v: Vec<_> = records.values().cloned().collect();
        v.sort_by(|a, b| b.last_seen_secs.cmp(&a.last_seen_secs));
        v
    }

    /// Subscribe to send-advert requests from the web UI.
    ///
    /// Delivers `true` for flood mode, `false` for direct-only.
    /// `MeshTransport` subscribes during `start()` and forwards each
    /// request to the companion bridge as `OutboundFrame::SendSelfAdvert`.
    pub fn subscribe_send(&self) -> broadcast::Receiver<bool> {
        self.send_tx.subscribe()
    }

    /// Request that listening transports broadcast our self-advertisement.
    ///
    /// Returns `true` if at least one listener picked up the request,
    /// `false` if no mesh transport is currently subscribed.
    pub fn request_send(&self, flood: bool) -> bool {
        self.send_tx.send(flood).is_ok()
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn hex_encode(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut acc, b| {
        let _ = write!(acc, "{b:02x}");
        acc
    })
}
