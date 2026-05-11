# Process Transport Plugins — Operator Guide

This guide covers how to install, configure, and manage **process transport
plugins** on a running Supply Drop BBS. It assumes you are the sysop who has
shell access to the server.

> **Are you a plugin developer?**
> This guide is for operators who are *installing* a plugin someone else wrote.
> If you are *building* a plugin, see
> [Process Transports (Developer Guide)](./PROCESS_TRANSPORTS.md) instead.

---

## What is a process transport plugin?

A process transport is an external program that Supply Drop spawns at startup.
It connects users to the BBS through whatever channel the plugin supports
(Telnet, Slack, Discord, APRS, SMS, LoRa API, …). Supply Drop and the plugin
communicate over stdin/stdout using a simple JSON protocol — you do not need
to know the details to operate one.

From your perspective as a sysop: you drop an executable on the server,
add a short stanza to `config.toml` (or use the CLI or web UI), and Supply
Drop handles the rest.

---

## Quick start

1. Install the plugin executable (e.g. `/usr/local/bin/supply-drop-telnet`).
2. Add it to your config:

```toml
[[plugins.process]]
name    = "telnet"
command = "/usr/local/bin/supply-drop-telnet"
args    = ["--port", "2323"]
enabled = true
restart_on_crash   = true
restart_delay_secs = 5
```

3. Restart Supply Drop (or use `supply-drop-bbs plugin add` if the daemon is
   already running and you do not want to restart it).
4. Tail the plugin's stderr: `supply-drop-bbs plugin logs telnet`.

---

## Installing a plugin executable

A process transport is just an executable file. Installation is the same as
for any binary or script:

**Compiled binary** (Rust, Go, C, …):

```sh
sudo cp supply-drop-telnet /usr/local/bin/
sudo chmod 755 /usr/local/bin/supply-drop-telnet
```

**Python script** (requires Python on the server):

```sh
sudo cp supply-drop-slack.py /usr/local/bin/supply-drop-slack
sudo chmod 755 /usr/local/bin/supply-drop-slack
# Ensure the shebang at line 1 is: #!/usr/bin/env python3
pip3 install slack-bolt   # any runtime deps the plugin needs
```

The plugin runs as the same OS user as Supply Drop. Make sure the executable
is readable and executable by that user.

### Environment variables

Set any secrets the plugin needs (API tokens, passwords) in the environment of
the Supply Drop process — not in `config.toml` where they would be committed
to version control.

For systemd units:

```ini
[Service]
Environment="SLACK_BOT_TOKEN=xoxb-..."
Environment="SLACK_APP_TOKEN=xapp-..."
ExecStart=/usr/local/bin/supply-drop-bbs run
```

The plugin inherits Supply Drop's environment, so all `Environment=` entries
are visible to it.

---

## Configuring plugins in config.toml

Each plugin gets one `[[plugins.process]]` table (note the double brackets —
it is an array of tables, so you can have many):

```toml
[[plugins.process]]
name               = "telnet"           # unique name, used in CLI and web UI
command            = "/usr/local/bin/supply-drop-telnet"
args               = ["--port", "2323"] # passed verbatim to the executable
enabled            = true               # false → not started at all
restart_on_crash   = true               # re-spawn if the process exits unexpectedly
restart_delay_secs = 5                  # seconds to wait before re-spawning
```

All fields except `name` and `command` are optional (defaults shown above).

### Multiple plugins

```toml
[[plugins.process]]
name    = "telnet"
command = "/usr/local/bin/supply-drop-telnet"
args    = ["--port", "2323"]
enabled = true

[[plugins.process]]
name    = "slack"
command = "/usr/local/bin/supply-drop-slack"
enabled = true
restart_on_crash   = true
restart_delay_secs = 10
```

---

## Managing plugins from the CLI

The `supply-drop-bbs plugin` sub-command lets you manage plugins without
editing config.toml by hand. It writes changes back to the config file for
you, preserving comments and formatting.

```
supply-drop-bbs plugin <action>
```

| Command | Description |
|---------|-------------|
| `plugin list` | Show all configured plugins and their current state |
| `plugin add --name NAME --command CMD [--args "…"] [--disabled] [--no-restart] [--restart-delay N]` | Add a new plugin |
| `plugin remove NAME` | Remove a plugin (stops it if running) |
| `plugin enable NAME` | Enable a disabled plugin |
| `plugin disable NAME` | Disable a running plugin (stops it immediately) |
| `plugin logs NAME` | Show the last 100 stderr lines from a plugin |

### Examples

```sh
# List all plugins
supply-drop-bbs plugin list

# Add a Telnet plugin
supply-drop-bbs plugin add \
  --name telnet \
  --command /usr/local/bin/supply-drop-telnet \
  --args "--port 2323"

# Add a Slack plugin, disabled for now
supply-drop-bbs plugin add \
  --name slack \
  --command /usr/local/bin/supply-drop-slack \
  --disabled

# Enable the Slack plugin once you have the tokens configured
supply-drop-bbs plugin enable slack

# Temporarily disable the Telnet plugin for maintenance
supply-drop-bbs plugin disable telnet

# View recent stderr output from a plugin
supply-drop-bbs plugin logs slack

# Remove a plugin entirely
supply-drop-bbs plugin remove telnet
```

