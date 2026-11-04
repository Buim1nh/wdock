#!/usr/bin/env bash
# Runs the wdock unit tests on Linux through Wine (needs mingw-w64 and wine).
# Skipped tests need things Wine lacks: Windows fonts, OLE drag-and-drop,
# WinRT Windows.Data.Json. Run the full suite on Windows with `cargo test`.
set -euo pipefail
rustup target add x86_64-pc-windows-gnu >/dev/null
export CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUNNER=wine WINEDEBUG=-all
cargo test --release -p wdock --target x86_64-pc-windows-gnu -- \
  --skip island_drag::tests \
  --skip font_row_cycles_installed_families \
  --skip missing_font_falls_back_to_an_installed_family \
  --skip spotify_payload_validates_line_indices "$@"
