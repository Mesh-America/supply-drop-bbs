#!/usr/bin/env bash
# Supply Drop BBS installer
#
# Usage:
#   curl -sSL https://raw.githubusercontent.com/Mesh-America/supply-drop-bbs/main/install.sh | sudo bash
#
# Or download and run directly:
#   sudo bash install.sh
#
# To uninstall:
#   sudo bash install.sh --uninstall

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
GITHUB_API="https://api.github.com/repos/Mesh-America/supply-drop-bbs"
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

# ── Uninstaller ───────────────────────────────────────────────────────────────

if [[ "${1:-}" == "--uninstall" ]]; then
    echo
    if command -v figlet &>/dev/null; then
        echo -e "${GREEN}$(figlet "Supply Drop" 2>/dev/null || echo "  Supply Drop")${NC}"
        echo -e "${GREEN}$(figlet "  BBS" 2>/dev/null || echo "  BBS")${NC}"
    else
        echo -e "${GREEN}  Supply Drop BBS${NC}"
    fi
    echo -e "  ${GREEN}uninstaller${NC}"
    echo

    [[ $EUID -eq 0 ]] || die "Please run with sudo:  sudo bash install.sh --uninstall"

    warn "This will remove Supply Drop BBS and pymc-companion (MeshCore HAT bridge) from this system."
    echo
    read -r -p "  Continue? [y/N] " _confirm
    [[ "$_confirm" =~ ^[Yy] ]] || { echo "  Aborted."; exit 0; }
    echo

    # Stop and disable services.
    for _svc in supply-drop-bbs pymc-companion; do
        if systemctl is-active --quiet "$_svc" 2>/dev/null; then
            info "Stopping $_svc..."
            systemctl stop "$_svc"
        fi
        if systemctl is-enabled --quiet "$_svc" 2>/dev/null; then
            info "Disabling $_svc..."
            systemctl disable "$_svc"
        fi
    done

    # Remove service unit files.
    for _unit in \
        /etc/systemd/system/supply-drop-bbs.service \
        /etc/systemd/system/pymc-companion.service; do
        if [[ -f "$_unit" ]]; then
            rm -f "$_unit"
            success "Removed $_unit"
        fi
    done
    systemctl daemon-reload

    # Remove sudoers rule.
    rm -f /etc/sudoers.d/supply-drop-bbs && success "Removed /etc/sudoers.d/supply-drop-bbs"

    # Remove binary and source.
    rm -f "$BIN_PATH" && success "Removed $BIN_PATH"
    rm -rf /opt/pymc-companion && success "Removed /opt/pymc-companion"
    rm -rf "$SRC_DIR"          && success "Removed $SRC_DIR"

    # Config directory — ask before deleting.
    if [[ -d "$CONFIG_DIR" ]]; then
        echo
        read -r -p "  Delete config directory $CONFIG_DIR? [y/N] " _del_cfg
        if [[ "$_del_cfg" =~ ^[Yy] ]]; then
            rm -rf "$CONFIG_DIR"
            success "Removed $CONFIG_DIR"
        else
            info "Kept $CONFIG_DIR"
        fi
    fi

    # Data directory — ask before deleting (contains message store).
    if [[ -d "$DATA_DIR" ]]; then
        echo
        warn "The data directory contains your message store and identity key."
        read -r -p "  Delete data directory $DATA_DIR? [y/N] " _del_data
        if [[ "$_del_data" =~ ^[Yy] ]]; then
            rm -rf "$DATA_DIR"
            success "Removed $DATA_DIR"
        else
            info "Kept $DATA_DIR"
        fi
    fi

    # Service user — ask before removing.
    if id -u "$SERVICE_USER" &>/dev/null; then
        echo
        read -r -p "  Remove service user '$SERVICE_USER'? [y/N] " _del_user
        if [[ "$_del_user" =~ ^[Yy] ]]; then
            userdel "$SERVICE_USER"
            success "Removed user '$SERVICE_USER'"
        else
            info "Kept user '$SERVICE_USER'"
        fi
    fi

    echo
    success "Supply Drop BBS uninstalled."
    echo
    exit 0
fi

# ── Banner ────────────────────────────────────────────────────────────────────

