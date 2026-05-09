# ADR-0012: bbs-core persistence layer

- **Status:** Accepted
- **Date:** 2026-05-08
- **Deciders:** Mesh-America

## Context

ADR-0005 decided disk-only WAL SQLite with SD-card-tuned PRAGMAs and a
two-pool model (read pool + one dedicated write connection). That ADR
left open: which Rust library, which query style, how to run migrations,
what shape the store types take, how domain accessors hang off `Host`,
where passwords live, and where the audit log lives.

ADR-0011 constrains the schema: no transport-specific columns
(`mesh_node_id` etc.) anywhere in `bbs-core` tables.

The `User`, `Room`, and `Message` domain types are already landed in
`bbs-core`. Their field shapes determine the schema directly.

## Decision

### sqlx version and crate features

Use `sqlx 0.8` with features `["sqlite", "runtime-tokio", "time",
"macros"]`. The `time` feature teaches sqlx to bind and decode
`time::OffsetDateTime` natively, which maps directly to
`Timestamp::as_offset_datetime()`. The `macros` feature is needed for
`sqlx::query!`.

Do **not** enable the `json` feature in `bbs-core`. If the audit-log
`before_state` / `after_state` columns are JSONB, they are stored and
retrieved as `TEXT` from `bbs-core`'s perspective; serde handles
serialisation in the Rust layer. This keeps the schema simple and the
dependency surface minimal.

### `sqlx::query!` vs `sqlx::query` (runtime strings)

**Use `sqlx::query!` throughout `bbs-core`.**

Rationale: the entire point of the `bbs-core` test surface specification
in ARCHITECTURE.md §11 is "SQL that doesn't match the schema fails to
build, not at runtime." `query!` is the only mechanism that enforces
this. Runtime `query` strings would let a typo in a column name survive
`cargo test` and surface only when a specific code path is exercised in
production.

The trade-off is that `query!` requires schema introspection at build
time. We adopt the **offline metadata** approach:

1. Developers run `cargo sqlx prepare --workspace` after any schema
   change. This writes cached query metadata to the `.sqlx/` directory
   at the repo root.
2. The `.sqlx/` directory is committed to the repository.
3. All CI builds set `SQLX_OFFLINE=true`, which directs `query!` to
   read from `.sqlx/` rather than opening a live database.
4. A CI job gated on PRs that touch `crates/bbs-core/migrations/` or
   any `.rs` file containing `sqlx::query!` runs `cargo sqlx prepare
   --check` (which fails if the committed `.sqlx/` is stale) and
   refuses to merge if the check fails.

This approach has no live-database dependency in CI and no `DATABASE_URL`
secret to manage. The cost is: developers must remember to re-run
`cargo sqlx prepare` when they change the schema or a query. The failing
`--check` job catches misses before merge.

Developers who prefer a live DB during local iteration can set
`DATABASE_URL=sqlite://./dev.sqlite` and skip `SQLX_OFFLINE`; the
macros will introspect live. Neither mode affects the other developer's
workflow.

### `sqlx::migrate!` vs custom migration runner

**Use `sqlx::migrate!`.**

`sqlx::migrate!` embeds migration files at compile time using
`include_str!`-style macros, applies them in filename order, and
records each in the `_sqlx_migrations` table. This is exactly the
"append-only `.sql` files, applied in order, tracked in a table" model
specified in ARCHITECTURE.md §4.4 - the only difference is the table
name is `_sqlx_migrations` rather than `schema_migrations`. We adopt
`_sqlx_migrations` to stay consistent with what sqlx creates.

A custom runner would replicate this logic for no gain specific to this
project. The one thing `sqlx::migrate!` cannot do is run unapplied
migrations inside caller-controlled transactions; it runs each migration
in its own transaction automatically. That is the correct behaviour for
forward-only schema changes.

The startup sequence is:
```
Database::open(path) →
  build_read_pool() →
  build_write_connection() →
  apply_after_connect_hook_to_all_connections() →
  sqlx::migrate!("migrations/").run(&write_conn).await? →
  verify_room_walk_order(&read_pool).await? →
  Ok(Database { read_pool, write_conn })
```

`verify_room_walk_order` is explained under Room walk-order invariant
below.

### Store trait shape

**Option A: free-standing traits (`UserStore`, `RoomStore`,
`MessageStore`) implemented by `Database`.**

Rationale: the architecture document (§5.3) already writes the
signatures as traits (`fn users(&self, perms: &PermissionCtx) -> &dyn
UserStore`). Implementing these as traits rather than plain `impl`
blocks on `Database` preserves testability - unit tests can substitute
a `FakeUserStore` without spinning up SQLite. A `FakeHost` that returns
fake stores is cheaper in test setup than a full database.

