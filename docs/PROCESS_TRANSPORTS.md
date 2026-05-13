# Process Transport Plugin Developer Guide

This guide covers everything you need to build a Supply Drop transport plugin
as an **external executable**. You write a program in any language; Supply Drop
spawns it, talks to it over stdin/stdout, and handles all BBS logic. Your
program only needs to manage its own connections and speak a simple
line-delimited JSON protocol.

> **Is this guide for you?**
> This guide is for **operators and third-party developers** who want to connect
> a new device or protocol to their own BBS instance without modifying Supply
> Drop's source code.
>
> If you are **contributing a native transport** (Meshtastic, APRS, etc.) to the
> Supply Drop project itself, see [Transport Plugins](./TRANSPORT_PLUGINS.md)
> instead — you will write a Rust crate and ship it in the binary.

---

## Mental model

```
┌─────────────────────────────────────┐
│  Your plugin process                │
│  ┌──────────────────────────────┐   │
│  │  Your connections            │   │
│  │  (TCP sockets, serial port,  │   │
│  │   Slack websocket, …)        │   │
│  └──────────────┬───────────────┘   │
│                 │ JSON over         │
│                 │ stdin/stdout      │
└─────────────────┼───────────────────┘
                  │
┌─────────────────┼───────────────────┐
│  Supply Drop BBS│                   │
│  ┌──────────────┴───────────────┐   │
│  │  ProcessTransport            │   │
│  │  • session management        │   │
│  │  • command parsing           │   │
│  │  • permission checks         │   │
│  └──────────────────────────────┘   │
│  ┌──────────────────────────────┐   │
│  │  BBS core (rooms, messages,  │   │
│  │  users, mail, …)             │   │
│  └──────────────────────────────┘   │
└─────────────────────────────────────┘
```

Supply Drop spawns your process at startup. Your process manages whatever
connections it wants (TCP, serial, websocket, radio API, …). When a user sends
a line of text, you tell Supply Drop. Supply Drop figures out what the text
means, applies it to BBS state, and tells you what to send back. Your job is
just to relay text.

---

## The IPC protocol

All messages are **JSON objects, one per line**, terminated by `\n`.
No binary, no length prefixes. Each object has a `"t"` field as the type
discriminator.

### Plugin → Supply Drop (your stdout)

| `t` | Other fields | Meaning |
|-----|-------------|---------|
| `ready` | `payload_limit?`, `version?` | You have initialised and are ready to accept connections |
| `open` | `id` | A new user connection arrived |
| `recv` | `id`, `line` | A user sent a line of text |
| `close` | `id` | A connection was closed by the remote end |

### Supply Drop → Plugin (your stdin)

| `t` | Other fields | Meaning |
|-----|-------------|---------|
| `send` | `id`, `text`, `hide_input?` | Send this text to the user |
| `kick` | `id` | Forcibly close this connection |
| `shutdown` | — | Graceful exit — stop accepting, close all connections, exit |

### Field reference

**`id`** — A string you choose that uniquely identifies one connection within
your process. It can be anything: the socket address, a counter, a node key.
It must be unique for the lifetime of the connection. Supply Drop echoes it
back when sending responses so you know which connection to write to.

**`payload_limit`** — Maximum bytes per response text frame. Set this to your
transport's MTU. Supply Drop truncates responses that exceed the limit. Use
`0` or omit the field for no limit (CLI-style transports).

**`version`** — A human-readable version string for your plugin, shown in the
admin web UI's plugins table. Use `env!("CARGO_PKG_VERSION")` in Rust or the
equivalent in your language. Optional — omit it if not meaningful, and `—`
will appear in the UI.

**`text`** — The display-ready string to deliver to the user. Supply Drop
does not append a newline; transports should add whatever framing their
protocol requires (e.g. Telnet appends `\r\n`, Slack posts the string as-is).

**`hide_input`** — When `true` in a `send` message, the user's next reply
should be visually hidden (password entry). Transports that don't support
input masking can ignore this.

> **Telnet note:** the standard way to hide input on a Telnet connection is
> to send `IAC WILL ECHO` (bytes `FF FB 01`) before the prompt, then
> `IAC WONT ECHO` (bytes `FF FC 01`) after the user's next line arrives.
> Most clients honour this and blank the echoed characters; clients that
> ignore it will show the password in plain text. Treat it as best-effort.

---

## Session lifecycle

