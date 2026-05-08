//! `RoomStore` trait and its SQLite implementation on `Database`.

use super::{error::StoreError, Database};
use crate::{ids::RoomId, room::Room, timestamp::Timestamp};
use async_trait::async_trait;
use bbs_plugin_api::PermissionLevel;

// ── Helpers ───────────────────────────────────────────────────────────

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

fn map_room_row(
    id: i64,
    name: String,
    description: Option<String>,
    read_only: i64,
    min_permission_level: i64,
    prev_neighbor: Option<i64>,
    next_neighbor: Option<i64>,
    created_at: String,
) -> Result<Room, StoreError> {
    let min_permission_level = permission_from_i64(min_permission_level)?;
    let created_at = Timestamp::parse_rfc3339(&created_at)
        .map_err(|e| StoreError::Decode(format!("invalid created_at: {e}")))?;
    Ok(Room {
        id: RoomId::new(id),
        name,
        description,
        read_only: read_only != 0,
        min_permission_level,
        prev_neighbor: prev_neighbor.map(RoomId::new),
        next_neighbor: next_neighbor.map(RoomId::new),
        created_at,
    })
}

// ── Trait ─────────────────────────────────────────────────────────────

/// Read/write access to the `rooms` table and the room linked list.
#[async_trait]
pub trait RoomStore: Send + Sync {
    /// Fetch a room by ID.
    async fn get_by_id(&self, id: RoomId) -> Result<Option<Room>, StoreError>;

    /// Fetch a room by its unique name.
    async fn get_by_name(&self, name: &str) -> Result<Option<Room>, StoreError>;

    /// Return all rooms in linked-list walk order (head first).
    async fn list_in_order(&self) -> Result<Vec<Room>, StoreError>;

    /// Return rooms whose `min_permission_level` is at most `min_permission`.
    async fn list_readable(
        &self,
        min_permission: PermissionLevel,
    ) -> Result<Vec<Room>, StoreError>;

    /// Create a new room appended to the tail of the walk order.
    async fn create(
        &self,
        name: &str,
        description: Option<&str>,
        read_only: bool,
        min_permission_level: PermissionLevel,
        created_at: Timestamp,
    ) -> Result<RoomId, StoreError>;

    /// Update mutable fields. `None` = leave unchanged;
    /// `Some(None)` for `description` = clear to NULL.
    async fn update(
        &self,
        id: RoomId,
        description: Option<Option<&str>>,
        read_only: Option<bool>,
        min_permission_level: Option<PermissionLevel>,
    ) -> Result<(), StoreError>;

    /// Move `room_id` to immediately after `after_id` in the walk
    /// order, or to the head position if `after_id` is `None`. Runs
    /// as a single write transaction.
    async fn reorder(
        &self,
        room_id: RoomId,
        after_id: Option<RoomId>,
    ) -> Result<(), StoreError>;

    /// Delete a room. Cascades to `room_messages` and
    /// `user_room_state`; the underlying `messages` rows survive.
    async fn delete(&self, id: RoomId) -> Result<(), StoreError>;
}

// ── Implementation ────────────────────────────────────────────────────

#[async_trait]
impl RoomStore for Database {
    async fn get_by_id(&self, id: RoomId) -> Result<Option<Room>, StoreError> {
        let rid = id.as_i64();
        let row = sqlx::query!(
            r#"SELECT id AS "id!", name AS "name!", description, read_only AS "read_only!",
                      min_permission_level AS "min_permission_level!",
                      prev_neighbor, next_neighbor, created_at AS "created_at!"
               FROM rooms WHERE id = ?"#,
            rid
        )
        .fetch_optional(&self.read_pool)
        .await?;
        row.map(|r| {
            map_room_row(
                r.id,
                r.name,
                r.description,
                r.read_only,
                r.min_permission_level,
                r.prev_neighbor,
                r.next_neighbor,
                r.created_at,
            )
        })
        .transpose()
    }

    async fn get_by_name(&self, name: &str) -> Result<Option<Room>, StoreError> {
        let row = sqlx::query!(
            r#"SELECT id AS "id!", name AS "name!", description, read_only AS "read_only!",
                      min_permission_level AS "min_permission_level!",
                      prev_neighbor, next_neighbor, created_at AS "created_at!"
               FROM rooms WHERE name = ?"#,
            name
        )
        .fetch_optional(&self.read_pool)
        .await?;
        row.map(|r| {
            map_room_row(
                r.id,
                r.name,
                r.description,
                r.read_only,
                r.min_permission_level,
                r.prev_neighbor,
                r.next_neighbor,
                r.created_at,
            )
        })
        .transpose()
    }