print_banner() {
    echo
    if command -v figlet &>/dev/null; then
        echo -e "${GREEN}$(figlet "Supply Drop" 2>/dev/null || echo "  Supply Drop")${NC}"
        echo -e "${GREEN}$(figlet "  BBS" 2>/dev/null || echo "  BBS")${NC}"
    else
        echo -e "${GREEN}   _____                   __         ____"
        echo    "  / ___/__  ______  ____  / /_  __   / __ \________  ___  ____"
        echo    "  \__ \/ / / / __ \/ __ \/ / / / /  / / / / ___/ _ \/ _ \/ __ \\"
        echo    " ___/ / /_/ / /_/ / /_/ / / /_/ /  / /_/ / /  /  __/  __/ /_/ /"
        echo    "/____/\__,_/ .___/ .___/_/\__, /  /_____/_/   \___/\___/ .___/"
        echo    "          /_/   /_/       /____/                        /_/"
        echo    ""
        echo    "                        ____  ____  _____"
        echo    "                       / __ )/ __ )/ ___/"
        echo    "                      / __  / __  /\__ \\"
        echo    "                     / /_/ / /_/ /___/ /"
        echo -e "                    /_____/_____//____/${NC}"
        echo
    fi
    echo -e "  ${GREEN}mesh radio bulletin board system${NC}"
    echo
}

print_banner

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

# ── Minimal system packages (always needed) ───────────────────────────────────
# curl and git are required regardless of install method.
# figlet is optional (banner only) — failure is fine.

info "Installing base dependencies..."
apt-get update -qq
apt-get install -y -qq curl git figlet 2>/dev/null || \
    apt-get install -y -qq curl git
success "Base dependencies installed"

# ── Clone or update source ────────────────────────────────────────────────────
# Always clone/pull — we need the source tree for:
#   • systemd unit files
#   • pymc-companion scripts and service file
# Even when installing a pre-built binary we still want these up to date.

if [[ -d "$SRC_DIR/.git" ]]; then
    info "Updating Supply Drop BBS source..."
    git -C "$SRC_DIR" pull --ff-only
    success "Source updated"
else
    info "Cloning Supply Drop BBS..."
    git clone --depth 1 "$REPO" "$SRC_DIR"
    success "Source cloned to $SRC_DIR"
fi

# ── Try pre-built binary download ─────────────────────────────────────────────
# Maps uname -m → GitHub release target triple.
# Downloads the binary and verifies its SHA256 checksum.
# Returns 0 on success (binary installed), 1 to fall back to source build.

_installed_from_binary=false

