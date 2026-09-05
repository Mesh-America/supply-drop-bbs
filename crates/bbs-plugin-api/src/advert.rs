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
//!
//! ## Contact protection ("Persist Mesh Contacts" feature)
//!
//! A record can additionally be marked *protected* (MeshCore: "favourite";
//! Meshtastic: "favorite") once its owner has DMed the BBS — see
//! `specs/001-persist-mesh-contacts/` for the full design. Protection state
//! lives entirely on [`AdvertRecord`] (no new persistent storage); it is
//! rehydrated from each platform's own wire data on reconnect rather than
//! surviving a BBS restart on its own. [`AdvertBus::mark_favourite_if_eligible`]
//! and [`AdvertBus::mark_favourite_if_eligible_by_node_num`] are the sole
//! entry points that transition a record to protected — both atomically
//! enforce a per-transport cap, evicting the oldest-protected, currently
//! session-inactive contact to make room for a newly-eligible one once the
//! cap is reached (`specs/001-persist-mesh-contacts/research.md` Decision 5b).
//!
//! This module is the foundational storage layer for that feature.
//! `crates/bbs-mesh` wires it into MeshCore's DM-dispatch path (see
//! `try_protect_contact` in `crates/bbs-mesh/src/transport.rs`); Meshtastic's
//! equivalent wiring lands in a later phase.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

/// How long after a local protect/unprotect transition the transition itself
/// is trusted over conflicting incoming wire data or eviction-selection
/// criteria — long enough to comfortably cover LoRa's slow airtime and any
/// resync race, short enough that a genuine later external change (via the
/// platform's own tools) still eventually takes effect.
///
/// See `specs/001-persist-mesh-contacts/research.md` Decision 7a/12a.
pub const PROTECT_GRACE_SECS: u64 = 300;

// ── AdvertRecord ──────────────────────────────────────────────────────────────

/// A single mesh node advertisement, captured from the air.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AdvertRecord {
    /// Full 32-byte public key, hex-encoded.
    pub pubkey_hex: String,
    /// Human-readable node name. Empty if only a short (pubkey-only) advert
    /// has been received so far.
    pub name: String,
    /// Node/eligibility type byte. On MeshCore this is the wire `adv_type`
    /// byte (chat/person vs. repeater/room-server). On Meshtastic,
    /// `record_node_advert` stores the node's `User.role` value here instead
    /// — the web interprets this per-transport. `0` = unknown / short-advert
    /// only.
    pub adv_type: u8,
    /// GPS latitude in decimal degrees (`0.0` if not reported).
    pub lat: f64,
    /// GPS longitude in decimal degrees (`0.0` if not reported).
    pub lon: f64,
    /// Unix timestamp (seconds) when this node was first observed.
    pub first_seen_secs: i64,
    /// Unix timestamp (seconds) when this node was most recently observed.
    pub last_seen_secs: i64,
    /// Name of the transport this advert was heard on (e.g. `"meshcore"`,
    /// `"meshtastic"`). Identifies which radio network the node belongs to.
    pub transport: String,

    /// `true` once a frame carrying real per-contact detail (not just a bare
    /// pubkey/short advert) has been seen. This is the transport-agnostic
    /// gate [`AdvertBus::mark_favourite_if_eligible`] checks before ever
    /// protecting a record — a record can't be protected from a short advert
    /// alone.
    pub has_full_record: bool,
    /// Protection bits. On MeshCore this caches the device's **full**
    /// `Contact.flags` byte (bit 0 = protected/favourited; upper bits are
    /// real, firmware-meaningful per-contact flags the BBS preserves but
    /// never interprets). On Meshtastic this is synthetic, always `0` or `1`
    /// (`is_favorite as u8`).
    pub flags: u8,
    /// Unix-seconds timestamp set whenever this record transitions to
    /// protected. While within [`PROTECT_GRACE_SECS`] of now, readers must
    /// treat the record as protected even if `flags` bit 0 has not (yet)
    /// been confirmed by a device sync, and writers must not let incoming
    /// wire data lower it.
    pub protected_at: Option<u64>,
    /// Unix-seconds timestamp set whenever this record transitions to
    /// unprotected (manual delete or automatic cap eviction). Mirrors
    /// `protected_at`'s grace-window protection in the opposite direction —
    /// guards against a failed/dropped radio-side removal being silently
    /// undone by the next routine device sync.
    pub unprotected_at: Option<u64>,
    /// Meshtastic node number; `None` for MeshCore records. Needed to
    /// dispatch Meshtastic's native favourite/removal admin commands, which
    /// address nodes by number rather than by pubkey.
    pub node_num: Option<u32>,
    /// MeshCore only; `0` for Meshtastic records. Cached from
    /// `Contact.last_advert_timestamp` (a required wire field with no
    /// `Default`), validated the same way `upsert_with_timestamp` validates
    /// device-reported timestamps.
    pub last_advert_timestamp: u32,
    /// MeshCore only; `0` for Meshtastic records. The device's own
    /// `Contact.lastmod`, re-sent unchanged when protecting.
    pub lastmod: u32,
    /// MeshCore only; empty for Meshtastic records. Cached routing path from
    /// the last full `Contact`/`NewAdvert` frame, replayed as-is when
    /// protecting (sending "unknown" instead would unconditionally reset the
    /// device's working route — see research.md Decision 1).
    pub out_path: Vec<u8>,
    /// MeshCore only. `-1` (0xFF) = unknown/no path.
    pub out_path_len: i8,
    /// Plain counter, incremented under the bus's lock on every `flags`
    /// bit-0 transition (protect, unprotect, or reverted protect). The
    /// compare-and-swap discriminant [`AdvertBus::revert_protect`] and
    /// friends use to avoid clobbering a different, later transition on the
    /// same record — unix-seconds timestamps are not collision-safe or
    /// guaranteed monotonic enough for that purpose.
    pub generation: u64,
}

impl AdvertRecord {
    /// Whether this record is currently protected, applying the same
    /// grace-window rule every other reader in this module uses (`flags`
    /// bit 0, OR a still-live `protected_at` — see
    /// [`AdvertBus::is_currently_favourited`]). Exposed so callers outside
    /// this module (e.g. the web admin API) that already hold a cloned
    /// `AdvertRecord` from [`AdvertBus::list`] don't have to re-implement
    /// this logic against the raw bit alone, which would silently disagree
    /// with [`AdvertBus::list_protected`]'s own filter during an active
    /// grace window (found by the Phase 5 hostile audit).
    pub fn is_currently_protected(&self) -> bool {
        is_effectively_protected(self, unix_now_u64())
    }
}

/// Build a fresh, unprotected, not-yet-fully-known record.
fn new_record(pubkey: [u8; 32], now: i64, transport: &str) -> AdvertRecord {
    AdvertRecord {
        pubkey_hex: hex_encode(&pubkey),
        name: String::new(),
        adv_type: 0,
        lat: 0.0,
        lon: 0.0,
        first_seen_secs: now,
        last_seen_secs: now,
        transport: transport.to_owned(),
        has_full_record: false,
        flags: 0,
        protected_at: None,
        unprotected_at: None,
        node_num: None,
        last_advert_timestamp: 0,
        lastmod: 0,
        out_path: Vec::new(),
        out_path_len: -1,
        generation: 0,
    }
}

/// Whether `record` should currently be treated as protected — the bit
/// itself, OR'd with a still-live protect grace window (so a just-issued,
/// not-yet-device-confirmed protect can't be undone by a stale inbound sync
/// racing it). Used everywhere protection status is *read* (eviction
/// exclusion, cap counting, `AlreadyProtected` checks, TOCTOU re-checks) —
/// never a bare `flags & 1 != 0` check on its own.
fn is_effectively_protected(record: &AdvertRecord, now: u64) -> bool {
    record.flags & 1 != 0 || record.protected_at.is_some_and(|t| within_grace(t, now))
}

/// Merge an incoming wire-reported protected bit against local grace-window
/// state, for writers (`upsert_contact`/`upsert_meshtastic_node`) syncing
/// fresh device data. Whichever of `protected_at`/`unprotected_at` is more
/// recent and still live wins; if neither is live, the incoming bit is
/// trusted as-is. See research.md Decision 12a's round-17 correction — this
/// is a single order-independent comparison, not two sequential checks.
fn merge_protected_bit(
    incoming_bit: bool,
    protected_at: Option<u64>,
    unprotected_at: Option<u64>,
    now: u64,
) -> bool {
    let p = protected_at.filter(|&t| within_grace(t, now));
    let u = unprotected_at.filter(|&t| within_grace(t, now));
    match (p, u) {
        (Some(p), Some(u)) => p >= u,
        (Some(_), None) => true,
        (None, Some(_)) => false,
        (None, None) => incoming_bit,
    }
}

fn within_grace(t: u64, now: u64) -> bool {
    now.saturating_sub(t) < PROTECT_GRACE_SECS
}

fn prefix6(pubkey: &[u8; 32]) -> [u8; 6] {
    pubkey[..6].try_into().expect("pubkey is 32 bytes")
}

// ── Protection decision types ───────────────────────────────────────────────

/// Result of attempting to protect a record via
/// [`AdvertBus::mark_favourite_if_eligible`] /
/// [`AdvertBus::mark_favourite_if_eligible_by_node_num`].
#[derive(Debug, Clone, PartialEq)]
pub enum FavouriteOutcome {
    /// Just transitioned to protected — send the outbound protect frame
    /// built from this snapshot.
    Protected(FavouriteSnapshot),
    /// Transitioned to protected AND evicted a different, existing protected
    /// record to make room (the per-transport cap was reached). The caller
    /// must send both the new protect frame (from the first `FavouriteSnapshot`)
    /// and the evicted record's native removal command. The evicted record's
    /// raw pubkey is carried explicitly as the second field (not just
    /// `AdvertRecord.pubkey_hex`, which isn't decodable back into wire bytes)
    /// precisely so the caller can dispatch that removal without needing to
    /// already know it; the `AdvertRecord` itself is the third field, carried
    /// for its `transport`/`node_num` (Meshtastic removal needs `node_num`,
    /// not the pubkey).
    ProtectedWithEviction(FavouriteSnapshot, [u8; 32], Box<AdvertRecord>),
    /// Already protected before this call — nothing to do, and the caller
    /// must NOT retry later (there is nothing more that will ever change
    /// this outcome).
    AlreadyProtected,
    /// Cached record exists and is ineligible (wrong adv_type/role) —
    /// nothing to do, and the caller must NOT retry later.
    Ineligible,
    /// No cached record yet, or `has_full_record` is false — nothing to do
    /// YET, but the caller SHOULD retry once more data arrives (this is the
    /// only outcome that should ever cause a caller to mark an identity
    /// pending).
    NoRecordYet,
    /// The per-transport protected-contact cap was reached and no eviction
    /// candidate remained after excluding active-session and
    /// within-grace-window records. Terminal for this message, like
    /// `Ineligible` — not retried via a pending-protect mechanism. The
    /// caller SHOULD log a distinct warning.
    CapReached,
}

