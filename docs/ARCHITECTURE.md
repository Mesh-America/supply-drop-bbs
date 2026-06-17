# Supply Drop BBS - Architecture

This document is the canonical description of how Supply Drop BBS is
put together and why. It is the first thing a contributor should read
after the README. If you find code that contradicts this document,
that's a bug - either in the code or in the document. Open an issue
either way.

Detailed rationales for individual decisions live as ADRs (architectural
decision records) under [`adr/`](adr/). This document references them
where relevant.

---

## 1. Goals and non-goals

### Goals

1. **Run a mesh BBS reliably on a Raspberry Pi for months at a time**
   without operator intervention beyond occasional rebooting. Memory
   stays bounded, disk usage is capped, the SD card isn't worn out
   prematurely, and one transport's failure doesn't cascade into
   total system death.

2. **Be obvious to operate.** A hobbyist sysop should be able to take
   a release tarball, drop it on a Pi, run one command, and have a
   working BBS without reading thousands of pages of documentation.
   Single binary, single config file, single systemd unit (or two,
   counting the radio bridge). No language runtime to install.

3. **Be obvious to extend.** A new transport (Telnet? Matrix bridge?
   Gemini protocol?) is a new crate that implements one trait. A new
   admin feature is a contribution to the web admin plugin or a
   separate plugin that ships routes of its own. The plugin API is a
   real contract, not "go read the source."

4. **Be hard to misuse.** Bad config fails fast at startup with a
   readable error. Permission checks live in one place and can't be
   bypassed by transport authors. SQL injection is structurally
   impossible. Sysop actions are audit-logged.

5. **Be honest about its place.** This is a hobbyist mesh BBS. It
   does not need to scale to a million users, federate over the
   public internet, or replace IRC. Decisions optimise for a Pi-sized
   deployment with a handful of validated users and an active mesh.

### Non-goals

1. **Not a federated platform.** Supply Drop BBS doesn't talk to
   other BBSes. If a future feature needs that, it's a deliberate
   addition, not the default mode.

2. **Not a general-purpose chat server.** The data model is messages
   in rooms, not threaded forums or real-time channels. Rooms are
   linked-list-ordered (prev/next walk), not topic graphs. This is a
   BBS, not Discord.

3. **Not a hardware abstraction layer for radios.** Radio I/O is
   delegated to the bridge process. The BBS-side code never opens
   `/dev/spidev0.0`.

4. **Not OSI open source.** The Commons Clause restricts resale.
   See [LICENSE](../LICENSE) and [ADR-0001](adr/0001-license.md).

5. **Not a Python rewrite of mesh-citadel.** No shared schema, no
   migration path, no preserved code. See
   [ADR-0006](adr/0006-no-migration-from-mesh-citadel.md).

---

## 2. System view

### 2.1 Process topology

The topology depends on the radio hardware. Both configurations share
the same Rust binary and config schema; only the `connection_type`
setting in `[plugins.mesh]` differs.

#### USB device - single process

```
                  ┌──────────────────────────────┐
                  │    supply-drop-bbs            │
                  │    (Rust - the BBS host)      │
                  │                               │
                  │    ┌─────────────────────┐   │
                  │    │     bbs-core        │   │ ← domain, db,
                  │    └─────────────────────┘   │   sessions,
                  │                               │   workflows,
                  │    ┌──────────┐ ┌──────────┐ │   permissions
                  │    │ bbs-cli  │ │ bbs-mesh │ │
                  │    └────┬─────┘ └────┬─────┘ │ ← transport
                  │    ┌────┴─────┐      │       │   plugins
                  │    │ bbs-web  │ ← optional   │
                  │    └────┬─────┘      │       │
                  └─────────┼────────────┼───────┘
                            │       serial (USB)
       Unix socket ─────────┘            │
       (CLI admin)                       ▼
                                  USB companion device
       TCP+HTTPS ───── web UI     (Heltec V3, T-Beam, …)
       (optional)                 running MeshCore firmware
```

`bbs-mesh` speaks the companion-frame protocol directly over the USB
serial port via `meshcore-companion`. No bridge process. No Python.

#### Pi HAT - two processes

