# Plugin API guide

This document is the prose introduction to writing plugins for
Supply Drop BBS. The canonical reference is the rustdoc on the
`bbs-plugin-api` crate - generate with `cargo doc -p bbs-plugin-api
--open` once the codebase exists. This file explains the *why* and
points at the *what*.

> **Status:** this document is a stub being grown alongside the
> implementation. Sections marked **TBD** will be filled in as the
> corresponding code lands. Each PR that adds a new plugin
> capability also updates the relevant section here.

## Audience

You should read this if you're:

- Writing a new transport (Telnet, IRC bridge, Matrix, Gemini, etc.)
- Writing an admin tool that ships HTTP routes, scheduled jobs, or
  event consumers
- Modifying an existing plugin and want to understand the API it
  speaks
- Reviewing a plugin contribution for inclusion in the workspace

If you're an operator looking to enable or configure a plugin, see
[CONFIG.md](CONFIG.md) instead.

## What is a plugin

A plugin is a Rust crate in the `crates/` directory of the
workspace that:

1. Implements the `Plugin` trait from `bbs-plugin-api`
2. Optionally implements one or more capability traits
   (`TransportEngine`, `RouteContributor`, `ScheduledTask`, etc.)
3. Declares its config schema as a `serde::Deserialize` struct
4. Is registered in the host binary via a cargo feature

Plugins are linked at compile time. See
[ADR-0004](adr/0004-cargo-features-not-runtime-plugins.md) for the
rationale.

## The Plugin trait

Every plugin starts with the base `Plugin` trait. (Sketch - final
shape evolves as we implement.)

```rust
#[async_trait]
pub trait Plugin: Send + Sync + 'static {
    /// Stable identifier. Lowercase ASCII, hyphens allowed,
    /// must be unique across all loaded plugins. Logged with
    /// every plugin event.
    fn name(&self) -> &'static str;

    /// Human-readable version. Reported in `/health` and on
    /// startup. Conventionally `env!("CARGO_PKG_VERSION")`.
    fn version(&self) -> &'static str;

    /// Called once at startup with the plugin's deserialised
    /// config and a handle to the BBS host. Return Ok(self) or
    /// Err(...) to abort startup. Long initialisation
    /// (DB migrations, cache warm) belongs here, not in start().
    async fn init(
        config: Self::Config,
        host: Arc<dyn Host>,
    ) -> Result<Self, PluginError>
    where
        Self: Sized;

    /// Called after every plugin has init'd successfully.
    /// Spawn worker tasks, open listeners, begin accepting work.
    /// Returns when the plugin is ready to serve.
    async fn start(&self) -> Result<(), PluginError>;

    /// Cooperative shutdown. Must complete within the
    /// configured deadline (default 10s) or the supervisor
    /// aborts the task and logs the breach.
    async fn stop(&self) -> Result<(), PluginError>;

    /// The plugin's config schema. Implementors pass their own
    /// type here; the host's TOML loader uses it to deserialise
    /// `[plugins.<name>]`.
    type Config: DeserializeOwned + Send;
}
```

## Capability traits

A plugin opts into capabilities by also implementing additional
traits:

| Trait                | Capability                                    | When to implement                              |
|----------------------|-----------------------------------------------|------------------------------------------------|
| `TransportEngine`    | Accept connections, process commands          | A way for users to talk to the BBS             |
| `RouteContributor`   | Add HTTP routes to the admin web's axum router | Plugin contributes admin UI features          |
| `StaticFileMount`    | Serve static files at a mount path            | Plugin ships a frontend bundle                 |
| `ScheduledTask`      | Run a task on a cron-like schedule            | Periodic work (cleanup, reports, syncs)        |
| `EventConsumer`      | Subscribe to domain events                    | React to messages posted, users validated, etc.|
| `MetricsContributor` | Add metrics to the Prometheus exporter        | Plugin tracks something operators want to see  |
| `HealthCheck`        | Contribute to the `/health` endpoint           | Plugin has a meaningful health state          |

A plugin can implement any combination. The mesh transport
implements `TransportEngine`. The web admin plugin implements
`TransportEngine` (the HTTP socket itself), `RouteContributor` (its
own admin endpoints), `StaticFileMount` (the Vue bundle), and
`HealthCheck`.

The detail of each capability trait is in `bbs-plugin-api`'s
rustdoc. **TBD** as the implementation lands.

## The Host interface

The `Host` trait, implemented by the BBS core, is what plugins call
into. A handle is given to every plugin at `init` time.

