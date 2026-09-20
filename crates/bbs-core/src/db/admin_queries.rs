//! Admin-only database queries.
//!
//! Inherent methods on [`Database`] used exclusively by the admin methods in
//! `BbsHost`.  These are `pub(crate)` only — no plugin can call them directly.
//!
//! We use `sqlx::query()` (runtime-checked) rather than `sqlx::query!`
//! (compile-time) so these queries do not require re-running
//! `cargo sqlx prepare` on every addition.

use super::{error::StoreError, Database};
use crate::restore_apply::sibling_with_suffix;
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

    /// List `.zip` and legacy `.db` backup files in `backup_dir`, newest first.
    ///
    /// Only reads the directory, so (like `stage_restore`) it needs no open
    /// database: the CLI lists backups even when the live database is broken.
    pub async fn admin_list_backups(
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
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("")
                .to_owned();
            // Accept zip (new) and db (legacy); the rule is shared with the
            // restore endpoint, and it skips the `_config.toml` sidecar files.
            if !crate::restore_stage::is_backup_file_name(&name)
                || !crate::restore_stage::backup_filename_is_safe(&name)
            {
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

            // For legacy .db files check for a sidecar _config.toml. A .zip bundle
            // carries its config inside, so look for it there.
            let (config_filename, config_size_bytes) = if name.ends_with(".zip") {
                let zip_path = path.clone();
                match tokio::task::spawn_blocking(move || {
                    crate::backup_bundle::bundled_config_size(&zip_path)
                })
                .await
                {
                    Ok(Some(size)) => (
                        Some(crate::backup_bundle::CONFIG_ENTRY.to_owned()),
                        Some(size),
                    ),
                    _ => (None, None),
                }
            } else if name.ends_with(".db") {
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

        records.sort_by(|a, b| {
            b.created_at
                .cmp(&a.created_at)
                .then_with(|| b.filename.cmp(&a.filename))
        });
        Ok(records)
    }

    /// Delete a backup `.db` file (and its associated config snapshot) from
    /// `backup_dir`.
    ///
    /// Returns `StoreError::Decode("invalid filename")` if the filename
    /// contains path traversal characters (`/`, `\`, `..`).
    /// Returns [`StoreError::NotFound`] if there is no such file.
    /// Like [`Self::admin_list_backups`] it needs no open database.
    pub async fn admin_delete_backup(backup_dir: &str, filename: &str) -> Result<(), StoreError> {
        if !crate::restore_stage::backup_filename_is_safe(filename) {
            return Err(StoreError::Decode("invalid filename".into()));
        }
        // Only backups: the backup directory can be shared with other files
        // (or be the data directory), and this must never remove those.
        if !crate::restore_stage::is_backup_file_name(filename) {
            return Err(StoreError::Decode(
                "not a backup file (only backup .db and .zip files can be deleted, not the restore's own files)".into(),
            ));
        }

        let dir = Path::new(backup_dir);
        let db_path = dir.join(filename);

        match tokio::fs::remove_file(&db_path).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(StoreError::NotFound),
            Err(e) => return Err(StoreError::Decode(format!("delete backup: {e}"))),
        }

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
    /// Validation has five tiers: a cheap SQLite file-format check; a check
    /// that the file already has migration history of its own (see below);
    /// running this binary's own embedded migrations against the uploaded
    /// file directly — the same mechanism `Database::open` uses on the live
    /// database, just pointed at the candidate file instead, which lets an
    /// older-schema backup be upgraded in place as part of staging it (this
    /// means "validation" can mutate the uploaded file's on-disk bytes, not
    /// merely inspect them); the same room-walk-order structural check
    /// `Database::open` runs after migrating; and finally SQLite's own
    /// `PRAGMA quick_check`, which catches page-level damage in tables the
    /// other checks never read (it makes staging take time proportional to the
    /// file size).
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
        // The whole file is never read into memory: a database can be
        // gigabytes, and this runs on small boards. Only the header is read
        // here, and a zip is extracted to disk by streaming.
        let uploaded_len = tokio::fs::metadata(uploaded_path)
            .await
            .map_err(|e| StoreError::Decode(format!("read uploaded file: {e}")))?
            .len();
        // The config.toml a zip bundle carries, held until the database is staged.
        let mut config_text: Option<String> = None;
        match classify_upload(uploaded_path).await? {
            UploadKind::Zip => {
                let extract_path = sibling_with_suffix(uploaded_path, ".extract.tmp");
                let (src, dest) = (uploaded_path.to_path_buf(), extract_path.clone());
                let extracted = tokio::task::spawn_blocking(move || {
                    let config = extract_optional_config(&src)?;
                    extract_single_db_from_zip(&src, &dest, MAX_RESTORE_DB_BYTES)?;
                    Ok::<_, String>(config)
                })
                .await
                .map_err(|e| StoreError::Decode(format!("extracting zip upload: {e}")))?
                .map_err(StoreError::Decode);
                match extracted {
                    Ok(config) => config_text = config,
                    Err(e) => {
                        let _ = tokio::fs::remove_file(&extract_path).await;
                        return Err(e);
                    }
                }
                // Same directory as the upload, so this replaces the zip with
                // the database it held atomically.
                if let Err(e) = tokio::fs::rename(&extract_path, uploaded_path).await {
                    let _ = tokio::fs::remove_file(&extract_path).await;
                    return Err(StoreError::Decode(format!(
                        "replacing the upload with the extracted database: {e}"
                    )));
                }
            }
            UploadKind::Sqlite => {
                if uploaded_len > MAX_RESTORE_DB_BYTES {
                    return Err(StoreError::Decode(format!(
                        "uploaded database is larger than the {} GiB restore limit",
                        MAX_RESTORE_DB_BYTES / (1024 * 1024 * 1024)
                    )));
                }
            }
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
        // only be caught AFTER the swap at startup (`restore_apply`), when the
        // restore can no longer be refused up front.
        let invariant_result = if migrate_result.is_ok() {
            Some(super::invariants::verify_room_walk_order(&pool).await)
        } else {
            None
        };
        // The checks above only read the migration bookkeeping and the `rooms`
        // table, so a file whose damage is elsewhere (messages, users, indexes)
        // would pass them and only fail after the swap. `quick_check` walks every
        // b-tree page (not the index-versus-table cross-checks, so it stays
        // linear in file size). `(1)` stops at the first problem.
        let integrity_result: Option<Result<Vec<String>, sqlx::Error>> =
            if matches!(invariant_result, Some(Ok(_))) {
                Some(
                    sqlx::query_scalar("PRAGMA quick_check(1)")
                        .fetch_all(&pool)
                        .await,
                )
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
        let damage = match integrity_result {
            Some(Ok(rows)) if rows.len() == 1 && rows[0] == "ok" => None,
            Some(Ok(rows)) => Some(rows.join("; ")),
            Some(Err(e)) => Some(e.to_string()),
            None => None,
        };
        if let Some(damage) = damage {
            return Err(StoreError::Decode(format!(
                "uploaded file failed SQLite's integrity check ({damage}) — it is \
                 damaged and cannot be restored"
            )));
        }

        let staged_path = data_dir.join("pending_restore.staged.db");
        // A config staged earlier belongs to the database staged earlier. Clear
        // it before the new database lands, so no crash can leave the new
        // database next to the old one's settings (the worst a crash now leaves
        // is a database with no settings, which restores the data only).
        let _ =
            tokio::fs::remove_file(data_dir.join(crate::restore_config::STAGED_CONFIG_NAME)).await;
        // `rename` is atomic but fails across filesystems (EXDEV) — the
        // upload's temp file and data_dir are not guaranteed to share one,
        // so fall back to copy+delete on that specific failure.
        if tokio::fs::rename(uploaded_path, &staged_path)
            .await
            .is_err()
        {
            let len = tokio::fs::metadata(uploaded_path)
                .await
                .map_err(|e| StoreError::Decode(format!("stage restore file: {e}")))?
                .len();
            crate::disk_space::ensure_free_space(data_dir, len).map_err(StoreError::Decode)?;
            if let Err(e) = tokio::fs::copy(uploaded_path, &staged_path).await {
                // Don't leave a torn file where a later confirm could pick it up.
                let _ = tokio::fs::remove_file(&staged_path).await;
                return Err(StoreError::Decode(format!("stage restore file: {e}")));
            }
            let _ = tokio::fs::remove_file(uploaded_path).await;
        }

        // The staged config always describes the staged database: a bundle
        // without one clears any config staged by an earlier upload.
        if let Err(e) = stage_config(data_dir, config_text.as_deref()).await {
            let _ = tokio::fs::remove_file(&staged_path).await;
            return Err(e);
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
        Self::admin_apply_staged_restore_with(data_dir, true).await
    }

    /// [`Self::admin_apply_staged_restore`], choosing whether the `config.toml`
    /// staged with the database (from a backup bundle) is confirmed with it.
    /// With `include_config` false it is discarded and only the database is
    /// restored. Either way no config staged earlier can be left to be applied
    /// with a different database.
    ///
    /// # Errors
    /// As [`Self::admin_apply_staged_restore`].
    pub async fn admin_apply_staged_restore_with(
        data_dir: &Path,
        include_config: bool,
    ) -> Result<(), StoreError> {
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

        // A confirmed config left by an earlier restore is removed before the new
        // database is confirmed, so it can never pair with it.
        let _ =
            tokio::fs::remove_file(data_dir.join(crate::restore_config::PENDING_CONFIG_NAME)).await;
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

        // The database is confirmed; now its config. A stale confirmed config
        // from an earlier restore is removed first so it can never pair with
        // this database.
        let staged_config = data_dir.join(crate::restore_config::STAGED_CONFIG_NAME);
        let pending_config = data_dir.join(crate::restore_config::PENDING_CONFIG_NAME);
        if include_config && staged_config.exists() {
            if tokio::fs::rename(&staged_config, &pending_config)
                .await
                .is_err()
            {
                // Not fatal: the database restore stands without the settings.
                tracing::warn!(
                    "could not confirm the staged config.toml; restoring the database only"
                );
                let _ = tokio::fs::remove_file(&staged_config).await;
            }
        } else {
            let _ = tokio::fs::remove_file(&staged_config).await;
        }
        Ok(())
    }
}

/// The `config.toml` entry of a backup bundle, if it has one and it is usable.
/// Read into memory (it is a few KiB; anything over the cap is refused) and
/// checked to be TOML, so a bundle with a damaged config is refused when it is
/// staged rather than when it is applied.
pub(crate) fn extract_optional_config(zip_path: &Path) -> Result<Option<String>, String> {
    use std::io::Read as _;
    let file = std::fs::File::open(zip_path).map_err(|e| format!("reading zip: {e}"))?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| format!("reading zip: {e}"))?;
    let mut entry = match archive.by_name("config.toml") {
        Ok(e) => e,
        Err(zip::result::ZipError::FileNotFound) => return Ok(None),
        Err(e) => return Err(format!("reading config.toml from the zip: {e}")),
    };
    let mut text = String::new();
    (&mut entry)
        .take(crate::restore_config::MAX_CONFIG_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|e| format!("reading config.toml from the zip: {e}"))?;
    if text.len() as u64 > crate::restore_config::MAX_CONFIG_BYTES {
        return Err("the config.toml in the backup is too large".into());
    }
    crate::restore_config::validate_staged_config(&text)?;
    Ok(Some(text))
}

/// Stage (or, with `None`, clear) the config that goes with the staged
/// database. Written private and flushed, as the database is.
async fn stage_config(data_dir: &Path, text: Option<&str>) -> Result<(), StoreError> {
    let path = data_dir.join(crate::restore_config::STAGED_CONFIG_NAME);
    let Some(text) = text else {
        let _ = tokio::fs::remove_file(&path).await;
        return Ok(());
    };
    let text = text.to_owned();
    let data_dir_owned = data_dir.to_path_buf();
    tokio::task::spawn_blocking(move || -> std::io::Result<()> {
        use std::io::Write as _;
        let mut f = create_private(&path)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        crate::restore_stage::hand_to_dir_owner(data_dir_owned.as_path(), &path);
        Ok(())
    })
    .await
    .map_err(|e| StoreError::Decode(format!("staging config.toml: {e}")))?
    .map_err(|e| StoreError::Decode(format!("staging config.toml: {e}")))
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

/// The largest database restore accepts, whether uploaded raw or extracted
/// from a zip. The web upload caps the file itself at 2 GiB; this is the bound
/// on what a zip may expand to, so a small crafted archive can't fill the disk.
const MAX_RESTORE_DB_BYTES: u64 = 4 * 1024 * 1024 * 1024;

enum UploadKind {
    Sqlite,
    Zip,
}

/// Decide what an upload is from its first bytes alone.
async fn classify_upload(path: &Path) -> Result<UploadKind, StoreError> {
    use tokio::io::AsyncReadExt;
    let mut head = Vec::with_capacity(16);
    tokio::fs::File::open(path)
        .await
        .map_err(|e| StoreError::Decode(format!("read uploaded file: {e}")))?
        .take(16)
        .read_to_end(&mut head)
        .await
        .map_err(|e| StoreError::Decode(format!("read uploaded file: {e}")))?;
    if head.starts_with(b"PK\x03\x04") {
        Ok(UploadKind::Zip)
    } else if head.starts_with(b"SQLite format 3\0") {
        Ok(UploadKind::Sqlite)
    } else {
        Err(StoreError::Decode(
            "not a SQLite database file or a recognized backup zip (bad header)".into(),
        ))
    }
}

/// How much of a zip entry is copied between free-space re-checks.
const COPY_CHUNK_BYTES: u64 = 64 * 1024 * 1024;

/// Create (or truncate) `path` writable and, on Unix, owner-only (0600) from the
/// moment it exists. The extracted file replaces the upload and becomes the
/// live database, which holds password hashes; the upload itself is created
/// 0600 by the callers, so the extracted copy must not be more permissive.
fn create_private(path: &Path) -> std::io::Result<std::fs::File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600);
    }
    opts.open(path)
}

