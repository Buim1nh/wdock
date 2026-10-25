// Release builds have no console; everything goes to the log file.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod dev;
mod explorer;
mod guard;
mod instance;
mod logging;
mod model;
mod paths;
mod platform;
mod services;
mod ui;

use std::process::ExitCode;

/// How this process was started.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// wdock itself: `--shell` from the guard, or no arguments.
    Shell,
    /// `--guard`: started at sign-in (see install.ps1). Keeps a `--shell`
    /// child alive and restores explorer's taskbar if wdock gives up.
    Guard,
    /// `--dev`: runs a `--dev-child` and restores explorer's taskbar however it ends.
    DevSupervisor,
    /// `--dev-child`: the actual shell, started by the dev supervisor.
    DevChild,
    /// `--restore-taskbar`: shows explorer's taskbar and any windows hidden for
    /// workspaces (uninstall.ps1, or by hand after killing wdock).
    RestoreTaskbar,
}

fn parse_mode() -> Result<Mode, String> {
    let mut mode = Mode::Shell;
    for arg in std::env::args().skip(1) {
        mode = match arg.as_str() {
            "--dev" => Mode::DevSupervisor,
            "--guard" => Mode::Guard,
            "--shell" => Mode::Shell,
            "--restore-taskbar" => Mode::RestoreTaskbar,
            "--dev-child" => Mode::DevChild,
            other => return Err(format!("unknown argument: {other}")),
        };
    }
    Ok(mode)
}

fn main() -> ExitCode {
    let mode = match parse_mode() {
        Ok(mode) => mode,
        Err(err) => {
            eprintln!("{err}\nusage: wdock [--dev | --guard | --restore-taskbar]");
            return ExitCode::from(2);
        }
    };

    let log_name = match mode {
        Mode::DevSupervisor => "wdock-dev.log",
        Mode::Guard => "wdock-guard.log",
        Mode::Shell | Mode::DevChild | Mode::RestoreTaskbar => "wdock.log",
    };
    if let Err(err) = logging::init(&paths::log_dir().join(log_name)) {
        eprintln!("failed to open log file: {err}");
    }
    log::info!("wdock {} starting, mode={mode:?}", env!("CARGO_PKG_VERSION"));

    let result: Result<(), Box<dyn std::error::Error>> = match mode {
        Mode::DevSupervisor => dev::supervise().map_err(Into::into),
        Mode::Guard => guard::supervise().map_err(Into::into),
        Mode::RestoreTaskbar => {
            model::windows::restore_orphans();
            explorer::restore_taskbar();
            Ok(())
        }
        Mode::Shell | Mode::DevChild => app::run(mode).map_err(Into::into),
    };

    match result {
        Ok(()) => {
            log::info!("wdock exited cleanly");
            ExitCode::SUCCESS
        }
        Err(err) => {
            log::error!("wdock failed: {err:#}");
            ExitCode::FAILURE
        }
    }
}
