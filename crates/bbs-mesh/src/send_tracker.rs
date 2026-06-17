//! Delivery tracking and retransmission for outbound text replies.
//!
//! # Why
//!
//! The MeshCore companion link is fire-and-forget: the host writes a
//! `CMD_SEND_TXT_MSG` and moves on. On a multi-hop mesh the *return* path is
//! lossy, so a reply (or its end-to-end ACK) is routinely dropped — the user
//! sends a command, the BBS processes it, but the answer never arrives and the
//! BBS looks unresponsive.
//!
//! The firmware actually gives us enough to do better, we just weren't using it:
//!
//! - `RESP_CODE_SENT` (`InboundFrame::Sent`) is the device's immediate reply to
//!   a send. It carries an `expected_ack` CRC identifying the message and a
//!   `timeout_ms` hint for how long to wait for end-to-end delivery. A CRC of 0
//!   means the device could not even queue it (no route / unknown contact).
//! - `PUSH_CODE_SEND_CONFIRMED` (`InboundFrame::SendConfirmed`) arrives later
//!   with the same CRC once the destination has acknowledged receipt.
//!
//! [`SendTracker`] turns those signals into at-least-once delivery: every text
//! send is recorded; if no `SendConfirmed` arrives before the deadline, the
//! message is retransmitted (up to a configured attempt cap). A delivered
//! message that loses only its `SendConfirmed` may be retransmitted once more —
//! a duplicate reply is far less harmful than silence, and inbound commands are
//! already deduplicated elsewhere.
//!
//! # Correlation
//!
//! `RESP_CODE_SENT` is the device's synchronous response to `CMD_SEND_TXT_MSG`
//! and is emitted in the same order the sends were written to the wire. So a
//! FIFO of "sent, awaiting RESP_CODE_SENT" records correlates 1:1 with incoming
//! `Sent` frames by order; once a CRC is known the record moves to a CRC-keyed
//! map to await `SendConfirmed`. All text sends must be recorded here in send
//! order for the correlation to hold (see `enqueue_text` in `transport.rs`).
//!
//! This module is pure state — no I/O, no clock of its own (the caller passes
//! `Instant`s) — so the state machine is unit-tested directly.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

/// Tunables for reply retransmission.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RetryConfig {
    /// Total transmissions per message, including the first. `1` disables
    /// retransmission entirely (record-and-forget, only used for observability).
    pub max_attempts: u8,
    /// Floor for the per-attempt ACK wait, regardless of the device hint.
    pub min_timeout: Duration,
    /// Ceiling for the per-attempt ACK wait (guards against absurd hints).
    pub max_timeout: Duration,
}

impl RetryConfig {
    /// Clamp the device's `timeout_ms` hint into the configured window.
    fn ack_wait(&self, timeout_ms: u32) -> Duration {
        Duration::from_millis(u64::from(timeout_ms)).clamp(self.min_timeout, self.max_timeout)
    }
}

/// A text send awaiting a delivery outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PendingSend {
    pub prefix: [u8; 6],
    pub text: String,
    pub txt_type: u8,
    /// 1-based count of transmissions made so far (1 = first send).
    pub attempt: u8,
    /// When this attempt was written to the wire. Used to measure round-trip
    /// latency when a `SendConfirmed` arrives. A retransmission carries the time
    /// of the retransmission, so latency is from the delivered attempt.
    pub sent_at: Instant,
    /// When the wait for this attempt's `SendConfirmed` expires. Only meaningful
    /// once the record is in `awaiting_ack`; a placeholder while in
    /// `awaiting_sent`.
    pub deadline: Instant,
}

/// Outcome of an `on_sent` correlation.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum SentOutcome {
    /// Device accepted the message; now awaiting end-to-end confirmation.
    Accepted,
    /// Device reported `MSG_SEND_FAILED` (CRC 0). The record is returned so the
    /// caller can retransmit immediately if attempts remain.
    Failed(PendingSend),
    /// A `Sent` frame arrived with no recorded send to correlate it to.
    Spurious,
}

/// FIFO + CRC-keyed tracker of in-flight text sends. See module docs.
pub(crate) struct SendTracker {
    cfg: RetryConfig,
    /// Sent, awaiting `RESP_CODE_SENT`, in send order.
    awaiting_sent: VecDeque<PendingSend>,
    /// CRC (`expected_ack`) → record awaiting `SendConfirmed` or timeout.
    awaiting_ack: HashMap<u32, PendingSend>,
}

