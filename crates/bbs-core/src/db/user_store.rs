//! `UserStore` trait and its SQLite implementation on `Database`.

use super::{error::StoreError, Database};
use crate::{
    ids::UserId,
    timestamp::Timestamp,
    user::{User, UserStatus},
};
use async_trait::async_trait;
use bbs_plugin_api::{PermissionLevel, Username};

// ── Helpers ───────────────────────────────────────────────────────────

fn status_from_i64(n: i64) -> Result<UserStatus, StoreError> {
    match n {
        0 => Ok(UserStatus::Active),
        1 => Ok(UserStatus::Banned),
        2 => Ok(UserStatus::Deleted),
        other => Err(StoreError::Decode(format!(
            "unknown UserStatus discriminant {other}"
        ))),
    }
}

fn permission_from_i64(n: i64) -> Result<PermissionLevel, StoreError> {
    match n {
        0 => Ok(PermissionLevel::Unvalidated),
        10 => Ok(PermissionLevel::User),
        50 => Ok(PermissionLevel::Aide),
        100 => Ok(PermissionLevel::Sysop),
        other => Err(StoreError::Decode(format!(
            "unknown PermissionLevel discriminant {other}"
        ))),
    }
}

// ── Trait ─────────────────────────────────────────────────────────────

/// Read/write access to the `users` table.
///
/// Credential operations are deliberately absent; they live on the
/// internal `CredentialStore`, which is never exposed through
/// [`bbs_plugin_api::Host`].
#[async_trait]
pub trait UserStore: Send + Sync {
    /// Fetch a user by their stable integer ID. Returns `None` if no
    /// such row exists.
    async fn get_by_id(&self, id: UserId) -> Result<Option<User>, StoreError>;

    /// Fetch a user by their username. Returns `None` if not found.
    async fn get_by_username(&self, username: &Username) -> Result<Option<User>, StoreError>;

    /// List users, optionally filtered to a specific lifecycle status.
    ///
    /// Results are ordered by `created_at` ascending. `limit` caps the
    /// page size; `offset` skips that many rows for pagination.
    async fn list(
        &self,
        filter_status: Option<UserStatus>,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<User>, StoreError>;

    /// Insert a new user row and return its auto-assigned ID.
    ///
    /// The new user starts with `status = Active` and no
    /// `last_login_at`. Returns [`StoreError::Conflict`] if the
    /// username is already taken.
    async fn create(
        &self,
        username: &Username,
        display_name: Option<&str>,
        permission_level: PermissionLevel,
        created_at: Timestamp,
    ) -> Result<UserId, StoreError>;

    /// Update mutable fields on a user row.
    ///
    /// `None` outer = leave the current value unchanged.
    /// `Some(None)` for `display_name` = clear to NULL.
    /// `Some(Some(s))` = set to `s`.
    async fn update(
        &self,
        id: UserId,
        display_name: Option<Option<&str>>,
        status: Option<UserStatus>,
        permission_level: Option<PermissionLevel>,
        last_login_at: Option<Timestamp>,
    ) -> Result<(), StoreError>;

    /// Hard-delete a user row. Prefer `status = Deleted` for normal
    /// deactivation. Returns [`StoreError::IntegrityViolation`] if the
    /// user has authored messages (soft-delete instead).
    async fn hard_delete(&self, id: UserId) -> Result<(), StoreError>;
}

// ── Implementation ────────────────────────────────────────────────────

/// Map a raw query-row tuple to a `User`, validating enum discriminants.
fn map_user_row(
    id: i64,
    username: String,
    display_name: Option<String>,
    status: i64,
    permission_level: i64,
    created_at: String,
    last_login_at: Option<String>,
) -> Result<User, StoreError> {
    let status = status_from_i64(status)?;
    let permission_level = permission_from_i64(permission_level)?;
    let username = Username::new(username)
        .map_err(|e| StoreError::Decode(format!("invalid stored username: {e}")))?;
    let created_at = Timestamp::parse_rfc3339(&created_at)
        .map_err(|e| StoreError::Decode(format!("invalid created_at: {e}")))?;
    let last_login_at = last_login_at
        .as_deref()
        .map(Timestamp::parse_rfc3339)
        .transpose()
        .map_err(|e| StoreError::Decode(format!("invalid last_login_at: {e}")))?;
    Ok(User {
        id: UserId::new(id),
        username,
        display_name,
        status,
        permission_level,
        created_at,
        last_login_at,
    })
}

#[async_trait]
impl UserStore for Database {
    async fn get_by_id(&self, id: UserId) -> Result<Option<User>, StoreError> {
        let uid = id.as_i64();
        // "id!" asserts non-null to sqlx (INTEGER PRIMARY KEY is never null
        // in SQLite but sqlx can't infer that without an explicit NOT NULL).
        let row = sqlx::query!(
            r#"SELECT id AS "id!", username AS "username!", display_name,
                      status AS "status!", permission_level AS "permission_level!",
                      created_at AS "created_at!", last_login_at
               FROM users WHERE id = ?"#,
            uid
        )
        .fetch_optional(&self.read_pool)
        .await?;
        row.map(|r| {
            map_user_row(
                r.id,
                r.username,
                r.display_name,
                r.status,
                r.permission_level,
                r.created_at,
                r.last_login_at,
            )
        })
        .transpose()
    }

