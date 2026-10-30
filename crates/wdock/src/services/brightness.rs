//! Display brightness: the built-in panel through WMI (`root\WMI`
//! WmiMonitorBrightness) and external monitors through DDC/CI (dxva2
//! `GetMonitorBrightness` / `SetMonitorBrightness`).
//!
//! Both are slow (tens to hundreds of milliseconds), so everything runs on one
//! worker thread. The UI sends requests; the worker detects displays (at
//! start, and when asked after a display change or resume — never polling),
//! throttles slider writes to the latest value every `WRITE_INTERVAL`, and
//! publishes the list of controllable displays through `displays()`, then posts
//! `KIND_BRIGHTNESS` to the main window.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread;
use std::time::{Duration, Instant};

use windows::Win32::Devices::Display::{
    DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME, DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME, DISPLAYCONFIG_DEVICE_INFO_HEADER,
    DISPLAYCONFIG_MODE_INFO, DISPLAYCONFIG_PATH_INFO, DISPLAYCONFIG_SOURCE_DEVICE_NAME, DISPLAYCONFIG_TARGET_DEVICE_NAME,
    DestroyPhysicalMonitors, DisplayConfigGetDeviceInfo, GetDisplayConfigBufferSizes, GetMonitorBrightness,
    GetNumberOfPhysicalMonitorsFromHMONITOR, GetPhysicalMonitorsFromHMONITOR, PHYSICAL_MONITOR, QDC_ONLY_ACTIVE_PATHS,
    QueryDisplayConfig, SetMonitorBrightness,
};
use windows::Win32::Foundation::{HANDLE, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoSetProxyBlanket, CoTaskMemFree,
    EOAC_NONE, RPC_C_AUTHN_LEVEL_CALL, RPC_C_IMP_LEVEL_IMPERSONATE,
};
use windows::Win32::System::Rpc::{RPC_C_AUTHN_WINNT, RPC_C_AUTHZ_NONE};
use windows::Win32::System::Variant::{VARIANT, VariantToInt32, VariantToStringAlloc};
use windows::Win32::System::Wmi::{
    IWbemClassObject, IWbemLocator, IWbemServices, WBEM_FLAG_FORWARD_ONLY, WBEM_FLAG_RETURN_IMMEDIATELY, WBEM_INFINITE,
    WbemLocator,
};
use windows::Win32::UI::WindowsAndMessaging::PostMessageW;
use windows::core::{BOOL, BSTR, Result, w};

use super::{KIND_BRIGHTNESS, WM_APP_STATUS};

/// Id of the built-in panel; external monitors are numbered from 1.
pub const BUILTIN: u32 = 0;
const BUILTIN_NAME: &str = "Built-in display";
/// Longest a DDC/CI controller is asked to take writes while a slider is dragged.
const WRITE_INTERVAL: Duration = Duration::from_millis(100);

/// A display whose brightness wdock can change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Display {
    pub id: u32,
    pub name: String,
    /// 0 ..= 100
    pub percent: u8,
}

static DISPLAYS: Mutex<Vec<Display>> = Mutex::new(Vec::new());

/// The displays found by the last detection, built-in panel first.
pub fn displays() -> Vec<Display> {
    DISPLAYS.lock().map(|d| d.clone()).unwrap_or_default()
}

enum Request {
    Detect,
    Set { id: u32, percent: u8, last: bool },
}

pub struct Brightness {
    requests: Sender<Request>,
}

impl Brightness {
    /// Starts the worker; it detects displays right away and notifies `target`.
    pub fn start(target: HWND) -> Self {
        let (requests, rx) = mpsc::channel();
        let target = target.0 as isize;
        let spawned = thread::Builder::new().name("brightness".into()).spawn(move || Worker::new(target).run(&rx));
        if let Err(err) = spawned {
            log::error!("brightness worker: {err}");
        }
        Self { requests }
    }

