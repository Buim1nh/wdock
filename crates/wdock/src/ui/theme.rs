use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;
use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
use windows::core::w;

use crate::model::config::{Appearance, Backdrop, ThemeMode};
use crate::ui::gfx::rgba;

pub struct Theme {
    pub dark: bool,
    pub backdrop: Backdrop,
    pub text: D2D1_COLOR_F,
    /// Secondary labels.
    pub text_dim: D2D1_COLOR_F,
    /// Selection, slider fill, today in the calendar.
    pub accent: D2D1_COLOR_F,
    /// Text drawn on top of the accent colour.
    pub on_accent: D2D1_COLOR_F,
    /// Buttons and tiles at rest.
    pub control: D2D1_COLOR_F,
    /// Acrylic tint passed to DWM, 0xAABBGGRR.
    pub acrylic_tint: u32,
    /// Background fill when the backdrop is solid.
    pub solid_fill: D2D1_COLOR_F,
    pub border: D2D1_COLOR_F,
    pub hover: D2D1_COLOR_F,
}

impl Theme {
    pub fn resolve(appearance: &Appearance) -> Self {
        let dark = match appearance.theme {
            ThemeMode::Dark => true,
            ThemeMode::Light => false,
            ThemeMode::Auto => !system_uses_light_theme(),
        };
        if dark {
            Self {
                dark,
                backdrop: appearance.backdrop,
                text: rgba(255, 255, 255, 0.92),
                text_dim: rgba(255, 255, 255, 0.55),
                accent: rgba(10, 132, 255, 1.0),
                on_accent: rgba(255, 255, 255, 1.0),
                control: rgba(255, 255, 255, 0.08),
                acrylic_tint: 0x991E_1C1C,
                solid_fill: rgba(30, 30, 32, 0.94),
                border: rgba(255, 255, 255, 0.10),
                hover: rgba(255, 255, 255, 0.14),
            }
        } else {
            Self {
                dark,
                backdrop: appearance.backdrop,
                text: rgba(0, 0, 0, 0.88),
                text_dim: rgba(0, 0, 0, 0.5),
                accent: rgba(0, 122, 255, 1.0),
                on_accent: rgba(255, 255, 255, 1.0),
                control: rgba(0, 0, 0, 0.06),
                acrylic_tint: 0x88F3_F3F3,
                solid_fill: rgba(246, 246, 248, 0.95),
                border: rgba(0, 0, 0, 0.10),
                hover: rgba(0, 0, 0, 0.08),
            }
        }
    }
}

fn system_uses_light_theme() -> bool {
    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize"),
            w!("SystemUsesLightTheme"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast()),
            Some(&mut size),
        )
    };
    status.is_ok() && value != 0
}
