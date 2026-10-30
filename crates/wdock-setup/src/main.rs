//! wdock-setup: installs wdock (and optionally agent-usage-bar) for the
//! current user, no admin needed.
//!
//! * `wdock-setup.exe` opens a small window with every option: start at
//!   sign-in, start now, a Start menu shortcut, agent-usage-bar, and wdock's
//!   main settings (theme, backdrop, top bar monitors, dock auto-hide, the
//!   Dynamic Island and its equalizer). Updating reads the current
//!   `config.toml` first, so only what is changed in the window is written.
//! * Installing:
//!   1. quits a running wdock with its own quit hotkey, so it puts explorer's
//!      taskbar back first,
//!   2. writes `%LOCALAPPDATA%\Programs\wdock\wdock.exe` and a copy of itself
//!      as `uninstall.exe`,
//!   3. writes the chosen settings to `%APPDATA%\wdock\config.toml`,
//!   4. registers the per-user sign-in task `wdock` (`wdock.exe --guard`),
//!      the Start menu shortcut and the Settings > Apps entry, and starts it.
//!      agent-usage-bar gets the same in `Programs\agent-usage-bar`.
//! * `uninstall.exe --uninstall` (Settings > Apps > wdock) removes wdock and
//!   brings explorer's taskbar back; `--uninstall-usage` (Settings > Apps >
//!   agent-usage-bar) removes agent-usage-bar. Configuration and logs stay.
//! * `/S` installs or uninstalls without the window. Install flags:
//!   `/WithUsage /NoSignIn /NoStart /NoShortcut /NoIsland /NoEqualizer
//!   /Theme=auto|dark|light /Backdrop=acrylic|solid /TopBar=all|primary
//!   /AutoHide=smart|on|off`.
//!
//! Scheduled tasks, shortcuts and registry entries go through Windows
//! PowerShell (present on every Windows 10/11), with the cmdlets the
//! install scripts use.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::cell::RefCell;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    COLOR_WINDOW, CreateFontW, DEFAULT_CHARSET, DeleteObject, FW_NORMAL, FW_SEMIBOLD, GetSysColor, GetSysColorBrush,
    HDC, HFONT, SetBkColor, SetTextColor, COLOR_GRAYTEXT, COLOR_WINDOWTEXT,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, SetProcessDpiAwarenessContext};
use windows::Win32::UI::Input::KeyboardAndMouse::EnableWindow;
use windows::Win32::UI::WindowsAndMessaging::{
    AdjustWindowRectEx, BM_GETCHECK, BM_SETCHECK, BS_AUTOCHECKBOX, BS_DEFPUSHBUTTON, BS_PUSHBUTTON, CB_ADDSTRING,
    CB_GETCURSEL, CB_SETCURSEL, CBS_DROPDOWNLIST, CW_USEDEFAULT, CreateWindowExW, DefWindowProcW, DestroyWindow,
    DispatchMessageW, FindWindowW, GetMessageW, GetSystemMetrics, HMENU, IDC_ARROW, IDYES, IsDialogMessageW,
    LoadCursorW, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_YESNO, MESSAGEBOX_STYLE, MSG,
    MessageBoxW, PostMessageW, PostQuitMessage, RegisterClassExW, SM_CXSCREEN, SM_CYSCREEN, SW_SHOW, SendMessageW,
    SetWindowTextW, ShowWindow, TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLOSE, WM_COMMAND,
    WM_CTLCOLORBTN, WM_CTLCOLORSTATIC, WM_DESTROY, WM_HOTKEY, WM_SETFONT, WNDCLASSEXW, WS_CAPTION, WS_CHILD,
    WS_EX_CONTROLPARENT, WS_MINIMIZEBOX, WS_SYSMENU, WS_TABSTOP, WS_VISIBLE, WS_VSCROLL,
};
use windows::core::{HSTRING, PCWSTR, w};

const WDOCK: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/wdock.exe"));
const USAGE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/agent-usage-bar.exe"));
const TEMPLATE: &str = include_str!("../../wdock/src/model/default_config.toml");
const VERSION: &str = env!("CARGO_PKG_VERSION");
const TITLE: &str = "wdock setup";
/// wdock's emergency-exit hotkey id: a clean quit that restores the taskbar.
const HOTKEY_QUIT: usize = 1;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const RUN_KEY: &str = r"HKCU:\Software\Microsoft\Windows\CurrentVersion\Run";
const APPS_KEY: &str = r"HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall";

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let silent = args.iter().any(|a| a.eq_ignore_ascii_case("/S") || a == "--silent");
    let result = if args.iter().any(|a| a == "--uninstall") {
        uninstall_wdock(silent)
    } else if args.iter().any(|a| a == "--uninstall-usage") {
        uninstall_usage(silent)
    } else if silent {
        Options::detect().with_flags(&args).and_then(|o| install(&o, &|_| {})).map(drop)
    } else {
        run_window();
        Ok(())
    };
    if let Err(err) = result {
        if silent {
            // Nobody sees a silent run; leave the reason where it can be found.
            let _ = std::fs::write(std::env::temp_dir().join("wdock-setup.log"), &err);
        } else {
            message(&err, MB_ICONERROR);
        }
        std::process::exit(1);
    }
}