/// Stream the single `.db`-named entry of the zip at `zip_path` to `out_path`
/// and return how many bytes were written. Rejects an archive with zero or more
/// than one `.db` entry rather than guessing which one is the database.
///
/// Nothing is sized from the archive's own claims: the declared size is only
/// used to refuse early and to check free space, and the copy is capped at
/// `max_bytes` regardless of what the entry says, so a forged size header can
/// neither trigger a huge allocation nor make the extraction outgrow the cap.
/// On any error the partial output is removed. Runs synchronously; callers on
/// an async runtime should wrap it in `spawn_blocking`.
fn extract_single_db_from_zip(
    zip_path: &Path,
    out_path: &Path,
    max_bytes: u64,
) -> Result<u64, String> {
    extract_single_db_with_chunk(zip_path, out_path, max_bytes, COPY_CHUNK_BYTES)
}

/// [`extract_single_db_from_zip`] with the free-space re-check interval as a
/// parameter, so tests can cross chunk boundaries with small files.
fn extract_single_db_with_chunk(
    zip_path: &Path,
    out_path: &Path,
    max_bytes: u64,
    chunk: u64,
) -> Result<u64, String> {
    use std::io::Read as _;

    let file = std::fs::File::open(zip_path).map_err(|e| format!("reading zip: {e}"))?;
    let mut archive = zip::ZipArchive::new(std::io::BufReader::new(file))
        .map_err(|e| format!("reading zip: {e}"))?;
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

    let declared = entry.size();
    if declared > max_bytes {
        return Err(format!(
            "the database in the zip is larger than the {} GiB restore limit",
            max_bytes / (1024 * 1024 * 1024)
        ));
    }
    let out_dir = out_path.parent().unwrap_or_else(|| Path::new("."));
    crate::disk_space::ensure_free_space(out_dir, declared)?;

    let copied = (|| -> Result<u64, String> {
        let mut out =
            create_private(out_path).map_err(|e| format!("creating extracted database: {e}"))?;
        let mut total: u64 = 0;
        loop {
            // Never ask for more than one chunk, nor for more than what is
            // left before the cap plus one byte (so overshooting is detected).
            let want = chunk.min(max_bytes.saturating_add(1) - total);
            let n = std::io::copy(&mut (&mut entry).take(want), &mut out)
                .map_err(|e| format!("extracting zip entry: {e}"))?;
            total += n;
            if total > max_bytes {
                return Err(format!(
                    "the database in the zip expands past the {} GiB restore limit",
                    max_bytes / (1024 * 1024 * 1024)
                ));
            }
            if n < want {
                break;
            }
            // The declared size was only a hint. Re-check the disk as the
            // real data arrives, so an entry that understates its size can't
            // fill the volume the live database is on.
            crate::disk_space::ensure_free_space(out_dir, 0)?;
        }
        out.sync_all()
            .map_err(|e| format!("writing extracted database: {e}"))?;
        Ok(total)
    })();

    if copied.is_err() {
        let _ = std::fs::remove_file(out_path);
    }
    copied
}