Option B (methods directly on `Database`) collapses the seam and makes
every test that touches the store hit the database. That's fine for
integration tests but awkward for unit-testing command-processing logic
that calls `host.users()`. Option C (separate repository structs each
holding `Arc<Pool>`) adds unnecessary allocation and object proliferation
for a project of this scale.

The trait objects returned by `host.users()` etc. are `&dyn UserStore`,
not owned values. This means the host implementation can return a
reference to itself (or to a field it holds) with no allocation per
call. The store traits are `async_trait`-compatible.

### Pool wiring

The `Database` struct owns:
- `read_pool: sqlx::Pool<sqlx::Sqlite>` - sized `cpu_count + 2`,
  opened with `SQLITE_OPEN_READ_ONLY` flag via `SqliteConnectOptions`
  to enforce the separation at the file-descriptor level.
- `write_conn: sqlx::Pool<sqlx::Sqlite>` - max connections 1, opened
  with `SQLITE_OPEN_READ_WRITE | SQLITE_OPEN_CREATE`.

Using `Pool` with `max_connections(1)` for the write connection rather
than a bare `SqliteConnection` is deliberate: `Pool` is `Clone + Send +
Sync`, which makes it easier to hold in an `Arc<Database>` and pass to
async tasks. A single-connection pool serialises write callers through
sqlx's own wait queue, honouring `busy_timeout`.

### PRAGMA hook (`after_connect`)

sqlx provides `SqliteConnectOptions::with_after_connect`. We supply a
function `after_connect(conn: &mut SqliteConnection) -> impl Future` that
executes each PRAGMA from ADR-0005 as a `sqlx::query` (no `!` - PRAGMAs
are not checked against the schema). The hook is registered on both the
read pool and the write connection's options before the pools are built.

`PRAGMA journal_mode = WAL` returns a result row; the hook must execute
it with `query().execute()` and discard the result (the return value is
"wal", confirming the mode, but we don't need to assert it every
connection - a failed migration would surface the problem more clearly).

### Password storage: separate `user_credentials` table

The `User` struct (the domain view) must never carry raw password bytes
or hashes - this is stated as a constraint in the task. The question is
where the hash lives.

**Decision: separate `user_credentials` table, not a column on `users`.**

Rationale:
- Every `SELECT * FROM users` (or equivalent `query!` that maps to
  `User`) would incidentally pull the hash if it were a column on
  `users`. Forgetting to project it away in a `SELECT` is a silent
  data-exposure risk.
- A separate table makes the `User` query obviously safe: it cannot
  return a hash because the hash isn't in `users`.
- The `user_credentials` table has exactly one query that reads it:
  `verify_password(username, candidate) -> Result<bool>`. This query
  lives in a `CredentialStore` that is not part of the `UserStore`
  trait. `CredentialStore` is not exposed through `Host` - only the
  host's internal authentication flow calls it. Plugins never touch
  credentials directly.
- A future multi-factor or external auth scheme can add a second
  credentials table without touching `users` at all.

The hash is `argon2id` as specified in ARCHITECTURE.md §10.2. Parameters
(memory, iterations, parallelism) are stored alongside the hash in PHC
string format (`$argon2id$v=19$m=...,t=...,p=...$salt$hash`). This
self-describing format means parameter tuning on the next login is
automatic: read the stored PHC parameters, compare to the current
configured parameters, rehash if they differ.

The `salt` is embedded in the PHC string; there is no separate `salt`
column.

### Audit log placement: same SQLite file

The audit log lives in the same database file as the main data, in an
`audit_log` table.

Rationale for not using a separate file:
- The stated benefit of a separate file is "survives a backup restore
  that rolls back the main DB." But the backup/restore procedure
  documented in ARCHITECTURE.md §12.4 stops the BBS before restoring.
  Nothing writes to either file during a restore. The rollback problem
  only materialises if audit log writes happen concurrently with a
  restore, which our procedure prevents by stopping the BBS first.
- A separate file doubles the number of files operators manage (backup,
  prune, restore). ARCHITECTURE.md §8.2 values operator simplicity
  explicitly ("single binary, single config file").
- The `audit_log` table is protected by `CHECK` constraints and
  application-layer enforcement. `DELETE` on `audit_log` is refused by
  the store layer (it has no `delete` method); the SQL schema has no
  trigger needed because the store is the only route.
