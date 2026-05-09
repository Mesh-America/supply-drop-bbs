# Store trait sketch - bbs-core persistence layer

Design-grade pseudocode. Close to compilable but not the final
implementation - some details (error mapping, `sqlx::FromRow` derives,
exact query text) are elided. Records the agreed architectural shape so
implementation can proceed mechanically.

---

## 1. Cargo additions

`Cargo.toml` `[workspace.dependencies]`:

```toml
sqlx   = { version = "0.8", features = ["sqlite", "runtime-tokio", "time", "macros"] }
argon2 = "0.5"
# tokio already present; add "rt-multi-thread" to its features list
```

`crates/bbs-core/Cargo.toml`:

```toml
[dependencies]
sqlx.workspace   = true
argon2.workspace = true
tokio.workspace  = true
```

---

## 2. The `Database` struct

```rust
// crates/bbs-core/src/db/mod.rs

use sqlx::{Pool, Sqlite, SqliteConnectOptions, SqlitePoolOptions};
use std::num::NonZeroUsize;

/// Top-level database handle. Owns the read pool and the
/// single-connection write pool. Both are `Clone + Send + Sync`.
///
/// `Database` implements `UserStore`, `RoomStore`, `MessageStore`,
/// `CredentialStore`, and `AuditStore` directly. The `Host` impl
/// wraps `Arc<Database>` and returns `&dyn UserStore` etc. from
/// the domain accessor methods.
#[derive(Clone)]
pub struct Database {
    /// Read pool: cpu_count + 2 connections, read-only fd.
    read_pool: Pool<Sqlite>,
    /// Write pool: max 1 connection, read-write fd.
    write_pool: Pool<Sqlite>,
}

impl Database {
    /// Open the database at `path`, apply PRAGMAs, run pending
    /// migrations, verify the room walk-order invariant.
    pub async fn open(path: &str) -> Result<Self, DbOpenError> {
        let read_opts = base_connect_options(path).read_only(true);

        let cpu_count = std::thread::available_parallelism()
            .map(NonZeroUsize::get)
            .unwrap_or(4);

        let read_pool = SqlitePoolOptions::new()
            .max_connections((cpu_count + 2) as u32)
            .after_connect(|conn, _meta| Box::pin(apply_pragmas(conn)))
            .connect_with(read_opts)
            .await?;

        let write_opts = base_connect_options(path);
        let write_pool = SqlitePoolOptions::new()
            .max_connections(1)
            .after_connect(|conn, _meta| Box::pin(apply_pragmas(conn)))
            .connect_with(write_opts)
            .await?;

        sqlx::migrate!("migrations/").run(&write_pool).await?;

        verify_room_walk_order(&read_pool).await?;

        Ok(Self { read_pool, write_pool })
    }
}

fn base_connect_options(path: &str) -> SqliteConnectOptions {
    path.parse::<SqliteConnectOptions>()
        .expect("valid path")
        .create_if_missing(true)
        .foreign_keys(true)
}
```

---

## 3. The `after_connect` PRAGMA hook

```rust
// crates/bbs-core/src/db/pragmas.rs

use sqlx::sqlite::SqliteConnection;

/// Apply SD-card-tuned PRAGMAs on a fresh connection.
/// Called by the pool's `after_connect` hook.
/// PRAGMAs use runtime `query` (not `query!`) - they don't map to schema columns.
pub async fn apply_pragmas(conn: &mut SqliteConnection) -> Result<(), sqlx::Error> {
    // journal_mode returns a result row; execute and discard.
    sqlx::query("PRAGMA journal_mode = WAL").execute(&mut *conn).await?;
    sqlx::query("PRAGMA synchronous = NORMAL").execute(&mut *conn).await?;
    sqlx::query("PRAGMA cache_size = -8000").execute(&mut *conn).await?;
    sqlx::query("PRAGMA mmap_size = 268435456").execute(&mut *conn).await?;
    sqlx::query("PRAGMA temp_store = MEMORY").execute(&mut *conn).await?;
    sqlx::query("PRAGMA wal_autocheckpoint = 10000").execute(&mut *conn).await?;
    sqlx::query("PRAGMA journal_size_limit = 67108864").execute(&mut *conn).await?;
    sqlx::query("PRAGMA foreign_keys = ON").execute(&mut *conn).await?;
    sqlx::query("PRAGMA busy_timeout = 5000").execute(&mut *conn).await?;
    Ok(())
}
```

---

## 4. Room walk-order verification

