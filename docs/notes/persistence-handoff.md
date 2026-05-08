# Persistence Layer Handoff

**Date:** 2026-05-08  
**Status:** Design complete; implementation pending (commit 7)  
**Repo:** `D:\Projects\supply-drop-bbs` / `Mesh-America/supply-drop-bbs`  
**Branch:** `main` @ `bd6731a` — feat(bbs-core): foundation domain types  
**Toolchain:** Rust 1.88 (rust-toolchain.toml + CI)

---

## Decisions locked (ADRs 0001–0011)

### Database (ADR-0005)
- **WAL-mode SQLite, disk only.** No in-memory DB + periodic backup. The mesh-citadel May 8 incident showed that design creates a single-wedge point: a hung `backup()` blocks all DB operations forever. Never repeat it.
- **Two pools:** read pool (`cpu_count + 2` connections, concurrent in WAL mode) + one dedicated write connection. A hung write never blocks reads.
- **PRAGMAs on every new connection** (sqlx `after_connect` hook):
  ```sql
  PRAGMA journal_mode       = WAL;
  PRAGMA synchronous        = NORMAL;
  PRAGMA cache_size         = -8000;
  PRAGMA mmap_size          = 268435456;
  PRAGMA temp_store         = MEMORY;
  PRAGMA wal_autocheckpoint = 10000;
  PRAGMA journal_size_limit = 67108864;
  PRAGMA foreign_keys       = ON;
  PRAGMA busy_timeout       = 5000;
  ```
- **Periodic backups:** `VACUUM INTO 'backup-YYYY-MM-DD.sqlite'` on a timer (non-blocking; live DB keeps serving). Not an in-memory flush.

### Protocol-agnostic core (ADR-0011)
- No protocol-specific fields (`mesh_node_id`, `meshtastic_node_id`, etc.) on any `bbs-core` domain type or table.
- Transport identity mapping lives in transport-plugin-owned tables (e.g. `meshcore_identities` keyed to `Username`). None of that is in `bbs-core` schema.
- All timestamps stored as UTC RFC3339. `Timestamp` enforces this.

### Migration strategy
- Append-only `.sql` files in `crates/bbs-core/migrations/`
- Named `0001_initial.sql`, `0002_…`, etc.
- `sqlx::migrate!` handles the `_sqlx_migrations` table automatically
- **Never edit a migration merged to `main`; write a new one**

---

## Domain types already in bbs-core (`bd6731a`)

### `crates/bbs-core/src/ids.rs`
```rust
// Macro-generated i64 newtypes (SQLite INTEGER PRIMARY KEY)
pub struct UserId(i64);
pub struct RoomId(i64);
pub struct MessageId(i64);
// Each: new(i64), as_i64(), Display ("user:N"/"room:N"/"msg:N"),
//       From<i64>, Into<i64>, serde transparent
```

### `crates/bbs-core/src/timestamp.rs`
```rust
pub struct Timestamp(OffsetDateTime);   // always UTC
// now(), from_utc(OffsetDateTime), parse_rfc3339(&str) → Result
// Serializes as RFC3339 with Z suffix; deserializes any offset → normalises to UTC
```

### `crates/bbs-core/src/user.rs`
```rust
#[repr(u8)] #[non_exhaustive]
pub enum UserStatus { Active = 0, Banned = 1, Deleted = 2 }

pub struct User {
    pub id: UserId,
    pub username: Username,                    // from bbs-plugin-api
    pub display_name: Option<String>,          // None → show username
    pub status: UserStatus,
    pub permission_level: PermissionLevel,     // from bbs-plugin-api
    pub created_at: Timestamp,
    pub last_login_at: Option<Timestamp>,
}
```

### `crates/bbs-core/src/room.rs`
```rust
pub struct Room {
    pub id: RoomId,
    pub name: String,                          // 1–32 chars, ASCII alnum+_-
    pub description: Option<String>,           // 1–256 chars, no NUL
    pub read_only: bool,
    pub min_permission_level: PermissionLevel,
    pub prev_neighbor: Option<RoomId>,         // linked-list walk order
    pub next_neighbor: Option<RoomId>,
    pub created_at: Timestamp,
}
```

### `crates/bbs-core/src/message.rs`
```rust
pub struct Message {
    pub id: MessageId,
    pub sender: Username,
    pub recipient: Option<Username>,   // None = public room post
    pub content: String,               // 1–4096 bytes, no NUL
    pub timestamp: Timestamp,
}
```

### `crates/bbs-plugin-api` key shapes (commit `1b4ed65`)
```rust
#[async_trait] pub trait Host: Send + Sync {
    async fn process_command(&self, session: SessionId, cmd: Command) -> Result<Response, HostError>;
    async fn create_session(&self, transport: &'static str) -> Result<SessionId, HostError>;
    async fn end_session(&self, session: SessionId) -> Result<(), HostError>;
    async fn permission_ctx(&self, session: SessionId) -> Result<PermissionCtx, HostError>;
    fn events(&self) -> broadcast::Receiver<DomainEvent>;
    // Domain accessors (users/rooms/messages) NOT YET on trait — land with commit 7
}
```

---

## Current workspace deps (sqlx not yet added)

