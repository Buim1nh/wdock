//! Clipboard notifications wake one bounded read; no clipboard polling.

use std::cell::RefCell;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use windows::Win32::Foundation::{E_INVALIDARG, GlobalFree, HANDLE, HGLOBAL, HWND, LPARAM, WPARAM};
use windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
    GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, RemoveClipboardFormatListener, SetClipboardData,
};
use windows::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock, GMEM_MOVEABLE, GMEM_ZEROINIT};
use windows::Win32::System::Ole::CF_UNICODETEXT;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{Error, Result, w};

use crate::model::clipboard::{self, MAX_ENTRY_BYTES};

pub const WM_APP_CLIPBOARD: u32 = WM_APP + 27;
const MAX_READ_BYTES: usize = (MAX_ENTRY_BYTES + 1) * 2 + 16;

#[derive(Clone, Copy)]
struct Formats {
    exclude: u32,
    allowed: [u32; 4],
}

impl Formats {
    fn register() -> Option<Self> {
        let formats = unsafe {
            Self {
                exclude: RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")),
                allowed: [
                    RegisterClipboardFormatW(w!("CanIncludeInClipboardHistory")),
                    RegisterClipboardFormatW(w!("CanUploadToCloudClipboard")),
                    RegisterClipboardFormatW(w!("ClipboardHistoryAllowed")),
                    RegisterClipboardFormatW(w!("CanUploadToCloud")),
                ],
            }
        };
        (formats.exclude != 0 && formats.allowed.iter().all(|format| *format != 0)).then_some(formats)
    }

    fn excluded(self) -> bool {
        if unsafe { IsClipboardFormatAvailable(self.exclude) }.is_ok() {
            return true;
        }
        self.allowed.into_iter().any(|format| {
            if unsafe { IsClipboardFormatAvailable(format) }.is_err() {
                return false;
            }
            // Malformed privacy metadata is treated as an exclusion too.
            read_dword(format).is_none_or(|value| value == 0)
        })
    }
}

#[derive(Default)]
struct Listener {
    listening: bool,
    suspended: bool,
    formats: Option<Formats>,
    sequence: u32,
    error: Option<&'static str>,
}

thread_local! {
    static LISTENER: RefCell<Listener> = RefCell::default();
}

struct Read {
    start: u32,
    end: u32,
    text: Option<String>,
}

struct Completed {
    generation: u64,
    requested: u32,
    read: Option<Read>,
}

static GENERATION: AtomicU64 = AtomicU64::new(1);
static OWN_SEQUENCE: AtomicU32 = AtomicU32::new(0);
static IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static PASTE_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
static CAPTURED: Mutex<Option<Completed>> = Mutex::new(None);

pub fn error() -> Option<&'static str> {
    LISTENER.with_borrow(|listener| listener.error)
}

fn discard_pending() {
    GENERATION.fetch_add(1, Ordering::AcqRel);
    CAPTURED.lock().unwrap_or_else(|err| err.into_inner()).take();
}

pub fn skip_current() {
    discard_pending();
    LISTENER.with_borrow_mut(|listener| listener.sequence = unsafe { GetClipboardSequenceNumber() });
}

/// Disabling capture also forgets pins; nothing is serialized to disk.
pub fn configure(hwnd: HWND, enabled: bool, suspended: bool) -> bool {
    let changed = clipboard::configure(enabled);
    LISTENER.with_borrow_mut(|listener| {
        if listener.suspended != suspended || changed {
            discard_pending();
        }
        listener.suspended = suspended;
        if !enabled {
            listener.error = None;
        }
        let listen = enabled && !suspended;
        if listen == listener.listening {
            return;
        }
        if listen {
            let Some(formats) = Formats::register() else {
                listener.error = Some("Clipboard capture unavailable: privacy format registration failed.");
                log::warn!("clipboard privacy format registration failed; capture stays off");
                return;
            };
            listener.formats = Some(formats);
            listener.sequence = unsafe { GetClipboardSequenceNumber() };
            match unsafe { AddClipboardFormatListener(hwnd) } {
                Ok(()) => {
                    listener.listening = true;
                    listener.error = None;
                }
                Err(err) => {
                    listener.error = Some("Clipboard capture unavailable: Windows could not register the listener.");
                    log::warn!("clipboard listener unavailable: {err}");
                }
            }
        } else {
            let _ = unsafe { RemoveClipboardFormatListener(hwnd) };
            listener.listening = false;
            discard_pending();
        }
    });
    changed
}

