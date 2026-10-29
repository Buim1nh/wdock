//! System status for the top bar and quick settings: volume, network,
//! battery, brightness. Everything is change-driven: services post
//! `WM_APP_STATUS` (or Windows sends WM_POWERBROADCAST) and the main window
//! refreshes the snapshot here; views read `status()` when they repaint.

pub mod apps_index;
pub mod audio;
pub mod brightness;
pub mod icons;
pub mod media;
pub mod network;
pub mod power;
pub mod wifi;

use std::cell::RefCell;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::UI::WindowsAndMessaging::WM_APP;

pub const WM_APP_STATUS: u32 = WM_APP + 20;
pub const KIND_AUDIO: usize = 1;
pub const KIND_AUDIO_DEVICE: usize = 2;
pub const KIND_NETWORK: usize = 3;
pub const KIND_BRIGHTNESS: usize = 4;
pub const KIND_AUDIO_LIST: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Volume {
    /// 0.0 ..= 1.0
    pub level: f32,
    pub muted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum NetState {
    #[default]
    Offline,
    Wired,
    Wifi {
        ssid: String,
        /// 0 ..= 100
        signal: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Battery {
    pub percent: u8,
    pub plugged: bool,
    pub charging: bool,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SystemStatus {
    pub volume: Option<Volume>,
    pub network: NetState,
    pub battery: Option<Battery>,
    /// `None` until known, or when the display has no software control.
    pub brightness: Option<u8>,
    /// Counts default-output-device changes (headphones plugged in, ...).
    pub audio_device: u32,
    /// Counts changes to the set of output devices (added, removed, default moved).
    pub audio_devices: u32,
}

struct Services {
    audio: Option<audio::Audio>,
    network: network::Network,
    _power: power::Power,
    brightness: brightness::Brightness,
    status: SystemStatus,
}

thread_local! {
    static SERVICES: RefCell<Option<Services>> = const { RefCell::new(None) };
}

/// Starts all services; notifications go to `target` (the main window).
pub fn start(target: HWND) {
    let audio = audio::Audio::new(target).inspect_err(|err| log::warn!("audio unavailable: {err}")).ok();
    let network = network::Network::new(target);
    let services = Services {
        status: SystemStatus {
            volume: audio.as_ref().and_then(audio::Audio::state),
            network: network.state(),
            battery: power::battery(),
            brightness: None,
            audio_device: 0,
            audio_devices: 0,
        },
        audio,
        network,
        _power: power::Power::register(target),
        brightness: brightness::Brightness::start(target),
    };
    log::info!("status: {:?}", services.status);
    SERVICES.set(Some(services));
}

pub fn stop() {
    SERVICES.set(None);
}

fn with<R>(f: impl FnOnce(&mut Services) -> R) -> Option<R> {
    SERVICES.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

pub fn status() -> SystemStatus {
    with(|s| s.status.clone()).unwrap_or_default()
}

/// Handles `WM_APP_STATUS`. Returns whether the visible status changed.
pub fn handle_message(wparam: WPARAM, lparam: LPARAM) -> bool {
    log::debug!("status event kind={} value={}", wparam.0, lparam.0);
    with(|s| {
        let before = s.status.clone();
        match wparam.0 {
            KIND_AUDIO_DEVICE => {
                if let Some(audio) = &mut s.audio {
                    audio.attach_default();
                }
                s.status.volume = s.audio.as_ref().and_then(audio::Audio::state);
                s.status.audio_device = s.status.audio_device.wrapping_add(1);
            }
            KIND_AUDIO_LIST => s.status.audio_devices = s.status.audio_devices.wrapping_add(1),
            KIND_AUDIO => s.status.volume = s.audio.as_ref().and_then(audio::Audio::state),
            KIND_NETWORK => s.status.network = s.network.state(),
            KIND_BRIGHTNESS => {
                s.status.brightness = (lparam.0 != brightness::UNSUPPORTED).then(|| lparam.0.clamp(0, 100) as u8);
            }
            _ => {}
        }
        s.status != before
    })
    .unwrap_or(false)
}

/// Handles WM_POWERBROADCAST.
pub fn refresh_power() -> bool {
    with(|s| {
        let battery = power::battery();
        std::mem::replace(&mut s.status.battery, battery) != battery
    })
    .unwrap_or(false)
}

pub fn audio_outputs() -> Vec<audio::Output> {
    with(|s| s.audio.as_ref().map(audio::Audio::outputs)).flatten().unwrap_or_default()
}

/// Switches the default output; opens the Sound settings page when Windows refuses.
pub fn set_default_output(id: &str) {
    if let Some(Err(err)) = with(|s| s.audio.as_ref().map(|a| a.set_default_output(id))).flatten() {
        log::warn!("cannot switch default output: {err}");
        crate::platform::launch::open("ms-settings:sound");
    }
}

pub fn set_volume(level: f32) {
    with(|s| {
        if let Some(audio) = &s.audio {
            audio.set_level(level);
        }
    });
}

pub fn set_muted(muted: bool) {
    with(|s| {
        if let Some(audio) = &s.audio {
            audio.set_muted(muted);
        }
    });
}

/// Steps the volume by `delta` (e.g. mouse wheel), unmuting.
pub fn nudge_volume(delta: f32) {
    with(|s| {
        if let (Some(audio), Some(volume)) = (&s.audio, s.status.volume) {
            audio.set_level((volume.level + delta).clamp(0.0, 1.0));
        }
    });
}

pub fn set_brightness(percent: u8) {
    with(|s| s.brightness.set(percent));
}