```toml
async-trait = "0.1"
proptest    = "1"
serde       = { version = "1", features = ["derive"] }
serde_json  = "1"
thiserror   = "1"
time        = { version = "0.3", features = ["serde", "serde-well-known", "macros"] }
tokio       = { version = "1", features = ["sync", "rt", "macros", "time"] }
tracing     = "0.1"
uuid        = { version = "1", features = ["v4", "serde"] }
```

---

## What commit 7 must deliver

1. `sqlx` + SQLite added to workspace deps and `bbs-core`
2. Connection pool (read pool + single write connection) with PRAGMAs via `after_connect`
3. `crates/bbs-core/migrations/0001_initial.sql` with the full v1 schema
4. `UserStore`, `RoomStore`, `MessageStore` traits + SQLite impls
5. Domain accessors (`users()`, `rooms()`, `messages()`) added to `bbs-plugin-api::Host`
6. Integration tests against real SQLite in `tempfile::tempdir()`
7. `docs/adr/0012-persistence-layer.md`

---

## Open questions (unresolved — Plan agent must answer)

1. **`sqlx::query!` macros vs `sqlx::query` runtime:**  
   `query!` compile-time SQL validation requires either `SQLX_OFFLINE=true` with committed `.sqlx/` metadata (run `cargo sqlx prepare` locally), or a live DB at build time. Which does CI use? Offline mode is standard; confirm approach and CI implications.

2. **`sqlx::migrate!` vs custom runner:**  
   `sqlx::migrate!` handles `_sqlx_migrations` table automatically and is well-tested. Custom runner = more control, more code. Given our mesh-citadel history with custom runners, opine on which.

3. **Store trait shape:**  
   - Option A: `trait UserStore { async fn get(&self, id: UserId) -> Result<Option<User>>; … }` as free-standing traits on a `Db` handle  
   - Option B: Methods directly on a `Database` struct (no traits, impl blocks only)  
   - Option C: Separate repo structs (`UserRepository`, `RoomRepository`) each holding `Arc<Pool>`  
   Which fits `Host::users()` cleanest?

4. **Domain accessors on `Host` trait:**  
   `host.users()`, `host.rooms()`, `host.messages()` — do they take a `&PermissionCtx` and return permission-filtered views, or do they return the raw store (permission enforcement in callers)?

5. **Password storage:**  
   On `User` struct vs separate `user_credentials` table (argon2id hash + salt). Separate is cleaner (security-only state not in domain type); join is one more query.

6. **Audit log:**  
   Same SQLite file as main data vs separate file. Separate means audit log survives a backup restore (which rolls back the main DB). Opine.

7. **Room walk-order invariant at startup:**  
   On `Database::open()`, should we verify the linked-list (exactly one head, one tail, no cycles)? Where does this live — SQL query check? A `verify_room_order()` fn?

---

## Plan agent brief (for `docs/adr/0012-persistence-layer.md`)

**Goal:** Design document only — no `Cargo.toml` edits, no `.rs` file writes. Output an ADR plus DDL and trait sketches the implementer will execute.

**Read before starting:**
- `docs/ARCHITECTURE.md` §3 (domain model), §4 (persistence), §5.3 (Host accessors)
- `docs/adr/0005-db-strategy.md` — PRAGMA settings, two-pool design
- `docs/adr/0011-transport-protocol-agnostic-core.md` — no protocol fields in schema
- `crates/bbs-core/src/` — all five modules for exact field shapes
- `crates/bbs-plugin-api/src/host.rs` — current Host trait

**Answer the seven open questions above and produce:**

1. `docs/adr/0012-persistence-layer.md` (Context / Decision / Consequences / Alternatives — match existing ADR format)
2. SQL DDL for `0001_initial.sql` — tables: `users`, `rooms`, `room_messages`, `messages`, `user_room_state`, `audit_log`
3. Store trait pseudocode (Rust; doesn't need to compile) for `UserStore`, `RoomStore`, `MessageStore`
4. The `after_connect` sqlx hook pattern for PRAGMAs
5. How `sqlx::migrate!` (or the custom runner, if preferred) wires into startup
6. Revised `Host` trait additions for domain accessors
7. Test strategy section: integration test skeletons + which two proptest cases to add first

**Constraints to respect:**
- No `mesh_node_id` or transport-specific columns anywhere in `bbs-core` schema
- All timestamps as `TEXT NOT NULL` storing RFC3339 with Z suffix
- IDs as `INTEGER PRIMARY KEY` (i64)
- `foreign_keys = ON` always; schema must be FK-consistent
- `UserStatus` and `PermissionLevel` as `INTEGER NOT NULL` columns with CHECK constraints

---

## Known pitfalls (don't repeat)

1. YAML step names with colons need quoting: `- name: "Build (headless: cli only)"`
2. `&'static str` in serde-derived structs → E0597; use `String`
3. Intra-doc links on types that don't exist yet → rustdoc error; use plain backticks
4. Doc list continuation (clippy 1.88): exactly 2-space indent for continued list items
5. `dtolnay/rust-toolchain@stable` + `rust-toolchain.toml` pin → cross-build target missing; use `@master` + explicit `toolchain: "1.88"` + `targets:`
6. `time 0.3.47` MSRV is rustc 1.88 — toolchain floor is 1.88, don't lower it
