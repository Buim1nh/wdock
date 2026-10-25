use std::path::PathBuf;

/// `%LOCALAPPDATA%\wdock` — logs, caches and runtime state (not roamed).
pub fn local_dir() -> PathBuf {
    std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("wdock")
}

pub fn log_dir() -> PathBuf {
    local_dir().join("logs")
}

/// `%SystemRoot%\explorer.exe`, never resolved through PATH.
pub fn explorer_exe() -> PathBuf {
    std::env::var_os("SystemRoot")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
        .join("explorer.exe")
}
