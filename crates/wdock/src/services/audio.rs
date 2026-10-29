//! Default playback device volume through Core Audio. Changes made anywhere
//! (keyboard keys, other apps) arrive through COM callbacks on a system
//! thread; those only post a message to the UI thread.

use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{HWND, LPARAM, PROPERTYKEY, WPARAM};
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::{
    AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE, DEVICE_STATE_ACTIVE, EDataFlow, ERole, IMMDevice,
    IMMDeviceEnumerator, IMMNotificationClient, IMMNotificationClient_Impl, MMDeviceEnumerator, eCommunications,
    eConsole, eMultimedia, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, STGM_READ};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::core::{BSTR, GUID, HRESULT, IUnknown, IUnknown_Vtbl, PCWSTR, Result, implement};

use super::{KIND_AUDIO, KIND_AUDIO_DEVICE, KIND_AUDIO_LIST, Volume, WM_APP_STATUS};

fn post(target: isize, kind: usize) {
    let _ = unsafe { PostMessageW(Some(HWND(target as *mut _)), WM_APP_STATUS, WPARAM(kind), LPARAM(0)) };
}

#[implement(IAudioEndpointVolumeCallback)]
struct VolumeCallback(isize);

impl IAudioEndpointVolumeCallback_Impl for VolumeCallback_Impl {
    fn OnNotify(&self, _data: *mut AUDIO_VOLUME_NOTIFICATION_DATA) -> Result<()> {
        post(self.0, KIND_AUDIO);
        Ok(())
    }
}

#[implement(IMMNotificationClient)]
struct DeviceCallback(isize);

impl IMMNotificationClient_Impl for DeviceCallback_Impl {
    fn OnDeviceStateChanged(&self, _id: &PCWSTR, _state: DEVICE_STATE) -> Result<()> {
        post(self.0, KIND_AUDIO_LIST);
        Ok(())
    }
    fn OnDeviceAdded(&self, _id: &PCWSTR) -> Result<()> {
        post(self.0, KIND_AUDIO_LIST);
        Ok(())
    }
    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> Result<()> {
        post(self.0, KIND_AUDIO_LIST);
        Ok(())
    }
    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _id: &PCWSTR) -> Result<()> {
        if flow == eRender && role == eConsole {
            post(self.0, KIND_AUDIO_DEVICE);
            post(self.0, KIND_AUDIO_LIST);
        }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _id: &PCWSTR, _key: &PROPERTYKEY) -> Result<()> {
        Ok(())
    }
}

/// The undocumented policy-config interface every open-source output switcher
/// uses. Only `set_default_endpoint` is called; the other slots keep the
/// vtable layout.
#[windows::core::interface("f8679f50-850a-41cf-9c72-430f290290c8")]
unsafe trait IPolicyConfig: IUnknown {
    unsafe fn get_mix_format(&self) -> HRESULT;
    unsafe fn get_device_format(&self) -> HRESULT;
    unsafe fn reset_device_format(&self) -> HRESULT;
    unsafe fn set_device_format(&self) -> HRESULT;
    unsafe fn get_processing_period(&self) -> HRESULT;
    unsafe fn set_processing_period(&self) -> HRESULT;
    unsafe fn get_share_mode(&self) -> HRESULT;
    unsafe fn set_share_mode(&self) -> HRESULT;
    unsafe fn get_property_value(&self) -> HRESULT;
    unsafe fn set_property_value(&self) -> HRESULT;
    unsafe fn set_default_endpoint(&self, id: PCWSTR, role: ERole) -> HRESULT;
    unsafe fn set_endpoint_visibility(&self) -> HRESULT;
}

