//! Reply-delivery counters for the MeshCore transport.
//!
//! # Why
//!
//! The companion link is fire-and-forget and the return path on a multi-hop
//! mesh is lossy (see the `send_tracker` module). `SendTracker` already turns
//! the device's `RESP_CODE_SENT` / `PUSH_CODE_SEND_CONFIRMED` signals into
//! at-least-once delivery, but those signals were previously consumed and
//! discarded — the operator had no way to see how healthy the link actually is.
//!
//! [`DeliveryStats`] taps the same signal points and keeps cumulative counters
//! so the admin UI can show round-trip success on the radio link. It is pure
//! counting — no clock, no I/O, no influence on delivery behaviour — and works
//! regardless of whether retransmission is enabled (it counts every outbound
//! text frame at the point it is handed to the companion client, not via the
//! tracker, which only records sends when retries are on).
//!
//! # Lifetime
//!
//! Counters are cumulative since process start; they reset on restart. A
//! persisted time-series for trend analysis is a separate, later layer — this
//! module is the live snapshot only.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use bbs_plugin_api::TransportStats;
use serde::Serialize;
use serde_json::Value;

/// Cap on the number of per-node delivery records kept in memory. When the map
/// is full and a new node appears, the least-recently-active record is evicted —
/// the operator cares about nodes currently exchanging traffic, not stale ones.
const MAX_TRACKED_NODES: usize = 256;

/// Cap on how many per-node records are serialised in a snapshot (worst-first).
const MAX_NODES_IN_SNAPSHOT: usize = 50;

/// Cap on retained history samples. At one sample per minute this is 8 hours of
/// trend — enough to watch reliability change while tuning a link, without the
/// memory or payload of a full day. History is in-memory only and resets on
/// restart; durable persistence is a separate layer.
const MAX_HISTORY_SAMPLES: usize = 480;

/// Per-node delivery counters. Plain integers — guarded by the `nodes` mutex,
/// not atomics, since they are only touched under that lock.
#[derive(Debug, Default, Clone)]
struct NodeCounters {
    sends: u64,
    accepted: u64,
    failed_no_route: u64,
    confirmed: u64,
    gave_up: u64,
    latency_count: u64,
    latency_sum_ms: u64,
    latency_min_ms: u64,
    latency_max_ms: u64,
}

impl NodeCounters {
    fn record_latency(&mut self, ms: u64) {
        if self.latency_count == 0 || ms < self.latency_min_ms {
            self.latency_min_ms = ms;
        }
        if ms > self.latency_max_ms {
            self.latency_max_ms = ms;
        }
        self.latency_count += 1;
        self.latency_sum_ms += ms;
    }
}

/// A per-node record plus the last time it was touched (for stale eviction).
#[derive(Debug)]
struct NodeEntry {
    counters: NodeCounters,
    last_update: Instant,
}

