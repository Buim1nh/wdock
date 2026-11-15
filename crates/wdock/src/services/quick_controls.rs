//! Quick Settings toggles. State is read and changed on workers so the UI never
//! blocks; a worker posts back to the open panel when it finishes instead of
//! the panel polling.
use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicU8, AtomicUsize, Ordering};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_TIMER};

static WATCHER: AtomicIsize = AtomicIsize::new(0);
static WATCHER_TIMER: AtomicUsize = AtomicUsize::new(0);
static NIGHT_LIGHT_BUSY: AtomicBool = AtomicBool::new(false);
// 0 = unknown/unavailable, 1 = off, 2 = on.
static AIRPLANE: AtomicU8 = AtomicU8::new(0);
static AIRPLANE_BUSY: AtomicBool = AtomicBool::new(false);
static AIRPLANE_PENDING: AtomicBool = AtomicBool::new(false);
static QUICK_MESSAGE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
static PROJECTION_BUSY: AtomicBool = AtomicBool::new(false);
static ENERGY_SAVER_BUSY: AtomicBool = AtomicBool::new(false);

// Windows 11 24H2/25H2's Energy Saver override uses a four-byte WNF value.
// This is an undocumented Windows interface, so calls are restricted to the
// Windows builds already supported by wdock and the outcome is verified via
// the public GUID_ENERGY_SAVER_STATUS notification.
const ENERGY_SAVER_OVERRIDE: u64 = 0x41c6013da3bc3075;

#[link(name = "ntdll")]
unsafe extern "system" {
    fn NtQueryWnfStateData(name: *const u64, type_id: *const std::ffi::c_void, scope: *const std::ffi::c_void, stamp: *mut u32, buffer: *mut std::ffi::c_void, size: *mut u32) -> i32;
    fn NtUpdateWnfStateData(name: *const u64, buffer: *const std::ffi::c_void, size: u32, type_id: *const std::ffi::c_void, scope: *const std::ffi::c_void, stamp: u32, check_stamp: u32) -> i32;
}

fn read_energy_override() -> Result<(u32, u32), i32> {
    let mut value = 0u32;
    let mut size = std::mem::size_of::<u32>() as u32;
    let mut stamp = 0u32;
    let status = unsafe { NtQueryWnfStateData(&ENERGY_SAVER_OVERRIDE, std::ptr::null(), std::ptr::null(), &mut stamp, (&mut value as *mut u32).cast(), &mut size) };
    if status < 0 { return Err(status); }
    if size != 4 || !matches!(value, 1 | 2) { return Err(-1); }
    Ok((value, stamp))
}

fn write_energy_override(value: u32, stamp: u32) -> Result<(), i32> {
    if !matches!(value, 1 | 2) { return Err(-1); }
    let status = unsafe { NtUpdateWnfStateData(&ENERGY_SAVER_OVERRIDE, (&value as *const u32).cast(), 4, std::ptr::null(), std::ptr::null(), stamp, 1) };
    if status < 0 { Err(status) } else { Ok(()) }
}

pub fn toggle_energy_saver() {
    if ENERGY_SAVER_BUSY.swap(true, Ordering::AcqRel) { return; }
    message("Changing Energy saver...");
    if std::thread::Builder::new().name("wdock-energy-saver".into()).spawn(|| {
        let result = (|| -> Result<bool, &'static str> {
            if !crate::platform::virtual_desktops::supported_windows() { return Err("Energy saver is unsupported on this Windows build"); }
            let (current, stamp) = read_energy_override().map_err(|_| "Could not read Energy saver")?;
            let desired = !crate::services::power::energy_saver().unwrap_or(current == 1);
            write_energy_override(if desired { 1 } else { 2 }, stamp).map_err(|_| "Could not change Energy saver")?;
            for _ in 0..50 {
                if crate::services::power::energy_saver() == Some(desired) { return Ok(desired); }
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            Err("Energy saver change not confirmed")
        })();
        match result {
            Ok(true) => message("Energy saver on"),
            Ok(false) => message("Energy saver off"),
            Err(error) => message(error),
        }
        ENERGY_SAVER_BUSY.store(false, Ordering::Release);
    }).is_err() { message("Could not change Energy saver"); ENERGY_SAVER_BUSY.store(false, Ordering::Release); }
}

