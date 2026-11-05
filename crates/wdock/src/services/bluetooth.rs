//! Popup-scoped Bluetooth discovery and control. WinRT waits and audio driver
//! requests stay on the worker; callbacks only queue a coalesced refresh.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, Weak, atomic::{AtomicBool, Ordering}, mpsc};

use windows::Devices::Bluetooth::{BluetoothConnectionStatus, BluetoothDevice, BluetoothLEDevice, BluetoothMajorClass};
use windows::Devices::Enumeration::{DeviceInformation, DeviceInformationKind, DeviceInformationUpdate, DeviceWatcher};
use windows::Devices::Radios::{Radio, RadioAccessStatus, RadioKind, RadioState};
use windows::Foundation::{IPropertyValue, TypedEventHandler};
use windows::Win32::Foundation::{E_FAIL, HWND, PROPERTYKEY};
use windows::Win32::Media::Audio::{DEVICE_STATE, DEVICE_STATEMASK_ALL, IDeviceTopology, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, eAll};
use windows::Win32::Media::KernelStreaming::{IKsControl, KSIDENTIFIER, KSIDENTIFIER_0, KSIDENTIFIER_0_0, KSPROPERTY_ONESHOT_DISCONNECT, KSPROPERTY_ONESHOT_RECONNECT, KSPROPSETID_BtAudio};
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, CoTaskMemFree, STGM_READ};
use windows::Win32::System::Com::StructuredStorage::PropVariantToGUID;
use windows::Win32::System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize};
use windows::core::{GUID, HSTRING, Interface, PCWSTR, Result};

use crate::{platform::launch, ui::bar};

pub const SETTINGS_PAGE: &str = "ms-settings:bluetooth";
const CONTAINER: &str = "System.Devices.Aep.ContainerId";
const BATTERY: &str = "{104EA319-6EE2-4701-BD47-8DDBF425BBE5} 2";
const SYSTEM_CONTAINER: GUID = GUID::from_u128(0x00000000_0000_0000_ffff_ffffffffffff);
const CONTAINER_KEY: PROPERTYKEY = PROPERTYKEY { fmtid: GUID::from_u128(0x8c7ed206_3f8a_4827_b3ab_ae9e1faefc6c), pid: 2 };

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind { Audio, Other }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub connected: bool,
    pub kind: Kind,
    pub battery: Option<u8>,
    pub container: Option<GUID>,
}

pub fn tidy(list: Vec<Device>) -> Vec<Device> {
    let mut merged: Vec<Device> = Vec::new();
    for mut device in list.into_iter().filter(|d| !d.name.trim().is_empty()) {
        device.battery = device.battery.filter(|&n| n <= 100);
        let existing = merged.iter_mut().find(|d| d.id == device.id || same_container(d.container, device.container));
        if let Some(existing) = existing {
            existing.connected |= device.connected;
            existing.battery = existing.battery.or(device.battery);
            existing.container = existing.container.or(device.container);
            if device.kind == Kind::Audio { existing.kind = Kind::Audio; }
        } else {
            merged.push(device);
        }
    }
    merged.sort_by(|a, b| b.connected.cmp(&a.connected).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())).then(a.id.cmp(&b.id)));
    merged
}

fn valid_container(id: GUID) -> bool {
    id != GUID::zeroed() && id != SYSTEM_CONTAINER
}

fn same_container(a: Option<GUID>, b: Option<GUID>) -> bool {
    a.is_some_and(|a| valid_container(a) && Some(a) == b)
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub ready: bool,
    pub available: bool,
    pub on: bool,
    pub devices: Vec<Device>,
}

impl Snapshot {
    pub fn subtitle(&self) -> String {
        let count = self.devices.iter().filter(|d| d.connected).count();
        if !self.on { "Off".into() } else if count == 0 { "On".into() } else { format!("On · {count} connected") }
    }
}

type Cleanup = Box<dyn FnOnce() + Send>;

