//! Interactive first-run setup wizard.
//!
//! Guides the operator through:
//!
//! 1. Choosing which radio protocols to enable (MeshCore, Meshtastic, or both)
//! 2. Configuring each enabled protocol (connection type, serial port / address)
//! 3. Setting BBS identity (name, data directory)
//! 4. Writing a `config.toml`
//! 5. For MeshCore Pi HAT: writing `pymc-companion.yaml`
//! 6. Printing platform-specific next steps (group membership, systemd)
//!
//! Which protocol sections appear is determined by compiled-in cargo features:
//! - `transport-mesh`        → MeshCore section present
//! - `transport-meshtastic`  → Meshtastic section present
//! - If both are compiled in, the wizard asks the operator to select which to
//!   enable.  If only one is compiled in, that protocol is always offered.
//!
//! Entry point: [`run_wizard`].

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::{
    fmt::Write as FmtWrite,
    fs,
    path::{Path, PathBuf},
};

// ── Existing-config loader ────────────────────────────────────────────────────

struct Existing {
    bbs_name: String,
    data_dir: String,
    // MeshCore
    mesh_enabled: bool,
    mesh_connection_type: String,
    mesh_serial_port: Option<String>,
    mesh_baud_rate: u32,
    mesh_addr: Option<String>,
    // Meshtastic
    meshtastic_enabled: bool,
    meshtastic_connection_type: String,
    meshtastic_serial_port: Option<String>,
    meshtastic_baud_rate: u32,
    meshtastic_radio_region: Option<String>,
    meshtastic_radio_preset: Option<String>,
    meshtastic_short_name: Option<String>,
    meshtastic_long_name: Option<String>,
    // Web
    web_enabled: bool,
    web_bind: String,
    web_backup_dir: Option<String>,
    // pymc-companion (HAT)
    region_idx: usize,
    hat_idx: usize,
    // USB serial radio config (None = not configured)
    mesh_radio: Option<RadioChoice>,
    // GPS
    latitude: Option<f64>,
    longitude: Option<f64>,
    // Process plugins — preserved verbatim through reconfigure
    process_plugins_toml: Option<String>,
}