/// Cumulative reply-delivery counters, updated from the transport's send path
/// and inbound-frame handlers. Shared via `Arc`. The global counters are
/// lock-free atomics; the per-node map is behind a short-lived mutex (mesh
/// traffic is low-frequency, so contention is negligible).
#[derive(Debug)]
pub struct DeliveryStats {
    /// Outbound text frames handed to the companion client (first sends and
    /// retransmissions both count). The denominator for "did the device even
    /// take it".
    sends_total: AtomicU64,
    /// Subset of `sends_total` that were retransmissions (attempt > 1).
    retransmits: AtomicU64,
    /// Outbound text frames dropped before the wire because the command channel
    /// was full or closed (never reached the device).
    dropped: AtomicU64,
    /// Device accepted a send for delivery — `RESP_CODE_SENT` with a non-zero
    /// ACK CRC, or a flood send.
    accepted: AtomicU64,
    /// Device reported `MSG_SEND_FAILED` (ACK CRC 0, not flood): no route /
    /// unknown contact. The send never left the device.
    failed_no_route: AtomicU64,
    /// End-to-end delivery confirmations (`PUSH_CODE_SEND_CONFIRMED`): the
    /// destination acknowledged receipt.
    confirmed: AtomicU64,
    /// Replies abandoned after exhausting the retransmission budget without a
    /// confirmation.
    gave_up: AtomicU64,
    /// Number of round-trip latency samples. A sample is recorded when a
    /// confirmation correlates to a tracked send, so this is only populated when
    /// retransmission tracking is enabled (`reply_max_attempts > 1`).
    latency_count: AtomicU64,
    /// Sum of round-trip latencies in milliseconds, for the average.
    latency_sum_ms: AtomicU64,
    /// Smallest round-trip latency seen, in milliseconds. Initialised to
    /// `u64::MAX` so the first `fetch_min` wins; ignore unless `latency_count > 0`.
    latency_min_ms: AtomicU64,
    /// Largest round-trip latency seen, in milliseconds.
    latency_max_ms: AtomicU64,
    /// Per-destination delivery counters, keyed by 6-byte pubkey prefix. Bounded
    /// at [`MAX_TRACKED_NODES`]; the stalest entry is evicted when full.
    nodes: Mutex<HashMap<[u8; 6], NodeEntry>>,
    /// Rolling history of cumulative-counter snapshots, appended on a timer.
    /// Bounded at [`MAX_HISTORY_SAMPLES`]; consumers derive per-interval rates
    /// from the deltas between consecutive samples.
    history: Mutex<VecDeque<DeliverySample>>,
}

impl Default for DeliveryStats {
    fn default() -> Self {
        Self {
            sends_total: AtomicU64::new(0),
            retransmits: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            accepted: AtomicU64::new(0),
            failed_no_route: AtomicU64::new(0),
            confirmed: AtomicU64::new(0),
            gave_up: AtomicU64::new(0),
            latency_count: AtomicU64::new(0),
            latency_sum_ms: AtomicU64::new(0),
            // Sentinel so the first recorded sample becomes the minimum.
            latency_min_ms: AtomicU64::new(u64::MAX),
            latency_max_ms: AtomicU64::new(0),
            nodes: Mutex::new(HashMap::new()),
            history: Mutex::new(VecDeque::new()),
        }
    }
}

impl DeliveryStats {
    /// Update one node's counters under the map lock, evicting the stalest entry
    /// first if the map is full and this node is new.
    fn with_node<F: FnOnce(&mut NodeCounters)>(&self, prefix: [u8; 6], f: F) {
        let mut map = self.nodes.lock().expect("nodes mutex poisoned");
        if !map.contains_key(&prefix) && map.len() >= MAX_TRACKED_NODES {
            if let Some(stalest) = map
                .iter()
                .min_by_key(|(_, e)| e.last_update)
                .map(|(k, _)| *k)
            {
                map.remove(&stalest);
            }
        }
        let entry = map.entry(prefix).or_insert_with(|| NodeEntry {
            counters: NodeCounters::default(),
            last_update: Instant::now(),
        });
        f(&mut entry.counters);
        entry.last_update = Instant::now();
    }

    /// Record an outbound text frame accepted by the command channel, addressed
    /// to `prefix`. `attempt` is the 1-based transmission count (1 = first send);
    /// anything greater is a retransmission.
    pub fn on_send(&self, prefix: [u8; 6], attempt: u8) {
        self.sends_total.fetch_add(1, Ordering::Relaxed);
        if attempt > 1 {
            self.retransmits.fetch_add(1, Ordering::Relaxed);
        }
        self.with_node(prefix, |c| c.sends += 1);
    }

