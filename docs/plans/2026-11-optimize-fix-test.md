# Plan: optimize, fix bugs, test (cloud run)

Runs in a Linux cloud container on branch `claude/adoring-fermi-7u2rg3`.
wdock is Windows-only, so nothing here can be *run*; everything is verified by
cross-target `cargo check`/`clippy` and by tests that don't touch Win32.
Respect every rule in `AGENTS.md` (resources, anti-cheat, no network,
recoverable). Commit after each phase so a cut-off session loses little.

## Phase 0 — toolchain (≈5 min)

```bash
rustup target add x86_64-pc-windows-msvc
cargo check  --release --workspace --target x86_64-pc-windows-msvc
cargo clippy --release --workspace --target x86_64-pc-windows-msvc
```

`check`/`clippy` don't link, so no MSVC is needed. If a build script
(winres/embed-resource) needs `rc.exe`, try `cargo-xwin`; if that fails, note it
and limit verification to crates/modules that build. Record the baseline
warning count. Tests can't run here (`cargo test` must link a Windows exe); try
`cargo xwin test --no-run` only to prove they compile.

## Phase 1 — bug hunt (highest value)

Priorities, by crash/state-loss risk:

1. **Panics in the shell process.** ~110 `unwrap()`/TODOs; hot spots:
   `model/island_files.rs` (21), `services/lyrics_bridge.rs` (16),
   `ui/island.rs` (14), `platform/island_drag.rs` (9), `model/tray.rs` (8),
   `platform/tray_proto.rs` (5). Replace unwraps on OS/IO/socket/parse results
   with fail-soft handling + `log::warn!`. Leave unwraps on true invariants
   (locks, constants) alone.
2. **Lyrics bridge** (`services/lyrics_bridge.rs`): malformed/oversized HTTP
   requests, origin check bypass, unbounded buffers, thread that dies on error.
   Must stay bound to `127.0.0.1:43179` only.
3. **Tray protocol** (`platform/tray_proto.rs`, `model/tray.rs`): bounds checks
   on `COPYDATASTRUCT` payloads from other processes (size/version fields
   trusted?).
4. **Restore paths:** every change to taskbar/AppBar/work area in `app.rs`,
   `platform/` has a restore on exit, guard, and `--restore-taskbar`.
5. **Clipboard history / bookmarks** (newest code, 0.6.0): config writes go
   through `config_edit.rs`; history size capped; opt-in respected.
6. `unsafe` blocks (~600): only spot-check ones handling external buffers
   (lengths, null HWNDs, `from_raw_parts`).

Each fix: minimal diff, a unit test where the logic is pure.

## Phase 2 — optimization

- Grep `SetTimer`/`thread::sleep` (35 hits): flag any timer that runs while
  nothing changes (AGENTS.md forbids idle polling); convert to event-driven or
  stop the timer when idle/full-screen.
- Large files `ui/island.rs` (2200 lines), `ui/dock.rs` (1040): look for
  per-paint allocations (Strings, Vecs, D2D brushes/text formats recreated each
  frame) and cache them.
- Icon/thumbnail caches: bounded size (RAM < 25 MB).
- No behaviour changes, no new dependencies.

## Phase 3 — tests

Add unit tests for pure logic that has none or little:
`model/config_edit.rs` (comments/unknown keys survive), `model/clipboard.rs`
(cap, dedupe, opt-in), `model/search.rs`, `model/pomodoro.rs`, lyrics bridge
request parsing/origin check (factor the parser out if needed), tray payload
parsing. Spicetify: `node extensions/spicetify/test-bridge.cjs` and
`test-timed.cjs` (these run on Linux).

Optional: add `.github/workflows/ci.yml` on `windows-latest` running
`cargo build/test/clippy --release` (no secrets, no network calls from wdock).

## Phase 4 — wrap up

- Clippy zero warnings on the Windows target.
- README.md + README.vi.md only if user-visible behaviour/config changed.
- Push to `claude/adoring-fermi-7u2rg3`; summary of fixes, what couldn't be
  verified on Linux (runtime/DPI/two monitors) for the user to test with
  `cargo run --release -- --dev`. No PR unless asked.

## Budget

If usage runs low: Phase 0 → Phase 1 items 1–3 → Phase 3 tests for those →
push. Phase 2 and CI are lower priority.
