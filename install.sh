#!/usr/bin/env bash
# Supply Drop BBS installer
#
# Usage:
#   curl -sSL https://raw.githubusercontent.com/Mesh-America/supply-drop-bbs/main/install.sh | sudo bash
#
# Or download and run directly:
#   sudo bash install.sh

set -euo pipefail

# ── When piped through curl, stdin is the script itself, not the terminal.
# Re-exec from a temp file so interactive prompts work.
if [ ! -t 0 ]; then
    tmp=$(mktemp /tmp/supply-drop-install-XXXXXX.sh)
    trap 'rm -f "$tmp"' EXIT
    cat > "$tmp"
    exec bash "$tmp" "$@"
fi

# ── Config ────────────────────────────────────────────────────────────────────

REPO="https://github.com/Mesh-America/supply-drop-bbs.git"
SRC_DIR="/opt/supply-drop-bbs"
BIN_PATH="/usr/local/bin/supply-drop-bbs"
SERVICE_USER="supply-drop"
CONFIG_DIR="/etc/supply-drop-bbs"
DATA_DIR="/var/lib/supply-drop-bbs"
UNIT_FILE="/etc/systemd/system/supply-drop-bbs.service"

# ── Colours ───────────────────────────────────────────────────────────────────

RED='\033[0;31m'; GREEN='\033[0;32m'; YELLOW='\033[1;33m'; BLUE='\033[0;34m'; NC='\033[0m'
info()    { echo -e "${BLUE}  →${NC} $*"; }
success() { echo -e "${GREEN}  ✓${NC} $*"; }
warn()    { echo -e "${YELLOW}  !${NC} $*"; }
die()     { echo -e "${RED}  ✗${NC} $*" >&2; exit 1; }

# ── Banner ────────────────────────────────────────────────────────────────────

echo
echo "╔══════════════════════════════════════════════════╗"
echo "║         Supply Drop BBS — Installer              ║"
echo "╚══════════════════════════════════════════════════╝"
echo

# ── Root check ────────────────────────────────────────────────────────────────

[[ $EUID -eq 0 ]] || die "Please run with sudo:  curl ... | sudo bash"

# ── OS check ─────────────────────────────────────────────────────────────────

if [[ -f /etc/os-release ]]; then
    . /etc/os-release
    if [[ "$ID" != "raspbian" && "${ID_LIKE:-}" != *"debian"* && "$ID" != "debian" && "$ID" != "ubuntu" ]]; then
        warn "This installer targets Raspberry Pi OS / Debian / Ubuntu."
        warn "Proceeding anyway — you may need to adjust package names."
    fi
else
    warn "Cannot detect OS — proceeding anyway."
fi

# ── System packages ───────────────────────────────────────────────────────────

info "Installing system dependencies..."
apt-get update -qq
apt-get install -y -qq \
    build-essential curl git pkg-config libssl-dev \
    nodejs npm
success "System dependencies installed"

# ── Rust ─────────────────────────────────────────────────────────────────────

if command -v cargo &>/dev/null; then
    success "Rust already installed ($(cargo --version))"
else
    info "Installing Rust (this is quick)..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
        | sh -s -- -y --profile minimal --no-modify-path
    export PATH="$HOME/.cargo/bin:$PATH"
    success "Rust installed"
fi

# Make sure cargo is on PATH for the rest of the script
export PATH="${HOME}/.cargo/bin:/root/.cargo/bin:$PATH"
command -v cargo &>/dev/null || die "cargo not found after install — open a new shell and re-run."

# ── Clone or update source ────────────────────────────────────────────────────

if [[ -d "$SRC_DIR/.git" ]]; then
    info "Updating Supply Drop BBS source..."
    git -C "$SRC_DIR" pull --ff-only
    success "Source updated"
else
    info "Cloning Supply Drop BBS..."
    git clone --depth 1 "$REPO" "$SRC_DIR"
    success "Source cloned to $SRC_DIR"
fi