#[derive(Default)]
struct Live {
    active: bool,
    snapshot: Snapshot,
    cleanup: Vec<Cleanup>,
    connections: HashMap<String, Weak<Connection>>,
}

struct Shared {
    live: Mutex<Live>,
    active: AtomicBool,
    pending: AtomicBool,
    target: isize,
}

impl Shared {
    fn new(target: HWND) -> Arc<Self> {
        Arc::new(Self { live: Mutex::new(Live { active: true, ..Default::default() }), active: AtomicBool::new(true), pending: AtomicBool::new(false), target: target.0 as isize })
    }

    fn snapshot(&self) -> Snapshot {
        self.live.lock().map(|s| s.snapshot.clone()).unwrap_or_default()
    }

    fn publish(&self, snapshot: Snapshot) {
        if let Ok(mut live) = self.live.lock() && live.active && live.snapshot != snapshot {
            live.snapshot = snapshot;
            bar::notify_model_changed(HWND(self.target as *mut _));
        }
    }

    fn stop(&self) {
        self.active.store(false, Ordering::Release);
        let (cleanup, connections) = if let Ok(mut live) = self.live.lock() {
            live.active = false;
            (std::mem::take(&mut live.cleanup), std::mem::take(&mut live.connections))
        } else { (Vec::new(), HashMap::new()) };
        // Revoke outside the lock: WinRT may finish an in-flight callback here.
        for remove in cleanup.into_iter().rev() { remove(); }
        for connection in connections.into_values().filter_map(|c| c.upgrade()) { connection.stop(); }
    }
}

pub struct Probe { shared: Arc<Shared> }

impl Probe {
    pub fn open(target: HWND) -> Self {
        let shared = Shared::new(target);
        let worker = shared.clone();
        spawn(move || {
            let snapshot = read_snapshot(&worker, None).unwrap_or_else(|err| {
                log::info!("bluetooth probe: {err}");
                Snapshot { ready: true, ..Default::default() }
            });
            worker.publish(snapshot);
        });
        Self { shared }
    }

    pub fn snapshot(&self) -> Snapshot { self.shared.snapshot() }
}

impl Drop for Probe {
    fn drop(&mut self) { self.shared.stop(); }
}

enum Command { Refresh, Radio(bool), Device(String), Stop }

pub struct Session {
    shared: Arc<Shared>,
    tx: mpsc::Sender<Command>,
}

fn spawn(work: impl FnOnce() + Send + 'static) {
    if let Err(err) = std::thread::Builder::new().name("bluetooth".into()).spawn(move || {
        if let Err(err) = unsafe { RoInitialize(RO_INIT_MULTITHREADED) } {
            log::warn!("bluetooth WinRT: {err}");
            return;
        }
        work();
        unsafe { RoUninitialize(); }
    }) { log::warn!("bluetooth worker: {err}"); }
}

fn changed(shared: &Shared, tx: &mpsc::Sender<Command>) {
    if shared.active.load(Ordering::Acquire) && !shared.pending.swap(true, Ordering::AcqRel) {
        let _ = tx.send(Command::Refresh);
    }
}

impl Session {
    pub fn open(target: HWND) -> Self {
        let shared = Shared::new(target);
        let (tx, rx) = mpsc::channel();
        let (worker, sender) = (shared.clone(), tx.clone());
        spawn(move || {
            let mut connections = HashMap::new();
            if let Err(err) = watch(&worker, &sender) {
                log::info!("bluetooth watchers: {err}");
                worker.publish(Snapshot { ready: true, ..Default::default() });
                worker.stop();
                return;
            }
            changed(&worker, &sender);
            while let Ok(command) = rx.recv() {
                if !worker.active.load(Ordering::Acquire) { break; }
                let result = match command {
                    Command::Stop => break,
                    Command::Refresh => {
                        worker.pending.store(false, Ordering::Release);
                        read_snapshot(&worker, Some((&sender, &mut connections))).map(|s| worker.publish(s))
                    }
                    Command::Radio(on) => set_radio(&worker, on),
                    Command::Device(ref id) => {
                        let snapshot = worker.snapshot();
                        snapshot.devices.iter().find(|d| &d.id == id).ok_or_else(|| E_FAIL.into()).and_then(|d| audio_action(&worker, d))
                    }
                };
                if let Err(err) = result {
                    log::info!("bluetooth: {err}");
                    if !matches!(command, Command::Refresh) && worker.active.load(Ordering::Acquire) { launch::open(SETTINGS_PAGE); }
                    if matches!(command, Command::Refresh) {
                        worker.publish(Snapshot { ready: true, ..Default::default() });
                    }
                }
            }
        });
        Self { shared, tx }
    }

