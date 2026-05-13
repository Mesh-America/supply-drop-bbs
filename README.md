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
  Other LoRa mesh protocols - [Meshtastic](https://meshtastic.org)
  most notably - are explicitly on the roadmap as sibling transport
  plugins. The BBS-core itself is protocol-agnostic; see
  [ADR-0011](docs/adr/0011-transport-protocol-agnostic-core.md).
- **CLI clients** over a Unix-domain socket, for local administration and
  scripting.
- **An optional admin web UI** (off by default), purely for sysop
  maintenance - not for end-user message reading.

Users - humans on mesh nodes - interact with the BBS over whichever mesh
transport is configured. The web UI exists so the sysop can keep the
system healthy without having to drive everything through mesh DMs.

## Why a rewrite

Supply Drop BBS is the spiritual successor to
[`mesh-citadel`](https://github.com/taedryn/mesh-citadel), a Python
implementation of the same idea. The Python project taught us a lot about
what a mesh BBS actually needs to do; this one starts fresh with those
lessons baked into the architecture from the first commit. There is no
shared code, no shared schema, and no migration path between the two -
this is a clean break, not a port.

Specifically, this project bakes in from day 1:

- A real concurrency model (connection pool, not single-thread aiosqlite)
- WAL-mode SQLite tuned for SD-card durability
- A pluggable transport-engine API that all I/O goes through
- The web admin UI as a *plugin* against that API, not a first-class
  citizen - the same API any third-party UI or extension would use
- Compile-time-checked SQL via `sqlx`
- Logging that respects config (no silent `--debug` overrides)
- Audit-logged sysop actions
- Single static binary, single TOML config file

## Installation

Pre-built packages and binaries for Raspberry Pi (aarch64, armv7) and x86-64
Linux are attached to each
[GitHub Release](https://github.com/Mesh-America/supply-drop-bbs/releases).

### Option 1 — Debian package (recommended)

The `.deb` is the easiest way to install on Raspberry Pi OS, Ubuntu, or any
Debian-based system. It handles user creation, directory layout, and systemd
service registration automatically.

Run this on your Pi or Linux box — it auto-detects your architecture:

```sh
ARCH=$(dpkg --print-architecture)   # arm64, armhf, or amd64
curl -fsSL \
  "https://github.com/Mesh-America/supply-drop-bbs/releases/latest/download/supply-drop-bbs_${ARCH}.deb" \
  -o supply-drop-bbs.deb
sudo dpkg -i supply-drop-bbs.deb
sudo supply-drop-bbs setup
sudo systemctl start supply-drop-bbs
```

Or download manually from the [latest release](https://github.com/Mesh-America/supply-drop-bbs/releases/latest):

| Hardware | File |
|---|---|
| Raspberry Pi 4/5 (64-bit) | `supply-drop-bbs_arm64.deb` |
| Raspberry Pi 2/3/Zero 2 (32-bit) | `supply-drop-bbs_armhf.deb` |
| x86-64 Linux | `supply-drop-bbs_amd64.deb` |

### Option 2 — Raw binary

Download the binary directly and verify the checksum before running it.

```sh
# Example for Raspberry Pi 4 (arm64):
TAG=v0.6.0   # replace with the latest release tag
curl -fsSL "https://github.com/Mesh-America/supply-drop-bbs/releases/download/${TAG}/supply-drop-bbs-${TAG}-aarch64-unknown-linux-gnu" \
     -o supply-drop-bbs
curl -fsSL "https://github.com/Mesh-America/supply-drop-bbs/releases/download/${TAG}/SHA256SUMS" \
     -o SHA256SUMS
grep "supply-drop-bbs-${TAG}-aarch64-unknown-linux-gnu" SHA256SUMS | sha256sum -c
sudo install -m 755 supply-drop-bbs /usr/local/bin/supply-drop-bbs
```

Other available targets: `armv7-unknown-linux-gnueabihf` (armhf),
`x86_64-unknown-linux-gnu` (amd64). Append `-headless` for a smaller build
without the admin web UI.

### Option 3 — Guided setup script

If you prefer a wizard that handles everything (including optional
[pymc-companion](https://github.com/Mesh-America/pymc-companion) HAT
configuration), download and review the script first, then run it:

```sh
curl -fsSL https://raw.githubusercontent.com/Mesh-America/supply-drop-bbs/main/install.sh \
     -o install.sh
less install.sh    # read it before running
sudo bash install.sh
```

---

See [`docs/OPERATIONS.md`](docs/OPERATIONS.md) for full configuration
reference and upgrade instructions.

## License

Supply Drop BBS is licensed under the **Apache License 2.0** with the
**Commons Clause** restriction appended. See [LICENSE](LICENSE) for the
full text.

**This is not OSI-approved open source.** The Commons Clause specifically
prohibits selling the software (including selling hosted services derived
from it). All other rights granted by Apache 2.0 - use, modify, fork,
redistribute for non-commercial or internal commercial purposes that don't
constitute resale - remain in effect.

If you want to use Supply Drop BBS for a service whose value derives from
its functionality and you intend to charge for that service, contact the
licensor for a separate commercial arrangement.

## Documentation

Architecture, API, configuration, and operations documentation lives under
[`docs/`](docs/). As the codebase grows, per-crate `cargo doc` becomes the
canonical reference for the plugin API.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Note that contributing implies a
license grant to the project - read the document before sending a PR.

## Security

Found a vulnerability? See [SECURITY.md](SECURITY.md) for the disclosure
process.

## Code of conduct

See [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md). Short version: be excellent
to each other or be elsewhere.
