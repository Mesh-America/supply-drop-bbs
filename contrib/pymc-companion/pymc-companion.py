#!/usr/bin/env python3
"""
pymc-companion — minimal openhop_core companion frame server for supply-drop-bbs.

Drives a LoRa HAT directly via SPI using openhop_core and exposes the MeshCore
companion frame protocol on a TCP port so supply-drop-bbs can connect to it.

This is the same approach as mesh-citadel's HatRuntime — openhop_core runs
in-process as a library, not as a separate daemon.

Usage:
    python pymc-companion.py --config /etc/supply-drop-bbs/pymc-companion.yaml
"""

from __future__ import annotations

import argparse
import asyncio
import logging
import os
import signal
import sqlite3
import sys

import yaml

log = logging.getLogger("pymc-companion")

# ── Contact persistence ───────────────────────────────────────────────────────
#
# openhop_core's ContactStore is purely in-memory (see its own docstring) —
# nothing in openhop_core or this script persisted it to disk until now, so
# every restart of this process (an upgrade, a crash, a reboot — not just a
# restart of the BBS itself) silently wiped every contact, including which
# ones were favourited/protected. supply-drop-bbs's own "protected contact"
# state is rehydrated FROM whatever this bridge reports on each reconnect, so
# losing the bridge's copy meant losing the BBS's copy too, with no way to
# recover. See supply-drop-bbs-7ot / GitHub #244.
#
# load_contacts_db/save_contacts_db below are plain synchronous sqlite3 --
# NOT matching openhop_repeater's own approach to persisting this same
# ContactStore (a hostile-audit finding: openhop_repeater actually wraps its
# equivalent calls in asyncio.to_thread(), explicitly to avoid blocking its
# event loop). Call sites that run concurrently with radio I/O / companion
# protocol handling (the periodic autosave, the shutdown save) route through
# asyncio.to_thread() for the same reason; only the one-time startup load,
# which runs before anything else is happening on the loop, calls it
# directly.

_CONTACTS_SCHEMA = """
CREATE TABLE IF NOT EXISTS contacts (
    public_key TEXT PRIMARY KEY,
    name TEXT NOT NULL DEFAULT '',
    adv_type INTEGER NOT NULL DEFAULT 0,
    flags INTEGER NOT NULL DEFAULT 0,
    out_path_len INTEGER NOT NULL DEFAULT -1,
    out_path TEXT NOT NULL DEFAULT '',
    last_advert_timestamp INTEGER NOT NULL DEFAULT 0,
    lastmod INTEGER NOT NULL DEFAULT 0,
    gps_lat REAL NOT NULL DEFAULT 0.0,
    gps_lon REAL NOT NULL DEFAULT 0.0,
    sync_since INTEGER NOT NULL DEFAULT 0,
    last_advert_packet TEXT NOT NULL DEFAULT ''
)
"""

# Same field names as openhop_core's ContactStore.to_dicts()/load_from_dicts()
# use — the schema is a direct mirror so load/save need no field translation.
_CONTACT_FIELDS = (
    "public_key",
    "name",
    "adv_type",
    "flags",
    "out_path_len",
    "out_path",
    "last_advert_timestamp",
    "lastmod",
    "gps_lat",
    "gps_lon",
    "sync_since",
    "last_advert_packet",
)


def load_contacts_db(path: str) -> list[dict]:
    """Load persisted contacts from `path`.

    Returns an empty list if the file doesn't exist yet (first run — not an
    error). Raises on any other failure (corrupt file, permission error, or
    a file too small/damaged to be a real SQLite database): callers must not
    treat that the same as "no contacts survived" — silently continuing with
    an empty store risks the next autosave overwriting a recoverable file
    with nothing, permanently losing what was in it.

    A hostile-audit repro found that a plain `SELECT` alone does NOT catch
    this: SQLite treats a 0-byte file as a freshly-initialisable empty
    database (no error, `CREATE TABLE IF NOT EXISTS` succeeds, the SELECT
    returns `[]`) and a file truncated to ~75-90% of its real size can also
    return a row set with no exception. Two explicit checks close this: a
    minimum-size floor (a real SQLite file is at least one page, 4096 bytes,
    once anything has ever been written to it) and `PRAGMA integrity_check`,
    which walks SQLite's own on-disk structures rather than trusting that a
    query merely returning success means the data is intact.
    """
    if not os.path.exists(path):
        return []
    if os.path.getsize(path) < 4096:
        raise ValueError(
            f"{path} exists but is smaller than one SQLite page (4096 bytes) "
            "-- too small to be a valid database that ever held data; "
            "treating as corrupt rather than risking a truncated read."
        )
    conn = sqlite3.connect(path)
    try:
        conn.row_factory = sqlite3.Row
        integrity = conn.execute("PRAGMA integrity_check").fetchone()[0]
        if integrity != "ok":
            raise ValueError(f"integrity_check failed for {path}: {integrity}")
        conn.execute(_CONTACTS_SCHEMA)
        rows = conn.execute(
            f"SELECT {', '.join(_CONTACT_FIELDS)} FROM contacts"
        ).fetchall()
        return [dict(row) for row in rows]
    finally:
        conn.close()