    pub fn snapshot(&self) -> Snapshot { self.shared.snapshot() }
    fn send(&self, command: Command) {
        if let Err(err) = self.tx.send(command) {
            log::info!("bluetooth worker unavailable: {err}");
            launch::open(SETTINGS_PAGE);
        }
    }

    pub fn set_radio(&self, on: bool) { self.send(Command::Radio(on)); }
    pub fn activate(&self, id: &str) { self.send(Command::Device(id.into())); }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shared.stop();
        let _ = self.tx.send(Command::Stop);
    }
}

fn radios() -> Result<Vec<Radio>> {
    Ok(Radio::GetRadiosAsync()?.join()?.into_iter().filter(|r| r.Kind().ok() == Some(RadioKind::Bluetooth)).collect())
}

fn selectors() -> Result<[(HSTRING, bool); 2]> {
    Ok([(BluetoothDevice::GetDeviceSelectorFromPairingState(true)?, false), (BluetoothLEDevice::GetDeviceSelectorFromPairingState(true)?, true)])
}

fn watch(shared: &Arc<Shared>, tx: &mpsc::Sender<Command>) -> Result<()> {
    if !shared.active.load(Ordering::Acquire) { return Ok(()); }
    let radios = radios()?;
    if radios.is_empty() { return Err(E_FAIL.into()); }
    for radio in radios {
        let Ok(mut live) = shared.live.lock() else { return Err(E_FAIL.into()); };
        if !live.active { return Ok(()); }
        let (state, sender) = (shared.clone(), tx.clone());
        let token = radio.StateChanged(&TypedEventHandler::new(move |_, _| { changed(&state, &sender); Ok(()) }))?;
        live.cleanup.push(Box::new(move || { let _ = radio.RemoveStateChanged(token); }));
    }
    for (selector, _) in selectors()? {
        // Serialize registration and Start against Drop, including partial failures.
        let Ok(mut live) = shared.live.lock() else { return Err(E_FAIL.into()); };
        if !live.active { return Ok(()); }
        let properties = properties();
        let watcher = DeviceInformation::CreateWatcherAqsFilterAndAdditionalProperties(&selector, &properties)?;
        let (state, sender) = (shared.clone(), tx.clone());
        let token = watcher.Added(&TypedEventHandler::<DeviceWatcher, DeviceInformation>::new(move |_, _| { changed(&state, &sender); Ok(()) }))?;
        let remove = watcher.clone();
        live.cleanup.push(Box::new(move || { let _ = remove.RemoveAdded(token); }));
        let (state, sender) = (shared.clone(), tx.clone());
        let token = watcher.Updated(&TypedEventHandler::<DeviceWatcher, DeviceInformationUpdate>::new(move |_, _| { changed(&state, &sender); Ok(()) }))?;
        let remove = watcher.clone();
        live.cleanup.push(Box::new(move || { let _ = remove.RemoveUpdated(token); }));
        let (state, sender) = (shared.clone(), tx.clone());
        let token = watcher.Removed(&TypedEventHandler::<DeviceWatcher, DeviceInformationUpdate>::new(move |_, _| { changed(&state, &sender); Ok(()) }))?;
        let remove = watcher.clone();
        live.cleanup.push(Box::new(move || { let _ = remove.RemoveRemoved(token); }));
        watcher.Start()?;
        live.cleanup.push(Box::new(move || { let _ = watcher.Stop(); }));
    }
    Ok(())
}

