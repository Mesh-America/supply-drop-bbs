//! Audit log persistence.
//!
//! Every privileged action (ban, unban, validate, delete message, create/delete
//! room, permission change) is appended here via [`AuditStore::write`].
//! [`AuditStore::query`] returns a paginated, optionally filtered view for the
//! admin web UI.

use super::{error::StoreError, Database};
use bbs_plugin_api::AdminAuditEntry;
use sqlx::Row;

// async_trait rewrites async fn bodies; Clippy's dead_code pass misses these.
#[allow(dead_code)]
impl Database {
    /// Append one entry to the audit log.
    pub(crate) async fn audit_write(
        &self,
        actor: &str,
        action: &str,
        target: Option<&str>,
        detail: Option<&str>,
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO audit_log (actor, action, target, detail) VALUES (?, ?, ?, ?)")
            .bind(actor)
            .bind(action)
            .bind(target)
            .bind(detail)
            .execute(&self.write_pool)
            .await?;
        Ok(())
    }

    /// Return paginated audit log entries, newest first.
    ///
    /// `action_filter`: when `Some`, only entries whose `action` equals the
    /// given string are returned.
    pub(crate) async fn audit_query(
        &self,
        limit: u32,
        offset: u32,
        action_filter: Option<&str>,
    ) -> Result<Vec<AdminAuditEntry>, StoreError> {
        let rows = if let Some(action) = action_filter {
            sqlx::query(
                "SELECT id, actor, action, target, detail, created_at \
                 FROM audit_log \
                 WHERE action = ? \
                 ORDER BY id DESC \
                 LIMIT ? OFFSET ?",
            )
            .bind(action)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.read_pool)
            .await?
        } else {
            sqlx::query(
                "SELECT id, actor, action, target, detail, created_at \
                 FROM audit_log \
                 ORDER BY id DESC \
                 LIMIT ? OFFSET ?",
            )
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.read_pool)
            .await?
        };

        rows.into_iter()
            .map(|r| {
                Ok(AdminAuditEntry {
                    id: r.try_get("id")?,
                    actor: r.try_get("actor")?,
                    action: r.try_get("action")?,
                    target: r.try_get("target")?,
                    detail: r.try_get("detail")?,
                    created_at: r.try_get("created_at")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(StoreError::Db)
    }

    /// The highest id among entries created before `before` (an ISO 8601 UTC
    /// timestamp), or `None` when there are none.
    ///
    /// Archiving takes this first and works to it, so entries written while
    /// the archive is being built get higher ids, stay in the live log, and
    /// are neither archived twice nor dropped.
    ///
    /// `created_at` is stored as `YYYY-MM-DDTHH:MM:SSZ`, which sorts
    /// lexicographically in time order, so a string comparison is a date
    /// comparison here.
    pub(crate) async fn audit_max_id_before(
        &self,
        before: &str,
    ) -> Result<Option<i64>, StoreError> {
        let max: Option<i64> =
            sqlx::query_scalar("SELECT MAX(id) FROM audit_log WHERE created_at < ?")
                .bind(before)
                .fetch_one(&self.read_pool)
                .await?;
        Ok(max)
    }

    /// The month of the oldest entry in the log, as `(year, month)`.
    ///
    /// Archiving works forward from here, so a log that accumulated while the
    /// BBS was switched off is archived a month at a time under each month's
    /// own name rather than swept into one misleading file.
    pub(crate) async fn audit_oldest_month(&self) -> Result<Option<(i32, u32)>, StoreError> {
        let oldest: Option<String> = sqlx::query_scalar("SELECT MIN(created_at) FROM audit_log")
            .fetch_one(&self.read_pool)
            .await?;
        let Some(oldest) = oldest else {
            return Ok(None);
        };
        // YYYY-MM-DD... — anything that isn't in that shape can't be placed
        // in a month, and guessing would misfile it.
        let year = oldest.get(0..4).and_then(|s| s.parse::<i32>().ok());
        let month = oldest.get(5..7).and_then(|s| s.parse::<u32>().ok());
        Ok(match (year, month) {
            (Some(y), Some(m)) if (1..=12).contains(&m) => Some((y, m)),
            _ => None,
        })
    }

    /// Entries created in `[from, until)` with `id > after` and
    /// `id <= through`, oldest first, at most `limit` of them.
    ///
    /// The date bounds are what keep a month's archive to that month. The id
    /// bounds do the batching and hold the upper edge still while the archive
    /// is being written. Both are needed: without the dates, archiving a
    /// month would sweep in every older entry too, which is only invisible
    /// when the caller happens to work forward from the oldest month.
    pub(crate) async fn audit_page_in_range(
        &self,
        after: i64,
        through: i64,
        from: &str,
        until: &str,
        limit: u32,
    ) -> Result<Vec<AdminAuditEntry>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, actor, action, target, detail, created_at \
             FROM audit_log \
             WHERE id > ? AND id <= ? AND created_at >= ? AND created_at < ? \
             ORDER BY id ASC \
             LIMIT ?",
        )
        .bind(after)
        .bind(through)
        .bind(from)
        .bind(until)
        .bind(limit)
        .fetch_all(&self.read_pool)
        .await?;

        rows.into_iter()
            .map(|r| {
                Ok(AdminAuditEntry {
                    id: r.try_get("id")?,
                    actor: r.try_get("actor")?,
                    action: r.try_get("action")?,
                    target: r.try_get("target")?,
                    detail: r.try_get("detail")?,
                    created_at: r.try_get("created_at")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(StoreError::Db)
    }

    /// How many entries were created in `[from, until)` at or below
    /// `through`.
    pub(crate) async fn audit_count_in_range(
        &self,
        through: i64,
        from: &str,
        until: &str,
    ) -> Result<u64, StoreError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM audit_log \
             WHERE id <= ? AND created_at >= ? AND created_at < ?",
        )
        .bind(through)
        .bind(from)
        .bind(until)
        .fetch_one(&self.read_pool)
        .await?;
        Ok(count as u64)
    }

    /// Delete entries created in `[from, until)` at or below `through`,
    /// returning how many went.
    ///
    /// Only ever called once an archive holding exactly that range is
    /// complete and renamed into place.
    pub(crate) async fn audit_delete_in_range(
        &self,
        through: i64,
        from: &str,
        until: &str,
    ) -> Result<u64, StoreError> {
        let result = sqlx::query(
            "DELETE FROM audit_log \
             WHERE id <= ? AND created_at >= ? AND created_at < ?",
        )
        .bind(through)
        .bind(from)
        .bind(until)
        .execute(&self.write_pool)
        .await?;
        Ok(result.rows_affected())
    }

    /// Entries with `id > after` and `id <= through`, oldest first, at most
    /// `limit` of them.
    ///
    /// Archiving reads in batches rather than all at once — an audit log left
    /// to grow for months shouldn't have to fit in memory to be archived.
    pub(crate) async fn audit_page_through(
        &self,
        after: i64,
        through: i64,
        limit: u32,
    ) -> Result<Vec<AdminAuditEntry>, StoreError> {
        let rows = sqlx::query(
            "SELECT id, actor, action, target, detail, created_at \
             FROM audit_log \
             WHERE id > ? AND id <= ? \
             ORDER BY id ASC \
             LIMIT ?",
        )
        .bind(after)
        .bind(through)
        .bind(limit)
        .fetch_all(&self.read_pool)
        .await?;

        rows.into_iter()
            .map(|r| {
                Ok(AdminAuditEntry {
                    id: r.try_get("id")?,
                    actor: r.try_get("actor")?,
                    action: r.try_get("action")?,
                    target: r.try_get("target")?,
                    detail: r.try_get("detail")?,
                    created_at: r.try_get("created_at")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(StoreError::Db)
    }

    /// How many entries sit at or below `through`.
    pub(crate) async fn audit_count_through(&self, through: i64) -> Result<u64, StoreError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM audit_log WHERE id <= ?")
            .bind(through)
            .fetch_one(&self.read_pool)
            .await?;
        Ok(count as u64)
    }

    /// Insert an entry at a chosen id and timestamp.
    ///
    /// Test-only. `created_at` defaults to the current time, so a log
    /// spanning several past months — which is what month-by-month archiving
    /// has to get right — can't be built any other way.
    #[cfg(test)]
    pub(crate) async fn audit_write_at_for_test(
        &self,
        id: i64,
        actor: &str,
        action: &str,
        target: &str,
        created_at: &str,
    ) -> Result<(), StoreError> {
        sqlx::query(
            "INSERT INTO audit_log (id, actor, action, target, created_at) \
             VALUES (?, ?, ?, ?, ?)",
        )
        .bind(id)
        .bind(actor)
        .bind(action)
        .bind(target)
        .bind(created_at)
        .execute(&self.write_pool)
        .await?;
        Ok(())
    }

    /// Insert an entry at a chosen id.
    ///
    /// Test-only. It reproduces the state a crash between an archive's rename
    /// and its clear leaves behind — entries that an archive already holds,
    /// still sitting in the live log at their original ids. Ids are otherwise
    /// assigned by the database, so that state can't be built any other way.
    #[cfg(test)]
    pub(crate) async fn audit_restore_for_test(
        &self,
        id: i64,
        actor: &str,
        action: &str,
        target: &str,
    ) -> Result<(), StoreError> {
        sqlx::query("INSERT INTO audit_log (id, actor, action, target) VALUES (?, ?, ?, ?)")
            .bind(id)
            .bind(actor)
            .bind(action)
            .bind(target)
            .execute(&self.write_pool)
            .await?;
        Ok(())
    }

    /// Delete entries at or below `through`, returning how many went.
    ///
    /// Only ever called once an archive holding them is complete and renamed
    /// into place, so a failure part-way through archiving leaves the live log
    /// untouched rather than half-cleared.
    pub(crate) async fn audit_delete_through(&self, through: i64) -> Result<u64, StoreError> {
        let result = sqlx::query("DELETE FROM audit_log WHERE id <= ?")
            .bind(through)
            .execute(&self.write_pool)
            .await?;
        Ok(result.rows_affected())
    }
}
