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

**TBD** - a complete reference plugin (`crates/bbs-example-hello/`)
that prints a log line every time a message is posted, with a
configurable log level. About 50 lines of Rust. Published when the
implementation lands so this section can be filled in with real
code that compiles.

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
- Integration-test against a real `Host` implementation provided
  by `bbs-plugin-api::testing` (TBD: this helper is part of the
  initial `bbs-plugin-api` work)
- Use `proptest` for state-machine and parser code
- Document any test that requires external infrastructure
  (real radio, real broker) and gate it behind a cargo feature
  so CI doesn't run it by default

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