enum Link {
    Classic(BluetoothDevice, Option<i64>),
    LowEnergy(BluetoothLEDevice, Option<i64>),
}

struct Connection {
    link: Link,
    active: AtomicBool,
}

impl Connection {
    fn connected(&self) -> bool {
        match &self.link {
            Link::Classic(d, _) => d.ConnectionStatus(),
            Link::LowEnergy(d, _) => d.ConnectionStatus(),
        }.ok() == Some(BluetoothConnectionStatus::Connected)
    }

    fn stop(&self) {
        if !self.active.swap(false, Ordering::AcqRel) { return; }
        match &self.link {
            Link::Classic(d, token) => { if let Some(token) = token { let _ = d.RemoveConnectionStatusChanged(*token); } let _ = d.Close(); }
            Link::LowEnergy(d, token) => { if let Some(token) = token { let _ = d.RemoveConnectionStatusChanged(*token); } let _ = d.Close(); }
        }
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.stop();
    }
}

type Connections = HashMap<String, Arc<Connection>>;

fn property(info: &DeviceInformation, key: &str) -> Option<IPropertyValue> {
    info.Properties().ok()?.Lookup(&HSTRING::from(key)).ok()?.cast().ok()
}

fn battery(info: &DeviceInformation) -> Option<u8> {
    let p = property(info, BATTERY)?;
    p.GetUInt8().ok().or_else(|| p.GetUInt32().ok().and_then(|v| u8::try_from(v).ok())).filter(|&p| p <= 100)
}

fn properties() -> windows_collections::IIterable<HSTRING> {
    vec![HSTRING::from(CONTAINER), HSTRING::from(BATTERY)].into()
}

fn connection(shared: &Arc<Shared>, sender: Option<&mpsc::Sender<Command>>, id: &str, ble: bool) -> Result<Option<Arc<Connection>>> {
    let mut link = if ble {
        Link::LowEnergy(BluetoothLEDevice::FromIdAsync(&HSTRING::from(id))?.join()?, None)
    } else {
        Link::Classic(BluetoothDevice::FromIdAsync(&HSTRING::from(id))?.join()?, None)
    };
    let Ok(mut live) = shared.live.lock() else { return Err(E_FAIL.into()); };
    if !live.active {
        drop(Connection { link, active: AtomicBool::new(true) });
        return Ok(None);
    }
    if let Some(sender) = sender {
        let (state, sender) = (shared.clone(), sender.clone());
        let registered = match &mut link {
            Link::Classic(d, token) => d.ConnectionStatusChanged(&TypedEventHandler::new(move |_, _| { changed(&state, &sender); Ok(()) })).map(|t| *token = Some(t)),
            Link::LowEnergy(d, token) => d.ConnectionStatusChanged(&TypedEventHandler::new(move |_, _| { changed(&state, &sender); Ok(()) })).map(|t| *token = Some(t)),
        };
        if let Err(err) = registered {
            drop(Connection { link, active: AtomicBool::new(true) });
            return Err(err);
        }
    }
    let connection = Arc::new(Connection { link, active: AtomicBool::new(true) });
    if sender.is_some() { live.connections.insert(id.into(), Arc::downgrade(&connection)); }
    Ok(Some(connection))
}

