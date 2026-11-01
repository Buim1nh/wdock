//! Ordered Explorer taskbar pins, read once during wdock initialization.
//! IPinnedList3 is an undocumented Windows shell interface.
use std::ffi::c_void;
use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, CoTaskMemFree};
use windows::Win32::UI::Shell::{IEnumFullIDList, SHGetNameFromIDList, SIGDN_DESKTOPABSOLUTEPARSING};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::core::{GUID, HRESULT, IUnknown, IUnknown_Vtbl, Interface, Result};

const CLSID_TASKBAND_PIN: GUID = GUID::from_u128(0x90aa3a4e_1cba_4233_b8bb_535773d48449);
const IID_PINNED_LIST3: GUID = GUID::from_u128(0x0dd79ae2_d156_45d4_9eeb_3b549769e940);

#[repr(C)]
struct PinnedListVtable {
    base: IUnknown_Vtbl,
    enum_objects: unsafe extern "system" fn(*mut c_void, *mut *mut c_void) -> HRESULT,
}

fn normalize_target(text: String) -> String {
    if text.contains('!') && !text.contains('\\') && !text.contains('/') && !text.contains(':') {
        format!(r"shell:AppsFolder\{text}")
    } else {
        text
    }
}

/// Must be called from an initialized COM apartment.
pub fn read() -> Result<Vec<String>> {
    unsafe {
        let object: IUnknown = CoCreateInstance(&CLSID_TASKBAND_PIN, None, CLSCTX_INPROC_SERVER)?;
        let mut raw = std::ptr::null_mut();
        object.query(&IID_PINNED_LIST3, &mut raw).ok()?;
        let list = IUnknown::from_raw(raw);
        let table = *(list.as_raw() as *const *const PinnedListVtable);
        let mut raw_enum = std::ptr::null_mut();
        ((*table).enum_objects)(list.as_raw(), &mut raw_enum).ok()?;
        if raw_enum.is_null() {
            return Err(windows::core::Error::from_hresult(HRESULT(0x80004003u32 as i32)));
        }
        let enumerator = IEnumFullIDList::from_raw(raw_enum);
        let mut targets = Vec::new();
        loop {
            let mut item: *mut ITEMIDLIST = std::ptr::null_mut();
            let result = enumerator.Next(std::slice::from_mut(&mut item), None);
            if result.0 == 1 { break; }
            result.ok()?;
            if item.is_null() {
                return Err(windows::core::Error::from_hresult(HRESULT(0x80004003u32 as i32)));
            }
            let name = SHGetNameFromIDList(item, SIGDN_DESKTOPABSOLUTEPARSING);
            CoTaskMemFree(Some(item.cast()));
            let name = name?;
            let text = name.to_string();
            CoTaskMemFree(Some(name.0.cast()));
            let text = text.map_err(|_| windows::core::Error::from_hresult(HRESULT(0x80070057u32 as i32)))?;
            if !text.is_empty() { targets.push(normalize_target(text)); }
        }
        Ok(targets)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore = "reads this user's taskbar"]
    fn reads_live_taskbar() {
        use windows::Win32::System::Com::{CoInitializeEx, CoCreateInstance, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
        use windows::Win32::Graphics::Imaging::{CLSID_WICImagingFactory, IWICImagingFactory};
        unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok().unwrap(); }
        let pins = super::read().unwrap();
        let wic: IWICImagingFactory = unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).unwrap() };
        for pin in &pins { eprintln!("taskbar pin: {pin}"); }
        assert!(!pins.is_empty());
        for pin in &pins {
            let resolved = crate::model::apps::Pinned::resolve(&wic, pin, 32);
            assert!(resolved.is_some(), "could not resolve {pin}");
            assert!(resolved.unwrap().icon.is_some(), "could not load icon for {pin}");
        }
    }

    #[test]
    fn normalizes_packaged_apps_without_altering_shortcuts() {
        assert_eq!(super::normalize_target("Package!App".into()), r"shell:AppsFolder\Package!App");
        assert_eq!(super::normalize_target(r"C:\Apps\Test!.lnk".into()), r"C:\Apps\Test!.lnk");
    }
}
