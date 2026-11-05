//! `--reload`: reloads the running shell from outside it, so it still works
//! when the shell's own UI and hotkeys do not.
//!
//! A shell that answers rebuilds itself in place. One that stays silent is
//! hung: it is terminated with a failure code, which the guard treats as a
//! crash and restarts. Without a guard, this process starts the new shell.

use std::io;
use std::process::Command;

use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WAIT_OBJECT_0, WPARAM};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE, TerminateProcess, WaitForSingleObject,
};
use windows::Win32::UI::WindowsAndMessaging::{
    FindWindowW, GetWindowThreadProcessId, PostMessageW, SMTO_ABORTIFHUNG, SendMessageTimeoutW, WM_NULL,
};
use windows::core::w;

use crate::app::WM_APP_RELOAD;
use crate::instance::SingleInstance;

const RESPONSE_TIMEOUT_MS: u32 = 3000;
const EXIT_TIMEOUT_MS: u32 = 5000;
const HUNG_EXIT_CODE: u32 = 1;

fn responds(hwnd: HWND) -> bool {
    let result = unsafe {
        SendMessageTimeoutW(hwnd, WM_NULL, WPARAM(0), LPARAM(0), SMTO_ABORTIFHUNG, RESPONSE_TIMEOUT_MS, None)
    };
    result.0 != 0
}

fn terminate(hwnd: HWND) -> io::Result<()> {
    let mut pid = 0;
    unsafe { GetWindowThreadProcessId(hwnd, Some(&mut pid)) };
    let process = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, false, pid) }.map_err(io::Error::other)?;
    let killed = unsafe { TerminateProcess(process, HUNG_EXIT_CODE) }.map_err(io::Error::other);
    // The single-instance mutex is only free once the process is gone.
    let gone = killed.is_ok() && unsafe { WaitForSingleObject(process, EXIT_TIMEOUT_MS) } == WAIT_OBJECT_0;
    let _ = unsafe { CloseHandle(process) };
    killed?;
    if !gone {
        return Err(io::Error::other("the hung shell did not exit"));
    }
    Ok(())
}

fn start_shell() -> io::Result<()> {
    Command::new(std::env::current_exe()?).arg("--shell").spawn().map(drop)
}

pub fn request() -> io::Result<()> {
    let Ok(hwnd) = (unsafe { FindWindowW(w!("wdock.Main"), None) }) else {
        log::info!("reload: no shell is running; starting one");
        return start_shell();
    };
    if responds(hwnd) {
        log::info!("reload: asking the running shell to rebuild");
        return unsafe { PostMessageW(Some(hwnd), WM_APP_RELOAD, WPARAM(0), LPARAM(0)) }.map_err(io::Error::other);
    }
    log::warn!("reload: the shell did not answer for {RESPONSE_TIMEOUT_MS} ms; restarting it");
    terminate(hwnd)?;
    if SingleInstance::guard_running() { Ok(()) } else { start_shell() }
}
