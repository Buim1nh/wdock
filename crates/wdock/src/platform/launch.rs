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

/// Runs a PowerShell script elevated (UAC prompt) in a console that stays open
/// so its result can be read. The script travels in the command line rather
/// than a file, so nothing user-writable is executed with admin rights.
pub fn powershell_elevated(script: &str, arguments: &str) {
    let command = format!("& {{
{script}
}} {arguments}");
    let utf16: Vec<u8> = command.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let parameters = format!("-NoProfile -NoExit -ExecutionPolicy Bypass -EncodedCommand {}", base64(&utf16));
    let spawned = thread::Builder::new().name("elevate".into()).spawn(move || {
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) };
        let wide = |s: &str| s.encode_utf16().chain([0]).collect::<Vec<u16>>();
        let (verb, file, params) = (wide("runas"), wide("powershell.exe"), wide(&parameters));
        let mut info = SHELLEXECUTEINFOW {
            cbSize: size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOASYNC,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params.as_ptr()),
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };
        if let Err(err) = unsafe { ShellExecuteExW(&mut info) } {
            // Also reached when the UAC prompt is declined.
            log::warn!("elevated PowerShell did not start: {err}");
        }
    });
    if let Err(err) = spawned {
        log::error!("could not spawn elevation thread: {err}");
    }
}

/// Quotes a value as a PowerShell single-quoted string literal.
pub fn powershell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { TABLE[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn base64_and_quoting_match_powershell() {
        assert_eq!(super::base64(b""), "");
        assert_eq!(super::base64(b"f"), "Zg==");
        assert_eq!(super::base64(b"fo"), "Zm8=");
        assert_eq!(super::base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(super::powershell_quote("It's"), "'It''s'");
    }
}
