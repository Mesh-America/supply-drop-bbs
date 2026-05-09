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

# ── pymc-companion (Pi HAT radio bridge) ──────────────────────────────────────

PYMC_DIR="/opt/pymc-companion"
PYMC_UNIT="/etc/systemd/system/pymc-companion.service"
PYMC_CONFIG="$CONFIG_DIR/pymc-companion.yaml"

# Read the connection_type the setup wizard wrote.
_conn_type=$(grep '^connection_type' "$CONFIG_DIR/config.toml" \
    | sed 's/connection_type = "\(.*\)"/\1/' | tr -d '[:space:]')

if [[ "$_conn_type" == "hat" ]]; then
    echo
    echo "─── Pi HAT radio bridge ────────────────────────────────────────────────────"
    echo

    # ── Region / frequency ────────────────────────────────────────────────────
    echo "  Select your region:"
    echo "  1) United States  (910.525 MHz)"
    echo "  2) Europe         (869.618 MHz)"
    echo "  3) Enter frequency manually"
    echo
    read -r -p "  Region [1]: " _region
    _region="${_region:-1}"
    case "$_region" in
        2) _freq=869618000 ;;
        3) read -r -p "  Frequency in Hz (e.g. 910525000): " _freq; _freq="${_freq:-910525000}" ;;
        *) _freq=910525000 ;;
    esac

    # ── HAT selection ─────────────────────────────────────────────────────────
    echo
    echo "  Select your Pi HAT:"
    echo "   1) ZebraHat 1W                (wehooper4)"
    echo "   2) Waveshare SX1262 LoRa HAT"
    echo "   3) PiMesh-1W (V1)"
    echo "   4) PiMesh-1W (V2)"
    echo "   5) MeshAdv Mini"
    echo "   6) MeshAdv"
    echo "   7) FemtoFox SX1262 1W"
    echo "   8) FemtoFox SX1262 2W"
    echo "   9) NebraHat 2W"
    echo "  10) RAK6421 + RAK13300x        (Slot 1)"
    echo "  11) RAK6421 + RAK13300x        (Slot 2)"
    echo "  12) Zindello UltraPeater E22"
    echo "  13) Zindello UltraPeater E22P"
    echo "  14) uConsole LoRa Module v1"
    echo "  15) uConsole LoRa Module v2"
    echo
    read -r -p "  HAT [1]: " _hat
    _hat="${_hat:-1}"

    # Optional fields — empty means omit from YAML.
    _gpiod=false; _gpio_chip=0
    _en_pin=""; _cs_id=""; _tx_led=""; _rx_led=""

    case "$_hat" in
      1)  _bus=0;  _cs=24;  _reset=17; _busy=27; _irq=22; _txen=-1; _rxen=-1; _dio2=true;  _dio3=true;  _power=18 ;;
      2)  _bus=0;  _cs=21;  _reset=18; _busy=20; _irq=16; _txen=13; _rxen=12; _dio2=false; _dio3=false; _power=22 ;;
      3)  _bus=0;  _cs=21;  _reset=18; _busy=20; _irq=16; _txen=13; _rxen=12; _dio2=false; _dio3=true;  _power=22 ;;
      4)  _bus=0;  _cs=-1;  _reset=18; _busy=5;  _irq=6;  _txen=-1; _rxen=-1; _dio2=true;  _dio3=true;  _power=22; _en_pin=26 ;;
      5)  _bus=0;  _cs=8;   _reset=24; _busy=20; _irq=16; _txen=-1; _rxen=12; _dio2=false; _dio3=false; _power=22 ;;
      6)  _bus=0;  _cs=21;  _reset=18; _busy=20; _irq=16; _txen=13; _rxen=12; _dio2=false; _dio3=true;  _power=22 ;;
      7)  _bus=0;  _cs=16;  _reset=25; _busy=22; _irq=23; _txen=-1; _rxen=24; _dio2=false; _dio3=true;  _power=30; _gpiod=true; _gpio_chip=1 ;;
      8)  _bus=0;  _cs=16;  _reset=25; _busy=22; _irq=23; _txen=-1; _rxen=24; _dio2=true;  _dio3=true;  _power=8;  _gpiod=true; _gpio_chip=1 ;;
      9)  _bus=0;  _cs=8;   _reset=18; _busy=4;  _irq=22; _txen=-1; _rxen=25; _dio2=true;  _dio3=true;  _power=8 ;;
      10) _bus=0;  _cs=-1;  _reset=16; _busy=24; _irq=22; _txen=-1; _rxen=-1; _dio2=true;  _dio3=true;  _power=22; _gpiod=true; _gpio_chip=1; _en_pin=12 ;;
      11) _bus=0;  _cs=-1;  _reset=24; _busy=19; _irq=18; _txen=-1; _rxen=-1; _dio2=true;  _dio3=true;  _power=22; _gpiod=true; _gpio_chip=1; _cs_id=1; _en_pin=26 ;;
      12) _bus=0;  _cs=16;  _reset=22; _busy=11; _irq=10; _txen=20; _rxen=21; _dio2=false; _dio3=true;  _power=22; _gpiod=true; _gpio_chip=1; _tx_led=8; _rx_led=1 ;;
      13) _bus=0;  _cs=16;  _reset=22; _busy=11; _irq=10; _txen=20; _rxen=-1; _dio2=false; _dio3=true;  _power=22; _gpiod=true; _gpio_chip=1; _en_pin=21; _tx_led=8; _rx_led=1 ;;
      14) _bus=1;  _cs=-1;  _reset=25; _busy=24; _irq=26; _txen=-1; _rxen=-1; _dio2=false; _dio3=false; _power=22 ;;
      15) _bus=1;  _cs=-1;  _reset=25; _busy=24; _irq=26; _txen=-1; _rxen=-1; _dio2=true;  _dio3=true;  _power=22 ;;
      *)  warn "Unknown choice — defaulting to ZebraHat 1W"
          _bus=0; _cs=24; _reset=17; _busy=27; _irq=22; _txen=-1; _rxen=-1; _dio2=true; _dio3=true; _power=18 ;;
    esac

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
    info "Installing HAT system dependencies..."
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

    # ── Write pymc-companion.yaml ─────────────────────────────────────────────
    _bbs_name=$(grep '^name = ' "$CONFIG_DIR/config.toml" \
        | sed 's/^name = "\(.*\)"$/\1/')
    _bbs_name="${_bbs_name:-Supply Drop BBS}"

    info "Writing $PYMC_CONFIG..."
    cat > "$PYMC_CONFIG" <<YAML