```
                  ┌──────────────────────────────┐
                  │    pymc_core                  │
                  │    CompanionFrameServer       │
                  │    (Python - radio bridge)    │
                  │                               │
                  │    ┌──────────┐               │
                  │    │  SX1262  │ ← physical    │
                  │    └──────────┘   LoRa radio  │
                  └─────────────┬─────────────────┘
                                │ TCP companion-frame
                                │ (default 127.0.0.1:5000)
                                ▼
                  ┌──────────────────────────────┐
                  │    supply-drop-bbs            │
                  │    (Rust - the BBS host)      │
                  │                               │
                  │    ┌─────────────────────┐   │
                  │    │     bbs-core        │   │ ← domain, db,
                  │    └─────────────────────┘   │   sessions,
                  │                               │   workflows,
                  │    ┌──────────┐ ┌──────────┐ │   permissions
                  │    │ bbs-cli  │ │ bbs-mesh │ │
                  │    └────┬─────┘ └────┬─────┘ │ ← transport
                  │    ┌────┴─────┐      │       │   plugins
                  │    │ bbs-web  │ ← optional   │
                  │    └────┬─────┘      │       │
                  └─────────┼────────────┼───────┘
                            │            │
       Unix socket ─────────┘            │
       (CLI admin)                       │
                                         │
       TCP+HTTPS ────────────────────────┘  ← optional
       (web admin UI; default OFF)
```

The two processes are independent. Stopping one doesn't break the
other; the BBS reports mesh unavailability cleanly and continues
serving CLI and web clients.

`pymc_core` is **not part of this project's source tree**. We install
and configure it as part of the setup wizard for HAT deployments.
See [ADR-0007](adr/0007-bridge-stays-pymc-core.md) and
[ADR-0013](adr/0013-native-serial-transport-for-usb-devices.md).

### 2.2 Crate layout

A Cargo workspace. The repo root is the binary; the workspace member
crates are libraries that the binary depends on.

```
supply-drop-bbs/
├── Cargo.toml              ← workspace root + binary crate
├── src/main.rs             ← entry point (small; mostly CLI parsing
│                              and supervisor wiring)
├── crates/
│   ├── bbs-core/           ← domain types, persistence, business
│   │                         logic, session management, workflows,
│   │                         permission system. No I/O concerns.
│   ├── bbs-plugin-api/     ← Plugin trait, Host interface, event
│   │                         types. The contract every plugin
│   │                         compiles against. Tiny crate - types
│   │                         and traits, no logic.
│   ├── bbs-cli/            ← CLI transport plugin (Unix socket).
│   ├── bbs-mesh/           ← Mesh transport plugin. Uses
│   │                         meshcore-companion to talk to the
│   │                         radio bridge.
│   ├── bbs-web/            ← Web admin UI plugin. Opt-in via the
│   │                         admin-web cargo feature. Embeds the
│   │                         compiled Vue frontend via rust-embed.
│   └── meshcore-companion/ ← Pure-Rust client for the
│                             companion-frame TCP protocol that
│                             pymc_core's CompanionFrameServer
│                             speaks. Standalone crate; could be
│                             published to crates.io eventually.
├── docs/                   ← what you're reading
└── config.example.toml     ← every knob documented; minimal
                              real-world configs much smaller
```

| Crate              | Public API stability | Test surface                          |
|--------------------|----------------------|---------------------------------------|
| `bbs-core`         | Internal             | Unit + integration with real SQLite   |
| `bbs-plugin-api`   | Stable contract      | Doc tests + worked example plugin     |
| `bbs-cli`          | Internal             | Integration via the binary            |
| `bbs-mesh`         | Internal             | Integration with companion stub       |
| `bbs-web`          | Internal             | Integration + browser smoke test      |
| `meshcore-companion` | Stable contract    | Property + fuzz against captured frames |

"Stable contract" means semver-meaningful: breaking changes require
a major version bump. "Internal" means the crate can change shape
freely; nothing outside this repo should depend on it.

### 2.3 Threading model

Inside the BBS-host binary, everything async-runs on a single Tokio
runtime (multi-threaded scheduler, default worker count). Specific
threading rules:

- **No blocking I/O on Tokio worker threads.** SQLite calls go through
  `sqlx` which schedules them appropriately; file I/O uses `tokio::fs`
  or `spawn_blocking`.
- **Each transport runs as its own Tokio task** spawned by the
  supervisor at startup. A panic in one transport is caught by the
  supervisor and logged; the other transports keep running.
- **The persist task is dead.** No more in-memory + periodic backup.
  All writes go to disk-WAL SQLite directly. See
  [ADR-0005](adr/0005-db-strategy.md).
- **Plugin lifecycle runs on the supervisor.** Plugin `init`,
  `start`, and `stop` are awaited in sequence; a slow plugin can't
  starve the others because they're separate tasks once started.

The bridge process is independent. It's not our problem.

---

## 3. Domain model

The domain is small and intentionally so. Top-level types:

