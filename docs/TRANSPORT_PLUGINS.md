# Transport Plugin Development Guide

This document is the authoritative guide for building transport plugins for
Supply Drop BBS. A transport plugin is how users connect to the BBS — the
MeshCore radio bridge, the Unix domain socket CLI, and the HTTP web admin
are all transports. You can add Meshtastic, APRS, Telnet, IRC bridge, Matrix,
or any other channel by writing a new transport crate.

---

## Table of contents

1. [Architecture overview](#1-architecture-overview)
2. [Multi-transport: running several at once](#2-multi-transport-running-several-at-once)
3. [The plugin traits](#3-the-plugin-traits)
4. [Session lifecycle](#4-session-lifecycle)
5. [Command parsing and dispatch](#5-command-parsing-and-dispatch)
6. [Rendering responses](#6-rendering-responses)
7. [Receiving domain events and notifications](#7-receiving-domain-events-and-notifications)
7a. [Advisory events and state reconciliation](#7a-advisory-events-and-state-reconciliation)
7b. [Notification retry semantics](#7b-notification-retry-semantics)
8. [Payload size constraints](#8-payload-size-constraints)
9. [Persistent node identity (auto-login)](#9-persistent-node-identity-auto-login)
9a. [Session lifetime and restart behavior](#9a-session-lifetime-and-restart-behavior)
10. [Registering a transport in the host binary](#10-registering-a-transport-in-the-host-binary)
11. [Configuration](#11-configuration)
12. [Error handling](#12-error-handling)
13. [Testing](#13-testing)
14. [Complete worked example](#14-complete-worked-example)
15. [Style guide and contribution checklist](#15-style-guide-and-contribution-checklist)

---

## 1. Architecture overview

```
┌─────────────────────────────────────────────────────────────────┐
│                        BBS host process                         │
│                                                                 │
│  ┌──────────────────────────────────────────────────────────┐   │
│  │                       bbs-core                           │   │
│  │  BbsHost: rooms, messages, users, sessions, audit log    │   │
│  └────────────────────────┬─────────────────────────────────┘   │
│                           │  Arc<dyn Host>                      │
│          ┌────────────────┼────────────────┐                    │
│          │                │                │                    │
│  ┌───────▼──────┐ ┌───────▼──────┐ ┌───────▼──────┐            │
│  │  bbs-mesh    │ │  bbs-cli     │ │  bbs-web     │  ...       │
│  │  (MeshCore   │ │  (Unix sock  │ │  (HTTP admin │            │
│  │   radio)     │ │   CLI)       │ │   UI)        │            │
│  └──────────────┘ └──────────────┘ └──────────────┘            │
└─────────────────────────────────────────────────────────────────┘
```

The BBS core (`bbs-core`) owns all persistent state: users, rooms, messages,
sessions, and the audit log. It exposes this state through the `Host` trait
(`bbs-plugin-api`). Transport plugins receive an `Arc<dyn Host>` at startup
and never touch the database directly.

Every transport is structurally identical from the host's perspective:

- It holds an `Arc<dyn Host>`.
- It creates sessions via `host.create_session(transport_name)`.
- It feeds user input as `Command` values via `host.process_command(session, cmd)`.
- It receives `Response` values back and renders them for the user.
- It subscribes to the domain event bus to receive unsolicited notifications.
- It ends sessions via `host.end_session(session)` on disconnect.

There is **no shared state** between transports other than what flows through
the host. Sessions from different transports are in the same namespace and
users can be simultaneously connected on multiple transports.

---

## 2. Multi-transport: running several at once

Multiple transports run simultaneously in the same process. This is the normal
production configuration — MeshCore radio, the CLI socket, and the web admin
UI are all active at the same time.

**What this means:**

- A user connected via MeshCore radio and a user on Meshtastic are in the
  same BBS. They see each other's room messages. They can DM each other.
- A sysop action taken in the web admin UI (ban, validate, delete message)
  immediately affects all active transport sessions — the host broadcasts a
  `DomainEvent` and each transport's event handler acts on it.
- Session IDs are unique across all transports. There is no "transport" field
  on `SessionId`; the transport name is recorded at session creation for audit
  purposes only.
- The domain event broadcast channel fans out to every subscriber
  independently. If three transports are running, each gets its own
  `broadcast::Receiver<DomainEvent>`.

**What this does NOT mean:**

- Transports do not know about each other. `bbs-mesh` cannot reach into
  `bbs-meshtastic`. Coordination happens through the host (post a message,
  emit an event) not through direct plugin-to-plugin calls.
- A session's `notify()` call goes to the transport that owns that session.
  The host routes notifications by looking up the transport name recorded
  at `create_session` time. A mesh session is notified through the mesh
  transport, not the CLI transport, even if the same user is connected on both.

**Adding a third transport at runtime** is not currently supported — transports
are compiled in and started at startup (see ADR-0004). The planned path to
runtime-loadable plugins is WASM, but that is post-1.0.

### 2.1 Wiring two transports together: step-by-step

Suppose you are adding a Meshtastic transport (`bbs-meshtastic`) alongside the
existing MeshCore transport (`bbs-mesh`). Here is exactly what you change.

#### Step 1 — `Cargo.toml` (workspace root)

Add the new crate and a feature flag. **Each transport gets its own flag** so
operators can choose what to compile in.

```toml
# Cargo.toml (workspace members)
[workspace]
members = [
    "crates/bbs-core",
    "crates/bbs-plugin-api",
    "crates/bbs-mesh",       # existing MeshCore transport
    "crates/bbs-meshtastic", # new transport
    "crates/bbs-cli",
    "crates/bbs-web",
    "src",                   # the host binary
]

# Supply-drop-bbs binary Cargo.toml (src/Cargo.toml or Cargo.toml)
[features]
default = ["transport-mesh", "transport-meshtastic", "transport-cli", "admin-web"]
transport-mesh       = ["dep:bbs-mesh"]
transport-meshtastic = ["dep:bbs-meshtastic"]   # NEW
transport-cli        = ["dep:bbs-cli"]
admin-web            = ["dep:bbs-web"]

[dependencies]
bbs-mesh        = { path = "crates/bbs-mesh",        optional = true }
bbs-meshtastic  = { path = "crates/bbs-meshtastic",  optional = true }  # NEW
bbs-cli         = { path = "crates/bbs-cli",         optional = true }
bbs-web         = { path = "crates/bbs-web",         optional = true }
```

#### Step 2 — `src/main.rs`: declare the handle

Each transport gets an `Option<T>` handle in `cmd_run` so it can be stopped
cleanly on shutdown. Follow the exact same pattern as the existing transports:

```rust
// In cmd_run(), section 6 (plugins):

#[cfg(feature = "transport-meshtastic")]
let meshtastic_transport =
    init_meshtastic_plugin(&cfg.plugins.meshtastic, Arc::clone(&host)).await;

// ... (shutdown section, reverse order) ...

#[cfg(feature = "transport-meshtastic")]
if let Some(t) = meshtastic_transport {
    if let Err(e) = t.stop().await {
        error!("meshtastic transport stop error: {e}");
    }
}
```

Add the corresponding `init_meshtastic_plugin` helper:

```rust
#[cfg(feature = "transport-meshtastic")]
async fn init_meshtastic_plugin(
    cfg: &bbs_meshtastic::MeshtasticConfig,
    host: Arc<dyn bbs_plugin_api::Host>,
) -> Option<bbs_meshtastic::MeshtasticTransport> {
    use bbs_plugin_api::Plugin;

    if !cfg.enabled {
        info!("meshtastic: disabled in config — skipping");
        return None;
    }

    let transport = match bbs_meshtastic::MeshtasticTransport::init(cfg.clone(), host).await {
        Ok(t) => t,
        Err(e) => {
            error!("meshtastic transport init failed: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = transport.start().await {
        error!("meshtastic transport start failed: {e}");
        std::process::exit(1);
    }

    Some(transport)
}
```

#### Step 3 — `src/config.rs`: add the config section

```rust
#[cfg(feature = "transport-meshtastic")]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeshtasticPluginConfig {
    pub enabled: bool,
    // TCP host of the Meshtastic companion daemon:
    pub host: String,
    pub port: u16,
}

// In the top-level Config struct:
#[cfg(feature = "transport-meshtastic")]
pub meshtastic: MeshtasticPluginConfig,
```

#### Step 4 — `config.toml` (operator config)

```toml
[plugins.meshtastic]
enabled = true
host    = "127.0.0.1"
port    = 4404
```

### 2.2 What happens at runtime with two radio transports

Once both transports are running, the following all work correctly without any
extra plumbing:

| Scenario | What happens |
|---|---|
| Alice on MeshCore DMs Bob on Meshtastic | `post_direct` writes to DB; Bob's Meshtastic session receives a `Notification::NewDirectMessage` pushed by the host's notify loop |
| Sysop bans a user from web admin | `DomainEvent::SessionEnded` fires; **both** the MeshCore transport and the Meshtastic transport receive it; each transport ends any session belonging to that user |
| Sysop posts to a room from CLI | `DomainEvent::MessagePosted` fires; subscribers on all transports see it and can push in-session notifications to users who are in that room |
| User connects on both MeshCore and Meshtastic simultaneously | Both sessions coexist. The same `Username` appears twice in `W` (who's online). Read-state (last-read pointer) is shared — reading on one transport advances the pointer for the other |

### 2.3 Payload size is per-transport, not global

Each transport enforces its own `MAX_REPLY_BYTES` constant. The host returns
the same `Response` enum value to all transports; it is each transport's
responsibility to truncate or paginate before sending.

```
Transport        Max text per frame   Notes
──────────────   ──────────────────   ─────────────────────────────────────
MeshCore radio   156 bytes            MAX_FRAME_SIZE(172) − 16 B overhead
Meshtastic       ~220 bytes           MTU varies by modem preset
APRS             ~64 bytes            AX.25 payload minus header
CLI / TCP        unlimited            full UTF-8, no truncation needed
Web admin API    unlimited            JSON over HTTP
```

A long room listing that fits on CLI will be silently truncated on APRS.
Design your `Response` rendering to be shortest-first: lead with the most
important information so truncation loses only the least important tail.

---

## 3. The plugin traits

Every transport implements two traits from `bbs-plugin-api`:

### `Plugin`

```rust
#[async_trait]
pub trait Plugin: Send + Sync + 'static {
    /// Stable, unique identifier. Lowercase ASCII, hyphens allowed.
    /// This string is passed to host.create_session() and recorded in
    /// audit logs. Never change it after shipping.
    fn name(&self) -> &'static str;

    /// Human-readable version. Use env!("CARGO_PKG_VERSION").
    fn version(&self) -> &'static str;

    /// Optional list of other plugin names this one depends on.
    /// The supervisor checks this at startup and aborts if any
    /// dependency is not compiled in.
    fn dependencies(&self) -> &[&'static str] { &[] }

    /// Long-running initialization: open connections, validate config,
    /// warm caches. Called once before start(). Failure here aborts
    /// the entire BBS startup with a clear error.
    async fn init(
        config: Self::Config,
        host: Arc<dyn Host>,
    ) -> Result<Self, PluginError>
    where
        Self: Sized;

    /// Spawn worker tasks, open listeners, begin accepting traffic.
    /// Called after all plugins have init'd. Failure here is fatal.
    async fn start(&self) -> Result<(), PluginError>;

    /// Cooperative shutdown. Signal workers to stop, drain in-flight
    /// messages, close listeners. Must complete within ~10 seconds.
    async fn stop(&self) -> Result<(), PluginError>;

    /// Config schema. Deserialized from [plugins.<name>] in config.toml.
    type Config: serde::de::DeserializeOwned + Send;
}
```

### `TransportEngine`

```rust
#[async_trait]
pub trait TransportEngine: Plugin {
    /// Push an unsolicited notification to a live session.
    ///
    /// Called by the host's notification router when a domain event
    /// concerns a user connected through this transport (e.g., a DM
    /// arrives, their account is validated, another user joins a room).
    ///
    /// Returns NotifyOutcome so the router can decide whether to retry,
    /// queue, or drop. The transport must NOT block here — if the
    /// underlying connection is slow, enqueue and return Queued.
    async fn notify(
        &self,
        session: SessionId,
        payload: Notification,
    ) -> NotifyOutcome;
}
```

The `notify()` method is called from the host's event routing machinery.
Your transport does not call it directly — instead it *implements* it.
How you deliver the notification (write to a TCP socket, queue to a channel,
log it) is entirely up to the transport.

---

## 4. Session lifecycle

Sessions are the unit of a user connection. One physical connection =
one session. A user can have multiple simultaneous sessions (connected via
radio and CLI at the same time).

```
connection arrives
       │
       ▼
host.create_session("my-transport")
       │ returns SessionId
       ▼
[session is unbound — Unvalidated tier]
       │
       ▼
process_command(session, Command::Register { username })
 or
process_command(session, Command::Login { username })
       │ host handles the workflow (prompts for password, etc.)
       │ session becomes bound to a user when login completes
       ▼
[session is bound — User/Aide/Sysop tier]
       │
       ▼
...process_command calls for each user message...
       │
       ▼
connection closes (user disconnects, timeout, error)
       │
       ▼
host.end_session(session)
[session is destroyed — DomainEvent::SessionEnded emitted]
```

**Key rules:**

- Always call `end_session` when a connection closes, even if the session
  was never authenticated. The host emits `DomainEvent::SessionEnded` which
  other subscribers (e.g., the web admin "who's online" list) rely on.
- `end_session` is idempotent. Calling it on an unknown session returns `Ok`.
- Sessions are identified by `SessionId` (opaque u64). Never store raw
  integers; always use the newtype.
- The `transport` string passed to `create_session` must exactly match your
  `Plugin::name()` return value. This is how the host routes `notify()` calls
  back to your transport.

**Checking session state:**

```rust
// Check what permission level the session currently has:
let ctx = host.permission_ctx(session).await?;
match ctx.level {
    PermissionLevel::Unvalidated => { /* not logged in */ }
    PermissionLevel::User        => { /* normal user */ }
    PermissionLevel::Aide        => { /* moderator */ }
    PermissionLevel::Sysop       => { /* full access */ }
}

// Check if the session is bound to a user:
if let Some(username) = ctx.username {
    println!("logged in as {username}");
}
```

---

## 5. Command parsing and dispatch

The host speaks `Command` values — a protocol-neutral enum defined in
`bbs-plugin-api`. Your transport's job is to turn raw wire input into
`Command` values and pass them to `host.process_command`.

```rust
// Command enum (non_exhaustive — always handle the _ arm)
pub enum Command {
    // Authentication
    Help { topic: Option<String> },
    Register { username: Username },
    Login    { username: Username },
    Logout,
    Whoami,
    WorkflowReply { reply: String },  // response to a host Prompt
    Unknown { raw: String },

    // Room navigation
    ListRooms,
    GoNextUnread,
    ChangeRoom  { target: String },   // name or numeric ID
    GoMail,
    IgnoreRoom,

    // Message reading
    ReadNew,
    ReadForward  { after: Option<i64> },
    ReadReverse,
    ScanMessages,
    FastForward,

    // Message posting
    EnterMessage,
    DeleteMessage { id: i64 },

    // Session control
    Quit,
    Cancel,

    // Moderation (requires Aide+)
    WhoIsOnline,
    ListPending,
    ValidateUser { username: Username },
    BlockUser    { username: Username, force: Option<bool> },
    BanUser      { username: Username },
    UnbanUser    { username: Username },
    EditProfile,

    // Sysop (requires Sysop)
    CreateRoom { name: String },
    DeleteRoom { name: String },
    EditRoom,
    EditUser   { username: Username },
}
```

The host enforces permissions internally. You do not need to check whether
the user is allowed to run a command before calling `process_command` — if
they lack the required tier, the host returns an appropriate `Response::Error`.

**Workflow replies** deserve special attention. When the host returns
`Response::Prompt`, it is waiting for a free-form response (e.g., the user
is mid-registration and the host just asked for a password). The transport
must track this per-session and, while a prompt is pending, treat the next
incoming message as `Command::WorkflowReply { reply: raw_text }` rather than
trying to parse it as a keyword command.

`bbs-mesh` handles this with a per-session `awaiting_reply` flag in a shared
`SessionState` struct. The mesh command parser (`parse_command`) receives this
flag and produces `WorkflowReply` when it is set. You can copy this pattern
or design your own.

---

## 6. Rendering responses

`host.process_command` returns `Result<Response, HostError>`. On `Ok`, render
the response for your transport:

```rust
pub enum Response {
    Text(String),
    Prompt { text: String, hide_input: bool },
    LoggedIn  { user: Username },
    LoggedOut,
    Error(String),
    // non_exhaustive — handle _ with a no-op or fallback
}
```

**`Text(s)`** — Send `s` to the user verbatim. For radio transports, this is
the main response type. Check the length against your payload limit (see
section 8).

**`Prompt { text, hide_input }`** — The host is mid-workflow and expects a
follow-up reply. Send `text` to the user. If `hide_input` is true, the user
is entering a password — suppress echo if your transport supports it. Mark
this session as `awaiting_reply` so the next message is routed as
`WorkflowReply`.

**`LoggedIn { user }`** — Authentication completed. Render a welcome message.
On radio transports this is typically `"Welcome, {user}. Type 'H' for commands."`.
On Telnet you might render an ANSI welcome screen.

**`LoggedOut`** — The user logged out. Send a goodbye message and close the
connection if your transport is connection-oriented.

**`Error(s)`** — A recoverable error (wrong password, room not found, etc.).
Send the error message; do NOT close the session.

**Unknown variants** — `Response` is `#[non_exhaustive]`. Handle `_` with a
no-op or a logged warning so future variants don't break your transport.

---

## 7. Receiving domain events and notifications

Transports receive two kinds of unsolicited messages from the host:

### Domain events (broadcast to all subscribers)

```rust
// In Plugin::init or Plugin::start:
let mut events = host.events(); // broadcast::Receiver<DomainEvent>

tokio::spawn(async move {
    loop {
        match events.recv().await {
            Ok(event) => handle_event(event),
            Err(broadcast::error::RecvError::Lagged(n)) => {
                warn!("event bus lagged, dropped {n} events");
                // This is normal under load — the event bus is best-effort.
                // Don't treat it as fatal.
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
});
```

The `DomainEvent` enum (non_exhaustive):

```rust
pub enum DomainEvent {
    SessionCreated       { session: SessionId, transport: String },
    SessionAuthenticated { session: SessionId, user: Username },
    SessionEnded         { session: SessionId, reason: String },
    MessagePosted        { sender: Username, recipient: Option<Username>, message_id: i64 },
    UserCreated          { user: Username },
    UserValidated        { user: Username },
    CommandExecuted      { session: SessionId, command: String, user: Option<Username> },
}
```

Common uses:
- `MessagePosted` with `recipient: Some(username)` → push a `MailWaiting`
  notification to that user's active sessions.
- `UserValidated` → notify the newly-validated user their account is active.
- `SessionEnded` → update your internal session map.

### Notifications (routed to specific transports)

The host calls `TransportEngine::notify(session, payload)` on your transport
when it needs to push something to one of your sessions. You implement this
method.

```rust
pub enum Notification {
    Text(String),
    MailWaiting { count: u32 },
    SystemEvent(String),
    // non_exhaustive
}

pub enum NotifyOutcome {
    Delivered,              // sent or reliably queued
    Queued,                 // enqueued for retry
    Dropped,                // session offline, message lost
    PermanentFailure(String), // session gone, stop trying
}
```

Typical implementation pattern:

```rust
async fn notify(&self, session: SessionId, payload: Notification) -> NotifyOutcome {
    // Look up the outbound channel for this session.
    let tx = {
        let sessions = self.sessions.lock().unwrap();
        sessions.get(&session).cloned()
    };

    let Some(tx) = tx else {
        return NotifyOutcome::Dropped; // session not found in our map
    };

    let text = match payload {
        Notification::Text(s) => s,
        Notification::MailWaiting { count } => format!("You have {count} new message(s)."),
        Notification::SystemEvent(s) => s,
        _ => return NotifyOutcome::Dropped, // unknown variant
    };

    // Enqueue to the outbound channel; the event loop sends it.
    match tx.try_send(text) {
        Ok(())                           => NotifyOutcome::Delivered,
        Err(mpsc::error::TrySendError::Full(_))   => NotifyOutcome::Queued,
        Err(mpsc::error::TrySendError::Closed(_)) => NotifyOutcome::PermanentFailure(
            "send channel closed".into()
        ),
    }
}
```

**Do not block in `notify()`**. If the underlying connection is slow or the
send buffer is full, enqueue the message and return `Queued`. The host may
retry or drop based on the outcome you return.

---

## 7a. Advisory Events and State Reconciliation

Domain events are advisory, not authoritative.

The event bus exists to provide low-latency notifications and cache
invalidation signals to transports. It is not intended to be a durable
replication stream or guaranteed-delivery synchronization mechanism.

Internally, the event bus uses a bounded broadcast channel. Under load, slow
subscribers may lag behind and lose events.

**What this means in practice:**

- Events may be dropped if a subscriber falls behind.
- Events are not replayed.
- Events may arrive late or out of order.
- Missing events are not considered a host error condition.

A lagged subscriber should treat its local state as potentially stale and
reconcile directly against the host. For example:

- rebuild online-user lists
- refresh unread counts
- re-query room membership
- re-check session existence

Do not assume that observing every event is required for correctness.

**Canonical handling pattern:**

```rust
loop {
    match events.recv().await {
        Ok(event) => {
            handle_event(&host, &sessions, event).await;
        }
        Err(broadcast::error::RecvError::Lagged(n)) => {
            warn!("event bus lagged, dropped {n} events");
            // Reconcile local cached state from the host.
            reconcile_session_map(&host, &sessions).await;
        }
        Err(broadcast::error::RecvError::Closed) => {
            break;
        }
    }
}
```

If your transport maintains any in-memory representation of host state, treat
it as a cache only. Examples include:

- online-user lists
- room membership caches
- unread counters
- active-node tracking

The event bus invalidates these caches, but it is not authoritative state
synchronization. If reconciliation is required, query the host directly rather
than waiting for future events.

For durable delivery guarantees, audit trails, or replayable history, use
explicit host APIs or persistence mechanisms rather than the domain event
stream.

---

## 7b. Notification Retry Semantics

The transport owns all retry behavior. The host does not retry notifications
after `notify()` returns.

When the host calls:

```rust
transport.notify(session_id, notification).await
```

the returned `NotifyOutcome` tells the host how routing should proceed.

| Outcome | Meaning |
|---|---|
| `Delivered` | Notification was sent or durably accepted by the transport |
| `Queued` | Transport accepted responsibility for later delivery |
| `Dropped` | Notification could not be delivered due to transient conditions |
| `PermanentFailure` | Session is no longer valid and should no longer be routed |

If a transport returns `Queued`, the host assumes the transport will handle
all retry behavior internally. The host does not:

- retry notifications
- implement backoff
- maintain a global delivery queue
- track delivery acknowledgement

This avoids duplicate-delivery ambiguity and keeps delivery policy
transport-specific.

**Recommended transport pattern:**

```rust
async fn notify(
    &self,
    session: SessionId,
    payload: Notification,
) -> NotifyOutcome {
    let tx = {
        self.sessions
            .lock()
            .unwrap()
            .get(&session)
            .map(|entry| entry.tx.clone())
    };

    let Some(tx) = tx else {
        return NotifyOutcome::Dropped;
    };

    match tx.try_send(render(payload)) {
        Ok(()) => {
            NotifyOutcome::Queued
        }
        Err(mpsc::error::TrySendError::Full(_)) => {
            NotifyOutcome::Dropped
        }
        Err(mpsc::error::TrySendError::Closed(_)) => {
            NotifyOutcome::PermanentFailure(
                "session closed".into()
            )
        }
    }
}
```

Do not block inside `notify()` and do not implement retry loops directly
inside the method. The host may call `notify()` from shared routing paths —
blocking or long-running retry loops inside `notify()` can stall delivery to
unrelated sessions. Instead, transports should enqueue work quickly, return
immediately, and handle retry from background tasks or internal queues.

**`Dropped` vs `PermanentFailure`**

Use `Dropped` for transient conditions:

- node temporarily offline
- queue full
- intermittent radio loss
- temporary transport backpressure

Use `PermanentFailure` only when the session is definitively invalid:

- socket closed permanently
- session removed from transport state
- connection teardown completed

The host may stop routing notifications to a session after `PermanentFailure`.

**Radio Transport Note**

Radio transports often cannot synchronously confirm over-the-air delivery.
Returning `Queued` after successfully enqueueing a frame for transmission is
correct even if the remote node has not yet acknowledged receipt. Mesh-layer
acknowledgement behavior belongs to the radio protocol itself, not to
`NotifyOutcome`.

---

## 8. Payload size constraints

Radio transports have hard payload size limits imposed by the physical layer.
You must enforce these in your transport — the host does not truncate
responses on your behalf (beyond the safety net already in `bbs-mesh`).

| Transport              | Approx. usable payload  | Notes                            |
|------------------------|-------------------------|----------------------------------|
| MeshCore companion     | 156 bytes               | `MAX_FRAME_SIZE(172) - 16` overhead |
| Meshtastic             | ~160–220 bytes          | Depends on channel settings      |
| APRS (AX.25)           | ~64 bytes               | Path + header reduce usable body |
| LoRa (raw)             | 64–255 bytes            | SF and BW dependent              |
| Telnet / TCP           | Unlimited               | Paginate long output for UX      |

**Where to enforce the limit:**

In your transport's response renderer, before calling `host` or before
sending the response to the user:

```rust
const MAX_TEXT_BYTES: usize = 156; // tune for your transport

fn truncate_to_limit(s: String, limit: usize) -> String {
    if s.len() <= limit {
        return s;
    }
    // Walk back to a valid UTF-8 boundary.
    let mut end = limit;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    // Trim to a clean word/line boundary if possible.
    let truncated = &s[..end];
    truncated.trim_end().to_owned()
}
```

**For transports with very small payloads (APRS, ~64 bytes):**

A single-packet model won't work for most commands. Consider:

- **Abbreviated output**: shorter room listings, terse help text, abbreviated
  message format.
- **Paging**: `N 1`, `N 2`, etc. to step through messages one at a time.
  Each page is one packet. The transport tracks the current page per session.
- **Request/reply chunking**: the transport breaks a long response into
  multiple sequential packets and sends them with a delay between each to
  avoid collisions on the shared channel.

Chunking belongs entirely inside the transport crate. The host returns a
full string; your transport splits it. The host never knows about chunks.

**Canned strings in `bbs-core`** (help text, prompts, error messages) must
also fit within the tightest transport's limit. The test
`host::tests::help_strings_fit_mesh_payload` enforces `≤ 156 bytes` for all
canned strings. If your transport has a smaller limit, add an additional
assertion in your own tests, or contribute stricter canned strings upstream.

---

## 9. Persistent node identity (auto-login)

Radio transports typically identify nodes by a public key prefix rather than
by a TCP connection. Between BBS restarts, or after a node goes off-air and
comes back, you want to restore the user's session without requiring them to
type their password again.

The `Host` trait provides three methods for this:

```rust
// On connection: check for a stored binding.
// `prefix` = first 6 bytes of the node's public key (or equivalent opaque ID).
// `ttl_days` = how old a binding can be before it's considered stale.
// Returns Some(username) if the session was auto-restored.
async fn mesh_node_restore(
    &self,
    session: SessionId,
    prefix: [u8; 6],
    ttl_days: u32,
) -> Result<Option<Username>, HostError>;

// After a successful login: save the binding.
async fn mesh_node_bind(
    &self,
    session: SessionId,
    prefix: [u8; 6],
) -> Result<(), HostError>;

// On explicit logout: remove the binding.
async fn mesh_node_unbind(
    &self,
    prefix: [u8; 6],
) -> Result<(), HostError>;
```

> **Note:** These methods are currently named `mesh_node_*` because they were
> introduced for the MeshCore transport. The concept applies equally to any
> radio transport that identifies nodes by a stable key prefix. If you are
> building a Meshtastic transport, use these same methods with the
> Meshtastic node's public key prefix as the `prefix` argument. A future API
> refactor may rename them to `node_credential_*`.

**Typical flow:**

```rust
// When a known node contacts us:
let session = host.create_session("my-transport").await?;

let auto_login = host
    .mesh_node_restore(session, node_prefix, ttl_days: 30)
    .await?;

if let Some(username) = auto_login {
    // Session is now authenticated. Send a welcome-back message.
    send_to_node(node, format!("Welcome back, {username}!"));
} else {
    // No stored binding. Send the normal welcome + login prompt.
    send_to_node(node, "Welcome to Supply Drop BBS! LOGIN <user> or REGISTER <user>.");
}

// Later, after a successful login (Response::LoggedIn):
host.mesh_node_bind(session, node_prefix).await?;

// On explicit logout (Response::LoggedOut):
host.mesh_node_unbind(node_prefix).await?;
```

---

## 9a. Session Lifetime and Restart Behavior

Sessions are ephemeral runtime objects and do not survive process restarts.

A `SessionId` is valid only for the lifetime of the running host process.
Transports must not:

- persist `SessionId` values
- assume session identifiers are stable across restarts
- attempt to restore old sessions after a restart

When the host process exits or crashes:

- all sessions are lost
- all in-progress workflows are lost
- all pending prompts are lost
- all transport-owned session mappings become invalid

After restart, reconnecting users or nodes receive newly created sessions.

### Persistent Identity vs Ephemeral Session

Persistent node identity and session identity are separate concepts.

Persistent identity is typically backed by durable credential storage, such as:

- node public-key bindings
- login credentials
- transport identity mappings

Session state itself is not persisted. For example:

```rust
host.create_session("meshtastic").await
```

creates a brand new runtime session. A later identity restore step may
authenticate that session against stored credentials, but it does not reuse
the previous `SessionId`.

### Workflow Continuation

Workflow continuation across reconnects is not currently supported. If a
transport disconnects during registration, login, prompts, or multi-step
workflows, the reconnecting user typically begins a fresh session and restarts
the workflow. Transports should design UX flows accordingly:

- keep prompts concise
- avoid unnecessarily large transient state
- tolerate interrupted interaction

### Session Cleanup

Transports should always call `host.end_session(session_id).await` when a
connection or transport context terminates. This includes:

- disconnect paths
- transport shutdown
- task cancellation
- error handling paths

**Recommended pattern:**

```rust
async fn handle_connection(
    conn: Connection,
    session: SessionId,
    host: Arc<dyn Host>,
) {
    let result = run_session_loop(&conn, session, &host).await;

    // Always clean up the session, even on error.
    let _ = host.end_session(session).await;

    if let Err(err) = result {
        warn!("session {:?} ended with error: {}", session, err);
    }
}
```

Transports should not assume abandoned sessions are automatically cleaned up
immediately.

---

## 10. Registering a transport in the host binary

Adding a transport involves four files:

### 1. `Cargo.toml` — add a feature and workspace crate

```toml
[features]
default         = ["transport-cli", "transport-mesh", "admin-web"]
transport-cli   = ["dep:bbs-cli"]
transport-mesh  = ["dep:bbs-mesh"]
admin-web       = ["dep:bbs-web"]
transport-mynet = ["dep:bbs-mynet"]    # ← add this

[dependencies]
bbs-mynet = { workspace = true, optional = true }  # ← and this
```

In the workspace `[workspace.dependencies]`:

```toml
bbs-mynet = { path = "crates/bbs-mynet", version = "0.2.0" }
```

### 2. `src/main.rs` — add a feature-gated init function and wire it in

```rust
// In the Commands enum, no changes needed — transports don't add subcommands.

// Add a cfg-gated init function following the same pattern as init_mesh_plugin:
#[cfg(feature = "transport-mynet")]
async fn init_mynet_plugin(
    cfg: &bbs_mynet::MyNetConfig,
    host: Arc<dyn bbs_plugin_api::Host>,
) -> Option<bbs_mynet::MyNetTransport> {
    use bbs_plugin_api::Plugin;

    if !cfg.enabled {
        info!("mynet transport: disabled in config — skipping");
        return None;
    }

    let transport = match bbs_mynet::MyNetTransport::init(cfg.clone(), host).await {
        Ok(t) => t,
        Err(e) => {
            error!("mynet transport init failed: {e}");
            std::process::exit(1);
        }
    };

    if let Err(e) = transport.start().await {
        error!("mynet transport start failed: {e}");
        std::process::exit(1);
    }

    Some(transport)
}
```

In `cmd_run`, add to the plugin init sequence (after all other inits):

```rust
#[cfg(feature = "transport-mynet")]
let mynet_transport = init_mynet_plugin(&cfg.plugins.mynet, Arc::clone(&host)).await;
```

In the shutdown sequence (in reverse init order):

```rust
#[cfg(feature = "transport-mynet")]
if let Some(ref t) = mynet_transport {
    use bbs_plugin_api::Plugin;
    if let Err(e) = t.stop().await {
        warn!("mynet transport stop error: {e}");
    }
}
```

### 3. `src/config.rs` — add the plugin config section

Your transport's config struct must be reachable from the top-level config.
Follow the existing pattern for `MeshConfig` and `CliConfig`:

```rust
#[derive(Deserialize, Clone)]
pub struct Config {
    pub bbs:      BbsConfig,
    pub database: DatabaseConfig,
    // ... existing fields ...
    pub plugins:  PluginsConfig,
}

#[derive(Deserialize, Clone, Default)]
pub struct PluginsConfig {
    pub mesh:   bbs_mesh::MeshConfig,
    pub cli:    bbs_cli::CliConfig,
    pub web:    bbs_web::WebConfig,
    #[cfg(feature = "transport-mynet")]
    pub mynet:  bbs_mynet::MyNetConfig,
}
```

### 4. `config.toml` / `config.example.toml` — document the new section

```toml
[plugins.mynet]
enabled = true
listen = "0.0.0.0:4321"
# ... your transport's options
```

---

## 11. Configuration

Your transport's config struct is deserialized from `[plugins.<name>]` in
`config.toml`. Use `serde::Deserialize` with `#[serde(default)]` for optional
fields so the section can be omitted entirely when the plugin is compiled out.

```rust
#[derive(Debug, Clone, serde::Deserialize)]
pub struct MyNetConfig {
    /// Enable or disable this transport. Defaults to true.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Address and port to listen on.
    #[serde(default = "default_listen")]
    pub listen: String,

    /// Maximum payload size in bytes for this transport.
    #[serde(default = "default_max_payload")]
    pub max_payload_bytes: usize,
}

fn default_true()        -> bool   { true }
fn default_listen()      -> String { "0.0.0.0:4321".into() }
fn default_max_payload() -> usize  { 156 }

impl Default for MyNetConfig {
    fn default() -> Self {
        Self {
            enabled:          default_true(),
            listen:           default_listen(),
            max_payload_bytes: default_max_payload(),
        }
    }
}
```

Expose `MyNetConfig` as `pub` from your crate's `lib.rs` so `src/config.rs`
can embed it in `PluginsConfig`.

---

## 12. Error handling

Use `thiserror` for your error types. Distinguish between fatal errors (config
invalid, listener failed to bind) and transient errors (one connection dropped,
frame decode failed):

```rust
#[derive(Debug, thiserror::Error)]
pub enum MyNetError {
    #[error("invalid config: {0}")]
    Config(String),

    #[error("listener failed to start: {0}")]
    Listen(#[from] std::io::Error),

    #[error("frame decode error: {0}")]
    Decode(String),

    #[error("connection closed")]
    ConnectionClosed,
}

impl From<MyNetError> for bbs_plugin_api::error::PluginError {
    fn from(e: MyNetError) -> Self {
        match e {
            MyNetError::Config(s) => PluginError::InvalidConfig(s),
            MyNetError::Listen(e) => PluginError::StartFailed(e.to_string()),
            _ => PluginError::Runtime(e.to_string()),
        }
    }
}
```

Never `unwrap()` or `expect()` outside of test code. Use `?` and propagate
errors to the supervisor. For worker tasks that must not crash the whole
plugin on a single bad connection, log the error and continue:

```rust
tokio::spawn(async move {
    if let Err(e) = handle_connection(conn, host).await {
        warn!("connection error: {e}");
        // Don't propagate — other connections are unaffected.
    }
});
```

---

## 13. Testing

### Unit tests

Test your command parser exhaustively. The `bbs-mesh` command parser tests in
`crates/bbs-mesh/src/command.rs` are a good reference.

### Integration tests against MockHost

`bbs-plugin-api` provides `testing::MockHost`, a scriptable host implementation
that does not require a database. Use it to test your transport's handling of
every `Response` variant:

```rust
use bbs_plugin_api::testing::{MockHost, ScriptedResponse};

#[tokio::test]
async fn help_command_reaches_host() {
    let host = Arc::new(MockHost::new());
    host.script(vec![
        ScriptedResponse::Text("Help text here.".into()),
    ]);

    let transport = MyNetTransport::init(MyNetConfig::default(), host.clone())
        .await
        .unwrap();
    transport.start().await.unwrap();

    // Simulate a connection and send "H"
    // ... assert the response was delivered ...

    transport.stop().await.unwrap();
}
```

### Payload size tests

Add a test that every fixed string in your transport fits within your payload
limit:

```rust
#[test]
fn canned_strings_fit_payload() {
    const LIMIT: usize = 64; // your transport's limit
    let strings = [
        ("welcome", WELCOME_MSG),
        ("prompt",  PASSWORD_PROMPT),
    ];
    for (name, s) in strings {
        assert!(
            s.len() <= LIMIT,
            "{name} is {} bytes — exceeds {LIMIT}-byte payload limit",
            s.len()
        );
    }
}
```

### End-to-end / hardware tests

Tests that require a physical radio or external service should be gated behind
a cargo feature so CI doesn't run them by default:

```toml
[features]
hardware-tests = []
```

```rust
#[cfg(feature = "hardware-tests")]
#[tokio::test]
async fn real_radio_round_trip() { ... }
```

---

## 14. Complete worked example

Below is a minimal skeleton for a hypothetical Meshtastic transport. It is
not production-ready but demonstrates every integration point.

### `crates/bbs-meshtastic/Cargo.toml`

```toml
[package]
name    = "bbs-meshtastic"
version.workspace  = true
edition.workspace  = true

[dependencies]
bbs-plugin-api.workspace = true
async-trait.workspace    = true
tokio.workspace          = true
tracing.workspace        = true
serde.workspace          = true
thiserror                = "1"
# meshtastic = "0.1"   # hypothetical Meshtastic Rust client crate
```

### `crates/bbs-meshtastic/src/lib.rs`

```rust
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bbs_plugin_api::{
    error::{PluginError, TransportError},
    event::{DomainEvent, Notification, NotifyOutcome},
    identity::SessionId,
    plugin::Plugin,
    transport::TransportEngine,
    Host, Response,
};
use tokio::sync::{mpsc, watch};
use tracing::{info, warn};

// ── Config ────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, serde::Deserialize)]
pub struct MeshtasticConfig {
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// TCP address of the Meshtastic device or daemon.
    #[serde(default = "default_host")]
    pub device_host: String,
    #[serde(default = "default_port")]
    pub device_port: u16,
    #[serde(default = "default_max_payload")]
    pub max_payload_bytes: usize,
    #[serde(default = "default_credential_ttl")]
    pub credential_ttl_days: u32,
}

fn default_true()           -> bool   { true }
fn default_host()           -> String { "localhost".into() }
fn default_port()           -> u16    { 4403 }
fn default_max_payload()    -> usize  { 200 }
fn default_credential_ttl() -> u32    { 30 }

impl Default for MeshtasticConfig {
    fn default() -> Self {
        Self {
            enabled:             true,
            device_host:         default_host(),
            device_port:         default_port(),
            max_payload_bytes:   default_max_payload(),
            credential_ttl_days: default_credential_ttl(),
        }
    }
}

// ── Per-session state ─────────────────────────────────────────────────────────

struct SessionEntry {
    /// Channel to push outbound text to this node.
    outbound_tx: mpsc::Sender<String>,
    /// True when the host is mid-workflow and expecting a WorkflowReply.
    awaiting_reply: bool,
    /// The node's public key prefix, for credential binding.
    node_prefix: [u8; 6],
}

// ── Transport struct ──────────────────────────────────────────────────────────

pub struct MeshtasticTransport {
    config:      MeshtasticConfig,
    host:        Arc<dyn Host>,
    sessions:    Arc<Mutex<HashMap<SessionId, SessionEntry>>>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
}

// ── Plugin impl ───────────────────────────────────────────────────────────────

#[async_trait]
impl Plugin for MeshtasticTransport {
    type Config = MeshtasticConfig;

    fn name(&self)    -> &'static str { "transport-meshtastic" }
    fn version(&self) -> &'static str { env!("CARGO_PKG_VERSION") }

    async fn init(config: Self::Config, host: Arc<dyn Host>) -> Result<Self, PluginError> {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Ok(Self {
            config,
            host,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            shutdown_tx,
            shutdown_rx,
        })
    }

    async fn start(&self) -> Result<(), PluginError> {
        if !self.config.enabled {
            info!("meshtastic transport disabled in config — skipping");
            return Ok(());
        }

        // Connect to Meshtastic device and spawn the event loop.
        // In real code: open a TCP connection, subscribe to packets, etc.
        let host        = Arc::clone(&self.host);
        let sessions    = Arc::clone(&self.sessions);
        let config      = self.config.clone();
        let mut shutdown = self.shutdown_rx.clone();

        tokio::spawn(async move {
            tokio::select! {
                _ = run_event_loop(host, sessions, config) => {}
                _ = shutdown.changed() => { info!("meshtastic: shutting down"); }
            }
        });

        info!("meshtastic transport started");
        Ok(())
    }

    async fn stop(&self) -> Result<(), PluginError> {
        let _ = self.shutdown_tx.send(true);
        Ok(())
    }
}

// ── TransportEngine impl ──────────────────────────────────────────────────────

#[async_trait]
impl TransportEngine for MeshtasticTransport {
    async fn notify(&self, session: SessionId, payload: Notification) -> NotifyOutcome {
        let tx = {
            let sessions = self.sessions.lock().unwrap();
            sessions.get(&session).map(|e| e.outbound_tx.clone())
        };

        let Some(tx) = tx else {
            return NotifyOutcome::Dropped;
        };

        let text = match payload {
            Notification::Text(s)              => s,
            Notification::MailWaiting { count } => format!("You have {count} new message(s)."),
            Notification::SystemEvent(s)        => s,
            _                                  => return NotifyOutcome::Dropped,
        };

        match tx.try_send(text) {
            Ok(())                                    => NotifyOutcome::Delivered,
            Err(mpsc::error::TrySendError::Full(_))   => NotifyOutcome::Queued,
            Err(mpsc::error::TrySendError::Closed(_)) => {
                NotifyOutcome::PermanentFailure("channel closed".into())
            }
        }
    }
}

// ── Internal event loop ───────────────────────────────────────────────────────

async fn run_event_loop(
    host:     Arc<dyn Host>,
    sessions: Arc<Mutex<HashMap<SessionId, SessionEntry>>>,
    config:   MeshtasticConfig,
) {
    // In real code: loop over incoming Meshtastic packets.
    // For each DM packet:
    //   1. Extract node_prefix from the sender's node ID.
    //   2. Look up or create a session for this prefix.
    //   3. Determine if awaiting_reply and parse Command accordingly.
    //   4. Call host.process_command(session, cmd).await.
    //   5. Render and send the Response back to the node.
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
    }
}

async fn handle_incoming_dm(
    host:        &Arc<dyn Host>,
    sessions:    &Arc<Mutex<HashMap<SessionId, SessionEntry>>>,
    config:      &MeshtasticConfig,
    node_prefix: [u8; 6],
    text:        &str,
) {
    // ── 1. Find or create session ──────────────────────────────────────────────

    let (session, awaiting_reply) = {
        let sessions_guard = sessions.lock().unwrap();
        if let Some(entry) = sessions_guard.values()
            .find(|e| e.node_prefix == node_prefix)
        {
            // session already open — find its ID
            // (in real code, store session→entry mapping directly)
            todo!("look up session by prefix")
        } else {
            drop(sessions_guard);
            // New node — create a session and attempt auto-login.
            let session = host.create_session("transport-meshtastic").await.unwrap();

            let auto_login = host
                .mesh_node_restore(session, node_prefix, config.credential_ttl_days)
                .await
                .unwrap_or(None);

            let (tx, mut rx) = mpsc::channel(8);
            {
                let mut sessions_guard = sessions.lock().unwrap();
                sessions_guard.insert(session, SessionEntry {
                    outbound_tx: tx,
                    awaiting_reply: false,
                    node_prefix,
                });
            }

            // Spawn a task to forward outbound text to the radio.
            tokio::spawn(async move {
                while let Some(text) = rx.recv().await {
                    // send_to_meshtastic_node(node_prefix, text).await;
                    info!("→ node {node_prefix:02x?}: {text}");
                }
            });

            if let Some(username) = auto_login {
                // send_to_meshtastic_node(node_prefix,
                //     format!("Welcome back, {username}! Type H for commands."));
                info!("auto-login: {username}");
            } else {
                // send_to_meshtastic_node(node_prefix,
                //     "Welcome! LOGIN <user> or REGISTER <user>.");
            }

            (session, false)
        }
    };

    // ── 2. Parse command ───────────────────────────────────────────────────────

    let cmd = if awaiting_reply {
        bbs_plugin_api::command::Command::WorkflowReply { reply: text.to_owned() }
    } else {
        // Parse text into a Command. You can copy bbs-mesh's parse_command
        // or write your own parser tailored to your transport's conventions.
        parse_my_command(text)
    };

    // ── 3. Dispatch to host ────────────────────────────────────────────────────

    let response = match host.process_command(session, cmd).await {
        Ok(r)  => r,
        Err(e) => {
            warn!("process_command error: {e}");
            return;
        }
    };

    // ── 4. Handle special responses ────────────────────────────────────────────

    let is_prompt = matches!(response, Response::Prompt { .. });
    {
        let mut sessions_guard = sessions.lock().unwrap();
        if let Some(entry) = sessions_guard.get_mut(&session) {
            entry.awaiting_reply = is_prompt;
        }
    }

    if matches!(response, Response::LoggedIn { .. }) {
        let _ = host.mesh_node_bind(session, node_prefix).await;
    }
    if matches!(response, Response::LoggedOut) {
        let _ = host.mesh_node_unbind(node_prefix).await;
        let _ = host.end_session(session).await;
        sessions.lock().unwrap().remove(&session);
    }

    // ── 5. Render and send ─────────────────────────────────────────────────────

    if let Some(text) = render_response(response, config.max_payload_bytes) {
        // send_to_meshtastic_node(node_prefix, text).await;
        info!("← node {node_prefix:02x?}: {text}");
    }
}

fn parse_my_command(text: &str) -> bbs_plugin_api::command::Command {
    // Delegate to bbs-mesh's parser if you want identical command syntax,
    // or implement your own. The Command enum is defined in bbs-plugin-api.
    bbs_plugin_api::command::Command::Unknown { raw: text.to_owned() }
}

fn render_response(response: Response, max_bytes: usize) -> Option<String> {
    let text = match response {
        Response::Text(s)              => s,
        Response::Prompt { text, .. }  => text,
        Response::LoggedIn { user }    => format!("Welcome, {}. Type H.", user.as_str()),
        Response::LoggedOut            => "Goodbye.".into(),
        Response::Error(e)             => format!("Error: {e}"),
        _                              => return None,
    };

    // Truncate to payload limit, preserving UTF-8 boundaries.
    if text.len() <= max_bytes {
        return Some(text);
    }
    let mut end = max_bytes;
    while !text.is_char_boundary(end) { end -= 1; }
    Some(text[..end].trim_end().to_owned())
}
```

---

## 15. Style guide and contribution checklist

### Naming

- Crate: `bbs-<short-name>` (in-tree) or `<org>-bbs-<short-name>` (community)
- Feature flag: `transport-<short-name>` for transports, `plugin-<short-name>` for others
- Config struct: `<NameInPascalCase>Config`
- Transport struct: `<NameInPascalCase>Transport`
- Error enum: `<NameInPascalCase>Error`

### Logging

Use `tracing` macros. Prefix log messages with your transport name:

```rust
info!("meshtastic: connected to device");
warn!("meshtastic: frame too long ({} bytes) — truncating", n);
```

### Before opening a PR

- [ ] `rustup run 1.88 cargo fmt --all --check` passes
- [ ] `rustup run 1.88 cargo test --workspace` passes
- [ ] `rustup run 1.88 cargo clippy --workspace -- -D warnings` passes
- [ ] `rustup run 1.88 cargo doc --workspace --no-deps --all-features` passes
- [ ] Payload size test added for every canned string
- [ ] Config documented in `docs/CONFIG.md`
- [ ] Feature flag documented in root `Cargo.toml` comment
- [ ] `Plugin::name()` returns a stable, unique string (check for conflicts)
- [ ] `end_session` called on every code path that closes a connection
- [ ] `notify()` does not block (use channels, not direct writes)
- [ ] Hardware/network tests gated behind a cargo feature