#[cfg(test)]
mod tests {
    use super::{
        classify_upload, extract_single_db_from_zip, extract_single_db_with_chunk, StoreError,
        UploadKind,
    };
    use std::path::PathBuf;

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

    const NO_LIMIT: u64 = 1 << 30;

    /// Write `bytes` as `in.zip` in a temp dir and extract to `out.db` there.
    fn extract(bytes: &[u8], max: u64) -> (tempfile::TempDir, PathBuf, Result<u64, String>) {
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("in.zip");
        let out = dir.path().join("out.db");
        std::fs::write(&zip_path, bytes).unwrap();
        let r = extract_single_db_from_zip(&zip_path, &out, max);
        (dir, out, r)
    }

    #[test]
    fn extract_single_db_from_zip_finds_the_lone_db_entry() {
        let zip_bytes = build_test_zip(&[
            ("backup-2026-09-04.db", b"SQLite format 3\0fake db bytes"),
            ("config.toml", b"[bbs]\nname = \"Test\"\n"),
        ]);
        let (_dir, out, r) = extract(&zip_bytes, NO_LIMIT);
        assert_eq!(r.expect("a single .db entry must extract"), 29);
        assert_eq!(
            std::fs::read(out).unwrap(),
            b"SQLite format 3\0fake db bytes"
        );
    }

