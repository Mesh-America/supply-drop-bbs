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

    /// The highest id in the log, or `None` when it's empty.
    ///
    /// Archiving takes this first and works to it, so entries written while
    /// the archive is being built get higher ids, stay in the live log, and
    /// are neither archived twice nor dropped.
    pub(crate) async fn audit_max_id(&self) -> Result<Option<i64>, StoreError> {
        let max: Option<i64> = sqlx::query_scalar("SELECT MAX(id) FROM audit_log")
            .fetch_one(&self.read_pool)
            .await?;
        Ok(max)
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