// ---- options ----

const THEMES: [&str; 3] = ["auto", "dark", "light"];
const BACKDROPS: [&str; 2] = ["acrylic", "solid"];
const TOPBARS: [&str; 2] = ["all", "primary"];
const AUTOHIDES: [&str; 3] = ["smart", "on", "off"];

#[derive(Clone, Debug, PartialEq)]
struct Options {
    sign_in: bool,
    start_now: bool,
    shortcut: bool,
    usage: bool,
    island: bool,
    equalizer: bool,
    theme: usize,
    backdrop: usize,
    topbar: usize,
    autohide: usize,
}

impl Options {
    /// Defaults, overlaid with what an existing install and config say.
    fn detect() -> Self {
        let text = std::fs::read_to_string(config_path()).ok();
        let read = |section: &str, key: &str| text.as_deref().and_then(|t| get_key(t, section, key));
        let pick = |choices: &[&str], value: Option<String>, default: usize| {
            value.and_then(|v| choices.iter().position(|c| *c == v)).unwrap_or(default)
        };
        Self {
            sign_in: true,
            start_now: true,
            shortcut: true,
            usage: usage_dir().is_ok_and(|d| d.join("agent-usage-bar.exe").exists()),
            island: read("island", "enabled").is_none_or(|v| v == "true"),
            equalizer: read("island", "animate_equalizer").is_none_or(|v| v == "true"),
            theme: pick(&THEMES, read("appearance", "theme"), 0),
            backdrop: pick(&BACKDROPS, read("appearance", "backdrop"), 0),
            topbar: pick(&TOPBARS, read("topbar", "monitors"), 0),
            autohide: pick(&AUTOHIDES, read("dock", "auto_hide"), 0),
        }
    }

    /// Applies `/Flag` and `/Name=value` command-line switches.
    fn with_flags(mut self, args: &[String]) -> Result<Self, String> {
        for arg in args {
            let lower = arg.to_ascii_lowercase();
            let (name, value) = lower.split_once('=').map_or((lower.as_str(), None), |(n, v)| (n, Some(v)));
            let choose = |choices: &[&str]| -> Result<usize, String> {
                let value = value.ok_or(format!("{arg} needs a value"))?;
                choices.iter().position(|c| *c == value).ok_or(format!("{arg}: expected one of {}", choices.join(", ")))
            };
            match name {
                "/s" | "--silent" => {}
                "/withusage" => self.usage = true,
                "/nosignin" => self.sign_in = false,
                "/nostart" => self.start_now = false,
                "/noshortcut" => self.shortcut = false,
                "/noisland" => self.island = false,
                "/noequalizer" => self.equalizer = false,
                "/theme" => self.theme = choose(&THEMES)?,
                "/backdrop" => self.backdrop = choose(&BACKDROPS)?,
                "/topbar" => self.topbar = choose(&TOPBARS)?,
                "/autohide" => self.autohide = choose(&AUTOHIDES)?,
                _ => return Err(format!("Unknown option: {arg}")),
            }
        }
        if self.usage && USAGE.is_empty() {
            return Err("This setup does not include agent-usage-bar.".into());
        }
        Ok(self)
    }
}

// ---- paths ----

fn local_programs(name: &str) -> Result<PathBuf, String> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set")?;
    Ok(PathBuf::from(local).join("Programs").join(name))
}

fn install_dir() -> Result<PathBuf, String> {
    local_programs("wdock")
}

fn usage_dir() -> Result<PathBuf, String> {
    local_programs("agent-usage-bar")
}

fn config_path() -> PathBuf {
    std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_default().join("wdock").join("config.toml")
}

// ---- config.toml editing (keeps comments and everything else) ----

/// The value of `key` in `[section]`, without quotes.
fn get_key(text: &str, section: &str, key: &str) -> Option<String> {
    let mut current = "";
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            current = name.trim();
            continue;
        }
        if current == section
            && let Some((k, v)) = trimmed.split_once('=')
            && k.trim() == key
        {
            let v = v.split('#').next().unwrap_or("").trim();
            return Some(v.trim_matches('"').trim_matches('\'').to_owned());
        }
    }
    None
}

/// Sets `key = value` (`value` already TOML, e.g. `"dark"` or `true`) in
/// `[section]`, replacing the existing line, adding it under the header, or
/// adding the section at the end.
fn set_key(text: &str, section: &str, key: &str, value: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let mut current = String::new();
    let mut header = None;
    for (i, line) in lines.iter_mut().enumerate() {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            current = name.trim().to_owned();
            if current == section {
                header = Some(i);
            }
            continue;
        }
        if current == section
            && let Some((k, _)) = trimmed.split_once('=')
            && k.trim() == key
            && !trimmed.starts_with('#')
        {
            *line = format!("{key} = {value}");
            return lines.join("\n") + "\n";
        }
    }
    match header {
        Some(i) => lines.insert(i + 1, format!("{key} = {value}")),
        None => {
            lines.push(String::new());
            lines.push(format!("[{section}]"));
            lines.push(format!("{key} = {value}"));
        }
    }
    lines.join("\n") + "\n"
}