    /// Looks for displays again (display change, resume).
    pub fn detect(&self) {
        let _ = self.requests.send(Request::Detect);
    }

    /// Sets a display's level. While dragging pass `last = false`: writes are
    /// throttled; the final value (`last = true`) is always written.
    pub fn set(&self, id: u32, percent: u8, last: bool) {
        let _ = self.requests.send(Request::Set { id, percent: percent.min(100), last });
    }
}

// ---- pure logic ------------------------------------------------------------

/// Maps a monitor's raw `current` within `min..=max` to 0..=100.
pub fn to_percent(min: u32, current: u32, max: u32) -> u8 {
    if max <= min {
        return 0;
    }
    let span = u64::from(max - min);
    let offset = u64::from(current.clamp(min, max) - min);
    ((offset * 100 + span / 2) / span) as u8
}

/// Maps 0..=100 to a raw value within `min..=max`.
pub fn from_percent(min: u32, max: u32, percent: u8) -> u32 {
    if max <= min {
        return min;
    }
    let span = u64::from(max - min);
    min + ((span * u64::from(percent.min(100)) + 50) / 100) as u32
}

/// Limits writes to one per `interval` while a value keeps changing, but never
/// loses the newest value: it stays pending until due, or until a final write.
struct Throttle {
    interval: Duration,
    last_sent: Option<Instant>,
    pending: Option<u8>,
}

impl Throttle {
    fn new(interval: Duration) -> Self {
        Self { interval, last_sent: None, pending: None }
    }

    /// A new requested value; returns the value to write now, if any.
    fn submit(&mut self, value: u8, last: bool, now: Instant) -> Option<u8> {
        let due = self.last_sent.is_none_or(|sent| now.saturating_duration_since(sent) >= self.interval);
        if last || due {
            self.pending = None;
            Some(value)
        } else {
            self.pending = Some(value);
            None
        }
    }

    /// Records that a write finished at `now`.
    fn sent(&mut self, now: Instant) {
        self.last_sent = Some(now);
    }

    /// When the pending value becomes writable.
    fn deadline(&self) -> Option<Instant> {
        self.pending?;
        Some(self.last_sent.map_or_else(Instant::now, |sent| sent + self.interval))
    }

    /// The pending value, once due.
    fn poll(&mut self, now: Instant) -> Option<u8> {
        if self.deadline().is_some_and(|due| now >= due) { self.pending.take() } else { None }
    }
}

/// Two monitors of one model share a friendly name: number them.
fn disambiguate(names: &mut [String]) {
    let snapshot = names.to_vec();
    for (i, name) in names.iter_mut().enumerate() {
        let total = snapshot.iter().filter(|n| **n == snapshot[i]).count();
        if total > 1 {
            let nth = snapshot[..=i].iter().filter(|n| **n == snapshot[i]).count();
            *name = format!("{name} ({nth})");
        }
    }
}

// ---- worker ----------------------------------------------------------------

struct Ddc {
    id: u32,
    handle: HANDLE,
    name: String,
    min: u32,
    max: u32,
    percent: u8,
}

struct Worker {
    target: isize,
    wmi: Option<IWbemServices>,
    builtin: Option<u8>,
    ddc: Vec<Ddc>,
    throttles: HashMap<u32, Throttle>,
}

impl Worker {
    fn new(target: isize) -> Self {
        Self { target, wmi: None, builtin: None, ddc: Vec::new(), throttles: HashMap::new() }
    }

