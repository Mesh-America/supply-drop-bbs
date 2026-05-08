# Protocol notes

Two protocols are documented here:

1. **Companion-frame** — the binary TCP protocol between Supply
   Drop BBS and the radio bridge process (`pymc_core`'s
   `CompanionFrameServer`)
2. **BBS-over-mesh** — the application-layer command vocabulary
   that mesh users exchange with the BBS

The HTTP / REST API for the web admin is documented separately as
OpenAPI: see [`openapi.json`](openapi.json) (generated from Rust;
committed for diffability).

> **Status:** stub. The companion-frame section captures what we
> know from reading `meshcore_py` and `pymc_core` source; details
> get pinned down precisely when we implement the
> `meshcore-companion` crate. Sections marked **TBD** require
> implementation experience to confirm.

## Part 1: Companion-frame protocol

### Purpose

`pymc_core`'s `CompanionFrameServer` exposes a TCP server that
speaks the MeshCore "companion" wire protocol — the same protocol
a USB or serial-attached MeshCore companion device speaks. This
abstracts the radio: the BBS doesn't care if the bridge is on a
local Pi, a remote host, or someday a Rust daemon talking to an
SX1262.

### Transport

- TCP, default `127.0.0.1:5000` (configurable on both sides).
- One persistent connection per BBS process.
- No TLS at this layer — the protocol is binary, not text-based,
  and is expected to run on loopback. Operators wanting to run
  the bridge on a different host should use ssh tunnelling or
  WireGuard.

### Framing

**TBD** — exact framing inherited from MeshCore companion protocol.
Working hypothesis from `pymc_core` source:

- Each frame is length-prefixed
- A single-byte frame type identifier
- A typed payload depending on the frame type
- No end-of-frame marker; framing is purely length-driven

When `meshcore-companion` lands, this section gets concrete: byte
diagrams, the exhaustive frame-type enum, and worked decode/encode
examples.

### Frame types

The MeshCore companion protocol defines roughly:

- **Identity / handshake** — establish session, negotiate
  capabilities, sync time
- **Contact management** — list known nodes, add/remove contacts,
  query by node ID
- **Outgoing message** — send a packet (DM or flood) to a contact
  or channel
- **Incoming message** — receive a packet (delivered to the
  application; we forward as a domain event)
- **Advert** — announce / heard-from-node events
- **Status** — radio state, signal strength, battery (for nodes
  that report it)
- **Channel ops** — encrypted channel join/leave/list

**TBD** for each: full payload schema and semantics. Reference
implementation: `meshcore_py` (Python client of the same protocol).

### State machine

The BBS-side mesh transport keeps:

- A connection state: `disconnected → connecting → handshaking →
  ready → disconnected → ...`
- A contact cache, mirroring what the bridge knows
- A pending-messages queue with retry semantics for outgoing DMs
- Per-session bindings of mesh node IDs to BBS sessions

Reconnection is automatic with exponential backoff per
`reconnect_delay_ms` / `max_reconnect_delay_ms` config keys. While
disconnected, outgoing messages queue up to a configurable limit;
beyond that, oldest are dropped with a WARN log.

### Errors

The companion-frame protocol surface produces:

- **TCP-level errors** (connection refused, reset, timeout) —
  trigger reconnection
- **Frame-decode errors** (malformed length, unknown type, payload
  too short) — log + close connection + reconnect. Persistent
  decode errors after a reconnect indicate a protocol-version
  mismatch, which we surface as a fatal `meshcore-companion`
  error to the operator.
- **Application-level errors** (radio busy, contact unknown, send
  failed) — surfaced as `MeshTransportError` variants the
  transport plugin maps to user-visible responses.

### Versioning

`pymc_core` versions it ships, and so does the companion protocol
itself. Our `meshcore-companion` crate pins a version range it
supports and refuses to talk to a bridge outside that range. The
range is documented in the crate's README and in the BBS's
`/health` output.

### Testing

- **Unit tests** of the frame decoder/encoder against known-good
  hex captures. Captures live in `crates/meshcore-companion/tests/fixtures/`.
- **Property tests** (`proptest`) of `decode(encode(frame)) == frame`
  for every frame type.
- **Fuzz tests** (`cargo fuzz`) of the decoder. This is one of the
  highest-priority fuzz targets because untrusted bytes from the
  network reach our parser here.
- **Integration tests** against a `MockBridgeServer` — a Rust test
  harness that imitates the bridge well enough for the BBS to
  exercise its mesh transport without actual radio hardware.
- **End-to-end tests** against a real `pymc_core` instance — gated
  behind a `--features integration-tests-with-bridge` cargo flag,
  not run in default CI.

## Part 2: BBS-over-mesh

### Purpose

A mesh user with a MeshCore client sends DMs to the BBS's mesh
node. The BBS interprets those DMs as **commands** and replies
with one or more DMs containing the response. This is where the
"BBS personality" lives — what commands users can issue, what
the BBS sends back, how state is maintained per-user.

