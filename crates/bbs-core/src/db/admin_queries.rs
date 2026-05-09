//! Admin-only database queries.
//!
//! Inherent methods on [`Database`] used exclusively by the admin methods in
//! `BbsHost`.  These are `pub(crate)` only — no plugin can call them directly.
//!
//! We use `sqlx::query()` (runtime-checked) rather than `sqlx::query!`
//! (compile-time) so these queries do not require re-running
//! `cargo sqlx prepare` on every addition.

use super::{error::StoreError, Database};
use bbs_plugin_api::{AdminBackupRecord, AdminRoomSummary, AdminStats};
use sqlx::Row;
use std::path::Path;

// async_trait rewrites the callers in host.rs into closures that Clippy's
// dead_code analysis does not follow, so these pub(crate) helpers appear unused.
#[allow(dead_code)]
impl Database {
    /// Aggregate BBS statistics.  `active_sessions` is passed in because the
    /// session count lives in `BbsHost`, not the DB.
    pub(crate) async fn admin_stats(
        &self,
        active_sessions: usize,
    ) -> Result<AdminStats, StoreError> {
        let active: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM users WHERE status = 0 AND permission_level > 0",
        )
        .fetch_one(&self.read_pool)
        .await?;

        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM users WHERE status = 0 AND permission_level = 0",
        )
        .fetch_one(&self.read_pool)
        .await?;

        let banned: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM users WHERE status = 1")
            .fetch_one(&self.read_pool)
            .await?;

        let total_messages: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
            .fetch_one(&self.read_pool)
            .await?;

        let total_rooms: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rooms")
            .fetch_one(&self.read_pool)
            .await?;

        Ok(AdminStats {
            active_users: active,
            pending_users: pending,
            banned_users: banned,
            total_messages,
            total_rooms,
            active_sessions,
        })
    }

    /// List all rooms with their message counts (LEFT JOIN).
    pub(crate) async fn admin_list_rooms(&self) -> Result<Vec<AdminRoomSummary>, StoreError> {
        let rows = sqlx::query(
            r#"
            SELECT r.id, r.name, r.description, r.read_only, r.min_permission_level,
                   r.created_at, COUNT(rm.message_id) AS message_count
            FROM rooms r
            LEFT JOIN room_messages rm ON rm.room_id = r.id
            GROUP BY r.id
            ORDER BY r.id
            "#,
        )
        .fetch_all(&self.read_pool)
        .await?;

        rows.into_iter()
            .map(|r| {
                Ok(AdminRoomSummary {
                    id: r.try_get("id")?,
                    name: r.try_get("name")?,
                    description: r.try_get("description")?,
                    read_only: r.try_get::<i64, _>("read_only")? != 0,
                    min_permission_level: r.try_get::<i64, _>("min_permission_level")? as u8,
                    message_count: r.try_get("message_count")?,
                    created_at: r.try_get("created_at")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(StoreError::Db)
    }

    /// Run `VACUUM INTO dest_path` to create a backup copy of the database.
    ///
    /// # Safety / injection
    ///
    /// SQLite does not support bound parameters for `VACUUM INTO`.  The path
    /// is sanitised (single-quotes escaped) before being interpolated.  This
    /// method is `pub(crate)` and only called with paths constructed by the
    /// host from trusted config values — it is never called with user input.
    pub(crate) async fn admin_backup(&self, dest_path: &str) -> Result<(), StoreError> {
        // Create destination directory if needed.
        if let Some(parent) = Path::new(dest_path).parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| StoreError::Decode(format!("create backup dir: {e}")))?;
        }

        let safe = dest_path.replace('\'', "''");
        sqlx::query(&format!("VACUUM INTO '{safe}'"))
            .execute(&self.write_pool)
            .await
            .map_err(StoreError::Db)?;

        Ok(())
    }

    /// List `.db` files in `backup_dir`.
    pub(crate) async fn admin_list_backups(
        &self,
        backup_dir: &str,
    ) -> Result<Vec<AdminBackupRecord>, StoreError> {
        use std::time::{SystemTime, UNIX_EPOCH};

        let dir = Path::new(backup_dir);
        let mut entries = match tokio::fs::read_dir(dir).await {
            Ok(e) => e,
            Err(_) => return Ok(Vec::new()),
        };

        let mut records = Vec::new();
        while let Ok(Some(entry)) = entries.next_entry().await {
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "db" {
                continue;
            }
            let Ok(meta) = tokio::fs::metadata(&path).await else {
                continue;
            };
            let size_bytes = meta.len();
            let modified = meta
                .modified()
                .unwrap_or(SystemTime::UNIX_EPOCH)
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);

            // Format as RFC 3339 (UTC).
            let secs = modified as i64;
            let created_at = time::OffsetDateTime::from_unix_timestamp(secs)
                .map(|dt| {
                    dt.format(&time::format_description::well_known::Rfc3339)
                        .unwrap_or_default()
                })
                .unwrap_or_default();

            let filename = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_owned();

            records.push(AdminBackupRecord {
                filename,
                size_bytes,
                created_at,
            });
        }

        records.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(records)
    }
}