/// A snapshot of the data needed to build an outbound protect frame,
/// captured atomically at the moment a record transitions to protected.
#[derive(Debug, Clone, PartialEq)]
pub struct FavouriteSnapshot {
    /// Full 32-byte public key of the now-protected record.
    pub pubkey: [u8; 32],
    /// The record's cached name, at the moment of protection.
    pub name: String,
    /// The record's `adv_type`/role byte, at the moment of protection.
    pub adv_type: u8,
    /// Cached GPS latitude in decimal degrees.
    pub lat: f64,
    /// Cached GPS longitude in decimal degrees.
    pub lon: f64,
    /// MeshCore only; `0` on Meshtastic. Cached `Contact.last_advert_timestamp`.
    pub last_advert_timestamp: u32,
    /// MeshCore only; `0` on Meshtastic. Cached `Contact.lastmod`.
    pub lastmod: u32,
    /// MeshCore only; empty on Meshtastic. Cached routing path, replayed as-is.
    pub out_path: Vec<u8>,
    /// MeshCore only. Cached path length; `-1` means unknown.
    pub out_path_len: i8,
    /// The record's full flags byte after the protect bit was set —
    /// preserves any non-protect bits the device had already set.
    pub flags: u8,
    /// Unix-seconds timestamp this protect transition set.
    pub protected_at: u64,
    /// The record's generation counter after this protect transition — the
    /// compare-and-swap token a later revert must match exactly.
    pub generation: u64,
}

// ── AdvertBus ─────────────────────────────────────────────────────────────────

/// Storage backing an [`AdvertBus`], unified under one lock so the record
/// map and the Meshtastic `node_num` reverse index can never be observed or
/// mutated out of sync with each other.
struct AdvertBusInner {
    records: HashMap<[u8; 32], AdvertRecord>,
    /// Meshtastic only: `node_num -> pubkey`, since `record_node_advert`
    /// keys records by the node's real pubkey when known (falling back to a
    /// synthetic one only otherwise) and `try_protect_node` only ever has
    /// `node_num` to look up with.
    node_num_index: HashMap<u32, [u8; 32]>,
}

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
    inner: Mutex<AdvertBusInner>,
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
            .field(
                "record_count",
                &self.inner.lock().map_or(0, |i| i.records.len()),
            )
            .finish_non_exhaustive()
    }
}

impl AdvertBus {
    /// Create a new, empty bus.
    pub fn new() -> Self {
        let (send_tx, _) = broadcast::channel(8);
        Self {
            inner: Mutex::new(AdvertBusInner {
                records: HashMap::new(),
                node_num_index: HashMap::new(),
            }),
            send_tx,
        }
    }