---

## Managing plugins from the web UI

In the web admin, navigate to **Plugins** in the left sidebar. The Plugins
page is only visible to sysops (permission level ≥ 100).

### Plugin table

The table shows all configured plugins with:

- **name** — the plugin's configured name
- **command** — the full command line being run
- **state** — `running`, `stopped`, `crashed`, or `disabled`
- **restarts** — how many times the plugin has been re-spawned since startup

### Actions

| Button | What it does |
|--------|-------------|
| **logs** | Opens a side drawer showing the last 100 stderr lines |
| **enable / disable** | Toggle the plugin on or off; change persists to config.toml |
| **restart** | Stop and immediately re-start the plugin process |
| **remove** | Stop the plugin and delete its config entry |

### Adding a plugin

Click **+ add plugin** (top right). Fill in:

- **name** — a short identifier, e.g. `telnet`
- **command** — full path to the executable, e.g. `/usr/local/bin/supply-drop-telnet`
- **args** — space-separated arguments, e.g. `--port 2323 --verbose`
- **start enabled** — checked by default; uncheck to add in disabled state
- **restart on crash** — checked by default; uncheck if the plugin should not auto-restart
- **restart delay (s)** — seconds between crash and restart (default 5)

The plugin is started immediately if "start enabled" is checked.

---

## Monitoring and troubleshooting

### Viewing logs

Each plugin's stderr is captured by Supply Drop and stored in a ring buffer
(last 1000 lines). Well-written plugins send startup messages, connection
events, and errors to stderr.

**CLI:**

```sh
supply-drop-bbs plugin logs <name>
```

**Web UI:** Plugins page → **logs** button next to the plugin.

**Supply Drop's own logs** also include plugin lifecycle events
(spawned, exited, restart scheduled). Check these if a plugin never appears
in the plugin table at all:

```sh
journalctl -u supply-drop-bbs -n 200
```

### Plugin states

| State | Meaning |
|-------|---------|
| `running` | Process is alive and sent `{"t":"ready"}` |
| `stopped` | Plugin is enabled but not yet started, or was cleanly stopped |
| `crashed` | Process exited unexpectedly; the exit reason is shown next to the state |
| `disabled` | `enabled = false` in config; Supply Drop will not start it |

### Common problems

**Plugin state is `stopped`, never becomes `running`**

1. Check plugin stderr: `supply-drop-bbs plugin logs <name>` — startup errors
   (missing dependencies, port already in use) appear here.
2. Check that the executable path is correct and executable by Supply Drop's
   user: `ls -la /usr/local/bin/supply-drop-telnet`.
3. If the plugin exits before printing `{"t":"ready"}`, Supply Drop marks it
   `stopped` and (if `restart_on_crash = true`) schedules a restart.

**Plugin state is `crashed` and keeps restarting**

The plugin is exiting unexpectedly after starting. Check stderr for the error.
If the plugin crashes immediately on every restart, disable it while you
investigate to avoid rapid restart loops:

```sh
supply-drop-bbs plugin disable <name>
```

**Users can connect but see no output / get no welcome message**

The BBS session is opening but responses aren't reaching the user. Possible causes:
- The plugin is not forwarding `send` messages correctly — check plugin stderr.
- A `payload_limit` too small is causing responses to be truncated or dropped.

**Users get `session expired` messages unexpectedly**

Supply Drop ended the session on its side (e.g. due to a timeout). Check if
the plugin sent a `{"t":"close","id":"…"}` event that shouldn't have been sent.

**Port already in use**

The plugin's listen port is taken by another process. Either change the port
in the plugin's `args`, or stop the conflicting service:

```sh
sudo lsof -i :2323   # find what's using port 2323
```

### Testing a plugin in isolation

You can verify a plugin's basic protocol compliance without running Supply
Drop by piping JSON to it manually:

```sh
echo '{"t":"open","id":"test"}' | /usr/local/bin/supply-drop-telnet
```

The plugin should print `{"t":"ready",...}` first (from its startup), then
process your input. This is useful for catching JSON parse errors or crashes
before wiring the plugin into Supply Drop.

---

## Restart behaviour

When `restart_on_crash = true`:

1. The plugin process exits (any non-zero exit code, or signal).
2. Supply Drop waits `restart_delay_secs` seconds.
3. The plugin is re-spawned from scratch (`init` → `start`).
4. All in-flight sessions from before the crash are ended on the BBS side.

When `restart_on_crash = false` (or the daemon receives `shutdown`):

- The plugin is stopped and stays stopped.
- The `state` in the web UI becomes `stopped` (clean stop) or `crashed`
  (unexpected exit, no auto-restart).

A graceful Supply Drop shutdown sends `{"t":"shutdown"}` to every running
plugin before exiting. Well-behaved plugins close their connections and exit
within 10 seconds; Supply Drop then sends SIGKILL if they do not.

---

## Security considerations

- Plugin executables run as the Supply Drop OS user. Keep them out of
  world-writable directories.
- Store API tokens in environment variables, not in `config.toml`.
- The plugin has full access to stdin/stdout of Supply Drop. Only install
  plugins from sources you trust.
- Supply Drop's permission system (user roles, banned users) still applies —
  a plugin cannot bypass BBS-level access controls.
