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
    let sender = Username::new(sender)
        .map_err(|e| StoreError::Decode(format!("invalid stored sender: {e}")))?;
    let recipient = recipient
        .map(Username::new)
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
    /// if they have never visited it.
    async fn get_last_read(
        &self,
        user_id: UserId,
        room_id: RoomId,
    ) -> Result<Option<MessageId>, StoreError>;

    /// Return the `limit` most recent messages in a room, newest first.
    async fn list_recent_in_room(
        &self,
        room_id: RoomId,
        limit: u32,
    ) -> Result<Vec<Message>, StoreError>;
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

        let count: i64 = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) FROM room_messages rm
            JOIN messages m ON m.id = rm.message_id
            LEFT JOIN user_room_state urs
              ON urs.user_id = ? AND urs.room_id = rm.room_id
            WHERE rm.room_id = ?
              AND (urs.last_read_message_id IS NULL
                   OR m.id > urs.last_read_message_id)
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

        let count: i64 = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) FROM messages m
            LEFT JOIN user_room_state urs ON urs.user_id = ? AND urs.room_id = ?
            WHERE (m.sender = ? OR m.recipient = ?)
              AND m.recipient IS NOT NULL
              AND (urs.last_read_message_id IS NULL
                   OR m.id > urs.last_read_message_id)
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
}