    /// Insert or update a full advertisement from a named contact.
    ///
    /// Updates all fields for an existing record; preserves `first_seen_secs`.
    pub fn upsert(
        &self,
        pubkey: [u8; 32],
        name: String,
        adv_type: u8,
        gps_lat: i32,
        gps_lon: i32,
        transport: &str,
    ) {
        let now = unix_now();
        let lat = gps_lat as f64 / 1_000_000.0;
        let lon = gps_lon as f64 / 1_000_000.0;
        let mut inner = self.inner.lock().expect("advert bus poisoned");
        let entry = inner
            .records
            .entry(pubkey)
            .or_insert_with(|| new_record(pubkey, now, transport));
        entry.name = name;
        entry.adv_type = adv_type;
        entry.lat = lat;
        entry.lon = lon;
        entry.last_seen_secs = now;
        entry.transport = transport.to_owned();
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
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_with_timestamp(
        &self,
        pubkey: [u8; 32],
        name: String,
        adv_type: u8,
        gps_lat: i32,
        gps_lon: i32,
        device_last_seen: i64,
        transport: &str,
    ) {
        let now = unix_now();
        let last_seen = plausible_timestamp(device_last_seen, now);
        let lat = gps_lat as f64 / 1_000_000.0;
        let lon = gps_lon as f64 / 1_000_000.0;
        let mut inner = self.inner.lock().expect("advert bus poisoned");
        let entry = inner.records.entry(pubkey).or_insert_with(|| {
            let mut r = new_record(pubkey, now, transport);
            r.last_seen_secs = last_seen;
            r
        });
        entry.name = name;
        entry.adv_type = adv_type;
        entry.lat = lat;
        entry.lon = lon;
        entry.transport = transport.to_owned();
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
    pub fn upsert_short(&self, pubkey: [u8; 32], transport: &str) {
        let now = unix_now();
        let mut inner = self.inner.lock().expect("advert bus poisoned");
        inner
            .records
            .entry(pubkey)
            .and_modify(|e| {
                e.last_seen_secs = now;
                e.transport = transport.to_owned();
            })
            .or_insert_with(|| new_record(pubkey, now, transport));
    }

    /// Insert or update a full MeshCore contact record — the sole ingest path
    /// for `Contact`/`NewAdvert` frames, replacing `upsert`/`upsert_with_timestamp`
    /// for those two call sites (which carry per-contact detail `upsert`
    /// itself never captured: cached flags, routing path, device timestamps).
    ///
    /// Sets `has_full_record = true`. `flags` is the device's own full
    /// `Contact.flags` byte (not just bit 0) — the bus preserves whatever
    /// non-protect bits the device has set, merging only bit 0 against local
    /// grace-window state so a racing stale sync can't silently revert an
    /// unconfirmed local protect (or resurrect a just-deleted contact).
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_contact(
        &self,
        pubkey: [u8; 32],
        name: String,
        adv_type: u8,
        gps_lat: i32,
        gps_lon: i32,
        device_last_seen: i64,
        flags: u8,
        last_advert_timestamp: u32,
        lastmod: u32,
        out_path: Vec<u8>,
        out_path_len: i8,
        transport: &str,
    ) {
        let now = unix_now();
        let now_u64 = now.max(0) as u64;
        let last_seen = plausible_timestamp(device_last_seen, now);
        let lat = gps_lat as f64 / 1_000_000.0;
        let lon = gps_lon as f64 / 1_000_000.0;
        let mut inner = self.inner.lock().expect("advert bus poisoned");
        let already_existed = inner.records.contains_key(&pubkey);
        let entry = inner
            .records
            .entry(pubkey)
            .or_insert_with(|| new_record(pubkey, now, transport));
        entry.name = name;
        entry.adv_type = adv_type;
        entry.lat = lat;
        entry.lon = lon;
        entry.transport = transport.to_owned();
        if last_seen > entry.last_seen_secs {
            entry.last_seen_secs = last_seen;
        }
        entry.has_full_record = true;
        // Same plausibility clamp as `last_seen_secs` above — a device
        // without a synced RTC reports this as boot-relative (near-zero)
        // too, and it shares the same wire origin (Decision 4a).
        entry.last_advert_timestamp =
            plausible_timestamp(i64::from(last_advert_timestamp), now) as u32;
        entry.lastmod = lastmod;
        entry.out_path = out_path;
        entry.out_path_len = out_path_len;

        let previous_bit = entry.flags & 1 != 0;
        let incoming_bit = flags & 1 != 0;
        let merged_bit = merge_protected_bit(
            incoming_bit,
            entry.protected_at,
            entry.unprotected_at,
            now_u64,
        );
        entry.flags = (flags & !1) | u8::from(merged_bit);
        if merged_bit != previous_bit {
            entry.generation += 1;
            // Found by audit (round-4, phase4 hostile QA persona — the same
            // gap exists here as in upsert_meshtastic_node, see that
            // function's matching comment for the full round-2 fix
            // explanation): mirror protect_record/unprotect_locked's own
            // field-setting, but ONLY on a brand-new record's first-ever
            // sync (Decision 7 rehydration), never on an already-tracked
            // record's later merge-driven flip — doing it unconditionally
            // (round-1 fix) accidentally granted passive device reports a
            // grace-window priority they were never meant to have.
            if !already_existed && merged_bit {
                entry.protected_at = Some(now_u64);
                entry.unprotected_at = None;
            } else if !already_existed {
                entry.protected_at = None;
                entry.unprotected_at = Some(now_u64);
            }
        }
    }

    /// Insert or update a full Meshtastic node record — the sole ingest path
    /// for `record_node_advert`. Also records the `node_num -> pubkey`
    /// mapping used by the `*_by_node_num` methods below, in the same lock
    /// scope as the record update.
    ///
    /// `is_favorite`: `Some(v)` applies the same grace-window merge
    /// `upsert_contact` uses (only the connect-time full-sync path has a
    /// genuine device-reported bit); `None` leaves `flags` untouched
    /// entirely (the live `PORT_NODEINFO_APP` path carries no such bit —
    /// passing `Some(false)` there would silently un-protect a node on its
    /// own next routine self-announce).
    ///
    /// Returns `true` only if `node_num` already mapped to a *different*
    /// pubkey (a real identity change, e.g. a re-flashed device) — not the
    /// ordinary first-sighting or same-pubkey-reupserted cases. Callers use
    /// this to know whether a pending-protect retry needs to be re-armed for
    /// the corrected identity.
    #[allow(clippy::too_many_arguments)]
    pub fn upsert_meshtastic_node(
        &self,
        pubkey: [u8; 32],
        node_num: u32,
        name: String,
        role: u8,
        gps_lat: i32,
        gps_lon: i32,
        is_favorite: Option<bool>,
        transport: &str,
    ) -> bool {
        let now = unix_now();
        let now_u64 = now.max(0) as u64;
        let lat = gps_lat as f64 / 1_000_000.0;
        let lon = gps_lon as f64 / 1_000_000.0;
        let mut inner = self.inner.lock().expect("advert bus poisoned");

        let identity_changed = matches!(
            inner.node_num_index.get(&node_num),
            Some(existing) if *existing != pubkey
        );
        inner.node_num_index.insert(node_num, pubkey);

        let already_existed = inner.records.contains_key(&pubkey);
        let entry = inner
            .records
            .entry(pubkey)
            .or_insert_with(|| new_record(pubkey, now, transport));
        entry.name = name;
        entry.adv_type = role;
        entry.lat = lat;
        entry.lon = lon;
        entry.transport = transport.to_owned();
        entry.last_seen_secs = now;
        entry.has_full_record = true;
        entry.node_num = Some(node_num);

        if let Some(is_favorite) = is_favorite {
            let previous_bit = entry.flags & 1 != 0;
            let merged_bit = merge_protected_bit(
                is_favorite,
                entry.protected_at,
                entry.unprotected_at,
                now_u64,
            );
            entry.flags = u8::from(merged_bit);
            if merged_bit != previous_bit {
                entry.generation += 1;
                // Found by audit (round-4, phase4 hostile QA persona,
                // .audit/persist-mesh-contacts-2026-09-02-phase4/repros/
                // rehydrated_favorite_eviction_order_repro): mirror
                // protect_record/unprotect_locked's own field-setting —
                // but ONLY on a brand-new record's first-ever sync
                // (Decision 7 rehydration, e.g. right after a BBS restart),
                // never on an already-tracked record's later merge-driven
                // flip. Round-2 fix (Phase 4 audit's Skeptic pass, executed
                // repro): setting these unconditionally on every flip
                // accidentally grants an ordinary passive device report a
                // fresh `PROTECT_GRACE_SECS` grace-window priority it was
                // never meant to have — a second, genuinely fresher
                // contradicting report from the SAME device shortly after
                // would then be silently discarded by `merge_protected_bit`,
                // contradicting Decision 12a's "a genuine later external
                // change still eventually takes effect." Without this
                // `already_existed` gate, a device-rehydrated favorite's
                // `protected_at` stays `None` forever instead, so
                // `decide_favourite`'s eviction-candidate
                // `min_by_key(|r| r.protected_at.unwrap_or(0))` treats it as
                // the OLDEST protected record — making a long-standing,
                // device-confirmed favorite the FIRST thing evicted once the
                // cap is reached, backwards from intent.
                if !already_existed && merged_bit {
                    entry.protected_at = Some(now_u64);
                    entry.unprotected_at = None;
                } else if !already_existed {
                    entry.protected_at = None;
                    entry.unprotected_at = Some(now_u64);
                }
            }
        }

        identity_changed
    }

    /// Attempt to protect a MeshCore record by pubkey, enforcing the
    /// per-transport protected-contact cap.
    ///
    /// `is_eligible` classifies `adv_type` (chat/person vs. repeater/room
    /// on MeshCore, device role on Meshtastic). `protected_cap` is the
    /// configured maximum number of simultaneously-protected contacts for
    /// this record's transport. `exclude_prefixes` (6-byte pubkey prefixes)
    /// names contacts with an active BBS session right now — never chosen
    /// as an eviction candidate, mirroring
    /// [`stalest_pubkey_excluding`](Self::stalest_pubkey_excluding)'s own
    /// exclusion contract.
    ///
    /// See [`FavouriteOutcome`] for the full outcome contract, and
    /// `specs/001-persist-mesh-contacts/research.md` Decisions 0 and 5b for
    /// the underlying design.
    pub fn mark_favourite_if_eligible(
        &self,
        pubkey: [u8; 32],
        is_eligible: impl Fn(u8) -> bool,
        protected_cap: usize,
        exclude_prefixes: &[[u8; 6]],
    ) -> FavouriteOutcome {
        let mut inner = self.inner.lock().expect("advert bus poisoned");
        decide_favourite(
            &mut inner,
            pubkey,
            is_eligible,
            protected_cap,
            exclude_prefixes,
        )
    }

    /// The Meshtastic, `node_num`-keyed sibling of
    /// [`mark_favourite_if_eligible`](Self::mark_favourite_if_eligible) —
    /// resolves `node_num` to a pubkey via the reverse index under the same
    /// lock (never re-acquiring it), then delegates to the identical
    /// decision logic. `try_protect_node` MUST call this rather than the
    /// pubkey-keyed method directly: a node with a real Curve25519 key is
    /// stored under that key, not `synthetic_pubkey(node_num)`, so a naive
    /// direct lookup would see "no record" for the common case.
    pub fn mark_favourite_if_eligible_by_node_num(
        &self,
        node_num: u32,
        is_eligible: impl Fn(u8) -> bool,
        protected_cap: usize,
        exclude_prefixes: &[[u8; 6]],
    ) -> FavouriteOutcome {
        let mut inner = self.inner.lock().expect("advert bus poisoned");
        let Some(&pubkey) = inner.node_num_index.get(&node_num) else {
            return FavouriteOutcome::NoRecordYet;
        };
        decide_favourite(
            &mut inner,
            pubkey,
            is_eligible,
            protected_cap,
            exclude_prefixes,
        )
    }

    /// Clear local protected state for `pubkey` — a manual delete (sysop
    /// action) or the caller's own choice to stop treating this contact as
    /// permanent. Sets `unprotected_at` (guards against a stale device sync
    /// silently re-raising the bit before the radio applies a native
    /// removal). Returns the pre-clear record (needed by the caller to know
    /// which native removal command to send), or `None` if the pubkey is
    /// unknown or was already unprotected.
    ///
    /// Does not touch the radio — sending the native removal command is the
    /// caller's responsibility.
    pub fn unprotect(&self, pubkey: &[u8; 32]) -> Option<AdvertRecord> {
        let mut inner = self.inner.lock().expect("advert bus poisoned");
        let now = unix_now_u64();
        unprotect_locked(&mut inner, pubkey, now)
    }

    /// Revert a local protect commit that never actually reached the radio
    /// (a synchronously-detected send failure, or a connection dropping
    /// before a queued write could flush) — clears `flags` bit 0 and
    /// `protected_at`, but only if `record.generation` still equals
    /// `expected_generation`, so a different, later protect on the same
    /// pubkey is never clobbered. A no-op if the pubkey is unknown or the
    /// generation has since moved on.
    pub fn revert_protect(&self, pubkey: &[u8; 32], expected_generation: u64) {
        let mut inner = self.inner.lock().expect("advert bus poisoned");
        revert_protect_locked(&mut inner, pubkey, expected_generation);
    }

    /// The Meshtastic, `node_num`-keyed sibling of
    /// [`revert_protect`](Self::revert_protect).
    pub fn revert_protect_by_node_num(&self, node_num: u32, expected_generation: u64) {
        let mut inner = self.inner.lock().expect("advert bus poisoned");
        let Some(&pubkey) = inner.node_num_index.get(&node_num) else {
            return;
        };
        revert_protect_locked(&mut inner, &pubkey, expected_generation);
    }

    /// Fresh re-check of whether `pubkey` is currently protected, applying
    /// the same grace-window rule every other reader uses. Used immediately
    /// before sending a native removal command (radio-side eviction or
    /// delete) to close the TOCTOU window between selecting/deciding on a
    /// removal and actually sending it — a concurrently-landed protect for
    /// the same identity must not be silently undone.
    pub fn is_currently_favourited(&self, pubkey: &[u8; 32]) -> bool {
        let inner = self.inner.lock().expect("advert bus poisoned");
        let now = unix_now_u64();
        inner
            .records
            .get(pubkey)
            .is_some_and(|r| is_effectively_protected(r, now))
    }

    /// Return the `node_num` that currently owns `pubkey` in the reverse
    /// index, if any — the inverse of [`pubkey_by_node_num`](Self::pubkey_by_node_num).
    ///
    /// Meshtastic's `record_node_advert` uses this to reject identity
    /// hijacking: a reported real (non-synthetic) `public_key` that already
    /// belongs to a *different* `node_num` is treated as spoofed the same
    /// way a synthetic-namespace collision already is (found by audit —
    /// the original synthetic-namespace-only check left real-key replay
    /// unguarded: an attacker broadcasting a victim's genuine public key
    /// under their own `node_num` would otherwise overwrite the victim's
    /// `AdvertRecord.node_num`, corrupting which physical device a later
    /// eviction/removal command targets).
    pub fn node_num_owning_pubkey(&self, pubkey: &[u8; 32]) -> Option<u32> {
        let inner = self.inner.lock().expect("advert bus poisoned");
        inner.records.get(pubkey).and_then(|r| r.node_num)
    }

    /// The Meshtastic, `node_num`-keyed sibling of
    /// [`is_currently_favourited`](Self::is_currently_favourited).
    pub fn is_currently_favourited_by_node_num(&self, node_num: u32) -> bool {
        let inner = self.inner.lock().expect("advert bus poisoned");
        let now = unix_now_u64();
        inner
            .node_num_index
            .get(&node_num)
            .and_then(|pk| inner.records.get(pk))
            .is_some_and(|r| is_effectively_protected(r, now))
    }

    /// Resolve `node_num` to its currently-known full 32-byte pubkey (real
    /// Curve25519 key, or `synthetic_pubkey(node_num)` if none has been
    /// reported yet), via the same reverse index
    /// [`mark_favourite_if_eligible_by_node_num`](Self::mark_favourite_if_eligible_by_node_num)
    /// uses. Meshtastic's `try_protect_node` needs this to build a correct
    /// eviction-exclusion prefix list from its active sessions' `node_num`s
    /// (`crates/bbs-meshtastic/src/lib.rs`) — a session's own pubkey isn't
    /// cached locally the way MeshCore's is, since Meshtastic sessions are
    /// keyed purely by `node_num`.
    pub fn pubkey_by_node_num(&self, node_num: u32) -> Option<[u8; 32]> {
        let inner = self.inner.lock().expect("advert bus poisoned");
        inner.node_num_index.get(&node_num).copied()
    }

    /// Return all records sorted by `last_seen_secs` descending (newest first).
    pub fn list(&self) -> Vec<AdvertRecord> {
        let inner = self.inner.lock().expect("advert bus poisoned");
        let mut v: Vec<_> = inner.records.values().cloned().collect();
        v.sort_by_key(|r| std::cmp::Reverse(r.last_seen_secs));
        v
    }

    /// Total number of records the bus has ever seen (protected or not) —
    /// backs the web UI's "Discovered Contacts" nav badge. Cheaper than
    /// `list().len()`: no clone, no sort.
    pub fn count(&self) -> usize {
        let inner = self.inner.lock().expect("advert bus poisoned");
        inner.records.len()
    }

    /// Count of currently-protected records — backs the web UI's "Contacts"
    /// nav badge. See [`list_protected`](Self::list_protected) for what
    /// "currently protected" means; cheaper than `list_protected().len()`:
    /// no clone, no sort.
    pub fn count_protected(&self) -> usize {
        let inner = self.inner.lock().expect("advert bus poisoned");
        let now = unix_now_u64();
        inner
            .records
            .values()
            .filter(|r| is_effectively_protected(r, now))
            .count()
    }

    /// [`list`](Self::list), filtered to currently-protected records — backs
    /// the web UI's "Contacts" view (as distinct from "Discovered Contacts",
    /// which shows everything via `list`).
    pub fn list_protected(&self) -> Vec<AdvertRecord> {
        let inner = self.inner.lock().expect("advert bus poisoned");
        let now = unix_now_u64();
        let mut v: Vec<_> = inner
            .records
            .values()
            .filter(|r| is_effectively_protected(r, now))
            .cloned()
            .collect();
        v.sort_by_key(|r| std::cmp::Reverse(r.last_seen_secs));
        v
    }

    /// Return the full 32-byte public key of the least-recently-seen,
    /// currently-unprotected contact whose key prefix (first 6 bytes) does
    /// not appear in `exclude_prefixes`, restricted to `transport`.
    ///
    /// Used by each transport's own `ContactsFull`-equivalent handler to
    /// pick a stale contact for eviction when the radio's contact table is
    /// full: the caller can then send a native removal for the returned key
    /// to free a table slot. Never returns a protected record (protection
    /// exists specifically to survive this kind of eviction) or one from a
    /// different transport (`AdvertBus` is shared across both, keyed by
    /// pubkey/synthetic-pubkey in the same map).
    ///
    /// Returns `None` if nothing on `transport` is both non-excluded and
    /// currently unprotected.
    pub fn stalest_pubkey_excluding(
        &self,
        exclude_prefixes: &[[u8; 6]],
        transport: &str,
    ) -> Option<[u8; 32]> {
        let inner = self.inner.lock().expect("advert bus poisoned");
        let now = unix_now_u64();
        inner
            .records
            .iter()
            .filter(|(pubkey, rec)| {
                rec.transport == transport
                    && !exclude_prefixes.contains(&prefix6(pubkey))
                    && !is_effectively_protected(rec, now)
            })
            .min_by_key(|(_, rec)| rec.last_seen_secs)
            .map(|(pubkey, _)| *pubkey)
    }

    /// Diagnostic query for `transport`'s `ContactsFull`-equivalent handler:
    /// `true` only if there is at least one non-excluded record on
    /// `transport` and every one of them is currently protected — i.e. the
    /// specific reason [`stalest_pubkey_excluding`](Self::stalest_pubkey_excluding)
    /// returned `None` was "all favourited," distinct from "nothing known"
    /// or "everything session-excluded." Never changes eviction behavior.
    pub fn all_remaining_favourited(&self, exclude_prefixes: &[[u8; 6]], transport: &str) -> bool {
        let inner = self.inner.lock().expect("advert bus poisoned");
        let now = unix_now_u64();
        let mut saw_any = false;
        for (pubkey, rec) in inner.records.iter() {
            if rec.transport != transport || exclude_prefixes.contains(&prefix6(pubkey)) {
                continue;
            }
            saw_any = true;
            if !is_effectively_protected(rec, now) {
                return false;
            }
        }
        saw_any
    }

    /// Look up the human-readable node name for a given 6-byte key prefix.
    ///
    /// Returns `None` if the prefix is not in the bus or if its name field is
    /// empty (i.e. only a short advert has been received so far).
    pub fn name_by_prefix(&self, prefix: &[u8; 6]) -> Option<String> {
        let inner = self.inner.lock().expect("advert bus poisoned");
        inner
            .records
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

    /// Look up the full 32-byte public key for a given 6-byte key prefix,
    /// restricted to `transport` (the bus is shared across both transports'
    /// pubkey/synthetic-pubkey spaces in the same map, so an unscoped lookup
    /// risks resolving a session on one transport to a record from the
    /// other).
    ///
    /// Seeds a brand-new session's cached full pubkey from an already-known
    /// `AdvertBus` record, so a sender whose full contact record predates
    /// their first-ever DM to the BBS is still protectable on that first
    /// message rather than only from the second one onward. Called from
    /// MeshCore's `get_or_create_session` (both its new-session slow path
    /// and its stale-session-refresh path — see
    /// `crates/bbs-mesh/src/transport.rs`); Meshtastic's equivalent call
    /// site lands in a later phase.
    pub fn full_pubkey_by_prefix(&self, prefix: &[u8; 6], transport: &str) -> Option<[u8; 32]> {
        let inner = self.inner.lock().expect("advert bus poisoned");
        inner
            .records
            .iter()
            .find(|(pubkey, rec)| pubkey[..6] == *prefix && rec.transport == transport)
            .map(|(pubkey, _)| *pubkey)
    }

    /// Remove all *unprotected* records from the bus — protected records
    /// (`flags & 1 != 0`) survive this call, along with their `node_num`
    /// mappings.
    ///
    /// Useful when a sysop wants to flush stale, never-messaged discovered
    /// contacts without restarting the BBS (e.g. after correcting device
    /// clocks). **BLOCKING fix, round 19**: an earlier version of this
    /// method (`clear()`) unconditionally wiped the *entire* bus, including
    /// every currently-protected contact's `flags`/`protected_at`/
    /// `has_full_record`/`node_num` — directly violating FR-007's "once
    /// protected, stays protected" guarantee, since "Contacts" and
    /// "Discovered Contacts" share this same storage. The BBS's own
    /// `ContactsFull` eviction handler would then see a just-wiped
    /// protected contact as an ordinary, evictable candidate and could
    /// issue a real removal against it before the next full resync
    /// rehydrates the bit — the exact repeater-flood/`ContactsFull` failure
    /// this feature exists to survive, self-inflicted by a pre-existing
    /// maintenance button (see specs/001-persist-mesh-contacts/research.md
    /// Decision 14).
    ///
    /// Returns the number of records removed, for callers that want to
    /// report it (e.g. an audit-log entry).
    pub fn clear_unprotected(&self) -> usize {
        let mut inner = self.inner.lock().expect("advert bus poisoned");
        let now = unix_now_u64();
        let to_remove: Vec<[u8; 32]> = inner
            .records
            .iter()
            .filter(|(_, r)| !is_effectively_protected(r, now))
            .map(|(pk, _)| *pk)
            .collect();
        for pk in &to_remove {
            inner.records.remove(pk);
        }
        inner.node_num_index.retain(|_, pk| !to_remove.contains(pk));
        to_remove.len()
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

// ── Protection decision core (lock already held) ────────────────────────────

/// The actual protect-or-not decision, callable while holding a single
/// already-acquired lock on `AdvertBusInner` — no separate check-then-act
/// sequence, so there is no window for a concurrent caller to observe or
/// create an inconsistent intermediate state.
fn decide_favourite(
    inner: &mut AdvertBusInner,
    pubkey: [u8; 32],
    is_eligible: impl Fn(u8) -> bool,
    protected_cap: usize,
    exclude_prefixes: &[[u8; 6]],
) -> FavouriteOutcome {
    let now = unix_now_u64();

    let (has_full_record, effectively_protected, adv_type, transport) = {
        let Some(record) = inner.records.get(&pubkey) else {
            return FavouriteOutcome::NoRecordYet;
        };
        (
            record.has_full_record,
            is_effectively_protected(record, now),
            record.adv_type,
            record.transport.clone(),
        )
    };

    if !has_full_record {
        return FavouriteOutcome::NoRecordYet;
    }
    if effectively_protected {
        return FavouriteOutcome::AlreadyProtected;
    }
    // `is_eligible` is caller-supplied and runs while `inner`'s lock is held;
    // a panicking classifier must not poison the whole bus (every reader
    // uses `.expect("advert bus poisoned")` with no recovery). Safe to catch
    // here specifically: nothing in `inner` has been mutated yet at this
    // point, so a caught panic leaves no partially-applied state behind.
    let eligible =
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| is_eligible(adv_type))) {
            Ok(eligible) => eligible,
            Err(_) => return FavouriteOutcome::Ineligible,
        };
    if !eligible {
        return FavouriteOutcome::Ineligible;
    }

    // A cap of 0 means "never protect on this transport" — never attempt an
    // evict-to-make-room here, or a single already-protected record would
    // perpetually be evicted and replaced, capping at 1 instead of 0.
    if protected_cap == 0 {
        return FavouriteOutcome::CapReached;
    }

    let protected_count = inner
        .records
        .values()
        .filter(|r| r.transport == transport && is_effectively_protected(r, now))
        .count();

    if protected_count < protected_cap {
        return FavouriteOutcome::Protected(protect_record(inner, pubkey, now));
    }

    // Cap reached: find the oldest-protected, currently-evictable candidate
    // on the same transport (never the record we're about to protect,
    // never an active session, never something protected too recently to
    // safely evict).
    let victim_pubkey = inner
        .records
        .iter()
        .filter(|(pk, r)| {
            **pk != pubkey
                && r.transport == transport
                && is_effectively_protected(r, now)
                && !exclude_prefixes.contains(&prefix6(pk))
                && !r.protected_at.is_some_and(|t| within_grace(t, now))
        })
        .min_by_key(|(_, r)| r.protected_at.unwrap_or(0))
        .map(|(pk, _)| *pk);

    let Some(victim_pubkey) = victim_pubkey else {
        return FavouriteOutcome::CapReached;
    };

    let victim_before = unprotect_locked(inner, &victim_pubkey, now)
        .expect("victim_pubkey was just selected from currently-protected records");

    let snapshot = protect_record(inner, pubkey, now);
    FavouriteOutcome::ProtectedWithEviction(snapshot, victim_pubkey, Box::new(victim_before))
}

/// Transition `pubkey`'s record to protected (caller has already confirmed
/// it exists, is eligible, and there is room for it) and return the
/// snapshot needed to build the outbound protect frame. Always succeeds —
/// callers wrap the result in the appropriate `FavouriteOutcome` variant
/// themselves rather than this function returning one, so there is no
/// spurious non-`Protected` case for a caller to (mis)handle after it may
/// have already committed an eviction.
fn protect_record(inner: &mut AdvertBusInner, pubkey: [u8; 32], now: u64) -> FavouriteSnapshot {
    let record = inner
        .records
        .get_mut(&pubkey)
        .expect("caller already confirmed this record exists");
    record.flags |= 1;
    record.protected_at = Some(now);
    record.unprotected_at = None;
    record.generation += 1;
    FavouriteSnapshot {
        pubkey,
        name: record.name.clone(),
        adv_type: record.adv_type,
        lat: record.lat,
        lon: record.lon,
        last_advert_timestamp: record.last_advert_timestamp,
        lastmod: record.lastmod,
        out_path: record.out_path.clone(),
        out_path_len: record.out_path_len,
        flags: record.flags,
        protected_at: now,
        generation: record.generation,
    }
}

/// Clear protected state for `pubkey` if it is currently (effectively)
/// protected; returns the pre-clear record. `None` if unknown or already
/// unprotected. Shared by [`AdvertBus::unprotect`] (manual delete) and
/// [`decide_favourite`]'s own cap-eviction path — one clearing
/// implementation, not two.
fn unprotect_locked(
    inner: &mut AdvertBusInner,
    pubkey: &[u8; 32],
    now: u64,
) -> Option<AdvertRecord> {
    let record = inner.records.get_mut(pubkey)?;
    if !is_effectively_protected(record, now) {
        return None;
    }
    let before = record.clone();
    record.flags &= !1;
    record.protected_at = None;
    record.unprotected_at = Some(now);
    record.generation += 1;
    Some(before)
}

fn revert_protect_locked(inner: &mut AdvertBusInner, pubkey: &[u8; 32], expected_generation: u64) {
    if let Some(record) = inner.records.get_mut(pubkey) {
        if record.generation == expected_generation {
            record.flags &= !1;
            record.protected_at = None;
            record.generation += 1;
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────────

// Accept only plausible Unix timestamps. Devices without a synced RTC
// report seconds-since-boot which are tiny (→ dates near 1970); devices
// with a misconfigured clock can exceed the current time by years.
// 2020-01-01 UTC is a safe floor — no BBS contact predates MeshCore.
// 5-minute ceiling fudge tolerates minor clock skew between devices.
const MIN_PLAUSIBLE_TS: i64 = 1_577_836_800; // 2020-01-01 00:00:00 UTC
const CLOCK_FUDGE_SECS: i64 = 300; // 5 minutes

/// Validate a device-reported timestamp, falling back to `now` if implausible.
fn plausible_timestamp(device_reported: i64, now: i64) -> i64 {
    if device_reported >= MIN_PLAUSIBLE_TS && device_reported <= now + CLOCK_FUDGE_SECS {
        device_reported
    } else {
        now
    }
}

fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn unix_now_u64() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
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

    fn always_eligible(_adv_type: u8) -> bool {
        true
    }

    fn never_eligible(_adv_type: u8) -> bool {
        false
    }

    /// A device timestamp of 0 (unset) falls back to wall-clock time.
    #[test]
    fn zero_device_ts_falls_back_to_now() {
        let bus = AdvertBus::new();
        bus.upsert_with_timestamp(dummy_key(1), "A".into(), 1, 0, 0, 0, "meshcore");
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
        bus.upsert_with_timestamp(dummy_key(2), "B".into(), 1, 0, 0, boot_relative, "meshcore");
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
        bus.upsert_with_timestamp(
            dummy_key(3),
            "C".into(),
            1,
            0,
            0,
            very_far_future,
            "meshcore",
        );
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
        bus.upsert_with_timestamp(dummy_key(4), "D".into(), 1, 0, 0, plausible, "meshcore");
        let ts = bus.list()[0].last_seen_secs;
        assert_eq!(ts, plausible, "plausible ts should be stored unchanged");
    }

    /// `clear_unprotected()` empties the bus of unprotected records.
    #[test]
    fn clear_unprotected_empties_bus_of_unprotected_records() {
        let bus = AdvertBus::new();
        bus.upsert(dummy_key(5), "E".into(), 1, 0, 0, "meshcore");
        assert_eq!(bus.list().len(), 1);
        bus.clear_unprotected();
        assert_eq!(bus.list().len(), 0);
    }

    /// T038d (round-19 BLOCKING fix): a protected record must survive
    /// `clear_unprotected()` — the whole point of the rename from `clear()`.
    #[test]
    fn clear_unprotected_preserves_protected_records() {
        let bus = AdvertBus::new();
        let protected_key = dummy_key(112);
        let unprotected_key = dummy_key(113);
        seed_meshcore_contact(&bus, protected_key);
        seed_meshcore_contact(&bus, unprotected_key);
        assert!(matches!(
            bus.mark_favourite_if_eligible(protected_key, always_eligible, 350, &[]),
            FavouriteOutcome::Protected(_)
        ));

        bus.clear_unprotected();

        assert!(
            bus.is_currently_favourited(&protected_key),
            "a protected record must survive clear_unprotected()"
        );
        assert_eq!(
            bus.list().len(),
            1,
            "the unprotected record must be removed, leaving only the protected one"
        );
    }

    /// `stalest_pubkey_excluding` returns the oldest contact not in the exclusion list.
    #[test]
    fn stalest_pubkey_excluding_returns_oldest_non_excluded() {
        let bus = AdvertBus::new();
        let old_key = dummy_key(10);
        let new_key = dummy_key(20);
        let excluded_key = dummy_key(30);

        // old_key was last seen in Nov 2023.
        bus.upsert_with_timestamp(
            old_key,
            "OldNode".into(),
            1,
            0,
            0,
            1_700_000_000,
            "meshcore",
        );
        // new_key was last seen in Jan 2025.
        bus.upsert_with_timestamp(
            new_key,
            "NewNode".into(),
            1,
            0,
            0,
            1_735_689_600,
            "meshcore",
        );
        // excluded_key is even older but excluded.
        bus.upsert_with_timestamp(
            excluded_key,
            "ExcludedNode".into(),
            1,
            0,
            0,
            1_500_000_000,
            "meshcore",
        );

        let excluded_prefix: [u8; 6] = excluded_key[..6].try_into().unwrap();
        let result = bus.stalest_pubkey_excluding(&[excluded_prefix], "meshcore");

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
        bus.upsert(key, "A".into(), 1, 0, 0, "meshcore");
        let prefix: [u8; 6] = key[..6].try_into().unwrap();
        assert_eq!(bus.stalest_pubkey_excluding(&[prefix], "meshcore"), None);
    }

    /// An empty bus returns `None`.
    #[test]
    fn stalest_pubkey_excluding_empty_bus_returns_none() {
        let bus = AdvertBus::new();
        assert_eq!(bus.stalest_pubkey_excluding(&[], "meshcore"), None);
    }

    /// `stalest_pubkey_excluding` never returns a record from the other transport,
    /// and never returns a protected record.
    #[test]
    fn stalest_pubkey_excluding_is_transport_and_protection_scoped() {
        let bus = AdvertBus::new();
        let meshcore_key = dummy_key(50);
        let meshtastic_key = dummy_key(51);
        seed_meshcore_contact(&bus, meshcore_key);
        bus.upsert_meshtastic_node(meshtastic_key, 1, "MT".into(), 0, 0, 0, None, "meshtastic");

        assert_eq!(
            bus.stalest_pubkey_excluding(&[], "meshcore"),
            Some(meshcore_key)
        );
        assert_eq!(
            bus.stalest_pubkey_excluding(&[], "meshtastic"),
            Some(meshtastic_key)
        );

        // Protect the meshcore contact — it must stop being a candidate.
        let outcome = bus.mark_favourite_if_eligible(meshcore_key, always_eligible, 350, &[]);
        assert!(matches!(outcome, FavouriteOutcome::Protected(_)));
        assert_eq!(bus.stalest_pubkey_excluding(&[], "meshcore"), None);
    }

    /// The transport name is stored and surfaced via `list()`.
    #[test]
    fn transport_is_recorded() {
        let bus = AdvertBus::new();
        bus.upsert(dummy_key(40), "MC".into(), 1, 0, 0, "meshcore");
        bus.upsert_short(dummy_key(41), "meshtastic");
        let by_name: std::collections::HashMap<String, String> = bus
            .list()
            .into_iter()
            .map(|r| (r.name, r.transport))
            .collect();
        assert_eq!(by_name.get("MC").map(String::as_str), Some("meshcore"));
        // The short advert has no name; find it by transport instead.
        assert!(
            bus.list().iter().any(|r| r.transport == "meshtastic"),
            "short advert should carry its transport"
        );
    }

    /// T038a: `list_protected()` returns only currently-protected records,
    /// as a subset of `list()`.
    #[test]
    fn list_protected_returns_only_protected_records() {
        let bus = AdvertBus::new();
        let protected_key = dummy_key(115);
        let unprotected_key = dummy_key(116);
        seed_meshcore_contact(&bus, protected_key);
        bus.upsert(unprotected_key, "U".into(), 1, 0, 0, "meshcore");
        assert!(matches!(
            bus.mark_favourite_if_eligible(protected_key, always_eligible, 350, &[]),
            FavouriteOutcome::Protected(_)
        ));

        assert_eq!(bus.list().len(), 2);
        let protected = bus.list_protected();
        assert_eq!(protected.len(), 1);
        assert_eq!(protected[0].pubkey_hex, hex_encode(&protected_key));
    }

    /// `count()`/`count_protected()` must agree with `list().len()`/
    /// `list_protected().len()` for the same bus state — they exist only to
    /// avoid the clone+sort those do, not to define a different answer.
    #[test]
    fn count_and_count_protected_match_list_lengths() {
        let bus = AdvertBus::new();
        assert_eq!(bus.count(), 0);
        assert_eq!(bus.count_protected(), 0);

        let protected_key = dummy_key(117);
        let unprotected_key = dummy_key(118);
        seed_meshcore_contact(&bus, protected_key);
        bus.upsert(unprotected_key, "U".into(), 1, 0, 0, "meshcore");
        assert!(matches!(
            bus.mark_favourite_if_eligible(protected_key, always_eligible, 350, &[]),
            FavouriteOutcome::Protected(_)
        ));

        assert_eq!(bus.count(), bus.list().len());
        assert_eq!(bus.count(), 2);
        assert_eq!(bus.count_protected(), bus.list_protected().len());
        assert_eq!(bus.count_protected(), 1);
    }

    /// Phase 5 hostile-audit regression (Extractor pass): `AdvertRecord::
    /// is_currently_protected` must agree with `list_protected()`'s own
    /// filter for the same record, so a caller reading via plain `list()`
    /// (e.g. the web admin API's "Discovered Contacts" view) can't disagree
    /// with what `list_protected()` ("Contacts") reports for the identical
    /// record.
    #[test]
    fn advert_record_is_currently_protected_matches_list_protected() {
        let bus = AdvertBus::new();
        let protected_key = dummy_key(117);
        let unprotected_key = dummy_key(118);
        seed_meshcore_contact(&bus, protected_key);
        bus.upsert(unprotected_key, "U".into(), 1, 0, 0, "meshcore");
        assert!(matches!(
            bus.mark_favourite_if_eligible(protected_key, always_eligible, 350, &[]),
            FavouriteOutcome::Protected(_)
        ));

        let all = bus.list();
        let protected_record = all
            .iter()
            .find(|r| r.pubkey_hex == hex_encode(&protected_key))
            .unwrap();
        let unprotected_record = all
            .iter()
            .find(|r| r.pubkey_hex == hex_encode(&unprotected_key))
            .unwrap();
        assert!(protected_record.is_currently_protected());
        assert!(!unprotected_record.is_currently_protected());
    }

    // ── FavouriteOutcome / decide_favourite ─────────────────────────────────

    fn seed_meshcore_contact(bus: &AdvertBus, key: [u8; 32]) {
        bus.upsert_contact(
            key,
            "Test".into(),
            1, // eligible adv_type
            0,
            0,
            now_secs(),
            0b0000_0110, // non-zero upper bits, to confirm they survive protection
            0,
            0,
            Vec::new(),
            -1,
            "meshcore",
        );
    }

    /// Push `key`'s `protected_at` back outside `PROTECT_GRACE_SECS`, so
    /// tests can simulate "protected a while ago" without actually sleeping.
    /// White-box access to `AdvertBus::inner` — valid since `mod tests` is a
    /// child of this module.
    fn backdate_protected_at(bus: &AdvertBus, key: &[u8; 32]) {
        let mut inner = bus.inner.lock().expect("advert bus poisoned");
        let record = inner.records.get_mut(key).expect("record must exist");
        assert!(record.protected_at.is_some(), "record must be protected");
        record.protected_at = Some(unix_now_u64().saturating_sub(PROTECT_GRACE_SECS + 1));
    }

    /// [`backdate_protected_at`]'s sibling for `unprotected_at`, so tests can
    /// simulate "unprotected a while ago" without sleeping.
    fn backdate_unprotected_at(bus: &AdvertBus, key: &[u8; 32]) {
        let mut inner = bus.inner.lock().expect("advert bus poisoned");
        let record = inner.records.get_mut(key).expect("record must exist");
        assert!(
            record.unprotected_at.is_some(),
            "record must be unprotected"
        );
        record.unprotected_at = Some(unix_now_u64().saturating_sub(PROTECT_GRACE_SECS + 1));
    }

    #[test]
    fn no_record_yet_before_full_record() {
        let bus = AdvertBus::new();
        let key = dummy_key(60);
        bus.upsert_short(key, "meshcore");
        assert_eq!(
            bus.mark_favourite_if_eligible(key, always_eligible, 350, &[]),
            FavouriteOutcome::NoRecordYet
        );
    }

    #[test]
    fn unknown_pubkey_is_no_record_yet() {
        let bus = AdvertBus::new();
        assert_eq!(
            bus.mark_favourite_if_eligible(dummy_key(61), always_eligible, 350, &[]),
            FavouriteOutcome::NoRecordYet
        );
    }

    #[test]
    fn ineligible_record_is_never_protected() {
        let bus = AdvertBus::new();
        let key = dummy_key(62);
        seed_meshcore_contact(&bus, key);
        assert_eq!(
            bus.mark_favourite_if_eligible(key, never_eligible, 350, &[]),
            FavouriteOutcome::Ineligible
        );
    }

    #[test]
    fn protects_eligible_full_record_preserving_upper_flag_bits() {
        let bus = AdvertBus::new();
        let key = dummy_key(63);
        seed_meshcore_contact(&bus, key);
        let outcome = bus.mark_favourite_if_eligible(key, always_eligible, 350, &[]);
        let FavouriteOutcome::Protected(snapshot) = outcome else {
            panic!("expected Protected, got {outcome:?}");
        };
        assert_eq!(snapshot.flags & 1, 1, "protect bit must be set");
        assert_eq!(
            snapshot.flags & 0b0000_0110,
            0b0000_0110,
            "device's own upper flag bits must survive protection"
        );
        assert!(bus.is_currently_favourited(&key));
    }

    #[test]
    fn already_protected_is_not_reprotected() {
        let bus = AdvertBus::new();
        let key = dummy_key(64);
        seed_meshcore_contact(&bus, key);
        assert!(matches!(
            bus.mark_favourite_if_eligible(key, always_eligible, 350, &[]),
            FavouriteOutcome::Protected(_)
        ));
        assert_eq!(
            bus.mark_favourite_if_eligible(key, always_eligible, 350, &[]),
            FavouriteOutcome::AlreadyProtected
        );
    }

    #[test]
    fn cap_reached_evicts_oldest_protected_contact() {
        let bus = AdvertBus::new();
        let old_key = dummy_key(70);
        let new_key = dummy_key(71);
        seed_meshcore_contact(&bus, old_key);
        seed_meshcore_contact(&bus, new_key);

        let outcome = bus.mark_favourite_if_eligible(old_key, always_eligible, 1, &[]);
        assert!(matches!(outcome, FavouriteOutcome::Protected(_)));
        backdate_protected_at(&bus, &old_key);

        let outcome = bus.mark_favourite_if_eligible(new_key, always_eligible, 1, &[]);
        let FavouriteOutcome::ProtectedWithEviction(snapshot, evicted_pubkey, evicted) = outcome
        else {
            panic!("expected ProtectedWithEviction, got {outcome:?}");
        };
        assert_eq!(snapshot.pubkey, new_key);
        assert_eq!(evicted_pubkey, old_key);
        assert_eq!(evicted.pubkey_hex, hex_encode(&old_key));
        assert!(bus.is_currently_favourited(&new_key));
        assert!(!bus.is_currently_favourited(&old_key));
    }

    #[test]
    fn cap_reached_skips_active_session_and_grace_window_candidates() {
        let bus = AdvertBus::new();
        let session_key = dummy_key(72);
        let fresh_key = dummy_key(73);
        let evictable_key = dummy_key(74);
        let new_key = dummy_key(75);
        seed_meshcore_contact(&bus, session_key);
        seed_meshcore_contact(&bus, fresh_key);
        seed_meshcore_contact(&bus, evictable_key);
        seed_meshcore_contact(&bus, new_key);

        // Protect three contacts, filling a cap of 3.
        assert!(matches!(
            bus.mark_favourite_if_eligible(evictable_key, always_eligible, 3, &[]),
            FavouriteOutcome::Protected(_)
        ));
        assert!(matches!(
            bus.mark_favourite_if_eligible(session_key, always_eligible, 3, &[]),
            FavouriteOutcome::Protected(_)
        ));
        assert!(matches!(
            bus.mark_favourite_if_eligible(fresh_key, always_eligible, 3, &[]),
            FavouriteOutcome::Protected(_)
        ));
        // Age session_key and evictable_key out of their own grace windows —
        // only fresh_key stays "just protected." session_key must still be
        // skipped (active session); evictable_key is the sole valid target.
        backdate_protected_at(&bus, &session_key);
        backdate_protected_at(&bus, &evictable_key);

        let session_prefix: [u8; 6] = session_key[..6].try_into().unwrap();
        // fresh_key was *just* protected, so it's within PROTECT_GRACE_SECS;
        // session_key is excluded as an active session — only evictable_key
        // (protected first, so oldest) remains a valid candidate.
        let outcome =
            bus.mark_favourite_if_eligible(new_key, always_eligible, 3, &[session_prefix]);
        let FavouriteOutcome::ProtectedWithEviction(_, evicted_pubkey, evicted) = outcome else {
            panic!("expected ProtectedWithEviction, got {outcome:?}");
        };
        assert_eq!(evicted_pubkey, evictable_key);
        assert_eq!(evicted.pubkey_hex, hex_encode(&evictable_key));
        assert!(bus.is_currently_favourited(&session_key));
        assert!(bus.is_currently_favourited(&fresh_key));
    }

    #[test]
    fn cap_reached_with_nothing_evictable_returns_cap_reached() {
        let bus = AdvertBus::new();
        let session_key = dummy_key(76);
        let new_key = dummy_key(77);
        seed_meshcore_contact(&bus, session_key);
        seed_meshcore_contact(&bus, new_key);

        assert!(matches!(
            bus.mark_favourite_if_eligible(session_key, always_eligible, 1, &[]),
            FavouriteOutcome::Protected(_)
        ));

        let session_prefix: [u8; 6] = session_key[..6].try_into().unwrap();
        let outcome =
            bus.mark_favourite_if_eligible(new_key, always_eligible, 1, &[session_prefix]);
        assert_eq!(outcome, FavouriteOutcome::CapReached);
        assert!(!bus.is_currently_favourited(&new_key));
        // The session-excluded contact must remain untouched.
        assert!(bus.is_currently_favourited(&session_key));
    }

    /// Audit regression (2026-09-02, finding M5): a cap of exactly 0 must
    /// mean "never protect," not "cap at 1 via evict-and-reprotect."
    #[test]
    fn zero_cap_never_protects_even_via_eviction() {
        let bus = AdvertBus::new();
        let already_protected = dummy_key(103);
        let new_key = dummy_key(104);
        seed_meshcore_contact(&bus, already_protected);
        seed_meshcore_contact(&bus, new_key);

        // Get one contact protected under a normal cap first...
        assert!(matches!(
            bus.mark_favourite_if_eligible(already_protected, always_eligible, 1, &[]),
            FavouriteOutcome::Protected(_)
        ));
        backdate_protected_at(&bus, &already_protected);

        // ...then confirm a cap of 0 refuses to evict it to make room for
        // a new contact (the pre-fix behavior would have evicted
        // `already_protected` and protected `new_key` instead, capping at
        // 1 rather than 0).
        assert_eq!(
            bus.mark_favourite_if_eligible(new_key, always_eligible, 0, &[]),
            FavouriteOutcome::CapReached
        );
        assert!(bus.is_currently_favourited(&already_protected));
        assert!(!bus.is_currently_favourited(&new_key));
    }

    /// Audit regression (2026-09-02, finding H1): a panicking `is_eligible`
    /// closure must not poison the bus — every other call must keep working
    /// afterward.
    #[test]
    fn panicking_is_eligible_does_not_poison_the_bus() {
        let bus = AdvertBus::new();
        let key = dummy_key(105);
        seed_meshcore_contact(&bus, key);

        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            bus.mark_favourite_if_eligible(key, |_| panic!("boom"), 350, &[])
        }));
        assert!(
            outcome.is_ok(),
            "a panicking classifier must not unwind out of mark_favourite_if_eligible"
        );
        assert_eq!(outcome.unwrap(), FavouriteOutcome::Ineligible);

        // The bus must still be fully usable — not poisoned.
        let FavouriteOutcome::Protected(snapshot) =
            bus.mark_favourite_if_eligible(key, always_eligible, 350, &[])
        else {
            panic!("expected Protected — the bus should not have been poisoned by the panic above");
        };
        assert_eq!(snapshot.pubkey, key);
        assert_eq!(snapshot.flags & 1, 1);
        assert!(bus.is_currently_favourited(&key));
    }

    /// Audit regression (2026-09-02, finding M4): a device-sync merge that
    /// actually changes the protected bit must bump `generation` too, not
    /// just `flags` — otherwise a `revert_protect`/`revert_protect_by_node_num`
    /// generation token from before the sync could still match after it.
    #[test]
    fn device_sync_bumps_generation_when_it_changes_the_bit() {
        let bus = AdvertBus::new();
        let key = dummy_key(106);
        seed_meshcore_contact(&bus, key);
        let FavouriteOutcome::Protected(snapshot) =
            bus.mark_favourite_if_eligible(key, always_eligible, 350, &[])
        else {
            panic!("expected Protected");
        };
        let generation_after_protect = snapshot.generation;
        backdate_protected_at(&bus, &key);

        // A resync now genuinely reporting unprotected (well outside the
        // grace window) must both clear the bit AND advance generation.
        bus.upsert_contact(
            key,
            "Test".into(),
            1,
            0,
            0,
            now_secs(),
            0,
            0,
            0,
            Vec::new(),
            -1,
            "meshcore",
        );
        assert!(!bus.is_currently_favourited(&key));

        // A revert carrying the OLD (pre-sync) generation must now be a
        // no-op — the sync already changed state out from under it.
        bus.revert_protect(&key, generation_after_protect);
        assert!(
            !bus.is_currently_favourited(&key),
            "a stale-generation revert must not resurrect a state the device sync already changed"
        );

        // A fresh protect must succeed normally afterward.
        assert!(matches!(
            bus.mark_favourite_if_eligible(key, always_eligible, 350, &[]),
            FavouriteOutcome::Protected(_)
        ));
    }

    /// Phase 4 hostile-audit regression (Hostile QA persona, executed
    /// repro): a device-rehydrated favorite (Decision 7 — the device's own
    /// full-record sync already reports it as favorited, so `upsert_contact`
    /// merges the bit without ever going through `decide_favourite`/
    /// `protect_record`) must get a real `protected_at`, not `None`. Before
    /// the fix, `None` sorted as the *oldest possible* eviction candidate
    /// (`unwrap_or(0)`), making a long-standing, device-confirmed favorite
    /// the FIRST thing evicted once the cap is reached — backwards from a
    /// FIFO-by-`protected_at` eviction policy's intent, and defeating
    /// FR-007's "protection survives a restart" guarantee under cap
    /// pressure specifically.
    #[test]
    fn device_rehydrated_favorite_gets_a_protected_at_not_treated_as_ancient() {
        let bus = AdvertBus::new();
        let rehydrated_key = dummy_key(108);
        let older_key = dummy_key(109);
        let new_key = dummy_key(110);

        // `older_key` is protected the normal way, then backdated so it is
        // genuinely the oldest protection.
        seed_meshcore_contact(&bus, older_key);
        assert!(matches!(
            bus.mark_favourite_if_eligible(older_key, always_eligible, 2, &[]),
            FavouriteOutcome::Protected(_)
        ));
        backdate_protected_at(&bus, &older_key);

        // `rehydrated_key` becomes protected purely via a device-reported
        // favorite bit on its first-ever sync (e.g. right after a BBS
        // restart) — decide_favourite/protect_record are never called.
        bus.upsert_contact(
            rehydrated_key,
            "Rehydrated".into(),
            1,
            0,
            0,
            now_secs(),
            1, // flags bit 0 set: device reports this contact as favorited
            0,
            0,
            Vec::new(),
            -1,
            "meshcore",
        );
        assert!(bus.is_currently_favourited(&rehydrated_key));

        let rehydrated_record = bus
            .list()
            .into_iter()
            .find(|r| r.pubkey_hex == hex_encode(&rehydrated_key))
            .expect("rehydrated record exists");
        assert!(
            rehydrated_record.protected_at.is_some(),
            "a device-rehydrated favorite must get a real protected_at, not None"
        );

        // Cap is now full (2/2). A third contact must evict the genuinely
        // older `older_key`, not the just-rehydrated `rehydrated_key`.
        seed_meshcore_contact(&bus, new_key);
        let outcome = bus.mark_favourite_if_eligible(new_key, always_eligible, 2, &[]);
        let FavouriteOutcome::ProtectedWithEviction(_, evicted_pubkey, _) = outcome else {
            panic!("expected ProtectedWithEviction, got {outcome:?}");
        };
        assert_eq!(
            evicted_pubkey, older_key,
            "the genuinely older protection must be evicted, not the just-rehydrated favorite"
        );
    }

    /// Phase 4 hostile-audit regression (Skeptic pass, executed repro,
    /// overturning the round-1 version of the fix above): a merge-driven
    /// bit flip on an ALREADY-TRACKED record (not a brand-new,
    /// just-rehydrated one) must NOT plant a `protected_at`/`unprotected_at`
    /// grace-window token — doing so would let an ordinary passive device
    /// report silently grant itself 5 minutes of immunity from the very
    /// next, genuinely fresher contradicting report from the same device,
    /// contradicting Decision 12a's "a genuine later external change still
    /// eventually takes effect."
    #[test]
    fn passive_resync_flip_on_existing_record_grants_no_grace_window() {
        let bus = AdvertBus::new();
        let key = dummy_key(111);
        seed_meshcore_contact(&bus, key); // record already exists, unprotected

        // The device reports this contact favorited (no local protect() call
        // involved at all — purely a passive resync).
        bus.upsert_contact(
            key,
            "Test".into(),
            1,
            0,
            0,
            now_secs(),
            1,
            0,
            0,
            Vec::new(),
            -1,
            "meshcore",
        );
        assert!(bus.is_currently_favourited(&key));

        // Moments later (still well inside PROTECT_GRACE_SECS), the SAME
        // device reports it unfavorited again. This must take effect
        // immediately — it must not be discarded as "too soon after a
        // recent protect."
        bus.upsert_contact(
            key,
            "Test".into(),
            1,
            0,
            0,
            now_secs(),
            0,
            0,
            0,
            Vec::new(),
            -1,
            "meshcore",
        );
        assert!(
            !bus.is_currently_favourited(&key),
            "a fresher passive resync must not be discarded by a grace window \
             the previous passive resync should never have been granted"
        );
    }

    /// Audit regression (2026-09-02, finding M3): `clear()` must also drop
    /// stale `node_num_index` entries, not just `records`. Updated for
    /// T038d's `clear()` → `clear_unprotected()` rename/refilter: the
    /// record here is deliberately left unprotected, since a protected
    /// one's index entry is now expected to survive (see
    /// `clear_unprotected_preserves_protected_records`).
    #[test]
    fn clear_unprotected_also_clears_node_num_index_for_removed_records() {
        let bus = AdvertBus::new();
        let key = dummy_key(107);
        bus.upsert_meshtastic_node(key, 55, "MT".into(), 0, 0, 0, None, "meshtastic");

        bus.clear_unprotected();

        // With the index cleared, a lookup by the old node_num must find
        // nothing at all — not resolve to a dangling pubkey.
        assert_eq!(
            bus.mark_favourite_if_eligible_by_node_num(55, always_eligible, 350, &[]),
            FavouriteOutcome::NoRecordYet
        );
    }

    /// T038d: a protected record's `node_num_index` entry must also survive
    /// `clear_unprotected()`, not just its `records` entry.
    #[test]
    fn clear_unprotected_preserves_protected_node_num_index_entry() {
        let bus = AdvertBus::new();
        let key = dummy_key(114);
        bus.upsert_meshtastic_node(key, 56, "MT".into(), 0, 0, 0, None, "meshtastic");
        assert!(matches!(
            bus.mark_favourite_if_eligible_by_node_num(56, always_eligible, 350, &[]),
            FavouriteOutcome::Protected(_)
        ));

        bus.clear_unprotected();

        assert!(
            bus.is_currently_favourited_by_node_num(56),
            "a protected node's node_num lookup must still resolve after clear_unprotected()"
        );
    }

    /// Audit regression (2026-09-02, finding H2): `upsert_contact`'s
    /// `last_advert_timestamp` must reject implausible device timestamps
    /// the same way `last_seen_secs` already does — this field was
    /// previously assigned unvalidated.
    #[test]
    fn upsert_contact_validates_last_advert_timestamp_too() {
        let bus = AdvertBus::new();
        let key = dummy_key(108);
        let now = now_secs();

        bus.upsert_contact(
            key,
            "Test".into(),
            1,
            0,
            0,
            now, // device_last_seen: plausible
            0,
            3600, // last_advert_timestamp: boot-relative, implausible
            0,
            Vec::new(),
            -1,
            "meshcore",
        );

        let record = bus
            .list()
            .into_iter()
            .find(|r| r.pubkey_hex == hex_encode(&key))
            .unwrap();
        let ts = i64::from(record.last_advert_timestamp);
        assert!(
            ts >= now - 2 && ts <= now + 2,
            "boot-relative last_advert_timestamp should have fallen back to now, got {ts}"
        );
    }

    /// T010: `upsert_contact` applies the same device-timestamp plausibility
    /// validation `upsert_with_timestamp` does — boot-relative and
    /// far-future values are rejected in favor of wall-clock time.
    #[test]
    fn upsert_contact_rejects_implausible_device_timestamps() {
        let bus = AdvertBus::new();
        let boot_relative_key = dummy_key(100);
        let far_future_key = dummy_key(101);
        let now = now_secs();

        bus.upsert_contact(
            boot_relative_key,
            "BootRelative".into(),
            1,
            0,
            0,
            3600, // 1 hour after epoch — clearly a boot-relative value
            0,
            0,
            0,
            Vec::new(),
            -1,
            "meshcore",
        );
        bus.upsert_contact(
            far_future_key,
            "FarFuture".into(),
            1,
            0,
            0,
            4_000_000_000, // ~year 2096
            0,
            0,
            0,
            Vec::new(),
            -1,
            "meshcore",
        );

        let by_key: std::collections::HashMap<String, i64> = bus
            .list()
            .into_iter()
            .map(|r| (r.pubkey_hex.clone(), r.last_seen_secs))
            .collect();

        let boot_ts = by_key[&hex_encode(&boot_relative_key)];
        let future_ts = by_key[&hex_encode(&far_future_key)];
        assert!(
            boot_ts >= now - 2 && boot_ts <= now + 2,
            "boot-relative device timestamp should have fallen back to now"
        );
        assert!(
            future_ts >= now - 2 && future_ts <= now + 2,
            "far-future device timestamp should have fallen back to now"
        );
    }

    /// T010a (research.md Decision 7a): a stale inbound sync racing an
    /// unconfirmed local protect must not let eviction-exclusion see the
    /// contact as unprotected during the grace window — and must go back to
    /// trusting the wire data once that window has genuinely elapsed.
    #[test]
    fn stale_sync_does_not_defeat_eviction_exclusion_during_grace_window() {
        let bus = AdvertBus::new();
        let key = dummy_key(102);
        seed_meshcore_contact(&bus, key);
        assert!(matches!(
            bus.mark_favourite_if_eligible(key, always_eligible, 350, &[]),
            FavouriteOutcome::Protected(_)
        ));

        // A resync frame reporting the pre-protect (unfavourited) state
        // arrives before the device has actually applied our write.
        bus.upsert_contact(
            key,
            "Test".into(),
            1,
            0,
            0,
            now_secs(),
            0, // reports unprotected
            0,
            0,
            Vec::new(),
            -1,
            "meshcore",
        );

        // Still within PROTECT_GRACE_SECS: eviction must still exclude it,
        // and a second DM must not trigger a redundant re-protect.
        assert_eq!(bus.stalest_pubkey_excluding(&[], "meshcore"), None);
        assert_eq!(
            bus.mark_favourite_if_eligible(key, always_eligible, 350, &[]),
            FavouriteOutcome::AlreadyProtected
        );

        // Once the grace window has genuinely elapsed, the device's report
        // becomes authoritative again — a further sync now actually clears it.
        backdate_protected_at(&bus, &key);
        bus.upsert_contact(
            key,
            "Test".into(),
            1,
            0,
            0,
            now_secs(),
            0,
            0,
            0,
            Vec::new(),
            -1,
            "meshcore",
        );
        assert_eq!(
            bus.stalest_pubkey_excluding(&[], "meshcore"),
            Some(key),
            "past the grace window, the device's own unfavourited report should take effect"
        );
    }

    /// Phase 4 hostile-audit regression (Verifier, claim phase4A-6): the
    /// `DeferredWrite::RemoveFavoriteNode`/`RemoveFavoriteNode` doc comments
    /// cite `stale_sync_does_not_defeat_eviction_exclusion_during_grace_window`
    /// as covering "a lost removal write, then a stale device sync
    /// re-adopting favorited state" — but that test only exercises the
    /// PROTECT-side grace window (a lost protect surviving a stale
    /// unfavourited resync), not this, the opposite direction: a local
    /// `unprotect()` whose native removal write never reached the radio,
    /// followed by a device resync that still reports the old favorited
    /// state. This test exercises that direction directly, closing the gap
    /// the doc comment's citation overstated.
    #[test]
    fn lost_removal_write_lets_stale_favorited_resync_re_adopt_after_grace_window() {
        let bus = AdvertBus::new();
        let key = dummy_key(103);
        seed_meshcore_contact(&bus, key);
        assert!(matches!(
            bus.mark_favourite_if_eligible(key, always_eligible, 350, &[]),
            FavouriteOutcome::Protected(_)
        ));

        // The caller unprotects locally (e.g. eviction, or a manual
        // delete) — the native removal write to the radio is assumed lost
        // (channel full, or a disconnect before it flushed).
        assert!(bus.unprotect(&key).is_some());
        assert!(!bus.is_currently_favourited(&key));

        // Still within the grace window: a resync still reporting the old
        // favorited state must NOT re-adopt it (the local unprotect wins).
        bus.upsert_contact(
            key,
            "Test".into(),
            1,
            0,
            0,
            now_secs(),
            1, // device still reports favorited — hasn't applied our removal yet
            0,
            0,
            Vec::new(),
            -1,
            "meshcore",
        );
        assert!(
            !bus.is_currently_favourited(&key),
            "within the grace window, the local unprotect must not be silently reversed"
        );

        // Once the grace window has genuinely elapsed, the device's report
        // becomes authoritative again — this is Decision 12a's disclosed,
        // accepted tradeoff, not a bug: a permanent latch would block
        // legitimate external re-favoriting.
        backdate_unprotected_at(&bus, &key);
        bus.upsert_contact(
            key,
            "Test".into(),
            1,
            0,
            0,
            now_secs(),
            1,
            0,
            0,
            Vec::new(),
            -1,
            "meshcore",
        );
        assert!(
            bus.is_currently_favourited(&key),
            "past the grace window, a stale-but-still-favorited device report re-adopts protection"
        );
    }

    #[test]
    fn revert_protect_undoes_matching_generation_only() {
        let bus = AdvertBus::new();
        let key = dummy_key(80);
        seed_meshcore_contact(&bus, key);
        let outcome = bus.mark_favourite_if_eligible(key, always_eligible, 350, &[]);
        let FavouriteOutcome::Protected(snapshot) = outcome else {
            panic!("expected Protected");
        };

        // A stale revert (wrong generation) must not touch a newer state.
        bus.revert_protect(&key, snapshot.generation.wrapping_sub(1));
        assert!(bus.is_currently_favourited(&key));

        // The matching generation reverts it.
        bus.revert_protect(&key, snapshot.generation);
        assert!(!bus.is_currently_favourited(&key));
    }

    #[test]
    fn mark_favourite_by_node_num_resolves_through_reverse_index() {
        let bus = AdvertBus::new();
        let key = dummy_key(90);
        bus.upsert_meshtastic_node(key, 42, "MT".into(), 0, 0, 0, None, "meshtastic");
        let outcome = bus.mark_favourite_if_eligible_by_node_num(42, always_eligible, 100, &[]);
        assert!(matches!(outcome, FavouriteOutcome::Protected(_)));
        assert!(bus.is_currently_favourited_by_node_num(42));
        assert_eq!(
            bus.mark_favourite_if_eligible_by_node_num(999, always_eligible, 100, &[]),
            FavouriteOutcome::NoRecordYet
        );
    }

    #[test]
    fn pubkey_by_node_num_resolves_through_reverse_index() {
        let bus = AdvertBus::new();
        let key = dummy_key(91);
        assert_eq!(bus.pubkey_by_node_num(43), None);
        bus.upsert_meshtastic_node(key, 43, "MT".into(), 0, 0, 0, None, "meshtastic");
        assert_eq!(bus.pubkey_by_node_num(43), Some(key));
        assert_eq!(bus.pubkey_by_node_num(999), None);
    }

    #[test]
    fn unprotect_clears_state_and_is_idempotent() {
        let bus = AdvertBus::new();
        let key = dummy_key(91);
        seed_meshcore_contact(&bus, key);
        assert!(matches!(
            bus.mark_favourite_if_eligible(key, always_eligible, 350, &[]),
            FavouriteOutcome::Protected(_)
        ));

        let unprotected = bus.unprotect(&key);
        assert!(unprotected.is_some());
        assert!(!bus.is_currently_favourited(&key));

        // A second unprotect finds nothing left to clear.
        assert!(bus.unprotect(&key).is_none());
    }

    #[test]
    fn all_remaining_favourited_distinguishes_reasons() {
        let bus = AdvertBus::new();
        assert!(!bus.all_remaining_favourited(&[], "meshcore"));

        let key = dummy_key(92);
        seed_meshcore_contact(&bus, key);
        assert!(!bus.all_remaining_favourited(&[], "meshcore"));

        assert!(matches!(
            bus.mark_favourite_if_eligible(key, always_eligible, 350, &[]),
            FavouriteOutcome::Protected(_)
        ));
        assert!(bus.all_remaining_favourited(&[], "meshcore"));
    }
}
