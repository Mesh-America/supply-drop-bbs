//! Interactive first-run setup wizard.
//!
//! Guides the operator through:
//!
//! 1. Choosing a radio connection type (USB serial / TCP / Pi HAT)
//! 2. Configuring the connection (port selection or address entry)
//! 3. Setting BBS identity (name, data directory)
//! 4. Writing a `config.toml`
//! 5. Printing platform-specific next steps (group membership, systemd)
//!
//! Entry point: [`run_wizard`].

#![allow(clippy::print_stdout, clippy::print_stderr)]

use std::{
    fmt::Write as FmtWrite,
    fs,
    path::{Path, PathBuf},
};

use dialoguer::{theme::ColorfulTheme, Confirm, Input, Password, Select};

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
        "USB / serial  (Heltec V3, T-Beam, RAK4631 — no pymc_core required)",
        "TCP / pymc_core  (Pi HAT or remote pymc_core bridge)",
    ];

    let conn_choice = prompt_select(&theme, "How does your radio connect?", conn_items, 0);

    let (connection_type, serial_port, baud_rate, tcp_addr) = match conn_choice {
        0 => configure_serial(&theme),
        _ => configure_tcp(&theme),
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

    let default_data_dir = dirs::data_local_dir()
        .map(|d| d.join("supply-drop-bbs"))
        .unwrap_or_else(|| PathBuf::from("/var/lib/supply-drop-bbs"));

    let data_dir_str: String = Input::with_theme(&theme)
        .with_prompt("Data directory")
        .default(default_data_dir.to_string_lossy().into_owned())
        .interact_text()
        .unwrap_or_else(|_| cancelled());

    let data_dir = PathBuf::from(&data_dir_str);

    // ── Web admin ─────────────────────────────────────────────────────────────
    section("Web admin UI");

    println!("The web admin lets you monitor adverts, logs, and send adverts from a browser.");
    println!("It binds to your Pi's network address so you can reach it from another device.");
    println!();

    let admin_password = loop {
        let pw: String = Password::with_theme(&theme)
            .with_prompt("Admin password")
            .with_confirmation("Confirm password", "Passwords do not match — try again")
            .interact()
            .unwrap_or_else(|_| cancelled());

        if pw.len() < 8 {
            println!("  Password must be at least 8 characters — try again.");
            continue;
        }
        break pw;
    };

    let web_bind: String = Input::with_theme(&theme)
        .with_prompt("Web admin bind address")
        .default("0.0.0.0:8080".into())
        .interact_text()
        .unwrap_or_else(|_| cancelled());

    // ── Confirm & write ───────────────────────────────────────────────────────
    section("Write config");

    let out_path = config_out
        .map(|p| p.to_owned())
        .unwrap_or_else(|| PathBuf::from("config.toml"));

    println!("\nConfig will be written to: {}", out_path.display());
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
        tcp_addr: tcp_addr.as_deref(),
        admin_password: &admin_password,
        web_bind: &web_bind,
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

    // ── Next steps ────────────────────────────────────────────────────────────
    section("Next steps");
    print_next_steps(connection_type, serial_port.as_deref(), &web_bind);
}

// ── Connection type configuration ─────────────────────────────────────────────

/// USB / serial flow: list ports, let operator pick one, ask baud rate.
///
/// Returns `(connection_type, serial_port, baud_rate, tcp_addr)`.
fn configure_serial(
    theme: &ColorfulTheme,
) -> (&'static str, Option<String>, Option<u32>, Option<String>) {
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
        // Build display strings for the menu.
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
            // "Enter manually" chosen.
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

    ("serial", Some(serial_port), Some(baud), None)
}

