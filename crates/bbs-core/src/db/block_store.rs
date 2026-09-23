//! User block-list operations.

use std::collections::HashSet;

use super::{error::StoreError, Database};
use crate::ids::MessageId;

/// What a block hid from its owner: messages the blocked user sent while the
/// block was up, which reading has already carried the read pointer past.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct HiddenWhileBlocked {
    /// How many such messages there are.
    pub count: u64,
    /// The oldest of them, for the reader to jump to.
    pub oldest: MessageId,
}

#[allow(dead_code)]
impl Database {
    /// Record that `blocker` wants to hide `blocked`'s messages.  No-op if
    /// already blocked.
    ///
    /// Stamps the newest message id at the moment the block goes up, which is
    /// what later tells [`Database::hidden_while_blocked`] where the block's
    /// effect begins.
    pub(crate) async fn block_user(&self, blocker: &str, blocked: &str) -> Result<(), StoreError> {
        let newest: Option<i64> = sqlx::query_scalar("SELECT MAX(id) FROM messages")
            .fetch_one(&self.read_pool)
            .await?;
        sqlx::query(
            "INSERT OR IGNORE INTO user_blocks (blocker, blocked, blocked_at_message_id)
             VALUES (?, ?, ?)",
        )
        .bind(blocker)
        .bind(blocked)
        .bind(newest.unwrap_or(0))
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

    /// What a live block on `blocked` has hidden from `blocker` so far:
    /// messages sent after the block went up that reading has already carried
    /// the read pointer past, so they won't turn up in `N` again.
    ///
    /// `None` when there are none, or when the block predates the column that
    /// records where it started — an unknown start can't be distinguished from
    /// "the whole history", and a guessed count is worse than none.
    ///
    /// Call this before removing the block row; it reads that row.
    pub(crate) async fn hidden_while_blocked(
        &self,
        blocker_id: i64,
        blocker: &str,
        blocked: &str,
    ) -> Result<Option<HiddenWhileBlocked>, StoreError> {
        let since: Option<i64> = sqlx::query_scalar(
            "SELECT blocked_at_message_id FROM user_blocks
              WHERE blocker = ? AND blocked = ?",
        )
        .bind(blocker)
        .bind(blocked)
        .fetch_optional(&self.read_pool)
        .await?
        .flatten();
        let Some(since) = since else {
            return Ok(None);
        };

        // A room message counts when the reader's pointer for that room has
        // already passed it. A DM counts on the mail pointer instead, which
        // is keyed by the mail room rather than by a room_messages row.
        let row: Option<(i64, Option<i64>)> = sqlx::query_as(
            r#"
            SELECT COUNT(*), MIN(m.id) FROM messages m
            JOIN room_messages rm ON rm.message_id = m.id
            JOIN user_room_state urs
              ON urs.room_id = rm.room_id AND urs.user_id = ?
            WHERE m.sender = ?
              AND m.id > ?
              AND urs.last_read_message_id IS NOT NULL
              AND m.id <= urs.last_read_message_id
            "#,
        )
        .bind(blocker_id)
        .bind(blocked)
        .bind(since)
        .fetch_optional(&self.read_pool)
        .await?;

        Ok(match row {
            Some((count, Some(oldest))) if count > 0 => Some(HiddenWhileBlocked {
                count: count as u64,
                oldest: MessageId::new(oldest),
            }),
            _ => None,
        })
    }

    /// Return the set of usernames blocked by `blocker`.
    pub(crate) async fn blocks_by(&self, blocker: &str) -> Result<HashSet<String>, StoreError> {
        let rows: Vec<String> =
            sqlx::query_scalar("SELECT blocked FROM user_blocks WHERE blocker = ?")
                .bind(blocker)
                .fetch_all(&self.read_pool)
                .await?;
        Ok(rows.into_iter().collect())
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