impl SendTracker {
    pub fn new(cfg: RetryConfig) -> Self {
        Self {
            cfg,
            awaiting_sent: VecDeque::new(),
            awaiting_ack: HashMap::new(),
        }
    }

    /// Whether retransmission is enabled at all.
    pub fn retries_enabled(&self) -> bool {
        self.cfg.max_attempts > 1
    }

    /// Record a text send that was just written to the wire. `attempt` is 1 for
    /// a first send, or the previous attempt + 1 for a retransmission.
    pub fn record(
        &mut self,
        prefix: [u8; 6],
        text: String,
        txt_type: u8,
        attempt: u8,
        now: Instant,
    ) {
        self.awaiting_sent.push_back(PendingSend {
            prefix,
            text,
            txt_type,
            attempt,
            sent_at: now,
            deadline: now, // replaced when the CRC + timeout arrive
        });
    }

    /// Correlate a `RESP_CODE_SENT` (`expected_ack` CRC, `timeout_ms` hint) with
    /// the oldest un-correlated send.
    pub fn on_sent(&mut self, crc: u32, timeout_ms: u32, now: Instant) -> SentOutcome {
        let Some(mut rec) = self.awaiting_sent.pop_front() else {
            return SentOutcome::Spurious;
        };
        if crc == 0 {
            return SentOutcome::Failed(rec);
        }
        rec.deadline = now + self.cfg.ack_wait(timeout_ms);
        // A CRC collision (same message retransmitted) simply refreshes the entry.
        self.awaiting_ack.insert(crc, rec);
        SentOutcome::Accepted
    }

    /// Mark a message delivered (`SendConfirmed`). Returns the tracked record
    /// (so the caller can read its prefix and `sent_at` for per-node and latency
    /// metrics), or `None` for an unknown/duplicate CRC.
    pub fn on_confirmed(&mut self, crc: u32) -> Option<PendingSend> {
        self.awaiting_ack.remove(&crc)
    }

    /// Remove and return all entries whose ACK deadline has passed, split into
    /// those that should be retransmitted (attempts remain) and those that have
    /// exhausted their attempts (caller logs the give-up).
    pub fn collect_due(&mut self, now: Instant) -> DueSends {
        let due_crcs: Vec<u32> = self
            .awaiting_ack
            .iter()
            .filter(|(_, r)| r.deadline <= now)
            .map(|(crc, _)| *crc)
            .collect();
        let mut to_retry = Vec::new();
        let mut gave_up = Vec::new();
        for crc in due_crcs {
            if let Some(rec) = self.awaiting_ack.remove(&crc) {
                if rec.attempt < self.cfg.max_attempts {
                    to_retry.push(rec);
                } else {
                    gave_up.push(rec);
                }
            }
        }
        DueSends { to_retry, gave_up }
    }

    #[cfg(test)]
    fn in_flight(&self) -> usize {
        self.awaiting_sent.len() + self.awaiting_ack.len()
    }
}

/// Result of [`SendTracker::collect_due`].
pub(crate) struct DueSends {
    /// Records to retransmit (resend with `attempt + 1`).
    pub to_retry: Vec<PendingSend>,
    /// Records abandoned after exhausting their attempt budget.
    pub gave_up: Vec<PendingSend>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(max_attempts: u8) -> RetryConfig {
        RetryConfig {
            max_attempts,
            min_timeout: Duration::from_secs(3),
            max_timeout: Duration::from_secs(30),
        }
    }

    fn rec(t: &mut SendTracker, txt: &str, attempt: u8, now: Instant) {
        t.record([1, 2, 3, 4, 5, 6], txt.to_owned(), 0, attempt, now);
    }

    #[test]
    fn confirmed_before_deadline_is_not_retried() {
        let now = Instant::now();
        let mut t = SendTracker::new(cfg(3));
        rec(&mut t, "hi", 1, now);
        assert_eq!(t.on_sent(0xABCD, 5_000, now), SentOutcome::Accepted);
        assert!(t.on_confirmed(0xABCD).is_some());
        // Past the deadline, nothing is due — it was delivered.
        let due = t.collect_due(now + Duration::from_secs(60));
        assert!(due.to_retry.is_empty() && due.gave_up.is_empty());
        assert_eq!(t.in_flight(), 0);
    }

