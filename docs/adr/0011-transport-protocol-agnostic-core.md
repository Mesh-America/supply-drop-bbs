# ADR-0011: Transport-protocol agnostic core

- **Status:** Accepted
- **Date:** 2026-05-08
- **Deciders:** Mesh-America

## Context

v1 ships with a single mesh transport: MeshCore, via
`pymc_core`'s CompanionFrameServer. The architecture is built
around a `TransportEngine` plugin contract that allows other
transports to be added later - and one of those is almost
certainly going to be Meshtastic, which has a substantially
larger user base than MeshCore today.

Other plausible future transports:

- **Meshtastic** - protobuf over USB / BLE / TCP, no bridge
  process needed (the device's firmware speaks the protocol
  directly to the host)
- **Reticulum / RNS** - a different mesh-network design altogether
- **Custom packet radio** - AX.25 over an analog HT
- **Telnet / TCP raw** - for network-attached operators
- **IRC bridge** - for participating in IRC channels as a
  pseudo-user
- **Matrix bridge** - same idea via Matrix
- **Gemini protocol** - for Gemini-served BBS clients

Each of these is a transport plugin. The architecture already
permits this - the `TransportEngine` trait abstracts away protocol
specifics. But "permits" isn't enough. Without an explicit
discipline, contributors will accidentally bake protocol-specific
assumptions into the core ("oh just add a `mesh_node_id` to
`User`") and the transport-agnosticism erodes from inside.

This ADR locks the discipline.

## Decision

The BBS-core (`bbs-core` crate) is **transport-protocol agnostic.**
This is enforced by the following rules, which apply to every PR
that touches `bbs-core` or `bbs-plugin-api`.

### Rule 1: No protocol-specific fields on core domain types

`User`, `Room`, `Message`, `Session`, `Workflow`, and any other
type in `bbs-core` cannot carry fields like `mesh_node_id`,
`meshtastic_node_id`, `irc_nick`, or `telnet_address`. Such
identifiers are **transport-specific** and live in
**transport-specific tables** owned by the relevant plugin.

### Rule 2: Identity mapping is per-transport

Each transport plugin owns the mapping between its protocol's
identifiers and BBS users.

- The MeshCore plugin maintains its own table mapping MeshCore
  public keys ↔ BBS usernames.
- A Meshtastic plugin would maintain its own table mapping
  Meshtastic node IDs ↔ BBS usernames.
- An IRC bridge plugin would map IRC nicks ↔ BBS usernames.

These tables are not shared. A user who registers via MeshCore is
a `User` in `bbs-core` with a username; the MeshCore plugin
remembers that this user's MeshCore identity is X. If the same
human later wants to log in via Meshtastic, they go through the
Meshtastic plugin's account-linking flow, which records the
Meshtastic identity in *its* table - but the underlying `User`
is the same row.

### Rule 3: Sessions identify by `(transport_name, opaque_id)`

A session in `bbs-core` carries `transport_name: &'static str`
and `transport_session_id: SessionId`. The core treats
`transport_session_id` as opaque. Whether the transport encodes
something meaningful into that ID is the transport's business.

### Rule 4: Commands and responses are protocol-neutral

The `Command` and `Response` enums in `bbs-core` cannot have
variants like `MeshCorePostFlood` or `MeshtasticTelemetry`. If a
feature requires protocol-specific knowledge - e.g., "show signal
strength" - the relevant data is exposed through a *capability
extension* that only the transports supporting it implement, not
through a core variant that's null on every other transport.

### Rule 5: Notifications are typed at the abstract level

A `Notification` is an opaque payload to deliver to a session.
How it gets to the user (DM packet over MeshCore, protobuf
message over Meshtastic, write to a Unix socket for CLI, HTTP
push for web) is the transport's concern. The core says
"deliver this text to session X"; the transport decides how.

### Rule 6: Transport-specific config under `[plugins.<name>]`

Per-protocol configuration lives in the plugin's own config
section, not in `bbs-core` config. MeshCore radio params,
Meshtastic channel PSKs, Telnet line endings - all in their
respective plugin sections. The core's config is concerned with
domain rules (room defaults, permission levels, rate limits),
not transport mechanics.

### Rule 7: Workflow-step protocol customisation is local

If registration over Meshtastic needs to ask different prompts
than registration over MeshCore (e.g., because of packet-size
differences forcing shorter prompts), that customisation lives
in the workflow's transport-specific *renderer*, not in
divergent workflow definitions per transport. The state machine
is the same; how prompts and responses are rendered to bytes
varies per transport.

## Rationale

The cost of protocol-specific drift is paid years later. A
`mesh_node_id` field added to `User` "just for now" becomes load-
bearing when other code starts to read it. By the time someone
wants to add a Meshtastic transport, they discover that core
tables, queries, and types implicitly assume MeshCore semantics -
and untangling it is a months-long refactor.

The mesh-citadel project hit this in microcosm: contact-cache
specifics from `meshcore_py` leaked into transport-engine code
that should have been protocol-neutral, making "could we use this
for something other than MeshCore" a non-trivial question.
Starting clean with a written discipline costs nothing now and
saves a refactor later.

## Examples

### Compliant: storing a MeshCore identity for a user

```rust
// In bbs-mesh (the MeshCore transport plugin)
//
// Migration owned by the plugin, in a separate prefix to avoid
// collision with future plugins:
//
//   CREATE TABLE meshcore_identities (
//     username TEXT NOT NULL REFERENCES users(username) ON DELETE CASCADE,
//     public_key BLOB NOT NULL UNIQUE,
//     first_seen TEXT NOT NULL,
//     PRIMARY KEY (username)
//   );
```

The mapping table lives in the MeshCore plugin's migration set.
`bbs-core` knows nothing about it.

### Compliant: a Meshtastic transport plugin doing the same thing

```rust
// In bbs-meshtastic (hypothetical future plugin)
//
//   CREATE TABLE meshtastic_identities (
//     username TEXT NOT NULL REFERENCES users(username) ON DELETE CASCADE,
//     node_id TEXT NOT NULL UNIQUE,
//     short_name TEXT,
//     long_name TEXT,
//     first_seen TEXT NOT NULL,
//     PRIMARY KEY (username)
//   );
```

Same shape, different plugin, different table. Both reference
the shared `users.username` from `bbs-core`. A single human can
be registered in both tables → same `User`.

### Non-compliant: a `mesh_node_id` field on `User`

```rust
// In bbs-core/src/user.rs (DON'T DO THIS)
pub struct User {
    pub username: String,
    pub display_name: String,
    pub permission_level: PermissionLevel,
    pub status: UserStatus,
    pub mesh_node_id: Option<MeshNodeId>,  // ← Rule 1 violation
}
```

This bakes MeshCore semantics into the core. A code review
should reject this.

### Non-compliant: a transport-specific Command variant

```rust
// In bbs-core/src/command.rs (DON'T DO THIS)
pub enum Command {
    Post(String),
    Read,
    // ...
    MeshCoreFloodAdvert,  // ← Rule 4 violation
}
```

Flood-advert is a MeshCore concept. If the MeshCore plugin needs
to expose a "flood advert" admin command, it does so through its
own command surface (e.g., a separate plugin-specific RPC or a
sysop-only HTTP endpoint contributed by the plugin), not through
`bbs-core::Command`.

## Consequences

### Positive

- **Adding a new transport is a self-contained change.** New
  crate, new migrations under a new prefix, no edits to
  `bbs-core`. Code review is bounded.
- **Each transport's identity model is honoured.** MeshCore
  doesn't pretend to be Meshtastic; vice versa. No
  lowest-common-denominator fight.
- **Multiple transports can coexist** for the same operator.
  Run MeshCore + Meshtastic + CLI + web on the same BBS - each
  is loaded as its own plugin, each owns its own state.
- **Protocol upgrades isolate to the plugin.** If MeshCore
  ships a v2 protocol, only `bbs-mesh` and `meshcore-companion`
  change. Core tables, types, and tests don't move.

### Negative

- **Some duplication across transport plugins.** Every transport
  with a "user identity" concept maintains its own mapping
  table. We accept this - the alternative is shared schema that
  forces protocol coupling.
- **Cross-transport features need explicit design.** "Push a
  notification to user X on whatever transport they're logged
  in on" requires the BBS to track active sessions per user
  and route notifications through the right transport. This
  is in `bbs-core::Host`'s notification routing, but the
  transports themselves don't know about each other.
- **More verbose review checklist.** PRs touching `bbs-core`
  need to be checked against this ADR. The discipline is the
  whole point; that doesn't make it free.

### Neutral

- This ADR doesn't constrain non-mesh transports. CLI, web,
  and any future Telnet / Matrix / IRC bridge plugins follow
  the same rules - they're all transports as far as the
  architecture is concerned. The fact that some carry over
  radio and others over TCP is a transport-internal detail.

## Enforcement

The rules above are checked in code review, not statically by
the type system in every case. We can structurally enforce some
of them:

- **Rule 1** is partially enforceable via crate boundaries:
  `bbs-core::User` doesn't import any transport-plugin types,
  so adding a `MeshNodeId` field would require importing
  `bbs-mesh` from `bbs-core`, which would be an obvious red
  flag in review.
- **Rule 2** is enforceable by convention: each transport
  plugin's migrations live in its own crate's `migrations/`
  directory, with a name prefix matching the plugin name.
- **Rule 4** is enforceable similarly: the `Command` and
  `Response` enums live in `bbs-core` and reviewers reject
  protocol-specific variants there.

We do not write a custom lint for this. The ADR is the
canonical reference; reviewers cite it.

## Future considerations

- **Cross-transport identity proofs.** When a single human is
  on both MeshCore and Meshtastic, how do they prove the same
  identity to both? The current answer is "log in to both and
  link them under the same username via a sysop-mediated step."
  Cryptographic linking (DID-style) is a future possibility,
  not a v1 requirement.
- **Transport capability advertisement.** Plugins might
  advertise capabilities (e.g., "supports rich text," "max
  payload 200 bytes") that the BBS uses to tailor responses.
  Currently this is implicit (each transport handles its own
  rendering). May become explicit if we want richer per-transport
  UX without per-transport command logic.
- **Protocol bridges as plugins.** A "MeshCore ↔ Meshtastic
  message bridge" is itself a transport-shaped object - it
  consumes domain events from one transport and emits commands
  to another. Possible plugin pattern, not v1.
