//! Supply Drop BBS — entry point.
//!
//! For now this is a stub binary that prints a startup banner and
//! the set of features compiled in, then exits. The real entry
//! point grows in subsequent commits as the host supervisor,
//! config loader, and plugin registry come online.
//!
//! Architecture: see `docs/ARCHITECTURE.md`.

fn main() {
    print_banner();
    print_compiled_features();
    println!();
    println!("This binary is currently a stub. Real implementation");
    println!("has not yet begun. See the `docs/` directory in the");
    println!("repository for the architecture and roadmap.");
}

fn print_banner() {
    println!(
        "{name} {version}",
        name = env!("CARGO_PKG_NAME"),
        version = env!("CARGO_PKG_VERSION"),
    );
    println!("{}", env!("CARGO_PKG_DESCRIPTION"));
    println!("{}", env!("CARGO_PKG_REPOSITORY"));
}

fn print_compiled_features() {
    println!();
    println!("Compiled-in plugins:");

    let plugins: &[(&str, bool)] = &[
        ("transport-cli", cfg!(feature = "transport-cli")),
        ("transport-mesh", cfg!(feature = "transport-mesh")),
        ("admin-web", cfg!(feature = "admin-web")),
    ];

    for (name, enabled) in plugins {
        let mark = if *enabled { "[x]" } else { "[ ]" };
        println!("  {mark} {name}");
    }
}