```rust
#[async_trait]
pub trait Host: Send + Sync {
    // ── Command processing ──────────────────────────────────────

    /// Process a command from a session. Permission checks happen
    /// inside; transports CANNOT bypass them. The session may
    /// not yet be bound to a user (registration flow).
    async fn process_command(
        &self,
        session_id: SessionId,
        cmd: Command,
    ) -> Result<Response, HostError>;

    // ── Sessions ────────────────────────────────────────────────

    /// Create a fresh, unbound session. Transport name is
    /// recorded for audit and notification routing.
    async fn create_session(
        &self,
        transport: &'static str,
    ) -> Result<SessionId, HostError>;

    /// Look up the user bound to a session, if any.
    async fn session_user(
        &self,
        session_id: SessionId,
    ) -> Option<UserId>;

    // ── Domain events ───────────────────────────────────────────

    /// Subscribe to domain events. Each subscriber gets its own
    /// broadcast::Receiver. Events fan out to all subscribers.
    fn events(&self) -> broadcast::Receiver<DomainEvent>;

    // ── Domain accessors ────────────────────────────────────────
    //
    // Each takes a permission context and refuses operations the
    // caller isn't authorised for. The compile-time signature
    // means "I forgot to check permissions" is hard to write.

    fn users(&self, perms: &PermissionCtx) -> &dyn UserStore;
    fn rooms(&self, perms: &PermissionCtx) -> &dyn RoomStore;
    fn messages(&self, perms: &PermissionCtx) -> &dyn MessageStore;

    // ── Node location ───────────────────────────────────────────────

    /// GPS coordinates for this node, if configured. Returns `None`
    /// when no coordinates are configured. Transports call this on
    /// every successful connect to push the position to their
    /// underlying hardware or network layer.
    fn node_location(&self) -> Option<(f64, f64)>;

    /// Update the in-memory GPS location without a restart.
    /// Called by the web admin plugin after a sysop saves new
    /// coordinates via the web UI. Transport plugins should NOT call
    /// this — it is only for the admin layer. The updated value is
    /// returned by the next call to `node_location()`.
    fn set_node_location(&self, location: Option<(f64, f64)>);

    // ── Audit ───────────────────────────────────────────────────

    /// Append-only audit log. Plugins should call this for any
    /// state change initiated by an authenticated actor with
    /// elevated permissions (sysop, aide). Failure to call is
    /// not a runtime error but is reviewable in PR.
    async fn audit_log(
        &self,
        actor: UserId,
        action: AuditAction,
        before: Option<serde_json::Value>,
        after: Option<serde_json::Value>,
    ) -> Result<(), HostError>;
}
```

The `Host` is the entirety of the BBS-core API surface for plugins.
Direct DB access, raw session-token manipulation, and bypassing the
permission system are **not** exposed; if a plugin needs something
that isn't on `Host`, that's a signal to extend `Host`.

## Node location (GPS)

The operator may configure a GPS position for the node under
`[location]` in `config.toml`:

```toml
[location]
latitude  = 46.478
longitude = -122.798
```

The host reads this at startup and keeps it in memory. The web admin
plugin updates it live (via `set_node_location`) whenever a sysop
saves new coordinates — no restart required.

### How transport plugins consume this

Call `host.node_location()` each time your transport successfully
connects to its underlying layer (radio bridge, network socket, etc.).
It returns `Option<(f64, f64)>` in `(latitude, longitude)` order:

- `None` → no location configured; leave the hardware default as-is.
- `Some((lat, lon))` → send the appropriate position frame to your
  hardware or network layer.

The call is synchronous and cheap (reads a `RwLock`). The mesh
transport does this in its `ClientEvent::Connected` handler:

```rust
// On every successful radio-bridge connect:
if let Some((lat, lon)) = host.node_location() {
    let lat_1e6 = (lat * 1_000_000.0) as i32;
    let lon_1e6 = (lon * 1_000_000.0) as i32;
    cmd_tx.send(OutboundFrame::SetAdvertLatlon { lat_1e6, lon_1e6 }).await?;
}
```

Calling on each reconnect (rather than caching at `init`) is
intentional: a sysop can update the coordinates via the web UI while
the service is running, and the change takes effect the next time
the transport reconnects.

### What transport plugins must NOT do

- Do **not** call `set_node_location`. That method is reserved for
  the admin layer. Transports are consumers of the location, not
  producers.
- Do **not** cache `node_location()` at `init` time. Always read it
  fresh on each connect so live updates from the web UI are picked up.

## Configuration

