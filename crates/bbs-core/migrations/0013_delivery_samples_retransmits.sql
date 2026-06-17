-- Add a retransmits column to delivery_samples.
--
-- The confirm-rate trend must be per reply, not per transmission: its
-- denominator is first sends (= sends_total − retransmits), not the cumulative
-- accepted count (which includes every retransmission). Persisting retransmits
-- lets the /history consumer derive first sends per interval after a restart.
--
-- Additive: existing rows (if any) default to 0, which slightly understates
-- first-send deltas for pre-upgrade samples only.

ALTER TABLE delivery_samples ADD COLUMN retransmits INTEGER NOT NULL DEFAULT 0;
