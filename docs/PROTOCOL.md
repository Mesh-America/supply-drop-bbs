# Protocol notes

Two protocols are documented here:

1. **Companion-frame** - the binary TCP protocol between Supply
   Drop BBS and the radio bridge process (`pymc_core`'s
   `CompanionFrameServer`)
2. **BBS-over-mesh** - the application-layer command vocabulary
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
speaks the MeshCore "companion" wire protocol - the same protocol
a USB or serial-attached MeshCore companion device speaks. This
abstracts the radio: the BBS doesn't care if the bridge is on a
local Pi, a remote host, or someday a Rust daemon talking to an
SX1262.

### Transport

- TCP, default `127.0.0.1:5000` (configurable on both sides).
- One persistent connection per BBS process.
- No TLS at this layer - the protocol is binary, not text-based,
  and is expected to run on loopback. Operators wanting to run
  the bridge on a different host should use ssh tunnelling or
  WireGuard.

### Framing

**TBD** - exact framing inherited from MeshCore companion protocol.
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

- **Identity / handshake** - establish session, negotiate
  capabilities, sync time
- **Contact management** - list known nodes, add/remove contacts,
  query by node ID
- **Outgoing message** - send a packet (DM or flood) to a contact
  or channel
- **Incoming message** - receive a packet (delivered to the
  application; we forward as a domain event)
- **Advert** - announce / heard-from-node events
- **Status** - radio state, signal strength, battery (for nodes
  that report it)
- **Channel ops** - encrypted channel join/leave/list

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

- **TCP-level errors** (connection refused, reset, timeout) -
  trigger reconnection
- **Frame-decode errors** (malformed length, unknown type, payload
  too short) - log + close connection + reconnect. Persistent
  decode errors after a reconnect indicate a protocol-version
  mismatch, which we surface as a fatal `meshcore-companion`
  error to the operator.
- **Application-level errors** (radio busy, contact unknown, send
  failed) - surfaced as `MeshTransportError` variants the
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
- **Integration tests** against a `MockBridgeServer` - a Rust test
  harness that imitates the bridge well enough for the BBS to
  exercise its mesh transport without actual radio hardware.
- **End-to-end tests** against a real `pymc_core` instance - gated
  behind a `--features integration-tests-with-bridge` cargo flag,
  not run in default CI.

## Part 2: BBS-over-mesh

### Purpose

A mesh user with a MeshCore client sends DMs to the BBS's mesh
node. The BBS interprets those DMs as **commands** and replies
with one or more DMs containing the response. This is where the
"BBS personality" lives - what commands users can issue, what
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
implemented. **TBD** - argument grammars, response formats, error
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
       1) alice - "I am building mesh repeaters in the PNW."
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
"you can't do that" message - no information leak about the action
that would have happened.

## Protected contacts

Both transports' radios (the MeshCore companion device and a Meshtastic
radio) keep their own bounded, on-device contact/node table. When that table
fills, the firmware evicts entries to make room for new ones — which is fine
for a hobbyist client but a problem for the BBS: if the entry the BBS itself
needs to reply to a user (or the entry a returning user needs to reach the
BBS) gets evicted at the wrong moment, replies silently fail to route. The
BBS mitigates this by marking a bounded set of contacts **protected**
("favorited" on the radio), which most firmware treats as ineligible for its
own eviction sweeps — trading a small amount of on-device favourite-slots for
reliable delivery to the users who actually talk to the BBS.

### Eligibility and the protection mechanism

A contact becomes eligible for protection the first time it exchanges a
real DM with the BBS (a chat-type message on MeshCore; a person-role node —
`CLIENT`/`CLIENT_MUTE`/`CLIENT_HIDDEN`/`CLIENT_BASE` — on Meshtastic; a
router/repeater is never protected). On MeshCore this is a full-record
`AddUpdateContact` write with the `is_favourite` flag set; on Meshtastic
it's a `set_favorite_node` admin-message write (protobuf tag `39`). Both
writes go through a deferred-write queue gated on a live session/passkey,
not sent inline with the reply — a failed or reverted write never blocks
the user's actual message from being answered.

### Capacity and eviction

Each transport has its own `protected_contact_cap` config key
(`[plugins.mesh]`/`[plugins.meshtastic]`, see [CONFIG.md](CONFIG.md)) —
MeshCore defaults to `350`, Meshtastic to `100`; the two are intentionally
different, tracking real device capacity rather than tracking each other.
`0` disables protection entirely on that transport. Once the cap is reached,
a newly-eligible sender's first DM evicts the **oldest-protected,
currently session-inactive** contact on that transport to make room, rather
than growing the protected set further — the contact with the earliest
`protected_at` timestamp among those with no active session and outside a
brief post-protection grace window. If every protected contact is currently
mid-session (so none is a safe eviction candidate), the new contact is
simply not protected and the BBS logs a distinct warning; nothing crashes
or silently loses state either way.

### MeshCore: saturated contact table

If MeshCore's own contact table is completely full — no further contact,
protected or not, fits — the BBS's own eviction above can't help (there's
no room even for the entry replacing an evicted one), and the transport
logs a clear, distinct warning rather than failing silently. This is a
refusal, not a crash: MeshCore firmware's own node-database code skips
favourited entries when it looks for something to evict but degrades
gracefully when nothing qualifies.

### Meshtastic: saturated node database is a firmware crash risk, not a refusal

