# ADR-0004: Cargo features for plugin selection (not runtime loading)

- **Status:** Accepted
- **Date:** 2026-05-08
- **Deciders:** Mesh-America

## Context

Plugins need to be discovered and loaded somehow. The candidates:

1. **Compile-time, cargo features.** Plugins are crates linked at
   build time. Disabled plugins aren't compiled in.
2. **Runtime, native dynamic libraries.** Plugins are `.so`/`.dll`
   files loaded with `libloading` at startup. Host and plugins
   communicate through a defined ABI.
3. **Runtime, WASM.** Plugins are `.wasm` files loaded into a
   wasmtime / extism / wasmer runtime. Sandboxed. ABI defined by
   a host-provided interface.

## Decision

For v1, **cargo features (option 1).**

WASM (option 3) is on the roadmap as a future enhancement (see
[ARCHITECTURE.md §13](../ARCHITECTURE.md#13-future--deferred)).

Native dynamic libraries (option 2) are **rejected entirely** and
won't be revisited.

## Rationale

### Why cargo features for v1

- **Simplest thing that works.** A plugin is a workspace crate that
  exposes an `Plugin`-implementing struct. Adding a plugin is
  adding a crate and a feature flag. Removing one is the inverse.
- **No ABI design work.** The `Plugin` trait can evolve freely
  because plugin authors rebuild against each version of the host
  anyway.
- **Type safety end-to-end.** The host imports the plugin's types
  directly. No serialisation across an ABI boundary.
- **No performance cost.** Plugin calls are direct method calls,
  not FFI.
- **Operators don't need a Rust toolchain.** We ship pre-built
  binaries with feature combinations. Most operators never
  recompile.

### Why not native dynamic libraries

- **Rust ABI is unstable.** Plugins must be built with the exact
  same compiler version, target, and feature set as the host.
  In practice operators end up recompiling everything from source,
  which is no better than cargo features and significantly more
  complex.
- **No sandboxing.** A native plugin can do anything the host can.
  Loading an untrusted plugin means trusting it completely. Not
  appropriate for a community-contribution model.
- **Crash isolation is poor.** A plugin segfault crashes the host.

### Why WASM for the future, not now

- **Multi-week design work.** The host-WASM ABI (which BBS API
  surface is exposed to plugins, in what shape, with what
  serialisation) is its own non-trivial project.
- **Plugin authors need to learn a new toolchain.** WASM-targeting
  Rust is straightforward, but other source languages (Go, C++,
  AssemblyScript) have varying levels of WASM-target maturity.
- **Performance overhead.** Function calls across the WASM
  boundary cost ~100ns + serialisation. Acceptable for most
  plugin operations, painful in hot paths.
- **Genuine value when it lands.** Sandboxed plugins, language-
  agnostic, no recompilation required — those are real wins.
  Worth doing, but not at the cost of v1 timeline.

## Consequences

### Positive

- Plugin API can evolve quickly because there's no stable ABI
  contract to honour
- All errors in plugin loading are compile-time errors
- No supply-chain question — plugins ship as source, are reviewed
  during merge to the workspace, and are compiled by the same CI
  that builds the host
- Operators get pre-built binaries; no toolchain on the Pi

### Negative

- **Adding a plugin requires forking the workspace** (or
  submitting a PR upstream). Not "drop a `.so` in a directory
  and restart."
- **No third-party closed-source plugins.** The plugin source
  must be in the workspace.
- **CI builds grow** as plugins are added (each feature
  combination is a separate build artifact).

### Neutral

- The decision is reversible. If WASM plugins land in a future
  version, cargo-feature plugins can keep working alongside them.
  The `bbs-plugin-api` trait is the same; only the loading
  mechanism differs.

## Build matrix

CI ships these binary variants per architecture:

| Variant                      | Features                                |
|------------------------------|-----------------------------------------|
| `supply-drop-bbs`            | `transport-cli, transport-mesh` (default) |
| `supply-drop-bbs-web`        | default + `admin-web`                   |
| `supply-drop-bbs-headless`   | `transport-cli` only (development)      |

Architectures: `aarch64-unknown-linux-gnu` (Pi 4/5),
`armv7-unknown-linux-gnueabihf` (older Pi),
`x86_64-unknown-linux-gnu` (general Linux).

Operators with unusual needs (custom plugin combinations, novel
architectures) build from source with `cargo build --release
--features <list>`.