pub fn quick_message() -> Option<String> { QUICK_MESSAGE.lock().ok().and_then(|v| v.clone()) }
fn message(value: &str) {
    if let Ok(mut v) = QUICK_MESSAGE.lock() { *v = Some(value.into()); }
    notify();
}

/// The open Quick Settings panel hears about finished work as `WM_TIMER(id)`.
pub fn watch(hwnd: Option<HWND>, timer: usize) {
    WATCHER_TIMER.store(timer, Ordering::Release);
    WATCHER.store(hwnd.map_or(0, |h| h.0 as isize), Ordering::Release);
}

fn notify() {
    let hwnd = WATCHER.load(Ordering::Acquire);
    if hwnd != 0 {
        let timer = WATCHER_TIMER.load(Ordering::Acquire);
        let _ = unsafe { PostMessageW(Some(HWND(hwnd as *mut _)), WM_TIMER, WPARAM(timer), LPARAM(0)) };
    }
}

pub fn set_projection(index: usize) {
    use windows::Win32::Devices::Display::{SDC_APPLY, SDC_TOPOLOGY_CLONE, SDC_TOPOLOGY_EXTEND, SDC_TOPOLOGY_EXTERNAL, SDC_TOPOLOGY_INTERNAL, SetDisplayConfig};
    let Some(mode) = [SDC_TOPOLOGY_INTERNAL, SDC_TOPOLOGY_CLONE, SDC_TOPOLOGY_EXTEND, SDC_TOPOLOGY_EXTERNAL].get(index).copied() else { return; };
    if PROJECTION_BUSY.swap(true, Ordering::AcqRel) { return; }
    message("Applying projection…");
    if std::thread::Builder::new().name("wdock-projection".into()).spawn(move || {
        let result = unsafe { SetDisplayConfig(None, None, SDC_APPLY | mode) };
        message(if result == 0 { "Projection updated" } else { "Could not change projection" });
        PROJECTION_BUSY.store(false, Ordering::Release);
    }).is_err() { message("Could not change projection"); PROJECTION_BUSY.store(false, Ordering::Release); }
}

pub fn energy_saver() -> Option<bool> {
    if let Some(state) = crate::services::power::energy_saver() { return Some(state); }
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    let mut status = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut status).ok()?; }
    match status.SystemStatusFlag { 0 => Some(false), 1 => Some(true), _ => None }
}

/// Reads the current Night Light state without changing its schedule.
pub fn night_light() -> Option<bool> {
    win_nightlight_lib::Nightlight::new().read_state().ok().map(|state| state.is_active())
}

pub fn toggle_night_light() {
    if NIGHT_LIGHT_BUSY.swap(true, Ordering::AcqRel) { return; }
    message("Changing Night light…");
    let started = std::thread::Builder::new().name("wdock-night-light".into()).spawn(|| {
        let light = win_nightlight_lib::Nightlight::new();
        let result = light.read_state().and_then(|state| light.set_active(!state.is_active()));
        if let Err(error) = result {
            log::warn!("Night Light control unavailable: {error}");
            message("Could not change Night light");
        } else {
            message("Night light updated");
        }
        NIGHT_LIGHT_BUSY.store(false, Ordering::Release);
    });
    if started.is_err() { message("Could not change Night light"); NIGHT_LIGHT_BUSY.store(false, Ordering::Release); }
}

pub fn airplane() -> Option<bool> {
    match AIRPLANE.load(Ordering::Relaxed) { 1 => Some(false), 2 => Some(true), _ => None }
}

pub fn refresh_airplane() { airplane_worker(false); }
pub fn toggle_airplane() { message("Changing Airplane mode…"); airplane_worker(true); }

