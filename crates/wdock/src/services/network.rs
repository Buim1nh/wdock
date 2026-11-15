//! Connection state: the connected Wi-Fi network (via the WLAN API) or, when
//! there is none, whether some other link (Ethernet) has internet access.
//! Both APIs call back on system threads; callbacks only post a message.

use std::ffi::c_void;

use windows::Win32::Foundation::{ERROR_SUCCESS, HANDLE, HWND, LPARAM, WPARAM};
use windows::Win32::NetworkManagement::IpHelper::{
    CancelMibChangeNotify2, GetNetworkConnectivityHint, NotifyNetworkConnectivityHintChange,
};
use windows::Win32::NetworkManagement::WiFi::{
    L2_NOTIFICATION_DATA, WLAN_CONNECTION_ATTRIBUTES, WLAN_INTERFACE_INFO_LIST,
    WLAN_NOTIFICATION_SOURCE_ACM, WLAN_NOTIFICATION_SOURCE_MSM, WLAN_NOTIFICATION_SOURCE_NONE,
    WlanCloseHandle, WlanEnumInterfaces, WlanFreeMemory, WlanOpenHandle, WlanQueryInterface,
    WlanRegisterNotification, wlan_interface_state_connected, wlan_intf_opcode_current_connection,
};
use windows::Win32::Networking::WinSock::{
    NL_NETWORK_CONNECTIVITY_HINT, NL_NETWORK_CONNECTIVITY_LEVEL_HINT, NetworkConnectivityLevelHintConstrainedInternetAccess,
    NetworkConnectivityLevelHintInternetAccess, NetworkConnectivityLevelHintLocalAccess,
};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

use super::{KIND_NETWORK, NetState, Portal, WM_APP_STATUS};

/// Windows' own portal probe. A portal redirects it to its sign-in page. Only
/// the user's browser loads it; wdock itself makes no request.
pub const SIGN_IN_PAGE: &str = "http://www.msftconnecttest.com/redirect";

fn portal_for(level: NL_NETWORK_CONNECTIVITY_LEVEL_HINT, on_wifi: bool) -> Portal {
    if level == NetworkConnectivityLevelHintConstrainedInternetAccess {
        Portal::Detected
    } else if level == NetworkConnectivityLevelHintLocalAccess && on_wifi {
        // Windows does not always identify a portal: it can stay at local access.
        Portal::Possible
    } else {
        Portal::None
    }
}

fn post(context: *const c_void) {
    let _ = unsafe { PostMessageW(Some(HWND(context as *mut _)), WM_APP_STATUS, WPARAM(KIND_NETWORK), LPARAM(0)) };
}

unsafe extern "system" fn wlan_callback(_data: *mut L2_NOTIFICATION_DATA, context: *mut c_void) {
    post(context);
}

unsafe extern "system" fn hint_callback(context: *const c_void, _hint: NL_NETWORK_CONNECTIVITY_HINT) {
    post(context);
}

pub struct Network {
    wlan: Option<HANDLE>,
    hint: Option<HANDLE>,
}

impl Network {
    pub fn new(target: HWND) -> Self {
        let context = target.0 as *const c_void;
        let wlan = unsafe {
            let mut version = 0;
            let mut handle = HANDLE::default();
            (WlanOpenHandle(2, None, &mut version, &mut handle) == ERROR_SUCCESS.0).then(|| {
                let registered = WlanRegisterNotification(
                    handle,
                    WLAN_NOTIFICATION_SOURCE_ACM | WLAN_NOTIFICATION_SOURCE_MSM,
                    true,
                    Some(wlan_callback),
                    Some(context),
                    None,
                    None,
                );
                if registered != ERROR_SUCCESS.0 {
                    log::warn!("WlanRegisterNotification failed: {registered}");
                }
                handle
            })
        };
        if wlan.is_none() {
            log::info!("WLAN service unavailable; Wi-Fi status disabled");
        }
        let hint = unsafe {
            let mut handle = HANDLE::default();
            NotifyNetworkConnectivityHintChange(Some(hint_callback), Some(context), false, &mut handle)
                .is_ok()
                .then_some(handle)
        };
        Self { wlan, hint }
    }

