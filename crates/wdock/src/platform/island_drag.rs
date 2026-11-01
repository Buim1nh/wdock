//! Shell file drag-and-drop for the Island shelf.
use std::cell::{Cell, RefCell};
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use windows::Win32::Foundation::{HWND, LPARAM, POINTL, WPARAM};
use windows::Win32::System::Com::{CoTaskMemFree, DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, IDropTarget,
    IDropTarget_Impl, RegisterDragDrop,
    ReleaseStgMedium, RevokeDragDrop,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::{
    CSIDL_DESKTOP, DragQueryFileW, HDROP, SHCreateDataObject, SHDoDragDrop,
    SHGetSpecialFolderLocation, SHParseDisplayName,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_LBUTTON};
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{HSTRING, Ref, Result, implement};

pub const DRAG_ENTER: usize = 20;
pub const DRAG_LEAVE: usize = 21;
pub const DRAG_DROP: usize = 22;
pub const WM_START_FILE_DRAG: u32 = WM_APP + 34;
thread_local! { static PENDING_DRAG: RefCell<Option<PathBuf>> = const { RefCell::new(None) }; }

fn format() -> FORMATETC {
    FORMATETC { cfFormat: CF_HDROP.0, dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32, ..Default::default() }
}

#[implement(IDropTarget)]
struct Target {
    hwnd: HWND,
    accepted: Cell<bool>,
}

impl IDropTarget_Impl for Target_Impl {
    fn DragEnter(&self, object: Ref<IDataObject>, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let accepted = object.as_ref().is_some_and(|o| unsafe { o.QueryGetData(&format()).0 == 0 });
        self.accepted.set(accepted);
        unsafe { *effect = if accepted && ((*effect).0 & DROPEFFECT_COPY.0 != 0) { DROPEFFECT_COPY } else { DROPEFFECT_NONE }; }
        if accepted { crate::ui::bar::post_command(self.hwnd, DRAG_ENTER); }
        Ok(())
    }

    fn DragOver(&self, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        unsafe { *effect = if self.accepted.get() && ((*effect).0 & DROPEFFECT_COPY.0 != 0) { DROPEFFECT_COPY } else { DROPEFFECT_NONE }; }
        Ok(())
    }

    fn DragLeave(&self) -> Result<()> {
        self.accepted.set(false);
        crate::ui::bar::post_command(self.hwnd, DRAG_LEAVE);
        Ok(())
    }

    fn Drop(&self, object: Ref<IDataObject>, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> Result<()> {
        let allowed = self.accepted.replace(false) && unsafe { (*effect).0 & DROPEFFECT_COPY.0 != 0 };
        let mut paths = Vec::new();
        if allowed && let Some(object) = object.as_ref()
            && let Ok(mut data) = unsafe { object.GetData(&format()) } {
                if data.tymed == TYMED_HGLOBAL.0 as u32 {
                    let drop = HDROP(unsafe { data.u.hGlobal.0 });
                    let count = unsafe { DragQueryFileW(drop, u32::MAX, None) }.min(32);
                    for index in 0..count {
                        let len = unsafe { DragQueryFileW(drop, index, None) } as usize;
                        if len == 0 || len > 32767 { continue; }
                        let mut name = vec![0u16; len + 1];
                        let copied = unsafe { DragQueryFileW(drop, index, Some(&mut name)) } as usize;
                        if copied != 0 { paths.push(PathBuf::from(std::ffi::OsString::from_wide(&name[..copied]))); }
                    }
                }
                unsafe { ReleaseStgMedium(&mut data); }
            }
        let accepted = crate::model::island_files::queue(paths) > 0;
        crate::ui::bar::post_command(self.hwnd, if accepted { DRAG_DROP } else { DRAG_LEAVE });
        unsafe { *effect = if accepted { DROPEFFECT_COPY } else { DROPEFFECT_NONE }; }
        Ok(())
    }
}

pub fn register(hwnd: HWND) -> bool {
    let target: IDropTarget = Target { hwnd, accepted: Cell::new(false) }.into();
    match unsafe { RegisterDragDrop(hwnd, &target) } {
        Ok(()) => true,
        Err(err) => { log::warn!("Island file drop unavailable: {err}"); false }
    }
}

pub fn unregister(hwnd: HWND) { let _ = unsafe { RevokeDragDrop(hwnd) }; }

fn data_object(path: &std::path::Path) -> Result<IDataObject> {
    unsafe {
        let desktop = SHGetSpecialFolderLocation(None, CSIDL_DESKTOP as i32)?;
        let mut item = std::ptr::null_mut();
        let name = HSTRING::from_wide(&path.as_os_str().encode_wide().collect::<Vec<_>>());
        let parsed = SHParseDisplayName(&name, None, &mut item, 0, None);
        let object = match parsed {
            Ok(()) => SHCreateDataObject(Some(desktop), Some(&[item.cast_const()]), None::<&IDataObject>),
            Err(err) => Err(err),
        };
        CoTaskMemFree(Some(desktop.cast()));
        if !item.is_null() { CoTaskMemFree(Some(item.cast())); }
        object
    }
}

/// Defer the nested OLE loop until the view's mutable event handler has returned.
pub fn drag_file(hwnd: HWND, path: PathBuf) {
    PENDING_DRAG.set(Some(path));
    if unsafe { PostMessageW(Some(hwnd), WM_START_FILE_DRAG, WPARAM(0), LPARAM(0)) }.is_err() {
        PENDING_DRAG.set(None);
    }
}

/// Called on the OLE-initialized UI thread before borrowing the bar's state.
pub fn start_pending_drag(hwnd: HWND) {
    let Some(path) = PENDING_DRAG.take() else { return; };
    if unsafe { GetAsyncKeyState(VK_LBUTTON.0 as i32) } >= 0 { return; }
    let result = data_object(&path).and_then(|object| unsafe {
        SHDoDragDrop(Some(hwnd), &object, None::<&windows::Win32::System::Ole::IDropSource>, DROPEFFECT_COPY).map(|_| ())
    });
    if let Err(err) = result { log::warn!("Island file drag failed: {err}"); }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ole_initializes_after_the_ui_apartment() {
        use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
        use windows::Win32::System::Ole::{OleInitialize, OleUninitialize};
        unsafe {
            CoInitializeEx(None, COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE).ok().unwrap();
            OleInitialize(None).unwrap();
            OleUninitialize();
            CoUninitialize();
        }
    }

    #[test]
    fn shell_data_object_contains_the_original_file() {
        use std::os::windows::ffi::OsStringExt;
        use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
        use windows::Win32::System::Ole::{OleInitialize, OleUninitialize, ReleaseStgMedium};
        unsafe { OleInitialize(None).unwrap(); }
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let data = super::data_object(&path).unwrap();
        let mut medium = unsafe { data.GetData(&super::format()).unwrap() };
        let drop = HDROP(unsafe { medium.u.hGlobal.0 });
        assert_eq!(unsafe { DragQueryFileW(drop, u32::MAX, None) }, 1);
        let len = unsafe { DragQueryFileW(drop, 0, None) } as usize;
        let mut buf = vec![0u16; len + 1];
        let n = unsafe { DragQueryFileW(drop, 0, Some(&mut buf)) } as usize;
        let actual = std::path::PathBuf::from(std::ffi::OsString::from_wide(&buf[..n]));
        assert_eq!(actual.to_string_lossy().to_lowercase(), path.to_string_lossy().to_lowercase());
        unsafe { ReleaseStgMedium(&mut medium); OleUninitialize(); }
    }

    #[test]
    fn drop_target_accepts_a_shell_file_without_copying_it() {
        use std::cell::Cell;
        use windows::Win32::Foundation::{HWND, POINTL};
        use windows::Win32::System::Ole::{IDropTarget, OleInitialize, OleUninitialize, DROPEFFECT_COPY};
        use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
        unsafe { OleInitialize(None).unwrap(); }
        crate::model::island_files::init_empty();
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
        let data = super::data_object(&path).unwrap();
        let target: IDropTarget = super::Target { hwnd: HWND::default(), accepted: Cell::new(false) }.into();
        let mut effect = DROPEFFECT_COPY;
        unsafe {
            target.DragEnter(&data, MODIFIERKEYS_FLAGS(0), POINTL::default(), &mut effect).unwrap();
            assert_eq!(effect, DROPEFFECT_COPY);
            target.Drop(&data, MODIFIERKEYS_FLAGS(0), POINTL::default(), &mut effect).unwrap();
        }
        assert_eq!(effect, DROPEFFECT_COPY);
        assert!(crate::model::island_files::accept_pending());
        assert_eq!(crate::model::island_files::list()[0].to_string_lossy().to_lowercase(), path.to_string_lossy().to_lowercase());
        assert!(path.exists());
        unsafe { OleUninitialize(); }
    }
}