| Type       | Purpose                              | Mutability        | Key relationships              |
|------------|--------------------------------------|-------------------|--------------------------------|
| `User`     | A registered account                 | Identity is fixed; profile mutable | `username` (unique), `permission_level` |
| `Room`     | A topic where messages are posted    | Created/deleted by sysops | Linked-list ordering via `prev`/`next` |
| `Message`  | A post by a user in a room (or DM)   | **Append-only** in normal operation; sysop can delete | `sender → User`, `room → Room` (via `room_messages`) |
| `Session`  | An authenticated connection          | Volatile (in-memory + DB-backed) | `user` (after binding), `transport_name`, `last_active` |
| `Workflow` | A multi-step user flow (register, login, validate) | Persistent state-machine | `session → Session`, `kind`, `step`, `data` |
| `Permission level` | Authority tier              | Set by sysop; sysop is highest | UNVALIDATED → USER → AIDE → SYSOP |

**Key invariants enforced at the type level where possible:**

- `UserId`, `RoomId`, `MessageId`, `SessionId` are distinct newtype
  wrappers around their underlying IDs. You can't pass a `RoomId`
  where a `UserId` is expected.
- `Username` is a validated newtype: non-empty, length ≤32,
  ASCII-printable, not in the forbidden-names list.
- `PermissionLevel` is an enum with exhaustive `match` checking; no
  raw integers in business logic.
- Room ordering is encoded as a doubly-linked list with the invariant
  that exactly one room has `prev = None` (the head) and exactly one
  has `next = None` (the tail). A migration check verifies this at
  startup.

**Workflows.** Multi-step flows (registration, login challenge,
sysop-mediated validation) are encoded as Rust enums representing
the state machine, persisted as JSON in the `workflow_state` table.
This survives BBS restarts mid-flow. New workflow kinds are a new
enum variant + a transition function.

---

## 4. Persistence

### 4.1 Database

**SQLite, single file, disk-only, WAL mode.** No in-memory + backup
dance ([ADR-0005](adr/0005-db-strategy.md)).

Library: `sqlx` with the `sqlite` feature. `sqlx`'s compile-time
query checking means SQL that doesn't match the schema fails to
build, not at runtime. This is one of the security baseline pillars.

### 4.2 Connection pooling

Two pools:

- **Read pool**: `cpu_count + 2` connections. Reads are concurrent in
  WAL mode and don't block writers.
- **Write pool**: 1 connection. SQLite is single-writer regardless;
  multiple write connections just contend on the file lock. One
  dedicated writer gives us a clean serialisation point.

A stuck operation on one connection isolates to that connection.
This is the structural fix for the May 8 mesh-citadel wedge: one
hung backup can't take down the whole DB layer.

### 4.3 PRAGMA settings

Applied at connection-time on every connection in both pools:

```sql
PRAGMA journal_mode      = WAL;
PRAGMA synchronous       = NORMAL;
PRAGMA cache_size        = -8000;       -- 8 MB page cache (negative = KB)
PRAGMA mmap_size         = 268435456;   -- 256 MB memory-mapped reads
PRAGMA temp_store        = MEMORY;
PRAGMA wal_autocheckpoint = 10000;      -- ~40 MB between checkpoints
PRAGMA journal_size_limit = 67108864;   -- cap WAL at 64 MB
PRAGMA foreign_keys      = ON;
PRAGMA busy_timeout      = 5000;        -- 5s waiting on writer lock
```

`synchronous = NORMAL` is the SD-card-friendly choice: fsyncs only
on WAL checkpoint, not on every transaction commit. Worst-case
power-loss data loss is the last few hundred milliseconds of writes;
the database itself stays consistent (no corruption). For a hobbyist
BBS, this is correct.

### 4.4 Schema migrations

A `schema_migrations` table tracks applied versions. Migrations are
defined in `bbs-core/migrations/` as `.sql` files numbered
sequentially. The startup path runs unapplied migrations in order
inside transactions. Migrations are **append-only after merge** -
you don't edit a migration that's been released; you write a new one.

