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

    // The release workflow cross-compiles inside cross-rs's Docker images
    // (Ubuntu 16.04-based, no Node.js/npm) — it builds the frontend once on
    // the runner itself beforehand and sets this to skip re-running npm
    // inside the container, where it would fail outright. Unset in every
    // other context (local dev, CI's native build), so this doesn't change
    // ordinary behavior.
    if std::env::var_os("BBS_WEB_SKIP_NPM_BUILD").is_some() {
        let index_html = web_dir.join("dist").join("index.html");
        if !index_html.exists() {
            panic!(
                "BBS_WEB_SKIP_NPM_BUILD is set but {} is missing — build the frontend \
                 first (npm ci && npm run build in {}) before compiling with this set",
                index_html.display(),
                web_dir.display()
            );
        }
        println!(
            "cargo:warning=BBS_WEB_SKIP_NPM_BUILD set — using pre-built web/dist/, not running npm"
        );
        return;
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