fn load_existing(out_path: &Path) -> Existing {
    let toml_raw = fs::read_to_string(out_path).unwrap_or_default();
    let toml_val: toml::Value = toml_raw
        .parse()
        .unwrap_or(toml::Value::Table(Default::default()));

    // Preserve [[plugins.process]] entries verbatim so reconfigure doesn't
    // wipe process plugins that were added after initial setup.
    let process_plugins_toml = extract_process_plugins_block(&toml_raw);

    let bbs = toml_val.get("bbs");
    let mesh = toml_val.get("plugins").and_then(|p| p.get("mesh"));
    let meshtastic = toml_val.get("plugins").and_then(|p| p.get("meshtastic"));
    let web = toml_val.get("plugins").and_then(|p| p.get("web"));
    let location = toml_val.get("location");

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

    // MeshCore existing values
    let mesh_enabled = mesh
        .and_then(|m| m.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let mesh_connection_type = mesh
        .and_then(|m| m.get("connection_type"))
        .and_then(|v| v.as_str())
        .unwrap_or("serial")
        .to_owned();
    let mesh_serial_port = mesh
        .and_then(|m| m.get("serial_port"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let mesh_baud_rate = mesh
        .and_then(|m| m.get("baud_rate"))
        .and_then(|v| v.as_integer())
        .map(|v| v as u32)
        .unwrap_or(115_200);
    let mesh_addr = mesh
        .and_then(|m| m.get("addr"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    // Meshtastic existing values
    let meshtastic_enabled = meshtastic
        .and_then(|m| m.get("enabled"))
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let meshtastic_connection_type = meshtastic
        .and_then(|m| m.get("connection_type"))
        .and_then(|v| v.as_str())
        .unwrap_or("serial")
        .to_owned();
    let meshtastic_serial_port = meshtastic
        .and_then(|m| m.get("serial_port"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let meshtastic_baud_rate = meshtastic
        .and_then(|m| m.get("baud_rate"))
        .and_then(|v| v.as_integer())
        .map(|v| v as u32)
        .unwrap_or(115_200);
    let meshtastic_radio_region = meshtastic
        .and_then(|m| m.get("radio"))
        .and_then(|r| r.get("region"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let meshtastic_radio_preset = meshtastic
        .and_then(|m| m.get("radio"))
        .and_then(|r| r.get("modem_preset"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let meshtastic_short_name = meshtastic
        .and_then(|m| m.get("short_name"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);
    let meshtastic_long_name = meshtastic
        .and_then(|m| m.get("long_name"))
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    // Web
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

    // GPS
    let latitude = location
        .and_then(|l| l.get("latitude"))
        .and_then(|v| v.as_float());
    let longitude = location
        .and_then(|l| l.get("longitude"))
        .and_then(|v| v.as_float());

    // pymc-companion (HAT)
    let yaml_path = companion_yaml_path(out_path);
    let yaml = fs::read_to_string(&yaml_path).unwrap_or_default();

    // USB serial radio config — reconstruct from [plugins.mesh.radio]
    let mesh_radio: Option<RadioChoice> = {
        let radio = mesh.and_then(|m| m.get("radio"));
        if let Some(radio) = radio {
            // First try to match a named preset.
            let by_preset = radio
                .get("preset")
                .and_then(|v| v.as_str())
                .and_then(|name| REGION_PRESETS.iter().position(|r| r.name == name))
                .map(RadioChoice::Preset);
            if by_preset.is_some() {
                by_preset
            } else {
                // Fall back to reading the individual fields (custom config).
                let freq = radio
                    .get("frequency_hz")
                    .and_then(|v| v.as_integer())
                    .map(|v| v as u64);
                let bw = radio
                    .get("bandwidth_hz")
                    .and_then(|v| v.as_integer())
                    .map(|v| v as u32);
                let sf = radio
                    .get("spreading_factor")
                    .and_then(|v| v.as_integer())
                    .map(|v| v as u8);
                let cr = radio
                    .get("coding_rate")
                    .and_then(|v| v.as_integer())
                    .map(|v| v as u8);
                let tp = radio
                    .get("tx_power_dbm")
                    .and_then(|v| v.as_integer())
                    .map(|v| v as i32);
                if let (
                    Some(frequency_hz),
                    Some(bandwidth_hz),
                    Some(spreading_factor),
                    Some(coding_rate),
                    Some(tx_power_dbm),
                ) = (freq, bw, sf, cr, tp)
                {
                    Some(RadioChoice::Custom {
                        frequency_hz,
                        bandwidth_hz,
                        spreading_factor,
                        coding_rate,
                        tx_power_dbm,
                    })
                } else {
                    None
                }
            }
        } else {
            None
        }
    };

    Existing {
        bbs_name,
        data_dir,
        mesh_enabled,
        mesh_connection_type,
        mesh_serial_port,
        mesh_baud_rate,
        mesh_addr,
        meshtastic_enabled,
        meshtastic_connection_type,
        meshtastic_serial_port,
        meshtastic_baud_rate,
        meshtastic_radio_region,
        meshtastic_radio_preset,
        meshtastic_short_name,
        meshtastic_long_name,
        web_enabled,
        web_bind,
        web_backup_dir,
        region_idx: match_region_preset(&yaml),
        hat_idx: match_hat_preset(&yaml),
        mesh_radio,
        latitude,
        longitude,
        process_plugins_toml,
    }
}

/// Extract the raw `[[plugins.process]]` blocks from a TOML string.
/// Returns them as a block of text to be appended to the regenerated config,
/// or `None` if there are no process plugins.
fn extract_process_plugins_block(raw: &str) -> Option<String> {
    let mut in_block = false;
    let mut lines: Vec<&str> = Vec::new();

    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed == "[[plugins.process]]" {
            in_block = true;
            lines.push(line);
        } else if in_block {
            // Any section header that isn't another [[plugins.process]] ends the block.
            if trimmed.starts_with('[') {
                in_block = false;
            } else {
                lines.push(line);
            }
        }
    }

    // Remove trailing blank lines.
    while lines.last().map(|l| l.trim().is_empty()).unwrap_or(false) {
        lines.pop();
    }

    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
}

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

use crate::mesh_presets::REGION_PRESETS;
use dialoguer::{theme::ColorfulTheme, Confirm, Input, MultiSelect, Select};

/// What the operator chose in the "MeshCore radio parameters" step.
#[derive(Clone)]
enum RadioChoice {
    /// A named preset from REGION_PRESETS.
    Preset(usize),
    /// Custom values entered field-by-field.
    Custom {
        frequency_hz: u64,
        bandwidth_hz: u32,
        spreading_factor: u8,
        coding_rate: u8,
        tx_power_dbm: i32,
    },
}

// ── Public entry point ────────────────────────────────────────────────────────

pub fn run_wizard(config_out: Option<&Path>) {
    print_banner();

    let theme = ColorfulTheme::default();

    let out_path = config_out.map(|p| p.to_owned()).unwrap_or_else(|| {
        #[cfg(target_os = "linux")]
        {
            PathBuf::from("/etc/supply-drop-bbs/config.toml")
        }
        #[cfg(not(target_os = "linux"))]
        {
            PathBuf::from("config.toml")
        }
    });

    println!("Writing config to: {}", out_path.display());

    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            if let Err(e) = fs::create_dir_all(parent) {
                eprintln!(
                    "warning: could not create config directory {}: {e}",
                    parent.display()
                );
            }
        }
    }

    let ex = load_existing(&out_path);

    // ── BBS identity ──────────────────────────────────────────────────────────
    //
    // Collected first so it is clear there is exactly one BBS instance.
    // All radio transports (MeshCore, Meshtastic, …) configured below are
    // simply different ways to reach the same BBS — they do not have
    // separate identities.
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

    // ── Protocol selection ────────────────────────────────────────────────────
    //
    // Which protocols appear here is determined by compiled-in features:
    //   transport-mesh        → MeshCore offered
    //   transport-meshtastic  → Meshtastic offered
    //
    // If only one protocol is compiled in the selection step is skipped and
    // the wizard goes straight to configuring that protocol.

    #[allow(unused_mut)]
    let mut use_mesh = cfg!(feature = "transport-mesh") && ex.mesh_enabled;
    #[allow(unused_mut)]
    let mut use_meshtastic = cfg!(feature = "transport-meshtastic") && ex.meshtastic_enabled;

    let mesh_available = cfg!(feature = "transport-mesh");
    let meshtastic_available = cfg!(feature = "transport-meshtastic");

    if mesh_available && meshtastic_available {
        // Both compiled in — ask the operator which to enable.
        section("Radio protocols");

        println!("This BBS supports multiple radio protocols simultaneously.");
        println!("Select which protocols to enable (space to toggle, enter to confirm):");
        println!();

        let mut items = vec![];
        if mesh_available {
            items.push("MeshCore  (ZebraHat, Heltec V3, T-Beam, RAK — MeshCore firmware)");
        }
        if meshtastic_available {
            items.push("Meshtastic (any Meshtastic-firmware radio — USB or meshtasticd TCP)");
        }

        let defaults: Vec<bool> = vec![
            mesh_available && ex.mesh_enabled,
            meshtastic_available && ex.meshtastic_enabled,
        ];

        let selections = MultiSelect::with_theme(&theme)
            .with_prompt("Enable protocols")
            .items(&items)
            .defaults(&defaults)
            .interact()
            .unwrap_or_else(|_| cancelled());

        use_mesh = mesh_available && selections.contains(&0);
        use_meshtastic = meshtastic_available && selections.contains(&1);

        if !use_mesh && !use_meshtastic {
            println!();
            println!("  No protocols selected — the BBS will start with no radio transports.");
            println!("  You can still use the CLI transport (Unix socket) and web admin.");
        }
    } else if mesh_available {
        // Only MeshCore compiled in — always offer it.
        use_mesh = true;
        use_meshtastic = false;
    } else if meshtastic_available {
        // Only Meshtastic compiled in — always offer it.
        use_mesh = false;
        use_meshtastic = true;
    }

    // ── MeshCore connection ───────────────────────────────────────────────────

    let mesh_conn_type;
    let mesh_serial_port;
    let mesh_baud_rate;
    let mesh_addr: Option<String>;
    let mut hat_params: Option<HatParams> = None;

    if use_mesh {
        section("MeshCore radio connection");

        let conn_items = &[
            "USB / serial  (Heltec V3, T-Beam, RAK4631 — plug in via USB)",
            "Pi HAT        (ZebraHat, Waveshare, PiMesh, FemtoFox — SPI on GPIO)",
            "TCP           (connect to a running pymc_core, default port 5000)",
        ];
        let conn_default = if ex.mesh_connection_type == "hat" {
            1
        } else if ex.mesh_connection_type == "tcp" {
            2
        } else {
            0
        };
        let conn_choice = prompt_select(
            &theme,
            "How does your MeshCore radio connect?",
            conn_items,
            conn_default,
        );

        let (ct, sp, br, ma) = match conn_choice {
            0 => {
                let (ct, sp, br) =
                    configure_serial(&theme, ex.mesh_serial_port.as_deref(), ex.mesh_baud_rate);
                (ct, sp, br, None)
            }
            1 => ("hat", None, None, None),
            _ => {
                let addr: String = Input::with_theme(&theme)
                    .with_prompt("pymc_core address")
                    .default(
                        ex.mesh_addr
                            .clone()
                            .unwrap_or_else(|| "127.0.0.1:5000".to_owned()),
                    )
                    .interact_text()
                    .unwrap_or_else(|_| cancelled());
                ("tcp", None, None, Some(addr))
            }
        };

        mesh_conn_type = ct;
        mesh_serial_port = sp;
        mesh_baud_rate = br;
        mesh_addr = ma;
    } else {
        mesh_conn_type = "serial";
        mesh_serial_port = None;
        mesh_baud_rate = None;
        mesh_addr = None;
    }

    // ── MeshCore radio parameters (serial mode only) ──────────────────────────
    //
    // For HAT mode, radio parameters go into pymc-companion.yaml (handled
    // later by configure_hat). For TCP mode, pymc_core owns the radio config.
    // Only USB serial devices are configured here.

    let mesh_radio: Option<RadioChoice> = if use_mesh && mesh_conn_type == "serial" {
        section("MeshCore radio parameters");

        println!("Select a region preset to configure the radio, or choose Custom to");
        println!("enter individual LoRa parameters. Skip if the device is already");
        println!("on the correct frequency.");
        println!();

        let configure = Confirm::with_theme(&theme)
            .with_prompt("Configure radio parameters now?")
            .default(ex.mesh_radio.is_some())
            .interact()
            .unwrap_or_else(|_| cancelled());

        if configure {
            // Build the menu: all presets + "Custom…" at the end.
            let mut region_names: Vec<String> = REGION_PRESETS
                .iter()
                .map(|r| {
                    format!(
                        "{:<26} ({:.3} MHz)",
                        r.name,
                        r.frequency_hz as f64 / 1_000_000.0
                    )
                })
                .collect();
            region_names.push("Custom…".to_owned());
            let custom_idx = region_names.len() - 1;

            // Default selection: match existing choice, or USA/Canada (index 14).
            let default_region = match &ex.mesh_radio {
                Some(RadioChoice::Preset(i)) => *i,
                Some(RadioChoice::Custom { .. }) => custom_idx,
                None => 14, // USA/Canada
            };
            let choice = prompt_select(&theme, "Select your region", &region_names, default_region);

            if choice == custom_idx {
                // Recover previous custom values (if any) for defaults.
                let (prev_freq, prev_bw, prev_sf, prev_cr, prev_tp) =
                    if let Some(RadioChoice::Custom {
                        frequency_hz,
                        bandwidth_hz,
                        spreading_factor,
                        coding_rate,
                        tx_power_dbm,
                    }) = &ex.mesh_radio
                    {
                        (
                            *frequency_hz,
                            *bandwidth_hz,
                            *spreading_factor,
                            *coding_rate,
                            *tx_power_dbm,
                        )
                    } else {
                        // Sensible defaults (USA/Canada values)
                        let usa = &REGION_PRESETS[14];
                        (
                            usa.frequency_hz,
                            usa.bandwidth_hz,
                            usa.spreading_factor,
                            usa.coding_rate,
                            usa.tx_power_dbm,
                        )
                    };

                println!();
                println!("Enter each LoRa parameter. Press Enter to accept the default.");
                println!();

                let frequency_hz: u64 = Input::with_theme(&theme)
                    .with_prompt("Carrier frequency (Hz)")
                    .default(prev_freq)
                    .interact_text()
                    .unwrap_or_else(|_| cancelled());

                let bandwidth_hz: u32 = Input::with_theme(&theme)
                    .with_prompt("Channel bandwidth (Hz)  [e.g. 250000, 125000, 62500]")
                    .default(prev_bw)
                    .interact_text()
                    .unwrap_or_else(|_| cancelled());

                let spreading_factor: u8 = Input::with_theme(&theme)
                    .with_prompt("Spreading factor        [7–12]")
                    .default(prev_sf)
                    .validate_with(|v: &u8| {
                        if (7..=12).contains(v) {
                            Ok(())
                        } else {
                            Err("Spreading factor must be 7–12")
                        }
                    })
                    .interact_text()
                    .unwrap_or_else(|_| cancelled());

                let coding_rate: u8 = Input::with_theme(&theme)
                    .with_prompt("Coding rate             [5–8]  (5 = 4/5, 8 = 4/8)")
                    .default(prev_cr)
                    .validate_with(|v: &u8| {
                        if (5..=8).contains(v) {
                            Ok(())
                        } else {
                            Err("Coding rate must be 5–8")
                        }
                    })
                    .interact_text()
                    .unwrap_or_else(|_| cancelled());

                let tx_power_dbm: i32 = Input::with_theme(&theme)
                    .with_prompt("TX power (dBm)          [e.g. 20]")
                    .default(prev_tp)
                    .interact_text()
                    .unwrap_or_else(|_| cancelled());

                Some(RadioChoice::Custom {
                    frequency_hz,
                    bandwidth_hz,
                    spreading_factor,
                    coding_rate,
                    tx_power_dbm,
                })
            } else {
                Some(RadioChoice::Preset(choice))
            }
        } else {
            None
        }
    } else {
        None
    };

    // ── Meshtastic connection ─────────────────────────────────────────────────

    let meshtastic_conn_type;
    let meshtastic_serial_port;
    let meshtastic_baud_rate;
    let meshtastic_addr: Option<String>;

    if use_meshtastic {
        section("Meshtastic radio connection");

        let conn_items = &[
            "USB / serial  (any Meshtastic-firmware radio — plug in via USB)",
            "Pi HAT        (Waveshare LoRa HAT, RAK LoRa HAT — Meshtastic firmware via GPIO UART)",
            "TCP           (connect to a running meshtasticd, default port 4403)",
        ];
        let conn_default = if ex.meshtastic_connection_type == "hat" {
            1
        } else if ex.meshtastic_connection_type == "tcp" {
            2
        } else {
            0
        };
        let conn_choice = prompt_select(
            &theme,
            "How does your Meshtastic radio connect?",
            conn_items,
            conn_default,
        );

        match conn_choice {
            0 => {
                let (ct, sp, br) = configure_serial(
                    &theme,
                    ex.meshtastic_serial_port.as_deref(),
                    ex.meshtastic_baud_rate,
                );
                meshtastic_conn_type = ct;
                meshtastic_serial_port = sp;
                meshtastic_baud_rate = br;
                meshtastic_addr = None;
            }
            1 => {
                let (port, baud) = configure_uart_hat(&theme, ex.meshtastic_serial_port.as_deref());
                meshtastic_conn_type = "hat";
                meshtastic_serial_port = Some(port);
                meshtastic_baud_rate = Some(baud);
                meshtastic_addr = None;
            }
            _ => {
                let addr: String = Input::with_theme(&theme)
                    .with_prompt("meshtasticd address")
                    .default("127.0.0.1:4403".to_owned())
                    .interact_text()
                    .unwrap_or_else(|_| cancelled());
                meshtastic_conn_type = "tcp";
                meshtastic_serial_port = None;
                meshtastic_baud_rate = None;
                meshtastic_addr = Some(addr);
            }
        }
    } else {
        meshtastic_conn_type = "serial";
        meshtastic_serial_port = None;
        meshtastic_baud_rate = None;
        meshtastic_addr = None;
    }

    // ── Meshtastic radio parameters ───────────────────────────────────────────

    /// (code, display label)
    const MESHTASTIC_REGIONS: &[(&str, &str)] = &[
        ("US", "United States"),
        ("EU_433", "Europe 433 MHz"),
        ("EU_868", "Europe 868 MHz"),
        ("ANZ", "Australia / New Zealand"),
        ("JP", "Japan"),
        ("CN", "China"),
        ("KR", "South Korea"),
        ("TW", "Taiwan"),
        ("RU", "Russia"),
        ("IN", "India"),
        ("NZ_865", "New Zealand 865 MHz"),
        ("TH", "Thailand"),
        ("LORA_24", "2.4 GHz LoRa"),
        ("UA_433", "Ukraine 433 MHz"),
        ("UA_868", "Ukraine 868 MHz"),
        ("MY_433", "Malaysia 433 MHz"),
        ("MY_919", "Malaysia 919 MHz"),
        ("SG_923", "Singapore 923 MHz"),
    ];

    /// (code, display label)
    const MESHTASTIC_PRESETS: &[(&str, &str)] = &[
        (
            "LONG_FAST",
            "Long range, fast      (default — good starting point)",
        ),
        ("LONG_MODERATE", "Long range, moderate"),
        ("MEDIUM_SLOW", "Medium range, slow"),
        ("MEDIUM_FAST", "Medium range, fast"),
        ("SHORT_SLOW", "Short range, slow"),
        ("SHORT_FAST", "Short range, fast"),
        ("LONG_SLOW", "Long range, slow      (very slow data rate)"),
        (
            "MAX_RANGE",
            "Maximum range         (extremely slow data rate)",
        ),
        ("SHORT_TURBO", "Short range, turbo    (high data rate)"),
    ];

    let (meshtastic_radio_region, meshtastic_radio_preset): (Option<String>, Option<String>) =
        if use_meshtastic {
            section("Meshtastic radio parameters");

            println!("Select the region and modem preset for your Meshtastic device.");
            println!("These are saved to config.toml and can be pushed to the device");
            println!("at any time via the web admin (Settings → Meshtastic radio).");
            println!("Skip if the device is already configured correctly.");
            println!();

            let configure = Confirm::with_theme(&theme)
                .with_prompt("Configure Meshtastic radio parameters now?")
                .default(ex.meshtastic_radio_region.is_some())
                .interact()
                .unwrap_or_else(|_| cancelled());

            if configure {
                let region_labels: Vec<String> = MESHTASTIC_REGIONS
                    .iter()
                    .map(|(code, name)| format!("{code:<10} {name}"))
                    .collect();
                let default_region = ex
                    .meshtastic_radio_region
                    .as_deref()
                    .and_then(|r| MESHTASTIC_REGIONS.iter().position(|(c, _)| *c == r))
                    .unwrap_or(0); // US
                let region_choice =
                    prompt_select(&theme, "Select your region", &region_labels, default_region);
                let region = MESHTASTIC_REGIONS[region_choice].0.to_owned();

                println!();
                let preset_labels: Vec<String> = MESHTASTIC_PRESETS
                    .iter()
                    .map(|(_, d)| d.to_string())
                    .collect();
                let default_preset = ex
                    .meshtastic_radio_preset
                    .as_deref()
                    .and_then(|p| MESHTASTIC_PRESETS.iter().position(|(c, _)| *c == p))
                    .unwrap_or(0); // LONG_FAST
                let preset_choice = prompt_select(
                    &theme,
                    "Select modem preset",
                    &preset_labels,
                    default_preset,
                );
                let preset = MESHTASTIC_PRESETS[preset_choice].0.to_owned();

                (Some(region), Some(preset))
            } else {
                (
                    ex.meshtastic_radio_region.clone(),
                    ex.meshtastic_radio_preset.clone(),
                )
            }
        } else {
            (None, None)
        };

    // ── Meshtastic node name ──────────────────────────────────────────────────

    let (meshtastic_short_name, meshtastic_long_name): (Option<String>, Option<String>) =
        if use_meshtastic {
            section("Meshtastic node name");

            println!("Set the long name (full display name) and short name (≤ 4 chars,");
            println!("shown on mesh maps). These are saved to config.toml and can be");
            println!("pushed to the device via the web admin UI at any time.");
            println!();

            let configure = Confirm::with_theme(&theme)
                .with_prompt("Configure Meshtastic node name?")
                .default(ex.meshtastic_short_name.is_some() || ex.meshtastic_long_name.is_some())
                .interact()
                .unwrap_or_else(|_| cancelled());

            if configure {
                let long: String = Input::with_theme(&theme)
                    .with_prompt("Long name (full display name)")
                    .default(ex.meshtastic_long_name.clone().unwrap_or_default())
                    .interact_text()
                    .unwrap_or_else(|_| cancelled());

                let short: String = Input::with_theme(&theme)
                    .with_prompt("Short name (≤ 4 chars, shown on maps)")
                    .default(ex.meshtastic_short_name.clone().unwrap_or_default())
                    .validate_with(|v: &String| {
                        if v.chars().count() <= 4 {
                            Ok(())
                        } else {
                            Err("Short name must be 4 characters or fewer")
                        }
                    })
                    .interact_text()
                    .unwrap_or_else(|_| cancelled());

                let ln = if long.is_empty() { None } else { Some(long) };
                let sn = if short.is_empty() { None } else { Some(short) };
                (sn, ln)
            } else {
                (
                    ex.meshtastic_short_name.clone(),
                    ex.meshtastic_long_name.clone(),
                )
            }
        } else {
            (None, None)
        };

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

    // ── GPS location ──────────────────────────────────────────────────────────
    section("GPS location (optional)");

    println!("If set, radio transports broadcast your coordinates on connect so");
    println!("your node appears on mesh maps. Leave blank to skip.");
    println!();

    let set_gps = Confirm::with_theme(&theme)
        .with_prompt("Set GPS coordinates?")
        .default(ex.latitude.is_some())
        .interact()
        .unwrap_or_else(|_| cancelled());

    let (gps_lat, gps_lon) = if set_gps {
        let lat_default = ex.latitude.map(|v| format!("{v}")).unwrap_or_default();
        let lat_str: String = Input::with_theme(&theme)
            .with_prompt("Latitude  (decimal degrees, e.g. 37.7749)")
            .default(lat_default)
            .validate_with(|s: &String| -> Result<(), &str> {
                match s.parse::<f64>() {
                    Ok(v) if (-90.0..=90.0).contains(&v) => Ok(()),
                    _ => Err("must be a number between -90 and 90"),
                }
            })
            .interact_text()
            .unwrap_or_else(|_| cancelled());

        let lon_default = ex.longitude.map(|v| format!("{v}")).unwrap_or_default();
        let lon_str: String = Input::with_theme(&theme)
            .with_prompt("Longitude (decimal degrees, e.g. -122.4194)")
            .default(lon_default)
            .validate_with(|s: &String| -> Result<(), &str> {
                match s.parse::<f64>() {
                    Ok(v) if (-180.0..=180.0).contains(&v) => Ok(()),
                    _ => Err("must be a number between -180 and 180"),
                }
            })
            .interact_text()
            .unwrap_or_else(|_| cancelled());

        (
            Some(lat_str.parse::<f64>().expect("validated")),
            Some(lon_str.parse::<f64>().expect("validated")),
        )
    } else {
        (None, None)
    };

    // ── MeshCore Pi HAT: region + model ──────────────────────────────────────
    if use_mesh && mesh_conn_type == "hat" {
        hat_params = Some(configure_hat(
            &theme,
            &bbs_name,
            &data_dir,
            ex.region_idx,
            ex.hat_idx,
        ));
    }

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
        use_mesh,
        mesh_connection_type: mesh_conn_type,
        mesh_serial_port: mesh_serial_port.as_deref(),
        mesh_baud_rate,
        mesh_addr: mesh_addr.as_deref(),
        use_meshtastic,
        meshtastic_connection_type: meshtastic_conn_type,
        meshtastic_serial_port: meshtastic_serial_port.as_deref(),
        meshtastic_baud_rate,
        meshtastic_addr: meshtastic_addr.as_deref(),
        meshtastic_radio_region: meshtastic_radio_region.as_deref(),
        meshtastic_radio_preset: meshtastic_radio_preset.as_deref(),
        meshtastic_short_name: meshtastic_short_name.as_deref(),
        meshtastic_long_name: meshtastic_long_name.as_deref(),
        web_enabled,
        web_bind: web_bind.as_deref(),
        web_backup_dir: web_backup_dir.as_deref(),
        latitude: gps_lat,
        longitude: gps_lon,
        process_plugins_toml: ex.process_plugins_toml.as_deref(),
        mesh_radio: mesh_radio.as_ref(),
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

    // On Linux, set ownership of the config file (and its parent directory)
    // so the BBS service process can update config.toml from the web admin.
    // Succeeds when setup is run as root (e.g. sudo supply-drop-bbs setup).
    // If it fails, a manual command is printed in the next-steps section.
    #[cfg(target_os = "linux")]
    let config_chown_ok = {
        const SERVICE_USER: &str = "supply-drop";
        let chown_arg = format!("{SERVICE_USER}:{SERVICE_USER}");
        // Chown the parent directory so the service can atomically rewrite the
        // file.  Track whether this succeeds — if it fails, the web admin
        // still cannot save config even if the file itself was chowned.
        let dir_ok = match out_path.parent() {
            Some(parent) if !parent.as_os_str().is_empty() => matches!(
                std::process::Command::new("chown")
                    .args([chown_arg.as_str(), &parent.to_string_lossy()])
                    .status(),
                Ok(s) if s.success()
            ),
            // No meaningful parent (e.g. path is just a filename with no
            // directory component) — nothing to chown, not a failure.
            _ => true,
        };
        let file_ok = matches!(
            std::process::Command::new("chown")
                .args([chown_arg.as_str(), &out_path.to_string_lossy()])
                .status(),
            Ok(s) if s.success()
        );
        // Both the directory AND the file must be re-owned for the web admin
        // to write config.toml atomically.
        let both_ok = dir_ok && file_ok;
        if both_ok {
            println!(
                "  ownership set to {SERVICE_USER}:{SERVICE_USER} (web admin can save config)"
            );
        }
        both_ok
    };
    #[cfg(not(target_os = "linux"))]
    let config_chown_ok = true;

    if let Some(ref dir) = web_backup_dir {
        if !dir.is_empty() {
            match fs::create_dir_all(dir) {
                Ok(()) => {
                    println!("Backup directory created: {dir}");
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

    if let Some(ref hat) = hat_params {
        let yaml_path = companion_yaml_path(&out_path);
        let yaml = build_companion_yaml(hat);
        if let Err(e) = fs::write(&yaml_path, &yaml) {
            eprintln!("error: could not write {}: {e}", yaml_path.display());
            std::process::exit(1);
        }
        println!("HAT config written to {}.", yaml_path.display());
    }

    // Meshtastic radio and node-name settings are applied to the device
    // automatically by the transport when the BBS connects (see the
    // auto-apply-on-connect logic in bbs-meshtastic).  No setup-time push is
    // needed — the operator just starts the BBS and the settings take effect.

    // ── Next steps ────────────────────────────────────────────────────────────
    section("Next steps");
    print_next_steps(
        use_mesh,
        mesh_conn_type,
        mesh_serial_port.as_deref(),
        use_meshtastic,
        meshtastic_conn_type,
        web_bind.as_deref(),
        &out_path,
        config_chown_ok,
    );
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

fn configure_uart_hat(theme: &ColorfulTheme, existing_port: Option<&str>) -> (String, u32) {
    println!();
    println!("The Meshtastic Pi HAT connects over the GPIO UART pins.");
    println!("You may need to enable UART and disable the serial console.");
    println!("Run 'sudo raspi-config' → Interface Options → Serial Port.");
    println!();

    const UART_OPTIONS: &[(&str, &str)] = &[
        ("/dev/ttyAMA0", "primary UART — most Pi setups"),
        ("/dev/serial0", "symlink to primary UART"),
        ("/dev/ttyS0", "mini UART — Pi 3/4 without overlay"),
    ];

    let mut items: Vec<String> = UART_OPTIONS
        .iter()
        .map(|(dev, desc)| format!("{dev:<20} ({desc})"))
        .collect();
    items.push("Enter path manually…".into());

    let default_idx = existing_port
        .and_then(|ep| UART_OPTIONS.iter().position(|(dev, _)| *dev == ep))
        .unwrap_or(0);

    let choice = prompt_select(theme, "Select UART port", &items, default_idx);

    let port = if choice == UART_OPTIONS.len() {
        let mut prompt = Input::with_theme(theme).with_prompt("UART port path");
        if let Some(p) = existing_port {
            prompt = prompt.default(p.to_owned());
        }
        prompt.interact_text().unwrap_or_else(|_| cancelled())
    } else {
        UART_OPTIONS[choice].0.to_owned()
    };

    (port, 115_200)
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
    section("MeshCore Pi HAT — region");

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

    section("MeshCore Pi HAT — model");

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
    // MeshCore
    use_mesh: bool,
    mesh_connection_type: &'a str,
    mesh_serial_port: Option<&'a str>,
    mesh_baud_rate: Option<u32>,
    mesh_addr: Option<&'a str>,
    // Meshtastic
    use_meshtastic: bool,
    meshtastic_connection_type: &'a str,
    meshtastic_serial_port: Option<&'a str>,
    meshtastic_baud_rate: Option<u32>,
    meshtastic_addr: Option<&'a str>,
    meshtastic_radio_region: Option<&'a str>,
    meshtastic_radio_preset: Option<&'a str>,
    meshtastic_short_name: Option<&'a str>,
    meshtastic_long_name: Option<&'a str>,
    // Web
    web_enabled: bool,
    web_bind: Option<&'a str>,
    web_backup_dir: Option<&'a str>,
    // GPS
    latitude: Option<f64>,
    longitude: Option<f64>,
    // Process plugins — preserved verbatim from the previous config
    process_plugins_toml: Option<&'a str>,
    // USB serial radio config (None = omit section)
    mesh_radio: Option<&'a RadioChoice>,
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

    // [location]
    if let (Some(lat), Some(lon)) = (p.latitude, p.longitude) {
        writeln!(s, "\n[location]").unwrap();
        writeln!(s, "latitude  = {lat}").unwrap();
        writeln!(s, "longitude = {lon}").unwrap();
    }

    // [plugins.mesh]
    writeln!(s, "\n[plugins.mesh]").unwrap();
    writeln!(s, "enabled = {}", p.use_mesh).unwrap();
    if p.use_mesh {
        writeln!(s, "connection_type = {}", toml_str(p.mesh_connection_type)).unwrap();
        match p.mesh_connection_type {
            "serial" => {
                if let Some(port) = p.mesh_serial_port {
                    writeln!(s, "serial_port = {}", toml_str(port)).unwrap();
                }
                if let Some(baud) = p.mesh_baud_rate {
                    if baud != 115_200 {
                        writeln!(s, "baud_rate = {baud}").unwrap();
                    }
                }
            }
            "tcp" => {
                if let Some(addr) = p.mesh_addr {
                    if addr != "127.0.0.1:5000" {
                        writeln!(s, "addr = {}", toml_str(addr)).unwrap();
                    }
                }
            }
            _ => {}
        }
    }

    // [plugins.mesh.radio] — only for USB serial with a radio config chosen
    if p.use_mesh && p.mesh_connection_type == "serial" {
        match p.mesh_radio {
            Some(RadioChoice::Preset(idx)) => {
                if let Some(preset) = REGION_PRESETS.get(*idx) {
                    writeln!(s, "\n[plugins.mesh.radio]").unwrap();
                    writeln!(s, "preset           = {}", toml_str(preset.name)).unwrap();
                    writeln!(s, "frequency_hz     = {}", preset.frequency_hz).unwrap();
                    writeln!(s, "bandwidth_hz     = {}", preset.bandwidth_hz).unwrap();
                    writeln!(s, "spreading_factor = {}", preset.spreading_factor).unwrap();
                    writeln!(s, "coding_rate      = {}", preset.coding_rate).unwrap();
                    writeln!(s, "tx_power_dbm     = {}", preset.tx_power_dbm).unwrap();
                }
            }
            Some(RadioChoice::Custom {
                frequency_hz,
                bandwidth_hz,
                spreading_factor,
                coding_rate,
                tx_power_dbm,
            }) => {
                writeln!(s, "\n[plugins.mesh.radio]").unwrap();
                writeln!(s, "frequency_hz     = {frequency_hz}").unwrap();
                writeln!(s, "bandwidth_hz     = {bandwidth_hz}").unwrap();
                writeln!(s, "spreading_factor = {spreading_factor}").unwrap();
                writeln!(s, "coding_rate      = {coding_rate}").unwrap();
                writeln!(s, "tx_power_dbm     = {tx_power_dbm}").unwrap();
            }
            None => {}
        }
    }

    // [plugins.meshtastic] — only written when the feature is compiled in
    #[cfg(feature = "transport-meshtastic")]
    {
        writeln!(s, "\n[plugins.meshtastic]").unwrap();
        writeln!(s, "enabled = {}", p.use_meshtastic).unwrap();
        if p.use_meshtastic {
            writeln!(
                s,
                "connection_type = {}",
                toml_str(p.meshtastic_connection_type)
            )
            .unwrap();
            match p.meshtastic_connection_type {
                "serial" | "hat" => {
                    if let Some(port) = p.meshtastic_serial_port {
                        writeln!(s, "serial_port = {}", toml_str(port)).unwrap();
                    }
                    if let Some(baud) = p.meshtastic_baud_rate {
                        if baud != 115_200 {
                            writeln!(s, "baud_rate = {baud}").unwrap();
                        }
                    }
                }
                "tcp" => {
                    if let Some(addr) = p.meshtastic_addr {
                        if addr != "127.0.0.1:4403" {
                            writeln!(s, "addr = {}", toml_str(addr)).unwrap();
                        }
                    }
                }
                _ => {}
            }
            if let Some(sn) = p.meshtastic_short_name {
                writeln!(s, "short_name = {}", toml_str(sn)).unwrap();
            }
            if let Some(ln) = p.meshtastic_long_name {
                writeln!(s, "long_name  = {}", toml_str(ln)).unwrap();
            }
        }
    }
    // [plugins.meshtastic.radio] — only when Meshtastic is enabled and params chosen
    #[cfg(feature = "transport-meshtastic")]
    if p.use_meshtastic
        && (p.meshtastic_radio_region.is_some() || p.meshtastic_radio_preset.is_some())
    {
        writeln!(s, "\n[plugins.meshtastic.radio]").unwrap();
        if let Some(region) = p.meshtastic_radio_region {
            writeln!(s, "region       = {}", toml_str(region)).unwrap();
        }
        if let Some(preset) = p.meshtastic_radio_preset {
            writeln!(s, "modem_preset = {}", toml_str(preset)).unwrap();
        }
    }

    // Suppress unused-variable warnings when feature is off.
    #[cfg(not(feature = "transport-meshtastic"))]
    {
        let _ = (
            p.use_meshtastic,
            p.meshtastic_connection_type,
            p.meshtastic_serial_port,
            p.meshtastic_baud_rate,
            p.meshtastic_addr,
            p.meshtastic_radio_region,
            p.meshtastic_radio_preset,
            p.meshtastic_short_name,
            p.meshtastic_long_name,
        );
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

    // [[plugins.process]] — preserved verbatim from previous config
    if let Some(block) = p.process_plugins_toml {
        writeln!(s).unwrap();
        writeln!(s, "{block}").unwrap();
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

fn usb_port_hint(vid: u16, pid: u16) -> Option<&'static str> {
    match (vid, pid) {
        // Silicon Labs CP2102 / CP2102N — Heltec V3, T-Beam, RAK4631
        (0x10C4, 0xEA60) => Some("Meshtastic/MeshCore radio — CP2102"),
        // WCH CH340 — common on budget ESP32 boards
        (0x1A86, 0x7523) => Some("Meshtastic radio — CH340"),
        // WCH CH9102 — newer Heltec and TTGO boards
        (0x1A86, 0x55D4) => Some("Meshtastic radio — CH9102"),
        // Espressif native USB (ESP32-S3) — Heltec V3 alt mode, some T-Beam S3
        (0x303A, 0x1001) => Some("Meshtastic radio — ESP32-S3 native USB"),
        // MaxLinear/Exar XR21V1410/1412 — pymc-companion MeshCore HAT
        (0x04E2, 0x1410 | 0x1412 | 0x1414) => Some("MeshCore HAT — XR serial"),
        _ => None,
    }
}

fn list_serial_ports() -> Vec<PortInfo> {
    match tokio_serial::available_ports() {
        Err(_) => vec![],
        Ok(ports) => ports
            .into_iter()
            .map(|p| {
                let description = match &p.port_type {
                    tokio_serial::SerialPortType::UsbPort(info) => {
                        let hint = usb_port_hint(info.vid, info.pid);
                        let mut parts: Vec<&str> = Vec::new();
                        if let Some(ref mfr) = info.manufacturer {
                            parts.push(mfr);
                        }
                        if let Some(ref prod) = info.product {
                            parts.push(prod);
                        }
                        let base = if parts.is_empty() {
                            "USB".to_owned()
                        } else {
                            parts.join(" ")
                        };
                        if let Some(h) = hint {
                            Some(format!("{base} — {h}"))
                        } else {
                            Some(base)
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

#[allow(clippy::too_many_arguments)]
fn print_next_steps(
    use_mesh: bool,
    mesh_conn_type: &str,
    mesh_serial_port: Option<&str>,
    use_meshtastic: bool,
    meshtastic_conn_type: &str,
    web_bind: Option<&str>,
    config_path: &std::path::Path,
    config_chown_ok: bool,
) {
    if use_mesh && mesh_conn_type == "tcp" {
        println!("MeshCore TCP mode: Supply Drop BBS will connect to pymc_core at");
        println!("the configured address. Make sure pymc_core is running before");
        println!("starting the BBS.");
        println!();
    }

    let needs_dialout = cfg!(target_os = "linux")
        && ((use_mesh && mesh_conn_type == "serial")
            || (use_meshtastic && matches!(meshtastic_conn_type, "serial" | "hat")));

    if needs_dialout {
        println!("To allow Supply Drop BBS to access serial ports, your user must");
        println!("be in the 'dialout' group:");
        println!();
        println!("  sudo usermod -aG dialout $USER");
        println!("  # then log out and back in, or run:");
        println!("  newgrp dialout");
        println!();
        if use_mesh && mesh_conn_type == "serial" {
            if let Some(port) = mesh_serial_port {
                println!("Verify MeshCore port access with:");
                println!("  ls -l {port}");
                println!();
            }
        }
    }

    // Node key hint (MeshCore serial only — not applicable for TCP / HAT / Meshtastic)
    if use_mesh && mesh_conn_type == "serial" {
        println!("MeshCore node key:");
        println!();
        println!("  To see the current node key (public key):");
        println!("    supply-drop-bbs node show-key");
        println!();
        println!("  To back up the private key before a firmware flash:");
        println!("    supply-drop-bbs node export-key");
        println!();
        println!("  To restore or migrate to a new 64-char hex key:");
        println!("    supply-drop-bbs node import-key <64-char-hex>");
        println!();
        println!("  The BBS service must not be running when using these commands.");
        println!();
    }

    // Meshtastic settings note
    if use_meshtastic {
        println!("Meshtastic radio and node settings:");
        println!();
        println!("  The region, modem preset, and node name were saved to config.toml.");
        println!("  They are applied to the device automatically when the BBS connects —");
        println!("  just start the BBS and they take effect. You can change them any time");
        println!("  from Settings in the web admin UI (also applied automatically).");
        println!();
    }

    if cfg!(target_os = "linux") {
        println!("To run Supply Drop BBS as a systemd service:");
        println!();
        println!("  sudo install -m 644 supply-drop-bbs.service /etc/systemd/system/");
        println!("  sudo systemctl daemon-reload");
        println!("  sudo systemctl enable --now supply-drop-bbs");
        println!();

        // If ownership was not set during setup (not run as root, or the
        // service user didn't exist yet), remind the operator to do it
        // manually so the web admin can save config changes.
        if !config_chown_ok {
            println!("After the service user is created, allow the BBS to update config.toml:");
            println!();
            println!(
                "  sudo chown supply-drop-bbs:supply-drop-bbs {}",
                config_path.display()
            );
            if let Some(parent) = config_path.parent() {
                if !parent.as_os_str().is_empty() {
                    println!(
                        "  sudo chown supply-drop-bbs:supply-drop-bbs {}",
                        parent.display()
                    );
                }
            }
            println!();
        }

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
    println!();
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!("  Supply Drop BBS is an open source project by");
    println!("  Mesh America — building resilient, community-");
    println!("  owned mesh networks for everyone.");
    println!();
    println!("    https://meshamerica.com");
    println!();
    println!("  If this software is useful to you, please");
    println!("  consider supporting our mission:");
    println!();
    println!("    https://meshamerica.com/pitch-in/");
    println!("━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━");
    println!();
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