fn airplane_worker(toggle: bool) {
    if AIRPLANE_BUSY.swap(true, Ordering::AcqRel) {
        if toggle {
            AIRPLANE_PENDING.store(true, Ordering::Release);
            if !AIRPLANE_BUSY.load(Ordering::Acquire) && AIRPLANE_PENDING.swap(false, Ordering::AcqRel) { airplane_worker(true); }
        }
        return;
    }
    let started = std::thread::Builder::new().name("wdock-airplane".into()).spawn(move || {
        let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).is_ok() };
        let result = airplane_system(toggle);
        match result {
            Ok(on) => { AIRPLANE.store(if on { 2 } else { 1 }, Ordering::Relaxed); if toggle { message(if on { "Airplane mode on" } else { "Airplane mode off" }); } },
            Err(error) => {
                AIRPLANE.store(0, Ordering::Relaxed);
                log::debug!("System airplane mode unavailable: {error}");
                if toggle { message("Could not change Airplane mode"); }
            }
        }
        if initialized { unsafe { CoUninitialize() }; }
        AIRPLANE_BUSY.store(false, Ordering::Release);
        notify();
        if AIRPLANE_PENDING.swap(false, Ordering::AcqRel) { airplane_worker(true); }
    });
    if started.is_err() { if toggle { message("Could not change Airplane mode"); } AIRPLANE_BUSY.store(false, Ordering::Release); }
}

/// Windows has no public system-wide airplane setter. This interface is
/// undocumented, so restrict it to the Windows 11 builds supported by wdock
/// and fall back to Settings if the service or interface is unavailable.
fn airplane_system(toggle: bool) -> windows::core::Result<bool> {
    use std::ffi::c_void;
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_LOCAL_SERVER};
    use windows::core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface};
    const CLASS: GUID = GUID::from_u128(0x581333f6_28db_41be_bc7a_ff201f12f3f6);
    const IID: GUID = GUID::from_u128(0xdb3afbfb_08e6_46c6_aa70_bf9a34c30ab7);
    #[repr(C)]
    struct RadioManagerVtable {
        base: IUnknown_Vtbl,
        is_supported: usize,
        instances: usize,
        get_state: unsafe extern "system" fn(*mut c_void, *mut i32, *mut i32, *mut u32) -> HRESULT,
        set_state: unsafe extern "system" fn(*mut c_void, i32) -> HRESULT,
    }
    if !crate::platform::virtual_desktops::supported_windows() { return Err(windows::core::Error::from_hresult(HRESULT(0x80004001u32 as i32))); }
    unsafe {
        let object: IUnknown = CoCreateInstance(&CLASS, None, CLSCTX_LOCAL_SERVER)?;
        let mut raw = std::ptr::null_mut();
        object.query(&IID, &mut raw).ok()?;
        if raw.is_null() { return Err(windows::core::Error::from_hresult(HRESULT(0x80004003u32 as i32))); }
        let manager = IUnknown::from_raw(raw);
        let table = &**(manager.as_raw() as *const *const RadioManagerVtable);
        let mut enabled = 0i32; let mut other = 0i32; let mut reason = 0u32;
        (table.get_state)(manager.as_raw(), &mut enabled, &mut other, &mut reason).ok()?;
        if toggle {
            (table.set_state)(manager.as_raw(), if enabled == 0 { 1 } else { 0 }).ok()?;
            (table.get_state)(manager.as_raw(), &mut enabled, &mut other, &mut reason).ok()?;
        }
        Ok(enabled == 0)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "temporarily changes and restores Windows Energy Saver"]
    fn energy_saver_override_changes_and_restores() {
        let (original, stamp) = super::read_energy_override().expect("read Energy Saver override");
        let desired = if original == 1 { 2 } else { 1 };
        super::write_energy_override(desired, stamp).expect("set Energy Saver override");
        struct Restore(u32, u32);
        impl Drop for Restore {
            fn drop(&mut self) {
                if let Ok((current, stamp)) = super::read_energy_override() {
                    if current == self.1 { let _ = super::write_energy_override(self.0, stamp); }
                }
            }
        }
        let restore = Restore(original, desired);
        assert_eq!(super::read_energy_override().expect("read changed override").0, desired);
        drop(restore);
        assert_eq!(super::read_energy_override().expect("read restored override").0, original);
    }
    #[test]
    #[ignore = "reads the interactive system radio state"]
    fn read_airplane_without_changing_it() {
        let initialized = unsafe { super::CoInitializeEx(None, super::COINIT_MULTITHREADED).is_ok() };
        let state = super::airplane_system(false);
        if initialized { unsafe { super::CoUninitialize() }; }
        assert!(state.is_ok(), "radio manager: {state:?}");
    }

    #[test]
    #[ignore = "reads the interactive system Night Light state"]
    fn read_night_light_without_changing_it() {
        let light = win_nightlight_lib::Nightlight::new();
        let state = light.read_state();
        assert!(state.is_ok(), "night light: {state:?}");
    }
}