    pub fn state(&self) -> NetState {
        if let Some((ssid, signal)) = self.wifi() {
            return NetState::Wifi { ssid, signal };
        }
        let mut hint = NL_NETWORK_CONNECTIVITY_HINT::default();
        let online = unsafe { GetNetworkConnectivityHint(&mut hint) }.is_ok()
            && (hint.ConnectivityLevel == NetworkConnectivityLevelHintInternetAccess
                || hint.ConnectivityLevel == NetworkConnectivityLevelHintConstrainedInternetAccess);
        if online { NetState::Wired } else { NetState::Offline }
    }

    pub fn portal(&self) -> Portal {
        let mut hint = NL_NETWORK_CONNECTIVITY_HINT::default();
        if unsafe { GetNetworkConnectivityHint(&mut hint) }.is_err() {
            return Portal::None;
        }
        portal_for(hint.ConnectivityLevel, self.wifi().is_some())
    }

    /// SSID and signal quality (0-100) of the first connected Wi-Fi interface.
    fn wifi(&self) -> Option<(String, u32)> {
        let handle = self.wlan?;
        unsafe {
            let mut list: *mut WLAN_INTERFACE_INFO_LIST = std::ptr::null_mut();
            if WlanEnumInterfaces(handle, None, &mut list) != ERROR_SUCCESS.0 || list.is_null() {
                return None;
            }
            let count = (*list).dwNumberOfItems as usize;
            let interfaces = std::slice::from_raw_parts((*list).InterfaceInfo.as_ptr(), count);
            let mut found = None;
            for interface in interfaces.iter().filter(|i| i.isState == wlan_interface_state_connected) {
                let mut size = 0;
                let mut data: *mut c_void = std::ptr::null_mut();
                let queried = WlanQueryInterface(
                    handle,
                    &interface.InterfaceGuid,
                    wlan_intf_opcode_current_connection,
                    None,
                    &mut size,
                    &mut data,
                    None,
                );
                if queried == ERROR_SUCCESS.0 && !data.is_null() {
                    let attributes = &(*(data as *const WLAN_CONNECTION_ATTRIBUTES)).wlanAssociationAttributes;
                    let ssid = &attributes.dot11Ssid;
                    let len = (ssid.uSSIDLength as usize).min(ssid.ucSSID.len());
                    found = Some((String::from_utf8_lossy(&ssid.ucSSID[..len]).into_owned(), attributes.wlanSignalQuality));
                    WlanFreeMemory(data);
                    break;
                }
            }
            WlanFreeMemory(list.cast());
            found
        }
    }
}

impl Drop for Network {
    fn drop(&mut self) {
        unsafe {
            if let Some(handle) = self.wlan {
                let _ = WlanRegisterNotification(handle, WLAN_NOTIFICATION_SOURCE_NONE, true, None, None, None, None);
                WlanCloseHandle(handle, None);
            }
            if let Some(hint) = self.hint {
                let _ = CancelMibChangeNotify2(hint);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::Networking::WinSock::{NetworkConnectivityLevelHintNone, NetworkConnectivityLevelHintUnknown};

    #[test]
    fn portal_follows_the_connectivity_level() {
        assert_eq!(portal_for(NetworkConnectivityLevelHintConstrainedInternetAccess, true), Portal::Detected);
        assert_eq!(portal_for(NetworkConnectivityLevelHintConstrainedInternetAccess, false), Portal::Detected);
        assert_eq!(portal_for(NetworkConnectivityLevelHintLocalAccess, true), Portal::Possible);
        assert_eq!(portal_for(NetworkConnectivityLevelHintLocalAccess, false), Portal::None);
        assert_eq!(portal_for(NetworkConnectivityLevelHintInternetAccess, true), Portal::None);
        assert_eq!(portal_for(NetworkConnectivityLevelHintNone, true), Portal::None);
        assert_eq!(portal_for(NetworkConnectivityLevelHintUnknown, true), Portal::None);
    }
}
