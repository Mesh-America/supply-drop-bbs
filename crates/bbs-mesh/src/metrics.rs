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

use std::sync::atomic::{AtomicU64, Ordering};

use bbs_plugin_api::TransportStats;
use serde::Serialize;
use serde_json::Value;

/// Cumulative reply-delivery counters, updated from the transport's send path
/// and inbound-frame handlers. Shared via `Arc`; all methods take `&self`
/// (atomic, lock-free) so they can be called from any task without coordination.
#[derive(Debug, Default)]
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
}

impl DeliveryStats {
    /// Record an outbound text frame accepted by the command channel.
    /// `attempt` is the 1-based transmission count (1 = first send); anything
    /// greater is a retransmission.
    pub fn on_send(&self, attempt: u8) {
        self.sends_total.fetch_add(1, Ordering::Relaxed);
        if attempt > 1 {
            self.retransmits.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record an outbound text frame dropped before the wire (channel full/closed).
    pub fn on_dropped(&self) {
        self.dropped.fetch_add(1, Ordering::Relaxed);
    }

    /// Record the device's `RESP_CODE_SENT` outcome: `accepted = true` when the
    /// device queued the message, `false` for `MSG_SEND_FAILED` (no route).
    pub fn on_sent_result(&self, accepted: bool) {
        if accepted {
            self.accepted.fetch_add(1, Ordering::Relaxed);
        } else {
            self.failed_no_route.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record an end-to-end delivery confirmation.
    pub fn on_confirmed(&self) {
        self.confirmed.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a reply abandoned after exhausting its retransmission budget.
    pub fn on_gave_up(&self) {
        self.gave_up.fetch_add(1, Ordering::Relaxed);
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
        }
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
}

impl TransportStats for DeliveryStats {
    fn snapshot(&self) -> Value {
        // Snapshot is a flat struct of plain numbers; serialisation cannot fail.
        serde_json::to_value(DeliveryStats::snapshot(self))
            .unwrap_or_else(|_| Value::Object(Default::default()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_sends_and_retransmits() {
        let s = DeliveryStats::default();
        s.on_send(1); // first send
        s.on_send(2); // retransmission
        s.on_send(3); // retransmission
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
        s.on_send(1);
        let v = TransportStats::snapshot(&s);
        assert_eq!(v["sends_total"], 1);
        assert!(v["confirm_rate"].is_null());
    }
}
