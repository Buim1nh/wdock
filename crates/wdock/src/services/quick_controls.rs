//! Bluetooth state is read on a worker so opening Quick Settings never blocks the UI.
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use windows::Devices::Radios::{Radio, RadioAccessStatus, RadioKind, RadioState};
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

// 0 = unknown/unavailable, 1 = off, 2 = on.
static BLUETOOTH: AtomicU8 = AtomicU8::new(0);
static BUSY: AtomicBool = AtomicBool::new(false);
static PENDING_TOGGLE: AtomicBool = AtomicBool::new(false);
static NIGHT_LIGHT_BUSY: AtomicBool = AtomicBool::new(false);
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
fn message(value: &str) { if let Ok(mut v) = QUICK_MESSAGE.lock() { *v = Some(value.into()); } }
pub fn report(value: &str) { message(value); }

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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BluetoothDevice {
    pub id: String,
    pub name: String,
    pub paired: bool,
    pub connected: Option<bool>,
    pub can_pair: bool,
}
static DEVICE_SCAN_BUSY: AtomicBool = AtomicBool::new(false);
static DEVICES: std::sync::Mutex<Option<Vec<BluetoothDevice>>> = std::sync::Mutex::new(None);
static DEVICE_ACTION: std::sync::Mutex<Option<(String, String)>> = std::sync::Mutex::new(None);
static DEVICE_ACTION_BUSY: AtomicBool = AtomicBool::new(false);

fn properties() -> windows_collections::IIterable<windows::core::HSTRING> {
    vec![windows::core::HSTRING::from("System.Devices.Aep.IsConnected")].into()
}

pub fn bluetooth_devices() -> Option<Vec<BluetoothDevice>> {
    DEVICES.lock().ok().and_then(|devices| devices.clone())
}
pub fn bluetooth_device_action() -> Option<(String, String)> {
    DEVICE_ACTION.lock().ok().and_then(|action| action.clone())
}
pub fn bluetooth_device_busy() -> bool { DEVICE_ACTION_BUSY.load(Ordering::Acquire) }
pub fn bluetooth_scan_busy() -> bool { DEVICE_SCAN_BUSY.load(Ordering::Acquire) }

fn classic_bluetooth_devices() -> Vec<BluetoothDevice> {
    use windows::Win32::Devices::Bluetooth::{BluetoothFindDeviceClose, BluetoothFindFirstDevice, BluetoothFindNextDevice, BLUETOOTH_DEVICE_INFO, BLUETOOTH_DEVICE_SEARCH_PARAMS};
    let params = BLUETOOTH_DEVICE_SEARCH_PARAMS {
        dwSize: std::mem::size_of::<BLUETOOTH_DEVICE_SEARCH_PARAMS>() as u32,
        fReturnAuthenticated: true.into(), fReturnRemembered: true.into(), fReturnConnected: true.into(),
        ..Default::default()
    };
    let mut info = BLUETOOTH_DEVICE_INFO { dwSize: std::mem::size_of::<BLUETOOTH_DEVICE_INFO>() as u32, ..Default::default() };
    let mut devices = Vec::new();
    if let Ok(find) = unsafe { BluetoothFindFirstDevice(&params, &mut info) } {
        loop {
            let len = info.szName.iter().position(|&c| c == 0).unwrap_or(info.szName.len());
            let name = String::from_utf16_lossy(&info.szName[..len]);
            if !name.is_empty() {
                devices.push(BluetoothDevice { id: format!("classic:{name}"), name, paired: true, connected: Some(info.fConnected.as_bool()), can_pair: false });
            }
            if unsafe { BluetoothFindNextDevice(find, &mut info) }.is_err() { break; }
        }
        let _ = unsafe { BluetoothFindDeviceClose(find) };
    }
    devices
}

