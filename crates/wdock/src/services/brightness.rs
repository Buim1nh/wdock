//! Built-in panel brightness through WMI (`root\WMI` WmiMonitorBrightness).
//! WMI calls take tens of milliseconds, so they run on a worker thread that
//! coalesces slider drags into the latest requested value. External monitors
//! (DDC/CI) are not handled.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoSetProxyBlanket,
    EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE};
use windows::Win32::System::Variant::{VARIANT, VariantToInt32, VariantToStringAlloc};
use windows::Win32::System::Wmi::{
    IWbemClassObject, IWbemLocator, IWbemServices, WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY,
    WBEM_INFINITE, WbemLocator,
};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::core::{BSTR, Result, w};

use super::{KIND_BRIGHTNESS, WM_APP_STATUS};

/// Posted `lparam` when the display has no software brightness control.
pub const UNSUPPORTED: isize = -1;

pub struct Brightness {
    requests: Sender<u8>,
}

impl Brightness {
    /// Starts the worker; it reports the current level (or `UNSUPPORTED`) to `target`.
    pub fn start(target: HWND) -> Self {
        let (requests, rx) = mpsc::channel();
        let target = target.0 as isize;
        let spawned = thread::Builder::new().name("brightness".into()).spawn(move || worker(target, rx));
        if let Err(err) = spawned {
            log::error!("brightness worker: {err}");
        }
        Self { requests }
    }

    pub fn set(&self, percent: u8) {
        let _ = self.requests.send(percent.min(100));
    }
}

fn report(target: isize, value: isize) {
    let _ = unsafe {
        PostMessageW(Some(HWND(target as *mut _)), WM_APP_STATUS, WPARAM(KIND_BRIGHTNESS), LPARAM(value))
    };
}

fn worker(target: isize, requests: Receiver<u8>) {
    let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
    let service = match connect() {
        Ok(service) => service,
        Err(err) => {
            log::info!("WMI brightness unavailable: {err}");
            report(target, UNSUPPORTED);
            return;
        }
    };
    match current(&service) {
        Ok(level) => report(target, level as isize),
        Err(err) => {
            log::info!("no software brightness control: {err}");
            report(target, UNSUPPORTED);
            return;
        }
    }
    // Block for a request, then skip to the newest one queued behind it.
    while let Ok(mut level) = requests.recv() {
        while let Ok(newer) = requests.try_recv() {
            level = newer;
        }
        match apply(&service, level) {
            Ok(()) => report(target, level as isize),
            Err(err) => log::warn!("set brightness {level}: {err}"),
        }
    }
}

fn connect() -> Result<IWbemServices> {
    unsafe {
        let locator: IWbemLocator = CoCreateInstance(&WbemLocator, None, CLSCTX_INPROC_SERVER)?;
        let service = locator.ConnectServer(&BSTR::from(r"ROOT\WMI"), &BSTR::new(), &BSTR::new(), &BSTR::new(), 0, &BSTR::new(), None)?;
        CoSetProxyBlanket(
            &service,
            RPC_C_AUTHN_WINNT,
            RPC_C_AUTHZ_NONE,
            None,
            RPC_C_AUTHN_LEVEL_CALL,
            RPC_C_IMP_LEVEL_IMPERSONATE,
            None,
            EOAC_NONE,
        )?;
        Ok(service)
    }
}

fn first(service: &IWbemServices, query: &str) -> Result<IWbemClassObject> {
    unsafe {
        let rows = service.ExecQuery(
            &BSTR::from("WQL"),
            &BSTR::from(query),
            WBEM_FLAG_FORWARD_ONLY | WBEM_FLAG_RETURN_IMMEDIATELY,
            None,
        )?;
        let mut objects = [None];
        let mut returned = 0;
        rows.Next(WBEM_INFINITE, &mut objects, &mut returned).ok()?;
        objects[0].take().ok_or_else(windows::core::Error::empty)
    }
}

fn current(service: &IWbemServices) -> Result<u8> {
    let row = first(service, "SELECT CurrentBrightness FROM WmiMonitorBrightness WHERE Active=TRUE")?;
    let mut value = VARIANT::default();
    unsafe {
        row.Get(w!("CurrentBrightness"), 0, &mut value, None, None)?;
        Ok(VariantToInt32(&value)?.clamp(0, 100) as u8)
    }
}

fn apply(service: &IWbemServices, level: u8) -> Result<()> {
    unsafe {
        let instance = first(service, "SELECT * FROM WmiMonitorBrightnessMethods WHERE Active=TRUE")?;
        let mut path = VARIANT::default();
        instance.Get(w!("__PATH"), 0, &mut path, None, None)?;
        let path_text = VariantToStringAlloc(&path)?;
        let path = BSTR::from_wide(path_text.as_wide());
        CoTaskMemFree(Some(path_text.0 as *const _));

        let mut class = None;
        service.GetObject(&BSTR::from("WmiMonitorBrightnessMethods"), Default::default(), None, Some(&mut class), None)?;
        let class = class.ok_or_else(windows::core::Error::empty)?;
        let mut inputs = None;
        class.GetMethod(w!("WmiSetBrightness"), 0, &mut inputs, std::ptr::null_mut())?;
        let inputs = inputs.ok_or_else(windows::core::Error::empty)?.SpawnInstance(0)?;
        inputs.Put(w!("Timeout"), 0, &VARIANT::from(1i32), 0)?;
        inputs.Put(w!("Brightness"), 0, &VARIANT::from(level), 0)?;
        service.ExecMethod(&path, &BSTR::from("WmiSetBrightness"), Default::default(), None, &inputs, None, None)?;
        Ok(())
    }
}
