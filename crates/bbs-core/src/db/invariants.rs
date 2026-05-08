//! Startup invariant checks for the database.
//!
//! These run after migrations in `Database::open`. They are assertions,
//! not migrations: they never modify data. If they fail, the BBS
//! refuses to start and the operator must repair the database.

use super::error::DbOpenError;
use sqlx::{Pool, Sqlite};
use std::collections::{HashMap, HashSet};

/// Verify that the room linked list is structurally valid.
///
/// The invariant: at most one head (`prev_neighbor IS NULL`) and at
/// most one tail (`next_neighbor IS NULL`), head count == tail count,
/// and the list contains no cycles or disconnected sub-lists.
///
/// An empty room table is valid (zero heads, zero tails).
pub async fn verify_room_walk_order(pool: &Pool<Sqlite>) -> Result<(), DbOpenError> {
    let head_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM rooms WHERE prev_neighbor IS NULL",
    )
    .fetch_one(pool)
    .await?;

    let tail_count: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM rooms WHERE next_neighbor IS NULL",
    )
    .fetch_one(pool)
    .await?;

    if head_count > 1 {
        return Err(DbOpenError::RoomOrder(format!(
            "room list has {head_count} head nodes (prev_neighbor IS NULL); expected 0 or 1"
        )));
    }
    if tail_count > 1 {
        return Err(DbOpenError::RoomOrder(format!(
            "room list has {tail_count} tail nodes (next_neighbor IS NULL); expected 0 or 1"
        )));
    }
    if head_count != tail_count {
        return Err(DbOpenError::RoomOrder(format!(
            "room list has {head_count} heads and {tail_count} tails; must be equal"
        )));
    }

    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM rooms")
        .fetch_one(pool)
        .await?;

    if total == 0 {
        return Ok(());
    }

    // Fetch all rows so we can walk the list in memory.
    let rows: Vec<(i64, Option<i64>)> =
        sqlx::query_as("SELECT id, next_neighbor FROM rooms")
            .fetch_all(pool)
            .await?;

    // Build a next-pointer map and find the head.
    let next_map: HashMap<i64, i64> = rows
        .iter()
        .filter_map(|(id, next)| next.map(|n| (*id, n)))
        .collect();

    // The head is the node that appears in no other node's next_neighbor.
    let all_next: HashSet<i64> = next_map.values().copied().collect();
    let head_id = rows
        .iter()
        .find(|(id, _)| !all_next.contains(id))
        .map(|(id, _)| *id);

    if let Some(mut current) = head_id {
        let mut visited = HashSet::new();
        loop {
            if !visited.insert(current) {
                return Err(DbOpenError::RoomOrder(
                    "room list contains a cycle".to_owned(),
                ));
            }
            match next_map.get(&current) {
                None => break,
                Some(&next) => current = next,
            }
            // Guard: more steps than total rows → definitely a cycle.
            if visited.len() > total as usize {
                return Err(DbOpenError::RoomOrder(
                    "room list contains a cycle".to_owned(),
                ));
            }
        }
        if visited.len() != total as usize {
            return Err(DbOpenError::RoomOrder(format!(
                "room list is disconnected: {} rooms reachable from head, {total} total",
                visited.len()
            )));
        }
    }

    Ok(())
}
