//! Persistence for transport delivery-metric samples (mesh link-health trends).
//!
//! The mesh transport snapshots its cumulative delivery counters once a minute
//! and [`Database::delivery_sample_write`] appends them here, pruning rows older
//! than the retention window so the table stays bounded.
//! [`Database::delivery_samples_query`] returns a transport's recent samples to
//! seed the in-memory trend on startup.

use super::{error::StoreError, Database};
use bbs_plugin_api::DeliverySampleRecord;
use sqlx::Row;

/// How long delivery samples are retained. Pruned on each write so the table
/// never grows without bound (one row per minute per transport otherwise).
const RETENTION_SECS: u64 = 7 * 24 * 60 * 60;

// async_trait rewrites async fn bodies; Clippy's dead_code pass misses these.
#[allow(dead_code)]
impl Database {
    /// Append one delivery sample for `transport`, then prune samples older than
    /// the retention window (relative to this sample's timestamp).
    pub(crate) async fn delivery_sample_write(
        &self,
        transport: &str,
        s: &DeliverySampleRecord,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO delivery_samples \
             (transport, ts, sends_total, retransmits, accepted, failed_no_route, confirmed, \
              latency_count, latency_sum_ms) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(transport)
        .bind(s.ts as i64)
        .bind(s.sends_total as i64)
        .bind(s.retransmits as i64)
        .bind(s.accepted as i64)
        .bind(s.failed_no_route as i64)
        .bind(s.confirmed as i64)
        .bind(s.latency_count as i64)
        .bind(s.latency_sum_ms as i64)
        .execute(&self.write_pool)
        .await?;

        let cutoff = s.ts.saturating_sub(RETENTION_SECS) as i64;
        sqlx::query("DELETE FROM delivery_samples WHERE transport = ? AND ts < ?")
            .bind(transport)
            .bind(cutoff)
            .execute(&self.write_pool)
            .await?;
        Ok(())
    }

    /// Return delivery samples for `transport` with `ts >= since` (Unix seconds),
    /// oldest first.
    pub(crate) async fn delivery_samples_query(
        &self,
        transport: &str,
        since: u64,
    ) -> Result<Vec<DeliverySampleRecord>, StoreError> {
        let rows = sqlx::query(
            "SELECT ts, sends_total, retransmits, accepted, failed_no_route, confirmed, \
                    latency_count, latency_sum_ms \
             FROM delivery_samples \
             WHERE transport = ? AND ts >= ? \
             ORDER BY ts ASC",
        )
        .bind(transport)
        .bind(since as i64)
        .fetch_all(&self.read_pool)
        .await?;

        rows.into_iter()
            .map(|r| {
                Ok(DeliverySampleRecord {
                    ts: r.try_get::<i64, _>("ts")? as u64,
                    sends_total: r.try_get::<i64, _>("sends_total")? as u64,
                    retransmits: r.try_get::<i64, _>("retransmits")? as u64,
                    accepted: r.try_get::<i64, _>("accepted")? as u64,
                    failed_no_route: r.try_get::<i64, _>("failed_no_route")? as u64,
                    confirmed: r.try_get::<i64, _>("confirmed")? as u64,
                    latency_count: r.try_get::<i64, _>("latency_count")? as u64,
                    latency_sum_ms: r.try_get::<i64, _>("latency_sum_ms")? as u64,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(StoreError::Db)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_db() -> (Database, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("delivery.sqlite");
        let db = Database::open(path.to_str().unwrap()).await.unwrap();
        (db, dir)
    }

    fn rec(ts: u64) -> DeliverySampleRecord {
        DeliverySampleRecord {
            ts,
            sends_total: ts,
            retransmits: ts / 10,
            accepted: ts,
            failed_no_route: 0,
            confirmed: ts,
            latency_count: 1,
            latency_sum_ms: 200,
        }
    }

    #[tokio::test]
    async fn write_query_round_trip_with_since_and_transport_isolation() {
        let (db, _dir) = test_db().await;
        db.delivery_sample_write("meshcore", &rec(1000))
            .await
            .unwrap();
        db.delivery_sample_write("meshcore", &rec(1060))
            .await
            .unwrap();
        db.delivery_sample_write("meshtastic", &rec(1060))
            .await
            .unwrap();

        let all = db.delivery_samples_query("meshcore", 0).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].ts, 1000, "oldest first");
        assert_eq!(all[1].ts, 1060);
        assert_eq!(all[1].retransmits, 106, "retransmits column round-trips");
        assert_eq!(
            all[1].confirmed, 1060,
            "u64 round-trips through i64 columns"
        );

        let recent = db.delivery_samples_query("meshcore", 1060).await.unwrap();
        assert_eq!(recent.len(), 1, "since filter is inclusive");
        assert_eq!(recent[0].ts, 1060);

        let mt = db.delivery_samples_query("meshtastic", 0).await.unwrap();
        assert_eq!(mt.len(), 1, "samples are isolated per transport");
    }

    #[tokio::test]
    async fn retention_prunes_old_samples_on_write() {
        let (db, _dir) = test_db().await;
        db.delivery_sample_write("meshcore", &rec(1000))
            .await
            .unwrap();
        // A sample more than the retention window later evicts the old one.
        let future = 1000 + RETENTION_SECS + 60;
        db.delivery_sample_write("meshcore", &rec(future))
            .await
            .unwrap();

        let all = db.delivery_samples_query("meshcore", 0).await.unwrap();
        assert_eq!(all.len(), 1);
        assert_eq!(all[0].ts, future);
    }
}