fn write_config(options: &Options) -> Result<(), String> {
    let path = config_path();
    let mut text = std::fs::read_to_string(&path).unwrap_or_else(|_| TEMPLATE.to_owned());
    let quoted = |s: &str| format!("\"{s}\"");
    for (section, key, value) in [
        ("appearance", "theme", quoted(THEMES[options.theme])),
        ("appearance", "backdrop", quoted(BACKDROPS[options.backdrop])),
        ("topbar", "monitors", quoted(TOPBARS[options.topbar])),
        ("dock", "auto_hide", quoted(AUTOHIDES[options.autohide])),
        ("island", "enabled", options.island.to_string()),
        ("island", "animate_equalizer", options.equalizer.to_string()),
    ] {
        if get_key(&text, section, key).as_deref() != Some(value.trim_matches('"')) {
            text = set_key(&text, section, key, &value);
        }
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    }
    // UTF-8 without a BOM: wdock's TOML parser rejects one.
    std::fs::write(&path, text).map_err(|e| format!("Could not write {}: {e}", path.display()))
}

// ---- install / uninstall ----

/// Installs or updates; `progress` receives short status lines.
fn install(options: &Options, progress: &dyn Fn(&str)) -> Result<String, String> {
    if WDOCK.is_empty() {
        return Err("This setup was built without wdock.exe inside; build wdock first.".into());
    }
    let dir = install_dir()?;
    let exe = dir.join("wdock.exe");
    let uninstaller = dir.join("uninstall.exe");

    progress("Closing the running wdock...");
    quit_running_wdock();
    progress("Copying files...");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    write_retrying(&exe, WDOCK)?;
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    if !same_file(&me, &uninstaller) {
        std::fs::copy(&me, &uninstaller).map_err(|e| format!("Could not write {}: {e}", uninstaller.display()))?;
    }
    progress("Saving settings...");
    write_config(options)?;

    progress("Registering wdock...");
    let mut script = String::from("$ErrorActionPreference = 'Stop'\n");
    script += &app_script(&App {
        id: "wdock",
        name: "wdock",
        exe: &exe,
        args: "--guard",
        description: "Starts wdock at sign-in",
        dir: &dir,
        uninstall: format!("\"{}\" --uninstall", uninstaller.display()),
        size_kb: WDOCK.len() / 1024,
        url: "https://github.com/Buim1nh/wdock",
        sign_in: options.sign_in,
        shortcut: options.shortcut,
        start: options.start_now,
    });
    script += &format!("Remove-ItemProperty -Path '{RUN_KEY}' -Name wdock -ErrorAction SilentlyContinue\n");
    powershell(&script)?;

    if options.usage && !USAGE.is_empty() {
        progress("Installing agent-usage-bar...");
        let dir = usage_dir()?;
        let exe = dir.join("agent-usage-bar.exe");
        // It has no quit command of its own; it holds no unsaved state.
        let _ = Command::new("taskkill")
            .args(["/F", "/IM", "agent-usage-bar.exe"])
            .creation_flags(CREATE_NO_WINDOW)
            .output();
        std::fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
        write_retrying(&exe, USAGE)?;
        let mut script = String::from("$ErrorActionPreference = 'Stop'\n");
        script += &app_script(&App {
            id: "agent-usage-bar",
            name: "agent-usage-bar",
            exe: &exe,
            args: "",
            description: "Starts agent-usage-bar (AI agent usage in the tray) at sign-in",
            dir: &dir,
            uninstall: format!("\"{}\" --uninstall-usage", uninstaller.display()),
            size_kb: USAGE.len() / 1024,
            url: "https://github.com/Buim1nh/agent-usage-bar",
            sign_in: options.sign_in,
            shortcut: options.shortcut,
            start: options.start_now,
        });
        powershell(&script)?;
    }

    let mut done = format!("wdock {VERSION} is installed");
    if options.usage {
        done += ", with agent-usage-bar";
    }
    done += if options.start_now { " and running." } else { "." };
    Ok(done)
}

struct App<'a> {
    id: &'a str,
    name: &'a str,
    exe: &'a Path,
    args: &'a str,
    description: &'a str,
    dir: &'a Path,
    uninstall: String,
    size_kb: usize,
    url: &'a str,
    sign_in: bool,
    shortcut: bool,
    start: bool,
}

