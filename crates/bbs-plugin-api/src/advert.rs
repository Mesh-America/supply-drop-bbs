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

    /// Insert or update a full advertisement with an explicit `last_seen` timestamp.
    ///
    /// Use this when the timestamp comes from the device (e.g. the
    /// `last_advert_timestamp` field in a `RESP_CODE_CONTACT` frame) rather than
    /// the current wall clock.
    ///
    /// `device_last_seen` is validated before use: MeshCore devices without a
    /// synced RTC report seconds-since-boot (small values → near 1970 epoch)
    /// and devices with a misconfigured clock can report future values. Any
    /// value outside `[MIN_PLAUSIBLE_TS, now + CLOCK_FUDGE_SECS]` is treated
    /// as unreliable and falls back to the current wall-clock time.
    ///
    /// Updates all fields for an existing record; preserves `first_seen_secs`.
    pub fn upsert_with_timestamp(
        &self,
        pubkey: [u8; 32],
        name: String,
        adv_type: u8,
        gps_lat: i32,
        gps_lon: i32,
        device_last_seen: i64,
    ) {
        let now = unix_now();
        // Accept only plausible Unix timestamps. Devices without a synced RTC
        // report seconds-since-boot which are tiny (→ dates near 1970); devices
        // with a misconfigured clock can exceed the current time by years.
        // 2020-01-01 UTC is a safe floor — no BBS contact predates MeshCore.
        // 5-minute ceiling fudge tolerates minor clock skew between devices.
        const MIN_PLAUSIBLE_TS: i64 = 1_577_836_800; // 2020-01-01 00:00:00 UTC
        const CLOCK_FUDGE_SECS: i64 = 300; // 5 minutes
        let last_seen =
            if device_last_seen >= MIN_PLAUSIBLE_TS && device_last_seen <= now + CLOCK_FUDGE_SECS {
                device_last_seen
            } else {
                now
            };
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
            last_seen_secs: last_seen,
        });
        entry.name = name;
        entry.adv_type = adv_type;
        entry.lat = lat;
        entry.lon = lon;
        // Only advance last_seen — never move it backwards. A live advert
        // arriving later will always have a wall-clock time ≥ the stored value.
        if last_seen > entry.last_seen_secs {
            entry.last_seen_secs = last_seen;
        }
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

    /// Return the full 32-byte public key of the least-recently-seen contact
    /// whose key prefix (first 6 bytes) does not appear in `exclude_prefixes`.
    ///
    /// Used by the mesh transport to pick a stale contact for eviction when
    /// the radio's contact table is full ([`ContactsFull`] push): the caller
    /// can then send `RemoveContact` for the returned key to free a table slot.
    ///
    /// Returns `None` if the bus is empty or all contacts match an excluded
    /// prefix (e.g. all known contacts have active BBS sessions).
    pub fn stalest_pubkey_excluding(&self, exclude_prefixes: &[[u8; 6]]) -> Option<[u8; 32]> {
        let records = self.records.lock().expect("advert bus poisoned");
        records
            .iter()
            .filter(|(pubkey, _)| {
                let prefix: [u8; 6] = pubkey[..6].try_into().expect("pubkey is 32 bytes");
                !exclude_prefixes.contains(&prefix)
            })
            .min_by_key(|(_, rec)| rec.last_seen_secs)
            .map(|(pubkey, _)| *pubkey)
    }

    /// Look up the human-readable node name for a given 6-byte key prefix.
    ///
    /// Returns `None` if the prefix is not in the bus or if its name field is
    /// empty (i.e. only a short advert has been received so far).
    pub fn name_by_prefix(&self, prefix: &[u8; 6]) -> Option<String> {
        let records = self.records.lock().expect("advert bus poisoned");
        records
            .iter()
            .find(|(pubkey, _)| pubkey[..6] == *prefix)
            .and_then(|(_, r)| {
                if r.name.is_empty() {
                    None
                } else {
                    Some(r.name.clone())
                }
            })
    }

    /// Remove all records from the bus.
    ///
    /// Useful when a sysop wants to flush stale data without restarting the
    /// BBS (e.g. after correcting device clocks or clearing old contacts).
    pub fn clear(&self) {
        self.records.lock().expect("advert bus poisoned").clear();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_key(byte: u8) -> [u8; 32] {
        [byte; 32]
    }

    fn now_secs() -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs() as i64)
    }

    /// A device timestamp of 0 (unset) falls back to wall-clock time.
    #[test]
    fn zero_device_ts_falls_back_to_now() {
        let bus = AdvertBus::new();
        bus.upsert_with_timestamp(dummy_key(1), "A".into(), 1, 0, 0, 0);
        let records = bus.list();
        let ts = records[0].last_seen_secs;
        let now = now_secs();
        assert!(
            ts >= now - 2 && ts <= now + 2,
            "ts {ts} should be near now {now}"
        );
    }

    /// A boot-relative timestamp (small value — seconds since reboot, near 1970)
    /// must be rejected and replaced with the current wall-clock time.
    #[test]
    fn boot_relative_ts_is_rejected() {
        let bus = AdvertBus::new();
        let boot_relative: i64 = 3600; // 1 hour after epoch — clearly 1970
        bus.upsert_with_timestamp(dummy_key(2), "B".into(), 1, 0, 0, boot_relative);
        let ts = bus.list()[0].last_seen_secs;
        let now = now_secs();
        assert!(
            ts >= now - 2 && ts <= now + 2,
            "boot-relative ts {boot_relative} should have been replaced with now ({now}), got {ts}"
        );
    }

    /// A far-future timestamp (device with misconfigured clock) is rejected.
    #[test]
    fn far_future_ts_is_rejected() {
        let bus = AdvertBus::new();
        let far_future: i64 = 2_000_000_000; // ~year 2033 — plausible false positive guard
                                             // Use a value well past now+fudge to ensure rejection.
        let very_far_future: i64 = 4_000_000_000; // ~year 2096
        bus.upsert_with_timestamp(dummy_key(3), "C".into(), 1, 0, 0, very_far_future);
        let ts = bus.list()[0].last_seen_secs;
        let now = now_secs();
        assert!(
            ts >= now - 2 && ts <= now + 2,
            "far-future ts {very_far_future} should have been replaced with now ({now}), got {ts}"
        );
        let _ = far_future; // suppress unused warning
    }

    /// A plausible Unix timestamp (recent past) is accepted as-is.
    #[test]
    fn plausible_ts_is_accepted() {
        let bus = AdvertBus::new();
        let plausible: i64 = 1_700_000_000; // Nov 2023 — clearly reasonable
        bus.upsert_with_timestamp(dummy_key(4), "D".into(), 1, 0, 0, plausible);
        let ts = bus.list()[0].last_seen_secs;
        assert_eq!(ts, plausible, "plausible ts should be stored unchanged");
    }

    /// `clear()` empties the bus.
    #[test]
    fn clear_empties_bus() {
        let bus = AdvertBus::new();
        bus.upsert(dummy_key(5), "E".into(), 1, 0, 0);
        assert_eq!(bus.list().len(), 1);
        bus.clear();
        assert_eq!(bus.list().len(), 0);
    }

    /// `stalest_pubkey_excluding` returns the oldest contact not in the exclusion list.
    #[test]
    fn stalest_pubkey_excluding_returns_oldest_non_excluded() {
        let bus = AdvertBus::new();
        let old_key = dummy_key(10);
        let new_key = dummy_key(20);
        let excluded_key = dummy_key(30);

        // old_key was last seen in Nov 2023.
        bus.upsert_with_timestamp(old_key, "OldNode".into(), 1, 0, 0, 1_700_000_000);
        // new_key was last seen in Jan 2025.
        bus.upsert_with_timestamp(new_key, "NewNode".into(), 1, 0, 0, 1_735_689_600);
        // excluded_key is even older but excluded.
        bus.upsert_with_timestamp(excluded_key, "ExcludedNode".into(), 1, 0, 0, 1_500_000_000);

        let excluded_prefix: [u8; 6] = excluded_key[..6].try_into().unwrap();
        let result = bus.stalest_pubkey_excluding(&[excluded_prefix]);

        assert_eq!(
            result,
            Some(old_key),
            "should return old_key — oldest non-excluded"
        );
    }

    /// When all contacts are excluded, `stalest_pubkey_excluding` returns `None`.
    #[test]
    fn stalest_pubkey_excluding_all_excluded_returns_none() {
        let bus = AdvertBus::new();
        let key = dummy_key(11);
        bus.upsert(key, "A".into(), 1, 0, 0);
        let prefix: [u8; 6] = key[..6].try_into().unwrap();
        assert_eq!(bus.stalest_pubkey_excluding(&[prefix]), None);
    }

    /// An empty bus returns `None`.
    #[test]
    fn stalest_pubkey_excluding_empty_bus_returns_none() {
        let bus = AdvertBus::new();
        assert_eq!(bus.stalest_pubkey_excluding(&[]), None);
    }
}