    fn run(&mut self, requests: &mpsc::Receiver<Request>) {
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        self.detect();
        loop {
            let deadline = self.throttles.values().filter_map(Throttle::deadline).min();
            let request = match deadline {
                Some(due) => match requests.recv_timeout(due.saturating_duration_since(Instant::now())) {
                    Ok(request) => Some(request),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => break,
                },
                None => match requests.recv() {
                    Ok(request) => Some(request),
                    Err(_) => break,
                },
            };
            match request {
                Some(Request::Detect) => {
                    // Display changes arrive in bursts; one detection covers them.
                    while let Ok(Request::Detect) = requests.try_recv() {}
                    self.detect();
                }
                Some(Request::Set { id, percent, last }) => {
                    let throttle = self.throttles.entry(id).or_insert_with(|| Throttle::new(WRITE_INTERVAL));
                    if let Some(value) = throttle.submit(percent, last, Instant::now()) {
                        self.write(id, value);
                    }
                }
                None => {}
            }
            let due: Vec<(u32, u8)> = {
                let now = Instant::now();
                self.throttles.iter_mut().filter_map(|(id, t)| t.poll(now).map(|v| (*id, v))).collect()
            };
            for (id, value) in due {
                self.write(id, value);
            }
        }
        self.release();
    }

    fn write(&mut self, id: u32, percent: u8) {
        let result = if id == BUILTIN {
            match &self.wmi {
                Some(service) => apply(service, percent).map(|()| self.builtin = Some(percent)),
                None => return,
            }
        } else if let Some(monitor) = self.ddc.iter_mut().find(|m| m.id == id) {
            let raw = from_percent(monitor.min, monitor.max, percent);
            if unsafe { SetMonitorBrightness(monitor.handle, raw) } == 0 {
                Err(windows::core::Error::from_thread())
            } else {
                monitor.percent = percent;
                Ok(())
            }
        } else {
            return;
        };
        if let Some(throttle) = self.throttles.get_mut(&id) {
            throttle.sent(Instant::now());
        }
        match result {
            Ok(()) => self.publish(),
            Err(err) => log::warn!("set brightness {percent} on display {id}: {err}"),
        }
    }

    fn release(&mut self) {
        for monitor in self.ddc.drain(..) {
            let _ = unsafe { DestroyPhysicalMonitors(&[PHYSICAL_MONITOR { hPhysicalMonitor: monitor.handle, ..Default::default() }]) };
        }
    }

    fn detect(&mut self) {
        self.throttles.clear();
        self.builtin = None;
        if self.wmi.is_none() {
            self.wmi = connect().inspect_err(|err| log::info!("WMI brightness unavailable: {err}")).ok();
        }
        if let Some(service) = &self.wmi {
            match current(service) {
                Ok(level) => self.builtin = Some(level),
                Err(err) => log::info!("no software brightness control on the built-in panel: {err}"),
            }
        }
        self.release();
        self.ddc = detect_ddc();
        log::info!(
            "brightness: built-in {:?}, DDC/CI {:?}",
            self.builtin,
            self.ddc.iter().map(|m| (&m.name, m.percent)).collect::<Vec<_>>()
        );
        self.publish();
    }

    fn publish(&self) {
        let mut list = Vec::new();
        if let Some(percent) = self.builtin {
            list.push(Display { id: BUILTIN, name: BUILTIN_NAME.to_owned(), percent });
        }
        list.extend(self.ddc.iter().map(|m| Display { id: m.id, name: m.name.clone(), percent: m.percent }));
        if let Ok(mut shared) = DISPLAYS.lock() {
            *shared = list;
        }
        let _ = unsafe { PostMessageW(Some(HWND(self.target as *mut _)), WM_APP_STATUS, WPARAM(KIND_BRIGHTNESS), LPARAM(0)) };
    }
}

// ---- DDC/CI ----------------------------------------------------------------

struct Found {
    handle: HMONITOR,
    device: String,
}

unsafe extern "system" fn collect(handle: HMONITOR, _: HDC, _: *mut RECT, data: LPARAM) -> BOOL {
    let out = unsafe { &mut *(data.0 as *mut Vec<Found>) };
    let mut info = MONITORINFOEXW::default();
    info.monitorInfo.cbSize = size_of::<MONITORINFOEXW>() as u32;
    if unsafe { GetMonitorInfoW(handle, (&raw mut info).cast::<MONITORINFO>()) }.as_bool() {
        let len = info.szDevice.iter().position(|&c| c == 0).unwrap_or(info.szDevice.len());
        out.push(Found { handle, device: String::from_utf16_lossy(&info.szDevice[..len]) });
    }
    true.into()
}

