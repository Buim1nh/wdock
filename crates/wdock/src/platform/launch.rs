use std::thread;

use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx};
use windows::Win32::UI::Shell::{SEE_MASK_FLAG_NO_UI, SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::PCWSTR;

/// Opens a path, URI or `shell:AppsFolder\<AUMID>` without blocking the UI thread.
pub fn open(target: &str) {
    let target = target.to_owned();
    let spawned = thread::Builder::new().name("launch".into()).spawn(move || {
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        let file: Vec<u16> = target.encode_utf16().chain([0]).collect();
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOASYNC | SEE_MASK_FLAG_NO_UI,
            lpFile: PCWSTR(file.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        match unsafe { ShellExecuteExW(&mut info) } {
            Ok(()) => log::info!("launched {target}"),
            Err(err) => log::warn!("failed to launch {target}: {err}"),
        }
    });
    if let Err(err) = spawned {
        log::error!("could not spawn launch thread: {err}");
    }
}