companion:
  node_name: "$_bbs_name"
  identity_path: "/var/lib/supply-drop-bbs/companion.key"
  tcp_port: 5000
  bind_address: "127.0.0.1"
  autoadd_config: 0x0F

radio:
  frequency: $_freq
  bandwidth: 62500
  spreading_factor: 7
  coding_rate: 5
  tx_power: $_power
  preamble_length: 17
  sync_word: 0x3444
  bus_id: $_bus
  cs_pin: $_cs
  reset_pin: $_reset
  busy_pin: $_busy
  irq_pin: $_irq
  txen_pin: $_txen
  rxen_pin: $_rxen
  use_dio2_rf: $_dio2
  use_dio3_tcxo: $_dio3
YAML
    [[ "$_gpiod" == true ]] && printf "  use_gpiod_backend: true\n  gpio_chip: %s\n" "$_gpio_chip" >> "$PYMC_CONFIG"
    [[ -n "$_en_pin" ]]  && echo "  en_pin: $_en_pin"   >> "$PYMC_CONFIG"
    [[ -n "$_cs_id" ]]   && echo "  cs_id: $_cs_id"     >> "$PYMC_CONFIG"
    [[ -n "$_tx_led" ]]  && echo "  tx_led: $_tx_led"   >> "$PYMC_CONFIG"
    [[ -n "$_rx_led" ]]  && echo "  rx_led: $_rx_led"   >> "$PYMC_CONFIG"

    chown "root:$SERVICE_USER" "$PYMC_CONFIG"
    chmod 640 "$PYMC_CONFIG"
    success "pymc-companion.yaml written"

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
    # Not a HAT — remove pymc-companion if it was previously installed.
    if systemctl is-enabled --quiet pymc-companion 2>/dev/null; then
        info "Removing pymc-companion (not needed for '$_conn_type' connection)..."
        systemctl disable --now pymc-companion 2>/dev/null || true
        rm -f "$PYMC_UNIT"
        systemctl daemon-reload
        success "pymc-companion removed"
    fi
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