1. User connects → send `{"t":"open","id":"conn-1"}`
2. Supply Drop creates a BBS session, no response needed
3. User sends text → send `{"t":"recv","id":"conn-1","line":"login alice"}`
4. Supply Drop processes the command, responds with `{"t":"send","id":"conn-1","text":"Password: ","hide_input":true}`
5. User sends password → send `{"t":"recv","id":"conn-1","line":"hunter2"}`
6. Supply Drop validates, responds with `{"t":"send","id":"conn-1","text":"Welcome, alice. Type 'H' for commands."}`
7. User disconnects → send `{"t":"close","id":"conn-1"}`

**You do not need to parse BBS commands.** You just forward raw lines in and
rendered text out. Supply Drop handles `login`, `help`, `N` (read new), `E`
(enter message), and everything else.

**You do not need to track workflow state** (whether the user is entering a
password or message body). Supply Drop tracks this internally and sets
`hide_input` appropriately.

---

## Startup sequence

Your process must:

1. Start up and initialise (open sockets, connect to APIs, etc.)
2. Print `{"t":"ready"}` (or `{"t":"ready","payload_limit":156,"version":"1.0.0"}`) to stdout
3. Begin accepting connections and printing `open`/`recv`/`close` events

Supply Drop logs your startup and treats any delay before `ready` as normal.
If your process exits before printing `ready`, Supply Drop logs the error and
(if configured) restarts it after a delay.

---

## Shutdown sequence

When Supply Drop sends `{"t":"shutdown"}`:

1. Stop accepting new connections
2. Send `{"t":"close","id":"..."}` for every open connection (or just exit
   cleanly — Supply Drop will end the sessions either way)
3. Exit

Supply Drop sends `shutdown` before its own process exits. If you do not exit
within 10 seconds, the OS will kill your process.

---

## Unsolicited notifications

Supply Drop may send `send` messages at any time, not just in response to a
`recv`. This happens when:

- Another user sends a DM to this user
- The user's account is validated
- A system announcement is broadcast

Your process must handle `send` messages that arrive between `recv`/`send`
pairs. This is rare on text-based transports but important for correctness.

---

## Payload limits and truncation

If your transport has a per-message size limit (LoRa, SMS, APRS):

- Declare it in your `ready` message: `{"t":"ready","payload_limit":156}`
- Supply Drop truncates long responses to fit the limit
- For responses with multiple parts (room listings, help text), Supply Drop
  sends each part as a separate `send` message

If your transport is unlimited (TCP, Slack), set `payload_limit: 0` or omit it.

---

## Error handling

- **Malformed JSON on stdout**: Supply Drop logs the error and skips the line.
  Your plugin will not receive a response.
- **`open` with a duplicate `id`**: Supply Drop logs a warning and kicks the
  old connection before opening the new one.
- **`recv` for an unknown `id`**: Supply Drop logs a warning and ignores it.
- **Your process crashes**: Supply Drop logs the exit code. If
  `restart_on_crash = true` in your config, it re-spawns after
  `restart_delay_secs` seconds.

Write errors to **stderr**. Supply Drop captures stderr and makes it available
via `supply-drop-bbs plugin logs <name>` and the web admin Plugins page.

---

## Complete example: minimal Telnet server (Python)

This example implements a simple Telnet-like transport that listens on TCP
port 2323 and connects each socket to the BBS.