def save_contacts_db(path: str, records: list[dict]) -> None:
    """Atomically overwrite `path` with the current full contact list."""
    os.makedirs(os.path.dirname(path) or ".", exist_ok=True)
    tmp = path + ".tmp"
    for stale in (tmp, tmp + "-journal"):
        try:
            os.remove(stale)
        except FileNotFoundError:
            pass
    conn = sqlite3.connect(tmp)
    try:
        conn.execute(_CONTACTS_SCHEMA)
        # OR REPLACE: to_dicts() is keyed by public_key internally so it
        # can't itself produce duplicates today, but a plain INSERT would
        # otherwise let one future duplicate discard the entire batch (a
        # hostile-audit repro confirmed executemany's failure is all-or-
        # nothing, not per-row) -- cheap insurance against that either way.
        conn.executemany(
            f"INSERT OR REPLACE INTO contacts ({', '.join(_CONTACT_FIELDS)}) "
            f"VALUES ({', '.join('?' for _ in _CONTACT_FIELDS)})",
            [tuple(rec[f] for f in _CONTACT_FIELDS) for rec in records],
        )
        conn.commit()
    finally:
        conn.close()
    os.replace(tmp, path)


def load_config(path: str) -> dict:
    with open(path) as f:
        return yaml.safe_load(f)


def load_or_create_identity(LocalIdentity, identity_path: str | None):
    """Load a persisted identity or generate and save a new one."""
    if not identity_path:
        log.warning(
            "No identity_path configured — using an ephemeral identity. "
            "Your public key will change on every restart, breaking contacts. "
            "Set companion.identity_path in your config."
        )
        return LocalIdentity()

    try:
        with open(identity_path, "rb") as f:
            seed = f.read()
        log.info(f"Loaded identity from {identity_path}")
        return LocalIdentity(seed=seed)
    except FileNotFoundError:
        log.info(f"No identity at {identity_path} — generating a new one")
        identity = LocalIdentity()
        seed = identity.get_signing_key_bytes()
        # Atomic write.
        tmp = identity_path + ".tmp"
        with open(tmp, "wb") as f:
            f.write(seed)
        os.replace(tmp, identity_path)
        os.chmod(identity_path, 0o600)
        log.info(f"Saved identity to {identity_path}")
        return identity


