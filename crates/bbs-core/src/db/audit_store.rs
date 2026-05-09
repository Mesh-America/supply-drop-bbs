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
}