fn read_snapshot(shared: &Arc<Shared>, mut events: Option<(&mpsc::Sender<Command>, &mut Connections)>) -> Result<Snapshot> {
    if !shared.active.load(Ordering::Acquire) { return Ok(Snapshot::default()); }
    let radios = radios()?;
    let mut snapshot = Snapshot { ready: true, available: !radios.is_empty(), on: radios.iter().any(|r| r.State().ok() == Some(RadioState::On)), devices: Vec::new() };
    if !snapshot.on || !shared.active.load(Ordering::Acquire) {
        if let Some((_, map)) = events {
            map.clear();
            if let Ok(mut live) = shared.live.lock() { live.connections.clear(); }
        }
        return Ok(snapshot);
    }
    let audio = audio_containers();
    let mut seen = Vec::new();
    for (selector, ble) in selectors()? {
        if !shared.active.load(Ordering::Acquire) { break; }
        let properties = properties();
        let found = match DeviceInformation::FindAllAsyncAqsFilterAndAdditionalProperties(&selector, &properties).and_then(|op| op.join()) {
            Ok(found) => found,
            Err(err) => { log::info!("bluetooth enumeration: {err}"); continue; }
        };
        for info in found {
            if !shared.active.load(Ordering::Acquire) { break; }
            let Ok(id) = info.Id() else { continue; };
            let id = id.to_string();
            seen.push(id.clone());
            let container = property(&info, CONTAINER).and_then(|p| p.GetGuid().ok()).filter(|g| valid_container(*g));
            let mut kind = if audio.iter().any(|&c| same_container(Some(c), container)) { Kind::Audio } else { Kind::Other };
            let connection = events.as_ref().and_then(|(_, map)| map.get(&id).cloned());
            let connection = connection.or_else(|| {
                if !shared.active.load(Ordering::Acquire) { return None; }
                match self::connection(shared, events.as_ref().map(|(tx, _)| *tx), &id, ble) {
                    Ok(connection) => connection,
                    Err(err) => { log::debug!("bluetooth device status: {err}"); None }
                }
            });
            let connected = connection.as_ref().is_some_and(|c| c.connected());
            // AEP objects have no instance ID; drivers expose battery on nodes in their container.
            let battery = battery(&info).or_else(|| {
                if !connected || !shared.active.load(Ordering::Acquire) { return None; }
                let container = container?;
                let filter = HSTRING::from(format!("System.Devices.ContainerId:=\"{{{container:?}}}\""));
                let properties: windows_collections::IIterable<HSTRING> = vec![HSTRING::from(BATTERY)].into();
                let nodes = DeviceInformation::FindAllAsyncWithKindAqsFilterAndAdditionalProperties(&filter, &properties, DeviceInformationKind::Device).and_then(|op| op.join()).ok()?;
                nodes.into_iter().find_map(|node| battery(&node))
            });
            if let Some(connection) = connection {
                if let Link::Classic(d, _) = &connection.link
                    && d.ClassOfDevice().and_then(|c| c.MajorClass()).ok() == Some(BluetoothMajorClass::AudioVideo) { kind = Kind::Audio; }
                if let Some((_, map)) = &mut events && !map.contains_key(&id) {
                    map.insert(id.clone(), connection.clone());
                }
            }
            snapshot.devices.push(Device { id, name: info.Name().map(|n| n.to_string()).unwrap_or_default(), connected, kind, battery, container });
        }
    }
    if let Some((_, map)) = events {
        map.retain(|id, _| seen.contains(id));
        if let Ok(mut live) = shared.live.lock() { live.connections.retain(|id, _| seen.contains(id)); }
    }
    snapshot.devices = tidy(snapshot.devices);
    Ok(snapshot)
}

fn set_radio(shared: &Shared, on: bool) -> Result<()> {
    if Radio::RequestAccessAsync()?.join()? != RadioAccessStatus::Allowed { return Err(E_FAIL.into()); }
    if !shared.active.load(Ordering::Acquire) { return Ok(()); }
    let radios = radios()?;
    if radios.is_empty() { return Err(E_FAIL.into()); }
    for radio in radios {
        if !shared.active.load(Ordering::Acquire) { return Ok(()); }
        if radio.SetStateAsync(if on { RadioState::On } else { RadioState::Off })?.join()? != RadioAccessStatus::Allowed { return Err(E_FAIL.into()); }
    }
    Ok(())
}