    /// Record an outbound text frame dropped before the wire (channel full/closed).
    pub fn on_dropped(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// Record the device's `RESP_CODE_SENT` outcome (global): `accepted = true`
    /// when the device queued the message, `false` for `MSG_SEND_FAILED`.
    pub fn on_sent_result(&self, accepted: bool) {
        if accepted {
            self.accepted.fetch_add(1, Ordering::Relaxed);
        } else {
            self.failed_no_route.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Attribute a `RESP_CODE_SENT` outcome to a specific node. Driven by the
    /// retransmission tracker's correlation, so it is only called when retries
    /// are enabled (the global [`Self::on_sent_result`] is always called).
    pub fn on_node_sent_result(&self, prefix: [u8; 6], accepted: bool) {
        self.with_node(prefix, |c| {
            if accepted {
                c.accepted += 1;
            } else {
                c.failed_no_route += 1;
            }
        });
    }

    /// Record an end-to-end delivery confirmation (global).
    pub fn on_confirmed(&self) {
        self.confirmed.fetch_add(1, Ordering::Relaxed);
    }

    /// Attribute a confirmation and its round-trip latency to a specific node.
    pub fn on_node_confirmed(&self, prefix: [u8; 6], latency_ms: u64) {
        self.with_node(prefix, |c| {
            c.confirmed += 1;
            c.record_latency(latency_ms);
        });
    }

    /// Record a reply abandoned after exhausting its retransmission budget (global).
    pub fn on_gave_up(&self) {
        self.gave_up.fetch_add(1, Ordering::Relaxed);
    }

    /// Attribute a give-up to a specific node.
    pub fn on_node_gave_up(&self, prefix: [u8; 6]) {
        self.with_node(prefix, |c| c.gave_up += 1);
    }

    /// Append a history sample of the current cumulative counters, stamped with
    /// `ts` (Unix seconds). Called on a timer; the oldest sample is dropped once
    /// the buffer is full. Consumers diff consecutive samples for interval rates.
    pub fn sample(&self, ts: u64) {
        let s = DeliverySample {
            ts,
            sends_total: self.sends_total.load(Ordering::Relaxed),
            accepted: self.accepted.load(Ordering::Relaxed),
            failed_no_route: self.failed_no_route.load(Ordering::Relaxed),
            confirmed: self.confirmed.load(Ordering::Relaxed),
            latency_count: self.latency_count.load(Ordering::Relaxed),
            latency_sum_ms: self.latency_sum_ms.load(Ordering::Relaxed),
        };
        let mut h = self.history.lock().expect("history mutex poisoned");
        if h.len() >= MAX_HISTORY_SAMPLES {
            h.pop_front();
        }
        h.push_back(s);
    }

    /// The retained history samples, oldest first.
    pub fn history(&self) -> Vec<DeliverySample> {
        self.history
            .lock()
            .expect("history mutex poisoned")
            .iter()
            .cloned()
            .collect()
    }

    /// Record a round-trip latency sample (send → end-to-end confirmation), in
    /// milliseconds. Only called when a confirmation correlates to a tracked
    /// send, so it requires retransmission tracking to be enabled.
    pub fn record_latency(&self, ms: u64) {
        self.latency_count.fetch_add(1, Ordering::Relaxed);
        self.latency_sum_ms.fetch_add(ms, Ordering::Relaxed);
        self.latency_min_ms.fetch_min(ms, Ordering::Relaxed);
        self.latency_max_ms.fetch_max(ms, Ordering::Relaxed);
    }

    /// Take a consistent-enough point-in-time snapshot for serialisation.
    ///
    /// Counters are read independently (no global lock), so a snapshot taken
    /// mid-update may be off by one between fields. That is fine for a metrics
    /// display and avoids any contention on the hot send path.
    pub fn snapshot(&self) -> DeliveryStatsSnapshot {
        let sends_total = self.sends_total.load(Ordering::Relaxed);
        let retransmits = self.retransmits.load(Ordering::Relaxed);
        let dropped = self.dropped.load(Ordering::Relaxed);
        let accepted = self.accepted.load(Ordering::Relaxed);
        let failed_no_route = self.failed_no_route.load(Ordering::Relaxed);
        let confirmed = self.confirmed.load(Ordering::Relaxed);
        let gave_up = self.gave_up.load(Ordering::Relaxed);

        // Confirm rate: of the sends the device accepted, how many were
        // confirmed end-to-end. `None` until there is something to divide.
        let confirm_rate = ratio(confirmed, accepted);
        // Route-failure rate: of the sends the device gave a verdict on, how
        // many it could not route.
        let route_failure_rate = ratio(failed_no_route, accepted + failed_no_route);

        // Round-trip latency. Only meaningful once at least one confirmation
        // correlated to a tracked send.
        let latency_count = self.latency_count.load(Ordering::Relaxed);
        let latency_sum_ms = self.latency_sum_ms.load(Ordering::Relaxed);
        let avg_latency_ms =
            (latency_count > 0).then(|| latency_sum_ms as f64 / latency_count as f64);
        let min_latency_ms =
            (latency_count > 0).then(|| self.latency_min_ms.load(Ordering::Relaxed));
        let max_latency_ms =
            (latency_count > 0).then(|| self.latency_max_ms.load(Ordering::Relaxed));

        let (per_node, nodes_tracked) = self.node_snapshots();

        DeliveryStatsSnapshot {
            sends_total,
            retransmits,
            dropped,
            accepted,
            failed_no_route,
            confirmed,
            gave_up,
            confirm_rate,
            route_failure_rate,
            latency_count,
            avg_latency_ms,
            min_latency_ms,
            max_latency_ms,
            nodes_tracked,
            per_node,
        }
    }

    /// Build the per-node snapshot list, sorted worst-first (lowest confirm rate,
    /// then highest volume) and capped at [`MAX_NODES_IN_SNAPSHOT`]. Returns the
    /// capped list and the total number of nodes tracked.
    fn node_snapshots(&self) -> (Vec<NodeDeliverySnapshot>, usize) {
        let map = self.nodes.lock().expect("nodes mutex poisoned");
        let total = map.len();
        let mut rows: Vec<NodeDeliverySnapshot> = map
            .iter()
            .map(|(prefix, e)| {
                let c = &e.counters;
                let prefix_hex = prefix.iter().map(|b| format!("{b:02x}")).collect();
                let avg_latency_ms =
                    (c.latency_count > 0).then(|| c.latency_sum_ms as f64 / c.latency_count as f64);
                NodeDeliverySnapshot {
                    prefix: prefix_hex,
                    sends: c.sends,
                    accepted: c.accepted,
                    failed_no_route: c.failed_no_route,
                    confirmed: c.confirmed,
                    gave_up: c.gave_up,
                    confirm_rate: ratio(c.confirmed, c.accepted),
                    avg_latency_ms,
                }
            })
            .collect();
        // Worst link first: lowest confirm rate (a node with no confirmations
        // sorts as 0.0), then highest send volume so busy bad links lead.
        rows.sort_by(|a, b| {
            let ra = a.confirm_rate.unwrap_or(0.0);
            let rb = b.confirm_rate.unwrap_or(0.0);
            ra.partial_cmp(&rb)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(b.sends.cmp(&a.sends))
        });
        rows.truncate(MAX_NODES_IN_SNAPSHOT);
        (rows, total)
    }
}

/// Ratio of `num / den` as a 0.0–1.0 fraction, or `None` when `den == 0`.
fn ratio(num: u64, den: u64) -> Option<f64> {
    (den > 0).then(|| num as f64 / den as f64)
}

/// A serialisable point-in-time view of [`DeliveryStats`].
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DeliveryStatsSnapshot {
    /// Outbound text frames handed to the device (first sends + retransmissions).
    pub sends_total: u64,
    /// Retransmissions among `sends_total`.
    pub retransmits: u64,
    /// Outbound frames dropped before the wire (channel full/closed).
    pub dropped: u64,
    /// Sends the device accepted for delivery.
    pub accepted: u64,
    /// Sends the device could not route (`MSG_SEND_FAILED`).
    pub failed_no_route: u64,
    /// End-to-end delivery confirmations received.
    pub confirmed: u64,
    /// Replies abandoned after exhausting all retransmission attempts.
    pub gave_up: u64,
    /// `confirmed / accepted`, or `null` when nothing has been accepted yet.
    pub confirm_rate: Option<f64>,
    /// `failed_no_route / (accepted + failed_no_route)`, or `null` when the
    /// device has not reported on any send yet.
    pub route_failure_rate: Option<f64>,
    /// Number of round-trip latency samples behind the latency figures. Zero
    /// when retransmission tracking is disabled.
    pub latency_count: u64,
    /// Mean send→confirmation round-trip in milliseconds, or `null` when there
    /// are no samples.
    pub avg_latency_ms: Option<f64>,
    /// Fastest round-trip in milliseconds, or `null` when there are no samples.
    pub min_latency_ms: Option<u64>,
    /// Slowest round-trip in milliseconds, or `null` when there are no samples.
    pub max_latency_ms: Option<u64>,
    /// Total number of nodes currently tracked (may exceed `per_node.len()`,
    /// which is capped).
    pub nodes_tracked: usize,
    /// Per-node delivery breakdown, worst link first, capped at the 50 worst.
    /// Populated from the retransmission tracker, so it reflects reply traffic
    /// when `reply_max_attempts > 1`.
    pub per_node: Vec<NodeDeliverySnapshot>,
}

/// Per-destination delivery view: how well one node's replies are getting through.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct NodeDeliverySnapshot {
    /// Destination pubkey prefix, 12 hex chars (the first 6 bytes).
    pub prefix: String,
    /// Outbound frames addressed to this node (first sends + retransmissions).
    pub sends: u64,
    /// Sends the device accepted for delivery to this node.
    pub accepted: u64,
    /// Sends to this node the device could not route.
    pub failed_no_route: u64,
    /// End-to-end confirmations from this node.
    pub confirmed: u64,
    /// Replies to this node abandoned after exhausting retransmissions.
    pub gave_up: u64,
    /// `confirmed / accepted` for this node, or `null` when nothing accepted yet.
    pub confirm_rate: Option<f64>,
    /// Mean round-trip to this node in milliseconds, or `null` with no samples.
    pub avg_latency_ms: Option<f64>,
}

/// One point in the delivery history: cumulative counters at a moment in time.
/// Consumers derive per-interval rates from the deltas between samples.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DeliverySample {
    /// Unix timestamp (seconds) the sample was taken.
    pub ts: u64,
    /// Cumulative frames sent at this point.
    pub sends_total: u64,
    /// Cumulative device-accepted sends.
    pub accepted: u64,
    /// Cumulative no-route failures.
    pub failed_no_route: u64,
    /// Cumulative end-to-end confirmations.
    pub confirmed: u64,
    /// Cumulative latency sample count (for deriving interval average latency).
    pub latency_count: u64,
    /// Cumulative latency sum in milliseconds.
    pub latency_sum_ms: u64,
}

impl TransportStats for DeliveryStats {
    fn snapshot(&self) -> Value {
        // Snapshot is a flat struct of plain numbers; serialisation cannot fail.
        serde_json::to_value(DeliveryStats::snapshot(self))
            .unwrap_or_else(|_| Value::Object(Default::default()))
    }