### Conventions

- Commands are short. Mesh packets are bandwidth-constrained.
- Commands are line-based: one command per DM, terminated by
  newline or end-of-message.
- Responses may span multiple DMs. Long output is paginated.
- Case-insensitive command names. Arguments preserve case where
  meaningful (room names, message bodies).
- A configurable command prefix (`[plugins.mesh] command_prefix`)
  may be required. Default empty: any DM to the BBS is a command.
  Set to `"/"` to require `/help`, `/read`, etc.

### The command surface

**Status:** TBD. The full command vocabulary is designed alongside
the `bbs-core::Command` enum. Below is the working v1 proposal,
subject to revision.

Each command's name, argument shape, required permission level,
and response format will be tabulated here. For now, a sketch:

| Command           | Permission        | Description                                  |
|-------------------|-------------------|----------------------------------------------|
| `help [topic]`    | any               | Show available commands or help for a topic  |
| `register <name>` | unauthenticated   | Begin the registration workflow              |
| `login <name>`    | unauthenticated   | Begin the login workflow                     |
| `logout`          | logged in         | End session                                  |
| `whoami`          | logged in         | Show current identity + session info         |
| `rooms`           | logged in         | List rooms                                   |
| `room <name>`     | logged in         | Switch to a room                             |
| `read [n]`        | logged in         | Read up to n recent unread messages          |
| `read next`       | logged in         | Read next batch                              |
| `post <text>`     | logged in         | Post a public message in current room        |
| `dm <user> <text>`| logged in         | Send a DM                                    |
| `mail`            | logged in         | List pending DMs                             |
| `who`             | logged in         | Active sessions / recently-online users      |
| `block <user>`    | logged in         | Block another user                           |
| `unblock <user>`  | logged in         | Unblock another user                         |
| `valid <user>`    | sysop / aide      | Approve a pending validation                 |
| `deny <user>`     | sysop / aide      | Reject a pending validation                  |
| `kick <user>`     | sysop             | End user's sessions                          |

This list will be made authoritative when the command processor is
implemented. **TBD** — argument grammars, response formats, error
codes per command, paging details.

### Workflows over mesh

Multi-step interactions (registration, login challenge, sysop-led
validation) use the workflow state machine. The user sees a series
of prompts, each answered by sending a DM with the response. The
workflow's persistent state (`workflow_state` table) survives BBS
restarts mid-flow.

Examples (informal):

**Registration:**

```
USER: register alice
BBS:  Welcome alice. Please answer the following:
      1) Why do you want to join? (one sentence)
USER: I am building mesh repeaters in the PNW.
BBS:  Thanks. Choose a recovery question:
      a) What is your favourite color?
      b) What was your first pet's name?
      c) Who was your favourite teacher?
USER: a
BBS:  Answer to "What is your favourite color?":
USER: blue
BBS:  Set a password (8-64 characters):
USER: hunter2hunter2
BBS:  Confirm password:
USER: hunter2hunter2
BBS:  Registration submitted. Awaiting sysop validation.
      You will be notified when validated.
```

**Sysop validation flow:**

```
SYSOP: pending
BBS:   Pending validations:
       1) alice — "I am building mesh repeaters in the PNW."
SYSOP: valid alice
BBS:   alice is now validated. They have been notified.
```

The exact wording, prompts, and pagination are **TBD** as the
implementation lands.

### Notifications

The BBS pushes unsolicited DMs to logged-in mesh users for:

- New mail arrived
- A user posted in a room they're subscribed to (future feature)
- Validation approved or denied
- Sysop announcements (System room post)

Push delivery uses the mesh transport's `notify` method. The
transport queues notifications when the user is offline and
delivers when they come back online (subject to retention limits
in config).

### Errors and limits

Per-user rate limit (default 60 commands/min, configurable).
Unknown commands respond with the help topic. Authentication
failures lock further attempts for a brief cooldown. Authorisation
failures (insufficient permission level) respond with a clear
"you can't do that" message — no information leak about the action
that would have happened.

## Part 3: Internal command schema

The `Command` and `Response` enums in `bbs-core` are the canonical
internal representation. Both are serialisable (mostly for audit
logging and tests; they don't cross a wire boundary in normal
operation since plugins are in-process).

**TBD** — full enum variants when the implementation lands.

## See also

- `crates/meshcore-companion/` — the Rust client implementation
  (TBD)
- `crates/bbs-mesh/` — the BBS-side mesh transport plugin (TBD)
- `crates/bbs-core/src/command.rs` — internal command/response
  types (TBD)
- [`pymc_core`](https://github.com/meshcore-dev/pymc_core) —
  upstream radio bridge
- [`meshcore_py`](https://github.com/meshcore-dev/meshcore_py) —
  Python reference client of the companion-frame protocol