fn detect_ddc() -> Vec<Ddc> {
    let mut found: Vec<Found> = Vec::new();
    let _ = unsafe { EnumDisplayMonitors(None, None, Some(collect), LPARAM((&raw mut found) as isize)) };
    let friendly = friendly_names();
    let mut monitors = Vec::new();
    for item in &found {
        let mut count = 0;
        if unsafe { GetNumberOfPhysicalMonitorsFromHMONITOR(item.handle, &mut count) }.is_err() || count == 0 {
            continue;
        }
        let mut physical = vec![PHYSICAL_MONITOR::default(); count as usize];
        if unsafe { GetPhysicalMonitorsFromHMONITOR(item.handle, &mut physical) }.is_err() {
            continue;
        }
        let fallback = friendly.iter().find(|(device, _)| *device == item.device).map(|(_, name)| name.clone());
        for monitor in physical {
            let (mut min, mut cur, mut max) = (0, 0, 0);
            // The first DDC/CI exchange after wake-up sometimes fails: try twice.
            let ok = (0..2).any(|_| unsafe { GetMonitorBrightness(monitor.hPhysicalMonitor, &mut min, &mut cur, &mut max) } != 0);
            if !ok || max <= min {
                let _ = unsafe { DestroyPhysicalMonitors(&[monitor]) };
                continue;
            }
            let description = wide_to_string(&{ monitor.szPhysicalMonitorDescription });
            let name = fallback.clone().filter(|n| !n.is_empty()).unwrap_or(description);
            monitors.push(Ddc {
                id: monitors.len() as u32 + 1,
                handle: monitor.hPhysicalMonitor,
                name,
                min,
                max,
                percent: to_percent(min, cur, max),
            });
        }
    }
    let mut names: Vec<String> = monitors.iter().map(|m| m.name.clone()).collect();
    disambiguate(&mut names);
    for (monitor, name) in monitors.iter_mut().zip(names) {
        monitor.name = name;
    }
    monitors
}

fn wide_to_string(wide: &[u16]) -> String {
    String::from_utf16_lossy(&wide[..wide.iter().position(|&c| c == 0).unwrap_or(wide.len())])
}

/// `(GDI device name such as \\.\DISPLAY1, EDID friendly name)` for every active output.
fn friendly_names() -> Vec<(String, String)> {
    let (mut paths_len, mut modes_len) = (0, 0);
    if unsafe { GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut paths_len, &mut modes_len) }.is_err() {
        return Vec::new();
    }
    let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); paths_len as usize];
    let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); modes_len as usize];
    if unsafe { QueryDisplayConfig(QDC_ONLY_ACTIVE_PATHS, &mut paths_len, paths.as_mut_ptr(), &mut modes_len, modes.as_mut_ptr(), None) }
        .is_err()
    {
        return Vec::new();
    }
    paths
        .iter()
        .take(paths_len as usize)
        .filter_map(|path| unsafe {
            let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                    size: size_of::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>() as u32,
                    adapterId: path.sourceInfo.adapterId,
                    id: path.sourceInfo.id,
                },
                ..Default::default()
            };
            let mut target = DISPLAYCONFIG_TARGET_DEVICE_NAME {
                header: DISPLAYCONFIG_DEVICE_INFO_HEADER {
                    r#type: DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
                    size: size_of::<DISPLAYCONFIG_TARGET_DEVICE_NAME>() as u32,
                    adapterId: path.targetInfo.adapterId,
                    id: path.targetInfo.id,
                },
                ..Default::default()
            };
            if DisplayConfigGetDeviceInfo(&mut source.header) != 0 || DisplayConfigGetDeviceInfo(&mut target.header) != 0 {
                return None;
            }
            Some((wide_to_string(&source.viewGdiDeviceName), wide_to_string(&target.monitorFriendlyDeviceName)))
        })
        .collect()
}

