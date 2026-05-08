#![allow(missing_docs)]

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let web_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap()).join("web");

    // Trigger a rebuild when frontend source or config changes.
    for path in [
        "web/src",
        "web/index.html",
        "web/package.json",
        "web/vite.config.ts",
        "web/tsconfig.json",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }

    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };

    let install = Command::new(npm)
        .arg("install")
        .current_dir(&web_dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to run `npm install`: {e}"));

    if !install.success() {
        panic!("`npm install` exited with {install}");
    }

    let build = Command::new(npm)
        .args(["run", "build"])
        .current_dir(&web_dir)
        .status()
        .unwrap_or_else(|e| panic!("failed to run `npm run build`: {e}"));

    if !build.success() {
        panic!("`npm run build` exited with {build}");
    }
}