    async fn list_in_order(&self) -> Result<Vec<Room>, StoreError> {
        let rows = sqlx::query!(
            r#"SELECT id AS "id!", name AS "name!", description, read_only AS "read_only!",
                      min_permission_level AS "min_permission_level!",
                      prev_neighbor, next_neighbor, created_at AS "created_at!"
               FROM rooms"#
        )
        .fetch_all(&self.read_pool)
        .await?;

        let mut rooms: Vec<Room> = rows
            .into_iter()
            .map(|r| {
                map_room_row(
                    r.id,
                    r.name,
                    r.description,
                    r.read_only,
                    r.min_permission_level,
                    r.prev_neighbor,
                    r.next_neighbor,
                    r.created_at,
                )
            })
            .collect::<Result<_, _>>()?;

        // Sort into linked-list order in Rust (cheaper than a recursive
        // SQL CTE for a realistic BBS room count).
        let mut ordered = Vec::with_capacity(rooms.len());
        if rooms.is_empty() {
            return Ok(ordered);
        }
        let mut map: std::collections::HashMap<RoomId, Room> =
            rooms.drain(..).map(|r| (r.id, r)).collect();

        let head_id = map.values().find(|r| r.prev_neighbor.is_none()).map(|r| r.id);
        if let Some(mut current_id) = head_id {
            loop {
                let room = map.remove(&current_id).expect("id must be present");
                let next = room.next_neighbor;
                ordered.push(room);
                match next {
                    None => break,
                    Some(n) => current_id = n,
                }
            }
        }
        // Append any unreachable rooms (cycle-guard: startup invariant check
        // should have caught this, but be defensive).
        ordered.extend(map.into_values());
        Ok(ordered)
    }

    async fn list_readable(
        &self,
        min_permission: PermissionLevel,
    ) -> Result<Vec<Room>, StoreError> {
        let level = min_permission as i64;
        let rows = sqlx::query!(
            r#"SELECT id AS "id!", name AS "name!", description, read_only AS "read_only!",
                      min_permission_level AS "min_permission_level!",
                      prev_neighbor, next_neighbor, created_at AS "created_at!"
               FROM rooms WHERE min_permission_level <= ?"#,
            level
        )
        .fetch_all(&self.read_pool)
        .await?;
        rows.into_iter()
            .map(|r| {
                map_room_row(
                    r.id,
                    r.name,
                    r.description,
                    r.read_only,
                    r.min_permission_level,
                    r.prev_neighbor,
                    r.next_neighbor,
                    r.created_at,
                )
            })
            .collect()
    }

    async fn create(
        &self,
        name: &str,
        description: Option<&str>,
        read_only: bool,
        min_permission_level: PermissionLevel,
        created_at: Timestamp,
    ) -> Result<RoomId, StoreError> {
        let ro = read_only as i64;
        let pl = min_permission_level as i64;
        let ts = created_at.to_rfc3339();

        let mut tx = self.write_pool.begin().await?;

        let tail_id: Option<i64> = sqlx::query_scalar!(
            "SELECT id FROM rooms WHERE next_neighbor IS NULL LIMIT 1"
        )
        .fetch_optional(&mut *tx)
        .await?;

        let result = sqlx::query!(
            "INSERT INTO rooms (name, description, read_only, min_permission_level,
                                prev_neighbor, next_neighbor, created_at)
             VALUES (?, ?, ?, ?, ?, NULL, ?)",
            name,
            description,
            ro,
            pl,
            tail_id,
            ts
        )
        .execute(&mut *tx)
        .await
        .map_err(|e| {
            if let sqlx::Error::Database(ref db_err) = e {
                if db_err.is_unique_violation() {
                    return StoreError::Conflict(format!("room name '{name}' is already taken"));
                }
            }
            StoreError::Db(e)
        })?;

        let new_id = result.last_insert_rowid();

        if let Some(tid) = tail_id {
            sqlx::query!(
                "UPDATE rooms SET next_neighbor = ? WHERE id = ?",
                new_id,
                tid
            )
            .execute(&mut *tx)
            .await?;
        }

