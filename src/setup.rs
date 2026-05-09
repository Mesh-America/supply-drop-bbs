//! Interactive first-run setup wizard.
//!
//! Guides the operator through:
//!
//! 1. Choosing a radio connection type (USB serial / TCP / Pi HAT)
//! 2. Configuring the connection (port selection or address entry)
//! 3. Setting BBS identity (name, data directory)
//! 4. Writing a `config.toml`
//! 5. For Pi HAT: prompting region + HAT model and writing `pymc-companion.yaml`
//! 6. Printing platform-specific next steps (group membership, systemd)
//!
//! Entry point: [`run_wizard`].

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::{
    fmt::Write as FmtWrite,
    fs,
    path::{Path, PathBuf},
};

// ── Existing-config loader ────────────────────────────────────────────────────

/// Values read from any already-existing config files.
/// All fields fall back to the same hard-coded defaults the wizard previously
/// used when no config existed.
struct Existing {
    bbs_name: String,
    data_dir: String,
    connection_type: String, // "serial" | "hat" | "tcp"
    serial_port: Option<String>,
    baud_rate: u32,
    web_enabled: bool,
    web_bind: String,
    web_backup_dir: Option<String>,
    region_idx: usize,
    hat_idx: usize,
}

/// Load defaults from existing `config.toml` and `pymc-companion.yaml`.
/// Missing files or parse errors are silently ignored; compiled-in defaults
/// are used for anything that can't be read.
fn load_existing(out_path: &Path) -> Existing {
    // ── config.toml ───────────────────────────────────────────────────────────
    let toml_raw = fs::read_to_string(out_path).unwrap_or_default();
    let toml_val: toml::Value = toml_raw
        .parse()
        .unwrap_or(toml::Value::Table(Default::default()));

    let bbs = toml_val.get("bbs");
    let mesh = toml_val.get("plugins").and_then(|p| p.get("mesh"));
    let web = toml_val.get("plugins").and_then(|p| p.get("web"));

    let bbs_name = bbs
        .and_then(|b| b.get("name"))
        .and_then(|v| v.as_str())
        .unwrap_or("Supply Drop BBS")
        .to_owned();

    let data_dir = bbs
        .and_then(|b| b.get("data_dir"))
        .and_then(|v| v.as_str())
        .map(str::to_owned)
        .unwrap_or_else(|| {
            if cfg!(target_os = "linux") {
                "/var/lib/supply-drop-bbs".to_owned()
            } else {
                dirs::data_local_dir()
                    .map(|d| d.join("supply-drop-bbs").to_string_lossy().into_owned())
                    .unwrap_or_else(|| "/var/lib/supply-drop-bbs".to_owned())
            }
        });

    let connection_type = mesh
        .and_then(|m| m.get("connection_type"))
        .and_then(|v| v.as_str())
        .unwrap_or("serial")
        .to_owned();

    let serial_port = mesh
        .and_then(|m| m.get("serial_port"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    let baud_rate = mesh
        .and_then(|m| m.get("baud_rate"))
        .and_then(|v| v.as_integer())
        .map(|v| v as u32)
        .unwrap_or(115_200);

    let web_enabled = web
        .and_then(|w| w.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let web_bind = web
        .and_then(|w| w.get("bind"))
        .and_then(|v| v.as_str())
        .unwrap_or("0.0.0.0:8080")
        .to_owned();

    let web_backup_dir = web
        .and_then(|w| w.get("backup_dir"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    // ── pymc-companion.yaml ───────────────────────────────────────────────────
    let yaml_path = companion_yaml_path(out_path);
    let yaml = fs::read_to_string(&yaml_path).unwrap_or_default();

    Existing {
        bbs_name,
        data_dir,
        connection_type,
        serial_port,
        baud_rate,
        web_enabled,
        web_bind,
        web_backup_dir,
        region_idx: match_region_preset(&yaml),
        hat_idx: match_hat_preset(&yaml),
    }
}

/// Extract a scalar value from a flat YAML file by key name.
fn yaml_value(yaml: &str, key: &str) -> Option<String> {
    for line in yaml.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix(key) {
            if let Some(rest) = rest.strip_prefix(':') {
                let val = rest.trim().trim_matches('"').trim_matches('\'');
                if !val.is_empty() {
                    return Some(val.to_owned());
                }
            }
        }
    }
    None
}

/// Find the index in `REGION_PRESETS` whose frequency + bandwidth +
/// spreading_factor matches the YAML.  Falls back to USA/Canada (14).
fn match_region_preset(yaml: &str) -> usize {
    let freq = yaml_value(yaml, "frequency").and_then(|s| s.parse::<u64>().ok());
    let bw = yaml_value(yaml, "bandwidth").and_then(|s| s.parse::<u32>().ok());
    let sf = yaml_value(yaml, "spreading_factor").and_then(|s| s.parse::<u8>().ok());
    if let (Some(freq), Some(bw), Some(sf)) = (freq, bw, sf) {
        for (i, r) in REGION_PRESETS.iter().enumerate() {
            if r.frequency_hz == freq && r.bandwidth_hz == bw && r.spreading_factor == sf {
                return i;
            }
        }
    }
    14 // USA/Canada
}

/// Find the index in `HAT_PRESETS` whose key GPIO pins match the YAML.
/// Falls back to 0 (ZebraHat).
fn match_hat_preset(yaml: &str) -> usize {
    let get_i32 = |key: &str| yaml_value(yaml, key).and_then(|s| s.parse::<i32>().ok());
    let bus = get_i32("bus_id");
    let cs = get_i32("cs_pin");
    let reset = get_i32("reset_pin");
    let busy = get_i32("busy_pin");
    let irq = get_i32("irq_pin");
    let txen = get_i32("txen_pin");
    let rxen = get_i32("rxen_pin");
    if let (Some(bus), Some(cs), Some(reset), Some(busy), Some(irq)) = (bus, cs, reset, busy, irq) {
        for (i, h) in HAT_PRESETS.iter().enumerate() {
            if h.bus == bus
                && h.cs == cs
                && h.reset == reset
                && h.busy == busy
                && h.irq == irq
                && txen.is_none_or(|v| v == h.txen)
                && rxen.is_none_or(|v| v == h.rxen)
            {
                return i;
            }
        }
    }
    0
}

use dialoguer::{theme::ColorfulTheme, Confirm, Input, Select};

// ── Public entry point ────────────────────────────────────────────────────────

/// Run the interactive setup wizard.
///
/// `config_out` is where to write the resulting `config.toml`.  If `None` the
/// wizard writes to `./config.toml` in the current working directory (the most
/// common case for new installs).
pub fn run_wizard(config_out: Option<&Path>) {
    print_banner();

    let theme = ColorfulTheme::default();

    // Determine output path early so we can read the existing config from it.
    let out_path = config_out
        .map(|p| p.to_owned())
        .unwrap_or_else(|| PathBuf::from("config.toml"));

    let ex = load_existing(&out_path);

    // ── Radio connection ──────────────────────────────────────────────────────
    section("Radio connection");

    let conn_items = &[
        "USB / serial  (Heltec V3, T-Beam, RAK4631 — plug in via USB)",
        "Pi HAT        (ZebraHat, Waveshare, PiMesh, FemtoFox — SPI on GPIO)",
    ];

    let conn_default = if ex.connection_type == "hat" { 1 } else { 0 };
    let conn_choice = prompt_select(
        &theme,
        "How does your radio connect?",
        conn_items,
        conn_default,
    );

    let (connection_type, serial_port, baud_rate) = match conn_choice {
        0 => configure_serial(&theme, ex.serial_port.as_deref(), ex.baud_rate),
        _ => ("hat", None, None),
    };

    // ── BBS identity ──────────────────────────────────────────────────────────
    section("BBS identity");

    let bbs_name: String = Input::with_theme(&theme)
        .with_prompt("BBS name")
        .default(ex.bbs_name.clone())
        .interact_text()
        .unwrap_or_else(|_| cancelled());

    // ── Data storage ──────────────────────────────────────────────────────────
    section("Data storage");

    let data_dir_str: String = Input::with_theme(&theme)
        .with_prompt("Data directory")
        .default(ex.data_dir.clone())
        .interact_text()
        .unwrap_or_else(|_| cancelled());

    let data_dir = PathBuf::from(&data_dir_str);

    // ── Web admin ─────────────────────────────────────────────────────────────
    section("Web admin UI");

    println!("The web admin is a browser-based dashboard for managing users,");
    println!("rooms, messages, backups, and live logs. Log in with any BBS");
    println!("account that has Aide or Sysop permission.");
    println!();

    let web_enabled = Confirm::with_theme(&theme)
        .with_prompt("Enable the web admin UI?")
        .default(ex.web_enabled)
        .interact()
        .unwrap_or_else(|_| cancelled());

    let web_bind = if web_enabled {
        println!();
        let bind: String = Input::with_theme(&theme)
            .with_prompt("Web admin bind address")
            .default(ex.web_bind.clone())
            .interact_text()
            .unwrap_or_else(|_| cancelled());
        Some(bind)
    } else {
        println!("  Web admin disabled — you can enable it later by re-running setup.");
        None
    };

    let web_backup_dir = if web_enabled {
        println!();
        let default_dir = ex
            .web_backup_dir
            .clone()
            .unwrap_or_else(|| "/var/backup/supply-drop".to_owned());
        let dir: String = Input::with_theme(&theme)
            .with_prompt("Backup directory (for database snapshots via VACUUM INTO)")
            .default(default_dir)
            .interact_text()
            .unwrap_or_else(|_| cancelled());
        Some(dir)
    } else {
        None
    };

    // ── Pi HAT: region + model ────────────────────────────────────────────────
    let hat_params = if connection_type == "hat" {
        Some(configure_hat(
            &theme,
            &bbs_name,
            &data_dir,
            ex.region_idx,
            ex.hat_idx,
        ))
    } else {
        None
    };

    // ── Confirm & write ───────────────────────────────────────────────────────
    section("Write config");

    println!("\nConfig will be written to: {}", out_path.display());
    if hat_params.is_some() {
        let yaml_path = companion_yaml_path(&out_path);
        println!("HAT config will be written to: {}", yaml_path.display());
    }
    println!("(Run 'supply-drop-bbs config show' afterwards to see all effective values.)\n");

    let confirmed = Confirm::with_theme(&theme)
        .with_prompt(format!("Write {}", out_path.display()))
        .default(true)
        .interact()
        .unwrap_or_else(|_| cancelled());

    if !confirmed {
        println!("\nSetup cancelled — no files written.");
        std::process::exit(0);
    }

    let toml = build_toml(&TomlParams {
        bbs_name: &bbs_name,
        data_dir: &data_dir,
        connection_type,
        serial_port: serial_port.as_deref(),
        baud_rate,
        web_enabled,
        web_bind: web_bind.as_deref(),
        web_backup_dir: web_backup_dir.as_deref(),
    });

    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            if let Err(e) = fs::create_dir_all(parent) {
                eprintln!(
                    "error: could not create directory {}: {e}",
                    parent.display()
                );
                std::process::exit(1);
            }
        }
    }

    if let Err(e) = fs::write(&out_path, &toml) {
        eprintln!("error: could not write {}: {e}", out_path.display());
        std::process::exit(1);
    }

    println!("\nConfig written to {}.", out_path.display());

    // Create backup directory and set ownership so the service user can write to it.
    if let Some(ref dir) = web_backup_dir {
        if !dir.is_empty() {
            match fs::create_dir_all(dir) {
                Ok(()) => {
                    println!("Backup directory created: {dir}");
                    // On Linux, chown to the service user so it can write backup files.
                    #[cfg(target_os = "linux")]
                    {
                        let status = std::process::Command::new("chown")
                            .args(["supply-drop:supply-drop", dir])
                            .status();
                        match status {
                            Ok(s) if s.success() => {
                                println!("  ownership set to supply-drop:supply-drop");
                            }
                            _ => {
                                eprintln!(
                                    "  warning: could not chown {dir} — you may need to run:\n\
                                     \n    sudo chown supply-drop:supply-drop {dir}\n"
                                );
                            }
                        }
                    }
                }
                Err(e) => {
                    eprintln!("warning: could not create backup dir {dir}: {e}");
                    eprintln!("  Create it manually and ensure the service user can write to it.");
                }
            }
        }
    }

    // Write pymc-companion.yaml if HAT was chosen.
    if let Some(ref hat) = hat_params {
        let yaml_path = companion_yaml_path(&out_path);
        let yaml = build_companion_yaml(hat);
        if let Err(e) = fs::write(&yaml_path, &yaml) {
            eprintln!("error: could not write {}: {e}", yaml_path.display());
            std::process::exit(1);
        }
        println!("HAT config written to {}.", yaml_path.display());
    }

    // ── Next steps ────────────────────────────────────────────────────────────
    section("Next steps");
    print_next_steps(connection_type, serial_port.as_deref(), web_bind.as_deref());
}

// ── Connection type configuration ─────────────────────────────────────────────

fn configure_serial(
    theme: &ColorfulTheme,
    existing_port: Option<&str>,
    existing_baud: u32,
) -> (&'static str, Option<String>, Option<u32>) {
    let ports = list_serial_ports();

    let serial_port = if ports.is_empty() {
        println!("\nNo serial ports detected. Make sure your device is connected.");
        println!("You can enter the path manually.\n");
        let mut prompt =
            Input::with_theme(theme).with_prompt("Serial port path (e.g. /dev/ttyACM0 or COM3)");
        if let Some(p) = existing_port {
            prompt = prompt.default(p.to_owned());
        }
        prompt.interact_text().unwrap_or_else(|_| cancelled())
    } else {
        let mut items: Vec<String> = ports
            .iter()
            .map(|p| {
                if let Some(ref info) = p.description {
                    format!("{}  ({})", p.name, info)
                } else {
                    p.name.clone()
                }
            })
            .collect();
        items.push("Enter path manually…".into());

        // Pre-select the existing port if it appears in the detected list.
        let port_default = existing_port
            .and_then(|ep| ports.iter().position(|p| p.name == ep))
            .unwrap_or(0);

        let choice = prompt_select(theme, "Select serial port", &items, port_default);

        if choice == ports.len() {
            let mut prompt = Input::with_theme(theme).with_prompt("Serial port path");
            if let Some(p) = existing_port {
                prompt = prompt.default(p.to_owned());
            }
            prompt.interact_text().unwrap_or_else(|_| cancelled())
        } else {
            ports[choice].name.clone()
        }
    };

    let baud_str: String = Input::with_theme(theme)
        .with_prompt("Baud rate")
        .default(existing_baud.to_string())
        .validate_with(|s: &String| -> Result<(), &str> {
            if s.parse::<u32>().is_ok() {
                Ok(())
            } else {
                Err("baud rate must be a positive integer")
            }
        })
        .interact_text()
        .unwrap_or_else(|_| cancelled());

    let baud: u32 = baud_str.parse().expect("validated above");

    ("serial", Some(serial_port), Some(baud))
}

// ── Region presets ────────────────────────────────────────────────────────────

struct RegionPreset {
    name: &'static str,
    frequency_hz: u64,
    bandwidth_hz: u32,
    spreading_factor: u8,
    coding_rate: u8,
    tx_power_dbm: i32,
}

const REGION_PRESETS: &[RegionPreset] = &[
    RegionPreset {
        name: "Australia",
        frequency_hz: 915_800_000,
        bandwidth_hz: 250_000,
        spreading_factor: 10,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RegionPreset {
        name: "Australia (Narrow)",
        frequency_hz: 916_575_000,
        bandwidth_hz: 62_500,
        spreading_factor: 7,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RegionPreset {
        name: "Australia SA, WA, QLD",
        frequency_hz: 923_125_000,
        bandwidth_hz: 62_500,
        spreading_factor: 8,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RegionPreset {
        name: "Czech Republic",
        frequency_hz: 869_432_000,
        bandwidth_hz: 62_500,
        spreading_factor: 7,
        coding_rate: 5,
        tx_power_dbm: 14,
    },
    RegionPreset {
        name: "EU 433MHz",
        frequency_hz: 433_650_000,
        bandwidth_hz: 250_000,
        spreading_factor: 11,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RegionPreset {
        name: "EU/UK (Long Range)",
        frequency_hz: 869_525_000,
        bandwidth_hz: 250_000,
        spreading_factor: 11,
        coding_rate: 5,
        tx_power_dbm: 14,
    },
    RegionPreset {
        name: "EU/UK (Medium Range)",
        frequency_hz: 869_525_000,
        bandwidth_hz: 250_000,
        spreading_factor: 10,
        coding_rate: 5,
        tx_power_dbm: 14,
    },
    RegionPreset {
        name: "EU/UK (Narrow)",
        frequency_hz: 869_618_000,
        bandwidth_hz: 62_500,
        spreading_factor: 8,
        coding_rate: 5,
        tx_power_dbm: 14,
    },
    RegionPreset {
        name: "New Zealand",
        frequency_hz: 917_375_000,
        bandwidth_hz: 250_000,
        spreading_factor: 11,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RegionPreset {
        name: "New Zealand (Narrow)",
        frequency_hz: 917_375_000,
        bandwidth_hz: 62_500,
        spreading_factor: 7,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RegionPreset {
        name: "Portugal 433",
        frequency_hz: 433_375_000,
        bandwidth_hz: 62_500,
        spreading_factor: 9,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RegionPreset {
        name: "Portugal 869",
        frequency_hz: 869_618_000,
        bandwidth_hz: 62_500,
        spreading_factor: 7,
        coding_rate: 5,
        tx_power_dbm: 14,
    },
    RegionPreset {
        name: "Switzerland",
        frequency_hz: 869_618_000,
        bandwidth_hz: 62_500,
        spreading_factor: 8,
        coding_rate: 5,
        tx_power_dbm: 14,
    },
    RegionPreset {
        name: "USA Arizona",
        frequency_hz: 908_205_000,
        bandwidth_hz: 62_500,
        spreading_factor: 10,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RegionPreset {
        name: "USA/Canada",
        frequency_hz: 910_525_000,
        bandwidth_hz: 62_500,
        spreading_factor: 7,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RegionPreset {
        name: "Vietnam",
        frequency_hz: 920_250_000,
        bandwidth_hz: 250_000,
        spreading_factor: 11,
        coding_rate: 5,
        tx_power_dbm: 20,
    },
    RegionPreset {
        name: "Off-Grid 433",
        frequency_hz: 433_000_000,
        bandwidth_hz: 250_000,
        spreading_factor: 11,
        coding_rate: 8,
        tx_power_dbm: 20,
    },
    RegionPreset {
        name: "Off-Grid 869",
        frequency_hz: 869_000_000,
        bandwidth_hz: 250_000,
        spreading_factor: 11,
        coding_rate: 8,
        tx_power_dbm: 14,
    },
    RegionPreset {
        name: "Off-Grid 918",
        frequency_hz: 918_000_000,
        bandwidth_hz: 250_000,
        spreading_factor: 11,
        coding_rate: 8,
        tx_power_dbm: 20,
    },
];

// ── Pi HAT configuration ──────────────────────────────────────────────────────

struct HatPreset {
    name: &'static str,
    bus: i32,
    cs: i32,
    reset: i32,
    busy: i32,
    irq: i32,
    txen: i32,
    rxen: i32,
    dio2: bool,
    dio3: bool,
    gpiod: bool,
    gpio_chip: i32,
    en_pin: Option<i32>,
    cs_id: Option<i32>,
    tx_led: Option<i32>,
    rx_led: Option<i32>,
}

const HAT_PRESETS: &[HatPreset] = &[
    HatPreset {
        name: "ZebraHat 1W",
        bus: 0,
        cs: 24,
        reset: 17,
        busy: 27,
        irq: 22,
        txen: -1,
        rxen: -1,
        dio2: true,
        dio3: true,
        gpiod: false,
        gpio_chip: 0,
        en_pin: None,
        cs_id: None,
        tx_led: None,
        rx_led: None,
    },
    HatPreset {
        name: "Waveshare SX1262 LoRa HAT",
        bus: 0,
        cs: 21,
        reset: 18,
        busy: 20,
        irq: 16,
        txen: 13,
        rxen: 12,
        dio2: false,
        dio3: false,
        gpiod: false,
        gpio_chip: 0,
        en_pin: None,
        cs_id: None,
        tx_led: None,
        rx_led: None,
    },
    HatPreset {
        name: "PiMesh-1W (V1)",
        bus: 0,
        cs: 21,
        reset: 18,
        busy: 20,
        irq: 16,
        txen: 13,
        rxen: 12,
        dio2: false,
        dio3: true,
        gpiod: false,
        gpio_chip: 0,
        en_pin: None,
        cs_id: None,
        tx_led: None,
        rx_led: None,
    },
    HatPreset {
        name: "PiMesh-1W (V2)",
        bus: 0,
        cs: -1,
        reset: 18,
        busy: 5,
        irq: 6,
        txen: -1,
        rxen: -1,
        dio2: true,
        dio3: true,
        gpiod: false,
        gpio_chip: 0,
        en_pin: Some(26),
        cs_id: None,
        tx_led: None,
        rx_led: None,
    },
    HatPreset {
        name: "MeshAdv Mini",
        bus: 0,
        cs: 8,
        reset: 24,
        busy: 20,
        irq: 16,
        txen: -1,
        rxen: 12,
        dio2: false,
        dio3: false,
        gpiod: false,
        gpio_chip: 0,
        en_pin: None,
        cs_id: None,
        tx_led: None,
        rx_led: None,
    },
    HatPreset {
        name: "MeshAdv",
        bus: 0,
        cs: 21,
        reset: 18,
        busy: 20,
        irq: 16,
        txen: 13,
        rxen: 12,
        dio2: false,
        dio3: true,
        gpiod: false,
        gpio_chip: 0,
        en_pin: None,
        cs_id: None,
        tx_led: None,
        rx_led: None,
    },
    HatPreset {
        name: "FemtoFox SX1262 1W",
        bus: 0,
        cs: 16,
        reset: 25,
        busy: 22,
        irq: 23,
        txen: -1,
        rxen: 24,
        dio2: false,
        dio3: true,
        gpiod: true,
        gpio_chip: 1,
        en_pin: None,
        cs_id: None,
        tx_led: None,
        rx_led: None,
    },
    HatPreset {
        name: "FemtoFox SX1262 2W",
        bus: 0,
        cs: 16,
        reset: 25,
        busy: 22,
        irq: 23,
        txen: -1,
        rxen: 24,
        dio2: true,
        dio3: true,
        gpiod: true,
        gpio_chip: 1,
        en_pin: None,
        cs_id: None,
        tx_led: None,
        rx_led: None,
    },
    HatPreset {
        name: "NebraHat 2W",
        bus: 0,
        cs: 8,
        reset: 18,
        busy: 4,
        irq: 22,
        txen: -1,
        rxen: 25,
        dio2: true,
        dio3: true,
        gpiod: false,
        gpio_chip: 0,
        en_pin: None,
        cs_id: None,
        tx_led: None,
        rx_led: None,
    },
    HatPreset {
        name: "RAK6421 + RAK13300x  (Slot 1)",
        bus: 0,
        cs: -1,
        reset: 16,
        busy: 24,
        irq: 22,
        txen: -1,
        rxen: -1,
        dio2: true,
        dio3: true,
        gpiod: true,
        gpio_chip: 1,
        en_pin: Some(12),
        cs_id: None,
        tx_led: None,
        rx_led: None,
    },
    HatPreset {
        name: "RAK6421 + RAK13300x  (Slot 2)",
        bus: 0,
        cs: -1,
        reset: 24,
        busy: 19,
        irq: 18,
        txen: -1,
        rxen: -1,
        dio2: true,
        dio3: true,
        gpiod: true,
        gpio_chip: 1,
        en_pin: Some(26),
        cs_id: Some(1),
        tx_led: None,
        rx_led: None,
    },
    HatPreset {
        name: "Zindello UltraPeater E22",
        bus: 0,
        cs: 16,
        reset: 22,
        busy: 11,
        irq: 10,
        txen: 20,
        rxen: 21,
        dio2: false,
        dio3: true,
        gpiod: true,
        gpio_chip: 1,
        en_pin: None,
        cs_id: None,
        tx_led: Some(8),
        rx_led: Some(1),
    },
    HatPreset {
        name: "Zindello UltraPeater E22P",
        bus: 0,
        cs: 16,
        reset: 22,
        busy: 11,
        irq: 10,
        txen: 20,
        rxen: -1,
        dio2: false,
        dio3: true,
        gpiod: true,
        gpio_chip: 1,
        en_pin: Some(21),
        cs_id: None,
        tx_led: Some(8),
        rx_led: Some(1),
    },
    HatPreset {
        name: "uConsole LoRa Module v1",
        bus: 1,
        cs: -1,
        reset: 25,
        busy: 24,
        irq: 26,
        txen: -1,
        rxen: -1,
        dio2: false,
        dio3: false,
        gpiod: false,
        gpio_chip: 0,
        en_pin: None,
        cs_id: None,
        tx_led: None,
        rx_led: None,
    },
    HatPreset {
        name: "uConsole LoRa Module v2",
        bus: 1,
        cs: -1,
        reset: 25,
        busy: 24,
        irq: 26,
        txen: -1,
        rxen: -1,
        dio2: true,
        dio3: true,
        gpiod: false,
        gpio_chip: 0,
        en_pin: None,
        cs_id: None,
        tx_led: None,
        rx_led: None,
    },
];

struct HatParams {
    bbs_name: String,
    identity_path: String,
    region: usize,
    preset: usize,
}

fn configure_hat(
    theme: &ColorfulTheme,
    bbs_name: &str,
    data_dir: &Path,
    existing_region: usize,
    existing_hat: usize,
) -> HatParams {
    section("Pi HAT — region");

    let region_names: Vec<String> = REGION_PRESETS
        .iter()
        .map(|r| {
            format!(
                "{:<26} ({:.3} MHz)",
                r.name,
                r.frequency_hz as f64 / 1_000_000.0
            )
        })
        .collect();

    let region_choice = prompt_select(theme, "Select your region", &region_names, existing_region);

    section("Pi HAT — model");

    let hat_names: Vec<&str> = HAT_PRESETS.iter().map(|h| h.name).collect();
    let hat_choice = prompt_select(theme, "Select your Pi HAT", &hat_names, existing_hat);

    HatParams {
        bbs_name: bbs_name.to_owned(),
        identity_path: data_dir
            .join("companion.key")
            .to_string_lossy()
            .into_owned(),
        region: region_choice,
        preset: hat_choice,
    }
}

fn companion_yaml_path(config_out: &Path) -> PathBuf {
    config_out
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("pymc-companion.yaml")
}

fn build_companion_yaml(p: &HatParams) -> String {
    let h = &HAT_PRESETS[p.preset];
    let r = &REGION_PRESETS[p.region];
    let mut s = String::new();

    writeln!(s, "# pymc-companion configuration").unwrap();
    writeln!(s, "# Generated by: supply-drop-bbs setup").unwrap();
    writeln!(s).unwrap();
    writeln!(s, "companion:").unwrap();
    writeln!(s, "  node_name: {:?}", p.bbs_name).unwrap();
    writeln!(s, "  identity_path: {:?}", p.identity_path).unwrap();
    writeln!(s, "  tcp_port: 5000").unwrap();
    writeln!(s, "  bind_address: \"127.0.0.1\"").unwrap();
    writeln!(s, "  autoadd_config: 0x0F").unwrap();
    writeln!(s).unwrap();
    writeln!(s, "radio:").unwrap();
    writeln!(s, "  frequency: {}", r.frequency_hz).unwrap();
    writeln!(s, "  bandwidth: {}", r.bandwidth_hz).unwrap();
    writeln!(s, "  spreading_factor: {}", r.spreading_factor).unwrap();
    writeln!(s, "  coding_rate: {}", r.coding_rate).unwrap();
    writeln!(s, "  tx_power: {}", r.tx_power_dbm).unwrap();
    writeln!(s, "  preamble_length: 17").unwrap();
    writeln!(s, "  sync_word: 0x3444").unwrap();
    writeln!(s, "  bus_id: {}", h.bus).unwrap();
    writeln!(s, "  cs_pin: {}", h.cs).unwrap();
    writeln!(s, "  reset_pin: {}", h.reset).unwrap();
    writeln!(s, "  busy_pin: {}", h.busy).unwrap();
    writeln!(s, "  irq_pin: {}", h.irq).unwrap();
    writeln!(s, "  txen_pin: {}", h.txen).unwrap();
    writeln!(s, "  rxen_pin: {}", h.rxen).unwrap();
    writeln!(s, "  use_dio2_rf: {}", h.dio2).unwrap();
    writeln!(s, "  use_dio3_tcxo: {}", h.dio3).unwrap();
    if h.gpiod {
        writeln!(s, "  use_gpiod_backend: true").unwrap();
        writeln!(s, "  gpio_chip: {}", h.gpio_chip).unwrap();
    }
    if let Some(v) = h.en_pin {
        writeln!(s, "  en_pin: {v}").unwrap();
    }
    if let Some(v) = h.cs_id {
        writeln!(s, "  cs_id: {v}").unwrap();
    }
    if let Some(v) = h.tx_led {
        writeln!(s, "  tx_led: {v}").unwrap();
    }
    if let Some(v) = h.rx_led {
        writeln!(s, "  rx_led: {v}").unwrap();
    }

    s
}

// ── TOML builder ──────────────────────────────────────────────────────────────

struct TomlParams<'a> {
    bbs_name: &'a str,
    data_dir: &'a Path,
    connection_type: &'a str,
    serial_port: Option<&'a str>,
    baud_rate: Option<u32>,
    web_enabled: bool,
    web_bind: Option<&'a str>,
    web_backup_dir: Option<&'a str>,
}

fn build_toml(p: &TomlParams<'_>) -> String {
    let mut s = String::new();

    writeln!(s, "# Supply Drop BBS — configuration").unwrap();
    writeln!(s, "# Generated by: supply-drop-bbs setup").unwrap();
    writeln!(s, "#").unwrap();
    writeln!(
        s,
        "# Run 'supply-drop-bbs config show' to see all effective values."
    )
    .unwrap();
    writeln!(
        s,
        "# Run 'supply-drop-bbs config check' to validate without starting."
    )
    .unwrap();

    // [bbs]
    writeln!(s, "\n[bbs]").unwrap();
    writeln!(s, "name = {}", toml_str(p.bbs_name)).unwrap();
    writeln!(s, "data_dir = {}", toml_str(&p.data_dir.to_string_lossy())).unwrap();

    // [plugins.mesh]
    writeln!(s, "\n[plugins.mesh]").unwrap();
    writeln!(s, "connection_type = {}", toml_str(p.connection_type)).unwrap();

    match p.connection_type {
        "serial" => {
            if let Some(port) = p.serial_port {
                writeln!(s, "serial_port = {}", toml_str(port)).unwrap();
            }
            if let Some(baud) = p.baud_rate {
                if baud != 115_200 {
                    writeln!(s, "baud_rate = {baud}").unwrap();
                }
            }
        }
        "hat" => {}
        _ => {}
    }

    // [plugins.web]
    writeln!(s, "\n[plugins.web]").unwrap();
    writeln!(s, "enabled = {}", p.web_enabled).unwrap();
    if p.web_enabled {
        if let Some(bind) = p.web_bind {
            if bind != "127.0.0.1:8080" {
                writeln!(s, "bind = {}", toml_str(bind)).unwrap();
            }
        }
        writeln!(s, "cookie_secure = false").unwrap();
        if let Some(dir) = p.web_backup_dir {
            if !dir.is_empty() {
                writeln!(s, "backup_dir = {}", toml_str(dir)).unwrap();
            }
        }
    }

    s
}

fn toml_str(s: &str) -> String {
    let escaped = s.replace('\\', "\\\\").replace('"', "\\\"");
    format!("\"{escaped}\"")
}

// ── Serial port listing ───────────────────────────────────────────────────────

struct PortInfo {
    name: String,
    description: Option<String>,
}

fn list_serial_ports() -> Vec<PortInfo> {
    match tokio_serial::available_ports() {
        Err(_) => vec![],
        Ok(ports) => ports
            .into_iter()
            .map(|p| {
                let description = match &p.port_type {
                    tokio_serial::SerialPortType::UsbPort(info) => {
                        let mut parts: Vec<&str> = Vec::new();
                        if let Some(ref mfr) = info.manufacturer {
                            parts.push(mfr);
                        }
                        if let Some(ref prod) = info.product {
                            parts.push(prod);
                        }
                        if parts.is_empty() {
                            Some("USB".into())
                        } else {
                            Some(parts.join(" "))
                        }
                    }
                    tokio_serial::SerialPortType::PciPort => Some("PCI".into()),
                    tokio_serial::SerialPortType::BluetoothPort => Some("Bluetooth".into()),
                    tokio_serial::SerialPortType::Unknown => None,
                };
                PortInfo {
                    name: p.port_name,
                    description,
                }
            })
            .collect(),
    }
}

// ── Next steps ────────────────────────────────────────────────────────────────

fn print_next_steps(connection_type: &str, serial_port: Option<&str>, web_bind: Option<&str>) {
    if connection_type == "serial" && cfg!(target_os = "linux") {
        println!("To allow Supply Drop BBS to access the serial port, your user must");
        println!("be in the 'dialout' group:");
        println!();
        println!("  sudo usermod -aG dialout $USER");
        println!("  # then log out and back in, or run:");
        println!("  newgrp dialout");
        println!();
        if let Some(port) = serial_port {
            println!("You can verify access with:");
            println!("  ls -l {port}");
            println!();
        }
    }

    if cfg!(target_os = "linux") {
        println!("To run Supply Drop BBS as a systemd service:");
        println!();
        println!("  sudo install -m 644 supply-drop-bbs.service /etc/systemd/system/");
        println!("  sudo systemctl daemon-reload");
        println!("  sudo systemctl enable --now supply-drop-bbs");
        println!();
        println!("Or run it directly in the foreground:");
        println!();
        println!("  supply-drop-bbs run");
    } else {
        println!("Run the BBS with:");
        println!();
        println!("  supply-drop-bbs run");
    }

    if let Some(bind) = web_bind {
        let display_bind = if bind.starts_with("0.0.0.0") {
            bind.replacen("0.0.0.0", "<your-pi-ip>", 1)
        } else {
            bind.to_owned()
        };
        println!("Once running, open the web admin at:");
        println!();
        println!("  http://{display_bind}");
        println!();
        println!("Log in with any BBS account that has Aide or Sysop permission.");
        println!("To promote an account: supply-drop-bbs user promote <username>");
    }
    println!();
    println!("Setup complete!");
}

// ── UI helpers ────────────────────────────────────────────────────────────────

fn print_banner() {
    println!();
    println!("╔══════════════════════════════════════════════════╗");
    println!("║         Supply Drop BBS — Setup Wizard           ║");
    println!("╚══════════════════════════════════════════════════╝");
    println!();
    println!("This wizard writes a config.toml for your BBS.");
    println!("Press Ctrl-C at any time to cancel without saving.");
    println!();
}

fn section(title: &str) {
    println!();
    println!("─── {title} ──────────────────────────────────────────────");
    println!();
}

fn prompt_select<S: ToString>(
    theme: &ColorfulTheme,
    prompt: &str,
    items: &[S],
    default: usize,
) -> usize {
    Select::with_theme(theme)
        .with_prompt(prompt)
        .items(items)
        .default(default)
        .interact()
        .unwrap_or_else(|_| cancelled())
}

fn cancelled() -> ! {
    println!("\nSetup cancelled.");
    std::process::exit(0);
}
