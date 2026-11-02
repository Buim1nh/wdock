//! Battery level and AC state. Windows sends WM_POWERBROADCAST to the window
//! registered here whenever either changes.

use std::sync::atomic::{AtomicU8, Ordering};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM};
use windows::Win32::System::Power::{
    GetSystemPowerStatus, HPOWERNOTIFY, POWERBROADCAST_SETTING,
    RegisterPowerSettingNotification, SYSTEM_POWER_STATUS, UnregisterPowerSettingNotification,
};
use windows::Win32::System::SystemServices::{GUID_ACDC_POWER_SOURCE, GUID_BATTERY_PERCENTAGE_REMAINING};
use windows::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_WINDOW_HANDLE;

use super::Battery;

const BATTERY_FLAG_CHARGING: u8 = 8;
const BATTERY_FLAG_NO_BATTERY: u8 = 128;
// Windows 11 24H2 reports Energy Saver separately from legacy Battery Saver.
const GUID_ENERGY_SAVER_STATUS: windows::core::GUID = windows::core::GUID::from_u128(0x550e8400_e29b_41d4_a716_446655440000);
// 0 = no notification yet, 1 = off, 2 = standard/high savings.
static ENERGY_SAVER: AtomicU8 = AtomicU8::new(0);

fn energy_saver_value(value: u32) -> Option<bool> {
    match value { 0 => Some(false), 1 | 2 => Some(true), _ => None }
}

pub fn energy_saver() -> Option<bool> {
    match ENERGY_SAVER.load(Ordering::Relaxed) { 1 => Some(false), 2 => Some(true), _ => None }
}

/// `lparam` is supplied by Windows for PBT_POWERSETTINGCHANGE.
pub fn power_setting_changed(lparam: LPARAM) -> bool {
    let Some(setting) = (unsafe { (lparam.0 as *const POWERBROADCAST_SETTING).as_ref() }) else { return false; };
    if setting.PowerSetting != GUID_ENERGY_SAVER_STATUS || setting.DataLength < 4 { return false; }
    let value = unsafe { std::ptr::read_unaligned(setting.Data.as_ptr().cast::<u32>()) };
    let Some(on) = energy_saver_value(value) else { return false; };
    ENERGY_SAVER.swap(if on { 2 } else { 1 }, Ordering::Relaxed) != if on { 2 } else { 1 }
}

pub struct Power(Vec<HPOWERNOTIFY>);

impl Power {
    pub fn register(target: HWND) -> Self {
        let handles = [GUID_BATTERY_PERCENTAGE_REMAINING, GUID_ACDC_POWER_SOURCE, GUID_ENERGY_SAVER_STATUS]
            .iter()
            .filter_map(|guid| unsafe {
                RegisterPowerSettingNotification(HANDLE(target.0), guid, DEVICE_NOTIFY_WINDOW_HANDLE).ok()
            })
            .collect();
        Self(handles)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn energy_saver_notification_values_are_distinct_from_legacy_battery_saver() {
        assert_eq!(super::energy_saver_value(0), Some(false));
        assert_eq!(super::energy_saver_value(1), Some(true));
        assert_eq!(super::energy_saver_value(2), Some(true));
        assert_eq!(super::energy_saver_value(3), None);
    }
}

impl Drop for Power {
    fn drop(&mut self) {
        for handle in self.0.drain(..) {
            let _ = unsafe { UnregisterPowerSettingNotification(handle) };
        }
    }
}

/// `None` on desktops without a battery.
pub fn battery() -> Option<Battery> {
    let mut status = SYSTEM_POWER_STATUS::default();
    unsafe { GetSystemPowerStatus(&mut status).ok()? };
    if status.BatteryFlag & BATTERY_FLAG_NO_BATTERY != 0 || status.BatteryLifePercent > 100 {
        return None;
    }
    Some(Battery {
        percent: status.BatteryLifePercent,
        plugged: status.ACLineStatus == 1,
        charging: status.BatteryFlag & BATTERY_FLAG_CHARGING != 0,
    })
}
