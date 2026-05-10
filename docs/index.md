---
layout: home

hero:
  name: Supply Drop BBS
  text: A resilient BBS that meets users where they are
  tagline: First-class support for MeshCore and Meshtastic LoRa networks — plus any transport you can write a plugin for. One node, every channel, running unattended for months.
  image:
    src: /supply-drop-icon-transparent.svg
    alt: Supply Drop BBS logo — pixel-art parachute dropping a supply crate
  actions:
    - theme: brand
      text: Get Started
      link: /OPERATIONS
    - theme: alt
      text: View on GitHub
      link: https://github.com/Mesh-America/supply-drop-bbs

features:
  - icon: 📻
    title: MeshCore & Meshtastic
    details: Native bridges for MeshCore LoRa and Meshtastic networks. Supports RAK WisBlock, Heltec, and other companion devices over serial or USB.
  - icon: 🔀
    title: Multi-Transport by Design
    details: Run MeshCore radio, a Unix socket CLI, the HTTP web admin, and any community transport — APRS, Telnet, IRC, Matrix — all simultaneously on one node. Sessions share the same message store and user database across every channel.
  - icon: 🪫
    title: Resilient & Offline
    details: Designed to run unattended for months on a Raspberry Pi. Single binary, single TOML config, single systemd unit — obvious to operate with no cloud dependency.
  - icon: ⚡
    title: Built in Rust
    details: Zero garbage collector, no runtime, no interpreter. Supply Drop BBS starts in milliseconds, idles at near-zero CPU, and handles bursts without breaking a sweat — exactly what you want on a Pi running off a solar battery pack.
  - icon: 🔌
    title: Extensible Plugin API
    details: Add new transports, handlers, and behaviors via a clean Rust plugin API backed by Cargo features. No runtime loading overhead — unused transports compile out entirely.
---
