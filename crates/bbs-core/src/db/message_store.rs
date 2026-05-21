//! `MessageStore` trait and its SQLite implementation on `Database`.

use super::{error::StoreError, Database};
use crate::{
    ids::{MessageId, RoomId, UserId},
    message::Message,
    timestamp::Timestamp,
};
use async_trait::async_trait;
use bbs_plugin_api::Username;

// ── Page type ─────────────────────────────────────────────────────────

/// A page of messages from a cursor-based query.
pub struct MessagePage {
    /// The messages in this page, in ascending ID order.
    pub messages: Vec<Message>,
    /// Pass this to the next call as `after_id` to get the next page.
    /// `None` means this is the last page.
    pub next_cursor: Option<MessageId>,
}

// ── Helpers ───────────────────────────────────────────────────────────

fn map_message_row(
    id: i64,
    sender: String,
    recipient: Option<String>,
    content: String,
    timestamp: String,
) -> Result<Message, StoreError> {
    let sender = Username::try_from(sender)
        .map_err(|e| StoreError::Decode(format!("invalid stored sender: {e}")))?;
    let recipient = recipient
        .map(Username::try_from)
        .transpose()
        .map_err(|e| StoreError::Decode(format!("invalid stored recipient: {e}")))?;
    let timestamp = Timestamp::parse_rfc3339(&timestamp)
        .map_err(|e| StoreError::Decode(format!("invalid timestamp: {e}")))?;
    Ok(Message {
        id: MessageId::new(id),
        sender,
        recipient,
        content,
        timestamp,
    })
}

fn build_page(mut rows: Vec<Message>, limit: u32) -> MessagePage {
    let has_more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    rows.sort_by_key(|m| m.id);
    // Cursor = last item in this page; next call passes it as after_id
    // (WHERE id > cursor) to continue from where this page left off.
    let next_cursor = if has_more {
        rows.last().map(|m| m.id)
    } else {
        None
    };
    MessagePage {
        messages: rows,
        next_cursor,
    }
}

// ── Trait ─────────────────────────────────────────────────────────────

/// Read/write access to the `messages` table, the `room_messages`
/// join table, and the `user_room_state` read-pointer table.
#[async_trait]
pub trait MessageStore: Send + Sync {
    /// Fetch a message by ID.
    async fn get_by_id(&self, id: MessageId) -> Result<Option<Message>, StoreError>;

    /// List messages posted to a room, in ascending ID order.
    ///
    /// Pass `after_id = None` to start from the beginning.
    /// Pass `after_id = page.next_cursor` to fetch the next page.
    async fn list_in_room(
        &self,
        room_id: RoomId,
        after_id: Option<MessageId>,
        limit: u32,
    ) -> Result<MessagePage, StoreError>;

    /// List direct messages involving `username` (as sender or
    /// recipient), in ascending ID order.
    async fn list_direct(
        &self,
        username: &Username,
        after_id: Option<MessageId>,
        limit: u32,
    ) -> Result<MessagePage, StoreError>;

    /// Post a message to a room. Creates the `messages` row and the
    /// `room_messages` join row atomically.
    async fn post_to_room(
        &self,
        room_id: RoomId,
        sender: &Username,
        content: &str,
        timestamp: Timestamp,
    ) -> Result<MessageId, StoreError>;

    /// Post a direct message from `sender` to `recipient`.
    async fn post_direct(
        &self,
        sender: &Username,
        recipient: &Username,
        content: &str,
        timestamp: Timestamp,
    ) -> Result<MessageId, StoreError>;

    /// Return `true` if `message_id` exists in `room_id`'s join table.
    async fn is_in_room(&self, message_id: MessageId, room_id: RoomId) -> Result<bool, StoreError>;

    /// Delete a message by ID. Returns `Ok(false)` if the row did not
    /// exist (idempotent). Cascades to `room_messages`; the
    /// `user_room_state` pointer goes NULL (ON DELETE SET NULL).
    async fn delete(&self, id: MessageId) -> Result<bool, StoreError>;

    /// Advance the read pointer for a user in a room. Only advances
    /// forward; calling with a message ID earlier than the current
    /// pointer is a no-op.
    async fn mark_read(
        &self,
        user_id: UserId,
        room_id: RoomId,
        message_id: MessageId,
    ) -> Result<(), StoreError>;

    /// Count unread messages in a room for a user.
    ///
    /// Returns 0 when the user has never visited the room **and** there are
    /// no messages, and also when the read pointer was reset to `NULL` by
    /// `ON DELETE SET NULL` (the `user_room_state` row exists but the pointer
    /// is `NULL`).  The latter prevents stale "N new" counters after a sysop
    /// deletes the exact message a user last read.
    async fn unread_count(&self, user_id: UserId, room_id: RoomId) -> Result<u64, StoreError>;

