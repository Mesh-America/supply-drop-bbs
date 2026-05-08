# ADR-0007: Pin pymc_core CompanionFrameServer as the radio bridge for v1

- **Status:** Accepted
- **Date:** 2026-05-08
- **Deciders:** Mesh-America

## Context

The radio-bridge process (see [ADR-0002](0002-process-model.md))
needs an implementation. Two choices for v1:

1. **Use the existing Python `pymc_core` `CompanionFrameServer`.**
   This is what mesh-citadel uses. It's mature, debugged against
   real radios, and supports the SX1262 plus several other
   MeshCore-compatible HATs.

2. **Write a Rust-native bridge.** Reimplements the
   companion-frame protocol's server side and the SX1262 driver
   in Rust. Eliminates Python from the deployment.

## Decision

**For v1: use `pymc_core`'s CompanionFrameServer.**

A Rust-native bridge is on the long-term roadmap (see
[ARCHITECTURE.md §13](../ARCHITECTURE.md#13-future--deferred)) but
explicitly out of scope for the first release.

## Rationale

### Why not write our own bridge for v1

A Rust-native bridge would mean:

- **Reimplementing the SX1262 driver** in Rust. Some crates exist
  (`sx126x-rs`, others) but none match `pymc_core`'s
  battle-tested integration with the MeshCore protocol.
- **Reimplementing the companion-frame server side.** We're
  already implementing the *client* side in `meshcore-companion`;
  the server side is meaningfully more complex (it manages the
  radio's contact cache, handles ack flooding, etc.).
- **Reimplementing the MeshCore protocol logic** that's
  abstracted by `pymc_core`'s `CompanionFrameServer`. Flooding,
  contact discovery, advert handling, message reassembly,
  encrypted channels.

That's months of work, against an upstream protocol that still
evolves. The risk of subtle protocol incompatibility (a malformed
frame that confuses real-world MeshCore nodes in a way our test
harness doesn't catch) is real and hard to debug in the field.

### Why a Rust bridge is appealing eventually

- **Single language stack.** A Rust-only deployment means no
  Python interpreter, no `pip`, no virtualenv. Cleaner ops story.
- **Single-binary deployment** if the bridge is statically linked.
  `wget` + run, full stop.
- **Performance.** Probably negligible at hobbyist scale, but
  Rust is consistently lower-overhead than Python.
- **Memory.** Pi RAM is precious. A Python bridge takes ~30 MB
  baseline; a Rust one would be a few MB.

These are real wins. They're not v1 wins.

## Consequences

### Positive

- **Time to v1 cut by months.** We don't reimplement the radio
  side at all.
- **Battle-tested code at the radio boundary.** Whatever the
  current state of `pymc_core`, it's been deployed against real
  radios in real mesh networks for years. Our Rust client just
  has to talk to it correctly.
- **Independent upstream.** Bug fixes in the radio side land in
  `pymc_core` without us shipping a Supply Drop release.
- **Multiple radio HATs supported** for free (whatever
  `pymc_core` supports).

### Negative

- **Python in the deployment supply chain.** Operators must
  install Python, `pymc_core`, and any system packages it needs.
  We document this in `OPERATIONS.md` but it's not a
  "single-binary" story.
- **Two systemd units to manage.** Mitigated with documentation
  and an example `supply-drop-bridge.service` unit file in the
  release tarball.
- **Two upgrade paths.** When `pymc_core` ships an update,
  operators have to update it independently. We pin a known-good
  version range in our docs and call out incompatibilities.

### Neutral

- The TCP companion-frame protocol is the boundary. If a
  Rust bridge is ever written (by us or anyone else), the BBS
  code doesn't change — the new bridge speaks the same protocol
  on the same port. Switching is purely an operations decision.

## Future re-evaluation

We will revisit this decision if:

- `pymc_core` becomes unmaintained or its upstream changes
  direction in ways that don't suit us
- A community member writes a Rust bridge that we can adopt
- The single-binary deployment story becomes important enough
  to warrant the engineering investment
- The protocol stabilises sufficiently that the
  reimplementation risk drops materially

Until one of those happens, `pymc_core` it is.

## Notes for operators

The bridge process is **not part of this project's source tree.**
It's an upstream dependency. To run a complete deployment:

1. Install `pymc_core` per its own documentation.
2. Configure the `CompanionFrameServer` to bind to a TCP port
   (default in our docs: `127.0.0.1:5000`).
3. Run it under its own systemd unit (we provide an example).
4. Run Supply Drop BBS, configured to connect to the same port.

The two processes are loosely coupled. Either can restart without
breaking the other; the mesh transport handles bridge
disconnection cleanly.
