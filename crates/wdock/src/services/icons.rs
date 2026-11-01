//! Original shell-provided icons for paths and `shell:AppsFolder` items.
//! Keep the bitmap at the size returned by Windows and convert its pixel
//! format for Direct2D; views can reuse it at their requested display sizes.

use windows::Win32::Foundation::SIZE;
use windows::Win32::Graphics::Gdi::{DeleteObject, HPALETTE};
use windows::Win32::Graphics::Imaging::{
    GUID_WICPixelFormat32bppPBGRA, IWICBitmapSource, IWICImagingFactory, WICBitmapCacheOnLoad,
    WICBitmapDitherTypeNone, WICBitmapPaletteTypeMedianCut,
    WICBitmapUsePremultipliedAlpha,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Shell::{
    IShellItem, IShellItemImageFactory, SHCreateItemFromParsingName, SIGDN_NORMALDISPLAY,
    SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
};
use windows::core::{HSTRING, Interface, Result};

pub struct ShellIcon {
    pub name: String,
    pub image: IWICBitmapSource,
}
pub fn load(wic: &IWICImagingFactory, target: &str, size_px: u32) -> Result<ShellIcon> {
    let item: IShellItem = unsafe { SHCreateItemFromParsingName(&HSTRING::from(target), None)? };
    load_item(wic, &item, size_px)
}

pub fn load_item(wic: &IWICImagingFactory, item: &IShellItem, size_px: u32) -> Result<ShellIcon> {
    unsafe {
        let display = item.GetDisplayName(SIGDN_NORMALDISPLAY)?;
        let name = display.to_string().unwrap_or_default();
        CoTaskMemFree(Some(display.0 as *const _));

        let factory: IShellItemImageFactory = item.cast()?;
        let size = SIZE { cx: size_px as i32, cy: size_px as i32 };
        let hbitmap = factory.GetImage(size, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK)?;
        let bitmap = wic.CreateBitmapFromHBITMAP(hbitmap, HPALETTE::default(), WICBitmapUsePremultipliedAlpha);
        let _ = DeleteObject(hbitmap.into());
        let bitmap = bitmap?;

        let converter = wic.CreateFormatConverter()?;
        converter.Initialize(
            &bitmap,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeMedianCut,
        )?;
        // Materialise the pixels so no COM chain back to the shell stays alive.
        let image = wic.CreateBitmapFromSource(&converter, WICBitmapCacheOnLoad)?.cast()?;
        Ok(ShellIcon { name, image })
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "reads this user's taskbar icons"]
    fn inspect_live_icon_sizes() {
        use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, IWICImagingFactory};
        use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap(); }
        let wic: IWICImagingFactory = unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap() };
        for target in crate::platform::taskbar_pins::read().unwrap() {
            for requested in [32, 64, 128, 256] {
                if let Ok(icon) = super::load(&wic, &target, requested) {
                    let (mut width, mut height) = (0, 0);
                    unsafe { icon.image.GetSize(&mut width, &mut height).unwrap(); }
                    eprintln!("{}: requested {requested}, received {width}x{height}", icon.name);
                }
            }
        }
    }
}
