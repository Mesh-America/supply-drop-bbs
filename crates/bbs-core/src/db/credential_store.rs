//! Internal credential store — argon2id password hashing.
//!
//! This module is `pub(crate)` only. It is never exposed through
//! `bbs_plugin_api::Host`; only the host's own authentication flow
//! calls it. Plugins have no direct access to password hashes.

use super::{error::StoreError, Database};
use crate::{ids::UserId, timestamp::Timestamp};
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};

/// Internal-only credential operations for `Database`.
pub(crate) struct CredentialStore<'db> {
    db: &'db Database,
}

// Auth workflow not yet wired up; callers land in a future commit.
#[allow(dead_code)]
impl<'db> CredentialStore<'db> {
    /// Borrow credential operations from a `Database` reference.
    pub(crate) fn new(db: &'db Database) -> Self {
        Self { db }
    }

    /// Hash `password` with argon2id and store/replace the PHC string.
    pub(crate) async fn set_password(
        &self,
        user_id: UserId,
        password: &str,
        now: Timestamp,
    ) -> Result<(), StoreError> {
        let salt = SaltString::generate(&mut OsRng);
        let phc = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .map_err(|e| StoreError::Decode(format!("argon2 hash error: {e}")))?
            .to_string();
        let uid = user_id.as_i64();
        let ts = now.to_rfc3339();

        sqlx::query!(
            r#"
            INSERT INTO user_credentials (user_id, phc_hash, updated_at)
            VALUES (?, ?, ?)
            ON CONFLICT(user_id) DO UPDATE
              SET phc_hash   = excluded.phc_hash,
                  updated_at = excluded.updated_at
            "#,
            uid,
            phc,
            ts
        )
        .execute(&self.db.write_pool)
        .await?;

        Ok(())
    }

    /// Verify `candidate` against the stored hash for `user_id`.
    ///
    /// Returns `Ok(false)` if no credential row exists (user was
    /// created without a password, e.g. a system account).
    ///
    /// On success, transparently rehashes if the stored parameters are
    /// weaker than the current default (argon2 parameter migration).
    pub(crate) async fn verify_password(
        &self,
        user_id: UserId,
        candidate: &str,
        now: Timestamp,
    ) -> Result<bool, StoreError> {
        let uid = user_id.as_i64();
        let row = sqlx::query!(
            r#"SELECT phc_hash AS "phc_hash!" FROM user_credentials WHERE user_id = ?"#,
            uid
        )
        .fetch_optional(&self.db.read_pool)
        .await?;

        let phc_str = match row {
            None => return Ok(false),
            Some(r) => r.phc_hash,
        };

        let parsed = PasswordHash::new(&phc_str)
            .map_err(|e| StoreError::Decode(format!("malformed PHC hash in DB: {e}")))?;

        let ok = Argon2::default()
            .verify_password(candidate.as_bytes(), &parsed)
            .is_ok();

        // Transparent rehash: on successful verification, re-hash if the
        // stored parameters differ from the current Argon2 default.
        // Best-effort: a rehash failure must never block a successful login.
        if ok {
            let needs_rehash = Argon2::default()
                .hash_password(b"probe", &SaltString::generate(&mut OsRng))
                .ok()
                .map(|fresh| {
                    // Compare the "m=" param of the stored vs fresh PHC string.
                    let stored_m = parsed.params.get_str("m");
                    let fresh_m = fresh.params.get_str("m");
                    stored_m != fresh_m
                })
                .unwrap_or(false);

            if needs_rehash {
                let _ = self.set_password(user_id, candidate, now).await;
            }
        }

        Ok(ok)
    }
}