pub fn suspend(hwnd: HWND, suspended: bool) {
    configure(hwnd, clipboard::enabled(), suspended);
}

pub fn stop(hwnd: HWND) {
    configure(hwnd, false, true);
    discard_pending();
}

/// A delayed-render clipboard owner may block GetClipboardData, so that call
/// never runs on the shell thread. A stuck owner cannot spawn more workers.
pub fn request_capture(hwnd: HWND) {
    if !clipboard::enabled() || clipboard::paused() {
        return;
    }
    let formats = LISTENER.with_borrow_mut(|listener| {
        if !listener.listening || listener.suspended {
            return None;
        }
        let sequence = unsafe { GetClipboardSequenceNumber() };
        if sequence == listener.sequence || sequence == OWN_SEQUENCE.load(Ordering::Acquire) {
            return None;
        }
        let formats = listener.formats?;
        if IN_FLIGHT.compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed).is_err() {
            return None;
        }
        listener.sequence = sequence;
        Some((formats, sequence))
    });
    let Some((formats, requested)) = formats else { return };
    let generation = GENERATION.load(Ordering::Acquire);
    let target = hwnd.0 as isize;
    let result = std::thread::Builder::new().name("clipboard-read".into()).spawn(move || {
        let mut read = read_text(Some(formats), true);
        if generation != GENERATION.load(Ordering::Acquire)
            && let Some(read) = &mut read
        {
            read.text = None;
        }
        {
            let mut captured = CAPTURED.lock().unwrap_or_else(|err| err.into_inner());
            *captured = Some(Completed { generation, requested, read });
        }
        IN_FLIGHT.store(false, Ordering::Release);
        // Completion checks for a newer notification coalesced during this read.
        // An unchanged or busy clipboard is never retried.
        let _ = unsafe {
            PostMessageW(Some(HWND(target as *mut _)), WM_APP_CLIPBOARD, WPARAM(0), LPARAM(0))
        };
    });
    if result.is_err() {
        IN_FLIGHT.store(false, Ordering::Release);
    }
}

pub fn receive(hwnd: HWND) -> bool {
    let Some(completed) = CAPTURED.lock().unwrap_or_else(|err| err.into_inner()).take() else {
        return false;
    };
    let stale = completed.generation != GENERATION.load(Ordering::Acquire);
    let mut changed = false;
    if let Some(read) = completed.read {
        // Delayed rendering itself increments the sequence. Forget that
        // old render without swallowing a later copy from another app.
        LISTENER.with_borrow_mut(|listener| {
            listener.sequence = rendered_sequence(listener.sequence, completed.requested, read.start, read.end);
        });
        if !stale && read.end != OWN_SEQUENCE.load(Ordering::Acquire)
            && LISTENER.with_borrow(|listener| listener.listening && !listener.suspended)
            && let Some(text) = read.text
        {
            changed = clipboard::insert(text);
        }
    }
    request_capture(hwnd);
    changed
}

fn rendered_sequence(baseline: u32, requested: u32, start: u32, end: u32) -> u32 {
    if baseline == requested && start == requested { end } else { baseline }
}

struct Open;

impl Open {
    fn new(hwnd: Option<HWND>) -> Result<Self> {
        unsafe { OpenClipboard(hwnd)? };
        Ok(Self)
    }
}

impl Drop for Open {
    fn drop(&mut self) {
        let _ = unsafe { CloseClipboard() };
    }
}

struct Locked {
    handle: HGLOBAL,
    ptr: *mut core::ffi::c_void,
}

impl Locked {
    fn new(handle: HGLOBAL) -> Option<Self> {
        let ptr = unsafe { GlobalLock(handle) };
        (!ptr.is_null()).then_some(Self { handle, ptr })
    }
}

impl Drop for Locked {
    fn drop(&mut self) {
        let _ = unsafe { GlobalUnlock(self.handle) };
    }
}

fn read_dword(format: u32) -> Option<u32> {
    let handle = HGLOBAL(unsafe { GetClipboardData(format) }.ok()?.0);
    let size = unsafe { GlobalSize(handle) };
    if !(4..=64).contains(&size) {
        return None;
    }
    let locked = Locked::new(handle)?;
    Some(unsafe { (locked.ptr as *const u32).read_unaligned() })
}

fn decode_text(units: &[u16]) -> Option<String> {
    let end = units.iter().position(|unit| *unit == 0)?;
    let text = String::from_utf16(&units[..end]).ok()?;
    (!text.is_empty() && text.len() <= MAX_ENTRY_BYTES).then_some(text)
}