    #[test]
    fn extract_single_db_from_zip_rejects_no_db_entry() {
        let zip_bytes = build_test_zip(&[("config.toml", b"[bbs]\n")]);
        let (_dir, out, r) = extract(&zip_bytes, NO_LIMIT);
        assert!(
            r.is_err(),
            "a zip with no .db entry must be rejected, not silently accepted"
        );
        assert!(!out.exists());
    }

    #[test]
    fn extract_single_db_from_zip_rejects_ambiguous_multiple_db_entries() {
        let zip_bytes = build_test_zip(&[
            ("one.db", b"SQLite format 3\0aaa"),
            ("two.db", b"SQLite format 3\0bbb"),
        ]);
        let (_dir, out, r) = extract(&zip_bytes, NO_LIMIT);
        assert!(
            r.is_err(),
            "an ambiguous zip with two .db entries must be rejected rather \
             than silently picking one"
        );
        assert!(!out.exists());
    }

    // Listing and deleting only read the directory: no database is open.
    #[tokio::test]
    async fn backups_can_be_listed_and_deleted_without_a_database() {
        use crate::db::Database;
        let dir = tempfile::tempdir().unwrap();
        let d = dir.path();
        let path = d.to_string_lossy().into_owned();
        // A zip with settings, a zip without, a legacy db with a sidecar config,
        // and files that are not backups.
        let db = d.join("b.db");
        std::fs::write(&db, b"db").unwrap();
        let cfg = d.join("config.toml");
        std::fs::write(&cfg, "[bbs]\nname = \"X\"\n").unwrap();
        crate::backup_bundle::write_bundle(&db, "b.db", Some(&cfg), &d.join("backup_a.zip"))
            .unwrap();
        crate::backup_bundle::write_bundle(&db, "b.db", None, &d.join("backup_b.zip")).unwrap();
        std::fs::write(d.join("backup_c.db"), b"db").unwrap();
        std::fs::write(d.join("backup_c_config.toml"), b"x").unwrap();
        std::fs::write(d.join("notes.txt"), b"x").unwrap();
        std::fs::remove_file(&db).unwrap();
        std::fs::remove_file(&cfg).unwrap();

        let listed = Database::admin_list_backups(&path).await.unwrap();
        let mut names: Vec<(&str, bool)> = listed
            .iter()
            .map(|r| (r.filename.as_str(), r.config_filename.is_some()))
            .collect();
        names.sort();
        assert_eq!(
            names,
            [
                ("backup_a.zip", true),
                ("backup_b.zip", false),
                ("backup_c.db", true)
            ]
        );

        Database::admin_delete_backup(&path, "backup_a.zip")
            .await
            .unwrap();
        Database::admin_delete_backup(&path, "backup_c.db")
            .await
            .unwrap();
        assert!(
            !d.join("backup_c_config.toml").exists(),
            "the sidecar goes with it"
        );
        assert!(d.join("notes.txt").exists());
        assert_eq!(Database::admin_list_backups(&path).await.unwrap().len(), 1);

        // Names that leave the directory are refused, and nothing outside goes.
        let outside = d.parent().unwrap().join("outside-victim.zip");
        std::fs::write(&outside, b"keep").unwrap();
        let outside_rel = format!("../{}", outside.file_name().unwrap().to_string_lossy());
        assert!(Database::admin_delete_backup(&path, &outside_rel)
            .await
            .is_err());
        assert!(
            outside.exists(),
            "a file outside the backup directory must survive"
        );
        let _ = std::fs::remove_file(&outside);
        // Only backups can be deleted: not the notes file next to them.
        assert!(Database::admin_delete_backup(&path, "notes.txt")
            .await
            .is_err());
        assert!(d.join("notes.txt").exists());
        // A backup that isn't there is "not found", not a storage failure.
        assert!(matches!(
            Database::admin_delete_backup(&path, "backup_missing.zip").await,
            Err(StoreError::NotFound)
        ));
        // The restore's own files are not backups, even in a shared directory.
        std::fs::write(d.join("pending_restore.db"), b"keep").unwrap();
        assert!(Database::admin_delete_backup(&path, "pending_restore.db")
            .await
            .is_err());
        assert!(d.join("pending_restore.db").exists());
        for bad in ["../x.zip", "a/b.zip", "..", ""] {
            assert!(
                Database::admin_delete_backup(&path, bad).await.is_err(),
                "{bad:?}"
            );
        }
        // Missing directory is an empty list, not an error.
        assert!(
            Database::admin_list_backups(&d.join("absent").to_string_lossy())
                .await
                .unwrap()
                .is_empty()
        );
    }

