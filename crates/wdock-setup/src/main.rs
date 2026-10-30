//! wdock-setup: installs wdock for the current user, no admin needed.
//!
//! * `wdock-setup.exe` asks, then installs or updates:
//!   1. quits a running wdock (its own Ctrl+Alt+Shift+Q hotkey, so it puts
//!      explorer's taskbar back first),
//!   2. writes `%LOCALAPPDATA%\Programs\wdock\wdock.exe` and a copy of
//!      itself as `uninstall.exe` next to it,
//!   3. registers the per-user scheduled task `wdock` that runs
//!      `wdock.exe --guard` at every sign-in (as `scripts\install.ps1` does),
//!   4. lists wdock in Settings > Apps, and starts it.
//! * `uninstall.exe --uninstall` (what Settings > Apps runs) reverses that and
//!   brings explorer's taskbar back. Config and logs are kept.
//! * `/S` or `--silent` skips the questions.
//!
//! The scheduled task and registry work go through Windows PowerShell, which
//! every Windows 10/11 has, using the same cmdlets as the install script.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, IDYES, MB_ICONERROR, MB_ICONINFORMATION, MB_ICONQUESTION, MB_OK, MB_YESNO, MESSAGEBOX_STYLE,
    MessageBoxW, PostMessageW, WM_HOTKEY,
};
use windows::core::{HSTRING, w};

const PAYLOAD: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/wdock.exe"));
const VERSION: &str = env!("CARGO_PKG_VERSION");
const TITLE: &str = "wdock setup";
/// wdock's emergency-exit hotkey id: a clean quit that restores the taskbar.
const HOTKEY_QUIT: usize = 1;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let silent = args.iter().any(|a| a.eq_ignore_ascii_case("/S") || a == "--silent");
    let uninstall = args.iter().any(|a| a == "--uninstall");
    let result = if uninstall { run_uninstall(silent) } else { run_install(silent) };
    if let Err(err) = result {
        if !silent {
            message(&err, MB_ICONERROR);
        }
        std::process::exit(1);
    }
}

fn install_dir() -> Result<PathBuf, String> {
    let local = std::env::var_os("LOCALAPPDATA").ok_or("LOCALAPPDATA is not set")?;
    Ok(PathBuf::from(local).join("Programs").join("wdock"))
}

