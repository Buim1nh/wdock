//! Wi-Fi list and control for quick settings. A `Session` exists only while the
//! list is open: it holds its own WLAN handle and notification registration
//! (which post `WM_APP_MODEL` to the popup) and releases both when dropped.
//! Nothing here polls; a scan is requested once when the session opens.

use std::ffi::c_void;

use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE, HWND};
use windows::Win32::NetworkManagement::WiFi::{
    L2_NOTIFICATION_DATA, WLAN_AVAILABLE_NETWORK_CONNECTED, WLAN_AVAILABLE_NETWORK_HAS_PROFILE,
    WLAN_AVAILABLE_NETWORK_LIST, WLAN_CONNECTION_PARAMETERS, WLAN_INTERFACE_INFO_LIST, WLAN_NOTIFICATION_SOURCE_ACM,
    WLAN_NOTIFICATION_SOURCE_MSM, WLAN_NOTIFICATION_SOURCE_NONE, WLAN_PHY_RADIO_STATE, WLAN_RADIO_STATE, WlanCloseHandle, WlanConnect, WlanDisconnect, WlanEnumInterfaces,
    WlanFreeMemory, WlanGetAvailableNetworkList, WlanOpenHandle, WlanQueryInterface, WlanRegisterNotification, WlanScan,
    WlanSetInterface, dot11_BSS_type_infrastructure, dot11_radio_state_off, dot11_radio_state_on,
    dot11_radio_state_unknown, wlan_connection_mode_profile, wlan_intf_opcode_radio_state,
};
use windows::core::{GUID, PCWSTR};

use crate::ui::bar;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccessPoint {
    pub ssid: String,
    /// 0 ..= 100
    pub signal: u32,
    pub secured: bool,
    pub connected: bool,
    /// Saved profile to connect with; `None` for a network never joined.
    pub profile: Option<String>,
}

/// Signal quality as 0-4 bars.
pub fn bars(signal: u32) -> u8 {
    signal.min(100).div_ceil(25) as u8
}

/// Lossy UTF-8 text of a fixed SSID buffer.
fn ssid_text(bytes: &[u8], len: u32) -> String {
    String::from_utf8_lossy(&bytes[..(len as usize).min(bytes.len())]).into_owned()
}