/// PowerShell for one app's sign-in task, Start menu shortcut, Settings >
/// Apps entry and start.
fn app_script(app: &App) -> String {
    let exe = ps(&app.exe.display().to_string());
    let args = ps(app.args);
    let id = app.id;
    let mut s = format!("$exe = '{exe}'\n$user = \"$env:USERDOMAIN\\$env:USERNAME\"\n");
    if app.sign_in {
        let action = if app.args.is_empty() {
            "New-ScheduledTaskAction -Execute $exe".to_owned()
        } else {
            format!("New-ScheduledTaskAction -Execute $exe -Argument '{args}'")
        };
        s += &format!(
            "Register-ScheduledTask -TaskName '{id}' -Description '{desc}' -Action ({action}) \
             -Trigger (New-ScheduledTaskTrigger -AtLogOn -User $user) \
             -Principal (New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited) \
             -Settings (New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries \
             -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew) -Force | Out-Null\n",
            desc = ps(app.description),
        );
    } else {
        s += &format!("Unregister-ScheduledTask -TaskName '{id}' -Confirm:$false -ErrorAction SilentlyContinue\n");
    }
    let link = format!("$([Environment]::GetFolderPath('Programs'))\\{}.lnk", ps(app.name));
    if app.shortcut {
        s += &format!(
            "$l = (New-Object -ComObject WScript.Shell).CreateShortcut(\"{link}\")\n\
             $l.TargetPath = $exe; $l.Arguments = '{args}'; $l.WorkingDirectory = '{dir}'; $l.Description = '{desc}'; $l.Save()\n",
            dir = ps(&app.dir.display().to_string()),
            desc = ps(app.name),
        );
    } else {
        s += &format!("Remove-Item \"{link}\" -ErrorAction SilentlyContinue\n");
    }
    s += &format!(
        "$key = '{APPS_KEY}\\{id}'\nNew-Item -Path $key -Force | Out-Null\n\
         foreach ($v in @{{ DisplayName = '{name}'; DisplayVersion = '{version}'; Publisher = 'Buim1nh'; DisplayIcon = $exe; \
         InstallLocation = '{dir}'; UninstallString = '{uninstall}'; QuietUninstallString = '{uninstall} /S'; URLInfoAbout = '{url}' }}.GetEnumerator()) {{ \
         New-ItemProperty -Path $key -Name $v.Key -Value $v.Value -PropertyType String -Force | Out-Null }}\n\
         foreach ($n in 'NoModify', 'NoRepair') {{ New-ItemProperty -Path $key -Name $n -Value 1 -PropertyType DWord -Force | Out-Null }}\n\
         New-ItemProperty -Path $key -Name EstimatedSize -Value {size} -PropertyType DWord -Force | Out-Null\n",
        name = ps(app.name),
        version = if id == "wdock" { VERSION } else { "bundled" },
        dir = ps(&app.dir.display().to_string()),
        uninstall = ps(&app.uninstall),
        url = app.url,
        size = app.size_kb,
    );
    if app.start {
        s += &if app.sign_in {
            format!("Start-ScheduledTask -TaskName '{id}'\n")
        } else if app.args.is_empty() {
            "Start-Process -FilePath $exe\n".to_owned()
        } else {
            format!("Start-Process -FilePath $exe -ArgumentList '{args}'\n")
        };
    }
    s
}

fn uninstall_wdock(silent: bool) -> Result<(), String> {
    let dir = install_dir()?;
    if !silent && !ask("Remove wdock and bring explorer's taskbar back?\n\nYour settings and logs are kept.") {
        return Ok(());
    }
    quit_running_wdock();
    powershell(&format!(
        "Unregister-ScheduledTask -TaskName wdock -Confirm:$false -ErrorAction SilentlyContinue\n\
         Remove-ItemProperty -Path '{RUN_KEY}' -Name wdock -ErrorAction SilentlyContinue\n\
         Remove-Item -Path '{APPS_KEY}\\wdock' -Recurse -ErrorAction SilentlyContinue\n\
         Remove-Item \"$([Environment]::GetFolderPath('Programs'))\\wdock.lnk\" -ErrorAction SilentlyContinue"
    ))?;
    let exe = dir.join("wdock.exe");
    if exe.exists() {
        // A killed wdock cannot clean up; this puts the taskbar and any
        // windows hidden in other workspaces back.
        let _ = Command::new(&exe).arg("--restore-taskbar").creation_flags(CREATE_NO_WINDOW).status();
        let _ = std::fs::remove_file(&exe);
    }
    // agent-usage-bar's own Apps entry uses this uninstaller: keep it while
    // agent-usage-bar is installed.
    let usage_installed = usage_dir().is_ok_and(|d| d.join("agent-usage-bar.exe").exists());
    if usage_installed {
        let _ = std::fs::write(dir.join(".keep-for-agent-usage-bar"), b"");
    } else {
        remove_dir_later(&dir);
    }
    if !silent {
        let extra = if usage_installed { "\n\nagent-usage-bar is still installed; remove it from Settings > Apps." } else { "" };
        message(&format!("wdock was removed. Explorer's taskbar is back.{extra}"), MB_ICONINFORMATION);
    }
    Ok(())
}

fn uninstall_usage(silent: bool) -> Result<(), String> {
    let dir = usage_dir()?;
    if !silent && !ask("Remove agent-usage-bar?\n\nIts settings and sign-ins are kept.") {
        return Ok(());
    }
    let _ = Command::new("taskkill").args(["/F", "/IM", "agent-usage-bar.exe"]).creation_flags(CREATE_NO_WINDOW).output();
    powershell(&format!(
        "Unregister-ScheduledTask -TaskName agent-usage-bar -Confirm:$false -ErrorAction SilentlyContinue\n\
         Remove-Item -Path '{APPS_KEY}\\agent-usage-bar' -Recurse -ErrorAction SilentlyContinue\n\
         Remove-Item \"$([Environment]::GetFolderPath('Programs'))\\agent-usage-bar.lnk\" -ErrorAction SilentlyContinue"
    ))?;
    std::thread::sleep(Duration::from_millis(500));
    let _ = std::fs::remove_dir_all(&dir);
    // If wdock is already gone, this uninstaller was only kept for us.
    let wdock_dir = install_dir()?;
    if !wdock_dir.join("wdock.exe").exists() {
        remove_dir_later(&wdock_dir);
    }
    if !silent {
        message("agent-usage-bar was removed.", MB_ICONINFORMATION);
    }
    Ok(())
}

