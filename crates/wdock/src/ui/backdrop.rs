//! DWM-side window decoration: acrylic blur, corner rounding, dark mode.

use std::ffi::c_void;
use std::sync::OnceLock;

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::Dwm::{
    DWM_WINDOW_CORNER_PREFERENCE, DWMWA_USE_IMMERSIVE_DARK_MODE, DWMWA_WINDOW_CORNER_PREFERENCE,
    DWMWCP_DONOTROUND, DWMWCP_ROUND, DwmSetWindowAttribute,
};
use windows::Win32::System::LibraryLoader::{GetModuleHandleW, GetProcAddress};
use windows::core::{BOOL, s, w};

use crate::model::config::Backdrop;
use crate::ui::theme::Theme;

/// Radius DWM uses for `DWMWCP_ROUND`, in DIPs. Our own fills match it.
pub const DWM_CORNER_RADIUS: f32 = 8.0;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Corners {
    Square,
    Round,
    /// No DWM decoration at all: a fully transparent layer we draw into.
    None,
}

pub fn apply(hwnd: HWND, theme: &Theme, corners: Corners) {
    let corner = match corners {
        Corners::Square | Corners::None => DWMWCP_DONOTROUND,
        Corners::Round => DWMWCP_ROUND,
    };
    set_attribute(hwnd, DWMWA_WINDOW_CORNER_PREFERENCE.0 as u32, &corner);
    set_attribute(hwnd, DWMWA_USE_IMMERSIVE_DARK_MODE.0 as u32, &BOOL::from(theme.dark));

    if corners == Corners::None {
        return;
    }
    if theme.backdrop == Backdrop::Acrylic && !set_acrylic(hwnd, theme.acrylic_tint) {
        log::warn!("acrylic backdrop unavailable; window will use its solid fill");
    }
}

fn set_attribute<T>(hwnd: HWND, attribute: u32, value: &T) {
    let result = unsafe {
        DwmSetWindowAttribute(
            hwnd,
            windows::Win32::Graphics::Dwm::DWMWINDOWATTRIBUTE(attribute as i32),
            (value as *const T).cast(),
            size_of::<T>() as u32,
        )
    };
    if let Err(err) = result {
        log::debug!("DwmSetWindowAttribute({attribute}) failed: {err}");
    }
}

// The accent API is undocumented but has been stable since Windows 10 1803 and
// is what translucent taskbar tools rely on. It works on frameless popups,
// unlike DWMWA_SYSTEMBACKDROP_TYPE.
#[repr(C)]
struct AccentPolicy {
    state: u32,
    flags: u32,
    gradient_color: u32,
    animation_id: u32,
}

#[repr(C)]
struct CompositionAttributeData {
    attribute: u32,
    data: *mut c_void,
    size: usize,
}

const WCA_ACCENT_POLICY: u32 = 19;
const ACCENT_ENABLE_ACRYLICBLURBEHIND: u32 = 4;

type SetWindowCompositionAttribute = unsafe extern "system" fn(HWND, *mut CompositionAttributeData) -> BOOL;

fn set_window_composition_attribute() -> Option<SetWindowCompositionAttribute> {
    static PROC: OnceLock<Option<SetWindowCompositionAttribute>> = OnceLock::new();
    *PROC.get_or_init(|| unsafe {
        let user32 = GetModuleHandleW(w!("user32.dll")).ok()?;
        let proc = GetProcAddress(user32, s!("SetWindowCompositionAttribute"))?;
        Some(std::mem::transmute::<unsafe extern "system" fn() -> isize, SetWindowCompositionAttribute>(proc))
    })
}

fn set_acrylic(hwnd: HWND, tint: u32) -> bool {
    let Some(set) = set_window_composition_attribute() else {
        return false;
    };
    let mut policy = AccentPolicy {
        state: ACCENT_ENABLE_ACRYLICBLURBEHIND,
        flags: 0,
        gradient_color: tint,
        animation_id: 0,
    };
    let mut data = CompositionAttributeData {
        attribute: WCA_ACCENT_POLICY,
        data: (&raw mut policy).cast(),
        size: size_of::<AccentPolicy>(),
    };
    unsafe { set(hwnd, &mut data) }.as_bool()
}

// Keep the enum type referenced so a windows-rs rename is caught at compile time.
const _: DWM_WINDOW_CORNER_PREFERENCE = DWMWCP_ROUND;