    /// Count unread direct messages involving `username`.
    ///
    /// Uses the same read-pointer stored in `user_room_state` under `room_id`
    /// (the Mail room). Call this instead of [`unread_count`](Self::unread_count)
    /// for the Mail room, where DMs live in `messages` (not `room_messages`).
    async fn unread_direct_count(
        &self,
        username: &Username,
        user_id: UserId,
        room_id: RoomId,
    ) -> Result<u64, StoreError>;

    /// Return the user's last-read message ID in a room, or `None`
    /// if they have never visited it **or** if the pointer was reset by
    /// `ON DELETE SET NULL`.
    ///
    /// Use [`has_read_state`](Self::has_read_state) to distinguish the two
    /// `None` cases when the difference matters.
    async fn get_last_read(
        &self,
        user_id: UserId,
        room_id: RoomId,
    ) -> Result<Option<MessageId>, StoreError>;

    /// Return `true` if a `user_room_state` row exists for `(user_id, room_id)`.
    ///
    /// Distinguishes "never visited this room" (`get_last_read` returns `None`
    /// *and* `has_read_state` returns `false`) from "visited but read-pointer
    /// was reset by a message deletion" (`get_last_read` returns `None` *and*
    /// `has_read_state` returns `true`).
    async fn has_read_state(&self, user_id: UserId, room_id: RoomId) -> Result<bool, StoreError>;

    /// Return the `limit` most recent messages in a room, newest first.
    async fn list_recent_in_room(
        &self,
        room_id: RoomId,
        limit: u32,
    ) -> Result<Vec<Message>, StoreError>;

    /// Count all messages in a room (via the `room_messages` join table).
    async fn count_in_room(&self, room_id: RoomId) -> Result<u64, StoreError>;

    /// Count all direct messages involving `username` (as sender or recipient).
    async fn count_direct(&self, username: &Username) -> Result<u64, StoreError>;

    /// Fetch the single message in `room_id` immediately after `after`.
    ///
    /// `after = None` returns the first (oldest) message.
    async fn next_in_room(
        &self,
        room_id: RoomId,
        after: Option<MessageId>,
    ) -> Result<Option<Message>, StoreError>;

    /// Fetch the single message in `room_id` immediately before `before`.
    ///
    /// `before = None` returns the last (newest) message.
    async fn prev_in_room(
        &self,
        room_id: RoomId,
        before: Option<MessageId>,
    ) -> Result<Option<Message>, StoreError>;

    /// Fetch the single direct message involving `username` immediately after `after`.
    ///
    /// `after = None` returns the first (oldest) DM.
    async fn next_direct(
        &self,
        username: &Username,
        after: Option<MessageId>,
    ) -> Result<Option<Message>, StoreError>;

    /// Fetch the single direct message involving `username` immediately before `before`.
    ///
    /// `before = None` returns the last (newest) DM.
    async fn prev_direct(
        &self,
        username: &Username,
        before: Option<MessageId>,
    ) -> Result<Option<Message>, StoreError>;
}

// ── Implementation ────────────────────────────────────────────────────

#[async_trait]
impl MessageStore for Database {
    async fn get_by_id(&self, id: MessageId) -> Result<Option<Message>, StoreError> {
        let mid = id.as_i64();
        let row = sqlx::query!(
            r#"SELECT id AS "id!", sender AS "sender!", recipient, content AS "content!",
                      timestamp AS "timestamp!"
               FROM messages WHERE id = ?"#,
            mid
        )
        .fetch_optional(&self.read_pool)
        .await?;
        row.map(|r| map_message_row(r.id, r.sender, r.recipient, r.content, r.timestamp))
            .transpose()
    }

    async fn list_in_room(
        &self,
        room_id: RoomId,
        after_id: Option<MessageId>,
        limit: u32,
    ) -> Result<MessagePage, StoreError> {
        let rid = room_id.as_i64();
        // Use i64::MIN as "no cursor" sentinel — AUTOINCREMENT IDs are always > 0.
        let after = after_id.map(|id| id.as_i64()).unwrap_or(i64::MIN);
        let fetch = (limit + 1) as i64;

        let rows = sqlx::query!(
            r#"
            SELECT m.id AS "id!", m.sender AS "sender!", m.recipient,
                   m.content AS "content!", m.timestamp AS "timestamp!"
            FROM messages m
            JOIN room_messages rm ON rm.message_id = m.id
            WHERE rm.room_id = ? AND m.id > ?
            ORDER BY m.id
            LIMIT ?
            "#,
            rid,
            after,
            fetch
        )
        .fetch_all(&self.read_pool)
        .await?;

        let messages: Vec<Message> = rows
            .into_iter()
            .map(|r| map_message_row(r.id, r.sender, r.recipient, r.content, r.timestamp))
            .collect::<Result<_, _>>()?;

        Ok(build_page(messages, limit))
    }

