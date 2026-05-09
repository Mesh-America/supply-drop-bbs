//! Persistent node-credential store — mesh node → user auto-login.
//!
//! Wraps the `node_credentials` table (migration 0006). Operations are
//! `pub(crate)` only; the public surface is through `BbsHost::mesh_node_*`.

use super::{error::StoreError, Database};
use crate::{ids::UserId, timestamp::Timestamp};
use sqlx::Row;

pub(crate) struct NodeCredentialStore<'db> {
    db: &'db Database,
}

#[allow(dead_code)]
impl<'db> NodeCredentialStore<'db> {
    pub(crate) fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// Insert or refresh the binding for a node pubkey prefix.
    pub(crate) async fn upsert(
        &self,
        prefix: &[u8; 6],
        user_id: UserId,
        now: Timestamp,
    ) -> Result<(), StoreError> {
        let blob: &[u8] = prefix;
        let uid = user_id.as_i64();
        let ts = now.to_rfc3339();
        sqlx::query(
            r#"
            INSERT INTO node_credentials (pubkey_prefix, user_id, last_auth)
            VALUES (?, ?, ?)
            ON CONFLICT(pubkey_prefix) DO UPDATE
              SET user_id   = excluded.user_id,
                  last_auth = excluded.last_auth
            "#,
        )
        .bind(blob)
        .bind(uid)
        .bind(&ts)
        .execute(&self.db.write_pool)
        .await?;
        Ok(())
    }

    /// Look up the user bound to this prefix, enforcing `ttl_days`.
    ///
    /// Returns `None` when no binding exists or it has expired.
    pub(crate) async fn lookup(
        &self,
        prefix: &[u8; 6],
        ttl_days: u32,
    ) -> Result<Option<UserId>, StoreError> {
        let blob: &[u8] = prefix;
        let ttl = format!("-{ttl_days} days");
        let row = sqlx::query(
            r#"
            SELECT user_id FROM node_credentials
            WHERE pubkey_prefix = ?
              AND last_auth >= datetime('now', ?)
            "#,
        )
        .bind(blob)
        .bind(&ttl)
        .fetch_optional(&self.db.read_pool)
        .await?;

        match row {
            None => Ok(None),
            Some(r) => {
                let uid: i64 = r.try_get("user_id")?;
                Ok(Some(UserId::new(uid)))
            }
        }
    }

    /// Remove the binding for this prefix (called on explicit logout).
    pub(crate) async fn delete(&self, prefix: &[u8; 6]) -> Result<(), StoreError> {
        let blob: &[u8] = prefix;
        sqlx::query("DELETE FROM node_credentials WHERE pubkey_prefix = ?")
            .bind(blob)
            .execute(&self.db.write_pool)
            .await?;
        Ok(())
    }
}
