# Running MeshCore and Meshtastic at the same time

Supply Drop BBS can serve MeshCore and Meshtastic users from a single instance. Both transports share the same user database, rooms, and message store — a user registered over MeshCore can receive mail from someone on Meshtastic and vice versa.

Each transport connects to its own radio. Both MeshCore and Meshtastic support the same two connection types:

- **Pi HAT** — a radio HAT connected to the Raspberry Pi's SPI/GPIO pins, managed by `pymc_core`
- **USB device** — a radio such as a Heltec V3, T-Beam, or RAK4631 connected over USB serial

Mix and match freely. Common setups include MeshCore HAT + Meshtastic USB, two USB devices, or two HATs if your Pi has the GPIO pins available.

---

## Using the setup wizard

The easiest way to configure both transports is to run the setup wizard:

```sh
supply-drop-bbs setup
```

The wizard walks you through each transport in turn — radio type, connection method, device selection — and writes the config for you. If you already have a working single-transport install, run the wizard again to add the second transport; it won't touch settings it doesn't ask about.

The rest of this guide covers manual configuration for operators who prefer to edit `config.toml` directly.

---

## How it works

```
MeshCore radio (HAT or USB)
    ├── HAT: pymc_core daemon → [plugins.mesh]  ──┐
    └── USB: direct serial   → [plugins.mesh]  ──┤
                                                  ├── Supply Drop BBS (shared DB + rooms)
Meshtastic radio (HAT or USB)                     │
    ├── HAT: pymc_core daemon → [plugins.meshtastic]  ──┤
    └── USB: direct serial   → [plugins.meshtastic] ───┘
```

The two transports run as independent tasks inside the same BBS process. They share nothing except the host — rooms, users, and messages are the same regardless of which radio a user comes in on.

---

## Meshtastic device setup

The radio's channel, frequency, region, and network settings are all configured through Meshtastic firmware, not through the BBS. Use the [Meshtastic app](https://meshtastic.org/downloads/) (Android, iOS, or desktop) over Bluetooth or USB to configure the device before connecting it to the Pi.

Things to set on the device before connecting:

- **Region** — your local frequency plan (e.g. `US`, `EU_868`)
- **Channel** — the channel your mesh uses
- **Role** — `CLIENT` or `ROUTER_CLIENT` is fine; `ROUTER` works too

The BBS does not push any configuration to the Meshtastic radio. It only sends and receives text packets. Everything about how the radio behaves on the mesh is controlled by the firmware.

---

## Finding serial port names

If either radio connects via USB, you need to know which port it claims. Plug it in and run:

```sh
dmesg | grep tty | tail -10
```

Look for a line like:

```
usb 1-1.2: cp210x converter now attached to ttyUSB0
```

If you have two USB radios, plug them in one at a time and note which port each claims. The first USB serial device is usually `/dev/ttyUSB0`, the second `/dev/ttyUSB1` — but this depends on plug order and the chips involved, so always confirm with `dmesg`.

HAT connections do not use a serial port number; the connection goes through `pymc_core` on `127.0.0.1:5000`.

---

## Configuration

Edit your `config.toml` (typically `/etc/supply-drop-bbs/config.toml`) and add both transport sections. Choose the connection type that matches your hardware for each.

### MeshCore transport

**Pi HAT:**

```toml
[plugins.mesh]
enabled         = true
connection_type = "hat"
addr            = "127.0.0.1:5000"  # pymc_core; default, change only if remote

[plugins.mesh.hat]
preset = "zebrahat"  # see preset table below
```

**USB device:**

```toml
[plugins.mesh]
enabled         = true
connection_type = "serial"
serial_port     = "/dev/ttyACM0"  # adjust to match your device
baud_rate       = 115200
```

### Meshtastic transport

**Pi HAT:**

```toml
[plugins.meshtastic]
enabled         = true
connection_type = "hat"
serial_port     = "/dev/ttyAMA0"  # UART port; enabled automatically by the installer
baud_rate       = 115200
```

**USB device:**

```toml
[plugins.meshtastic]
enabled         = true
connection_type = "serial"
serial_port     = "/dev/ttyUSB0"  # adjust to match your device
baud_rate       = 115200
```

### Optional: command prefix

Both transports accept an optional `command_prefix`. When set, users must start messages with that character (e.g. `!help`) instead of every direct message being treated as a command. Useful when a radio node also relays general mesh traffic.

```toml
# Add to either [plugins.mesh] or [plugins.meshtastic]
command_prefix = "!"
```

### HAT preset reference

If either transport uses a HAT, set the `preset` under `[plugins.mesh.hat]` or `[plugins.meshtastic.hat]` to match your hardware. For unlisted HATs, use `"custom"` and set GPIO pins manually — see [CONFIG.md](CONFIG.md#hat-pin-configuration-connection_type--hat).

| Preset          | Hardware              |
|-----------------|-----------------------|
| `zebrahat`      | ZebraHat              |
| `meshadv-mini`  | MeshAdv Mini          |
| `meshadv`       | MeshAdv (full size)   |
| `waveshare`     | Waveshare SX1262 HAT  |
| `uconsole`      | uConsole              |

---

## Applying the config

Validate before restarting:

```sh
supply-drop-bbs config check
```

Then restart the service:

```sh
sudo systemctl restart supply-drop-bbs
```

---

## Verifying both transports are up

Check the logs immediately after restart:

```sh
journalctl -u supply-drop-bbs -f
```

You should see a connected line for each transport within a few seconds:

```
INFO supply_drop_bbs::transport::mesh        connected
INFO supply_drop_bbs::transport::meshtastic  connected
```

If the MeshCore HAT transport logs a connection error, check that `pymc_core` is running:

```sh
systemctl status pymc-core
```

If either transport logs a serial error, confirm the port name with `dmesg | grep tty` and update `serial_port` in the config.

To turn on verbose logging for just one transport without making everything noisy:

```toml
[logging.targets]
"supply_drop_bbs::transport::mesh"       = "DEBUG"
"supply_drop_bbs::transport::meshtastic" = "DEBUG"
```

---

## What users experience

Users on either network see the same BBS: the same rooms, the same mail, the same other users. A MeshCore user posting to a room will have that message visible to a Meshtastic user who reads the same room. Private mail works the same way — delivered to whichever transport the recipient is currently connected on, or held for pickup if they're offline.

Account registration and login work identically on both transports. There is no separate account per radio type.
