//! "Start at sign-in": the per-user scheduled task `wdock`, the same one the
//! setup registers. The Run key is not used because MSIX virtualizes HKCU for
//! some callers, while scheduled tasks are not virtualized.

use std::os::windows::process::CommandExt;
use std::process::Command;

const TASK: &str = "wdock";
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// Whether the sign-in task exists. Runs `schtasks` once, so call it when a
/// window opens, not per frame.
pub fn enabled() -> bool {
    Command::new("schtasks.exe")
        .args(["/Query", "/TN", TASK])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Registers or removes the task in a background thread (PowerShell takes a
/// moment, and the UI thread must not wait for it).
pub fn set(on: bool) {
    let Ok(exe) = std::env::current_exe() else {
        log::error!("sign-in: cannot find wdock.exe");
        return;
    };
    let script = if on { register(&exe.display().to_string()) } else { unregister() };
    let spawned = std::thread::Builder::new().name("sign-in".into()).spawn(move || {
        match Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            Ok(out) if out.status.success() => log::info!("start at sign-in: {}", if on { "on" } else { "off" }),
            Ok(out) => log::error!("start at sign-in failed: {}", String::from_utf8_lossy(&out.stderr).trim()),
            Err(err) => log::error!("could not run PowerShell: {err}"),
        }
    });
    if let Err(err) = spawned {
        log::error!("could not spawn sign-in thread: {err}");
    }
}

/// Same settings as the setup's task: at logon, limited rights, no time limit.
fn register(exe: &str) -> String {
    let exe = exe.replace('\'', "''");
    format!(
        "$u = [Security.Principal.WindowsIdentity]::GetCurrent().Name\n\
         Register-ScheduledTask -TaskName '{TASK}' -Description 'Starts wdock at sign-in' \
         -Action (New-ScheduledTaskAction -Execute '{exe}' -Argument '--guard') \
         -Trigger (New-ScheduledTaskTrigger -AtLogOn -User $u) \
         -Principal (New-ScheduledTaskPrincipal -UserId $u -LogonType Interactive -RunLevel Limited) \
         -Settings (New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries \
         -ExecutionTimeLimit ([TimeSpan]::Zero) -MultipleInstances IgnoreNew) -Force | Out-Null"
    )
}

fn unregister() -> String {
    format!("Unregister-ScheduledTask -TaskName '{TASK}' -Confirm:$false -ErrorAction SilentlyContinue")
}
