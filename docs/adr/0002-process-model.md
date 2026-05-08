# ADR-0002: BBS-host + radio-bridge as separate processes

- **Status:** Accepted
- **Date:** 2026-05-08
- **Deciders:** Taedryn

## Context

The BBS needs to talk to a LoRa radio (SX1262 over SPI on a Pi).
Three architectures are possible:

1. **Single process.** The BBS owns the radio directly via SPI/GPIO.
2. **Two processes, our binary on both sides.** A Rust radio-bridge
   process talks to the SX1262; the BBS talks to the bridge.
3. **Two processes, third-party bridge.** An existing Python project
   (`pymc_core`'s `CompanionFrameServer`) owns the radio; our
   single Rust BBS process talks to that bridge over TCP.

The mesh-citadel codebase used option 1 with the radio integrated
in-process via `meshcore_py`. We learned several things:

- Radio I/O has its own threading model (SX1262 IRQ → callback →
  asyncio dispatch). Embedding that in the BBS process meant the
  BBS had to deal with cross-thread / cross-runtime hand-offs in
  multiple places.
- Failures in the radio code (driver hangs, contact-cache desync)
  cascaded into the BBS process. A wedged radio meant a wedged BBS.
- Restarting the BBS (e.g., to apply a config change) tore down
  the radio and disrupted in-flight mesh exchanges.
- Updating the radio library required restarting the BBS.

## Decision

For v1, **two processes, third-party bridge.** We run `pymc_core`'s
`CompanionFrameServer` as a separate Python process. The Rust BBS
connects to it over TCP using the companion-frame protocol. The BBS
process never touches the radio directly.

Future re-evaluation: a Rust-native bridge is plausible (see
[ADR-0007](0007-bridge-stays-pymc-core.md)) but explicitly deferred.

## Consequences

### Positive

- **Failure isolation.** Radio driver hangs no longer cascade into
  the BBS. The BBS's mesh transport sees a TCP disconnect and
  retries; nothing else breaks.
- **Independent restart.** Update the BBS without touching the radio,
  or vice versa. In-flight mesh exchanges survive a BBS restart.
- **No language runtime in the BBS.** The Rust BBS is a single
  static binary regardless of what the bridge is implemented in.
- **Smaller surface area.** The Rust codebase doesn't include
  hardware-specific drivers, IRQ-handling logic, or the
  companion-frame protocol's server side. Less to test, less to
  break.
- **Deployment flexibility.** Operators can run the bridge on a
  different host than the BBS — useful when the radio is on one
  Pi (with the antenna) and the BBS is on another (with more
  storage / RAM).

### Negative

- **Two systemd units to manage.** Operator complexity is slightly
  higher. We mitigate with documentation and example unit files.
- **Python in the deployment supply chain.** A clean "Rust only"
  story is appealing, and we don't get it in v1. (Future ADR may
  revisit.)
- **TCP overhead.** Negligible in practice; companion-frame frames
  are small and infrequent compared to the radio's actual capacity.
- **TCP failure modes.** The mesh transport must handle bridge
  reconnect cleanly. We test this explicitly.

### Mitigations

- Ship example systemd units for both processes, with a
  `Wants=` / `After=` ordering relationship so the BBS waits for
  the bridge to be ready.
- Document the TCP listen address / port in both projects'
  configs to keep them in sync.
- Health-check the bridge connection; expose its state via
  `/metrics` and the web admin's system-status panel.

## Future considerations

If we ever ship a Rust-native radio bridge, the BBS code doesn't
change — the new bridge speaks the same companion-frame protocol
on the same TCP port. The decision to switch bridges is purely an
operations concern, not an architecture concern. That's the value
of the protocol-level boundary.
