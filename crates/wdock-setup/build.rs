//! Embeds the programs the setup installs:
//!
//! * wdock: `WDOCK_EXE` if set, otherwise the workspace's release build
//!   (`cargo build --release -p wdock` first);
//! * agent-usage-bar (optional): `AGENT_USAGE_EXE` if set, otherwise the
//!   release build in a sibling `agent-usage-bar` checkout.
//!
//! A missing program is embedded as empty: without wdock the setup refuses
//! to install, and without agent-usage-bar it simply does not offer it.

use std::path::{Path, PathBuf};

fn main() {
    // Common controls v6: themed checkboxes, combo boxes and buttons.
    println!("cargo:rustc-link-arg-bins=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg-bins=/MANIFESTDEPENDENCY:type='win32' name='Microsoft.Windows.Common-Controls' version='6.0.0.0' processorArchitecture='*' publicKeyToken='6595b64144ccf1df' language='*'"
    );
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    embed(&out, "wdock.exe", "WDOCK_EXE", &manifest.join("../../target/release/wdock.exe"));
    embed(
        &out,
        "agent-usage-bar.exe",
        "AGENT_USAGE_EXE",
        &manifest.join("../../../agent-usage-bar/target/release/agent-usage-bar.exe"),
    );
}

fn embed(out: &Path, name: &str, env: &str, default: &Path) {
    let source = std::env::var_os(env).map(PathBuf::from).unwrap_or_else(|| default.to_owned());
    println!("cargo:rerun-if-env-changed={env}");
    println!("cargo:rerun-if-changed={}", source.display());
    let bytes = std::fs::read(&source).unwrap_or_else(|_| {
        println!("cargo:warning=no {name} at {}; the setup will not include it", source.display());
        Vec::new()
    });
    std::fs::write(out.join(name), bytes).unwrap();
}