/// Deletes `dir` once this process (possibly the uninstaller inside it) exits.
fn remove_dir_later(dir: &Path) {
    let _ = Command::new("cmd")
        .raw_arg(format!("/c ping -n 3 127.0.0.1 >nul & rmdir /s /q \"{}\"", dir.display()))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

/// Asks the running wdock to quit as Ctrl+Alt+Shift+Q would, and waits for
/// the shell and its guard to go.
fn quit_running_wdock() {
    let Ok(main) = (unsafe { FindWindowW(w!("wdock.Main"), None) }) else {
        return;
    };
    let _ = unsafe { PostMessageW(Some(main), WM_HOTKEY, WPARAM(HOTKEY_QUIT), LPARAM(0)) };
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && unsafe { FindWindowW(w!("wdock.Main"), None) }.is_ok() {
        std::thread::sleep(Duration::from_millis(100));
    }
    // The guard exits right after a clean quit; give it a moment to release the exe.
    std::thread::sleep(Duration::from_millis(1500));
}

/// The exe can stay locked briefly after its process exits.
fn write_retrying(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut last = String::new();
    for _ in 0..20 {
        match std::fs::write(path, bytes) {
            Ok(()) => return Ok(()),
            Err(err) => last = err.to_string(),
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err(format!("Could not write {}: {last}\n\nIs it still running? Quit it and try again.", path.display()))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// Text for inside a single-quoted PowerShell string.
fn ps(text: &str) -> String {
    text.replace('\'', "''")
}

/// Runs a script in Windows PowerShell without a window.
fn powershell(script: &str) -> Result<(), String> {
    // Without progress records stderr holds only real errors. A script that
    // got to its end succeeded: only a terminating error (Stop) fails it, not
    // a last cmdlet that merely found nothing to remove.
    let script = format!("$ProgressPreference = 'SilentlyContinue'
{script}
exit 0
");
    // -EncodedCommand takes UTF-16LE base64, which avoids every quoting issue.
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", &base64(&utf16)])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| format!("Could not run PowerShell: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("Setup step failed:\n\n{}", String::from_utf8_lossy(&output.stderr).trim()))
    }
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16) | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8) | u32::from(*chunk.get(2).unwrap_or(&0));
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn message(text: &str, icon: MESSAGEBOX_STYLE) {
    unsafe { MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(TITLE), MB_OK | icon) };
}

fn ask(text: &str) -> bool {
    unsafe { MessageBoxW(None, &HSTRING::from(text), &HSTRING::from(TITLE), MB_YESNO | MB_ICONQUESTION) == IDYES }
}

// ---- the window ----

const ID_SIGN_IN: u16 = 101;
const ID_START: u16 = 102;
const ID_SHORTCUT: u16 = 103;
const ID_USAGE: u16 = 104;
const ID_ISLAND: u16 = 105;
const ID_EQUALIZER: u16 = 106;
const ID_THEME: u16 = 110;
const ID_BACKDROP: u16 = 111;
const ID_TOPBAR: u16 = 112;
const ID_AUTOHIDE: u16 = 113;
const ID_INSTALL: u16 = 1;
const ID_CANCEL: u16 = 2;
/// A worker thread finished; lParam is a `Box<Result<String, String>>`.
const WM_APP_DONE: u32 = WM_APP + 1;
/// A worker status line; lParam is a `Box<String>`.
const WM_APP_STATUS: u32 = WM_APP + 2;

struct Ui {
    hwnd: HWND,
    status: HWND,
    install: HWND,
    cancel: HWND,
    controls: Vec<HWND>,
    fonts: Vec<HFONT>,
    finished: bool,
}

thread_local! {
    static UI: RefCell<Option<Ui>> = const { RefCell::new(None) };
}

fn run_window() {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let instance = GetModuleHandleW(None).unwrap_or_default();
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: GetSysColorBrush(COLOR_WINDOW),
            lpszClassName: w!("wdock.Setup"),
            ..Default::default()
        };
        RegisterClassExW(&class);

        let scale = GetDpiForSystem() as f32 / 96.0;
        let px = |v: f32| (v * scale).round() as i32;
        let (client_w, client_h) = (px(460.0), px(486.0));
        let style = WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut frame = windows::Win32::Foundation::RECT { left: 0, top: 0, right: client_w, bottom: client_h };
        let _ = AdjustWindowRectEx(&mut frame, style, false, WS_EX_CONTROLPARENT);
        let (w, h) = (frame.right - frame.left, frame.bottom - frame.top);
        let (x, y) = ((GetSystemMetrics(SM_CXSCREEN) - w) / 2, (GetSystemMetrics(SM_CYSCREEN) - h) / 2);
        let title = HSTRING::from(format!("wdock {VERSION} setup"));
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_CONTROLPARENT,
            w!("wdock.Setup"),
            &title,
            style,
            if x > 0 { x } else { CW_USEDEFAULT },
            y.max(0),
            w,
            h,
            None,
            None,
            Some(instance.into()),
            None,
        ) else {
            message("Could not open the setup window.", MB_ICONERROR);
            return;
        };

        let font = |size: f32, weight: u32| {
            CreateFontW(
                -px(size * 96.0 / 72.0),
                0,
                0,
                0,
                weight as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET,
                Default::default(),
                Default::default(),
                Default::default(),
                0,
                w!("Segoe UI"),
            )
        };
        let (normal, heading, title_font) = (font(9.0, FW_NORMAL.0), font(9.0, FW_SEMIBOLD.0), font(13.0, FW_SEMIBOLD.0));
        let options = Options::detect();
        let dir = install_dir().map(|d| d.display().to_string()).unwrap_or_default();
        let updating = install_dir().is_ok_and(|d| d.join("wdock.exe").exists());

        let mut controls = Vec::new();
        let add = |class: PCWSTR, text: &str, style: u32, id: u16, x: f32, y: f32, w: f32, h: f32, f: HFONT| {
            let child = CreateWindowExW(
                WINDOW_EX_STYLE(0),
                class,
                &HSTRING::from(text),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(style),
                px(x),
                px(y),
                px(w),
                px(h),
                Some(hwnd),
                Some(HMENU(id as usize as *mut _)),
                Some(instance.into()),
                None,
            )
            .unwrap_or_default();
            SendMessageW(child, WM_SETFONT, Some(WPARAM(f.0 as usize)), Some(LPARAM(1)));
            child
        };
        let check = |child: HWND, on: bool| {
            SendMessageW(child, BM_SETCHECK, Some(WPARAM(usize::from(on))), None);
        };
        let combo = |child: HWND, items: &[&str], selected: usize| {
            for item in items {
                let text = HSTRING::from(*item);
                SendMessageW(child, CB_ADDSTRING, None, Some(LPARAM(text.as_ptr() as isize)));
            }
            SendMessageW(child, CB_SETCURSEL, Some(WPARAM(selected)), None);
        };
        let checkbox = WS_TABSTOP.0 | BS_AUTOCHECKBOX as u32;
        let dropdown = WS_TABSTOP.0 | WS_VSCROLL.0 | CBS_DROPDOWNLIST as u32;
        let left = 24.0;

        add(w!("STATIC"), &format!("wdock {VERSION}"), 0, 0, left, 18.0, 400.0, 26.0, title_font);
        let intro = if updating { "Updates wdock for your account; no admin needed." } else { "Installs wdock for your account; no admin needed." };
        add(w!("STATIC"), intro, 0, 0, left, 46.0, 412.0, 18.0, normal);
        add(w!("STATIC"), &format!("Location: {dir}"), 0, 0, left, 66.0, 412.0, 18.0, normal);

        add(w!("STATIC"), "Install", 0, 0, left, 98.0, 412.0, 18.0, heading);
        let sign_in = add(w!("BUTTON"), "Start wdock when I sign in", checkbox, ID_SIGN_IN, left, 120.0, 412.0, 22.0, normal);
        check(sign_in, options.sign_in);
        let start = add(w!("BUTTON"), "Start wdock when setup finishes", checkbox, ID_START, left, 144.0, 412.0, 22.0, normal);
        check(start, options.start_now);
        let shortcut = add(w!("BUTTON"), "Add to the Start menu", checkbox, ID_SHORTCUT, left, 168.0, 412.0, 22.0, normal);
        check(shortcut, options.shortcut);
        let usage_text = if USAGE.is_empty() {
            "Also install agent-usage-bar (not included in this setup)"
        } else {
            "Also install agent-usage-bar (AI agent usage in the tray)"
        };
        let usage = add(w!("BUTTON"), usage_text, checkbox, ID_USAGE, left, 192.0, 412.0, 22.0, normal);
        check(usage, options.usage && !USAGE.is_empty());
        if USAGE.is_empty() {
            let _ = EnableWindow(usage, false);
        }
        add(
            w!("STATIC"),
            "agent-usage-bar is a separate app that signs in to those services; wdock itself makes no network calls.",
            0,
            0,
            left + 18.0,
            214.0,
            394.0,
            32.0,
            normal,
        );

        add(w!("STATIC"), "Appearance and behaviour", 0, 0, left, 256.0, 412.0, 18.0, heading);
        let row = |label: &str, y: f32| {
            add(w!("STATIC"), label, 0, 0, left, y + 3.0, 170.0, 18.0, normal);
        };
        row("Theme", 280.0);
        let theme = add(w!("COMBOBOX"), "", dropdown, ID_THEME, 200.0, 278.0, 236.0, 200.0, normal);
        combo(theme, &["Follow Windows", "Dark", "Light"], options.theme);
        row("Background", 310.0);
        let backdrop = add(w!("COMBOBOX"), "", dropdown, ID_BACKDROP, 200.0, 308.0, 236.0, 200.0, normal);
        combo(backdrop, &["Translucent (acrylic)", "Solid"], options.backdrop);
        row("Top bar", 340.0);
        let topbar = add(w!("COMBOBOX"), "", dropdown, ID_TOPBAR, 200.0, 338.0, 236.0, 200.0, normal);
        combo(topbar, &["On every monitor", "On the main monitor only"], options.topbar);
        row("Dock", 370.0);
        let autohide = add(w!("COMBOBOX"), "", dropdown, ID_AUTOHIDE, 200.0, 368.0, 236.0, 200.0, normal);
        combo(autohide, &["Hide when a window covers it", "Always auto-hide", "Always visible"], options.autohide);
        let island = add(w!("BUTTON"), "Dynamic Island (now playing, Pomodoro, notices)", checkbox, ID_ISLAND, left, 400.0, 412.0, 22.0, normal);
        check(island, options.island);
        let equalizer = add(w!("BUTTON"), "Animated equalizer in the island", checkbox, ID_EQUALIZER, left + 18.0, 424.0, 394.0, 22.0, normal);
        check(equalizer, options.equalizer);
        let _ = EnableWindow(equalizer, options.island);

        let status = add(w!("STATIC"), "", 0, 0, left, 458.0, 230.0, 18.0, normal);
        let action = if updating { "Update" } else { "Install" };
        let install = add(w!("BUTTON"), action, WS_TABSTOP.0 | BS_DEFPUSHBUTTON as u32, ID_INSTALL, 260.0, 450.0, 84.0, 28.0, normal);
        let cancel = add(w!("BUTTON"), "Cancel", WS_TABSTOP.0 | BS_PUSHBUTTON as u32, ID_CANCEL, 352.0, 450.0, 84.0, 28.0, normal);
        controls.extend([sign_in, start, shortcut, usage, island, equalizer, theme, backdrop, topbar, autohide]);

        UI.set(Some(Ui {
            hwnd,
            status,
            install,
            cancel,
            controls,
            fonts: vec![normal, heading, title_font],
            finished: false,
        }));
        let _ = ShowWindow(hwnd, SW_SHOW);

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            if !IsDialogMessageW(hwnd, &msg).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if let Some(ui) = UI.take() {
            for font in ui.fonts {
                let _ = DeleteObject(font.into());
            }
        }
    }
}

