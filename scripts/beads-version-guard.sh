#!/usr/bin/env bash
# beads-version-guard.sh — refuse any bd operation if this machine's bd doesn't match the fleet pin.
#
# Why this exists (2026-09-23): the generic beads install script always fetches "latest" and was
# silently re-run on this machine, replacing a pinned bd version with a newer one that wanted to
# migrate this project's shared, remote-backed Dolt database (refs/dolt/data on the git remote).
# Embedded mode has no push-time gate the way a shared dolt sql-server does, so the newer binary's
# very first `bd` invocation auto-migrated the LOCAL database in place with no confirmation step —
# and a later `bd dolt push` (auto or explicit) can carry that migrated schema to every other
# teammate's clone before anyone knows it happened. This project's remote is already on the new
# schema as of that incident; every clone now needs the same bd version or its own `bd dolt
# pull`/`push` will be refused. See the bd 1.3.0 release notes ("Upgrading Notes" / "Breaking
# changes") for the full designated-migrator recipe if the pin ever needs to move again.
#
# Run this before any bd operation that talks to the remote (dolt push/pull, bootstrap, migrate).
# Exit codes: 0 match · 1 mismatch or bd missing (refuses) · 2 no pin file (warns, does not refuse).
set -uo pipefail

if ! command -v bd >/dev/null 2>&1; then echo "beads-version-guard: bd is not installed" >&2; exit 1; fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PIN_FILE="$SCRIPT_DIR/../.beads/PINNED_BD_VERSION"

if [ ! -f "$PIN_FILE" ]; then
  echo "beads-version-guard: no $PIN_FILE -- SKIPPED (nothing to check against)." >&2
  exit 2
fi

pinned="$(tr -d '[:space:]' < "$PIN_FILE")"
actual="$(bd version 2>&1 | grep -oE '[0-9]+\.[0-9]+\.[0-9]+' | head -1)"

if [ -z "$actual" ]; then
  echo "beads-version-guard: couldn't parse 'bd version' output -- refusing. Compare by hand to $PIN_FILE ($pinned)." >&2
  exit 1
fi

if [ "$actual" != "$pinned" ]; then
  echo "beads-version-guard: bd is $actual, this project is pinned to $pinned (see $PIN_FILE). Do NOT run 'bd migrate', 'bd dolt pull/push', 'bd bootstrap', or any other bd command that touches the remote until every clone using this database is on the same version, or you risk forking its schema." >&2
  echo "                     Reinstall the pinned version from a verified GitHub release (never the generic install script, which always grabs latest):" >&2
  echo "                       gh release download v$pinned --repo gastownhall/beads -p 'beads_${pinned}_<platform>.tar.gz' -p checksums.txt -D /tmp/bd-pin --clobber   # .zip + sha256sum on Windows" >&2
  echo "                       cd /tmp/bd-pin && shasum -a 256 -c <(grep '<platform>' checksums.txt) && tar xzf 'beads_${pinned}_<platform>.tar.gz'" >&2
  echo "                       cp bd \"\$(command -v bd)\" && chmod +x \"\$(command -v bd)\" && bd version" >&2
  exit 1
fi

echo "beads-version-guard: bd $actual matches the pin ($PIN_FILE)."