- WAL mode means the audit log and main tables share the same WAL file.
  This is the desired behaviour: a sysop action and its audit entry are
  in the same WAL checkpoint, so a WAL-level replay always restores them
  together.

The trade-off accepted: if an operator manually restores the main DB
from backup outside the documented procedure (i.e., while the BBS is
running), audit log entries written after the backup's timestamp will be
inconsistent with the restored data. The documented procedure prevents
this; operators who deviate from it accept the inconsistency.

### Room walk-order invariant at startup

**A `verify_room_walk_order` async function runs after migrations, before
the `Database` handle is returned to callers.**

The function executes two queries against the read pool:

1. Count rooms with `prev_neighbor IS NULL` - must be exactly 0 or 1
   (0 is valid for an empty BBS).
2. Count rooms with `next_neighbor IS NULL` - must be exactly 0 or 1.
3. A cycle check: walk the linked list from the head up to
   `(SELECT COUNT(*) FROM rooms) + 1` steps; if the walk hasn't
   terminated at a `next_neighbor IS NULL` row, a cycle exists.

This is not a migration. It does not modify the schema. It is a
startup assertion. If it fails, `Database::open` returns an error and
the BBS refuses to start, forcing the operator to repair the database.
Startup-time is the right moment because: (a) no user traffic is in
flight yet, so the error is unambiguous; (b) the check is cheap
(at most `O(rooms)` reads, a negligible count for any realistic BBS);
(c) placing it in `Database::open` means any code path that constructs
a `Database` - including tests - gets the guarantee automatically.

A sysop reorder operation (room linked-list mutation) must be wrapped in
a transaction that maintains the invariant: update both the moved room's
neighbors' pointers and the moved room's own pointers atomically. This
transaction logic lives in `RoomStore::reorder`. The startup check is a
final safety net for corruption from outside the application (e.g.,
manual `sqlite3` edits).

## Consequences

### Positive

- Compile-time SQL checking via `query!` makes schema drift a build
  error, not a runtime failure. This is the specific security baseline
  cited in ARCHITECTURE.md §10.2.
- The `user_credentials` separation makes it structurally impossible for
  a `UserStore` query to accidentally return a password hash.
- Single audit log file simplifies operator backup and restore
  procedures.
- Free-standing store traits enable pure-Rust unit tests of command
  processing logic without a database.
- `sqlx::migrate!` handles migration tracking with zero bespoke code.
- The `verify_room_walk_order` check runs on every startup, catching
  corruption before any user can observe it.

### Negative

- Developers must remember to run `cargo sqlx prepare` after schema
  or query changes. The CI `--check` job catches misses before merge but
  not before a local build starts. A pre-commit hook in
  `.cargo/hooks/pre-commit` is recommended but not mandatory.
- `query!` macro expansion can be slow for crates with many queries;
  the `SQLX_OFFLINE` path mitigates this after the first prepare.
- A separate `user_credentials` table means a login requires two SELECT
  statements (one to fetch the user, one to fetch the hash) rather than
  one. At the scale of this BBS (handful of users, infrequent logins)
  this is not a performance concern.
- Storing the audit log in the same file means a botched manual restore
  while the BBS is running can produce an inconsistent audit log. The
  documented procedure prevents this; operators are responsible for
  following it.

## Alternatives considered

### `sqlx::query` (runtime strings) throughout

Rejected. Removes compile-time schema checking, which is listed
explicitly as a security baseline in the architecture document. The
safety advantage of `query!` outweighs the build-time complexity of the
offline metadata workflow.

### Custom migration runner

Rejected. Would replicate `sqlx::migrate!`'s logic for no project-
specific benefit. The `_sqlx_migrations` table is idiomatic; renaming it
`schema_migrations` buys nothing.

### Option B: methods on `Database` struct (no traits)

Rejected. Collapses the testability seam. Every test touching a
command-processing code path would need a real database. The trait
boundary is inexpensive and the payoff (fast unit tests, fake-able host
in plugin tests) is material.

### Option C: separate repository structs

Rejected. Adds unnecessary allocation (three `Arc<Pool>` clones per
database handle construction) and object proliferation without improving
testability or encapsulation over Option A.

### Password hash column on `users`

Rejected. Any query that selects from `users` risks returning the hash.
The separate table is a structural guarantee that no `User` value can
carry credential data, not a convention that reviewers must enforce case
by case.

### Separate audit log file

Rejected. Doubles the number of files operators manage, complicates the
backup/restore procedure, and doesn't address the stated risk (rollback
inconsistency) for operators who follow the documented procedure.