/// UTF-16 buffer up to its first NUL.
fn wide_text(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// One entry per SSID (the API lists an entry per profile/BSS type), the
/// connected network first and the rest by signal strength.
pub fn tidy(list: Vec<AccessPoint>) -> Vec<AccessPoint> {
    let mut merged: Vec<AccessPoint> = Vec::new();
    for ap in list.into_iter().filter(|ap| !ap.ssid.trim().is_empty()) {
        match merged.iter_mut().find(|m| m.ssid == ap.ssid) {
            Some(existing) => {
                let better = (ap.connected, ap.signal) > (existing.connected, existing.signal);
                let profile = existing.profile.clone().or_else(|| ap.profile.clone());
                if better {
                    *existing = ap;
                }
                existing.profile = existing.profile.take().or(profile);
            }
            None => merged.push(ap),
        }
    }
    merged.sort_by(|a, b| {
        b.connected.cmp(&a.connected).then(b.signal.cmp(&a.signal)).then_with(|| a.ssid.to_lowercase().cmp(&b.ssid.to_lowercase()))
    });
    merged
}

unsafe extern "system" fn callback(_data: *mut L2_NOTIFICATION_DATA, context: *mut c_void) {
    bar::notify_model_changed(HWND(context));
}

pub struct Session {
    handle: HANDLE,
    guid: GUID,
}

impl Session {
    /// Opens the first WLAN interface and scans once. `None` without Wi-Fi
    /// hardware or the WLAN service (wired-only machines).
    pub fn open(notify: HWND) -> Option<Self> {
        unsafe {
            let mut version = 0;
            let mut handle = HANDLE::default();
            if WlanOpenHandle(2, None, &mut version, &mut handle) != ERROR_SUCCESS.0 {
                return None;
            }
            let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
            let guid = (WlanEnumInterfaces(handle, None, &mut list) == ERROR_SUCCESS.0 && !list.is_null())
                .then(|| std::slice::from_raw_parts((*list).InterfaceInfo.as_ptr(), (*list).dwNumberOfItems as usize).first().map(|i| i.InterfaceGuid))
                .flatten();
            if !list.is_null() {
                WlanFreeMemory(list.cast());
            }
            let Some(guid) = guid else {
                WlanCloseHandle(handle, None);
                return None;
            };
            let registered =
                WlanRegisterNotification(handle, WLAN_NOTIFICATION_SOURCE_ACM | WLAN_NOTIFICATION_SOURCE_MSM, true, Some(callback), Some(notify.0 as *const c_void), None, None);
            if registered != ERROR_SUCCESS.0 {
                log::warn!("wifi list: WlanRegisterNotification failed: {registered}");
            }
            let session = Self { handle, guid };
            session.scan();
            Some(session)
        }
    }

    /// Asks the driver for fresh results; completion arrives as a notification.
    pub fn scan(&self) {
        let result = unsafe { WlanScan(self.handle, &self.guid, None, None, None) };
        if result != ERROR_SUCCESS.0 {
            log::debug!("WlanScan: {result}");
        }
    }

    pub fn networks(&self) -> Vec<AccessPoint> {
        unsafe {
            let mut list: *mut WLAN_AVAILABLE_NETWORK_LIST = std::ptr::null_mut();
            if WlanGetAvailableNetworkList(self.handle, &self.guid, 0, None, &mut list) != ERROR_SUCCESS.0 || list.is_null() {
                return Vec::new();
            }
            let items = std::slice::from_raw_parts((*list).Network.as_ptr(), (*list).dwNumberOfItems as usize);
            let found = items
                .iter()
                .filter(|n| n.dot11BssType == dot11_BSS_type_infrastructure)
                .map(|n| AccessPoint {
                    ssid: ssid_text(&n.dot11Ssid.ucSSID, n.dot11Ssid.uSSIDLength),
                    signal: n.wlanSignalQuality,
                    secured: n.bSecurityEnabled.as_bool(),
                    connected: n.dwFlags & WLAN_AVAILABLE_NETWORK_CONNECTED != 0,
                    profile: (n.dwFlags & WLAN_AVAILABLE_NETWORK_HAS_PROFILE != 0).then(|| wide_text(&n.strProfileName)),
                })
                .collect();
            WlanFreeMemory(list.cast());
            tidy(found)
        }
    }

    /// Whether the software and hardware radio are both on; `None` if unknown.
    pub fn radio_on(&self) -> Option<bool> {
        unsafe {
            let mut size = 0;
            let mut data: *mut c_void = std::ptr::null_mut();
            let queried = WlanQueryInterface(self.handle, &self.guid, wlan_intf_opcode_radio_state, None, &mut size, &mut data, None);
            if queried != ERROR_SUCCESS.0 || data.is_null() {
                return None;
            }
            let state = &*(data as *const WLAN_RADIO_STATE);
            let count = (state.dwNumberOfPhys as usize).min(state.PhyRadioState.len());
            let on = state.PhyRadioState[..count]
                .iter()
                .any(|p| p.dot11SoftwareRadioState == dot11_radio_state_on && p.dot11HardwareRadioState == dot11_radio_state_on);
            WlanFreeMemory(data);
            Some(on)
        }
    }

    /// Switches the software radio. Fails (false) without the needed rights.
    pub fn set_radio(&self, on: bool) -> bool {
        let state = WLAN_PHY_RADIO_STATE {
            dwPhyIndex: 0,
            dot11SoftwareRadioState: if on { dot11_radio_state_on } else { dot11_radio_state_off },
            dot11HardwareRadioState: dot11_radio_state_unknown,
        };
        let result = unsafe {
            WlanSetInterface(
                self.handle,
                &self.guid,
                wlan_intf_opcode_radio_state,
                size_of::<WLAN_PHY_RADIO_STATE>() as u32,
                std::ptr::from_ref(&state).cast(),
                None,
            )
        };
        if result != ERROR_SUCCESS.0 {
            log::info!("WlanSetInterface(radio): {result}");
        }
        result == ERROR_SUCCESS.0
    }

    /// Connects with a saved profile.
    pub fn connect(&self, profile: &str) -> bool {
        let name: Vec<u16> = profile.encode_utf16().chain([0]).collect();
        let params = WLAN_CONNECTION_PARAMETERS {
            wlanConnectionMode: wlan_connection_mode_profile,
            strProfile: PCWSTR(name.as_ptr()),
            pDot11Ssid: std::ptr::null_mut(),
            pDesiredBssidList: std::ptr::null_mut(),
            dot11BssType: dot11_BSS_type_infrastructure,
            dwFlags: 0,
        };
        let result = unsafe { WlanConnect(self.handle, &self.guid, &params, None) };
        if result != ERROR_SUCCESS.0 {
            log::warn!("WlanConnect: {result}");
        }
        result == ERROR_SUCCESS.0
    }

    pub fn disconnect(&self) {
        let result = unsafe { WlanDisconnect(self.handle, &self.guid, None) };
        if result != ERROR_SUCCESS.0 {
            log::warn!("WlanDisconnect: {result}");
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        unsafe {
            // Unregistering waits for a running callback, so none outlives this.
            let _ = WlanRegisterNotification(self.handle, WLAN_NOTIFICATION_SOURCE_NONE, true, None, None, None, None);
            WlanCloseHandle(self.handle, None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ap(ssid: &str, signal: u32, connected: bool, profile: Option<&str>) -> AccessPoint {
        AccessPoint { ssid: ssid.into(), signal, secured: true, connected, profile: profile.map(Into::into) }
    }

    #[test]
    fn bars_round_up_in_quarters() {
        assert_eq!([0, 1, 25, 26, 50, 51, 75, 76, 100, 200].map(bars), [0, 1, 1, 2, 2, 3, 3, 4, 4, 4]);
    }

    #[test]
    fn ssid_text_respects_length_and_lossy_utf8() {
        let mut buffer = [0u8; 32];
        buffer[..4].copy_from_slice(b"Home");
        assert_eq!(ssid_text(&buffer, 4), "Home");
        assert_eq!(ssid_text(&buffer, 99).len(), 32);
        assert_eq!(ssid_text(&[0xFF, b'a'], 2), "\u{FFFD}a");
    }

    #[test]
    fn wide_text_stops_at_nul() {
        let buffer: Vec<u16> = "Cafe\0junk".encode_utf16().collect();
        assert_eq!(wide_text(&buffer), "Cafe");
    }

    #[test]
    fn tidy_sorts_connected_first_then_signal() {
        let sorted = tidy(vec![ap("b", 40, false, None), ap("a", 90, false, None), ap("c", 10, true, Some("c")), ap("B", 40, false, None)]);
        let names: Vec<_> = sorted.iter().map(|a| a.ssid.as_str()).collect();
        assert_eq!(names, ["c", "a", "b", "B"]);
    }

    #[test]
    fn tidy_merges_duplicates_and_keeps_the_saved_profile() {
        let sorted = tidy(vec![ap("Net", 80, false, None), ap("Net", 50, false, Some("Net 2")), ap("", 99, false, None), ap("  ", 99, false, None)]);
        assert_eq!(sorted.len(), 1);
        assert_eq!(sorted[0].signal, 80);
        assert_eq!(sorted[0].profile.as_deref(), Some("Net 2"));
    }
}
