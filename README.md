<p align="center">
  <img src=".github/supply-drop-icon.svg" width="96" height="96" alt="Supply Drop BBS logo" />
</p>

<h1 align="center">Supply Drop BBS</h1>

<p align="center">
  A bulletin-board system for LoRa mesh networks, built in Rust.<br/>
  <a href="https://github.com/Mesh-America/supply-drop-bbs/actions/workflows/ci.yml">
    <img src="https://github.com/Mesh-America/supply-drop-bbs/actions/workflows/ci.yml/badge.svg" alt="CI" />
  </a>
  &nbsp;
  <a href="https://github.com/Mesh-America/supply-drop-bbs/releases/latest">
    <img src="https://img.shields.io/github/v/release/Mesh-America/supply-drop-bbs?color=3a8ad8" alt="Latest release" />
  </a>
  &nbsp;
  <a href="LICENSE">
    <img src="https://img.shields.io/badge/license-Apache--2.0%20%2B%20Commons%20Clause-lightgrey" alt="License" />
  </a>
</p>

---

## What it is

Supply Drop BBS is the BBS half of a mesh-radio operator's stack. It speaks
to:

- **Mesh radios**, via a pluggable transport architecture. v1 supports
  [MeshCore](https://meshcore.dev) (through
  [`pymc_core`](https://github.com/meshcore-dev/pymc_core)'s
  CompanionFrameServer running as a separate radio-bridge process).
  Other LoRa mesh protocols — [Meshtastic](https://meshtastic.org)
  most notably — are explicitly on the roadmap as sibling transport
  plugins. The BBS-core itself is protocol-agnostic; see
  [ADR-0011](docs/adr/0011-transport-protocol-agnostic-core.md).
- **CLI clients** over a Unix-domain socket, for local administration and
  scripting.
- **An optional admin web UI** (off by default), purely for sysop
  maintenance — not for end-user message reading.

Users — humans on mesh nodes — interact with the BBS over whichever mesh
transport is configured. The web UI exists so the sysop can keep the
system healthy without having to drive everything through mesh DMs.

## Why a rewrite

Supply Drop BBS is the spiritual successor to
[`mesh-citadel`](https://github.com/taedryn/mesh-citadel), a Python
implementation of the same idea. The Python project taught us a lot about
what a mesh BBS actually needs to do; this one starts fresh with those
lessons baked into the architecture from the first commit. There is no
shared code, no shared schema, and no migration path between the two —
this is a clean break, not a port.

Specifically, this project bakes in from day 1:

- A real concurrency model (connection pool, not single-thread aiosqlite)
- WAL-mode SQLite tuned for SD-card durability
- A pluggable transport-engine API that all I/O goes through
- The web admin UI as a *plugin* against that API, not a first-class
  citizen — the same API any third-party UI or extension would use
- Compile-time-checked SQL via `sqlx`
- Logging that respects config (no silent `--debug` overrides)
- Audit-logged sysop actions
- Single static binary, single TOML config file

## Installation

Pre-built binaries for Raspberry Pi (aarch64, armv7) and x86-64 Linux are
attached to each [GitHub Release](https://github.com/Mesh-America/supply-drop-bbs/releases).

For a one-command Pi setup:

```sh
curl -fsSL https://raw.githubusercontent.com/Mesh-America/supply-drop-bbs/main/install.sh | bash
```

See [`docs/OPERATIONS.md`](docs/OPERATIONS.md) for full installation and
configuration instructions.

## License

Supply Drop BBS is licensed under the **Apache License 2.0** with the
**Commons Clause** restriction appended. See [LICENSE](LICENSE) for the
full text.

**This is not OSI-approved open source.** The Commons Clause specifically
prohibits selling the software (including selling hosted services derived
from it). All other rights granted by Apache 2.0 — use, modify, fork,
redistribute for non-commercial or internal commercial purposes that don't
constitute resale — remain in effect.

If you want to use Supply Drop BBS for a service whose value derives from
its functionality and you intend to charge for that service, contact the
licensor for a separate commercial arrangement.

## Documentation

Architecture, API, configuration, and operations documentation lives under
[`docs/`](docs/). As the codebase grows, per-crate `cargo doc` becomes the
canonical reference for the plugin API.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Note that contributing implies a
license grant to the project — read the document before sending a PR.

## Security

Found a vulnerability? See [SECURITY.md](SECURITY.md) for the disclosure
process.

## Code of conduct

See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Short version: be excellent
to each other or be elsewhere.