```python
#!/usr/bin/env python3
"""
supply-drop-telnet — minimal TCP transport plugin for Supply Drop BBS.

Listens on TCP port 2323. Each TCP connection becomes a BBS session.
"""

import asyncio
import json
import sys
import argparse

parser = argparse.ArgumentParser()
parser.add_argument('--port', type=int, default=2323)
args = parser.parse_args()

# Track open connections: conn_id -> StreamWriter
connections: dict[str, asyncio.StreamWriter] = {}
conn_counter = 0
stdin_queue: asyncio.Queue[str] = asyncio.Queue()


def send_to_bbs(msg: dict) -> None:
    """Write a JSON message to Supply Drop via stdout."""
    print(json.dumps(msg), flush=True)


async def handle_connection(reader: asyncio.StreamReader, writer: asyncio.StreamWriter):
    global conn_counter
    conn_counter += 1
    conn_id = f"tcp:{writer.get_extra_info('peername')}:{conn_counter}"
    connections[conn_id] = writer

    send_to_bbs({"t": "open", "id": conn_id})

    try:
        while True:
            line = await reader.readline()
            if not line:
                break  # EOF — client disconnected
            text = line.decode('utf-8', errors='replace').rstrip('\r\n')
            send_to_bbs({"t": "recv", "id": conn_id, "line": text})
    except (ConnectionResetError, asyncio.IncompleteReadError):
        pass
    finally:
        send_to_bbs({"t": "close", "id": conn_id})
        connections.pop(conn_id, None)
        writer.close()


async def stdin_reader():
    """Read JSON messages from Supply Drop via stdin."""
    loop = asyncio.get_event_loop()
    reader = asyncio.StreamReader()
    protocol = asyncio.StreamReaderProtocol(reader)
    await loop.connect_read_pipe(lambda: protocol, sys.stdin.buffer)

    while True:
        line = await reader.readline()
        if not line:
            break
        await stdin_queue.put(line.decode('utf-8', errors='replace').strip())


async def stdin_dispatcher():
    """Handle messages from Supply Drop."""
    while True:
        raw = await stdin_queue.get()
        if not raw:
            continue
        try:
            msg = json.loads(raw)
        except json.JSONDecodeError:
            print(f"bad json from bbs: {raw!r}", file=sys.stderr, flush=True)
            continue

        t = msg.get("t")

        if t == "send":
            conn_id = msg["id"]
            text = msg.get("text", "")
            writer = connections.get(conn_id)
            if writer:
                try:
                    writer.write((text + "\r\n").encode('utf-8'))
                    await writer.drain()
                except Exception as e:
                    print(f"write error on {conn_id}: {e}", file=sys.stderr, flush=True)

        elif t == "kick":
            conn_id = msg["id"]
            writer = connections.pop(conn_id, None)
            if writer:
                writer.close()

        elif t == "shutdown":
            # Close all connections and exit.
            for writer in list(connections.values()):
                writer.close()
            connections.clear()
            sys.exit(0)


async def main():
    server = await asyncio.start_server(
        handle_connection, '0.0.0.0', args.port
    )
    addr = server.sockets[0].getsockname()
    print(f"listening on {addr}", file=sys.stderr, flush=True)

    # Signal readiness to Supply Drop.
    send_to_bbs({"t": "ready", "payload_limit": 0, "version": VERSION})

    async with server:
        await asyncio.gather(
            server.serve_forever(),
            stdin_reader(),
            stdin_dispatcher(),
        )


asyncio.run(main())
```

**config.toml entry:**

```toml
[[plugins.process]]
name    = "telnet"
command = "/usr/local/bin/supply-drop-telnet"
args    = ["--port", "2323"]
enabled = true
restart_on_crash  = true
restart_delay_secs = 5
```

---

## Complete example: Slack transport skeleton (Python)

This skeleton connects to Slack's Bolt API and routes messages from a
designated channel to the BBS. Each Slack user ID becomes a BBS connection.

```python
#!/usr/bin/env python3
"""
supply-drop-slack — Slack transport plugin skeleton for Supply Drop BBS.

Each Slack user who messages the bot gets their own BBS session.
Requires: pip install slack-bolt
"""

import asyncio
import json
import os
import sys
import threading
from slack_bolt import App
from slack_bolt.adapter.socket_mode import SocketModeHandler

SLACK_BOT_TOKEN = os.environ["SLACK_BOT_TOKEN"]
SLACK_APP_TOKEN = os.environ["SLACK_APP_TOKEN"]

app = App(token=SLACK_BOT_TOKEN)

# Active BBS sessions keyed by Slack user ID.
# Value is a function that sends text to that Slack user.
sessions: dict[str, callable] = {}
stdin_queue: asyncio.Queue[str] = asyncio.Queue()
loop: asyncio.AbstractEventLoop = None


def send_to_bbs(msg: dict) -> None:
    print(json.dumps(msg), flush=True)


@app.event("message")
def handle_message(event, say):
    user = event.get("user")
    text = event.get("text", "").strip()
    if not user or not text:
        return

    channel = event.get("channel")

    if user not in sessions:
        # New session.
        sessions[user] = lambda t: app.client.chat_postMessage(channel=channel, text=t)
        send_to_bbs({"t": "open", "id": user})

    send_to_bbs({"t": "recv", "id": user, "line": text})


def handle_stdin_msg(msg: dict) -> None:
    t = msg.get("t")
    if t == "send":
        user = msg["id"]
        text = msg.get("text", "")
        sender = sessions.get(user)
        if sender:
            try:
                sender(text)
            except Exception as e:
                print(f"slack send error: {e}", file=sys.stderr, flush=True)
    elif t == "kick":
        sessions.pop(msg["id"], None)
    elif t == "shutdown":
        sessions.clear()
        sys.exit(0)


async def stdin_reader():
    reader = asyncio.StreamReader()
    protocol = asyncio.StreamReaderProtocol(reader)
    await asyncio.get_event_loop().connect_read_pipe(lambda: protocol, sys.stdin.buffer)
    while True:
        raw = await reader.readline()
        if not raw:
            break
        text = raw.decode('utf-8', errors='replace').strip()
        if text:
            try:
                handle_stdin_msg(json.loads(text))
            except Exception as e:
                print(f"stdin parse error: {e}", file=sys.stderr, flush=True)


def run_slack():
    handler = SocketModeHandler(app, SLACK_APP_TOKEN)
    handler.start()


if __name__ == "__main__":
    print("connecting to Slack…", file=sys.stderr, flush=True)
    slack_thread = threading.Thread(target=run_slack, daemon=True)
    slack_thread.start()

    # Signal readiness (unlimited payload for Slack).
    send_to_bbs({"t": "ready", "payload_limit": 0, "version": VERSION})
    print("ready", file=sys.stderr, flush=True)

    asyncio.run(stdin_reader())
```