// ---- WMI (built-in panel) --------------------------------------------------

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percent_scaling_round_trips() {
        for (min, max) in [(0, 100), (0, 255), (10, 90), (0, 1)] {
            for p in 0..=100u8 {
                let raw = from_percent(min, max, p);
                assert!((min..=max).contains(&raw));
                let back = to_percent(min, raw, max);
                // A coarse range cannot represent every percent; it must be monotonic and close.
                let step = 100 / (max - min).max(1);
                assert!(u32::from(back.abs_diff(p)) <= step.div_ceil(2) + 1, "{min}..{max} {p} -> {raw} -> {back}");
            }
        }
    }

    #[test]
    fn scaling_edges() {
        assert_eq!(to_percent(0, 0, 100), 0);
        assert_eq!(to_percent(0, 100, 100), 100);
        assert_eq!(to_percent(20, 20, 220), 0);
        assert_eq!(to_percent(20, 120, 220), 50);
        assert_eq!(to_percent(0, 500, 100), 100, "out-of-range readings clamp");
        assert_eq!(to_percent(5, 5, 5), 0, "degenerate range");
        assert_eq!(from_percent(0, 255, 100), 255);
        assert_eq!(from_percent(0, 255, 0), 0);
        assert_eq!(from_percent(0, 255, 50), 128);
        assert_eq!(from_percent(10, 90, 250), 90, "percent clamps");
    }

    #[test]
    fn throttle_sends_first_value_then_holds_the_newest() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut throttle = Throttle::new(ms(100));
        assert_eq!(throttle.submit(10, false, t0), Some(10));
        throttle.sent(t0);
        assert_eq!(throttle.submit(20, false, t0 + ms(20)), None);
        assert_eq!(throttle.submit(30, false, t0 + ms(40)), None);
        assert_eq!(throttle.deadline(), Some(t0 + ms(100)));
        assert_eq!(throttle.poll(t0 + ms(60)), None);
        assert_eq!(throttle.poll(t0 + ms(100)), Some(30), "only the latest value is written");
        throttle.sent(t0 + ms(100));
        assert_eq!(throttle.poll(t0 + ms(300)), None);
        assert_eq!(throttle.deadline(), None);
    }

    #[test]
    fn throttle_writes_final_value_immediately_and_drops_pending() {
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        let mut throttle = Throttle::new(ms(100));
        assert_eq!(throttle.submit(10, false, t0), Some(10));
        throttle.sent(t0);
        assert_eq!(throttle.submit(20, false, t0 + ms(10)), None);
        assert_eq!(throttle.submit(25, true, t0 + ms(20)), Some(25));
        assert_eq!(throttle.deadline(), None, "nothing stale left to write");
    }

    #[test]
    fn throttle_allows_a_write_after_the_interval() {
        let t0 = Instant::now();
        let mut throttle = Throttle::new(Duration::from_millis(100));
        assert_eq!(throttle.submit(1, false, t0), Some(1));
        throttle.sent(t0);
        assert_eq!(throttle.submit(2, false, t0 + Duration::from_millis(150)), Some(2));
    }

    #[test]
    fn same_model_monitors_are_numbered() {
        let mut names = vec!["DELL U2415".to_owned(), "LG 24".to_owned(), "DELL U2415".to_owned()];
        disambiguate(&mut names);
        assert_eq!(names, ["DELL U2415 (1)", "LG 24", "DELL U2415 (2)"]);
        let mut single = vec!["LG 24".to_owned()];
        disambiguate(&mut single);
        assert_eq!(single, ["LG 24"]);
    }
}