    async fn list_direct(
        &self,
        username: &Username,
        after_id: Option<MessageId>,
        limit: u32,
    ) -> Result<MessagePage, StoreError> {
        let uname = username.as_str();
        let after = after_id.map(|id| id.as_i64()).unwrap_or(i64::MIN);
        let fetch = (limit + 1) as i64;

        let rows = sqlx::query!(
            r#"
            SELECT id AS "id!", sender AS "sender!", recipient,
                   content AS "content!", timestamp AS "timestamp!"
            FROM messages
            WHERE (sender = ? OR recipient = ?)
              AND recipient IS NOT NULL
              AND id > ?
            ORDER BY id
            LIMIT ?
            "#,
            uname,
            uname,
            after,
            fetch
        )
        .fetch_all(&self.read_pool)
        .await?;

        let messages: Vec<Message> = rows
            .into_iter()
            .map(|r| map_message_row(r.id, r.sender, r.recipient, r.content, r.timestamp))
            .collect::<Result<_, _>>()?;

        Ok(build_page(messages, limit))
    }

    async fn post_to_room(
        &self,
        room_id: RoomId,
        sender: &Username,
        content: &str,
        timestamp: Timestamp,
    ) -> Result<MessageId, StoreError> {
        let rid = room_id.as_i64();
        let snd = sender.as_str();
        let ts = timestamp.to_rfc3339();

        let mut tx = self.write_pool.begin().await?;

        let result = sqlx::query!(
            "INSERT INTO messages (sender, recipient, content, timestamp)
             VALUES (?, NULL, ?, ?)",
            snd,
            content,
            ts
        )
        .execute(&mut *tx)
        .await?;

        let mid = result.last_insert_rowid();

        sqlx::query!(
            "INSERT INTO room_messages (room_id, message_id) VALUES (?, ?)",
            rid,
            mid
        )
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok(MessageId::new(mid))
    }

    async fn post_direct(
        &self,
        sender: &Username,
        recipient: &Username,
        content: &str,
        timestamp: Timestamp,
    ) -> Result<MessageId, StoreError> {
        let snd = sender.as_str();
        let rcpt = recipient.as_str();
        let ts = timestamp.to_rfc3339();

        let result = sqlx::query!(
            "INSERT INTO messages (sender, recipient, content, timestamp)
             VALUES (?, ?, ?, ?)",
            snd,
            rcpt,
            content,
            ts
        )
        .execute(&self.write_pool)
        .await?;

        Ok(MessageId::new(result.last_insert_rowid()))
    }

