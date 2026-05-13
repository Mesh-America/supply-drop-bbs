---
layout: home

hero:
  name: Supply Drop BBS
  text: A BBS for LoRa mesh networks, written in Rust
  tagline: Works with MeshCore and Meshtastic out of the box. Runs on a Pi. Add other transports (APRS, Telnet, IRC, whatever) by writing a plugin.
  image:
    src: /supply-drop-icon-transparent.svg
    alt: Supply Drop BBS logo
  actions:
    - theme: brand
      text: Get Started
      link: /INTRO
    - theme: alt
      text: View on GitHub
      link: https://github.com/Mesh-America/supply-drop-bbs

features:
  - icon: 📻
    title: MeshCore and Meshtastic
    details: Ships with bridges for MeshCore and Meshtastic LoRa networks. Tested on RAK WisBlock and Heltec hardware over serial and USB.
  - icon: 🔀
    title: Run multiple transports at once
    details: MeshCore radio, the CLI socket, the web admin, and any custom transport all run in the same process and share one user database and message store. APRS, Telnet, IRC, Matrix are all possible.
  - icon: 🥧
    title: Simple to run
    details: One binary, one TOML config file, one systemd unit. Intended to sit on a Pi and be ignored for months at a time.
  - icon: ⚡
    title: Built in Rust
    details: Fast startup, low memory, no GC pauses. Useful when you're running on a solar-charged Pi 3.
  - icon: 🔌
    title: Plugin API
    details: New transports and behaviors are Rust crates gated behind Cargo features. If you don't compile it in, it isn't there.
---

## Quick install

**Debian / Ubuntu / Raspberry Pi OS** — download the `.deb` for your hardware from the [latest release](https://github.com/Mesh-America/supply-drop-bbs/releases/latest):

| Hardware | File |
|---|---|
| Raspberry Pi 4 / 5 (64-bit) | `supply-drop-bbs_VERSION_arm64.deb` |
| Raspberry Pi 2 / 3 / Zero 2 (32-bit) | `supply-drop-bbs_VERSION_armhf.deb` |
| x86-64 Linux | `supply-drop-bbs_VERSION_amd64.deb` |

```sh
sudo dpkg -i supply-drop-bbs_VERSION_ARCH.deb
sudo supply-drop-bbs setup
sudo systemctl start supply-drop-bbs
```

Have these on hand before running setup:

- **Radio type** — USB companion device or Pi HAT
- **HAT model** — if using a HAT (ZebraHat, Waveshare, PiMesh, etc.)
- **Region / frequency** — US (910.525 MHz), EU (869.618 MHz), or your local frequency

[Full installation guide](/OPERATIONS) · [Configuration reference](/CONFIG)
