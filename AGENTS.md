# AGENTS.md

Guidance for coding agents working on wdock. Read this before changing code.
Folder-specific rules live next to the code (for example
`crates/wdock/src/ui/AGENTS.md` for top-bar alignment); they take precedence
for files in that folder.

## What wdock is

A macOS-style top bar, dock and Dynamic Island for Windows 11, in Rust. It
runs next to explorer ("hybrid mode"): it hides explorer's taskbar, hosts the
tray itself, and restores everything when it exits or crashes for good.

These properties are product requirements, not nice-to-haves. Do not trade
them away for a feature:

- **Resources:** under 25 MB RAM, ~0% CPU idle, 0% while a full-screen app is
  in front. Everything is event-driven; never add a polling timer that runs
  while nothing changes. Animations go to DirectComposition/DWM, not per-frame
  redraws.
- **Anti-cheat safety:** no code injection into other processes, no keyboard
  hooks (`SetWindowsHookEx`), only the two `RegisterHotKey` hotkeys.
- **No network calls.** wdock holds no credentials. The only socket is the
  lyrics bridge listening on `127.0.0.1:43179`, which accepts only the Spotify
  client origin. Anything that needs the internet belongs in a separate app
  (see agent-usage-bar).
- **Recoverable:** explorer's taskbar and work area must come back on exit,
  crash (via the guard) and `--restore-taskbar`. Any new change to global
  shell state needs a matching restore path.

## Layout

| Path | Contents |
|---|---|
| `crates/wdock/src/main.rs` | entry point, command-line modes (`--shell`, `--guard`, `--dev`, `--restore-taskbar`) |
| `crates/wdock/src/app.rs` | message loop, hotkeys, game mode, AppBar checks |
| `crates/wdock/src/guard.rs`, `dev.rs`, `instance.rs` | sign-in supervisor, dev supervisor, single instance |
| `crates/wdock/src/ui/` | Direct2D bars, dock, island, popups, composition layers |
| `crates/wdock/src/platform/` | Win32 wrappers: tray host, AppBars, window events, virtual desktops |
| `crates/wdock/src/services/` | audio, network/Wi-Fi, power, brightness, media session, lyrics bridge, icons |
| `crates/wdock/src/model/` | config (`default_config.toml` is the template), windows, tray, pins, search, Pomodoro |
| `crates/wdock-setup/` | GUI installer that embeds the release `wdock.exe` |
| `extensions/spicetify/` | Spicetify lyrics bridge (`wdock-lyrics-plus.js`) and its installer |
| `extensions/wdock-lyrics/` | browser extension that reads lyrics from the local bridge |
| `scripts/` | `install.ps1`, `uninstall.ps1`, `windows-font.ps1` |
| `docs/media/` | screenshots and demo used by the READMEs |

## Build and check

```bash
cargo build --release
cargo test --release
cargo clippy --release --workspace
```

- Clippy must stay at zero warnings.
- Run the shell with `cargo run --release -- --dev` (restarts on crash, logs to
  `wdock-dev.log`). Quit a running copy with Ctrl+Alt+Shift+Q first; if the
  taskbar is left hidden, run `wdock.exe --restore-taskbar`.
- The installer needs `wdock.exe` built first:
  `cargo build --release -p wdock && cargo build --release -p wdock-setup`.
- Spicetify bridge tests: `node extensions/spicetify/test-bridge.cjs` and
  `test-timed.cjs`.

## Conventions

- Rust edition 2024, `windows` 0.62 (the workspace dependency). `winvd` pulls
  `windows` 0.58 as `windows058`; use it only at that boundary.
- Match the surrounding code: terse names, few comments, comments explain
  *why* (an undocumented API, a Windows quirk), not *what*.
- Wrap raw Win32 in `platform/`, keep UI code free of direct OS calls where a
  wrapper exists. Long or blocking work (WMI, DDC/CI, Wi-Fi scans) runs on a
  worker thread and posts back to the UI thread.
- Undocumented Windows interfaces (`IPolicyConfig`, virtual desktop COM,
  taskbar pins) must fail soft: detect, log, hide the feature or open the
  matching Settings page. Never crash the shell on an unknown build.
- Popups and settings changes that write config go through
  `model/config_edit.rs` so user comments and unknown keys survive.
- Logging uses the `log` crate; logs rotate at 2 MB under
  `%LOCALAPPDATA%\wdock\logs`.
- Test DPI-sensitive UI at fractional scales (125%, 150%) and on two monitors.

## Adding a config key

1. Add the field and default in `model/config.rs`.
2. Document it in `model/default_config.toml` (commented template).
3. If it belongs in the settings panel, add it in `ui/popups/settings.rs` and
   save it through `config_edit.rs`.
4. Update the configuration table in **both** `README.md` and `README.vi.md`.

## Documentation

- `README.md` (English) and `README.vi.md` (Vietnamese) are kept in sync.
  Change both in the same commit.
- Screenshots are 1920×1080 in `docs/media/`.

## Git

- Branch from `main`; open a PR, do not push to `main` directly.
- Version lives in the root `Cargo.toml` `[workspace.package]`; release commits
  are named `Release x.y.z`.
- Never commit font files (`fonts/*.ttf`, `*.otf`); SF Pro cannot be
  redistributed.