    fn history(&self) -> Value {
        serde_json::to_value(DeliveryStats::history(self))
            .unwrap_or_else(|_| Value::Array(Vec::new()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P1: [u8; 6] = [1, 1, 1, 1, 1, 1];
    const P2: [u8; 6] = [2, 2, 2, 2, 2, 2];

    #[test]
    fn counts_sends_and_retransmits() {
        let s = DeliveryStats::default();
        s.on_send(P1, 1); // first send
        s.on_send(P1, 2); // retransmission
        s.on_send(P1, 3); // retransmission
        let snap = s.snapshot();
        assert_eq!(snap.sends_total, 3);
        assert_eq!(snap.retransmits, 2);
    }

    #[test]
    fn rates_are_none_until_there_is_data() {
        let snap = DeliveryStats::default().snapshot();
        assert_eq!(snap.confirm_rate, None);
        assert_eq!(snap.route_failure_rate, None);
    }

    #[test]
    fn confirm_and_route_failure_rates() {
        let s = DeliveryStats::default();
        // 4 accepted, 1 no-route → route failure 1/5.
        for _ in 0..4 {
            s.on_sent_result(true);
        }
        s.on_sent_result(false);
        // 3 of the 4 accepted were confirmed.
        for _ in 0..3 {
            s.on_confirmed();
        }
        let snap = s.snapshot();
        assert_eq!(snap.accepted, 4);
        assert_eq!(snap.failed_no_route, 1);
        assert_eq!(snap.confirmed, 3);
        assert_eq!(snap.confirm_rate, Some(0.75));
        assert_eq!(snap.route_failure_rate, Some(0.2));
    }

    #[test]
    fn latency_tracks_avg_min_max() {
        let s = DeliveryStats::default();
        assert_eq!(s.snapshot().avg_latency_ms, None, "no samples yet");
        s.record_latency(100);
        s.record_latency(300);
        s.record_latency(200);
        let snap = s.snapshot();
        assert_eq!(snap.latency_count, 3);
        assert_eq!(snap.avg_latency_ms, Some(200.0));
        assert_eq!(snap.min_latency_ms, Some(100));
        assert_eq!(snap.max_latency_ms, Some(300));
    }

    #[test]
    fn dropped_and_gave_up_are_counted() {
        let s = DeliveryStats::default();
        s.on_dropped();
        s.on_gave_up();
        s.on_gave_up();
        let snap = s.snapshot();
        assert_eq!(snap.dropped, 1);
        assert_eq!(snap.gave_up, 2);
    }

    #[test]
    fn transport_stats_trait_emits_object() {
        let s = DeliveryStats::default();
        s.on_send(P1, 1);
        let v = TransportStats::snapshot(&s);
        assert_eq!(v["sends_total"], 1);
        assert!(v["confirm_rate"].is_null());
    }

    #[test]
    fn per_node_breakdown_sorts_worst_first() {
        let s = DeliveryStats::default();
        // Node 1: healthy — 4 accepted, 4 confirmed.
        for _ in 0..4 {
            s.on_send(P1, 1);
            s.on_node_sent_result(P1, true);
            s.on_node_confirmed(P1, 150);
        }
        // Node 2: bad — 4 accepted, only 1 confirmed.
        for _ in 0..4 {
            s.on_send(P2, 1);
            s.on_node_sent_result(P2, true);
        }
        s.on_node_confirmed(P2, 900);

        let snap = s.snapshot();
        assert_eq!(snap.nodes_tracked, 2);
        assert_eq!(snap.per_node.len(), 2);
        // Worst (lowest confirm rate) is listed first.
        assert_eq!(snap.per_node[0].prefix, "020202020202");
        assert_eq!(snap.per_node[0].confirm_rate, Some(0.25));
        assert_eq!(snap.per_node[0].avg_latency_ms, Some(900.0));
        assert_eq!(snap.per_node[1].prefix, "010101010101");
        assert_eq!(snap.per_node[1].confirm_rate, Some(1.0));
    }

    #[test]
    fn history_samples_accumulate_and_are_bounded() {
        let s = DeliveryStats::default();
        s.on_send(P1, 1);
        s.sample(100);
        s.on_send(P1, 1);
        s.sample(160);
        let h = s.history();
        assert_eq!(h.len(), 2);
        assert_eq!(h[0].ts, 100);
        assert_eq!(h[0].sends_total, 1);
        assert_eq!(h[1].ts, 160);
        assert_eq!(h[1].sends_total, 2);

        // Buffer is bounded: pushing past the cap drops the oldest.
        for i in 0..(MAX_HISTORY_SAMPLES + 5) {
            s.sample(1000 + i as u64);
        }
        let h = s.history();
        assert_eq!(h.len(), MAX_HISTORY_SAMPLES);
        // Oldest retained sample is newer than the very first ones we pushed.
        assert!(h.first().unwrap().ts > 100);
    }

    #[test]
    fn per_node_map_is_bounded_and_evicts_stalest() {
        let s = DeliveryStats::default();
        // Insert more than the cap; the map must not grow without bound.
        for i in 0..(MAX_TRACKED_NODES + 10) {
            let b = (i % 256) as u8;
            let b2 = (i / 256) as u8;
            s.on_send([b, b2, 0, 0, 0, 0], 1);
        }
        let snap = s.snapshot();
        assert_eq!(snap.nodes_tracked, MAX_TRACKED_NODES);
        assert!(snap.per_node.len() <= MAX_NODES_IN_SNAPSHOT);
    }
}
