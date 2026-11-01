//! Atomic local writes for user-created Island data.
use std::os::windows::ffi::OsStrExt;
use std::path::Path;
use windows::core::HSTRING;
use windows::Win32::Storage::FileSystem::{MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH};

pub fn write(path: &Path, data: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() { std::fs::create_dir_all(parent)?; }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, data)?;
    let from = HSTRING::from_wide(&tmp.as_os_str().encode_wide().collect::<Vec<_>>());
    let to = HSTRING::from_wide(&path.as_os_str().encode_wide().collect::<Vec<_>>());
    unsafe { MoveFileExW(&from, &to, MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) }
        .map_err(std::io::Error::other)
}
