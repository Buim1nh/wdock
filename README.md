# wdock

**English** · [Tiếng Việt](README.vi.md)

wdock is a macOS-style top bar, dock and Dynamic Island for Windows 11, written in Rust.

- Resource targets: under 25 MB of RAM, close to 0% CPU when idle, and 0% while a game is running.
- It injects no code into other processes, uses no keyboard hooks and makes no network calls, so it is safe next to anti-cheat software.
- It runs on top of explorer instead of replacing it, so Windows features keep working and quitting wdock brings the normal taskbar back.

[![wdock demo](docs/media/demo.gif)](docs/media/demo.mp4)

*The demo shows the dock's magnification, the launcher, the shortcut card, the calendar and quick settings. Click it for the 1080p MP4.*

## Contents

- [Screenshots](#screenshots)
- [Features](#features)
- [How it works](#how-it-works)
- [Installation](#installation)
- [Keyboard shortcuts](#keyboard-shortcuts)
- [Configuration](#configuration)
- [Troubleshooting](#troubleshooting)
- [Uninstalling](#uninstalling)
- [Development](#development)
- [Measurements](#measurements)

## Screenshots

All screenshots are 1920×1080.

| | |
|---|---|
| ![Desktop with the top bar and dock](docs/media/desktop.png) | ![Dock magnification](docs/media/dock.png) |
| Top bar and dock | Dock magnification and labels |
| ![Launcher](docs/media/launcher.png) | ![Shortcut card](docs/media/shortcuts.png) |
| Launcher (Win+Alt+Space) | Shortcut card (Win+Alt+/) |
| ![Calendar](docs/media/calendar.png) | ![Quick settings](docs/media/quick-settings.png) |
| Calendar | Quick settings with the Wi-Fi and sound-output lists open |
| ![Dynamic Island, expanded](docs/media/island.png) | |
| Dynamic Island, expanded | |

## Features

### Top bar

- Launcher button and shortcut-card button.
- Workspace pills (4 workspaces per monitor by default).
- Title of the active window.
- System tray, hosted by wdock itself: tray icons, balloons and overflow work as in the taskbar.
- Volume, network and battery indicators that open quick settings.
- A clock that opens a calendar.

### Quick settings

- **Network:** the connection row expands into the visible Wi-Fi networks.
  - Each network shows its name, signal bars, a lock if it is secured, and "Connected" for the current one, sorted by signal. A button rescans.
  - Clicking a network that has a saved profile connects to it. For a new network wdock never asks for a password itself; it opens Windows' Wi-Fi settings instead.
  - A Wi-Fi on/off switch and a Disconnect action. Machines without Wi-Fi show the list as unavailable.
  - Networks are scanned only while the list is open.
- **Sound:** a volume slider, plus a list of the active outputs (speakers, headphones, HDMI, Bluetooth).
  - Clicking an output makes it the default for every role, and the slider follows it.
  - This uses the undocumented `IPolicyConfig` interface, as other output switchers do. If Windows refuses, the Sound settings page opens instead.
- **Display:** brightness slider, on screens that support it.
- **Battery** state, and buttons to lock, sleep, sign out, restart, shut down and open Settings. Power actions ask for confirmation.

### Dock

- Pinned and running apps, with a dot under running ones.
- macOS-style magnification and name labels.
- Live window previews on hover.
- Drag to reorder or pin, and a right-click menu.
- Smart auto-hide: the dock slides away while a window overlaps it and comes back at the bottom edge.

### Dynamic Island

A pill at the top centre of each monitor that expands on hover.

- **Now playing**, for any app that shows Windows' media controls (Spotify, browsers, media players):
  - Title, artist, play/pause, previous/next and a seekable progress bar.
  - The cover art when the app provides one. Many web pages provide none (a browser only passes on what the page declares), so the island then shows the playing app's icon, and while expanded a live view of the playing window.
  - A moving equalizer while something plays. Windows' compositor (DWM) plays this animation, so wdock does no work per frame.
- **Pomodoro:** start, pause and reset from the expanded island. The compact pill shows the minutes left, and a chime plus a notice mark the end of each focus or break.
- **Notices:** the island briefly expands when volume, brightness, charging, battery, network or the audio device change. Windows' own pop-ups are left alone.

### Launcher and windows

- **Launcher:** fuzzy search over installed apps (Win32 and Store) and Settings pages.
- **Window switcher** (Alt+\`), **overview** of all windows (Win+\`), and **virtual workspaces** per monitor.

### Game mode

While a full-screen app (a game, a full-screen video, F11 in a browser) is in front, the bars and the island on that monitor are hidden and stop drawing and running timers. wdock also stops following window movement inside the game, so it uses 0% CPU.

## How it works

Explorer stays the Windows shell ("hybrid mode"), and wdock runs alongside it:

- **Explorer's taskbar** is hidden with auto-hide plus SW_HIDE. Its original state is saved to `%LOCALAPPDATA%\wdock\taskbar.txt` and restored when wdock exits, crashes for good, or the session ends.
- **The top bar** is registered as an AppBar, so explorer keeps maximized windows below it.
  - wdock checks that the strip is really reserved after building the bars and after a full-screen app closes.
  - If explorer stops honouring it, wdock registers again, and as a last resort sets the monitor's work area itself (restored on exit).
- **The dock** reserves no space in smart mode. With `auto_hide = "off"` it registers a bottom AppBar too.
- **The tray**: wdock owns the window that apps send tray icons to, and forwards the messages it does not handle to explorer's taskbar.
- **Shortcuts** use `RegisterHotKey` with combinations Windows does not use. Win+Shift+S, PrtScn, Win+X, `ms-settings:` links and Windows' other shortcuts keep working.
- **Rendering**: Direct2D on the CPU (WARP) by default, composed with DirectComposition and DWM acrylic. Everything is event-driven; no timer runs while nothing changes.

## Installation

### From a release (recommended)

1. Download `wdock-setup-<version>.exe` from the [latest release](https://github.com/Buim1nh/wdock/releases/latest).
2. Run it and pick your options, then **Install**. It installs for your account (no admin), starts wdock and lists it in **Settings > Apps**.
3. Windows SmartScreen may warn because the file is not code-signed: choose **More info > Run anyway**.

![The setup window](docs/media/setup.png)

The setup window offers:

| Option | Default |
|---|---|
| Start wdock when I sign in | on |
| Start wdock when setup finishes | on |
| Add to the Start menu | on |
| Also install agent-usage-bar (AI agent usage in the tray) | off (on when it is already installed) |
| Theme: follow Windows, dark, light | follow Windows |
| Background: translucent (acrylic) or solid | translucent |
| Top bar: on every monitor or the main one only | every monitor |
| Dock: hide when a window covers it, always auto-hide, always visible | hide when covered |
| Dynamic Island, and its animated equalizer | on, on |

Running the setup again updates wdock and shows your current settings, so you can also use it to change them. Only the options you change are written to `config.toml`.

To remove wdock, use **Settings > Apps > wdock > Uninstall**, or run `%LOCALAPPDATA%\Programs\wdock\uninstall.exe --uninstall`. agent-usage-bar has its own entry in Settings > Apps.

For a silent install, run `wdock-setup-<version>.exe /S`, optionally with `/WithUsage /NoSignIn /NoStart /NoShortcut /NoIsland /NoEqualizer /Theme=auto|dark|light /Backdrop=acrylic|solid /TopBar=all|primary /AutoHide=smart|on|off`. A silent run that fails writes the reason to `%TEMP%\wdock-setup.log`. The uninstallers take `/S` too.

The release also has a portable zip with `wdock.exe` and the install scripts.

### From source

Building needs the [Rust toolchain](https://rustup.rs) (stable, edition 2024). Installing does not need admin rights.

```bash
powershell -ExecutionPolicy Bypass -File scripts\install.ps1
```

The script:

1. Builds the release binary.
2. Copies it to `%LOCALAPPDATA%\Programs\wdock\wdock.exe`.
3. Registers a per-user scheduled task named `wdock` that runs `"<exe>" --guard` at every sign-in.
4. Starts wdock immediately.

A scheduled task is used rather than the `Run` registry key: when the script runs from a terminal inside a packaged (MSIX) app, Windows redirects its `HKCU` writes into that package, and explorer would never see a `Run` entry.

Options:

| Option | Effect |
|---|---|
| `-NoBuild` | use the existing `target\release` build |
| `-NoStart` | install without starting wdock |
| `-WithUsage` | also install [agent-usage-bar](https://github.com/Buim1nh/agent-usage-bar) (see below) |
| `-UsageRepo <path>` | where agent-usage-bar's source is (default: a sibling `agent-usage-bar` folder, cloned there if missing) |

Quit wdock before installing over a running copy.

**agent-usage-bar** is an optional, separate tray app that shows Claude, Codex and Antigravity usage limits (and a GitHub contribution graph) on wdock's top bar. It is kept out of wdock so that wdock itself still holds no credentials and makes no network calls. It starts at sign-in from its own scheduled task. It is off by default.

**The guard**: the `--guard` process started at sign-in runs the shell as a `--shell` child.

- If the shell crashes, the guard restarts it after about 1 second.
- After 3 crashes within 60 seconds, it gives up and brings explorer's taskbar back.
- Only one guard runs per session.

## Keyboard shortcuts

Win+Alt+/ or the keyboard button in the top bar shows these on a card you can drag around.

| Keys | Action |
|---|---|
| Win+Alt+Space (also Ctrl+Alt+Space if it is free) | Launcher |
| Alt+\` / Alt+Shift+\` | Window switcher (release Alt to pick) |
| Win+\` | Overview of the current workspace |
| Win+Ctrl+Alt+← / → / 1–9 | Switch workspace |
| Win+Alt+Shift+← / → / 1–9 | Move the active window to another workspace |
| Win+Alt+/ | Show or hide the shortcut card |
| Ctrl+Alt+Shift+Q | Quit wdock and bring explorer's taskbar back |

If another app already owns a combination, wdock logs it and skips it.

## Configuration

The configuration file is `%APPDATA%\wdock\config.toml`. wdock creates it from a commented template on first run.

- Save it as UTF-8 without a BOM.
- Changes apply after a restart: press Ctrl+Alt+Shift+Q, then run `Start-ScheduledTask wdock` in PowerShell, or sign out and back in.

| Section | Main settings |
|---|---|
| `[appearance]` | `theme` = `"auto"`, `"dark"` or `"light"`; `backdrop` = `"acrylic"` or `"solid"` |
| `[performance]` | `renderer` = `"software"` (WARP, about 10 MB, never wakes a discrete GPU) or `"gpu"` |
| `[topbar]` | `height` (30), `monitors` (`"all"` or `"primary"`), `date_format`, `time_format` |
| `[tray]` | `max_visible` icons before the overflow chevron (12), `overflow` executable fragments always hidden |
| `[dock]` | `icon_size` (32), `monitors` (`"primary"`), `windows` (`"all"` or `"this-monitor"`), `magnification` (2.0), `auto_hide` (`"smart"`, `"on"`, `"off"`), `pinned` |
| `[workspaces]` | `count` per monitor (4, up to 9) |
| `[switcher]` | `enabled` (true) |
| `[island]` | `enabled` (true), `pomodoro_work` (25), `pomodoro_break` (5), `notices` (true), `animate_equalizer` (true) |

Pinning, unpinning or reordering from the dock saves the list to `%LOCALAPPDATA%\wdock\pinned.txt`, which then takes precedence over `pinned`; delete that file to go back to the config.

State and logs live in `%LOCALAPPDATA%\wdock\`:

| File | Contents |
|---|---|
| `logs\wdock.log`, `logs\wdock-guard.log` | logs (rotated at 2 MB) |
| `pinned.txt` | pinned apps, in dock order |
| `hidden.txt` | windows parked in other workspaces |
| `taskbar.txt` | explorer's original auto-hide state |
| `shortcuts.txt` | where the shortcut card was dragged to |

## Troubleshooting

| Problem | What to do |
|---|---|
| The top bar covers maximized windows | wdock repairs this itself within a few seconds (see [How it works](#how-it-works)). It happens when explorer's AppBar manager gets stuck, for example after wdock was restarted many times in a row. If it persists, restart explorer from Task Manager. |
| Explorer's taskbar did not come back after wdock was killed | Run `"%LOCALAPPDATA%\Programs\wdock\wdock.exe" --restore-taskbar`. |
| A tray icon is missing | Restart the app that owns it. wdock asks apps to re-add their icons at start and again after 5 seconds. |
| The island shows no cover for a video | The page declares no artwork. The island shows the app's icon instead, and a live view of the window when expanded. |
| wdock does not start at sign-in | Check that the `wdock` task exists in Task Scheduler, or run `scripts\install.ps1 -NoBuild` again. |

## Uninstalling

```bash
powershell -ExecutionPolicy Bypass -File scripts\uninstall.ps1
```

The script:

1. Removes the sign-in task (and the `Run` key entry left by older installs).
2. Stops the guard, then the shell.
3. Restores explorer's taskbar and any windows parked in other workspaces.

`-RemoveFiles` also deletes the installed binary, and `-RemoveData` also deletes logs, caches and state. Your configuration is kept either way.

## Development

| Command | Purpose |
|---|---|
| `cargo run --release -- --dev` | run under a development supervisor that restarts on crashes and logs to `wdock-dev.log` |
| `cargo test --release` | unit tests |
| `cargo clippy --release --workspace` | lints (currently no warnings) |
| `cargo build --release -p wdock && cargo build --release -p wdock-setup` | the installer, with `wdock.exe` embedded (`crates/wdock-setup`) |

Command-line modes:

| Argument | Mode |
|---|---|
| none or `--shell` | the shell |
| `--guard` | the sign-in supervisor |
| `--dev` | the development supervisor |
| `--restore-taskbar` | restore explorer's taskbar and hidden windows, then exit |

The source is in `crates/wdock/src`:

| Path | Contents |
|---|---|
| `app.rs` | message loop, shortcuts, game mode, AppBar checks |
| `ui/` | Direct2D bars, the dock, the island, popups, composition layers |
| `platform/` | Win32 wrappers, the tray host, AppBars, window events |
| `services/` | audio, network and Wi-Fi, power, brightness, media session, app index, icons |
| `model/` | configuration, windows and workspaces, tray, search, Pomodoro |

## Measurements

Release build on Windows 11, two 1080p monitors.

| Item | Result |
|---|---|
| Private working set (shell / guard) | ~14–19 MB / ~1.2 MB |
| CPU when idle | 15–110 ms per 60 s (under 0.2%) |
| CPU while media plays (animated equalizer) | ~16 ms per 60 s; DWM's own load unchanged |
| CPU with a full-screen app in front | 0 ms over 20 s |
| Full-screen (F11) detection | ~15 ms |
| Leaks (200 shortcut-card toggles + 20 launcher cycles) | handles stable, GDI 40–43, memory plateaus |
| Shell crash | the guard restarts it in ~1 s; tray icons return and the taskbar stays hidden |

## License

MIT