fn endpoint_container(device: &IMMDevice) -> Option<GUID> {
    unsafe { device.OpenPropertyStore(STGM_READ).ok()?.GetValue(&CONTAINER_KEY).ok().and_then(|v| PropVariantToGUID(&v).ok()).filter(|g| valid_container(*g)) }
}

fn audio_containers() -> Vec<GUID> {
    let read = || -> Result<Vec<GUID>> {
        unsafe {
            let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            let endpoints = enumerator.EnumAudioEndpoints(eAll, DEVICE_STATE(DEVICE_STATEMASK_ALL))?;
            Ok((0..endpoints.GetCount()?).filter_map(|i| endpoints.Item(i).ok().and_then(|e| endpoint_container(&e))).collect())
        }
    };
    read().unwrap_or_default()
}

fn audio_controls(enumerator: &IMMDeviceEnumerator, endpoint: &IMMDevice) -> Vec<(String, IKsControl)> {
    let mut controls = Vec::new();
    unsafe {
        let Ok(topology) = endpoint.Activate::<IDeviceTopology>(CLSCTX_ALL, None) else { return controls; };
        for i in 0..topology.GetConnectorCount().unwrap_or(0) {
            let Ok(id) = topology.GetConnector(i).and_then(|c| c.GetDeviceIdConnectedTo()) else { continue; };
            let name = id.to_string();
            let adapter = enumerator.GetDevice(PCWSTR(id.0));
            CoTaskMemFree(Some(id.0.cast()));
            if let (Ok(name), Ok(control)) = (name, adapter.and_then(|a| a.Activate::<IKsControl>(CLSCTX_ALL, None))) { controls.push((name, control)); }
        }
    }
    controls
}

fn audio_property(connected: bool) -> KSIDENTIFIER {
    KSIDENTIFIER { Anonymous: KSIDENTIFIER_0 { Anonymous: KSIDENTIFIER_0_0 {
        Set: KSPROPSETID_BtAudio,
        Id: if connected { KSPROPERTY_ONESHOT_DISCONNECT.0 as u32 } else { KSPROPERTY_ONESHOT_RECONNECT.0 as u32 },
        Flags: 1, // KSPROPERTY_TYPE_GET, including for the one-shot actions.
    } } }
}

