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

    // ── Radio connection ──────────────────────────────────────────────────────
    section("Radio connection");

    let conn_items = &[
        "USB / serial  (Heltec V3, T-Beam, RAK4631 — plug in via USB)",
        "Pi HAT        (ZebraHat, Waveshare, PiMesh, FemtoFox — SPI on GPIO)",
    ];

    let conn_choice = prompt_select(&theme, "How does your radio connect?", conn_items, 0);

    let (connection_type, serial_port, baud_rate) = match conn_choice {
        0 => configure_serial(&theme),
        _ => ("hat", None, None),
    };

    // ── BBS identity ──────────────────────────────────────────────────────────
    section("BBS identity");

    let bbs_name: String = Input::with_theme(&theme)
        .with_prompt("BBS name")
        .default("Supply Drop BBS".into())
        .interact_text()
        .unwrap_or_else(|_| cancelled());

    // ── Data storage ──────────────────────────────────────────────────────────
    section("Data storage");

    let default_data_dir = if cfg!(target_os = "linux") {
        PathBuf::from("/var/lib/supply-drop-bbs")
    } else {
        dirs::data_local_dir()
            .map(|d| d.join("supply-drop-bbs"))
            .unwrap_or_else(|| PathBuf::from("/var/lib/supply-drop-bbs"))
    };

    let data_dir_str: String = Input::with_theme(&theme)
        .with_prompt("Data directory")
        .default(default_data_dir.to_string_lossy().into_owned())
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
        .default(true)
        .interact()
        .unwrap_or_else(|_| cancelled());

    let web_bind = if web_enabled {
        println!();
        let bind: String = Input::with_theme(&theme)
            .with_prompt("Web admin bind address")
            .default("0.0.0.0:8080".into())
            .interact_text()
            .unwrap_or_else(|_| cancelled());
        Some(bind)
    } else {
        println!("  Web admin disabled — you can enable it later by re-running setup.");
        None
    };

    // ── Pi HAT: region + model ────────────────────────────────────────────────
    let hat_params = if connection_type == "hat" {
        Some(configure_hat(&theme, &bbs_name, &data_dir))
    } else {
        None
    };

    // ── Confirm & write ───────────────────────────────────────────────────────
    section("Write config");

    let out_path = config_out
        .map(|p| p.to_owned())
        .unwrap_or_else(|| PathBuf::from("config.toml"));

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

fn configure_serial(theme: &ColorfulTheme) -> (&'static str, Option<String>, Option<u32>) {
    let ports = list_serial_ports();

    let serial_port = if ports.is_empty() {
        println!("\nNo serial ports detected. Make sure your device is connected.");
        println!("You can enter the path manually.\n");
        let path: String = Input::with_theme(theme)
            .with_prompt("Serial port path (e.g. /dev/ttyACM0 or COM3)")
            .interact_text()
            .unwrap_or_else(|_| cancelled());
        path
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

        let choice = prompt_select(theme, "Select serial port", &items, 0);

        if choice == ports.len() {
            Input::with_theme(theme)
                .with_prompt("Serial port path")
                .interact_text()
                .unwrap_or_else(|_| cancelled())
        } else {
            ports[choice].name.clone()
        }
    };

    let baud_str: String = Input::with_theme(theme)
        .with_prompt("Baud rate")
        .default("115200".into())
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
    power: i32,
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
        power: 18,
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
        power: 22,
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
        power: 22,
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
        power: 22,
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
        power: 22,
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
        power: 22,
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
        power: 30,
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
        power: 8,
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
        power: 8,
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
        power: 22,
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
        power: 22,
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
        power: 22,
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
        power: 22,
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
        power: 22,
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
        power: 22,
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
    frequency_hz: u64,
    preset: usize,
}

fn configure_hat(theme: &ColorfulTheme, bbs_name: &str, data_dir: &Path) -> HatParams {
    section("Pi HAT — region");

    let region_items = &[
        "United States  (910.525 MHz)",
        "Europe         (869.618 MHz)",
        "Enter frequency manually",
    ];

    let region_choice = prompt_select(theme, "Select your region", region_items, 0);

    let frequency_hz: u64 = match region_choice {
        1 => 869_618_000,
        2 => {
            let hz_str: String = Input::with_theme(theme)
                .with_prompt("Frequency in Hz (e.g. 910525000)")
                .default("910525000".into())
                .validate_with(|s: &String| -> Result<(), &str> {
                    if s.parse::<u64>().is_ok() {
                        Ok(())
                    } else {
                        Err("must be a positive integer")
                    }
                })
                .interact_text()
                .unwrap_or_else(|_| cancelled());
            hz_str.parse().expect("validated above")
        }
        _ => 910_525_000,
    };

    section("Pi HAT — model");

    let hat_names: Vec<&str> = HAT_PRESETS.iter().map(|h| h.name).collect();
    let hat_choice = prompt_select(theme, "Select your Pi HAT", &hat_names, 0);

    HatParams {
        bbs_name: bbs_name.to_owned(),
        identity_path: data_dir
            .join("companion.key")
            .to_string_lossy()
            .into_owned(),
        frequency_hz,
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
    writeln!(s, "  frequency: {}", p.frequency_hz).unwrap();
    writeln!(s, "  bandwidth: 62500").unwrap();
    writeln!(s, "  spreading_factor: 7").unwrap();
    writeln!(s, "  coding_rate: 5").unwrap();
    writeln!(s, "  tx_power: {}", h.power).unwrap();
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