**config.toml entry:**

```toml
[[plugins.process]]
name    = "slack"
command = "/usr/local/bin/supply-drop-slack"
args    = []
enabled = true
restart_on_crash  = true
restart_delay_secs = 10
```

Environment variables (`SLACK_BOT_TOKEN`, `SLACK_APP_TOKEN`) are set in your
systemd unit or shell environment before starting Supply Drop.

---

## Minimal example: Rust plugin using the SDK types

If you prefer Rust, the `bbs-plugin-api` crate exports the IPC types:

```rust
// Cargo.toml:
// bbs-plugin-api = { git = "https://github.com/Mesh-America/supply-drop-bbs" }

use bbs_plugin_api::ipc::{HostMsg, PluginMsg};
use std::io::{self, BufRead, Write};

fn send(msg: &PluginMsg) {
    let line = serde_json::to_string(msg).unwrap();
    println!("{line}");
}

fn main() {
    // Signal readiness.
    send(&PluginMsg::Ready {
        payload_limit: 0,
        version: Some(env!("CARGO_PKG_VERSION").to_owned()),
    });

    // Read commands from Supply Drop.
    let stdin = io::stdin();
    for line in stdin.lock().lines() {
        let line = line.unwrap();
        let msg: HostMsg = serde_json::from_str(&line).unwrap();
        match msg {
            HostMsg::Send { id, text, .. } => {
                eprintln!("[{id}] send: {text:?}");
                // Write to your connection here.
            }
            HostMsg::Kick { id } => {
                eprintln!("[{id}] kicked");
            }
            HostMsg::Shutdown => std::process::exit(0),
        }
    }
}
```

> Note: `bbs_plugin_api::ipc` is re-exported from `bbs-process-transport`.
> The IPC types (`PluginMsg`, `HostMsg`) are in `bbs_process_transport::ipc`.

---

## BBS command reference for plugin authors

Your plugin never needs to parse these. They are listed so you can test
your transport manually by typing them.

| Input | Action |
|-------|--------|
| `register <username>` | Start account registration |
| `login <username>` | Start login |
| `H` or `help` | Show help |
| `N` | Read new messages in current room |
| `R` | Read messages in reverse (newest first) |
| `E <message>` | Post a message |
| `E @alice <message>` | Send a direct message to alice |
| `K` | List rooms |
| `M` | Go to Mail (DM inbox) |
| `W` | Who is online |
| `Q` or `logout` | Log out |

---

## Testing your plugin locally

Run Supply Drop with your plugin in a test config:

```toml
# test-config.toml
[database]
path = "/tmp/test-bbs.db"

[[plugins.process]]
name    = "my-plugin"
command = "./my-plugin"
enabled = true
```

```sh
supply-drop-bbs --config test-config.toml run
```

Watch stderr from your plugin:

```sh
supply-drop-bbs plugin logs my-plugin
```

Or in the web admin under **Plugins → my-plugin → logs**.