/// Find paired and nearby classic / LE endpoints without blocking the UI thread.
pub fn refresh_bluetooth_devices() {
    if DEVICE_SCAN_BUSY.swap(true, Ordering::AcqRel) { return; }
    if std::thread::Builder::new().name("wdock-bluetooth-devices".into()).spawn(|| {
        use windows::Devices::Enumeration::{DeviceInformation, DeviceInformationKind};
        use windows::Foundation::IPropertyValue;
        use windows::core::{HSTRING, Interface};
        let initialized = unsafe { windows::Win32::System::WinRT::RoInitialize(windows::Win32::System::WinRT::RO_INIT_MULTITHREADED).is_ok() };
        let result = (|| -> windows::core::Result<()> {
            use windows::Devices::Enumeration::{DeviceInformationUpdate, DeviceWatcher};
            use windows::Foundation::TypedEventHandler;
            let filter = HSTRING::from(r#"(System.Devices.Aep.ProtocolId:="{e0cbf06c-cd8b-4647-bb8a-263b43f0f974}") OR (System.Devices.Aep.ProtocolId:="{bb7bb05e-5972-42b5-94fc-76eaa7084d49}")"#);
            let watcher = DeviceInformation::CreateWatcherWithKindAqsFilterAndAdditionalProperties(&filter, &properties(), DeviceInformationKind::AssociationEndpoint)?;
            let handler = TypedEventHandler::<DeviceWatcher, DeviceInformation>::new(|_, device| {
                let Some(device) = device.as_ref() else { return Ok(()); };
                let Ok(name) = device.Name() else { return Ok(()); };
                let name = name.to_string();
                if name.trim().is_empty() { return Ok(()); }
                let Ok(pairing) = device.Pairing() else { return Ok(()); };
                let paired = pairing.IsPaired().unwrap_or(false);
                let can_pair = pairing.CanPair().unwrap_or(false);
                if !paired && !can_pair { return Ok(()); }
                let connected = device.Properties().ok().and_then(|p| p.Lookup(&HSTRING::from("System.Devices.Aep.IsConnected")).ok()).and_then(|v| v.cast::<IPropertyValue>().ok()).and_then(|v| v.GetBoolean().ok());
                let Ok(id) = device.Id() else { return Ok(()); };
                if let Ok(mut cache) = DEVICES.lock() {
                    let devices = cache.get_or_insert_with(Vec::new);
                    let item = BluetoothDevice { id: id.to_string(), name, paired, connected, can_pair };
                    if let Some(existing) = devices.iter_mut().find(|d| d.id == item.id) { *existing = item; } else { devices.push(item); }
                    devices.sort_by(|a, b| b.connected.cmp(&a.connected).then_with(|| b.paired.cmp(&a.paired)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
                }
                Ok(())
            });
            let token = watcher.Added(&handler)?;
            let updated = TypedEventHandler::<DeviceWatcher, DeviceInformationUpdate>::new(|_, update| {
                let Some(update) = update.as_ref() else { return Ok(()); };
                let Ok(id) = update.Id() else { return Ok(()); };
                let connected = update.Properties().ok().and_then(|p| p.Lookup(&HSTRING::from("System.Devices.Aep.IsConnected")).ok()).and_then(|v| v.cast::<IPropertyValue>().ok()).and_then(|v| v.GetBoolean().ok());
                if let (Some(connected), Ok(mut cache)) = (connected, DEVICES.lock()) {
                    if let Some(device) = cache.as_mut().and_then(|devices| devices.iter_mut().find(|device| device.id == id.to_string())) {
                        device.connected = Some(connected);
                    }
                }
                Ok(())
            });
            let update_token = watcher.Updated(&updated)?;
            if let Ok(mut cache) = DEVICES.lock() { *cache = Some(Vec::new()); }
            watcher.Start()?;
            if let Ok(mut action) = DEVICE_ACTION.lock() {
                if action.as_ref().is_some_and(|(id, _)| id.is_empty()) { *action = None; }
            }
            // Keep discovery active long enough to receive both cached and nearby devices.
            std::thread::sleep(std::time::Duration::from_secs(15));
            let _ = watcher.Stop();
            let _ = watcher.RemoveAdded(token);
            let _ = watcher.RemoveUpdated(update_token);
            Ok(())
        })();
        if let Err(error) = result {
            log::warn!("Bluetooth discovery failed: {error}");
            if let Ok(mut cache) = DEVICES.lock() { *cache = Some(classic_bluetooth_devices()); }
            if let Ok(mut action) = DEVICE_ACTION.lock() { *action = Some((String::new(), format!("Bluetooth scan failed: {error}"))); }
        }
        if initialized { unsafe { windows::Win32::System::WinRT::RoUninitialize() }; }
        DEVICE_SCAN_BUSY.store(false, Ordering::Release);
    }).is_err() { DEVICE_SCAN_BUSY.store(false, Ordering::Release); }
}

/// Pairing may show a Windows confirmation/PIN dialog; never silently accept it.
pub fn pair_bluetooth_device(id: String) {
    if DEVICE_ACTION_BUSY.swap(true, Ordering::AcqRel) { return; }
    if let Ok(mut state) = DEVICE_ACTION.lock() { *state = Some((id.clone(), "Pairing...".into())); }
    let thread_id = id.clone();
    if std::thread::Builder::new().name("wdock-bluetooth-pair".into()).spawn(move || {
        use windows::Devices::Enumeration::{DeviceInformation, DeviceInformationKind, DevicePairingResultStatus};
        let initialized = unsafe { windows::Win32::System::WinRT::RoInitialize(windows::Win32::System::WinRT::RO_INIT_MULTITHREADED).is_ok() };
        let result = (|| -> windows::core::Result<_> {
            let device = DeviceInformation::CreateFromIdAsyncWithKindAndAdditionalProperties(&windows::core::HSTRING::from(thread_id.as_str()), &properties(), DeviceInformationKind::AssociationEndpoint)?.join()?;
            device.Pairing()?.PairAsync()?.join()?.Status()
        })();
        let message = match result {
            Ok(DevicePairingResultStatus::Paired) | Ok(DevicePairingResultStatus::AlreadyPaired) => "Paired",
            Ok(DevicePairingResultStatus::AuthenticationTimeout) => "Pairing timed out",
            Ok(DevicePairingResultStatus::AuthenticationFailure) | Ok(DevicePairingResultStatus::AuthenticationNotAllowed) => "Authentication failed",
            Ok(DevicePairingResultStatus::PairingCanceled) | Ok(DevicePairingResultStatus::RejectedByHandler) => "Pairing rejected",
            _ => "Pairing failed",
        };
        if let Ok(mut state) = DEVICE_ACTION.lock() { *state = Some((thread_id, message.into())); }
        if initialized { unsafe { windows::Win32::System::WinRT::RoUninitialize() }; }
        DEVICE_ACTION_BUSY.store(false, Ordering::Release);
        refresh_bluetooth_devices();
    }).is_err() {
        if let Ok(mut state) = DEVICE_ACTION.lock() { *state = Some((id, "Pairing failed".into())); }
        DEVICE_ACTION_BUSY.store(false, Ordering::Release);
    }
}

pub fn bluetooth() -> Option<bool> {
    match BLUETOOTH.load(Ordering::Relaxed) { 1 => Some(false), 2 => Some(true), _ => None }
}

pub fn refresh_bluetooth() { worker(false); }
pub fn toggle_bluetooth() { message("Changing Bluetooth…"); worker(true); }

fn worker(toggle: bool) {
    if BUSY.swap(true, Ordering::AcqRel) {
        if toggle {
            PENDING_TOGGLE.store(true, Ordering::Release);
            if !BUSY.load(Ordering::Acquire) && PENDING_TOGGLE.swap(false, Ordering::AcqRel) { worker(true); }
        }
        return;
    }
    let result = std::thread::Builder::new().name("wdock-bluetooth".into()).spawn(move || {
        let initialized = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).is_ok() };
        let result = (|| -> windows::core::Result<Option<bool>> {
            let radios = Radio::GetRadiosAsync()?.join()?;
            let mut bluetooth = Vec::new();
            for radio in radios { if radio.Kind()? == RadioKind::Bluetooth { bluetooth.push(radio); } }
            if bluetooth.is_empty() { return Ok(None); }
            let on = bluetooth.iter().any(|r| r.State().ok() == Some(RadioState::On));
            if toggle {
                if Radio::RequestAccessAsync()?.join()? != RadioAccessStatus::Allowed { return Err(windows::core::Error::from_thread()); }
                for radio in &bluetooth {
                    let state = if on { RadioState::Off } else { RadioState::On };
                    if radio.SetStateAsync(state)?.join()? != RadioAccessStatus::Allowed { return Err(windows::core::Error::from_thread()); }
                }
            }
            Ok(Some(bluetooth.iter().any(|r| r.State().ok() == Some(RadioState::On))))
        })();
        match result {
            Ok(state) => { BLUETOOTH.store(match state { Some(true) => 2, Some(false) => 1, None => 0 }, Ordering::Relaxed); }
            Err(error) => {
                BLUETOOTH.store(0, Ordering::Relaxed);
                log::debug!("Bluetooth control unavailable: {error}");
                if toggle { message("Could not change Bluetooth"); }
            }
        }
        if toggle {
            match bluetooth() {
                Some(true) => message("Bluetooth on"),
                Some(false) => message("Bluetooth off"),
                None => message("Bluetooth control unavailable"),
            }
        }
        if initialized { unsafe { CoUninitialize() }; }
        BUSY.store(false, Ordering::Release);
        if PENDING_TOGGLE.swap(false, Ordering::AcqRel) { worker(true); }
    });
    if result.is_err() { BUSY.store(false, Ordering::Release); }
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
    #[ignore = "reads nearby Bluetooth device information"]
    fn enumerate_bluetooth_endpoints() {
        super::refresh_bluetooth_devices();
        for _ in 0..400 {
            if !super::DEVICE_SCAN_BUSY.load(std::sync::atomic::Ordering::Acquire) { break; }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(super::bluetooth_devices().is_some(), "Bluetooth enumeration did not complete successfully: {:?}", super::bluetooth_device_action());
        assert!(!super::bluetooth_device_action().is_some_and(|(id, _)| id.is_empty()), "Bluetooth endpoint discovery fell back to classic devices");
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