fn control(id: u16) -> HWND {
    UI.with_borrow(|ui| {
        let ui = ui.as_ref()?;
        let index = [ID_SIGN_IN, ID_START, ID_SHORTCUT, ID_USAGE, ID_ISLAND, ID_EQUALIZER, ID_THEME, ID_BACKDROP, ID_TOPBAR, ID_AUTOHIDE]
            .iter()
            .position(|&i| i == id)?;
        ui.controls.get(index).copied()
    })
    .unwrap_or_default()
}

fn checked(id: u16) -> bool {
    unsafe { SendMessageW(control(id), BM_GETCHECK, None, None).0 == 1 }
}

fn selected(id: u16) -> usize {
    unsafe { SendMessageW(control(id), CB_GETCURSEL, None, None).0.max(0) as usize }
}

fn read_options() -> Options {
    Options {
        sign_in: checked(ID_SIGN_IN),
        start_now: checked(ID_START),
        shortcut: checked(ID_SHORTCUT),
        usage: checked(ID_USAGE),
        island: checked(ID_ISLAND),
        equalizer: checked(ID_EQUALIZER),
        theme: selected(ID_THEME),
        backdrop: selected(ID_BACKDROP),
        topbar: selected(ID_TOPBAR),
        autohide: selected(ID_AUTOHIDE),
    }
}

