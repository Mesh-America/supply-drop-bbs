#!/usr/bin/env bash
# Pre-push quality gate — mirrors every step CI runs.
#
# Run this before every push:
#   ./scripts/check.sh
#
# Exit code 0 means CI will pass (on native targets).
# Cross-compilation is not checked here; CI handles that.

set -euo pipefail

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m'

pass() { echo -e "${GREEN}✓${NC} $1"; }
fail() { echo -e "${RED}✗${NC} $1"; exit 1; }
step() { echo -e "\n${YELLOW}▶${NC} $1"; }

# ── 1. Format ─────────────────────────────────────────────────────────────────
step "Format check"
cargo fmt --all -- --check && pass "cargo fmt" || fail "cargo fmt --all -- --check failed. Run: cargo fmt --all"

# ── 2. Clippy — default features ──────────────────────────────────────────────
step "Clippy (default features)"
cargo clippy --workspace --all-targets -- -D warnings && pass "clippy default" || fail "clippy default failed"

# ── 3. Clippy — all features ──────────────────────────────────────────────────
step "Clippy (all features)"
cargo clippy --workspace --all-targets --all-features -- -D warnings && pass "clippy all-features" || fail "clippy all-features failed"

# ── 4. Build — default features ───────────────────────────────────────────────
step "Build (default features)"
cargo build --workspace && pass "build default" || fail "build default failed"

# ── 5. Build — all features ───────────────────────────────────────────────────
step "Build (all features)"
cargo build --workspace --all-features && pass "build all-features" || fail "build all-features failed"

# ── 6. Build — headless (mirrors cross-build CI step) ─────────────────────────
step "Build (headless: --no-default-features --features transport-cli)"
cargo build --no-default-features --features transport-cli && pass "build headless" || fail "build headless failed"

# ── 7. Tests — default features ───────────────────────────────────────────────
step "Tests (default features)"
cargo test --workspace && pass "test default" || fail "test default failed"

# ── 8. Tests — all features ───────────────────────────────────────────────────
step "Tests (all features)"
cargo test --workspace --all-features && pass "test all-features" || fail "test all-features failed"

# ── 9. Docs ───────────────────────────────────────────────────────────────────
step "Docs (--all-features)"
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --all-features && pass "rustdoc" || fail "rustdoc failed"

echo -e "\n${GREEN}All checks passed — safe to push.${NC}\n"
