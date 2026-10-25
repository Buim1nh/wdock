//! Battery level and AC state. Windows sends WM_POWERBROADCAST to the window
//! registered here whenever either changes.

use windows::Win32::Foundation::{HANDLE, HWND};
use windows::Win32::System::Power::{
    GetSystemPowerStatus, HPOWERNOTIFY,
    RegisterPowerSettingNotification, SYSTEM_POWER_STATUS, UnregisterPowerSettingNotification,
};
use windows::Win32::System::SystemServices::{GUID_ACDC_POWER_SOURCE, GUID_BATTERY_PERCENTAGE_REMAINING};
use windows::Win32::UI::WindowsAndMessaging::DEVICE_NOTIFY_WINDOW_HANDLE;

use super::Battery;

const BATTERY_FLAG_CHARGING: u8 = 8;
const BATTERY_FLAG_NO_BATTERY: u8 = 128;

pub struct Power(Vec<HPOWERNOTIFY>);

impl Power {
    pub fn register(target: HWND) -> Self {
        let handles = [GUID_BATTERY_PERCENTAGE_REMAINING, GUID_ACDC_POWER_SOURCE]
            .iter()
            .filter_map(|guid| unsafe {
                RegisterPowerSettingNotification(HANDLE(target.0), guid, DEVICE_NOTIFY_WINDOW_HANDLE).ok()
            })
            .collect();
        Self(handles)
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