fn read_text(formats: Option<Formats>, skip_own: bool) -> Option<Read> {
    // Busy clipboard owners are skipped, with no retry or blocking wait.
    let _open = Open::new(None).ok()?;
    let start = unsafe { GetClipboardSequenceNumber() };
    let text = (|| {
        if (skip_own && start == OWN_SEQUENCE.load(Ordering::Acquire))
            || formats.is_some_and(Formats::excluded)
        {
            return None;
        }
        let text = read_unicode()?;
        if formats.is_some_and(Formats::excluded) {
            return None;
        }
        Some(text)
    })();
    let end = unsafe { GetClipboardSequenceNumber() };
    Some(Read { start, end, text })
}

fn read_unicode() -> Option<String> {
    if unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT.0.into()) }.is_err() {
        return None;
    }
    let handle = HGLOBAL(unsafe { GetClipboardData(CF_UNICODETEXT.0.into()) }.ok()?.0);
    let size = unsafe { GlobalSize(handle) };
    if !(2..=MAX_READ_BYTES).contains(&size) || !size.is_multiple_of(2) {
        return None;
    }
    let locked = Locked::new(handle)?;
    let units = unsafe { std::slice::from_raw_parts(locked.ptr as *const u16, size / 2) };
    decode_text(units)
}

/// Explicit paste works with history disabled and never waits on the UI.
pub fn read_text_async(done: impl FnOnce(Option<String>) + Send + 'static) -> bool {
    if PASTE_IN_FLIGHT.compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed).is_err() {
        return false;
    }
    let result = std::thread::Builder::new().name("clipboard-paste".into()).spawn(move || {
        let text = read_text(None, false).and_then(|read| read.text);
        PASTE_IN_FLIGHT.store(false, Ordering::Release);
        done(text);
    });
    if result.is_err() {
        PASTE_IN_FLIGHT.store(false, Ordering::Release);
        return false;
    }
    true
}

struct Allocation(HGLOBAL);

impl Drop for Allocation {
    fn drop(&mut self) {
        if !self.0.is_invalid() {
            let _ = unsafe { GlobalFree(Some(self.0)) };
        }
    }
}

pub fn write_text(hwnd: HWND, text: &str) -> Result<()> {
    if hwnd.is_invalid() || text.len() > MAX_ENTRY_BYTES || text.contains('\0') {
        return Err(Error::from_hresult(E_INVALIDARG));
    }
    let units: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
    let mut memory = Allocation(unsafe { GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, units.len() * 2)? });
    {
        let locked = Locked::new(memory.0).ok_or_else(Error::from_thread)?;
        unsafe { std::ptr::copy_nonoverlapping(units.as_ptr(), locked.ptr as *mut u16, units.len()) };
    }
    let open = Open::new(Some(hwnd))?;
    unsafe {
        EmptyClipboard()?;
        SetClipboardData(CF_UNICODETEXT.0.into(), Some(HANDLE(memory.0.0)))?;
    }
    memory.0 = HGLOBAL::default();
    OWN_SEQUENCE.store(unsafe { GetClipboardSequenceNumber() }, Ordering::Release);
    drop(open);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_text_requires_terminator_and_valid_utf16() {
        assert_eq!(decode_text(&[65, 0, 66]), Some("A".into()));
        assert_eq!(decode_text(&[0]), None);
        assert_eq!(decode_text(&[65]), None);
        assert_eq!(decode_text(&[0xd800, 0]), None);
        let units: Vec<_> = "Tiếng Việt 🦀".encode_utf16().chain([0]).collect();
        assert_eq!(decode_text(&units).as_deref(), Some("Tiếng Việt 🦀"));
    }

    #[test]
    fn decoded_utf8_cannot_exceed_history_entry_limit() {
        let units: Vec<_> = "🦀".repeat(MAX_ENTRY_BYTES / 4 + 1).encode_utf16().chain([0]).collect();
        assert!(decode_text(&units).is_none());
    }

    #[test]
    fn delayed_render_after_clear_skips_old_text_but_keeps_new_copies() {
        // Clear recorded sequence 10 while its delayed render was outstanding.
        assert_eq!(rendered_sequence(10, 10, 10, 11), 11);
        // A real copy to 12 after rendering is still different from that 11.
        assert_ne!(rendered_sequence(10, 10, 10, 11), 12);
        // The worker opened only after a new copy; do not suppress that copy.
        assert_eq!(rendered_sequence(10, 10, 12, 12), 10);
        // A newer request or clear baseline must never move backwards.
        assert_eq!(rendered_sequence(12, 10, 10, 11), 12);
    }
}
