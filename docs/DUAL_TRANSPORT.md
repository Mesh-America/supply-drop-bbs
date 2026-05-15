# Running MeshCore and Meshtastic at the same time

Supply Drop BBS can serve MeshCore and Meshtastic users from a single instance. Both transports share the same user database, rooms, and message store — a user registered over MeshCore can receive mail from someone on Meshtastic and vice versa.

This guide covers the most common hardware combination:

- **MeshCore** — a Pi HAT connected to the Raspberry Pi's SPI/GPIO pins, managed by `pymc_core`
- **Meshtastic** — a USB device such as a Heltec V3 plugged into a USB port

---

## How it works

```
Pi HAT (SPI/GPIO)
    └── pymc_core (system daemon, port 5000)
            └── [plugins.mesh]  ──┐
                                  ├── Supply Drop BBS (shared DB + rooms)
Heltec V3 (USB serial)            │
    └── [plugins.meshtastic]  ────┘
```

The two transports run as independent tasks inside the same BBS process. They don't know about each other — they just both call into the same host. `pymc_core` is only involved on the MeshCore side; the Meshtastic side talks directly to the USB device.

---

## Meshtastic device setup

The Heltec V3's channel, frequency, region, and network settings are all configured through Meshtastic firmware, not through the BBS. Use the [Meshtastic app](https://meshtastic.org/downloads/) (Android, iOS, or desktop) over Bluetooth or USB to configure it before connecting it to the Pi.

Things to set on the device before connecting:

- **Region** — your local frequency plan (e.g. `US`, `EU_868`)
- **Channel** — the channel your mesh uses
- **Role** — `CLIENT` or `ROUTER_CLIENT` is fine; `ROUTER` works too

The BBS does not push any configuration to the Meshtastic radio. It only sends and receives text packets. Everything about how the radio behaves on the mesh is controlled by the firmware.

---

## Finding the serial port

Plug the Heltec V3 in and check which serial port it claimed:

```sh
ls /dev/ttyUSB* /dev/ttyACM*
```

The Heltec V3 uses a CP2102 USB-to-serial chip, which typically appears as `/dev/ttyUSB0`. If you already have other USB serial devices (like a USB MeshCore companion), it may claim `/dev/ttyUSB1` or higher. The reliable way to check:

```sh
dmesg | grep tty | tail -10
```

Look for a line like:

```
usb 1-1.2: cp210x converter now attached to ttyUSB0
```

---

## Configuration

Add or update these sections in your `config.toml` (typically `/etc/supply-drop-bbs/config.toml`):

```toml
# ─── MeshCore transport (Pi HAT via pymc_core) ─────────────────────

[plugins.mesh]
enabled         = true
connection_type = "hat"
# Address of the pymc_core CompanionFrameServer.
# Default is 127.0.0.1:5000; change only if pymc_core is remote.
addr            = "127.0.0.1:5000"

# Choose the preset that matches your HAT model.
# Options: zebrahat, meshadv-mini, meshadv, waveshare, uconsole, custom
[plugins.mesh.hat]
preset = "zebrahat"

# Optional: require a prefix character so mesh users must type
# "!help" instead of just "help". Useful if the BBS node also
# relays normal mesh traffic and you want to avoid false positives.
# command_prefix = "!"


# ─── Meshtastic transport (Heltec V3 via USB serial) ───────────────

[plugins.meshtastic]
enabled         = true
connection_type = "serial"
serial_port     = "/dev/ttyUSB0"    # adjust to match your device
baud_rate       = 115200

# Optional: set a prefix so Meshtastic users address the BBS
# explicitly rather than every DM being treated as a command.
# command_prefix = "!"
```

### HAT preset reference

Pick the preset that matches your HAT. If your HAT is not in the list, use `"custom"` and set the GPIO pins manually — see [CONFIG.md](CONFIG.md#hat-pin-configuration-connection_type--hat) for the pin fields.

| Preset          | Hardware                       |
|-----------------|--------------------------------|
| `zebrahat`      | ZebraHat                       |
| `meshadv-mini`  | MeshAdv Mini                   |
| `meshadv`       | MeshAdv (full size)            |
| `waveshare`     | Waveshare SX1262 HAT           |
| `uconsole`      | uConsole                       |

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

You should see two transport startup lines within a few seconds, something like:

```
INFO supply_drop_bbs::transport::mesh        connected addr=127.0.0.1:5000
INFO supply_drop_bbs::transport::meshtastic  connected port=/dev/ttyUSB0
```

If the MeshCore transport logs a connection error, check that `pymc_core` is running (`systemctl status pymc-core`) and that the HAT is seated properly.

If the Meshtastic transport logs a serial error, double-check the port with `dmesg | grep tty` and update `serial_port` accordingly.

To turn on verbose logging for just one transport while debugging, without making everything noisy:

```toml
[logging.targets]
"supply_drop_bbs::transport::mesh"       = "DEBUG"
"supply_drop_bbs::transport::meshtastic" = "DEBUG"
```

---

## What users experience

Users on either network see the same BBS: the same rooms, the same mail, the same other users. A MeshCore user posting to a room will have that message visible to a Meshtastic user who reads the same room. Private mail works the same way — it's delivered to whichever transport the recipient is currently connected on, or held for pickup if they're offline.

Account registration and login work identically on both transports. There is no separate account per radio type.
