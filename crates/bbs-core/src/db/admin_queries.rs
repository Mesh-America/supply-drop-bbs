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
    AdminBackupRecord, AdminDailyVolume, AdminHourlyActivity, AdminMessageRecord, AdminReports,
    AdminRoomSummary, AdminStaleRoom, AdminStats, AdminTopRoom, AdminTopSender, AdminWeeklySignups,
};
use sqlx::Row;
use std::path::Path;
use tracing;

// async_trait rewrites the callers in host.rs into closures that Clippy's
// dead_code analysis does not follow, so these pub(crate) helpers appear unused.
#[allow(dead_code)]
impl Database {
    /// Aggregate BBS statistics.  `active_sessions`, `discovered_contacts`,
    /// and `protected_contacts` are passed in because they live in
    /// `BbsHost`'s session tracker and advert bus, not the DB.
    pub(crate) async fn admin_stats(
        &self,
        active_sessions: usize,
        discovered_contacts: usize,
        protected_contacts: usize,
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
            discovered_contacts,
            protected_contacts,
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
                    locked: (2..=4).contains(&id),
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

        // Hourly activity distribution across all time.
        let hourly_rows = sqlx::query(
            "SELECT CAST(strftime('%H', timestamp) AS INTEGER) AS hour, COUNT(*) AS cnt \
             FROM messages GROUP BY hour ORDER BY hour ASC",
        )
        .fetch_all(&self.read_pool)
        .await?;

        let hourly_activity = hourly_rows
            .into_iter()
            .map(|r| {
                Ok(AdminHourlyActivity {
                    hour: r.try_get::<i64, _>("hour")? as u8,
                    count: r.try_get("cnt")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(StoreError::Db)?;

        // New user signups per week for the last 8 weeks.
        let signups_rows = sqlx::query(
            "SELECT strftime('%Y-W%W', created_at) AS week, COUNT(*) AS cnt \
             FROM users WHERE created_at >= datetime('now', '-56 days') \
             GROUP BY week ORDER BY week ASC",
        )
        .fetch_all(&self.read_pool)
        .await?;

        let new_users_by_week = signups_rows
            .into_iter()
            .map(|r| {
                Ok(AdminWeeklySignups {
                    week: r.try_get("week")?,
                    count: r.try_get("cnt")?,
                })
            })
            .collect::<Result<Vec<_>, sqlx::Error>>()
            .map_err(StoreError::Db)?;

        // Recent message window counts.
        let msgs_last_24h: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM messages WHERE timestamp >= datetime('now', '-1 day')",
        )
        .fetch_one(&self.read_pool)
        .await?;

        let msgs_last_7d: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM messages WHERE timestamp >= datetime('now', '-7 days')",
        )
        .fetch_one(&self.read_pool)
        .await?;

        let msgs_last_30d: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM messages WHERE timestamp >= datetime('now', '-30 days')",
        )
        .fetch_one(&self.read_pool)
        .await?;

        Ok(AdminReports {
            top_senders,
            top_rooms,
            daily_volume,
            stale_rooms,
            hourly_activity,
            new_users_by_week,
            msgs_last_24h,
            msgs_last_7d,
            msgs_last_30d,
        })
    }

    /// Message count for a single room (used by admin_update_room to populate the
    /// returned `AdminRoomSummary`).
    pub(crate) async fn room_message_count(&self, room_id: i64) -> Result<i64, StoreError> {
        let count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM room_messages WHERE room_id = ?")
            .bind(room_id)
            .fetch_one(&self.read_pool)
            .await?;
        Ok(count)
    }

    /// Search non-DM room messages by optional sender and/or content substring.
    ///
    /// Only messages linked via `room_messages` are searched — private Mail DMs
    /// (stored in `messages` but not in `room_messages`) are never returned.
    pub(crate) async fn admin_search_messages(
        &self,
        sender: Option<&str>,
        query: Option<&str>,
        limit: u32,
    ) -> Result<Vec<AdminMessageRecord>, StoreError> {
        // Build the WHERE clauses dynamically.  We always have the room_messages
        // join which already excludes DMs.  Additional filters are opt-in.
        let mut sql = String::from(
            "SELECT m.id, m.sender, m.recipient, m.content, m.timestamp \
             FROM messages m \
             INNER JOIN room_messages rm ON rm.message_id = m.id \
             WHERE 1=1",
        );
        if sender.is_some() {
            sql.push_str(" AND m.sender = ?");
        }
        if query.is_some() {
            sql.push_str(" AND m.content LIKE ? ESCAPE '\\'");
        }
        sql.push_str(" ORDER BY m.id DESC LIMIT ?");

        let mut q = sqlx::query(&sql);
        if let Some(s) = sender {
            q = q.bind(s);
        }
        if let Some(text) = query {
            // Escape LIKE metacharacters so user input is treated as a literal.
            let escaped = text
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            q = q.bind(format!("%{escaped}%"));
        }
        q = q.bind(limit as i64);

        let rows = q.fetch_all(&self.read_pool).await?;
        rows.into_iter()
            .map(|r| {
                Ok(AdminMessageRecord {
                    id: r.try_get("id")?,
                    sender: r.try_get("sender")?,
                    recipient: r.try_get("recipient")?,
                    content: r.try_get("content")?,
                    timestamp: r.try_get("timestamp")?,
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

    /// List `.zip` and legacy `.db` backup files in `backup_dir`.
    pub(crate) async fn admin_list_backups(
        &self,
        backup_dir: &str,
    ) -> Result<Vec<AdminBackupRecord>, StoreError> {
        use std::time::{SystemTime, UNIX_EPOCH};

        let dir = Path::new(backup_dir);
        let mut entries = match tokio::fs::read_dir(dir).await {
            Ok(e) => e,
            Err(e) => {
                // Directory does not exist yet or is unreadable.  This is
                // normal on first startup before the backup task has run and
                // created the directory, so we log at debug rather than warn.
                tracing::debug!(path = %dir.display(), err = %e, "backup: cannot read backup directory");
                return Ok(Vec::new());
            }
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

    /// Validate an uploaded file as a restorable supply-drop-bbs database,
    /// WITHOUT touching the live database, then stage it as
    /// `<data_dir>/pending_restore.staged.db`.
    ///
    /// This is deliberately an INERT filename that `main.rs`'s startup check
    /// never looks at — only `admin_apply_staged_restore` (called from the
    /// web layer's `api_apply_restore` once the sysop has explicitly
    /// confirmed) renames it to `pending_restore.db`, the name that actually
    /// triggers the destructive swap. Staging and confirming must be two
    /// distinct filesystem states, not two calls that both key off the same
    /// file: an earlier version of this feature staged directly to
    /// `pending_restore.db`, which meant ANY unrelated process restart
    /// between upload and confirmation — a crash, an operator restarting
    /// the service for an unrelated reason, systemd's own `Restart=always`
    /// firing after any exit — silently applied a restore nobody had
    /// confirmed yet (issue #195).
    ///
    /// Validation has four tiers: a cheap SQLite file-format check; a check
    /// that the file already has migration history of its own (see below);
    /// running this binary's own embedded migrations against the uploaded
    /// file directly — the same mechanism `Database::open` uses on the live
    /// database, just pointed at the candidate file instead, which lets an
    /// older-schema backup be upgraded in place as part of staging it (this
    /// means "validation" can mutate the uploaded file's on-disk bytes, not
    /// merely inspect them); and finally the same room-walk-order
    /// structural check `Database::open` runs after migrating.
    ///
    /// The migration history check is load-bearing, not redundant: every
    /// migration in `crates/bbs-core/migrations/` is written to be safely
    /// re-runnable (`CREATE TABLE IF NOT EXISTS`, `INSERT OR IGNORE`, or
    /// `DROP TABLE IF EXISTS` + `CREATE TABLE`), so `sqlx::migrate!().run()`
    /// on its own happily builds a full, valid, EMPTY schema out of a
    /// brand-new SQLite file — running the migrator alone cannot tell "a
    /// real backup that needs a couple of pending migrations" apart from
    /// "an empty file migrate! is willing to adopt from scratch". Requiring
    /// `_sqlx_migrations` to already contain at least one successful row
    /// closes that gap for the common accidental case (an empty file, or a
    /// foreign app's unrelated database). It is not a cryptographic
    /// guarantee: sqlx's migration checksums are plain SHA-384 hashes of
    /// this project's own (public) migration file contents, so a
    /// hand-crafted file could in principle pre-populate `_sqlx_migrations`
    /// with correct-looking rows and no real tables behind them. The
    /// `verify_room_walk_order` check below (mirroring what `Database::open`
    /// does on the live database) catches that case too, by actually
    /// querying the `rooms` table rather than trusting bookkeeping alone —
    /// and this endpoint is sysop-only regardless.
    ///
    /// `uploaded_path` may be a raw `.db` file OR a `.zip` bundle in the
    /// exact shape `admin_backup`'s caller produces (a single `.db`-named
    /// entry, optional `config.toml`) — a zip is detected by magic bytes
    /// and its `.db` entry extracted (overwriting `uploaded_path` in place)
    /// before the checks below run. The zip format is accepted here, in
    /// the one place both the CLI and the web upload handler call through,
    /// so neither caller needs its own copy of this detection logic.
    /// `uploaded_path` is always a disposable file the caller owns for the
    /// duration of this call (a per-request temp file, or a CLI-made copy
    /// of the operator's real source file) — never overwrite a file the
    /// caller doesn't expect to be consumed this way.
    ///
    /// Public (not `pub(crate)`) deliberately: this never opens or requires
    /// the LIVE database, so the CLI `restore` subcommand can call it
    /// directly, without going through `open_database`/`BbsHost` — staging
    /// a restore must keep working even when the live database is broken
    /// or missing, which is often exactly why an operator wants to restore.
    pub async fn stage_restore(uploaded_path: &Path, data_dir: &Path) -> Result<(), StoreError> {
        let raw = tokio::fs::read(uploaded_path)
            .await
            .map_err(|e| StoreError::Decode(format!("read uploaded file: {e}")))?;
        if raw.starts_with(b"PK\x03\x04") {
            let extracted = tokio::task::spawn_blocking(move || extract_single_db_from_zip(&raw))
                .await
                .map_err(|e| StoreError::Decode(format!("extracting zip upload: {e}")))?
                .map_err(StoreError::Decode)?;
            tokio::fs::write(uploaded_path, &extracted)
                .await
                .map_err(|e| StoreError::Decode(format!("writing extracted upload: {e}")))?;
        } else if !raw.starts_with(b"SQLite format 3\0") {
            return Err(StoreError::Decode(
                "not a SQLite database file or a recognized backup zip (bad header)".into(),
            ));
        }

        let opts = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(uploaded_path)
            .create_if_missing(false)
            .foreign_keys(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await
            .map_err(|e| StoreError::Decode(format!("open uploaded database: {e}")))?;

        let migrations_table_exists: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
        )
        .fetch_one(&pool)
        .await
        .unwrap_or(0);
        let applied_migrations: i64 = if migrations_table_exists > 0 {
            sqlx::query_scalar("SELECT COUNT(*) FROM _sqlx_migrations WHERE success = 1")
                .fetch_one(&pool)
                .await
                .unwrap_or(0)
        } else {
            0
        };
        if applied_migrations == 0 {
            pool.close().await;
            return Err(StoreError::Decode(
                "uploaded file has no supply-drop-bbs migration history — \
                 not a backup of this application (an empty or unrelated \
                 SQLite file cannot be restored)"
                    .into(),
            ));
        }

        let migrate_result = sqlx::migrate!("./migrations").run(&pool).await;
        // `Database::open` also verifies the room-walk-order invariant
        // after migrating (see db/mod.rs) — check it here too, on the same
        // still-open pool, before staging. Without this, a candidate file
        // that migrates cleanly but has a corrupt room linked-list would
        // only be caught AFTER the destructive swap in main.rs, whose only
        // failure handling is to exit — there is no automatic rollback to
        // the pre-restore safety snapshot.
        let invariant_result = if migrate_result.is_ok() {
            Some(super::invariants::verify_room_walk_order(&pool).await)
        } else {
            None
        };
        pool.close().await;
        // Best-effort: a VACUUM INTO backup (this project's own admin_backup)
        // is produced in DELETE journal mode and carries no sidecars, but
        // defend against an upload that somehow does — a stray WAL/SHM next
        // to the staged file would otherwise ride along into `data_dir`.
        let _ = tokio::fs::remove_file(format!("{}-wal", uploaded_path.display())).await;
        let _ = tokio::fs::remove_file(format!("{}-shm", uploaded_path.display())).await;
        migrate_result.map_err(|e| {
            StoreError::Decode(format!(
                "uploaded file is not a compatible supply-drop-bbs \
                 database (migration check failed: {e})"
            ))
        })?;
        if let Some(Err(e)) = invariant_result {
            return Err(StoreError::Decode(format!(
                "uploaded file has a broken room structure ({e}) — not a \
                 healthy supply-drop-bbs database"
            )));
        }

        let staged_path = data_dir.join("pending_restore.staged.db");
        // `rename` is atomic but fails across filesystems (EXDEV) — the
        // upload's temp file and data_dir are not guaranteed to share one,
        // so fall back to copy+delete on that specific failure.
        if tokio::fs::rename(uploaded_path, &staged_path)
            .await
            .is_err()
        {
            tokio::fs::copy(uploaded_path, &staged_path)
                .await
                .map_err(|e| StoreError::Decode(format!("stage restore file: {e}")))?;
            let _ = tokio::fs::remove_file(uploaded_path).await;
        }

        Ok(())
    }

    /// Confirm a previously staged restore (see `stage_restore`) by renaming
    /// it from its inert staged name to `pending_restore.db` — the only
    /// name `main.rs`'s startup check looks for. Re-checks the staged
    /// file's SQLite header immediately before the rename: cheap insurance
    /// against confirming a file left truncated by an interrupted upload
    /// (e.g. a concurrent upload's copy-fallback still in flight when the
    /// process exits to apply this one).
    ///
    /// Returns an error, touching nothing, if no restore is currently
    /// staged.
    ///
    /// Public for the same reason as `stage_restore`: confirming a restore
    /// must not require the live database to open cleanly first.
    pub async fn admin_apply_staged_restore(data_dir: &Path) -> Result<(), StoreError> {
        let staged_path = data_dir.join("pending_restore.staged.db");
        if !staged_path.exists() {
            return Err(StoreError::Decode("no restore is currently staged".into()));
        }
        if !sqlite_header_ok(&staged_path).await {
            let _ = tokio::fs::remove_file(&staged_path).await;
            return Err(StoreError::Decode(
                "staged restore file is corrupt (bad header) — discarded, upload again".into(),
            ));
        }

        let confirmed_path = data_dir.join("pending_restore.db");
        if tokio::fs::rename(&staged_path, &confirmed_path)
            .await
            .is_err()
        {
            tokio::fs::copy(&staged_path, &confirmed_path)
                .await
                .map_err(|e| StoreError::Decode(format!("confirm restore file: {e}")))?;
            let _ = tokio::fs::remove_file(&staged_path).await;
        }
        Ok(())
    }
}

/// Read just the first 16 bytes of `path` and check them against the SQLite
/// file-format magic, without loading the whole (potentially multi-gigabyte)
/// file into memory the way a plain `tokio::fs::read` header check would.
async fn sqlite_header_ok(path: &std::path::Path) -> bool {
    use tokio::io::AsyncReadExt;
    let mut buf = [0u8; 16];
    let Ok(mut file) = tokio::fs::File::open(path).await else {
        return false;
    };
    file.read_exact(&mut buf).await.is_ok() && buf.starts_with(b"SQLite format 3\0")
}

/// Extract the single `.db`-named entry from a zip archive's raw bytes, as
/// produced by `admin_backup`'s zip-bundling caller. Rejects an archive
/// with zero or more than one `.db` entry rather than guessing which one is
/// the database. Runs synchronously — callers on an async runtime should
/// wrap this in `spawn_blocking`, since decompression is CPU-bound.
fn extract_single_db_from_zip(zip_bytes: &[u8]) -> Result<Vec<u8>, String> {
    let cursor = std::io::Cursor::new(zip_bytes);
    let mut archive = zip::ZipArchive::new(cursor).map_err(|e| format!("reading zip: {e}"))?;
    let mut db_index = None;
    for i in 0..archive.len() {
        let entry = archive
            .by_index(i)
            .map_err(|e| format!("reading zip entry {i}: {e}"))?;
        if entry.name().ends_with(".db") {
            if db_index.is_some() {
                return Err("zip contains more than one .db file".to_owned());
            }
            db_index = Some(i);
        }
    }
    let idx = db_index.ok_or_else(|| "zip does not contain a .db file".to_owned())?;
    let mut entry = archive
        .by_index(idx)
        .map_err(|e| format!("reading zip entry: {e}"))?;
    let mut out = Vec::with_capacity(entry.size() as usize);
    std::io::Read::read_to_end(&mut entry, &mut out)
        .map_err(|e| format!("extracting zip entry: {e}"))?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::extract_single_db_from_zip;

    // Issue #195: `admin_backup`'s zip-bundling caller never offers a raw
    // `.db` for download, only the `.zip` bundle it always produces — so
    // `stage_restore` must accept that same zip, or the "download a
    // backup, restore it later" workflow is a dead end.
    fn build_test_zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write as _;
        let mut buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let opts = zip::write::SimpleFileOptions::default();
            for (name, contents) in entries {
                zip.start_file(*name, opts).unwrap();
                zip.write_all(contents).unwrap();
            }
            zip.finish().unwrap();
        }
        buf
    }

    #[test]
    fn extract_single_db_from_zip_finds_the_lone_db_entry() {
        let zip_bytes = build_test_zip(&[
            ("backup-2026-09-04.db", b"SQLite format 3\0fake db bytes"),
            ("config.toml", b"[bbs]\nname = \"Test\"\n"),
        ]);
        let extracted =
            extract_single_db_from_zip(&zip_bytes).expect("a single .db entry must extract");
        assert_eq!(extracted, b"SQLite format 3\0fake db bytes");
    }

    #[test]
    fn extract_single_db_from_zip_rejects_no_db_entry() {
        let zip_bytes = build_test_zip(&[("config.toml", b"[bbs]\n")]);
        let result = extract_single_db_from_zip(&zip_bytes);
        assert!(
            result.is_err(),
            "a zip with no .db entry must be rejected, not silently accepted"
        );
    }

    #[test]
    fn extract_single_db_from_zip_rejects_ambiguous_multiple_db_entries() {
        let zip_bytes = build_test_zip(&[
            ("one.db", b"SQLite format 3\0aaa"),
            ("two.db", b"SQLite format 3\0bbb"),
        ]);
        let result = extract_single_db_from_zip(&zip_bytes);
        assert!(
            result.is_err(),
            "an ambiguous zip with two .db entries must be rejected rather \
             than silently picking one"
        );
    }

    #[test]
    fn extract_single_db_from_zip_rejects_a_non_zip_buffer() {
        let result = extract_single_db_from_zip(b"not a zip at all");
        assert!(result.is_err());
    }
}