fn audio_action(shared: &Shared, device: &Device) -> Result<()> {
    if device.kind != Kind::Audio || device.container.is_none() { return Err(E_FAIL.into()); }
    unsafe {
        let enumerator: IMMDeviceEnumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let endpoints = enumerator.EnumAudioEndpoints(eAll, DEVICE_STATE(DEVICE_STATEMASK_ALL))?;
        let mut succeeded = false;
        let mut failed = None;
        let mut sent = HashSet::new();
        for i in 0..endpoints.GetCount()? {
            if !shared.active.load(Ordering::Acquire) { return Ok(()); }
            let Ok(endpoint) = endpoints.Item(i) else { continue; };
            if !same_container(endpoint_container(&endpoint), device.container) { continue; }
            for (id, control) in audio_controls(&enumerator, &endpoint) {
                if !shared.active.load(Ordering::Acquire) { return Ok(()); }
                if !sent.insert(id) { continue; }
                let property = audio_property(device.connected);
                let mut returned = 0;
                match control.KsProperty(&property, size_of::<KSIDENTIFIER>() as u32, std::ptr::null_mut(), 0, &mut returned) {
                    Ok(()) => succeeded = true,
                    Err(err) => { log::info!("bluetooth audio driver: {err}"); failed = Some(err); }
                }
            }
        }
        if let Some(err) = failed { return Err(err); }
        if succeeded { return Ok(()); }
    }
    Err(E_FAIL.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(id: &str, name: &str, connected: bool, container: Option<GUID>) -> Device {
        Device { id: id.into(), name: name.into(), connected, kind: Kind::Other, battery: None, container }
    }

    #[test]
    fn tidy_merges_transports_by_identity_and_keeps_metadata() {
        let container = Some(GUID::from_u128(1));
        let mut audio = device("classic", "Headphones", true, container);
        audio.kind = Kind::Audio;
        let mut ble = device("ble", "Headphones LE", false, container);
        ble.battery = Some(71);
        let list = tidy(vec![audio, ble, device("blank", "  ", false, None)]);
        assert_eq!(list.len(), 1);
        assert!(list[0].connected);
        assert_eq!(list[0].kind, Kind::Audio);
        assert_eq!(list[0].battery, Some(71));
    }

    #[test]
    fn tidy_never_merges_by_name_or_missing_container_and_sorts() {
        let list = tidy(vec![device("b", "Mouse", false, None), device("a", "Mouse", false, None), device("c", "Zulu", true, None), device("d", "alpha", false, None)]);
        assert_eq!(list.iter().map(|d| d.id.as_str()).collect::<Vec<_>>(), ["c", "d", "a", "b"]);
        assert!(!same_container(Some(GUID::zeroed()), Some(GUID::zeroed())));
    }

    #[test]
    fn sentinel_containers_never_merge_devices_or_match_endpoints() {
        let valid = GUID::from_u128(1);
        assert!(valid_container(valid));
        assert!(same_container(Some(valid), Some(valid)));
        for sentinel in [GUID::zeroed(), SYSTEM_CONTAINER] {
            assert!(!valid_container(sentinel));
            assert!(!same_container(Some(sentinel), Some(sentinel)));
            assert!(!same_container(Some(sentinel), Some(valid)));
            assert!(!same_container(Some(valid), Some(sentinel)));
            let list = tidy(vec![device("a", "Headphones", true, Some(sentinel)), device("b", "Speaker", false, Some(sentinel))]);
            assert_eq!(list.len(), 2);
        }
    }

    #[test]
    fn invalid_battery_is_absent_and_radio_subtitle_counts_connections() {
        let mut d = device("a", "Mouse", true, None);
        d.battery = Some(255);
        let devices = tidy(vec![d]);
        assert_eq!(devices[0].battery, None);
        let mut snapshot = Snapshot { on: true, devices, ..Default::default() };
        assert_eq!(snapshot.subtitle(), "On · 1 connected");
        snapshot.on = false;
        assert_eq!(snapshot.subtitle(), "Off");
    }

    #[test]
    fn stop_revokes_once_and_rejects_late_results_and_callbacks() {
        use std::sync::atomic::AtomicUsize;

        let shared = Shared::new(HWND::default());
        let count = Arc::new(AtomicUsize::new(0));
        let removed = count.clone();
        shared.live.lock().expect("live state").cleanup.push(Box::new(move || { removed.fetch_add(1, Ordering::Relaxed); }));
        shared.stop();
        shared.stop();
        assert_eq!(count.load(Ordering::Relaxed), 1);
        shared.publish(Snapshot { ready: true, available: true, on: true, devices: Vec::new() });
        assert_eq!(shared.snapshot(), Snapshot::default());
        let (tx, rx) = mpsc::channel();
        changed(&shared, &tx);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn event_bursts_queue_one_refresh_until_consumed() {
        let shared = Shared::new(HWND::default());
        let (tx, rx) = mpsc::channel();
        for _ in 0..100 { changed(&shared, &tx); }
        assert!(matches!(rx.try_recv(), Ok(Command::Refresh)));
        assert!(rx.try_recv().is_err());
        shared.pending.store(false, Ordering::Release);
        changed(&shared, &tx);
        assert!(matches!(rx.try_recv(), Ok(Command::Refresh)));
    }

    #[test]
    fn audio_requests_are_one_shot_gets_with_no_value() {
        for connected in [false, true] {
            let property = unsafe { audio_property(connected).Anonymous.Anonymous };
            assert_eq!(property.Set, KSPROPSETID_BtAudio);
            assert_eq!(property.Id, u32::from(connected));
            assert_eq!(property.Flags, 1);
            assert_eq!(size_of::<KSIDENTIFIER>(), 24);
        }
    }
}