You can also test the protocol manually without Supply Drop. Remember the
direction: **your plugin reads `HostMsg` on stdin** (`send`, `kick`,
`shutdown`) and **writes `PluginMsg` on stdout** (`ready`, `open`, `recv`,
`close`). To drive your plugin from the host side, pipe `HostMsg` JSON into
its stdin:

```sh
# Start your plugin (it will print {"t":"ready"} and wait).
./my-plugin --port 2323 &

# Send a shutdown from the fake host side (stdin of the plugin).
echo '{"t":"shutdown"}' | ./my-plugin --port 2323
```

For end-to-end testing of the full session flow (`open` → `recv` → `send`),
run the plugin normally and connect with a real client:

```sh
./my-plugin --port 2323 &
telnet localhost 2323   # drives open/recv/close; watch plugin stdout for send frames
```

Watch the plugin's stdout to see the `send` JSON frames Supply Drop would
consume, and stderr for your own log output.

### fake-host harness

For fuller testing without a running BBS, use the harness in
`contrib/fake-host`. It spawns your plugin, waits for `ready`, then lets
you drive it interactively or replay a scenario file:

```sh
# Interactive — type HostMsg shorthands or raw JSON at the prompt.
python3 contrib/fake-host ./my-plugin --port 2323

# Scripted — replay the bundled example scenario.
python3 contrib/fake-host --script contrib/fake-host-scenario.jsonl \
        ./my-plugin --port 2323
```

**Shorthands** accepted in both modes:

| Input | Sends |
|-------|-------|
| `send <id> <text>` | `{"t":"send","id":"<id>","text":"<text>"}` |
| `kick <id>` | `{"t":"kick","id":"<id>"}` |
| `shutdown` | `{"t":"shutdown"}` |
| `sleep <n>` | pause *n* seconds (script files only) |
| any JSON object | sent verbatim |

Traffic is colour-coded: **green** for plugin → host, **yellow** for
host → plugin, so you can see both sides at a glance.

The scenario file (`contrib/fake-host-scenario.jsonl`) walks through
register → login → post → read → disconnect → shutdown. Copy and edit it
to build regression scenarios for your own transport.

---

## Transport author checklist

Before shipping a process transport plugin, run through this list:

**Protocol basics**
- [ ] Plugin prints `{"t":"ready"}` (or `{"t":"ready","payload_limit":N}`)
  before accepting any connections — not after the first client arrives
- [ ] Connection IDs are unique for the lifetime of each connection;
  an ID may be reused only after `close` has been sent for the old one
- [ ] `{"t":"close","id":"..."}` is sent whenever the remote end drops,
  whether the client initiated it or the plugin did

**Host message handling**
- [ ] `send` messages are handled at any time, not only in direct response
  to a `recv` — unsolicited DMs, broadcasts, and validation notices arrive
  between turns
- [ ] `kick` closes the connection and removes it from your tracking map;
  the BBS has already ended the session — do not send `close` after a kick
- [ ] `shutdown` stops accepting new connections, sends `close` for any
  still-open connections (optional but polite), then exits within 10 s

**Framing**
- [ ] Your transport adds the appropriate terminator to `send.text`
  (Telnet: `\r\n`; Slack/HTTP: none — post the string as-is)
- [ ] Inbound text has transport framing stripped before it goes into
  `recv.line` (trailing `\r\n`, leading prompts, etc.)

**Robustness**
- [ ] A `send` frame that arrives for a connection your plugin already
  closed is silently dropped, not logged as an error
- [ ] All diagnostic output goes to **stderr** — stdout is reserved for
  the IPC protocol
- [ ] Plugin exits with code `0` after a clean `shutdown`; non-zero on
  unexpected failure (Supply Drop logs the exit code)

**Optional but recommended**
- [ ] `payload_limit` declared in `ready` if your transport has a per-frame
  MTU (LoRa, SMS, APRS — leave at `0` or omit for unlimited transports)
- [ ] `version` declared in `ready` — shown in the admin UI plugins table
- [ ] `hide_input` honoured, or explicitly noted as unsupported in your
  README
- [ ] `restart_on_crash = true` in the production config entry
- [ ] Tested end-to-end with `contrib/fake-host` and at least one scenario
  covering open → login → command → disconnect → shutdown

---

## Protocol changelog

| Version | Change |
|---------|--------|
| 0.3.0 | Initial protocol definition |