Meshtastic's failure mode here is more severe, and the BBS has no way to
detect or warn about it in advance. Meshtastic firmware's node-database
eviction search (`NodeDB::getOrCreateMeshNode`, `NodeDB.cpp`) does not
gracefully refuse when no evictable candidate is found — it falls through
to an out-of-bounds write against a fixed-size vector, which on a typical
embedded build (no C++ exception support) crashes or reboots the radio.
**No BBS-side warning is possible for this case**: by the time the BBS
could observe a saturated table, the device may have already crashed. This
finding rests on a single external firmware-source read, not yet
corroborated by a second independent source — treat it as "verified once,"
not settled fact, and keep `protected_contact_cap` at or below your
device's real `MAX_NUM_NODES` as the only available mitigation.

### Path-replay tradeoff

Protecting a MeshCore contact means writing its full record
(`AddUpdateContact` has no partial/flags-only variant), which necessarily
touches the device's cached routing path for that contact alongside the
favourite flag — there's no option that leaves the path provably untouched.
The BBS replays the contact's own last-known cached path rather than
resetting it to "unknown": firmware source confirms an explicit path reset
has the *more* destructive effect of guaranteeing a full rediscovery flood
on the next send, every time, whereas replaying the cached path is only
occasionally stale (and self-corrects on the next real exchange). Between
"occasionally stale but usually valid" and "always destructive," the BBS
deliberately chooses the former.

### MeshCore prefix-collision limitation

MeshCore contacts are resolved from a DM's 6-byte pubkey prefix, not the
full 32-byte key — a pre-existing wire-protocol characteristic this feature
does not introduce. A prefix collision between two distinct real contacts
was always possible (roughly 2⁻⁴⁸ per pair) and previously caused, at
worst, a transient misrouted session that self-corrected. This feature
attaches a more consequential outcome to the same limitation: since
protecting or deleting a contact is now a full-record radio-side write (not
just a session mixup), a resolved-by-prefix collision could direct that
write at the wrong contact's persistent device state. A passive collision
is still astronomically unlikely; a *deliberate* one is a large but
not-astronomical computation (roughly 2⁴⁸ Ed25519 keypairs to grind) for an
attacker who already knows or can observe a target's 6-byte prefix. This is
a known, accepted, unmitigated-in-code limitation — documented here rather
than silently carried, alongside the Meshtastic firmware-crash risk above.

### MeshCore: self-identification requires `CMD_APP_START` support

The BBS learns its own MeshCore public key exactly once, from the
`SelfInfo` the device returns in response to `CMD_APP_START` at connect
time — nothing else in the wire protocol sets it. On firmware that
responds `ERR_CODE_UNSUPPORTED_CMD` to `CMD_APP_START`, the BBS's own
public key is never learned for the life of that connection.

This matters because both the eviction path (`ContactsFull`) and the
new-protection eviction-exclusion list build their "never evict this"
set from the BBS's own known pubkey. Without it, the BBS's own
self-registered contact entry is not excluded from eviction-candidate
selection — worst case, the BBS could select its own entry as the
`ContactsFull` eviction victim and send `RemoveContact` against itself.

**No clean fix exists today.** The BBS's own contact entry, when it does
end up in `AdvertBus` on unsupported-`CMD_APP_START` firmware (via an
ordinary contact sync or an echoed self-advert), carries no marker
distinguishing it from any other real contact — there is currently no
protocol-level signal that says "this record is me" independent of
`CMD_APP_START`'s `SelfInfo`. The one other command that returns
key material, `ExportPrivateKey` (used on-demand by `node export-key`),
was considered and rejected as an automatic fallback: its firmware
support isn't independently verified either, and routinely invoking it
on every connection just to self-identify would mean materializing the
node's *private* key far more often than necessary, for a security-
sensitive operation currently gated behind an explicit sysop CLI action —
a worse trade than the gap it would close. This is a known, accepted,
unmitigated-in-code limitation on the affected firmware, documented here
rather than silently carried, alongside the two limitations above.

### Discovered Contacts vs. Contacts, and delete semantics

The web admin UI and API distinguish two views over the same underlying
advert data:

- **Discovered Contacts** (`GET /api/v1/adverts`) — every mesh node the BBS
  has ever seen, protected or not.
- **Contacts** (`GET /api/v1/contacts`) — only the currently-protected
  subset; mirrors the CLI's `contacts list` (see [CLI.md](CLI.md)).

Deleting a contact (`DELETE /api/v1/contacts/:pubkey`, Aide/Sysop only) is
**not a permanent block**. It clears the BBS's own local protection state
immediately, then best-effort asks the radio to remove the matching native
favourite/contact entry (a `remove_favorite_node` admin write on Meshtastic,
a native contact removal on MeshCore) — a failure on that native-removal
side is logged but doesn't fail the request, since the local state change
is what actually matters for the BBS's own eviction bookkeeping. Nothing
prevents the same contact from becoming protected again later if it's still
eligible (e.g. it DMs the BBS again) — this is a delete of the *protection*,
not a permanent deny-list entry. A pubkey that only ever appears in
Discovered Contacts (never protected) has nothing to delete; the endpoint
responds `404`.

## Part 3: Internal command schema

The `Command` and `Response` enums in `bbs-core` are the canonical
internal representation. Both are serialisable (mostly for audit
logging and tests; they don't cross a wire boundary in normal
operation since plugins are in-process).

**TBD** - full enum variants when the implementation lands.

## See also

- `crates/meshcore-companion/` - the Rust client implementation
  (TBD)
- `crates/bbs-mesh/` - the BBS-side mesh transport plugin (TBD)
- `crates/bbs-core/src/command.rs` - internal command/response
  types (TBD)
- [`pymc_core`](https://github.com/meshcore-dev/pymc_core) -
  upstream radio bridge
- [`meshcore_py`](https://github.com/meshcore-dev/meshcore_py) -
  Python reference client of the companion-frame protocol
