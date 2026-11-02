use std::thread;

use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize};
use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{PCWSTR, w};

/// Opens a path, URI or `shell:AppsFolder\<AUMID>` without blocking the UI thread.
pub fn open(target: &str) {
    launch(target, false);
}

/// Windows can elevate executable and shortcut targets through the normal UAC prompt.
pub fn supports_elevation(target: &str) -> bool {
    std::path::Path::new(target).extension().and_then(|value| value.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe") || ext.eq_ignore_ascii_case("lnk"))
}

pub fn open_as_admin(target: &str) {
    launch(target, true);
}

fn launch(target: &str, elevate: bool) {
    let target = target.to_owned();
    let spawned = thread::Builder::new().name("launch".into()).spawn(move || {
        let initialized = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).is_ok() };
        let file: Vec<u16> = target.encode_utf16().chain([0]).collect();
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOASYNC,
            lpVerb: if elevate { w!("runas") } else { PCWSTR::null() },
            lpFile: PCWSTR(file.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        match unsafe { ShellExecuteExW(&mut info) } {
            Ok(()) => log::info!("launched {target}"),
            Err(err) => log::warn!("failed to launch {target}: {err}"),
        }
        if initialized { unsafe { CoUninitialize() }; }
    });
    if let Err(err) = spawned {
        log::error!("could not spawn launch thread: {err}");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn elevation_option_is_limited_to_executables_and_shortcuts() {
        assert!(super::supports_elevation(r"C:\Apps\Editor.EXE"));
        assert!(super::supports_elevation(r"C:\Apps\Editor.lnk"));
        assert!(!super::supports_elevation("ms-settings:"));
        assert!(!super::supports_elevation(r"shell:AppsFolder\Example.App!Main"));
    }
}
