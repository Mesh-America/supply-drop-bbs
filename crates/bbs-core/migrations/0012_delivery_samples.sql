-- Durable reply-delivery metric samples for transports (mesh link health).
--
-- The mesh transport snapshots its cumulative delivery counters once a minute.
-- Persisting them here lets the admin UI keep its confirm-rate trend across a
-- restart or redeploy — which is exactly when an operator wants to compare
-- before/after a change (antenna, placement, radio params, retry budget).
--
-- transport       : transport name the sample belongs to (e.g. "meshcore").
-- ts              : Unix timestamp (seconds) the sample was taken.
-- The remaining columns are cumulative-since-process-start counters; consumers
-- derive per-interval rates from the deltas between consecutive samples.

CREATE TABLE IF NOT EXISTS delivery_samples (
    id              INTEGER PRIMARY KEY AUTOINCREMENT,
    transport       TEXT    NOT NULL,
    ts              INTEGER NOT NULL,
    sends_total     INTEGER NOT NULL,
    accepted        INTEGER NOT NULL,
    failed_no_route INTEGER NOT NULL,
    confirmed       INTEGER NOT NULL,
    latency_count   INTEGER NOT NULL,
    latency_sum_ms  INTEGER NOT NULL
);

-- Reads are always "this transport, samples at/after a cutoff, in time order".
CREATE INDEX IF NOT EXISTS idx_delivery_samples_transport_ts
    ON delivery_samples (transport, ts);
