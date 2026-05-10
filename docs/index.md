---
layout: home

hero:
  name: Supply Drop BBS
  text: A BBS for LoRa mesh networks, written in Rust
  tagline: Works with MeshCore and Meshtastic out of the box. Runs on a Pi. Add other transports (APRS, Telnet, IRC, whatever) by writing a plugin.
  image:
    src: /logo.png
    alt: Supply Drop BBS logo
  actions:
    - theme: brand
      text: Get Started
      link: /OPERATIONS
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
