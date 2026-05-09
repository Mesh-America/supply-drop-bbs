//! User block-list operations.

use super::{error::StoreError, Database};

#[allow(dead_code)]
impl Database {
    /// Record that `blocker` wants to hide `blocked`'s messages.  No-op if
    /// already blocked.
    pub(crate) async fn block_user(&self, blocker: &str, blocked: &str) -> Result<(), StoreError> {
        sqlx::query("INSERT OR IGNORE INTO user_blocks (blocker, blocked) VALUES (?, ?)")
            .bind(blocker)
            .bind(blocked)
            .execute(&self.write_pool)
            .await?;
        Ok(())
    }

    /// Remove a block previously placed by `blocker` on `blocked`.  No-op if
    /// not currently blocked.
    pub(crate) async fn unblock_user(
        &self,
        blocker: &str,
        blocked: &str,
    ) -> Result<(), StoreError> {
        sqlx::query("DELETE FROM user_blocks WHERE blocker = ? AND blocked = ?")
            .bind(blocker)
            .bind(blocked)
            .execute(&self.write_pool)
            .await?;
        Ok(())
    }

    /// Return `true` if `blocker` has blocked `blocked`.
    pub(crate) async fn is_blocking(
        &self,
        blocker: &str,
        blocked: &str,
    ) -> Result<bool, StoreError> {
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM user_blocks WHERE blocker = ? AND blocked = ?",
        )
        .bind(blocker)
        .bind(blocked)
        .fetch_one(&self.read_pool)
        .await?;
        Ok(count > 0)
    }
}