    #[test]
    fn missing_confirm_retransmits_until_cap() {
        let now = Instant::now();
        let mut t = SendTracker::new(cfg(3));
        // Attempt 1.
        rec(&mut t, "hi", 1, now);
        t.on_sent(0x1, 5_000, now);
        // Deadline ~5s later; before that, nothing due.
        assert!(t
            .collect_due(now + Duration::from_secs(4))
            .to_retry
            .is_empty());
        let due = t.collect_due(now + Duration::from_secs(6));
        assert_eq!(due.to_retry.len(), 1);
        assert_eq!(
            due.to_retry[0].attempt, 1,
            "returns the prior attempt count"
        );

        // Caller retransmits as attempt 2.
        let t2 = now + Duration::from_secs(6);
        rec(&mut t, "hi", 2, t2);
        t.on_sent(0x2, 5_000, t2);
        let due = t.collect_due(t2 + Duration::from_secs(6));
        assert_eq!(due.to_retry.len(), 1);

        // Attempt 3 is the cap → next timeout gives up, no further retry.
        let t3 = t2 + Duration::from_secs(6);
        rec(&mut t, "hi", 3, t3);
        t.on_sent(0x3, 5_000, t3);
        let due = t.collect_due(t3 + Duration::from_secs(6));
        assert!(due.to_retry.is_empty(), "attempt 3 hit the cap");
        assert_eq!(due.gave_up.len(), 1);
        assert_eq!(t.in_flight(), 0);
    }

    #[test]
    fn send_failed_crc_zero_returns_record() {
        let now = Instant::now();
        let mut t = SendTracker::new(cfg(3));
        rec(&mut t, "hi", 1, now);
        match t.on_sent(0, 0, now) {
            SentOutcome::Failed(r) => assert_eq!(r.attempt, 1),
            other => panic!("expected Failed, got {other:?}"),
        }
        assert_eq!(t.in_flight(), 0, "failed send is not left dangling");
    }

    #[test]
    fn fifo_correlation_matches_sends_in_order() {
        let now = Instant::now();
        let mut t = SendTracker::new(cfg(2));
        rec(&mut t, "first", 1, now);
        rec(&mut t, "second", 1, now);
        // Sent frames arrive in send order.
        t.on_sent(0xAA, 5_000, now);
        t.on_sent(0xBB, 5_000, now);
        // Confirm the second; the first is still in flight.
        assert!(t.on_confirmed(0xBB).is_some());
        let due = t.collect_due(now + Duration::from_secs(10));
        assert_eq!(due.to_retry.len(), 1);
        assert_eq!(due.to_retry[0].text, "first");
    }

    #[test]
    fn confirmed_returns_record_with_send_time_and_prefix() {
        let now = Instant::now();
        let mut t = SendTracker::new(cfg(3));
        t.record([9, 9, 9, 9, 9, 9], "hi".to_owned(), 0, 1, now);
        // on_sent happens slightly later but must not overwrite the wire-send time.
        t.on_sent(0x55, 5_000, now + Duration::from_millis(1));
        let rec = t.on_confirmed(0x55).expect("tracked send");
        assert_eq!(rec.prefix, [9, 9, 9, 9, 9, 9]);
        assert_eq!(
            rec.sent_at, now,
            "sent_at is the record() time, used to measure round-trip latency"
        );
    }

    #[test]
    fn spurious_sent_without_record_is_reported() {
        let now = Instant::now();
        let mut t = SendTracker::new(cfg(3));
        assert_eq!(t.on_sent(0x1, 5_000, now), SentOutcome::Spurious);
    }

    #[test]
    fn timeout_hint_is_clamped() {
        let now = Instant::now();
        let mut t = SendTracker::new(cfg(3));
        // A tiny hint is floored to min_timeout (3s), so not due at 1s.
        rec(&mut t, "hi", 1, now);
        t.on_sent(0x1, 10, now);
        assert!(t
            .collect_due(now + Duration::from_secs(1))
            .to_retry
            .is_empty());
        assert_eq!(
            t.collect_due(now + Duration::from_secs(4)).to_retry.len(),
            1
        );

        // A huge hint is capped to max_timeout (30s).
        let mut t = SendTracker::new(cfg(3));
        rec(&mut t, "hi", 1, now);
        t.on_sent(0x2, 10_000_000, now);
        assert!(t
            .collect_due(now + Duration::from_secs(29))
            .to_retry
            .is_empty());
        assert_eq!(
            t.collect_due(now + Duration::from_secs(31)).to_retry.len(),
            1
        );
    }
}