# ── Build ─────────────────────────────────────────────────────────────────────

echo
echo "  Building Supply Drop BBS."
echo "  This takes 5–15 minutes on a Pi — please wait."
echo
info "Running cargo build --release..."
cargo build --release --manifest-path "$SRC_DIR/Cargo.toml"
success "Build complete"

# ── Install binary ────────────────────────────────────────────────────────────

info "Installing binary to $BIN_PATH..."
install -m 755 "$SRC_DIR/target/release/supply-drop-bbs" "$BIN_PATH"
success "Binary installed"

# ── Service user ──────────────────────────────────────────────────────────────

if id -u "$SERVICE_USER" &>/dev/null; then
    success "Service user '$SERVICE_USER' already exists"
else
    info "Creating service user '$SERVICE_USER'..."
    useradd --system --no-create-home --shell /usr/sbin/nologin "$SERVICE_USER"
    success "Service user created"
fi

# Add service user to dialout for serial port access
usermod -aG dialout "$SERVICE_USER"

# ── Directories ───────────────────────────────────────────────────────────────

mkdir -p "$CONFIG_DIR" "$DATA_DIR"
chown "$SERVICE_USER:$SERVICE_USER" "$DATA_DIR"
# Config dir stays root-owned but readable by the service user
chmod 755 "$CONFIG_DIR"

success "Directories created"

# ── Systemd unit ─────────────────────────────────────────────────────────────

info "Installing systemd unit..."
install -m 644 "$SRC_DIR/supply-drop-bbs.service" "$UNIT_FILE"
systemctl daemon-reload
success "Systemd unit installed"

# ── Setup wizard ──────────────────────────────────────────────────────────────

run_setup=true
if [[ -f "$CONFIG_DIR/config.toml" ]]; then
    echo
    warn "A config file already exists at $CONFIG_DIR/config.toml"
    echo
    read -r -p "  Reconfigure now? [Y/n] " _reconfigure
    _reconfigure="${_reconfigure:-Y}"
    if [[ ! "$_reconfigure" =~ ^[Yy] ]]; then
        run_setup=false
    fi
fi

if [[ "$run_setup" == true ]]; then
    # Stop the service while reconfiguring so the new config is picked up cleanly.
    if systemctl is-active --quiet supply-drop-bbs 2>/dev/null; then
        info "Stopping supply-drop-bbs for reconfiguration..."
        systemctl stop supply-drop-bbs
    fi

    echo
    echo "─── Setup ─────────────────────────────────────────────────────────────────"
    echo
    "$BIN_PATH" setup --config "$CONFIG_DIR/config.toml"
    chown "root:$SERVICE_USER" "$CONFIG_DIR/config.toml"
    chmod 640 "$CONFIG_DIR/config.toml"
fi

# ── Enable and start ──────────────────────────────────────────────────────────

systemctl enable --now supply-drop-bbs

echo
echo "╔══════════════════════════════════════════════════╗"
echo "║          Supply Drop BBS is running!             ║"
echo "╚══════════════════════════════════════════════════╝"
echo
# Show web admin URL only if the config has it enabled
if grep -q 'enabled = true' "$CONFIG_DIR/config.toml" 2>/dev/null || \
   ! grep -q 'enabled = false' "$CONFIG_DIR/config.toml" 2>/dev/null; then
    _bind=$(grep -A5 '\[plugins.web\]' "$CONFIG_DIR/config.toml" 2>/dev/null | grep '^bind' | cut -d'"' -f2 || true)
    _bind="${_bind:-0.0.0.0:8080}"
    _port="${_bind##*:}"
    echo "  Web admin:  http://$(hostname -I | awk '{print $1}'):${_port}"
fi
echo "  Logs:       journalctl -u supply-drop-bbs -f"
echo "  Config:     $CONFIG_DIR/config.toml"
echo "  Reconfigure: sudo bash $SRC_DIR/install.sh"
echo "  Restart:    sudo systemctl restart supply-drop-bbs"
echo