fn set_status(text: &str) {
    if let Some(status) = UI.with_borrow(|ui| ui.as_ref().map(|u| u.status)) {
        let _ = unsafe { SetWindowTextW(status, &HSTRING::from(text)) };
    }
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_COMMAND => {
            let id = (wparam.0 & 0xFFFF) as u16;
            match id {
                ID_ISLAND => {
                    let _ = unsafe { EnableWindow(control(ID_EQUALIZER), checked(ID_ISLAND)) };
                }
                ID_CANCEL => {
                    let _ = unsafe { DestroyWindow(hwnd) };
                }
                ID_INSTALL => {
                    let finished = UI.with_borrow(|ui| ui.as_ref().is_some_and(|u| u.finished));
                    if finished {
                        let _ = unsafe { DestroyWindow(hwnd) };
                        return LRESULT(0);
                    }
                    start_install(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_APP_STATUS => {
            let text = unsafe { Box::from_raw(lparam.0 as *mut String) };
            set_status(&text);
            LRESULT(0)
        }
        WM_APP_DONE => {
            let result = unsafe { Box::from_raw(lparam.0 as *mut Result<String, String>) };
            finish_install(*result);
            LRESULT(0)
        }
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            // Labels and checkboxes on the window's own background.
            let hdc = HDC(wparam.0 as *mut _);
            let child = HWND(lparam.0 as *mut _);
            let dim = UI.with_borrow(|ui| ui.as_ref().is_some_and(|u| u.status == child));
            unsafe {
                SetTextColor(hdc, COLORREF(GetSysColor(if dim { COLOR_GRAYTEXT } else { COLOR_WINDOWTEXT })));
                SetBkColor(hdc, COLORREF(GetSysColor(COLOR_WINDOW)));
            }
            LRESULT(unsafe { GetSysColorBrush(COLOR_WINDOW) }.0 as isize)
        }
        WM_CLOSE => {
            let busy = UI.with_borrow(|ui| ui.as_ref().is_some_and(|u| !unsafe { windows::Win32::UI::Input::KeyboardAndMouse::IsWindowEnabled(u.cancel) }.as_bool() && !u.finished));
            if !busy {
                let _ = unsafe { DestroyWindow(hwnd) };
            }
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

fn start_install(hwnd: HWND) {
    let options = read_options();
    UI.with_borrow(|ui| {
        if let Some(ui) = ui {
            for &c in ui.controls.iter().chain([&ui.install, &ui.cancel]) {
                let _ = unsafe { EnableWindow(c, false) };
            }
        }
    });
    set_status("Installing...");
    let target = hwnd.0 as isize;
    std::thread::spawn(move || {
        let post = |msg: u32, payload: isize| {
            let _ = unsafe { PostMessageW(Some(HWND(target as *mut _)), msg, WPARAM(0), LPARAM(payload)) };
        };
        let progress = |text: &str| post(WM_APP_STATUS, Box::into_raw(Box::new(text.to_owned())) as isize);
        let result = install(&options, &progress);
        post(WM_APP_DONE, Box::into_raw(Box::new(result)) as isize);
    });
}

fn finish_install(result: Result<String, String>) {
    let Some((hwnd, install, cancel)) = UI.with_borrow(|ui| ui.as_ref().map(|u| (u.hwnd, u.install, u.cancel))) else {
        return;
    };
    match result {
        Ok(done) => {
            set_status(&done);
            UI.with_borrow_mut(|ui| {
                if let Some(ui) = ui {
                    ui.finished = true;
                }
            });
            unsafe {
                let _ = SetWindowTextW(install, w!("Close"));
                let _ = EnableWindow(install, true);
                let _ = ShowWindow(cancel, windows::Win32::UI::WindowsAndMessaging::SW_HIDE);
            }
            let tip = "Win+Alt+Space opens wdock's launcher. Change these options later by running this setup again.";
            let _ = hwnd;
            message(&format!("{done}\n\n{tip}"), MB_ICONINFORMATION);
        }
        Err(err) => {
            set_status("Setup did not finish.");
            UI.with_borrow(|ui| {
                if let Some(ui) = ui {
                    for &c in ui.controls.iter().chain([&ui.install, &ui.cancel]) {
                        let _ = unsafe { EnableWindow(c, true) };
                    }
                }
            });
            message(&err, MB_ICONERROR);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn quotes_are_doubled_for_powershell() {
        assert_eq!(ps(r"C:\Users\o'brien\x"), r"C:\Users\o''brien\x");
    }

    #[test]
    fn config_keys_round_trip_and_keep_comments() {
        let text = "# top\n[appearance]\n# theme\ntheme = \"auto\"\n\n[island]\nenabled = true\n";
        assert_eq!(get_key(text, "appearance", "theme").as_deref(), Some("auto"));
        let changed = set_key(text, "appearance", "theme", "\"dark\"");
        assert_eq!(get_key(&changed, "appearance", "theme").as_deref(), Some("dark"));
        assert!(changed.contains("# theme"));
        let added = set_key(&changed, "island", "animate_equalizer", "false");
        assert_eq!(get_key(&added, "island", "animate_equalizer").as_deref(), Some("false"));
        assert_eq!(get_key(&added, "island", "enabled").as_deref(), Some("true"));
        let section = set_key(&added, "dock", "auto_hide", "\"off\"");
        assert_eq!(get_key(&section, "dock", "auto_hide").as_deref(), Some("off"));
    }

    #[test]
    fn template_has_every_key_the_window_edits() {
        for (section, key) in [
            ("appearance", "theme"),
            ("appearance", "backdrop"),
            ("topbar", "monitors"),
            ("dock", "auto_hide"),
            ("island", "enabled"),
            ("island", "animate_equalizer"),
        ] {
            assert!(get_key(TEMPLATE, section, key).is_some(), "[{section}] {key}");
        }
    }

    #[test]
    fn flags_parse() {
        let base = Options {
            sign_in: true,
            start_now: true,
            shortcut: true,
            usage: false,
            island: true,
            equalizer: true,
            theme: 0,
            backdrop: 0,
            topbar: 0,
            autohide: 0,
        };
        let args = ["/S", "/NoSignIn", "/Theme=Dark", "/AutoHide=off", "/NoEqualizer"].map(String::from);
        let o = base.clone().with_flags(&args).unwrap();
        assert!(!o.sign_in && o.start_now && !o.equalizer);
        assert_eq!((o.theme, o.autohide), (1, 2));
        assert!(base.clone().with_flags(&["/Theme=blue".into()]).is_err());
        assert!(base.with_flags(&["/Bogus".into()]).is_err());
    }
}