```rust
// crates/bbs-core/src/db/invariants.rs

use sqlx::{Pool, Sqlite};

/// Verify the room linked-list invariant.
/// Called by `Database::open` after migrations, before returning the handle.
pub async fn verify_room_walk_order(pool: &Pool<Sqlite>) -> Result<(), RoomOrderError> {
    let head_count: i64 = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM rooms WHERE prev_neighbor IS NULL"
    )
    .fetch_one(pool)
    .await?;

    let tail_count: i64 = sqlx::query_scalar!(
        "SELECT COUNT(*) FROM rooms WHERE next_neighbor IS NULL"
    )
    .fetch_one(pool)
    .await?;

    if head_count > 1 {
        return Err(RoomOrderError::MultipleHeads(head_count));
    }
    if tail_count > 1 {
        return Err(RoomOrderError::MultipleTails(tail_count));
    }
    if head_count != tail_count {
        return Err(RoomOrderError::UnbalancedEnds { heads: head_count, tails: tail_count });
    }

    let total: i64 = sqlx::query_scalar!("SELECT COUNT(*) FROM rooms")
        .fetch_one(pool)
        .await?;

    if total == 0 {
        return Ok(());
    }

    struct Row { id: i64, next_neighbor: Option<i64> }
    let rooms = sqlx::query_as!(Row, "SELECT id, next_neighbor FROM rooms")
        .fetch_all(pool)
        .await?;

    let next_map: std::collections::HashMap<i64, i64> = rooms.iter()
        .filter_map(|r| r.next_neighbor.map(|n| (r.id, n)))
        .collect();

    // Walk from the head, detect cycles with a visited set.
    let head_id = rooms.iter()
        .find(|r| !next_map.values().any(|&v| v == r.id))
        .map(|r| r.id);

    if let Some(mut current) = head_id {
        let mut visited = std::collections::HashSet::new();
        loop {
            if !visited.insert(current) {
                return Err(RoomOrderError::Cycle);
            }
            match next_map.get(&current) {
                None => break,
                Some(&next) => current = next,
            }
            if visited.len() > total as usize {
                return Err(RoomOrderError::Cycle);
            }
        }
        if visited.len() != total as usize {
            return Err(RoomOrderError::DisconnectedRooms {
                reachable: visited.len() as i64,
                total,
            });
        }
    }

    Ok(())
}

#[derive(Debug, thiserror::Error)]
pub enum RoomOrderError {
    #[error("room list has {0} head nodes (prev_neighbor IS NULL); expected 0 or 1")]
    MultipleHeads(i64),
    #[error("room list has {0} tail nodes (next_neighbor IS NULL); expected 0 or 1")]
    MultipleTails(i64),
    #[error("room list has {heads} heads and {tails} tails; must be equal")]
    UnbalancedEnds { heads: i64, tails: i64 },
    #[error("room list contains a cycle")]
    Cycle,
    #[error("room list is disconnected: {reachable} rooms reachable from head, {total} total")]
    DisconnectedRooms { reachable: i64, total: i64 },
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}
```

---

## 5. Store traits

```rust
// crates/bbs-core/src/db/user_store.rs

use crate::ids::UserId;
use crate::user::{User, UserStatus};
use bbs_plugin_api::{PermissionLevel, Username};
use crate::timestamp::Timestamp;
use async_trait::async_trait;

/// Read/write access to the users table.
/// Does NOT include credential operations - those live on `CredentialStore`.
#[async_trait]
pub trait UserStore: Send + Sync {
    async fn get_by_id(&self, id: UserId) -> Result<Option<User>, StoreError>;
    async fn get_by_username(&self, username: &Username) -> Result<Option<User>, StoreError>;
    async fn list(
        &self,
        filter_status: Option<UserStatus>,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<User>, StoreError>;
    async fn create(
        &self,
        username: &Username,
        display_name: Option<&str>,
        permission_level: PermissionLevel,
        created_at: Timestamp,
    ) -> Result<UserId, StoreError>;
    /// Update mutable fields. `None` outer = leave unchanged;
    /// `Some(None)` for `display_name` = clear the value.
    async fn update(
        &self,
        id: UserId,
        display_name: Option<Option<&str>>,
        status: Option<UserStatus>,
        permission_level: Option<PermissionLevel>,
        last_login_at: Option<Timestamp>,
    ) -> Result<(), StoreError>;
    /// Hard-delete. Prefer `status = Deleted`. Fails if messages exist.
    async fn hard_delete(&self, id: UserId) -> Result<(), StoreError>;
}

// Representative impl block on Database (others follow the same pattern):
#[async_trait]
impl UserStore for Database {
    async fn get_by_id(&self, id: UserId) -> Result<Option<User>, StoreError> {
        let row = sqlx::query!(
            r#"
            SELECT id, username, display_name,
                   status           AS "status: u8",
                   permission_level AS "permission_level: u8",
                   created_at, last_login_at
            FROM users WHERE id = ?
            "#,
            id.as_i64()
        )
        .fetch_optional(&self.read_pool)
        .await?;
        row.map(map_user_row).transpose()
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
            name, display_name, pl, ts
        )
        .execute(&self.write_pool)
        .await
        .map_err(|e| match e {
            sqlx::Error::Database(ref db_err) if db_err.is_unique_violation() =>
                StoreError::Conflict(format!("username '{}' is already taken", name)),
            other => StoreError::Db(other),
        })?;
        Ok(UserId::new(result.last_insert_rowid()))
    }

    // ... remaining methods elided
}
```