    // The restore side reads exactly what a backup bundle holds.
    #[test]
    fn the_config_a_bundle_carries_is_what_restore_extracts() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("b.db");
        let cfg = dir.path().join("config.toml");
        let zip = dir.path().join("b.zip");
        std::fs::write(&db, b"SQLite format 3 db").unwrap();
        std::fs::write(
            &cfg,
            "[bbs]
name = \"X\"
",
        )
        .unwrap();
        crate::backup_bundle::write_bundle(&db, "b.db", Some(&cfg), &zip).unwrap();
        assert_eq!(
            super::extract_optional_config(&zip).unwrap().as_deref(),
            Some(
                "[bbs]
name = \"X\"
"
            )
        );

        // A bundle with no config, and a zip with no config at all, give None.
        crate::backup_bundle::write_bundle(&db, "b.db", None, &zip).unwrap();
        assert_eq!(super::extract_optional_config(&zip).unwrap(), None);
    }

    #[test]
    fn a_config_that_is_not_toml_or_is_too_big_is_refused_when_extracted() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("b.db");
        std::fs::write(&db, b"db").unwrap();
        let bad = dir.path().join("bad.toml");
        std::fs::write(&bad, "[[[ nope").unwrap();
        let zip = dir.path().join("bad.zip");
        crate::backup_bundle::write_bundle(&db, "b.db", Some(&bad), &zip).unwrap();
        let err = super::extract_optional_config(&zip).unwrap_err();
        assert!(err.contains("not valid TOML"), "{err}");
    }

    #[test]
    fn extract_single_db_from_zip_rejects_a_non_zip_file() {
        let (_dir, out, r) = extract(b"not a zip at all", NO_LIMIT);
        assert!(r.is_err());
        assert!(!out.exists());
    }

    #[test]
    fn extract_rejects_an_entry_whose_declared_size_is_over_the_limit() {
        let zip_bytes = build_test_zip(&[("a.db", &[7u8; 4096])]);
        let (_dir, out, r) = extract(&zip_bytes, 1024);
        let err = r.unwrap_err();
        assert!(err.contains("restore limit"), "{err}");
        assert!(!out.exists());
    }

    // The declared size is attacker-controlled and must never size an
    // allocation. With the production cap, a forged 32-bit size below the cap is
    // harmless: only the bytes that are really there are read.
    #[test]
    fn a_forged_declared_size_below_the_cap_extracts_only_the_real_bytes() {
        let mut zip_bytes = build_test_zip(&[("a.db", b"SQLite format 3\0abc")]);
        // Central directory header: signature 0x02014b50, uncompressed size at +24.
        let cd = zip_bytes
            .windows(4)
            .rposition(|w| w == b"PK\x01\x02")
            .expect("central directory header");
        zip_bytes[cd + 24..cd + 28].copy_from_slice(&0xFFFF_FFF0u32.to_le_bytes());
        let (_dir, out, r) = extract(&zip_bytes, super::MAX_RESTORE_DB_BYTES);
        // Either the zip crate notices the mismatch or it extracts the real
        // content; what it must not do is allocate for the claim or write junk.
        if r.is_ok() {
            assert_eq!(std::fs::read(&out).unwrap(), b"SQLite format 3\0abc");
        } else {
            assert!(!out.exists(), "no partial output may be left behind");
        }
    }

    // The shape that made `Vec::with_capacity(entry.size())` abort the process:
    // a zip64 extra field declaring 2^46 bytes for a tiny entry. It is refused
    // by the size cap before anything is allocated or written.
    #[test]
    fn a_forged_zip64_size_is_refused_by_the_cap_not_allocated() {
        let mut buf = Vec::new();
        {
            use std::io::Write as _;
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default().large_file(true);
            zip.start_file("a.db", opts).unwrap();
            zip.write_all(b"SQLite format 3\0abc").unwrap();
            zip.finish().unwrap();
        }
        // Walk the central directory entry's extra fields to the zip64 one
        // (id 0x0001) and overwrite its first u64, the uncompressed size.
        let cd = buf
            .windows(4)
            .rposition(|w| w == b"PK\x01\x02")
            .expect("central directory header");
        let name_len = u16::from_le_bytes([buf[cd + 28], buf[cd + 29]]) as usize;
        let extra_len = u16::from_le_bytes([buf[cd + 30], buf[cd + 31]]) as usize;
        let mut at = cd + 46 + name_len;
        let end = at + extra_len;
        let mut patched = false;
        while at + 4 <= end {
            let id = u16::from_le_bytes([buf[at], buf[at + 1]]);
            let len = u16::from_le_bytes([buf[at + 2], buf[at + 3]]) as usize;
            if id == 0x0001 && len >= 8 {
                buf[at + 4..at + 12].copy_from_slice(&(1u64 << 46).to_le_bytes());
                patched = true;
                break;
            }
            at += 4 + len;
        }
        assert!(patched, "the test zip has no zip64 extra field to forge");

        let (_dir, out, r) = extract(&buf, super::MAX_RESTORE_DB_BYTES);
        let err = r.unwrap_err();
        assert!(err.contains("restore limit"), "{err}");
        assert!(!out.exists());
    }

    // A zip that claims to be small but inflates past the cap is stopped at
    // the cap, and what was written is removed.
    #[test]
    fn an_entry_that_inflates_past_the_limit_is_cut_off_and_cleaned_up() {
        let big = vec![0u8; 64 * 1024]; // deflates to almost nothing
        let mut buf = Vec::new();
        {
            use std::io::Write as _;
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut buf));
            let opts = zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated);
            zip.start_file("a.db", opts).unwrap();
            zip.write_all(&big).unwrap();
            zip.finish().unwrap();
        }
        // Understate the size to 100 bytes in the central directory (+24) and
        // the local header (+22), so only the streaming cap can catch it.
        for (sig, offset) in [(&b"PK\x01\x02"[..], 24), (&b"PK\x03\x04"[..], 22)] {
            let at = buf.windows(4).position(|w| w == sig).unwrap();
            buf[at + offset..at + offset + 4].copy_from_slice(&100u32.to_le_bytes());
        }
        let (_dir, out, r) = extract(&buf, 1024);
        assert!(r.is_err(), "{r:?}");
        assert!(!out.exists(), "the partial output must be removed");
    }

    #[test]
    fn a_database_larger_than_a_chunk_extracts_intact() {
        // 3 MiB payload in 64 KiB chunks: many chunk boundaries.
        let payload: Vec<u8> = (0..3 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
        let zip_bytes = build_test_zip(&[("big.db", &payload)]);
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("in.zip");
        let out = dir.path().join("out.db");
        std::fs::write(&zip_path, &zip_bytes).unwrap();
        let n = extract_single_db_with_chunk(&zip_path, &out, NO_LIMIT, 64 * 1024).unwrap();
        assert_eq!(n, payload.len() as u64);
        assert_eq!(std::fs::read(out).unwrap(), payload);
    }

    // An entry exactly as long as a whole number of chunks ends on a chunk
    // boundary: the loop must still terminate and report the right length.
    #[test]
    fn an_entry_that_is_an_exact_multiple_of_the_chunk_extracts_intact() {
        let payload = vec![0x5Au8; 4096];
        let zip_bytes = build_test_zip(&[("a.db", &payload)]);
        let dir = tempfile::tempdir().unwrap();
        let zip_path = dir.path().join("in.zip");
        let out = dir.path().join("out.db");
        std::fs::write(&zip_path, &zip_bytes).unwrap();
        let n = extract_single_db_with_chunk(&zip_path, &out, NO_LIMIT, 1024).unwrap();
        assert_eq!(n, 4096);
        assert_eq!(std::fs::read(out).unwrap(), payload);
    }

    // The extracted file becomes the live database, so it must not be readable
    // by other users even though it is created fresh next to the upload.
    #[cfg(unix)]
    #[test]
    fn the_extracted_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let zip_bytes = build_test_zip(&[("a.db", b"SQLite format 3\0abc")]);
        let (_dir, out, r) = extract(&zip_bytes, NO_LIMIT);
        r.unwrap();
        let mode = std::fs::metadata(out).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "{mode:o}");
    }

    #[tokio::test]
    async fn classify_reads_only_the_header() {
        let dir = tempfile::tempdir().unwrap();
        let sqlite = dir.path().join("a");
        std::fs::write(&sqlite, b"SQLite format 3\0rest").unwrap();
        assert!(matches!(
            classify_upload(&sqlite).await,
            Ok(UploadKind::Sqlite)
        ));

        let zip = dir.path().join("b");
        std::fs::write(&zip, b"PK\x03\x04rest").unwrap();
        assert!(matches!(classify_upload(&zip).await, Ok(UploadKind::Zip)));
    }

    #[tokio::test]
    async fn classify_rejects_bad_short_empty_and_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        for (name, bytes) in [
            ("junk", &b"hello world, not a database"[..]),
            ("short", &b"SQLite"[..]),
            ("empty", &b""[..]),
        ] {
            let p = dir.path().join(name);
            std::fs::write(&p, bytes).unwrap();
            assert!(
                matches!(classify_upload(&p).await, Err(StoreError::Decode(_))),
                "{name}"
            );
        }
        assert!(matches!(
            classify_upload(&dir.path().join("absent")).await,
            Err(StoreError::Decode(_))
        ));
    }
}
