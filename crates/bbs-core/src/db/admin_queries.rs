//! Admin-only database queries.
//!
//! Inherent methods on [`Database`] used exclusively by the admin methods in
//! `BbsHost`.  These are `pub(crate)` only — no plugin can call them directly.
//!
//! We use `sqlx::query()` (runtime-checked) rather than `sqlx::query!`
//! (compile-time) so these queries do not require re-running
//! `cargo sqlx prepare` on every addition.

use super::{error::StoreError, Database};
use bbs_plugin_api::{
    AdminBackupRecord, AdminDailyVolume, AdminReports, AdminRoomSummary, AdminStaleRoom,
    AdminStats, AdminTopRoom, AdminTopSender,
};
use sqlx::Row;
use std::path::Path;
use tracing;

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
                let id: i64 = r.try_get("id")?;
                Ok(AdminRoomSummary {
                    id,
                    name: r.try_get("name")?,
                    description: r.try_get("description")?,
                    read_only: r.try_get::<i64, _>("read_only")? != 0,
                    min_permission_level: r.try_get::<i64, _>("min_permission_level")? as u8,
                    message_count: r.try_get("message_count")?,
                    created_at: r.try_get("created_at")?,
                    deletable: id > 5,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(StoreError::Db)
    }

    /// Aggregate analytics: top senders, top rooms, daily volume, stale rooms.
    pub(crate) async fn admin_reports(&self) -> Result<AdminReports, StoreError> {
        // Top 10 senders by message count.
        let top_sender_rows = sqlx::query(
            "SELECT sender, COUNT(*) AS cnt FROM messages GROUP BY sender ORDER BY cnt DESC LIMIT 10",
        )
        .fetch_all(&self.read_pool)
        .await?;

        let top_senders = top_sender_rows
            .into_iter()
            .map(|r| {
                Ok(AdminTopSender {
                    username: r.try_get("sender")?,
                    message_count: r.try_get("cnt")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(StoreError::Db)?;

        // Top 10 rooms by message count.
        let top_room_rows = sqlx::query(
            r#"
            SELECT r.id, r.name, COUNT(rm.message_id) AS cnt
            FROM rooms r
            LEFT JOIN room_messages rm ON rm.room_id = r.id
            GROUP BY r.id
            ORDER BY cnt DESC
            LIMIT 10
            "#,
        )
        .fetch_all(&self.read_pool)
        .await?;

        let top_rooms = top_room_rows
            .into_iter()
            .map(|r| {
                Ok(AdminTopRoom {
                    room_id: r.try_get("id")?,
                    room_name: r.try_get("name")?,
                    message_count: r.try_get("cnt")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(StoreError::Db)?;

        // Daily message volume for the past 30 days (ascending).
        let volume_rows = sqlx::query(
            r#"
            SELECT substr(timestamp, 1, 10) AS day, COUNT(*) AS cnt
            FROM messages
            WHERE timestamp >= datetime('now', '-30 days')
            GROUP BY day
            ORDER BY day ASC
            "#,
        )
        .fetch_all(&self.read_pool)
        .await?;

        let daily_volume = volume_rows
            .into_iter()
            .map(|r| {
                Ok(AdminDailyVolume {
                    day: r.try_get("day")?,
                    count: r.try_get("cnt")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(StoreError::Db)?;

        // Rooms with no messages in the last 30 days (or ever), oldest-first.
        let stale_rows = sqlx::query(
            r#"
            SELECT r.id, r.name, MAX(m.timestamp) AS last_msg
            FROM rooms r
            LEFT JOIN room_messages rm ON rm.room_id = r.id
            LEFT JOIN messages m ON m.id = rm.message_id
            GROUP BY r.id
            HAVING last_msg IS NULL OR last_msg < datetime('now', '-30 days')
            ORDER BY last_msg ASC
            "#,
        )
        .fetch_all(&self.read_pool)
        .await?;

        let stale_rooms = stale_rows
            .into_iter()
            .map(|r| {
                Ok(AdminStaleRoom {
                    room_id: r.try_get("id")?,
                    room_name: r.try_get("name")?,
                    last_message_at: r.try_get("last_msg")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(StoreError::Db)?;

        Ok(AdminReports {
            top_senders,
            top_rooms,
            daily_volume,
            stale_rooms,
        })
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

    /// List `.zip` and legacy `.db` backup files in `backup_dir`.
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
            // Accept zip (new) and db (legacy); skip _config.toml sidecar files.
            if ext != "zip" && ext != "db" {
                continue;
            }
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_owned();
            if name.ends_with("_config.toml") {
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

            // For legacy .db files check for a sidecar _config.toml.
            // For .zip files the config is already inside the archive.
            let (config_filename, config_size_bytes) = if name.ends_with(".db") {
                let config_name = format!("{}_config.toml", name.trim_end_matches(".db"));
                match tokio::fs::metadata(dir.join(&config_name)).await {
                    Ok(m) => (Some(config_name), Some(m.len())),
                    Err(_) => (None, None),
                }
            } else {
                (None, None)
            };

            records.push(AdminBackupRecord {
                filename: name,
                size_bytes,
                created_at,
                config_filename,
                config_size_bytes,
            });
        }

        records.sort_by(|a, b| b.created_at.cmp(&a.created_at));
        Ok(records)
    }

    /// Delete a backup `.db` file (and its associated config snapshot) from
    /// `backup_dir`.
    ///
    /// Returns `StoreError::Decode("invalid filename")` if the filename
    /// contains path traversal characters (`/`, `\`, `..`).
    pub(crate) async fn admin_delete_backup(
        &self,
        backup_dir: &str,
        filename: &str,
    ) -> Result<(), StoreError> {
        if filename.contains('/') || filename.contains('\\') || filename.contains("..") {
            return Err(StoreError::Decode("invalid filename".into()));
        }

        let dir = Path::new(backup_dir);
        let db_path = dir.join(filename);

        tokio::fs::remove_file(&db_path)
            .await
            .map_err(|e| StoreError::Decode(format!("delete backup: {e}")))?;

        // Best-effort: for legacy .db backups also remove the sidecar _config.toml.
        // .zip backups are self-contained so there is nothing extra to clean up.
        if filename.ends_with(".db") {
            let config_name = format!("{}_config.toml", filename.trim_end_matches(".db"));
            let config_path = dir.join(&config_name);
            match tokio::fs::remove_file(&config_path).await {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    tracing::warn!("could not delete config snapshot {config_name}: {e}");
                }
            }
        }

        Ok(())
    }
}