    async fn is_in_room(&self, message_id: MessageId, room_id: RoomId) -> Result<bool, StoreError> {
        let mid = message_id.as_i64();
        let rid = room_id.as_i64();
        let count: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM room_messages WHERE message_id = ? AND room_id = ?",
        )
        .bind(mid)
        .bind(rid)
        .fetch_one(&self.read_pool)
        .await?;
        Ok(count > 0)
    }

    async fn delete(&self, id: MessageId) -> Result<bool, StoreError> {
        let mid = id.as_i64();

        // Before deleting, rescue any read pointers that land on this message.
        // Move them to the highest message_id in the same room that is strictly
        // less than `id`.  If no earlier message exists (this was the first),
        // the subquery returns NULL and the FK's ON DELETE SET NULL fires; the
        // `unread_count` query treats NULL-with-existing-row as "all caught up".
        //
        // Only room messages are in `room_messages`; for DMs the subquery
        // returns NULL (a no-op since ON DELETE SET NULL handles it anyway).
        sqlx::query!(
            r#"
            UPDATE user_room_state
            SET last_read_message_id = (
                SELECT MAX(rm2.message_id)
                FROM room_messages rm2
                JOIN room_messages rm1 ON rm1.room_id = rm2.room_id
                WHERE rm1.message_id = ?
                  AND rm2.message_id < ?
            )
            WHERE last_read_message_id = ?
            "#,
            mid,
            mid,
            mid
        )
        .execute(&self.write_pool)
        .await?;

        let rows = sqlx::query!("DELETE FROM messages WHERE id = ?", mid)
            .execute(&self.write_pool)
            .await?
            .rows_affected();
        Ok(rows > 0)
    }

    async fn mark_read(
        &self,
        user_id: UserId,
        room_id: RoomId,
        message_id: MessageId,
    ) -> Result<(), StoreError> {
        let uid = user_id.as_i64();
        let rid = room_id.as_i64();
        let mid = message_id.as_i64();

        // Upsert: insert or advance forward only (COALESCE handles NULL pointer).
        sqlx::query!(
            r#"
            INSERT INTO user_room_state (user_id, room_id, last_read_message_id)
            VALUES (?, ?, ?)
            ON CONFLICT(user_id, room_id) DO UPDATE
              SET last_read_message_id = MAX(excluded.last_read_message_id,
                                            COALESCE(last_read_message_id, 0))
            "#,
            uid,
            rid,
            mid
        )
        .execute(&self.write_pool)
        .await?;

        Ok(())
    }

    async fn unread_count(&self, user_id: UserId, room_id: RoomId) -> Result<u64, StoreError> {
        let uid = user_id.as_i64();
        let rid = room_id.as_i64();

        // Distinguish three cases:
        //   urs.user_id IS NULL  — no state row: user never visited → all messages unread.
        //   urs.last_read_message_id IS NULL (row exists) — pointer was reset by an
        //       ON DELETE SET NULL when the pointed-to message was deleted.  Treat as
        //       "all caught up" (0 new) to prevent stale counters after admin deletes.
        //   m.id > urs.last_read_message_id — normal case: messages newer than pointer.
        let count: i64 = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) FROM room_messages rm
            JOIN messages m ON m.id = rm.message_id
            LEFT JOIN user_room_state urs
              ON urs.user_id = ? AND urs.room_id = rm.room_id
            WHERE rm.room_id = ?
              AND (
                urs.user_id IS NULL
                OR (urs.last_read_message_id IS NOT NULL
                    AND m.id > urs.last_read_message_id)
              )
            "#,
            uid,
            rid
        )
        .fetch_one(&self.read_pool)
        .await?;

        Ok(count as u64)
    }

    async fn unread_direct_count(
        &self,
        username: &Username,
        user_id: UserId,
        room_id: RoomId,
    ) -> Result<u64, StoreError> {
        let uname = username.as_str();
        let uid = user_id.as_i64();
        let rid = room_id.as_i64();

        // Same NULL-pointer semantics as unread_count: a NULL pointer with an
        // existing state row means the pointer was reset; treat as "caught up".
        let count: i64 = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) FROM messages m
            LEFT JOIN user_room_state urs ON urs.user_id = ? AND urs.room_id = ?
            WHERE (m.sender = ? OR m.recipient = ?)
              AND m.recipient IS NOT NULL
              AND (
                urs.user_id IS NULL
                OR (urs.last_read_message_id IS NOT NULL
                    AND m.id > urs.last_read_message_id)
              )
            "#,
            uid,
            rid,
            uname,
            uname
        )
        .fetch_one(&self.read_pool)
        .await?;

        Ok(count as u64)
    }

    async fn get_last_read(
        &self,
        user_id: UserId,
        room_id: RoomId,
    ) -> Result<Option<MessageId>, StoreError> {
        let uid = user_id.as_i64();
        let rid = room_id.as_i64();

        let row = sqlx::query_scalar!(
            r#"SELECT last_read_message_id FROM user_room_state
               WHERE user_id = ? AND room_id = ?"#,
            uid,
            rid
        )
        .fetch_optional(&self.read_pool)
        .await?;

        Ok(row.flatten().map(MessageId::new))
    }

    async fn has_read_state(&self, user_id: UserId, room_id: RoomId) -> Result<bool, StoreError> {
        let uid = user_id.as_i64();
        let rid = room_id.as_i64();

        let count: i64 = sqlx::query_scalar!(
            r#"SELECT COUNT(*) FROM user_room_state WHERE user_id = ? AND room_id = ?"#,
            uid,
            rid
        )
        .fetch_one(&self.read_pool)
        .await?;

        Ok(count > 0)
    }

    async fn list_recent_in_room(
        &self,
        room_id: RoomId,
        limit: u32,
    ) -> Result<Vec<Message>, StoreError> {
        let rid = room_id.as_i64();
        let lim = limit as i64;

        let rows = sqlx::query!(
            r#"
            SELECT m.id AS "id!", m.sender AS "sender!", m.recipient,
                   m.content AS "content!", m.timestamp AS "timestamp!"
            FROM messages m
            JOIN room_messages rm ON rm.message_id = m.id
            WHERE rm.room_id = ?
            ORDER BY m.id DESC
            LIMIT ?
            "#,
            rid,
            lim
        )
        .fetch_all(&self.read_pool)
        .await?;

        rows.into_iter()
            .map(|r| map_message_row(r.id, r.sender, r.recipient, r.content, r.timestamp))
            .collect()
    }

    async fn count_in_room(&self, room_id: RoomId) -> Result<u64, StoreError> {
        let rid = room_id.as_i64();
        let count: i64 =
            sqlx::query_scalar!("SELECT COUNT(*) FROM room_messages WHERE room_id = ?", rid)
                .fetch_one(&self.read_pool)
                .await?;
        Ok(count as u64)
    }

    async fn count_direct(&self, username: &Username) -> Result<u64, StoreError> {
        let uname = username.as_str();
        let count: i64 = sqlx::query_scalar!(
            r#"SELECT COUNT(*) FROM messages
               WHERE (sender = ? OR recipient = ?) AND recipient IS NOT NULL"#,
            uname,
            uname
        )
        .fetch_one(&self.read_pool)
        .await?;
        Ok(count as u64)
    }

    async fn next_in_room(
        &self,
        room_id: RoomId,
        after: Option<MessageId>,
    ) -> Result<Option<Message>, StoreError> {
        let rid = room_id.as_i64();
        let after = after.map(|id| id.as_i64()).unwrap_or(i64::MIN);
        let row = sqlx::query!(
            r#"SELECT m.id AS "id!", m.sender AS "sender!", m.recipient,
                      m.content AS "content!", m.timestamp AS "timestamp!"
               FROM messages m
               JOIN room_messages rm ON rm.message_id = m.id
               WHERE rm.room_id = ? AND m.id > ?
               ORDER BY m.id ASC LIMIT 1"#,
            rid,
            after
        )
        .fetch_optional(&self.read_pool)
        .await?;
        row.map(|r| map_message_row(r.id, r.sender, r.recipient, r.content, r.timestamp))
            .transpose()
    }

    async fn prev_in_room(
        &self,
        room_id: RoomId,
        before: Option<MessageId>,
    ) -> Result<Option<Message>, StoreError> {
        let rid = room_id.as_i64();
        let before = before.map(|id| id.as_i64()).unwrap_or(i64::MAX);
        let row = sqlx::query!(
            r#"SELECT m.id AS "id!", m.sender AS "sender!", m.recipient,
                      m.content AS "content!", m.timestamp AS "timestamp!"
               FROM messages m
               JOIN room_messages rm ON rm.message_id = m.id
               WHERE rm.room_id = ? AND m.id < ?
               ORDER BY m.id DESC LIMIT 1"#,
            rid,
            before
        )
        .fetch_optional(&self.read_pool)
        .await?;
        row.map(|r| map_message_row(r.id, r.sender, r.recipient, r.content, r.timestamp))
            .transpose()
    }

    async fn next_direct(
        &self,
        username: &Username,
        after: Option<MessageId>,
    ) -> Result<Option<Message>, StoreError> {
        let uname = username.as_str();
        let after = after.map(|id| id.as_i64()).unwrap_or(i64::MIN);
        let row = sqlx::query!(
            r#"SELECT id AS "id!", sender AS "sender!", recipient,
                      content AS "content!", timestamp AS "timestamp!"
               FROM messages
               WHERE (sender = ? OR recipient = ?) AND recipient IS NOT NULL AND id > ?
               ORDER BY id ASC LIMIT 1"#,
            uname,
            uname,
            after
        )
        .fetch_optional(&self.read_pool)
        .await?;
        row.map(|r| map_message_row(r.id, r.sender, r.recipient, r.content, r.timestamp))
            .transpose()
    }

    async fn prev_direct(
        &self,
        username: &Username,
        before: Option<MessageId>,
    ) -> Result<Option<Message>, StoreError> {
        let uname = username.as_str();
        let before = before.map(|id| id.as_i64()).unwrap_or(i64::MAX);
        let row = sqlx::query!(
            r#"SELECT id AS "id!", sender AS "sender!", recipient,
                      content AS "content!", timestamp AS "timestamp!"
               FROM messages
               WHERE (sender = ? OR recipient = ?) AND recipient IS NOT NULL AND id < ?
               ORDER BY id DESC LIMIT 1"#,
            uname,
            uname,
            before
        )
        .fetch_optional(&self.read_pool)
        .await?;
        row.map(|r| map_message_row(r.id, r.sender, r.recipient, r.content, r.timestamp))
            .transpose()
    }
}