const CLSID_POLICY_CONFIG_CLIENT: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
const PKEY_FORM_FACTOR: PROPERTYKEY = PROPERTYKEY { fmtid: GUID::from_u128(0x1da5d803_d492_4edd_8c23_e0c0ffee7f0e), pid: 0 };

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputKind {
    Speakers,
    Headphones,
    Bluetooth,
    Display,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Output {
    pub id: String,
    pub name: String,
    pub kind: OutputKind,
    pub default: bool,
}

fn device_id(device: &IMMDevice) -> Option<String> {
    unsafe { device.GetId().ok().and_then(|id| id.to_string().ok()) }
}

fn describe(device: &IMMDevice, default_id: Option<&str>) -> Option<Output> {
    let id = device_id(device)?;
    let store = unsafe { device.OpenPropertyStore(STGM_READ).ok()? };
    let name = unsafe { store.GetValue(&PKEY_Device_FriendlyName).ok() }
        .and_then(|v| BSTR::try_from(&v).ok())
        .map_or_else(|| "Unknown device".to_owned(), |b| b.to_string());
    // EndpointFormFactor: 1 speakers, 3 headphones, 4 microphone-less handset, 5 headset, 9 S/PDIF, 10 HDMI.
    let form = unsafe { store.GetValue(&PKEY_FORM_FACTOR).ok() }.and_then(|v| u32::try_from(&v).ok());
    // Endpoints carry no bus info, so Bluetooth is a best-effort guess from the name.
    let lower = name.to_lowercase();
    let kind = if lower.contains("bluetooth") || lower.contains("hands-free") {
        OutputKind::Bluetooth
    } else {
        match form {
            Some(3..=5) => OutputKind::Headphones,
            Some(1 | 2) => OutputKind::Speakers,
            Some(9 | 10) => OutputKind::Display,
            _ => OutputKind::Other,
        }
    };
    Some(Output { default: default_id == Some(id.as_str()), id, name, kind })
}

pub struct Audio {
    target: isize,
    enumerator: IMMDeviceEnumerator,
    device_callback: IMMNotificationClient,
    endpoint: Option<(IAudioEndpointVolume, IAudioEndpointVolumeCallback)>,
}

impl Audio {
    pub fn new(target: HWND) -> Result<Self> {
        let target = target.0 as isize;
        let enumerator: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? };
        let device_callback: IMMNotificationClient = DeviceCallback(target).into();
        unsafe { enumerator.RegisterEndpointNotificationCallback(&device_callback)? };
        let mut audio = Self { target, enumerator, device_callback, endpoint: None };
        audio.attach_default();
        Ok(audio)
    }

    /// (Re)binds to the current default output device, e.g. after headphones are plugged in.
    pub fn attach_default(&mut self) {
        self.detach();
        let attached = unsafe {
            self.enumerator.GetDefaultAudioEndpoint(eRender, eConsole).and_then(|device| {
                let endpoint: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None)?;
                let callback: IAudioEndpointVolumeCallback = VolumeCallback(self.target).into();
                endpoint.RegisterControlChangeNotify(&callback)?;
                Ok((endpoint, callback))
            })
        };
        match attached {
            Ok(endpoint) => self.endpoint = Some(endpoint),
            Err(err) => log::info!("no default audio output: {err}"),
        }
    }

    /// Active render endpoints, default first.
    pub fn outputs(&self) -> Vec<Output> {
        let default_id = unsafe { self.enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok() }.and_then(|d| device_id(&d));
        let mut list = Vec::new();
        unsafe {
            if let Ok(devices) = self.enumerator.EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
                && let Ok(count) = devices.GetCount()
            {
                for i in 0..count {
                    if let Some(output) = devices.Item(i).ok().and_then(|d| describe(&d, default_id.as_deref())) {
                        list.push(output);
                    }
                }
            }
        }
        list.sort_by_key(|o| !o.default);
        list
    }

    /// Makes `id` the default output for every role. Undocumented API; may be gone in a future Windows build.
    pub fn set_default_output(&self, id: &str) -> Result<()> {
        let policy: IPolicyConfig = unsafe { CoCreateInstance(&CLSID_POLICY_CONFIG_CLIENT, None, CLSCTX_ALL)? };
        let wide: Vec<u16> = id.encode_utf16().chain(std::iter::once(0)).collect();
        for role in [eConsole, eMultimedia, eCommunications] {
            unsafe { policy.set_default_endpoint(PCWSTR(wide.as_ptr()), role).ok()? };
        }
        Ok(())
    }

    fn detach(&mut self) {
        if let Some((endpoint, callback)) = self.endpoint.take() {
            let _ = unsafe { endpoint.UnregisterControlChangeNotify(&callback) };
        }
    }

    pub fn state(&self) -> Option<Volume> {
        let (endpoint, _) = self.endpoint.as_ref()?;
        unsafe {
            Some(Volume {
                level: endpoint.GetMasterVolumeLevelScalar().ok()?,
                muted: endpoint.GetMute().ok()?.as_bool(),
            })
        }
    }

    pub fn set_level(&self, level: f32) {
        if let Some((endpoint, _)) = &self.endpoint {
            unsafe {
                let _ = endpoint.SetMasterVolumeLevelScalar(level.clamp(0.0, 1.0), std::ptr::null());
                if level > 0.0 {
                    let _ = endpoint.SetMute(false, std::ptr::null());
                }
            }
        }
    }

    pub fn set_muted(&self, muted: bool) {
        if let Some((endpoint, _)) = &self.endpoint {
            let _ = unsafe { endpoint.SetMute(muted, std::ptr::null()) };
        }
    }
}

impl Drop for Audio {
    fn drop(&mut self) {
        self.detach();
        let _ = unsafe { self.enumerator.UnregisterEndpointNotificationCallback(&self.device_callback) };
    }
}

#[cfg(test)]
mod tests {
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx};

    use super::*;

    /// Enumeration only; never changes the default device. Skipped where no output exists.
    #[test]
    fn lists_outputs_with_at_most_one_default() {
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let Ok(audio) = Audio::new(HWND::default()) else {
            return;
        };
        let outputs = audio.outputs();
        for o in &outputs {
            eprintln!("{:?}", o);
        }
        assert!(outputs.iter().filter(|o| o.default).count() <= 1);
        assert!(outputs.iter().all(|o| !o.name.is_empty() && !o.id.is_empty()));
    }
}