All foreign keys declared in v1 schema include explicit `ON DELETE`
clauses. No NO ACTION defaults. (We learned this lesson from
mesh-citadel's `user_room_state` FK.)

### 4.5 Backups

A separate task runs `VACUUM INTO 'backup-YYYY-MM-DD-HHMMSS.sqlite'`
on a configurable interval (default: every 6 hours). The result is
a point-in-time copy on disk that operators can `scp` off the box.
This is for disaster recovery, not performance - there's no in-memory
DB to flush.

Backup is non-blocking; the live DB keeps serving reads and writes
while it runs.

Old backups are pruned per a retention policy in config (default:
keep last 7 daily backups + last 4 weekly backups).

---

## 5. Transports and the plugin contract

### 5.1 What a transport is

A **transport** is the boundary between the BBS-core and a way of
talking to users. Each transport:

1. Accepts connections from clients (TCP, Unix socket, mesh frames)
2. Translates client-specific protocol → internal `Command` values
3. Calls the `Host` interface to process the command
4. Translates the resulting `Response` back to the client's protocol
5. Pushes unsolicited notifications to bound sessions

The BBS-core doesn't care **where** a command came from - just
"session X wants action Y." That's what makes transports pluggable:
a new transport (Telnet, IRC bridge, web-admin) is a new crate that
implements `TransportEngine`.

### 5.2 The TransportEngine trait

Lives in `bbs-plugin-api`. Sketch (final shape may evolve as we build):

```rust
#[async_trait]
pub trait TransportEngine: Send + Sync + 'static {
    /// Stable identifier. Shows up in logs, audit trails, and
    /// notification routing. Must be unique across all loaded
    /// transports (the supervisor enforces this at startup).
    fn name(&self) -> &'static str;

    /// Stand up listeners and worker tasks. Returns when the
    /// transport is ready to accept connections. The supervisor
    /// gives every plugin a `Host` handle for driving the BBS.
    async fn start(&self, host: Arc<dyn Host>)
        -> Result<(), TransportError>;

    /// Cooperative shutdown. Must complete within
    /// `shutdown_deadline` (default 10s); the supervisor will
    /// abort the task if it exceeds the deadline.
    async fn stop(&self) -> Result<(), TransportError>;

    /// Push an unsolicited notification to a session. The
    /// session->user binding lives in the Host. Delivery
    /// semantics (queue if offline / drop / fail) are the
    /// transport's choice - it knows what its medium can do.
    async fn notify(
        &self,
        session_id: SessionId,
        payload: Notification,
    ) -> Result<NotifyOutcome, TransportError>;
}
```

A transport may also implement the optional `TransportStats` trait (also
in `bbs-plugin-api`), exposing operational metrics as JSON via `snapshot()`
and `history()`. The mesh transport implements it to surface reply-delivery
"link health"; the web admin reads it through
`GET /api/v1/transports/:name/stats` and `.../history`. Samples are stored
durably via two defaulted `Host` methods (`record_delivery_sample` /
`delivery_samples`) backing the `delivery_samples` table, so the trend
survives a restart.

### 5.3 The Host interface

What the BBS-core exposes **to** transports. Lives in
`bbs-plugin-api`; implemented by `bbs-core::HostImpl`.

```rust
#[async_trait]
pub trait Host: Send + Sync {
    /// Process a command from a session. Permission checks happen
    /// inside; transports CANNOT bypass them. The session may not
    /// be bound to a user yet (registration flow).
    async fn process_command(
        &self,
        session_id: SessionId,
        cmd: Command,
    ) -> Result<Response, HostError>;

    /// Create a fresh, unbound session. Transport name is recorded
    /// for audit and for routing notifications back.
    async fn create_session(
        &self,
        transport: &'static str,
    ) -> Result<SessionId, HostError>;

    /// Subscribe to domain events: message posted, user validated,
    /// session ended. Transports use this to push notifications.
    /// Each subscriber gets its own broadcast::Receiver.
    fn events(&self) -> broadcast::Receiver<DomainEvent>;

    /// Domain accessors. Each takes a permission context derived
    /// from the calling session and refuses operations the caller
    /// isn't allowed to perform. The compile-time signature makes
    /// "I forgot to check permissions" hard to write.
    fn users(&self, perms: &PermissionCtx) -> &dyn UserStore;
    fn rooms(&self, perms: &PermissionCtx) -> &dyn RoomStore;
    fn messages(&self, perms: &PermissionCtx) -> &dyn MessageStore;
}
```

The key invariant: **transports cannot bypass permission checks.**
Every `Host` method that touches user-visible state takes a
permission context derived from a `SessionId`. The transport doesn't
get to claim "this user is a sysop, trust me." See
[ADR-0003](adr/0003-web-ui-as-plugin.md) for why this matters
specifically for the web admin plugin.

### 5.4 Plugin lifecycle

1. **Compile time.** A plugin's crate is included in the host binary
   via cargo features. Disabled plugins aren't even compiled in.
   See [ADR-0004](adr/0004-cargo-features-not-runtime-plugins.md).
2. **Discovery.** The host has a static plugin registry constructed
   at compile time from the enabled features.
3. **Configuration.** Each plugin declares its config schema
   (`#[derive(Deserialize)]` struct). The top-level config has a
   per-plugin section keyed by `name()`. Unknown sections are an
   error (typo protection).
4. **`init`.** Called once with the deserialised config and a
   `Host` handle. The plugin returns a constructed instance or
   fails. Failure aborts startup.
5. **`start`.** Called after all plugins have successfully `init`'d.
   Plugins start their listeners. The supervisor waits for all
   `start`s to return Ready before announcing service availability.
6. **Runtime.** Plugins receive commands via `Host::process_command`
   and events via `Host::events()`. Plugins may call into other
   plugins indirectly through the Host (e.g., the web plugin asks
   the mesh plugin to deliver a notification).
7. **`stop`.** On `SIGTERM` / `SIGINT`, the supervisor calls
   `stop()` on every plugin in reverse-init order, with the
   configured deadline. Plugins that exceed the deadline are
   force-aborted and the event is logged.

### 5.5 Plugin selection

Cargo features in the binary's `Cargo.toml`:

```toml
[features]
default = ["transport-cli", "transport-mesh"]
transport-cli  = ["dep:bbs-cli"]
transport-mesh = ["dep:bbs-mesh"]
admin-web      = ["dep:bbs-web"]   # opt-in
```

CI builds three artefacts per architecture:

- `supply-drop-bbs`           - default features (cli + mesh)
- `supply-drop-bbs-web`       - default + admin-web
- `supply-drop-bbs-headless`  - cli only (no mesh, for dev)

A future ADR may revisit this if we want runtime-loaded WASM
plugins. Not v1.

---

## 6. Web admin plugin

### 6.1 What it is

A plugin (`bbs-web`) that serves an HTTP admin interface for the
BBS sysop. Built with `axum`. Serves a Vue 3 SPA bundled into the
binary via `rust-embed`. Speaks a JSON API documented as OpenAPI.

### 6.2 What it isn't

- Not a way for end-users to read messages. Mesh users use the mesh.
- Not a public-facing service. Default-bind is `127.0.0.1`; if the
  operator wants remote access, they put a TLS-terminating reverse
  proxy in front and explicitly bind to `0.0.0.0`.
- Not always-on. Default off - the binary doesn't even include the
  plugin unless built with `--features admin-web`.
- Not a transport for new users. Sysop accounts must be created
  via `supply-drop-bbs init` or the CLI; the web UI doesn't accept
  registrations.

### 6.3 Capabilities

- View system health: process uptime, DB size, last backup, mesh
  bridge connection state, recent errors.
- Manage users: list, view, validate, change permission level,
  block, delete.
- Manage rooms: create, rename, change description, reorder, delete.
- Moderate messages: view recent posts, delete with audit trail.
- View reports: message volume over time, top senders, top rooms,
  activity heatmap, validation funnel, failed login attempts,
  stale rooms. Aggregations only - no new sampling tables.
- View mesh link health: reply-delivery metrics per mesh transport -
  confirm rate (confirmed replies / first sends), route failures,
  sends, gave-up count, average round-trip latency, a per-node "worst
  links" table joined to advert names, and a per-minute confirm-rate
  trend. This one *does* sample: it writes the `delivery_samples` table
  (about once a minute, pruned after 7 days) and re-seeds the trend
  from it on startup so it survives a restart. Latency and the per-node
  breakdown populate only when reply retransmission is on
  (`reply_max_attempts > 1`; off by default).
- Manage backups: list, trigger manual backup, download.
- View logs: tail the structured-log feed live.
- View audit log: read-only, append-only history of sysop actions.

### 6.4 Authentication

Sysop logs in with username + password. Session cookie is HttpOnly,
Secure (operator must front with TLS or accept the warning),
SameSite=Strict. Session tokens are 256-bit random; stored hashed
in the DB so a stolen DB doesn't grant session takeover. Logout
invalidates the server-side record.

CSRF tokens on every state-changing endpoint. Origin header check
as a defense-in-depth layer.

Failed login attempts are rate-limited (token bucket) and logged
to the `login_failures` table for the security report.

### 6.5 Why a plugin

See [ADR-0003](adr/0003-web-ui-as-plugin.md). Short version: it
forces the plugin API to be expressive enough to support a real,
non-trivial use case. If `bbs-web` can be a plugin, almost any
extension can be.

---

## 7. Wire format

### 7.1 OpenAPI, generated from Rust

Wire types are Rust structs with `#[derive(Serialize, Deserialize)]`.
We use `utoipa` to derive an OpenAPI 3.1 schema from those types and
the `axum` route signatures.

The committed `docs/openapi.json` is the canonical contract.
[ADR-0010](adr/0010-openapi-from-rust.md) records the choice.

### 7.2 Versioning

API paths are prefixed `/api/v1/...`. Breaking changes go to
`/api/v2/...` and `v1` continues to work for at least one minor
release. Non-breaking additions (new fields, new endpoints) don't
require a version bump but do require an OpenAPI commit.

### 7.3 Other UIs

A third party can write a mobile app or a terminal client by
generating their client library from `docs/openapi.json` (or the
live `/openapi.json` endpoint when admin-web is running). They
authenticate the same way the bundled Vue UI does.

### 7.4 Mesh wire format

The companion-frame protocol between the BBS and the radio bridge
is documented in [`PROTOCOL.md`](PROTOCOL.md). It is not OpenAPI;
it's a binary framing format inherited from `pymc_core` /
MeshCore upstream.

The application layer on top of mesh - the BBS commands users send
and receive over the radio - is documented in `PROTOCOL.md` too.
This is where we encode things like "registration uses the
`new` command, validation uses `valid`," etc.

---

## 8. Configuration

### 8.1 Format and location

A single TOML file. Default search order:

1. `--config <path>` on the command line
2. `$SUPPLY_DROP_CONFIG` environment variable
3. `./config.toml` (current directory)
4. `/etc/supply-drop-bbs/config.toml` (system install)
5. `~/.config/supply-drop-bbs/config.toml` (user install)

The first path that exists is used. If none exist and there's no
`init` flag, exit with an error pointing at where to create one.

### 8.2 Override layering

Higher-numbered sources override lower-numbered:

1. Compiled-in defaults
2. Config file (TOML)
3. Environment variables (`SUPPLY_DROP__SECTION__KEY=value`)
4. Command-line flags (a small set: `--config`, `--data-dir`, log
   level overrides, ports)

Implementation: `figment` crate, which merges these in a documented
order. See [ADR-0008](adr/0008-toml-config-with-env-overrides.md).

### 8.3 Validation

Config is validated at startup, before any service starts. Failure
modes that exit immediately with a clear error pointing at file +
key + reason:

- Malformed TOML (file:line)
- Required key missing
- Key out of range (e.g., port > 65535)
- Reference to nonexistent thing (e.g., `default_room = "Lobby"`
  when no `Lobby` room is configured)
- Conflicting settings (e.g., `bind = "0.0.0.0"` with no TLS proxy
  warning acknowledged)

### 8.4 Operator subcommands

- `supply-drop-bbs init` - interactive first-run setup. Creates
  data dir, generates a default config, prompts for sysop
  credentials, optionally installs the systemd unit.
- `supply-drop-bbs config check [--config PATH]` - validate without
  starting. Exits 0 if config is valid, non-zero with the error
  message if not.
- `supply-drop-bbs config show [--config PATH]` - print the
  effective config (merged from all sources, defaults filled in).
  Tells the operator what's actually in effect.
- `supply-drop-bbs migrate` - apply pending schema migrations
  without starting the BBS. For pre-deploy ops scripts.

Full schema: [`CONFIG.md`](CONFIG.md). Example file:
[`config.example.toml`](../config.example.toml).

---

## 9. Logging and observability

### 9.1 Logging

`tracing` + `tracing-subscriber`. Two outputs by default:

- A rotating file (default `/var/log/supply-drop-bbs/bbs.log`,
  10 MB × 5 backups = 60 MB cap). Configurable.
- stderr (for systemd journal capture).

JSON output is opt-in (`logging.format = "json"`) for operators
who pipe to a log aggregator. Default is human-readable.

**Levels are respected.** No silent overrides. The CLI flag
`--log-level=debug` is a deliberate, documented override; it logs a
WARNING at startup announcing itself. Noisy crates (the radio
bridge client at frame level, sqlx at query level) are clamped to
WARN by default with explicit per-target overrides in config. See
[ADR-0009](adr/0009-tracing-config-respected.md).

The first line of every log file is a WARN-level summary of the
effective configuration so operators can see the level in effect.

### 9.2 Metrics

A Prometheus-format `/metrics` endpoint on the admin web plugin
(disabled by default; enable with `[web] prometheus = true`).
Exposes:

- Process uptime, RSS, FD count
- DB pool stats (active/idle/wait)
- Per-transport: connections, commands processed, errors
- Per-plugin: init/start/stop timings
- Domain events: messages posted, users registered, validations
  pending
- Backup job: last success, last duration, last error

### 9.3 Tracing

`tracing` spans wrap every command processed, every plugin
lifecycle event, every DB transaction. Spans carry the `session_id`
and `transport` as fields. With JSON logging on, downstream tools
can reconstruct full request flows.

### 9.4 Audit log

A separate, append-only `audit_log` table records every sysop
action (user delete, message delete, permission change, room
create/delete, manual backup trigger). Each row has actor,
action, timestamp, before-state JSON, after-state JSON. Sysops
can read it from the web admin; nobody can edit it. Schema
migrations explicitly forbid `DELETE` on this table at the
permissions layer.

---

## 10. Security

### 10.1 Threat model

We design against these adversaries:

| Adversary                                      | Defenses                                                    |
|------------------------------------------------|-------------------------------------------------------------|
| Mesh radio attacker (passive)                  | MeshCore protocol crypto (out of our scope; we trust the bridge's identity assertions) |
| Mesh radio attacker (forging packets)          | MeshCore identity verification at the bridge layer          |
| LAN attacker against admin web                 | Default-bind 127.0.0.1; HTTPS via reverse proxy; CSRF; SameSite cookies; CSP |
| Stolen sysop credentials                       | argon2id hashing (slow offline crack); audit log lets the real sysop see what happened; rate-limited login |
| Stolen DB file (offline analysis)              | Passwords argon2id-hashed; session tokens stored hashed; no API keys at rest in v1 |
| Curious or malicious validated user            | Permission system; rate limits; sysop validation funnel for new accounts |
| Physical access to the Pi                      | Out of scope. Disk encryption is the operator's call.       |
| Denial-of-service (flood, brute force)         | Per-transport token-bucket rate limiting; backoff on auth failures |

### 10.2 Baseline measures (encoded in code)

- **Password hashing.** `argon2id` via the `argon2` crate. Default
  parameters tuned for "~250ms on a Pi 4." Migration path if we
  ever increase parameters: rehash on next successful login.
- **Session tokens.** 256-bit, generated from `OsRng`. Stored in
  the DB hashed with SHA-256 (we don't need argon2 for these - they
  have high entropy and short lifetimes). Default lifetime: 12 hours
  for web, longer for mesh (mesh users disconnect and reconnect a lot).
- **SQL injection.** Structurally impossible in `bbs-core` - `sqlx`
  compile-time checking refuses to build queries that don't match
  the live schema. Plugins follow the same rule.
- **Permission checks** in the `Host` interface - see §5.3.
- **CSRF.** All state-changing web endpoints require both the
  session cookie (SameSite=Strict) and an explicit token in a
  request header. Origin header verification as defense in depth.
- **CSP.** `default-src 'self'; script-src 'self'; ...` on the
  admin web. No inline scripts, no eval.
- **Audit log.** §9.4.
- **Rate limiting.** Token bucket per (transport, identity) at
  configured rates. Login: tighter; commands: looser.
- **No telemetry.** Zero phone-home. No analytics, no auto-update
  checks, no crash reporting to a server. A hobbyist mesh BBS is
  by definition offline-friendly.

### 10.3 Out of scope for v1

These are real concerns and not security-by-obscurity:

- **MFA / 2FA on sysop login.** Worth doing; not blocking v1.
  Tracked as a future ADR.
- **API keys.** No external services to authenticate against in v1.
  If the project ever federates or talks to upstream services,
  this becomes a real concern.
- **TLS in process.** Reverse proxy is the documented answer.
  Optional rustls integration may come if there's demand.

---

## 11. Testing

The test surface is layered:

| Layer            | Tool                  | What it covers                               | When it runs    |
|------------------|-----------------------|----------------------------------------------|-----------------|
| Unit             | `cargo test` per-crate | Pure logic: parsers, state machines, validators | Every save (with `cargo-watch`) and CI |
| Integration      | `cargo test`, real SQLite in `tempfile::TempDir` | DB behaviour, FK enforcement, migration correctness, transaction semantics | CI |
| Property         | `proptest`            | Workflow state machine transitions, wire-format roundtrips, parser invariants | CI |
| Fuzz             | `cargo fuzz`          | Companion-frame parser. Bad input from the network reaches our code; this is where it's allowed to. | Nightly |
| Bench            | `criterion`           | Hot paths: command processing, room walks, session lookup. Regression detection. | Manual + before release |
| Loadgen smoke    | `drill` or `k6` script | Web admin under sustained load. Catches connection-pool sizing issues, timeout regressions. | CI on PRs |
| Browser smoke    | Playwright headless   | Web admin actually works end-to-end with the bundled Vue. | CI on PRs |

**Hard rules:**

- New behaviour requires new tests. PRs without tests get bounced.
- Tests must not depend on network or external services (the mesh
  bridge is mocked with a fake companion server in integration
  tests).
- Tests must clean up their own state. No test pollutes another.
- The test suite must pass on Linux, macOS, and Windows (CI matrix),
  even if production deployment is Linux-only. Cross-platform tests
  catch path / line-ending / timestamp-format bugs early.

---

## 12. Operations

The full guide lives in [`OPERATIONS.md`](OPERATIONS.md). Headlines:

### 12.1 Install

1. Download the release tarball for your architecture.
2. Extract.
3. Run `./supply-drop-bbs init`. Answer the prompts.
4. Start the systemd unit: `sudo systemctl enable --now supply-drop-bbs`.
5. Run the radio bridge alongside (per the bridge's own docs).

The `init` subcommand is the entire happy path. No reading required
for first-time setup.

### 12.2 Update

1. Stop the BBS.
2. Replace the binary.
3. Run `supply-drop-bbs migrate` to apply any new schema migrations.
4. Start the BBS.

Migrations are forward-only. We don't ship downgrades. If an upgrade
breaks something, restore from backup.

### 12.3 Backup

Automatic per `[backup]` section in config. Manual via the web admin
or `supply-drop-bbs backup`. Backups are SQLite files (`.sqlite`)
and can be restored by stopping the BBS, copying over the live DB,
and starting again.

### 12.4 Disaster recovery

If the live DB is corrupted:

1. Stop the BBS.
2. Move the corrupted file aside (don't delete - keep for diagnosis).
3. Copy the latest backup to the live DB path.
4. Start the BBS.
5. Run `supply-drop-bbs migrate` if the backup is from an older
   schema version.

---

## 13. Future / deferred

These are decisions we explicitly defer past v1. Each gets an ADR
when it becomes real.

- **WASM plugins.** Runtime-loadable, sandboxed, language-agnostic.
  Real long-term answer for "anyone can write a plugin." Not v1.
- **Native Rust radio bridge.** Replace `pymc_core`'s
  CompanionFrameServer with a Rust binary that talks to the SX1262
  directly. Eliminates the Python supply chain entirely.
- **MFA on sysop login.** TOTP or hardware key.
- **Federation.** Cross-BBS message exchange. Big design space.
- **TLS termination in process.** Optional `rustls` integration.
- **Log streaming.** A proper log subscription endpoint for the
  web admin's "live tail" feature, separate from `/metrics`.
- **Live config reload.** Some config keys can't change at runtime
  (DB path, ports). Many can. Worth doing properly when there's
  evidence operators need it.

---

## 14. ADR index

Decisions with their own dedicated record:

| ADR | Title                                            | Status   |
|-----|--------------------------------------------------|----------|
| 0001 | License: Apache 2.0 + Commons Clause            | Accepted |
| 0002 | Process model: BBS-host + radio-bridge          | Accepted |
| 0003 | Web admin UI as a plugin                        | Accepted |
| 0004 | Cargo features for plugin selection             | Accepted |
| 0005 | DB strategy: disk WAL with SD-card tuning       | Accepted |
| 0006 | No migration from mesh-citadel                  | Accepted |
| 0007 | Pin pymc_core CompanionFrameServer for v1       | Accepted |
| 0008 | TOML config with env var + CLI overrides        | Accepted |
| 0009 | Tracing-based logging that respects config      | Accepted |
| 0010 | OpenAPI generated from Rust via utoipa          | Accepted |
| 0011 | Transport-protocol agnostic core                | Accepted |
| 0012 | Persistence layer design                        | Accepted |
| 0013 | Native serial transport for USB companion devices | Accepted |

---

## Appendix A - Pointers for new contributors

- **Read this document.** Every section is something a contributor
  will eventually need to understand.
- **Read the relevant ADRs** for the area you're working on.
- **Read the rustdoc for `bbs-plugin-api`.** That's the contract
  every plugin builds against.
- **Look at `bbs-cli` as a reference plugin.** It's the smallest
  full implementation of a transport.
- **Run `cargo test --workspace`** before sending a PR.
- **Open an issue first** for non-trivial changes. Saves both of us
  from a wasted PR.

If something here is wrong, contradicted by the code, or just
unclear - open an issue. Documentation is part of the product, and
"the docs lied to me" is a real bug.