/// TCP / HAT flow: choose tcp vs hat, enter address.
///
/// Returns `(connection_type, serial_port, baud_rate, tcp_addr)`.
fn configure_tcp(
    theme: &ColorfulTheme,
) -> (&'static str, Option<String>, Option<u32>, Option<String>) {
    let type_items = &[
        "tcp  — standalone pymc_core or remote bridge",
        "hat  — pymc_core managing a Pi HAT (GPIO/SPI) — setup wizard will show HAT hints",
    ];

    let type_choice = prompt_select(theme, "Connection sub-type", type_items, 0);
    let connection_type = if type_choice == 0 { "tcp" } else { "hat" };

    let addr: String = Input::with_theme(theme)
        .with_prompt("pymc_core address (host:port)")
        .default("127.0.0.1:5000".into())
        .validate_with(|s: &String| -> Result<(), &str> {
            if s.parse::<std::net::SocketAddr>().is_ok() {
                Ok(())
            } else {
                Err("enter a valid host:port — e.g. 127.0.0.1:5000")
            }
        })
        .interact_text()
        .unwrap_or_else(|_| cancelled());

    if connection_type == "hat" {
        println!();
        println!("  Pi HAT setup notes:");
        println!("  • Make sure pymc_core is installed and running as a systemd service.");
        println!("  • Ensure UART is enabled (raspi-config → Interface Options → Serial Port).");
        println!("  • The setup wizard does not yet auto-configure pymc_core — see");
        println!("    docs/OPERATIONS.md for the HAT setup steps.");
    }

    (connection_type, None, None, Some(addr))
}

// ── TOML builder ──────────────────────────────────────────────────────────────

/// Build a minimal TOML config string from wizard answers.
///
/// Only non-default values are written.  Operators can run
/// `supply-drop-bbs config show` to see all effective values.
struct TomlParams<'a> {
    bbs_name: &'a str,
    data_dir: &'a Path,
    connection_type: &'a str,
    serial_port: Option<&'a str>,
    baud_rate: Option<u32>,
    tcp_addr: Option<&'a str>,
    admin_password: &'a str,
    web_bind: &'a str,
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
        "tcp" | "hat" => {
            if let Some(addr) = p.tcp_addr {
                if addr != "127.0.0.1:5000" {
                    writeln!(s, "addr = {}", toml_str(addr)).unwrap();
                }
            }
        }
        _ => {}
    }

    // [plugins.web]
    writeln!(s, "\n[plugins.web]").unwrap();
    writeln!(s, "admin_password = {}", toml_str(p.admin_password)).unwrap();
    if p.web_bind != "127.0.0.1:8080" {
        writeln!(s, "bind = {}", toml_str(p.web_bind)).unwrap();
    }
    writeln!(s, "cookie_secure = false").unwrap();

    s
}

/// TOML-quote a string value (using basic double-quoted strings).
fn toml_str(s: &str) -> String {
    // Escape backslashes and double-quotes; everything else is printable ASCII
    // or valid UTF-8 which TOML accepts verbatim in a basic string.
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

fn print_next_steps(connection_type: &str, serial_port: Option<&str>, web_bind: &str) {
    // Serial-specific: dialout group on Linux.
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

    // HAT: pymc_core service reminder.
    if connection_type == "hat" && cfg!(target_os = "linux") {
        println!("For Pi HAT mode, make sure pymc_core is running:");
        println!();
        println!("  sudo systemctl status pymc-core");
        println!();
        println!("See docs/OPERATIONS.md for the full HAT setup instructions.");
        println!();
    }

    // systemd (Linux).
    if cfg!(target_os = "linux") {
        println!("To run Supply Drop BBS as a systemd service:");
        println!();
        println!("  # Copy or install the unit file:");
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

    // Web admin URL hint.
    let display_bind = if web_bind.starts_with("0.0.0.0") {
        web_bind.replacen("0.0.0.0", "<your-pi-ip>", 1)
    } else {
        web_bind.to_owned()
    };
    println!("Once running, open the web admin at:");
    println!();
    println!("  http://{display_bind}");
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

/// Show a `Select` prompt and return the chosen index.
///
/// Accepts a slice of anything that implements `ToString`.
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

/// Called when a dialoguer prompt is interrupted (Ctrl-C or I/O error).
fn cancelled() -> ! {
    println!("\nSetup cancelled.");
    std::process::exit(0);
}
