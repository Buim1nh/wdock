use windows::Win32::Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE};
use windows::Win32::System::Threading::CreateMutexW;
use windows::core::{PCWSTR, w};

/// Held for the lifetime of the shell (or the guard); a second one refuses to start.
pub struct SingleInstance(HANDLE);

impl SingleInstance {
    /// Returns `Ok(None)` when another wdock shell is already running in this session.
    pub fn acquire() -> windows::core::Result<Option<Self>> {
        Self::named(w!("Local\\wdock-shell-instance"))
    }

    /// Returns `Ok(None)` when another guard is already running in this session.
    pub fn acquire_guard() -> windows::core::Result<Option<Self>> {
        Self::named(w!("Local\\wdock-guard-instance"))
    }

    fn named(name: PCWSTR) -> windows::core::Result<Option<Self>> {
        let handle = unsafe { CreateMutexW(None, true, name)? };
        if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
            unsafe { CloseHandle(handle)? };
            return Ok(None);
        }
        Ok(Some(Self(handle)))
    }
}

impl Drop for SingleInstance {
    fn drop(&mut self) {
        let _ = unsafe { CloseHandle(self.0) };
    }
}