```rust
// crates/bbs-core/src/db/room_store.rs

#[async_trait]
pub trait RoomStore: Send + Sync {
    async fn get_by_id(&self, id: RoomId) -> Result<Option<Room>, StoreError>;
    async fn get_by_name(&self, name: &str) -> Result<Option<Room>, StoreError>;
    async fn list_in_order(&self) -> Result<Vec<Room>, StoreError>;
    async fn list_readable(&self, min_permission: PermissionLevel) -> Result<Vec<Room>, StoreError>;
    async fn create(
        &self,
        name: &str,
        description: Option<&str>,
        read_only: bool,
        min_permission_level: PermissionLevel,
        created_at: Timestamp,
    ) -> Result<RoomId, StoreError>;
    async fn update(
        &self,
        id: RoomId,
        description: Option<Option<&str>>,
        read_only: Option<bool>,
        min_permission_level: Option<PermissionLevel>,
    ) -> Result<(), StoreError>;
    /// Move `room_id` to immediately after `after_id` (or to head if `None`).
    /// Runs in a single write transaction maintaining all neighbor pointers.
    async fn reorder(&self, room_id: RoomId, after_id: Option<RoomId>) -> Result<(), StoreError>;
    /// Cascades to room_messages and user_room_state; messages survive.
    async fn delete(&self, id: RoomId) -> Result<(), StoreError>;
}
```

```rust
// crates/bbs-core/src/db/message_store.rs

pub struct MessagePage {
    pub messages: Vec<Message>,
    /// Pass to the next call as `after_id`; None = last page.
    pub next_cursor: Option<MessageId>,
}

#[async_trait]
pub trait MessageStore: Send + Sync {
    async fn get_by_id(&self, id: MessageId) -> Result<Option<Message>, StoreError>;
    async fn list_in_room(
        &self,
        room_id: RoomId,
        after_id: Option<MessageId>,
        limit: u32,
    ) -> Result<MessagePage, StoreError>;
    async fn list_direct(
        &self,
        username: &Username,
        after_id: Option<MessageId>,
        limit: u32,
    ) -> Result<MessagePage, StoreError>;
    async fn post_to_room(
        &self,
        room_id: RoomId,
        sender: &Username,
        content: &str,
        timestamp: Timestamp,
    ) -> Result<MessageId, StoreError>;
    async fn post_direct(
        &self,
        sender: &Username,
        recipient: &Username,
        content: &str,
        timestamp: Timestamp,
    ) -> Result<MessageId, StoreError>;
    /// Returns Ok(false) if the message didn't exist.
    async fn delete(&self, id: MessageId) -> Result<bool, StoreError>;
    async fn mark_read(
        &self,
        user_id: UserId,
        room_id: RoomId,
        message_id: MessageId,
    ) -> Result<(), StoreError>;
    async fn unread_count(&self, user_id: UserId, room_id: RoomId) -> Result<u64, StoreError>;
}
```

```rust
// crates/bbs-core/src/db/error.rs

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("referential integrity violated: {0}")]
    IntegrityViolation(String),
    #[error("not found")]
    NotFound,
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
}
```

---

## 6. Credential store (internal only, not on `Host`)

```rust
// crates/bbs-core/src/db/credential_store.rs
// NOT part of UserStore. Called only by the auth workflow inside bbs-core.

pub(crate) struct CredentialStore<'db> {
    db: &'db Database,
}

impl<'db> CredentialStore<'db> {
    pub(crate) fn new(db: &'db Database) -> Self { Self { db } }

    pub(crate) async fn set_password(
        &self,
        user_id: UserId,
        password: &str,
        now: Timestamp,
    ) -> Result<(), StoreError> {
        let salt = SaltString::generate(&mut OsRng);
        let phc_hash = Argon2::default()
            .hash_password(password.as_bytes(), &salt)
            .expect("argon2 hash always succeeds for valid input")
            .to_string();
        let uid = user_id.as_i64();
        let ts = now.to_rfc3339();
        sqlx::query!(
            "INSERT INTO user_credentials (user_id, phc_hash, updated_at)
             VALUES (?, ?, ?)
             ON CONFLICT(user_id) DO UPDATE SET phc_hash = excluded.phc_hash,
                                                updated_at = excluded.updated_at",
            uid, phc_hash, ts
        )
        .execute(&self.db.write_pool)
        .await?;
        Ok(())
    }

    pub(crate) async fn verify_password(
        &self,
        user_id: UserId,
        candidate: &str,
    ) -> Result<bool, StoreError> {
        let uid = user_id.as_i64();
        let row = sqlx::query!(
            "SELECT phc_hash FROM user_credentials WHERE user_id = ?", uid
        )
        .fetch_optional(&self.db.read_pool)
        .await?;

        let phc_str = match row {
            None => return Ok(false),
            Some(r) => r.phc_hash,
        };

        let parsed = PasswordHash::new(&phc_str)
            .map_err(|_| StoreError::Db(sqlx::Error::Decode("malformed PHC hash".into())))?;

        let ok = Argon2::default()
            .verify_password(candidate.as_bytes(), &parsed)
            .is_ok();

        // Transparent rehash: if parameters changed since stored, rehash on success.
        if ok { /* compare params; call set_password if stale */ }

        Ok(ok)
    }
}
```