fn run_install(silent: bool) -> Result<(), String> {
    if PAYLOAD.is_empty() {
        return Err("This setup was built without wdock.exe inside; build wdock first.".into());
    }
    let dir = install_dir()?;
    let exe = dir.join("wdock.exe");
    let verb = if exe.exists() { "update" } else { "install" };
    if !silent
        && !ask(&format!(
            "This will {verb} wdock {VERSION} for your account:\n\n\
             \u{2022} a macOS-style top bar, dock and Dynamic Island\n\
             \u{2022} explorer's taskbar is hidden while wdock runs\n\
             \u{2022} it starts at every sign-in (Ctrl+Alt+Shift+Q quits it)\n\n\
             Location: {}\n\nContinue?",
            dir.display()
        ))
    {
        return Ok(());
    }

    quit_running_wdock();
    std::fs::create_dir_all(&dir).map_err(|e| format!("Could not create {}: {e}", dir.display()))?;
    write_retrying(&exe, PAYLOAD)?;
    let uninstaller = dir.join("uninstall.exe");
    let me = std::env::current_exe().map_err(|e| e.to_string())?;
    if !same_file(&me, &uninstaller) {
        std::fs::copy(&me, &uninstaller).map_err(|e| format!("Could not write {}: {e}", uninstaller.display()))?;
    }

    powershell(&format!(
        r#"$ErrorActionPreference = 'Stop'
$exe = '{exe}'
$user = "$env:USERDOMAIN\$env:USERNAME"
$task = @{{
    TaskName    = 'wdock'
    Description = 'Starts wdock at sign-in'
    Action      = New-ScheduledTaskAction -Execute $exe -Argument '--guard'
    Trigger     = New-ScheduledTaskTrigger -AtLogOn -User $user
    Principal   = New-ScheduledTaskPrincipal -UserId $user -LogonType Interactive -RunLevel Limited
    Settings    = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew
}}
Register-ScheduledTask @task -Force | Out-Null
# Older installs used the Run key; two guards must not run.
Remove-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name wdock -ErrorAction SilentlyContinue
$key = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\wdock'
New-Item -Path $key -Force | Out-Null
$values = @{{
    DisplayName = 'wdock'; DisplayVersion = '{VERSION}'; Publisher = 'wdock'
    DisplayIcon = $exe; InstallLocation = '{dir}'
    UninstallString = '"{uninstaller}" --uninstall'; QuietUninstallString = '"{uninstaller}" --uninstall /S'
    URLInfoAbout = 'https://github.com/Buim1nh/wdock'
}}
foreach ($v in $values.GetEnumerator()) {{ New-ItemProperty -Path $key -Name $v.Key -Value $v.Value -PropertyType String -Force | Out-Null }}
New-ItemProperty -Path $key -Name NoModify -Value 1 -PropertyType DWord -Force | Out-Null
New-ItemProperty -Path $key -Name NoRepair -Value 1 -PropertyType DWord -Force | Out-Null
New-ItemProperty -Path $key -Name EstimatedSize -Value {size_kb} -PropertyType DWord -Force | Out-Null
Start-ScheduledTask -TaskName wdock"#,
        exe = ps_quote(&exe),
        dir = ps_quote(&dir),
        uninstaller = ps_quote(&uninstaller),
        size_kb = PAYLOAD.len() / 1024,
    ))?;

    if !silent {
        message(
            &format!(
                "wdock {VERSION} is installed and running.\n\n\
                 Win+Alt+/ shows its shortcuts. To remove it, use Settings > Apps > wdock."
            ),
            MB_ICONINFORMATION,
        );
    }
    Ok(())
}

fn run_uninstall(silent: bool) -> Result<(), String> {
    let dir = install_dir()?;
    if !silent && !ask("Remove wdock and bring explorer's taskbar back?\n\nYour settings and logs are kept.") {
        return Ok(());
    }
    quit_running_wdock();
    powershell(
        r#"Unregister-ScheduledTask -TaskName wdock -Confirm:$false -ErrorAction SilentlyContinue
Remove-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name wdock -ErrorAction SilentlyContinue
Remove-Item -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\wdock' -Recurse -ErrorAction SilentlyContinue"#,
    )?;
    let exe = dir.join("wdock.exe");
    if exe.exists() {
        // A killed wdock cannot clean up after itself; this puts the taskbar
        // and any windows hidden in other workspaces back.
        let _ = Command::new(&exe).arg("--restore-taskbar").creation_flags(CREATE_NO_WINDOW).status();
        let _ = std::fs::remove_file(&exe);
    }
    // This program may be the uninstaller inside `dir`: let cmd delete the
    // folder once it has exited.
    let _ = Command::new("cmd")
        .raw_arg(format!("/c ping -n 3 127.0.0.1 >nul & rmdir /s /q \"{}\"", dir.display()))
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
    if !silent {
        message("wdock was removed. Explorer's taskbar is back.", MB_ICONINFORMATION);
    }
    Ok(())
}

/// Asks the running wdock to quit as Ctrl+Alt+Shift+Q would, and waits for
/// the shell and its guard to go.
fn quit_running_wdock() {
    let main = unsafe { FindWindowW(w!("wdock.Main"), None) };
    let Ok(main) = main else {
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
    Err(format!("Could not write {}: {last}\n\nIs wdock still running? Quit it with Ctrl+Alt+Shift+Q and try again.", path.display()))
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// A path as a single-quoted PowerShell string literal body.
fn ps_quote(path: &Path) -> String {
    path.display().to_string().replace('\'', "''")
}

/// Runs a script in Windows PowerShell without a window.
fn powershell(script: &str) -> Result<(), String> {
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
        assert_eq!(ps_quote(Path::new(r"C:\Users\o'brien\x")), r"C:\Users\o''brien\x");
    }
}