try_download_binary() {
    local arch
    arch=$(uname -m)

    local target
    case "$arch" in
        aarch64)       target="aarch64-unknown-linux-gnu" ;;
        armv7l|armv7)  target="armv7-unknown-linux-gnueabihf" ;;
        x86_64)        target="x86_64-unknown-linux-gnu" ;;
        *)
            warn "No pre-built binary available for arch '$arch'."
            return 1
            ;;
    esac

    info "Checking GitHub releases for a pre-built binary ($arch → $target)..."

    # Fetch latest release metadata from the GitHub API.
    local release_json
    if ! release_json=$(curl -sSf --max-time 15 "${GITHUB_API}/releases/latest" 2>/dev/null); then
        warn "Could not reach GitHub — will build from source."
        return 1
    fi

    # Parse the tag name using python3 (always available on Raspberry Pi OS).
    local tag
    tag=$(echo "$release_json" | python3 -c \
        "import sys,json; d=json.load(sys.stdin); print(d['tag_name'])" 2>/dev/null) || true
    if [[ -z "$tag" ]]; then
        warn "Could not parse release tag — will build from source."
        return 1
    fi

    local binary_name="supply-drop-bbs-${tag}-${target}"

    # Find the download URL for this binary in the release assets.
    local binary_url
    binary_url=$(echo "$release_json" | python3 -c "
import sys, json
d = json.load(sys.stdin)
name = sys.argv[1]
for a in d.get('assets', []):
    if a['name'] == name:
        print(a['browser_download_url'])
        break
" "$binary_name" 2>/dev/null) || true

    if [[ -z "$binary_url" ]]; then
        warn "No pre-built binary for $target in release $tag — will build from source."
        return 1
    fi

    # Also find the SHA256SUMS asset URL.
    local sums_url
    sums_url=$(echo "$release_json" | python3 -c "
import sys, json
d = json.load(sys.stdin)
for a in d.get('assets', []):
    if a['name'] == 'SHA256SUMS':
        print(a['browser_download_url'])
        break
" 2>/dev/null) || true

    # Download the binary to a temp file.
    info "Downloading $binary_name ($tag)..."
    local tmp_bin
    tmp_bin=$(mktemp /tmp/supply-drop-bin-XXXXXX)
    # shellcheck disable=SC2064
    trap "rm -f '$tmp_bin'" RETURN

    if ! curl -sSfL --max-time 120 --progress-bar "$binary_url" -o "$tmp_bin"; then
        warn "Download failed — will build from source."
        rm -f "$tmp_bin"
        return 1
    fi

    # Verify SHA256 checksum if the sums file is available.
    if [[ -n "$sums_url" ]]; then
        local tmp_sums
        tmp_sums=$(mktemp /tmp/supply-drop-sums-XXXXXX)
        # shellcheck disable=SC2064
        trap "rm -f '$tmp_bin' '$tmp_sums'" RETURN

        if curl -sSfL --max-time 15 "$sums_url" -o "$tmp_sums" 2>/dev/null; then
            info "Verifying checksum..."
            local expected actual
            expected=$(awk -v n="$binary_name" '$2 == n || $2 == "*"n {print $1}' "$tmp_sums")
            actual=$(sha256sum "$tmp_bin" | awk '{print $1}')
            rm -f "$tmp_sums"

            if [[ -z "$expected" ]]; then
                warn "Binary not listed in SHA256SUMS — skipping verification."
            elif [[ "$expected" != "$actual" ]]; then
                warn "Checksum mismatch! (expected: $expected, got: $actual)"
                warn "Refusing to install a corrupted binary — will build from source."
                rm -f "$tmp_bin"
                return 1
            else
                success "Checksum verified"
            fi
        else
            warn "Could not fetch SHA256SUMS — skipping checksum verification."
        fi
    fi

    # Install the verified binary.
    install -m 755 "$tmp_bin" "$BIN_PATH"
    rm -f "$tmp_bin"
    success "Installed pre-built binary $tag"
    return 0
}

echo
echo "─── Binary acquisition ─────────────────────────────────────────────────────"
echo

if try_download_binary; then
    _installed_from_binary=true
else
    # ── Fallback: build from source ───────────────────────────────────────────

    echo
    echo "  Falling back to building from source."
    echo "  This takes 5–15 minutes on a Pi — please wait."
    echo

    # Build-only system packages.
    info "Installing build dependencies..."
    apt-get install -y -qq \
        build-essential pkg-config libssl-dev \
        nodejs npm
    success "Build dependencies installed"

    # Rust.
    if command -v cargo &>/dev/null; then
        success "Rust already installed ($(cargo --version))"
    else
        info "Installing Rust (this is quick)..."
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
            | sh -s -- -y --profile minimal --no-modify-path
        export PATH="$HOME/.cargo/bin:$PATH"
        success "Rust installed"
    fi

    # Make sure cargo is on PATH for the rest of the script.
    export PATH="${HOME}/.cargo/bin:/root/.cargo/bin:$PATH"
    command -v cargo &>/dev/null || \
        die "cargo not found after install — open a new shell and re-run."

    info "Running cargo build --release..."
    cargo build --release --manifest-path "$SRC_DIR/Cargo.toml"
    success "Build complete"

    info "Installing binary to $BIN_PATH..."
    install -m 755 "$SRC_DIR/target/release/supply-drop-bbs" "$BIN_PATH"
    success "Binary installed"
fi

# ── Service user ──────────────────────────────────────────────────────────────

if id -u "$SERVICE_USER" &>/dev/null; then
    success "Service user '$SERVICE_USER' already exists"
else
    info "Creating service user '$SERVICE_USER'..."
    useradd --system --no-create-home --shell /usr/sbin/nologin "$SERVICE_USER"
    success "Service user created"
fi

# Add service user to dialout for serial port access.
usermod -aG dialout "$SERVICE_USER"

# ── Directories ───────────────────────────────────────────────────────────────

mkdir -p "$CONFIG_DIR" "$CONFIG_DIR/plugins.d" "$DATA_DIR"
chown "$SERVICE_USER:$SERVICE_USER" "$DATA_DIR"
# Config dir stays root-owned but readable by the service user.
# plugins.d is writable by root only; the BBS reads it as the service user.
chmod 755 "$CONFIG_DIR" "$CONFIG_DIR/plugins.d"

success "Directories created"

# ── Systemd unit ─────────────────────────────────────────────────────────────

info "Installing systemd unit..."
install -m 644 "$SRC_DIR/supply-drop-bbs.service" "$UNIT_FILE"
systemctl daemon-reload
success "Systemd unit installed"

# ── Web-UI service restart ────────────────────────────────────────────────────
# No sudoers rule is needed: the unit runs with NoNewPrivileges=yes (so `sudo`
# can't escalate anyway), and the web-UI "restart service" simply exits the
# process — systemd's Restart=always then starts a fresh instance. Remove any
# stale sudoers rule from older installs.
rm -f /etc/sudoers.d/supply-drop-bbs

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
    # Service user owns the config so the web UI can write it.
    # The directory stays root-owned so only root can add/remove files.
    chown "$SERVICE_USER:$SERVICE_USER" "$CONFIG_DIR/config.toml"
    chmod 640 "$CONFIG_DIR/config.toml"
fi

# ── Protocol-specific radio setup ────────────────────────────────────────────
#
# Each protocol is independent.  MeshCore Pi HAT needs pymc-companion;
# all other connection types (serial, tcp) and Meshtastic need nothing extra.
#
# We read the config that the setup wizard just wrote to determine what the
# operator enabled.  A section is "enabled" if its `enabled` key is absent
# (defaults to true for mesh) or explicitly set to `true`.

_mesh_enabled=false
_mesh_conn_type=""
_meshtastic_enabled=false

# Parse [plugins.mesh] — enabled defaults to true if key is absent.
if python3 - "$CONFIG_DIR/config.toml" <<'PYEOF'
import sys, re

path = sys.argv[1]
text = open(path).read()

# Find [plugins.mesh] section.
m = re.search(r'^\[plugins\.mesh\](.*?)(?=^\[|\Z)', text, re.M | re.S)
if not m:
    sys.exit(0)   # section absent → not configured

section = m.group(1)

enabled_m = re.search(r'^enabled\s*=\s*(true|false)', section, re.M)
enabled = enabled_m.group(1) if enabled_m else 'true'
if enabled != 'true':
    sys.exit(0)

conn_m = re.search(r'^connection_type\s*=\s*"(\w+)"', section, re.M)
conn = conn_m.group(1) if conn_m else 'tcp'

print(conn)
PYEOF
then
    _mesh_conn_type=$(python3 - "$CONFIG_DIR/config.toml" <<'PYEOF'
import sys, re
path = sys.argv[1]
text = open(path).read()
m = re.search(r'^\[plugins\.mesh\](.*?)(?=^\[|\Z)', text, re.M | re.S)
if not m: sys.exit(0)
section = m.group(1)
conn_m = re.search(r'^connection_type\s*=\s*"(\w+)"', section, re.M)
print(conn_m.group(1) if conn_m else 'tcp')
PYEOF
)
    _mesh_enabled=true
fi

# Parse [plugins.meshtastic] — enabled defaults to false if key is absent.
_meshtastic_conn_type="serial"
if python3 - "$CONFIG_DIR/config.toml" <<'PYEOF'
import sys, re
path = sys.argv[1]
text = open(path).read()
m = re.search(r'^\[plugins\.meshtastic\](.*?)(?=^\[|\Z)', text, re.M | re.S)
if not m: sys.exit(1)
section = m.group(1)
enabled_m = re.search(r'^enabled\s*=\s*(true|false)', section, re.M)
enabled = enabled_m.group(1) if enabled_m else 'false'
sys.exit(0 if enabled == 'true' else 1)
PYEOF
then
    _meshtastic_enabled=true
    _meshtastic_conn_type=$(python3 - "$CONFIG_DIR/config.toml" <<'PYEOF'
import sys, re
path = sys.argv[1]
text = open(path).read()
m = re.search(r'^\[plugins\.meshtastic\](.*?)(?=^\[|\Z)', text, re.M | re.S)
if not m: sys.exit(0)
section = m.group(1)
conn_m = re.search(r'^connection_type\s*=\s*"(\w+)"', section, re.M)
print(conn_m.group(1) if conn_m else 'serial')
PYEOF
)
fi

# ── MeshCore Pi HAT: pymc-companion ──────────────────────────────────────────

PYMC_DIR="/opt/pymc-companion"
PYMC_UNIT="/etc/systemd/system/pymc-companion.service"
PYMC_CONFIG="$CONFIG_DIR/pymc-companion.yaml"

if [[ "$_mesh_enabled" == true && "$_mesh_conn_type" == "hat" ]]; then
    echo
    echo "─── MeshCore Pi HAT radio bridge ───────────────────────────────────────────"
    echo

    # Detect if gpiod backend is needed (written by the wizard into the YAML).
    _gpiod=false
    if grep -q 'use_gpiod_backend: true' "$PYMC_CONFIG" 2>/dev/null; then
        _gpiod=true
    fi

    # ── Enable SPI ────────────────────────────────────────────────────────────
    _spi_ok=false
    for _cfg_file in /boot/config.txt /boot/firmware/config.txt; do
        if [[ -f "$_cfg_file" ]] && grep -q "^dtparam=spi=on" "$_cfg_file"; then
            _spi_ok=true; break
        fi
    done
    if [[ "$_spi_ok" == false ]]; then
        if command -v raspi-config &>/dev/null; then
            info "Enabling SPI via raspi-config..."
            raspi-config nonint do_spi 0
            success "SPI enabled (effective after reboot)"
        else
            warn "SPI may not be enabled — add 'dtparam=spi=on' to /boot/config.txt and reboot"
        fi
    else
        success "SPI already enabled"
    fi

    # ── System dependencies ───────────────────────────────────────────────────
    info "Installing MeshCore HAT system dependencies..."
    _syspkgs="python3-venv python3-dev liblgpio-dev"
    [[ "$_gpiod" == true ]] && _syspkgs="$_syspkgs libgpiod-dev"
    # shellcheck disable=SC2086
    apt-get install -y -qq $_syspkgs
    success "HAT system packages installed"

    # ── Python venv + pymc_core ───────────────────────────────────────────────
    mkdir -p "$PYMC_DIR"
    if [[ ! -d "$PYMC_DIR/venv" ]]; then
        info "Creating Python venv at $PYMC_DIR/venv..."
        python3 -m venv "$PYMC_DIR/venv"
    fi
    info "Installing pymc_core (this may take a minute)..."
    "$PYMC_DIR/venv/bin/pip" install -q --upgrade pip
    "$PYMC_DIR/venv/bin/pip" install -q pymc_core pyyaml spidev
    if [[ "$_gpiod" == false ]]; then
        "$PYMC_DIR/venv/bin/pip" install -q lgpio python-periphery
    fi
    success "pymc_core installed"

    # ── Install companion script ───────────────────────────────────────────────
    install -m 755 \
        "$SRC_DIR/contrib/pymc-companion/pymc-companion.py" \
        "$PYMC_DIR/pymc-companion.py"

    chown "root:$SERVICE_USER" "$PYMC_CONFIG"
    chmod 640 "$PYMC_CONFIG"
    success "pymc-companion.yaml configured"

    # ── Systemd service ───────────────────────────────────────────────────────
    info "Installing pymc-companion systemd service..."
    install -m 644 \
        "$SRC_DIR/contrib/pymc-companion/pymc-companion.service" \
        "$PYMC_UNIT"
    usermod -aG spi,gpio "$SERVICE_USER" 2>/dev/null || true
    systemctl daemon-reload
    systemctl enable --now pymc-companion
    success "pymc-companion service enabled and started"

else
    # MeshCore HAT not in use — remove pymc-companion if it was previously installed.
    if systemctl is-enabled --quiet pymc-companion 2>/dev/null; then
        info "Removing pymc-companion (MeshCore HAT not configured)..."
        systemctl disable --now pymc-companion 2>/dev/null || true
        rm -f "$PYMC_UNIT"
        systemctl daemon-reload
        success "pymc-companion removed"
    fi
fi

# ── Meshtastic: Pi HAT UART setup or informational only ──────────────────────

if [[ "$_meshtastic_enabled" == true && "$_meshtastic_conn_type" == "hat" ]]; then
    echo
    echo "─── Meshtastic Pi HAT (GPIO UART) ───────────────────────────────────────────"
    echo

    # Enable UART hardware and disable the serial console so the UART is free
    # for the Meshtastic radio firmware.
    if command -v raspi-config &>/dev/null; then
        info "Enabling UART hardware and disabling serial console via raspi-config..."
        raspi-config nonint do_serial_hw 0    # 0 = enable UART hardware
        raspi-config nonint do_serial_cons 1  # 1 = disable login shell on serial
        success "UART enabled, serial console disabled (takes effect after reboot)"
    else
        warn "raspi-config not found — add 'enable_uart=1' to /boot/firmware/config.txt"
        warn "and comment out any 'console=serial0,...' in /boot/firmware/cmdline.txt, then reboot"
    fi

    # Service user needs dialout group to access the UART device.
    usermod -aG dialout "$SERVICE_USER" 2>/dev/null || true
    success "Service user added to dialout group"
    echo

elif [[ "$_meshtastic_enabled" == true ]]; then
    echo
    echo "─── Meshtastic ─────────────────────────────────────────────────────────────"
    echo
    info "Meshtastic transport enabled."
    info "No companion service required — USB serial talks directly to the radio; TCP connects to meshtasticd."
    echo
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