    async fn get_by_username(&self, username: &Username) -> Result<Option<User>, StoreError> {
        let name = username.as_str();
        let row = sqlx::query!(
            r#"SELECT id AS "id!", username AS "username!", display_name,
                      status AS "status!", permission_level AS "permission_level!",
                      created_at AS "created_at!", last_login_at
               FROM users WHERE username = ?"#,
            name
        )
        .fetch_optional(&self.read_pool)
        .await?;
        row.map(|r| {
            map_user_row(
                r.id,
                r.username,
                r.display_name,
                r.status,
                r.permission_level,
                r.created_at,
                r.last_login_at,
            )
        })
        .transpose()
    }

    async fn list(
        &self,
        filter_status: Option<UserStatus>,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<User>, StoreError> {
        // Split into two private helpers to avoid incompatible query! anonymous
        // struct types across match arms (a known sqlx limitation).
        if let Some(s) = filter_status {
            self.list_by_status(s, limit as i64, offset as i64).await
        } else {
            self.list_all_users(limit as i64, offset as i64).await
        }
    }

    async fn create(
        &self,
        username: &Username,
        display_name: Option<&str>,
        permission_level: PermissionLevel,
        created_at: Timestamp,
    ) -> Result<UserId, StoreError> {
        let name = username.as_str();
        let pl = permission_level as i64;
        let ts = created_at.to_rfc3339();
        let result = sqlx::query!(
            "INSERT INTO users (username, display_name, status, permission_level, created_at)
             VALUES (?, ?, 0, ?, ?)",
            name,
            display_name,
            pl,
            ts
        )
        .execute(&self.write_pool)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db_err) = e {
                if db_err.is_unique_violation() {
                    return StoreError::Conflict(format!("username '{name}' is already taken"));
                }
            }
            StoreError::Db(e)
        })?;
        Ok(UserId::new(result.last_insert_rowid()))
    }

    async fn update(
        &self,
        id: UserId,
        display_name: Option<Option<&str>>,
        status: Option<UserStatus>,
        permission_level: Option<PermissionLevel>,
        last_login_at: Option<Timestamp>,
    ) -> Result<(), StoreError> {
        let uid = id.as_i64();

        let current = sqlx::query!(
            r#"SELECT display_name, status AS "status!", permission_level AS "permission_level!",
                      last_login_at
               FROM users WHERE id = ?"#,
            uid
        )
        .fetch_optional(&self.read_pool)
        .await?
        .ok_or(StoreError::NotFound)?;

        let new_display: Option<String> = match display_name {
            None => current.display_name,
            Some(None) => None,
            Some(Some(s)) => Some(s.to_owned()),
        };
        let new_status = status.map(|s| s as i64).unwrap_or(current.status);
        let new_pl = permission_level
            .map(|p| p as i64)
            .unwrap_or(current.permission_level);
        let new_login: Option<String> = match last_login_at {
            Some(t) => Some(t.to_rfc3339()),
            None => current.last_login_at,
        };

        sqlx::query!(
            "UPDATE users SET display_name = ?, status = ?, permission_level = ?,
             last_login_at = ? WHERE id = ?",
            new_display,
            new_status,
            new_pl,
            new_login,
            uid
        )
        .execute(&self.write_pool)
        .await?;

        Ok(())
    }

    async fn hard_delete(&self, id: UserId) -> Result<(), StoreError> {
        let uid = id.as_i64();

        let mut tx = self.write_pool.begin().await?;

        let row = sqlx::query!(
            r#"SELECT username AS "username!" FROM users WHERE id = ?"#,
            uid
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::NotFound)?;

        let sender = row.username;
        let msg_count: i64 =
            sqlx::query_scalar!("SELECT COUNT(*) FROM messages WHERE sender = ?", sender)
                .fetch_one(&mut *tx)
                .await?;

        if msg_count > 0 {
            return Err(StoreError::IntegrityViolation(format!(
                "user '{sender}' has {msg_count} messages; soft-delete (status=Deleted) instead"
            )));
        }

        let rows = sqlx::query!("DELETE FROM users WHERE id = ?", uid)
            .execute(&mut *tx)
            .await?
            .rows_affected();

        tx.commit().await?;

        if rows == 0 {
            return Err(StoreError::NotFound);
        }
        Ok(())
    }
}

// ── Private helpers ───────────────────────────────────────────────────

impl Database {
    async fn list_all_users(&self, lim: i64, off: i64) -> Result<Vec<User>, StoreError> {
        let rows = sqlx::query!(
            r#"SELECT id AS "id!", username AS "username!", display_name,
                      status AS "status!", permission_level AS "permission_level!",
                      created_at AS "created_at!", last_login_at
               FROM users ORDER BY created_at LIMIT ? OFFSET ?"#,
            lim,
            off
        )
        .fetch_all(&self.read_pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                map_user_row(
                    r.id,
                    r.username,
                    r.display_name,
                    r.status,
                    r.permission_level,
                    r.created_at,
                    r.last_login_at,
                )
            })
            .collect()
    }

    async fn list_by_status(
        &self,
        status: UserStatus,
        lim: i64,
        off: i64,
    ) -> Result<Vec<User>, StoreError> {
        let discriminant = status as i64;
        let rows = sqlx::query!(
            r#"SELECT id AS "id!", username AS "username!", display_name,
                      status AS "status!", permission_level AS "permission_level!",
                      created_at AS "created_at!", last_login_at
               FROM users WHERE status = ? ORDER BY created_at LIMIT ? OFFSET ?"#,
            discriminant,
            lim,
            off
        )
        .fetch_all(&self.read_pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                map_user_row(
                    r.id,
                    r.username,
                    r.display_name,
                    r.status,
                    r.permission_level,
                    r.created_at,
                    r.last_login_at,
                )
            })
            .collect()
    }
}
