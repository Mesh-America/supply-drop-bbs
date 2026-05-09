# ADR-0013: Native serial transport for USB companion devices

- **Status:** Accepted
- **Date:** 2026-05-08
- **Deciders:** Mesh-America

## Context

[ADR-0007](0007-bridge-stays-pymc-core.md) established that the radio
bridge for v1 is `pymc_core`'s `CompanionFrameServer`, running as a
separate process and communicating with the BBS over a TCP companion-
frame connection.

That decision addressed Pi HAT deployments, where the radio hardware
requires an SX1262 driver and a companion-frame server to expose it
over the protocol our client (`meshcore-companion`) understands.

A significant second class of deployment exists: **USB-native MeshCore
devices** such as the Heltec Wireless Tracker V3, T-Beam, and other
boards that run companion-frame firmware directly. On these devices the
radio is handled by the firmware on the microcontroller; the host
machine sees a USB serial port that already speaks the companion-frame
protocol. There is no SX1262 driver to install, no Python needed, no
`pymc_core` to run.

Requiring `pymc_core` for USB devices would mean asking operators to
install Python, create a virtualenv, and run a second process - purely
to act as a pass-through relay from serial to TCP. That's unnecessary
friction for a large class of users.

## Decision

**Add a native serial transport mode to `meshcore-companion`** so that
`bbs-mesh` can speak the companion-frame protocol directly over a USB
serial port, bypassing `pymc_core` entirely for USB device setups.

This is implemented as a new connection mode (`connection_type =
"serial"`) in `MeshConfig`. The `meshcore-companion` crate gains a
serial-backed `CompanionClient` variant (alongside the existing TCP
variant) using `tokio-serial`.

`pymc_core` remains **required for Pi HAT deployments only**.

## Why this doesn't contradict ADR-0007

ADR-0007 ruled out writing a Rust-native *radio bridge* - that is,
reimplementing the companion-frame *server* side, the SX1262 driver,
and the MeshCore protocol logic (flooding, advert handling, contact
cache, etc.). That work is still explicitly deferred.

Adding a serial *client* is categorically different:

- We are not implementing the companion-frame server side.
- We are not writing an SX1262 driver.
- We are not reimplementing MeshCore protocol logic.

We are adding a second I/O transport (serial instead of TCP) to the
existing companion-frame *client* (`meshcore-companion`). The protocol
parser, frame encoder, and all protocol logic are unchanged. Only the
byte stream source changes.

ADR-0007's TCP boundary remains valid for HAT deployments. For USB
deployments, the serial port *is* that boundary.

## Connection modes

After this ADR, `bbs-mesh` supports three `connection_type` values:

| Mode     | Description                                               | pymc_core? |
|----------|-----------------------------------------------------------|------------|
| `serial` | Talk companion-frame directly to a USB serial device      | No         |
| `tcp`    | Connect to an external `CompanionFrameServer` over TCP    | Yes (or any server speaking the protocol) |
| `hat`    | `tcp` target is `pymc_core` managing a Pi HAT (documented separately) | Yes |

`hat` is operationally `tcp` with the additional implication that
`pymc_core` is installed as a systemd service on the same host and
manages GPIO/SPI for the LoRa HAT. The distinction exists to help the
setup wizard guide the operator correctly; the `bbs-mesh` transport
layer itself treats `hat` and `tcp` identically.

## Consequences

### Positive

- **USB operators need zero Python.** Single static Rust binary + USB
  cable. The entire supply chain is one `curl | bash` plus a reboot.
- **Simpler systemd story for USB users.** One service unit, not two.
- **No protocol changes.** The companion-frame wire format is
  unchanged. USB devices and TCP servers are interchangeable from the
  BBS's perspective.
- **Consistent code path.** `CompanionClient` hides whether the byte
  stream comes from a TCP socket or a serial port. `bbs-mesh`'s
  `MeshTransport` is unchanged.

### Negative

- **`tokio-serial` dependency.** Adds a crate that does unsafe FFI
  into platform serial APIs. On most Linux systems this is `termios`
  and is well-understood; the risk is low but the dependency exists.
- **Serial quirks on Windows / macOS.** Not a target platform for v1,
  but worth noting: serial device enumeration and naming differ across
  OSes. We document Linux only.
- **Device detection is heuristic.** We try `/dev/ttyACM0` then
  `/dev/ttyUSB0` by default; the operator can override. Wrong port →
  clear error, not a hang.

### Neutral

- HAT operators see no change. Their workflow remains: install
  `pymc_core`, configure it, set `connection_type = "tcp"` (or `"hat"`
  via the setup wizard).
- The TCP companion-frame boundary from ADR-0007 remains the primary
  extensibility point. The serial path is additive.

## Implementation notes

- Serial support lives in `meshcore-companion` behind a `serial`
  Cargo feature, enabled by default in `bbs-mesh`.
- Baud rate defaults to `115200`. Configurable via
  `[plugins.mesh] baud_rate`.
- The setup wizard (`supply-drop setup`) auto-detects likely serial
  devices and prompts the operator to confirm or override.
- A watchdog (reconnect-on-silence) mirrors the TCP reconnect logic
  already present in `CompanionClient`.
