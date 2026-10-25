//! Lock, sleep, sign out, restart and shut down — the explorer power menu.

use windows::Win32::Foundation::{CloseHandle, HANDLE, LUID};
use windows::Win32::Security::{
    AdjustTokenPrivileges, LUID_AND_ATTRIBUTES, LookupPrivilegeValueW, SE_PRIVILEGE_ENABLED,
    SE_SHUTDOWN_NAME, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY,
};
use windows::Win32::System::Power::SetSuspendState;
use windows::Win32::System::Shutdown::{
    EWX_HYBRID_SHUTDOWN, EWX_LOGOFF, EWX_POWEROFF, EWX_REBOOT, EWX_SHUTDOWN, ExitWindowsEx,
    LockWorkStation, SHTDN_REASON_FLAG_PLANNED, SHTDN_REASON_MAJOR_OTHER, SHTDN_REASON_MINOR_OTHER,
};
use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAction {
    Lock,
    Sleep,
    SignOut,
    Restart,
    ShutDown,
}

impl SessionAction {
    /// Actions that end the session ask for a second click first.
    pub fn needs_confirmation(self) -> bool {
        matches!(self, Self::SignOut | Self::Restart | Self::ShutDown)
    }
}

pub fn perform(action: SessionAction) {
    log::info!("session action: {action:?}");
    let reason = SHTDN_REASON_MAJOR_OTHER | SHTDN_REASON_MINOR_OTHER | SHTDN_REASON_FLAG_PLANNED;
    let result = unsafe {
        match action {
            SessionAction::Lock => LockWorkStation(),
            SessionAction::Sleep => {
                if SetSuspendState(false, false, false) { Ok(()) } else { Err(windows::core::Error::from_thread()) }
            }
            SessionAction::SignOut => ExitWindowsEx(EWX_LOGOFF, reason),
            SessionAction::Restart => enable_shutdown_privilege().and_then(|()| ExitWindowsEx(EWX_REBOOT, reason)),
            // Hybrid shutdown keeps Fast Startup working, like the Start menu.
            SessionAction::ShutDown => enable_shutdown_privilege()
                .and_then(|()| ExitWindowsEx(EWX_SHUTDOWN | EWX_POWEROFF | EWX_HYBRID_SHUTDOWN, reason)),
        }
    };
    if let Err(err) = result {
        log::error!("{action:?} failed: {err}");
    }
}

fn enable_shutdown_privilege() -> windows::core::Result<()> {
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut token)?;
        let mut luid = LUID::default();
        let result = LookupPrivilegeValueW(None, SE_SHUTDOWN_NAME, &mut luid).and_then(|()| {
            let privileges = TOKEN_PRIVILEGES {
                PrivilegeCount: 1,
                Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }],
            };
            AdjustTokenPrivileges(token, false, Some(&privileges), 0, None, None)
        });
        let _ = CloseHandle(token);
        result
    }
}
