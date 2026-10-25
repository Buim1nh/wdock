//! `--dev`: try wdock without installing it.
//!
//! The supervisor runs the real shell as a child process and restores
//! explorer's taskbar (and any workspace-hidden windows) when the child ends
//! for any reason, including a crash that never reaches Rust's panic machinery.

use std::io;
use std::process::Command;

use crate::explorer;
use crate::model::windows::restore_orphans;

pub fn supervise() -> io::Result<()> {
    let exe = std::env::current_exe()?;
    let status = Command::new(exe).arg("--dev-child").status();
    match &status {
        Ok(status) => log::info!("dev child exited: {status}"),
        Err(err) => log::error!("failed to run dev child: {err}"),
    }
    restore_orphans();
    explorer::restore_taskbar();
    status.map(drop)
}
