//! Explicit file operations; no watching, contents, or recursive enumeration.
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
static CHECKING: AtomicBool = AtomicBool::new(false);
pub fn checking() -> bool { CHECKING.load(Ordering::Acquire) }

pub fn inspect(paths: Vec<PathBuf>, done: impl FnOnce(Vec<(PathBuf, bool)>) + Send + 'static) -> bool {
    if CHECKING.swap(true, Ordering::AcqRel) { return false; }
    if let Err(e) = std::thread::Builder::new().name("bookmark-check".into()).spawn(move || {
        let result = paths.into_iter().filter_map(|p| {
            let text = p.to_string_lossy();
            // UNC/device paths and mapped network drives stay unchecked.
            if text.starts_with(r"\") { return None; }
            let root = text.get(..3)?;
            let drive = unsafe { windows::Win32::Storage::FileSystem::GetDriveTypeW(&windows::core::HSTRING::from(root)) };
            if drive != 3 && drive != 2 && drive != 5 && drive != 6 { return None; }
            let exists = p.try_exists().unwrap_or(false); Some((p, exists))
        }).collect();
        CHECKING.store(false, Ordering::Release);
        done(result);
    }) { CHECKING.store(false, Ordering::Release); log::warn!("cannot check bookmarks: {e}"); return false; }
    true
}
pub fn reveal(path: PathBuf) {
    if let Err(e) = std::thread::Builder::new().name("bookmark-reveal".into()).spawn(move || {
        use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, CoTaskMemFree, COINIT_APARTMENTTHREADED};
        use windows::Win32::UI::Shell::{SHParseDisplayName, SHOpenFolderAndSelectItems};
        let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) }.is_ok();
        let mut id = std::ptr::null_mut();
        let result = unsafe { SHParseDisplayName(&windows::core::HSTRING::from(path.as_os_str()), None, &mut id, 0, None) };
        if let Err(e) = result.and_then(|()| unsafe { SHOpenFolderAndSelectItems(id,None,0) }) { log::warn!("cannot show bookmark: {e}"); }
        if !id.is_null() { unsafe { CoTaskMemFree(Some(id.cast())) }; }
        if initialized { unsafe { CoUninitialize() }; }
    }) { log::warn!("cannot start Explorer: {e}"); }
}
