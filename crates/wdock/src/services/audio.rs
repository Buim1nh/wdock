//! Default playback device volume through Core Audio. Changes made anywhere
//! (keyboard keys, other apps) arrive through COM callbacks on a system
//! thread; those only post a message to the UI thread.

use windows::Win32::Foundation::{HWND, LPARAM, PROPERTYKEY, WPARAM};
use windows::Win32::Media::Audio::Endpoints::{
    IAudioEndpointVolume, IAudioEndpointVolumeCallback, IAudioEndpointVolumeCallback_Impl,
};
use windows::Win32::Media::Audio::{
    AUDIO_VOLUME_NOTIFICATION_DATA, DEVICE_STATE, EDataFlow, ERole, IMMDeviceEnumerator,
    IMMNotificationClient, IMMNotificationClient_Impl, MMDeviceEnumerator, eConsole, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::core::{PCWSTR, Result, implement};

use super::{KIND_AUDIO, KIND_AUDIO_DEVICE, Volume, WM_APP_STATUS};

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
        Ok(())
    }
    fn OnDeviceAdded(&self, _id: &PCWSTR) -> Result<()> {
        Ok(())
    }
    fn OnDeviceRemoved(&self, _id: &PCWSTR) -> Result<()> {
        Ok(())
    }
    fn OnDefaultDeviceChanged(&self, flow: EDataFlow, role: ERole, _id: &PCWSTR) -> Result<()> {
        if flow == eRender && role == eConsole {
            post(self.0, KIND_AUDIO_DEVICE);
        }
        Ok(())
    }
    fn OnPropertyValueChanged(&self, _id: &PCWSTR, _key: &PROPERTYKEY) -> Result<()> {
        Ok(())
    }
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
