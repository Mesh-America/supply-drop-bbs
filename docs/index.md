---
layout: home

hero:
  name: Supply Drop BBS
  text: Open-source BBS for MeshCore LoRa radio networks
  tagline: Run a resilient off-grid bulletin board system on a Raspberry Pi with LoRa radio hardware — single binary, single config, months of unattended operation.
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
  - icon: 📡
    title: LoRa Radio Native
    details: Built for MeshCore LoRa networks. Supports RAK WisBlock, Heltec, and other companion devices over serial or USB.
  - icon: 🪫
    title: Resilient & Offline
    details: Designed to run unattended for months on a Raspberry Pi. Single binary, single TOML config, single systemd unit — obvious to operate.
  - icon: 🔌
    title: Extensible Plugin API
    details: Add transports, handlers, and behaviors via a clean Rust plugin API backed by Cargo features — no runtime loading overhead.
---