---

## 7. Revised `Host` trait additions

New methods on `bbs-plugin-api::host::Host`. Existing methods unchanged.

```rust
// Additions to crates/bbs-plugin-api/src/host.rs

pub trait Host: Send + Sync {
    // ... existing methods unchanged ...

    /// Access the user store. The returned reference is backed by the
    /// host's internal `Database`; no allocation per call.
    ///
    /// Permission enforcement is NOT in the store trait - it lives in
    /// `Host::process_command`. Plugins that call store methods directly
    /// bypass the command layer; this is acceptable because plugins are
    /// trusted compiled-in code (ADR-0004).
    fn users(&self) -> &dyn UserStore;
    fn rooms(&self) -> &dyn RoomStore;
    fn messages(&self) -> &dyn MessageStore;
}
```

---

## 8. Integration test skeletons

```rust
// crates/bbs-core/tests/user_store.rs

async fn test_db() -> Database {
    let dir = tempfile::tempdir().unwrap();
    Database::open(dir.path().join("test.sqlite").to_str().unwrap())
        .await.unwrap()
}

#[tokio::test]
async fn insert_user_and_fetch_by_username() {
    let db = test_db().await;
    let username = Username::new("alice").unwrap();
    let id = db.create(&username, Some("Alice"), PermissionLevel::Unvalidated, Timestamp::now())
        .await.expect("create should succeed");

    let fetched = db.get_by_username(&username).await.unwrap().unwrap();
    assert_eq!(fetched.id, id);
    assert_eq!(fetched.username, username);
    assert_eq!(fetched.display_name.as_deref(), Some("Alice"));
    assert!(fetched.last_login_at.is_none());

    // Duplicate username → Conflict.
    let dup = db.create(&username, None, PermissionLevel::User, Timestamp::now()).await;
    assert!(matches!(dup, Err(StoreError::Conflict(_))));
}

#[tokio::test]
async fn post_messages_and_paginate() {
    let db = test_db().await;
    let alice = Username::new("alice").unwrap();
    db.create(&alice, None, PermissionLevel::User, Timestamp::now()).await.unwrap();
    let room_id = db.create("Lobby", None, false, PermissionLevel::User, Timestamp::now())
        .await.unwrap();

    let mut ids = Vec::new();
    for i in 0u8..5 {
        ids.push(db.post_to_room(room_id, &alice, &format!("msg {i}"), Timestamp::now())
            .await.unwrap());
    }

    let page1 = db.list_in_room(room_id, None, 3).await.unwrap();
    assert_eq!(page1.messages.len(), 3);
    assert!(page1.next_cursor.is_some());

    let page2 = db.list_in_room(room_id, page1.next_cursor, 3).await.unwrap();
    assert_eq!(page2.messages.len(), 2);
    assert!(page2.next_cursor.is_none());

    let all_ids: Vec<_> = page1.messages.iter().chain(&page2.messages).map(|m| m.id).collect();
    assert_eq!(all_ids, ids);
}
```

---

## 9. Proptest cases (first two)

**Case 1: username roundtrip through create → get_by_username**

Strategy: `"[a-z][a-z0-9]{0,30}"` - filter to valid `Username::new` inputs.
Catches silent truncation or character mangling in the store's INSERT/SELECT.

**Case 2: message content roundtrip through post_to_room → get_by_id**

Strategy: `"[^\x00]{1,4096}"` - any non-NUL string up to 4096 bytes.
Catches encoding surprises in free-form text (multi-byte UTF-8,
apostrophes, backslashes). Widest character range generates inputs
hand-written tests never would.

These two cover the highest-traffic read/write paths and have input
spaces large enough that proptest adds genuine coverage value.
Room walk-order and PermissionLevel discriminant stability use
deterministic tests instead; property generation adds nothing there.