Plugins declare their config via the `Config` associated type.
Operators configure them under `[plugins.<plugin-name>]` in
`config.toml`.

Example: a hypothetical `bbs-mqtt` plugin that bridges domain
events to an MQTT broker:

```rust
#[derive(Deserialize)]
pub struct MqttBridgeConfig {
    pub broker: String,
    pub username: Option<String>,
    pub password: Option<String>,
    #[serde(default = "default_qos")]
    pub qos: u8,
    #[serde(default)]
    pub topics: Vec<TopicMapping>,
}

fn default_qos() -> u8 { 1 }

#[derive(Deserialize)]
pub struct TopicMapping {
    pub event: String,    // "message_posted", "user_validated"
    pub topic: String,    // "bbs/messages", "bbs/users/validated"
}
```

Operator config:

```toml
[plugins.mqtt-bridge]
broker = "tcp://localhost:1883"
username = "bbs"
password = "${MQTT_PASSWORD}"  # env-var interpolation; see CONFIG.md

[[plugins.mqtt-bridge.topics]]
event = "message_posted"
topic = "bbs/messages"

[[plugins.mqtt-bridge.topics]]
event = "user_validated"
topic = "bbs/users/validated"
```

The host's TOML loader deserialises this into `MqttBridgeConfig`
and passes it to `Plugin::init`. Schema errors fail at startup with
a clear message; see [CONFIG.md](CONFIG.md) for the validation
rules.

## Plugin dependencies

Some plugins depend on others. The web admin plugin's
"send a test mesh DM" feature requires the mesh transport to be
loaded. The plugin declares this:

```rust
impl Plugin for AdminWeb {
    fn dependencies(&self) -> &[&'static str] {
        &["transport-mesh"]   // names of plugins this requires
    }
    // ... rest of Plugin impl
}
```

The supervisor checks dependencies at startup. Missing
dependency → startup fails with a clear error pointing at the
config and suggesting which feature to enable.

Plugins cannot have circular dependencies. The supervisor
detects and refuses cycles at startup.

## Lifecycle

```
┌──────────┐                                      ┌──────────┐
│ supervisor│                                      │  plugin  │
└─────┬─────┘                                      └────┬─────┘
      │                                                 │
      │  Plugin::init(config, host)                     │
      ├─────────────────────────────────────────────────►│
      │                                                 │
      │                 Result<Self, PluginError>       │
      │◄─────────────────────────────────────────────────┤
      │                                                 │
      │  (after all plugins init)                       │
      │                                                 │
      │  Plugin::start()                                │
      ├─────────────────────────────────────────────────►│
      │                                                 │ (spawn workers,
      │                                                 │  open listeners)
      │                  Result<(), PluginError>        │
      │◄─────────────────────────────────────────────────┤
      │                                                 │
      │  ...runtime...                                  │
      │  (plugin calls Host methods,                    │
      │   Host fans out events to plugin)               │
      │                                                 │
      │  SIGTERM received                               │
      │  Plugin::stop()                                 │
      ├─────────────────────────────────────────────────►│
      │                                                 │ (drain, close
      │                                                 │  listeners)
      │                  Result<(), PluginError>        │
      │◄─────────────────────────────────────────────────┤
      │                                                 │
      ▼                                                 ▼
```

The supervisor enforces deadlines on every lifecycle phase. A
plugin that takes too long is logged and aborted. Init failures
abort the entire startup; start and stop failures are logged but
don't block other plugins.

## Worked example: a minimal plugin

`crates/bbs-hello-transport` is a fully-compilable reference transport
that demonstrates every integration point in under 200 lines of Rust.
Read `src/lib.rs` top-to-bottom; each section is annotated with the
pattern it illustrates.

| Pattern | Location |
|---------|----------|
| Serde config with `#[serde(default)]` | `HelloConfig` |
| `Plugin::init` / `start` / `stop` lifecycle | `HelloTransport` impl |
| Background accept loop with `watch` shutdown | `HelloTransport::start` |
| `host.create_session` → `host.process_command` → `host.end_session` | `handle_connection` |
| `awaiting_reply` state machine | `handle_connection` |
| `TransportEngine::notify` delivering push text | `HelloTransport` impl |
| `MockHost` in tests (port `0` for free-port allocation) | inline `tests` module |

The crate compiles as-is and its tests run with `cargo test -p bbs-hello-transport`.
To build a real transport, fork the crate, rename the structs, and replace
the TCP listener with your protocol's I/O layer.

## Errors

Plugin error types should:

- Implement `std::error::Error` (use `thiserror` for ergonomic
  derives)
