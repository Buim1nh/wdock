//! `--guard`: started at sign-in by the HKCU Run key install.ps1 writes.
//!
//! It does almost nothing itself (no window, no COM, no graphics), so it is
//! very unlikely to crash. It keeps a `--shell` child alive:
//!
//! * the child crashes: windows it had hidden for workspaces are shown again
//!   and a new child starts (which re-registers the tray, so apps re-add
//!   their icons);
//! * it crashes repeatedly, or exits on purpose (Ctrl+Alt+Shift+Q): explorer's
//!   taskbar comes back for the rest of the session;
//! * the session is ending: the guard simply exits.

use std::collections::VecDeque;
use std::io;
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SHUTTINGDOWN};

use crate::explorer;
use crate::model::windows::restore_orphans;

/// This many crashes within `CRASH_WINDOW` give up on wdock for the session.
const MAX_CRASHES: usize = 3;
const CRASH_WINDOW: Duration = Duration::from_secs(60);
const RESTART_DELAY: Duration = Duration::from_secs(1);

fn session_ending() -> bool {
    unsafe { GetSystemMetrics(SM_SHUTTINGDOWN) != 0 }
}

pub fn supervise() -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let mut crashes: VecDeque<Instant> = VecDeque::new();
    loop {
        let status = Command::new(&exe).arg("--shell").status();
        if session_ending() {
            log::info!("session ending; guard exits");
            return Ok(());
        }
        match &status {
            Ok(status) if status.success() => {
                log::info!("wdock exited on purpose; explorer's taskbar is back");
                return give_up();
            }
            Ok(status) => log::error!("wdock shell died: {status}"),
            Err(err) => log::error!("could not start wdock shell: {err}"),
        }

        restore_orphans();
        let now = Instant::now();
        crashes.push_back(now);
        while crashes.front().is_some_and(|&t| now - t > CRASH_WINDOW) {
            crashes.pop_front();
        }
        if crashes.len() >= MAX_CRASHES || status.is_err() {
            log::error!("wdock keeps failing; using explorer's taskbar for this session");
            return give_up();
        }
        thread::sleep(RESTART_DELAY);
    }
}

fn give_up() -> io::Result<()> {
    restore_orphans();
    explorer::restore_taskbar();
    Ok(())
}
