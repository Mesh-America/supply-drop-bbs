#!/usr/bin/env python3
"""
pymc-companion — minimal pymc_core companion frame server for supply-drop-bbs.

Drives a LoRa HAT directly via SPI using pymc_core and exposes the MeshCore
companion frame protocol on a TCP port so supply-drop-bbs can connect to it.

This is the same approach as mesh-citadel's HatRuntime — pymc_core runs
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
import sys

import yaml

log = logging.getLogger("pymc-companion")


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
        from pymc_core import LocalIdentity
        from pymc_core.companion import CompanionFrameServer, CompanionRadio
        from pymc_core.hardware.sx1262_wrapper import SX1262Radio
    except ImportError as e:
        log.error(
            f"pymc_core is not installed: {e}\n"
            "Install with: pip install pymc_core"
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
    for _opt_int in ("gpio_chip", "cs_id", "en_pin", "tx_led", "rx_led"):
        if _opt_int in radio_cfg:
            radio_kwargs[_opt_int] = int(radio_cfg[_opt_int])
    if radio_cfg.get("use_gpiod_backend"):
        radio_kwargs["use_gpiod_backend"] = True

    radio = SX1262Radio(**radio_kwargs)
    if radio.begin() is False:
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
    radio_params = {
        "frequency":        radio_kwargs["frequency"],
        "bandwidth":        radio_kwargs["bandwidth"],
        "spreading_factor": radio_kwargs["spreading_factor"],
        "coding_rate":      radio_kwargs["coding_rate"],
        "tx_power":         radio_kwargs["tx_power"],
    }
    companion = CompanionRadio(
        radio, identity, node_name=node_name, radio_config=radio_params
    )

    # Auto-add contacts so the BBS sees incoming users without manual approval.
    # 0x01 = overwrite oldest, 0x02 = chat, 0x04 = repeater, 0x08 = room
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

    # ── Run until signal ───────────────────────────────────────────────────────

    loop = asyncio.get_running_loop()
    stop: asyncio.Future = loop.create_future()
    for sig in (signal.SIGTERM, signal.SIGINT):
        loop.add_signal_handler(sig, lambda: stop.set_result(None))

    await stop
    log.info("Shutdown signal received")

    await server.stop()
    await companion.stop()
    log.info("pymc-companion stopped")


def main() -> None:
    parser = argparse.ArgumentParser(
        description="pymc_core companion frame server for supply-drop-bbs"
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