- Distinguish between *retryable* (transient network errors,
  temporary lock contention) and *fatal* (config invalid, DB
  schema mismatch) variants
- Carry context for debugging - `tracing::error!()` macros pick
  this up automatically

Avoid `unwrap()` and `expect()` in plugin code outside of test
modules. The supervisor catches plugin panics and logs them, but a
panic still terminates the plugin's task; a clean error return is
always better.

## Testing

Plugin authors should:

- Unit-test pure logic with `cargo test`
- Integration-test against `MockHost` from `bbs-plugin-api::testing`
- Use `proptest` for state-machine and parser code
- Gate tests that need real hardware (radio, broker) behind a Cargo
  feature so CI skips them by default

### Using MockHost

`bbs_plugin_api::testing::MockHost` is a fully in-memory `Host`.  It
records every command dispatched to it and lets you script responses:

```rust
use std::sync::Arc;
use bbs_plugin_api::{Command, Host, Response, testing::MockHost};

#[tokio::test]
async fn plugin_responds_to_help() {
    let host = Arc::new(MockHost::new());
    host.set_default_response(Response::Text("ok".to_owned()));

    let sid = (Arc::clone(&host) as Arc<dyn Host>)
        .create_session("test-transport")
        .await
        .unwrap();

    let response = (Arc::clone(&host) as Arc<dyn Host>)
        .process_command(sid, Command::Help { topic: None })
        .await
        .unwrap();

    assert!(matches!(response, Response::Text(_)));

    let cmds = host.commands_received();
    assert_eq!(cmds.len(), 1);
    assert!(matches!(cmds[0].1, Command::Help { topic: None }));
}
```

Key `MockHost` methods:

| Method | Purpose |
|--------|---------|
| `MockHost::new()` | Fresh mock with no sessions |
| `set_default_response(r)` | Return `r` for any unmatched command |
| `set_response_for(pred, r)` | Return `r` when `pred(cmd)` is true (first match wins) |
| `commands_received()` | `Vec<(SessionId, Command)>` — all dispatched commands in order |
| `emit_event(e)` | Inject a `DomainEvent` into the broadcast channel |

For complete working examples see:

- `crates/bbs-hello-transport/src/lib.rs` — basic `init`/`start`/`stop` test
- `crates/bbs-process-transport/tests/process.rs` — full session lifecycle,
  `awaiting_reply` state machine, `notify()` delivery

The project's overall test strategy is in
[ARCHITECTURE.md §11](ARCHITECTURE.md#11-testing).

## Versioning

`bbs-plugin-api` follows semantic versioning relative to plugins
that depend on it. Pre-1.0 (where we are now) means breaking
changes can land in any release; we'll document each one in the
changelog.

Plugins inside this workspace are recompiled with each release of
the host, so they automatically pick up `bbs-plugin-api` changes.
Out-of-tree plugins (community-maintained forks) need to track
upstream more carefully; the `bbs-plugin-api` changelog is their
canonical reference.

## Style guide

- One plugin per crate
- Crate name: `bbs-<short-name>` for in-tree plugins;
  `<owner>-bbs-<short-name>` for community plugins
- Public API on the plugin should be minimal - most types are
  `pub(crate)`; only the `Plugin`-implementing struct is `pub`
- Config struct is named `<Plugin>Config`
- Errors: one enum per plugin, named `<Plugin>Error`
- Logging: use `tracing` macros, target `supply_drop_bbs::<plugin-name>`

## Contributing a plugin

1. **Open an issue first** describing what you want to build and
   what capabilities it needs from the plugin API. We'd rather
   talk before you've written 1,000 lines.
2. **Branch from `main`**, add your crate to `crates/`,
   register it as a feature in the root `Cargo.toml`.
3. **Add tests.** No plugin merges without them.
4. **Document.** A `README.md` in the crate explaining what
   the plugin does and how to configure it. Update
   [CONFIG.md](CONFIG.md) with the plugin's config keys.
5. **Open a PR.** See [CONTRIBUTING.md](../CONTRIBUTING.md) for
   the contribution workflow and the license grant.

## Stability commitments

Until the project hits 1.0, the plugin API is **not stable**. We
will document every breaking change in the release notes, but
plugin authors should expect to update their code with each
release. After 1.0, the plugin API follows semver.

The `Host` interface and `Plugin` trait are the two pieces most
likely to evolve in the pre-1.0 phase. The capability traits
(`TransportEngine`, etc.) are likely to stabilise sooner because
they have fewer cross-cutting dependencies.