        tx.commit().await?;
        Ok(RoomId::new(new_id))
    }

    async fn update(
        &self,
        id: RoomId,
        description: Option<Option<&str>>,
        read_only: Option<bool>,
        min_permission_level: Option<PermissionLevel>,
    ) -> Result<(), StoreError> {
        let rid = id.as_i64();

        let current = sqlx::query!(
            r#"SELECT description, read_only AS "read_only!", min_permission_level AS "min_permission_level!"
               FROM rooms WHERE id = ?"#,
            rid
        )
        .fetch_optional(&self.read_pool)
        .await?
        .ok_or(StoreError::NotFound)?;

        let new_desc: Option<String> = match description {
            None => current.description,
            Some(None) => None,
            Some(Some(s)) => Some(s.to_owned()),
        };
        let new_ro = read_only.map(|b| b as i64).unwrap_or(current.read_only);
        let new_pl = min_permission_level
            .map(|p| p as i64)
            .unwrap_or(current.min_permission_level);

        sqlx::query!(
            "UPDATE rooms SET description = ?, read_only = ?, min_permission_level = ?
             WHERE id = ?",
            new_desc,
            new_ro,
            new_pl,
            rid
        )
        .execute(&self.write_pool)
        .await?;

        Ok(())
    }

    async fn reorder(
        &self,
        room_id: RoomId,
        after_id: Option<RoomId>,
    ) -> Result<(), StoreError> {
        let rid = room_id.as_i64();
        let mut tx = self.write_pool.begin().await?;

        let current = sqlx::query!(
            "SELECT prev_neighbor, next_neighbor FROM rooms WHERE id = ?",
            rid
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::NotFound)?;

        let cur_prev = current.prev_neighbor;
        let cur_next = current.next_neighbor;

        // Detach: stitch current neighbors together.
        if let Some(prev_id) = cur_prev {
            sqlx::query!(
                "UPDATE rooms SET next_neighbor = ? WHERE id = ?",
                cur_next,
                prev_id
            )
            .execute(&mut *tx)
            .await?;
        }
        if let Some(next_id) = cur_next {
            sqlx::query!(
                "UPDATE rooms SET prev_neighbor = ? WHERE id = ?",
                cur_prev,
                next_id
            )
            .execute(&mut *tx)
            .await?;
        }

        match after_id {
            None => {
                // Insert at head.
                let new_head: Option<i64> = sqlx::query_scalar!(
                    "SELECT id FROM rooms WHERE prev_neighbor IS NULL AND id != ? LIMIT 1",
                    rid
                )
                .fetch_optional(&mut *tx)
                .await?;

                if let Some(nh) = new_head {
                    sqlx::query!(
                        "UPDATE rooms SET prev_neighbor = ? WHERE id = ?",
                        rid,
                        nh
                    )
                    .execute(&mut *tx)
                    .await?;
                }

                sqlx::query!(
                    "UPDATE rooms SET prev_neighbor = NULL, next_neighbor = ? WHERE id = ?",
                    new_head,
                    rid
                )
                .execute(&mut *tx)
                .await?;
            }
            Some(target_id) => {
                let tid = target_id.as_i64();

                let target_next: Option<i64> = sqlx::query_scalar!(
                    "SELECT next_neighbor FROM rooms WHERE id = ?",
                    tid
                )
                .fetch_optional(&mut *tx)
                .await?
                .flatten();

                if let Some(tn) = target_next {
                    sqlx::query!(
                        "UPDATE rooms SET prev_neighbor = ? WHERE id = ?",
                        rid,
                        tn
                    )
                    .execute(&mut *tx)
                    .await?;
                }

                sqlx::query!(
                    "UPDATE rooms SET next_neighbor = ? WHERE id = ?",
                    rid,
                    tid
                )
                .execute(&mut *tx)
                .await?;

                sqlx::query!(
                    "UPDATE rooms SET prev_neighbor = ?, next_neighbor = ? WHERE id = ?",
                    tid,
                    target_next,
                    rid
                )
                .execute(&mut *tx)
                .await?;
            }
        }

        tx.commit().await?;
        Ok(())
    }

    async fn delete(&self, id: RoomId) -> Result<(), StoreError> {
        let rid = id.as_i64();
        let mut tx = self.write_pool.begin().await?;

        let current = sqlx::query!(
            "SELECT prev_neighbor, next_neighbor FROM rooms WHERE id = ?",
            rid
        )
        .fetch_optional(&mut *tx)
        .await?
        .ok_or(StoreError::NotFound)?;

        if let Some(prev_id) = current.prev_neighbor {
            sqlx::query!(
                "UPDATE rooms SET next_neighbor = ? WHERE id = ?",
                current.next_neighbor,
                prev_id
            )
            .execute(&mut *tx)
            .await?;
        }
        if let Some(next_id) = current.next_neighbor {
            sqlx::query!(
                "UPDATE rooms SET prev_neighbor = ? WHERE id = ?",
                current.prev_neighbor,
                next_id
            )
            .execute(&mut *tx)
            .await?;
        }

        sqlx::query!("DELETE FROM rooms WHERE id = ?", rid)
            .execute(&mut *tx)
            .await?;

        tx.commit().await?;
        Ok(())
    }
}
