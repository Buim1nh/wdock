//! Embeds the wdock binary: `WDOCK_EXE` if set, otherwise the workspace's
//! release build. Build `wdock` first (`cargo build --release -p wdock`).
//! Without it the setup still compiles, but refuses to install.

use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let source = std::env::var_os("WDOCK_EXE")
        .map(PathBuf::from)
        .unwrap_or_else(|| manifest.join("../../target/release/wdock.exe"));
    println!("cargo:rerun-if-env-changed=WDOCK_EXE");
    println!("cargo:rerun-if-changed={}", source.display());
    let payload = PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("wdock.exe");
    let bytes = std::fs::read(&source).unwrap_or_else(|_| {
        println!("cargo:warning=no wdock.exe at {}; this setup will not install anything", source.display());
        Vec::new()
    });
    std::fs::write(payload, bytes).unwrap();
}