async def run(config: dict) -> None:
    try:
        from openhop_core import LocalIdentity
        from openhop_core.companion import CompanionFrameServer, CompanionRadio
        from openhop_core.hardware.sx1262_wrapper import SX1262Radio
    except ImportError as e:
        log.error(
            f"openhop_core is not installed: {e}\n"
            "Install with: pip install openhop-core"
        )
        sys.exit(1)

    radio_cfg = config["radio"]
    companion_cfg = config.get("companion", {})

    # ── Radio ──────────────────────────────────────────────────────────────────

    freq_hz = radio_cfg["frequency"]
    log.info(
        f"Initialising SX1262 radio "
        f"(bus={radio_cfg.get('bus_id', 0)}, "
        f"cs={radio_cfg.get('cs_pin', -1)}, "
        f"freq={freq_hz / 1_000_000:.3f} MHz, "
        f"tx_power={radio_cfg.get('tx_power', 22)} dBm)"
    )

    radio_kwargs = {
        "bus_id":           int(radio_cfg.get("bus_id", 0)),
        "cs_pin":           int(radio_cfg.get("cs_pin", -1)),
        "reset_pin":        int(radio_cfg["reset_pin"]),
        "busy_pin":         int(radio_cfg["busy_pin"]),
        "irq_pin":          int(radio_cfg["irq_pin"]),
        "txen_pin":         int(radio_cfg.get("txen_pin", -1)),
        "rxen_pin":         int(radio_cfg.get("rxen_pin", -1)),
        "frequency":        int(freq_hz),
        "bandwidth":        int(radio_cfg.get("bandwidth", 62500)),
        "spreading_factor": int(radio_cfg.get("spreading_factor", 7)),
        "coding_rate":      int(radio_cfg.get("coding_rate", 5)),
        "tx_power":         int(radio_cfg.get("tx_power", 22)),
        "preamble_length":  int(radio_cfg.get("preamble_length", 17)),
        "sync_word":        int(radio_cfg.get("sync_word", 0x3444)),
        "use_dio2_rf":      bool(radio_cfg.get("use_dio2_rf", False)),
        "use_dio3_tcxo":    bool(radio_cfg.get("use_dio3_tcxo", False)),
    }
    if "dio3_tcxo_voltage" in radio_cfg:
        radio_kwargs["dio3_tcxo_voltage"] = float(radio_cfg["dio3_tcxo_voltage"])
    for _opt_int in ("gpio_chip", "cs_id", "en_pin"):
        if _opt_int in radio_cfg:
            radio_kwargs[_opt_int] = int(radio_cfg[_opt_int])
    # YAML key names (tx_led/rx_led) predate and differ from SX1262Radio's
    # real constructor parameter names (txled_pin/rxled_pin) — kept as-is on
    # the config side for stability, translated here.
    for _yaml_key, _kwarg_name in (("tx_led", "txled_pin"), ("rx_led", "rxled_pin")):
        if _yaml_key in radio_cfg:
            radio_kwargs[_kwarg_name] = int(radio_cfg[_yaml_key])
    if radio_cfg.get("use_gpiod_backend"):
        radio_kwargs["use_gpiod_backend"] = True

    radio = SX1262Radio(**radio_kwargs)
    # begin() signals failure two different ways depending on where it
    # fails: a plain `False` return for some paths, but an exception (e.g.
    # RuntimeError from IRQ-pin setup) for others — both are the same
    # class of problem from an operator's perspective (wiring/SPI
    # misconfiguration), so both get the same actionable hint instead of
    # only the `False` case getting it and the other showing a bare
    # traceback. log.exception still includes the traceback, just with
    # the hint prepended.
    try:
        began = radio.begin()
    except Exception:
        log.exception("SX1262Radio.begin() raised an exception — check wiring and SPI settings")
        sys.exit(1)
    if began is False:
        log.error("SX1262Radio.begin() returned False — check wiring and SPI settings")
        sys.exit(1)
    log.info("Radio initialised")

    # ── Identity ───────────────────────────────────────────────────────────────

    identity = load_or_create_identity(
        LocalIdentity, companion_cfg.get("identity_path")
    )
    pubkey = identity.get_public_key()
    log.info(f"Public key: {pubkey.hex()}")

    # ── CompanionRadio ─────────────────────────────────────────────────────────

    node_name = companion_cfg.get("node_name", "Supply Drop BBS")
    # adv_type controls what node type is advertised on the mesh.
    # 1=Chat, 2=Repeater, 3=Room (BBS), 4=Sensor. Default: 3 (Room/BBS).
    adv_type_val = int(companion_cfg.get("adv_type", 3))
    radio_params = {
        "frequency":        radio_kwargs["frequency"],
        "bandwidth":        radio_kwargs["bandwidth"],
        "spreading_factor": radio_kwargs["spreading_factor"],
        "coding_rate":      radio_kwargs["coding_rate"],
        "tx_power":         radio_kwargs["tx_power"],
    }
    companion = CompanionRadio(
        radio, identity, node_name=node_name, adv_type=adv_type_val,
        radio_config=radio_params
    )
    _type_names = {1: "Chat", 2: "Repeater", 3: "Room", 4: "Sensor"}
    log.info(f"Advertising as adv_type={adv_type_val} ({_type_names.get(adv_type_val, 'unknown')})")

    # ── Restore persisted contacts ────────────────────────────────────────────

    contacts_db_path = companion_cfg.get(
        "contacts_db_path", "/var/lib/supply-drop-bbs/pymc-companion-contacts.db"
    )
    try:
        persisted_contacts = load_contacts_db(contacts_db_path)
    except Exception as e:
        log.error(
            f"Could not load persisted contacts from {contacts_db_path}: {e}\n"
            "Refusing to start with an empty contact store — a corrupt or "
            "unreadable file is not the same as having no contacts, and "
            "continuing would risk the next autosave overwriting recoverable "
            "data with nothing. Fix or remove the file, then restart."
        )
        sys.exit(1)
    if persisted_contacts:
        companion.contacts.load_from_dicts(persisted_contacts)
        log.info(
            f"Restored {len(persisted_contacts)} persisted contact(s) "
            f"from {contacts_db_path}"
        )

    # Auto-add contacts so the BBS sees incoming users without manual approval.
    # 0x01 = overwrite oldest, 0x02 = chat, 0x04 = repeater, 0x08 = room,
    # 0x10 = sensor. Default 0x0F does NOT include sensor — use 0x1F to
    # also auto-add sensor contacts.
    autoadd = int(companion_cfg.get("autoadd_config", 0x0F))
    try:
        companion.set_autoadd_config(autoadd)
    except AttributeError:
        try:
            companion.prefs.autoadd_config = autoadd
        except Exception as e:
            log.warning(f"Could not set autoadd_config: {e}")

    await companion.start()
    log.info("CompanionRadio started")

    # ── CompanionFrameServer ───────────────────────────────────────────────────

    host = companion_cfg.get("bind_address", "127.0.0.1")
    port = int(companion_cfg.get("tcp_port", 5000))
    companion_hash = f"{pubkey[0]:02x}"

    server = CompanionFrameServer(
        bridge=companion,
        companion_hash=companion_hash,
        port=port,
        bind_address=host,
    )
    await server.start()
    log.info(f"Companion frame server listening on {host}:{port}")

    # ── Periodic contact autosave ──────────────────────────────────────────────
    #
    # Safety net for an unclean shutdown (crash, `kill -9`, power loss — a real
    # risk on Pi HAT hardware) that the graceful shutdown-time save below can't
    # cover. 60s balances write frequency against how much could be lost in a
    # crash between saves; not configurable since neither extreme (near-zero
    # loss window vs. near-zero write overhead) matters enough here to expose.

    _CONTACTS_AUTOSAVE_INTERVAL_SECS = 60

    async def _autosave_contacts_loop() -> None:
        while True:
            await asyncio.sleep(_CONTACTS_AUTOSAVE_INTERVAL_SECS)
            try:
                # to_dicts() itself stays on the event loop -- it's a fast,
                # synchronous read of in-memory state with no I/O, and doing
                # it here (rather than inside the worker thread) means the
                # snapshot it takes can never race a concurrent mutation from
                # elsewhere on this same single-threaded loop. Only the slow
                # part (writing the snapshot to disk) moves to a thread.
                records = companion.contacts.to_dicts()
                await asyncio.to_thread(save_contacts_db, contacts_db_path, records)
            except Exception as e:
                log.error(f"Failed to autosave contacts to {contacts_db_path}: {e}")

    autosave_task = asyncio.create_task(_autosave_contacts_loop())

    # ── Run until signal ───────────────────────────────────────────────────────

    loop = asyncio.get_running_loop()
    stop: asyncio.Future = loop.create_future()
    for sig in (signal.SIGTERM, signal.SIGINT):
        loop.add_signal_handler(sig, lambda: stop.set_result(None))

    await stop
    log.info("Shutdown signal received")

    autosave_task.cancel()
    try:
        records = companion.contacts.to_dicts()
        await asyncio.to_thread(save_contacts_db, contacts_db_path, records)
        log.info(
            f"Saved {companion.contacts.get_count()} contact(s) "
            f"to {contacts_db_path}"
        )
    except Exception as e:
        log.error(f"Failed to save contacts on shutdown: {e}")

    await server.stop()
    await companion.stop()
    log.info("pymc-companion stopped")


def main() -> None:
    parser = argparse.ArgumentParser(
        description="openhop_core companion frame server for supply-drop-bbs"
    )
    parser.add_argument("--config", required=True, help="Path to YAML config file")
    parser.add_argument("--log-level", default="INFO", help="Log level (default: INFO)")
    args = parser.parse_args()

    logging.basicConfig(
        level=getattr(logging, args.log_level.upper(), logging.INFO),
        format="%(asctime)s %(name)-20s %(levelname)-8s %(message)s",
    )

    config = load_config(args.config)
    asyncio.run(run(config))


if __name__ == "__main__":
    main()
