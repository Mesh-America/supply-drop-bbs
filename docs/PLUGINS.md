# Plugins

Supply Drop BBS supports transport plugins — standalone binaries that connect to the BBS over a simple stdin/stdout JSON IPC protocol. Each plugin runs as its own process, so you can add or remove transports without recompiling the BBS.

See the [Transport Plugins guide](TRANSPORT_PLUGINS.md) for details on how the plugin protocol works and how to write your own.

---

## Available plugins

### Telnet Transport

Lets classic Telnet clients connect to your Supply Drop BBS on port 2323. Handles CRLF, backspace, IAC negotiation, and echo suppression so any terminal that speaks Telnet works out of the box.

- **Prebuilt binaries** for Linux, macOS, and Windows
- **One-command install** — an included script registers the plugin and restarts the BBS
- Runs as an independent process; no BBS recompile needed

> **Security note:** Telnet is plaintext. Run it on a trusted LAN, or put it behind a VPN or SSH tunnel before exposing it further.

**Docs:** [supply-drop-telnet-transport-plugin](https://mesh-america.github.io/supply-drop-telnet-transport-plugin/)

---

## Writing your own plugin

Any executable that speaks the process transport IPC protocol can be a plugin. The [Plugin API Guide](PLUGIN_API.md) and [Transport Plugins guide](TRANSPORT_PLUGINS.md) cover everything you need to get started.
