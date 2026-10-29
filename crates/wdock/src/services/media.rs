//! Now playing, from the system media transport controls (the same source as
//! the volume flyout and the lock screen): whichever app owns the current
//! media session, with its title, artist, art, playback state and timeline.
//!
//! WinRT calls block and its events arrive on thread-pool threads, so all of
//! it lives on one worker thread that sleeps on a channel. Session, playback,
//! properties and timeline events only push a message into that channel; the
//! worker then reads a fresh snapshot, keeps it here and posts `WM_APP_MEDIA`
//! to the main window. Nothing polls. Commands from the island (play/pause,
//! skip, seek) travel the other way through the same channel.

use std::sync::atomic::{AtomicIsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session, GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status, MediaPropertiesChangedEventArgs,
    PlaybackInfoChangedEventArgs, TimelinePropertiesChangedEventArgs,
};
use windows::Storage::Streams::{DataReader, IRandomAccessStreamReference};
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppPBGRA, IWICBitmapSource, IWICImagingFactory,
    WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeMedianCut, WICDecodeMetadataCacheOnDemand,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Shell::SHCreateMemStream;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{Interface, Ref, Result, RuntimeType};

/// The media snapshot changed; read it with `current()`.
pub const WM_APP_MEDIA: u32 = WM_APP + 22;
/// Album art is decoded to this square size (pixels), enough for the expanded island.
const ART_PX: u32 = 112;
/// Timeline events within this much of the extrapolated position are ignored.
const POSITION_SLACK_MS: u64 = 1500;

/// Premultiplied BGRA pixels of the current track's cover.
pub struct Art {
    pub size: u32,
    pub pixels: Vec<u8>,
}

#[derive(Clone)]
pub struct Media {
    pub title: String,
    pub artist: String,
    pub album: String,
    pub playing: bool,
    pub can_play_pause: bool,
    pub can_prev: bool,
    pub can_next: bool,
    pub can_seek: bool,
    /// Position when the snapshot was taken (`taken`), and the track length. 0 when unknown.
    pub position_ms: u64,
    pub length_ms: u64,
    pub taken: Instant,
    /// Changes whenever the cover does.
    pub art_id: u64,
    pub art: Option<Arc<Art>>,
}

impl Media {
    /// Playback position now, advancing while playing.
    pub fn position_now(&self) -> u64 {
        let advance = if self.playing { self.taken.elapsed().as_millis() as u64 } else { 0 };
        let position = self.position_ms + advance;
        if self.length_ms > 0 { position.min(self.length_ms) } else { position }
    }

    /// Whether the progress bar has something to show.
    pub fn has_timeline(&self) -> bool {
        self.length_ms > 0
    }
}

enum Event {
    /// Re-read everything (session, playback state or properties changed).
    Refresh,
    /// The timeline changed: refresh only if the position jumped.
    Timeline,
    Command(Command),
    Quit,
}

#[derive(Debug, Clone, Copy)]
pub enum Command {
    PlayPause,
    Previous,
    Next,
    Seek(u64),
}

static TARGET: AtomicIsize = AtomicIsize::new(0);
static SNAPSHOT: Mutex<Option<Media>> = Mutex::new(None);
static SENDER: Mutex<Option<Sender<Event>>> = Mutex::new(None);

/// Starts the worker; `target` receives `WM_APP_MEDIA`. Does nothing if running.
pub fn start(target: HWND) {
    let Ok(mut sender) = SENDER.lock() else {
        return;
    };
    if sender.is_some() {
        return;
    }
    TARGET.store(target.0 as isize, Ordering::Relaxed);
    let (tx, rx) = mpsc::channel();
    let worker_tx = tx.clone();
    match std::thread::Builder::new().name("wdock-media".into()).stack_size(512 * 1024).spawn(move || {
        if let Err(err) = run(&worker_tx, &rx) {
            log::warn!("media session unavailable: {err}");
        }
    }) {
        Ok(_) => *sender = Some(tx),
        Err(err) => log::warn!("could not start the media thread: {err}"),
    }
}

pub fn stop() {
    if let Ok(mut sender) = SENDER.lock()
        && let Some(sender) = sender.take()
    {
        let _ = sender.send(Event::Quit);
    }
    if let Ok(mut snapshot) = SNAPSHOT.lock() {
        *snapshot = None;
    }
}

pub fn current() -> Option<Media> {
    SNAPSHOT.lock().ok()?.clone()
}

pub fn send(command: Command) {
    if let Ok(sender) = SENDER.lock()
        && let Some(sender) = sender.as_ref()
    {
        let _ = sender.send(Event::Command(command));
    }
}

fn post() {
    let target = HWND(TARGET.load(Ordering::Relaxed) as *mut _);
    let _ = unsafe { PostMessageW(Some(target), WM_APP_MEDIA, WPARAM(0), LPARAM(0)) };
}

/// Subscriptions on the current session; dropping them unsubscribes.
struct Subscribed {
    session: Session,
    tokens: [i64; 3],
}

impl Subscribed {
    fn new(session: Session, tx: &Sender<Event>) -> Result<Self> {
        let tokens = [
            session.PlaybackInfoChanged(&notify::<PlaybackInfoChangedEventArgs>(tx, || Event::Refresh))?,
            session.MediaPropertiesChanged(&notify::<MediaPropertiesChangedEventArgs>(tx, || Event::Refresh))?,
            session.TimelinePropertiesChanged(&notify::<TimelinePropertiesChangedEventArgs>(tx, || Event::Timeline))?,
        ];
        Ok(Self { session, tokens })
    }
}

/// A handler that only forwards `event` to the worker.
fn notify<A: RuntimeType + 'static>(tx: &Sender<Event>, event: fn() -> Event) -> TypedEventHandler<Session, A> {
    let tx = tx.clone();
    TypedEventHandler::new(move |_: Ref<'_, Session>, _: Ref<'_, A>| {
        let _ = tx.send(event());
        Ok(())
    })
}

impl Drop for Subscribed {
    fn drop(&mut self) {
        let _ = self.session.RemovePlaybackInfoChanged(self.tokens[0]);
        let _ = self.session.RemoveMediaPropertiesChanged(self.tokens[1]);
        let _ = self.session.RemoveTimelinePropertiesChanged(self.tokens[2]);
    }
}

fn run(tx: &Sender<Event>, rx: &Receiver<Event>) -> Result<()> {
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };
    let result = work(tx, rx);
    unsafe { CoUninitialize() };
    result
}

/// Current cover, kept while the track stays the same.
#[derive(Default)]
struct ArtCache {
    key: String,
    id: u64,
    art: Option<Arc<Art>>,
}

fn work(tx: &Sender<Event>, rx: &Receiver<Event>) -> Result<()> {
    let manager = Manager::RequestAsync()?.join()?;
    let session_tx = tx.clone();
    let session_token = manager.CurrentSessionChanged(&TypedEventHandler::new(move |_, _| {
        let _ = session_tx.send(Event::Refresh);
        Ok(())
    }))?;
    let wic: Option<IWICImagingFactory> = unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
        .inspect_err(|err| log::warn!("WIC unavailable, no album art: {err}"))
        .ok();

    let mut subscribed: Option<Subscribed> = None;
    let mut last: Option<Media> = None;
    let mut cache = ArtCache::default();

    while let Ok(event) = rx.recv() {
        let mut quit = false;
        let mut refresh = false;
        let mut handle = |event: Event, subscribed: &Option<Subscribed>, last: &Option<Media>| match event {
            Event::Quit => quit = true,
            Event::Refresh => refresh = true,
            Event::Command(command) => {
                if let Some(subscribed) = subscribed {
                    execute(&subscribed.session, command);
                }
            }
            Event::Timeline => {
                // Many players report the timeline every second; only a jump (seek) matters.
                let jumped = match (last, subscribed.as_ref().and_then(|s| timeline(&s.session))) {
                    (Some(last), Some((position, length))) => {
                        last.length_ms != length || last.position_now().abs_diff(position) > POSITION_SLACK_MS
                    }
                    _ => true,
                };
                refresh |= jumped;
            }
        };
        handle(event, &subscribed, &last);
        // Coalesce a burst of events into one read.
        while let Ok(extra) = rx.try_recv() {
            handle(extra, &subscribed, &last);
        }
        if quit {
            break;
        }
        if !refresh {
            continue;
        }

        let session = manager.GetCurrentSession().ok();
        if subscribed.as_ref().map(|s| &s.session) != session.as_ref() {
            subscribed = None;
            if let Some(session) = session {
                subscribed = Subscribed::new(session, tx).inspect_err(|err| log::warn!("media events: {err}")).ok();
            }
        }
        let media = subscribed.as_ref().and_then(|s| {
            snapshot(&s.session, wic.as_ref(), &mut cache)
                .inspect_err(|err| log::debug!("media snapshot: {err}"))
                .ok()
        });
        if let Ok(mut slot) = SNAPSHOT.lock() {
            slot.clone_from(&media);
        }
        last = media;
        post();
    }
    let _ = manager.RemoveCurrentSessionChanged(session_token);
    Ok(())
}

fn execute(session: &Session, command: Command) {
    let sent = match command {
        Command::PlayPause => session.TryTogglePlayPauseAsync().map(drop),
        Command::Previous => session.TrySkipPreviousAsync().map(drop),
        Command::Next => session.TrySkipNextAsync().map(drop),
        Command::Seek(ms) => session.TryChangePlaybackPositionAsync(ms as i64 * 10_000).map(drop),
    };
    if let Err(err) = sent {
        log::debug!("media command {command:?}: {err}");
    }
}

/// (position, length) in milliseconds, extrapolated to now.
fn timeline(session: &Session) -> Option<(u64, u64)> {
    let line = session.GetTimelineProperties().ok()?;
    let ms = |ticks: i64| (ticks.max(0) / 10_000) as u64;
    let start = ms(line.StartTime().ok()?.Duration);
    let end = ms(line.EndTime().ok()?.Duration);
    let position = ms(line.Position().ok()?.Duration).saturating_sub(start);
    let length = end.saturating_sub(start);
    // The position was valid at LastUpdatedTime.
    let playing = session.GetPlaybackInfo().ok()?.PlaybackStatus().ok()? == Status::Playing;
    let advance = if playing { elapsed_since(line.LastUpdatedTime().ok()?.UniversalTime) } else { 0 };
    let position = position + advance;
    Some((if length > 0 { position.min(length) } else { position }, length))
}

/// Milliseconds since a WinRT `DateTime` (100 ns ticks since 1601), or 0 if
/// it is absent or implausible.
fn elapsed_since(universal_ticks: i64) -> u64 {
    const EPOCH_DIFF_TICKS: i64 = 116_444_736_000_000_000;
    let Ok(now) = SystemTime::now().duration_since(UNIX_EPOCH) else {
        return 0;
    };
    let now_ticks = (now.as_nanos() / 100) as i64 + EPOCH_DIFF_TICKS;
    let elapsed = (now_ticks - universal_ticks) / 10_000;
    if (0..6 * 3_600_000).contains(&elapsed) { elapsed as u64 } else { 0 }
}

fn snapshot(session: &Session, wic: Option<&IWICImagingFactory>, cache: &mut ArtCache) -> Result<Media> {
    let props = session.TryGetMediaPropertiesAsync()?.join()?;
    let title = props.Title()?.to_string();
    let artist = props.Artist()?.to_string();
    let album = props.AlbumTitle()?.to_string();

    // Fetch the cover only when the track changed.
    let key = format!("{title}\n{artist}\n{album}");
    if cache.key != key {
        cache.key = key;
        cache.id += 1;
        cache.art = props
            .Thumbnail()
            .ok()
            .and_then(|thumb| read_art(&thumb, wic?).inspect_err(|err| log::debug!("album art: {err}")).ok())
            .map(Arc::new);
    }

    let info = session.GetPlaybackInfo()?;
    let controls = info.Controls()?;
    let (position_ms, length_ms) = timeline(session).unwrap_or((0, 0));
    Ok(Media {
        title,
        artist,
        album,
        playing: info.PlaybackStatus()? == Status::Playing,
        can_play_pause: controls.IsPlayPauseToggleEnabled().unwrap_or(false),
        can_prev: controls.IsPreviousEnabled().unwrap_or(false),
        can_next: controls.IsNextEnabled().unwrap_or(false),
        can_seek: controls.IsPlaybackPositionEnabled().unwrap_or(false),
        position_ms,
        length_ms,
        taken: Instant::now(),
        art_id: cache.id,
        art: cache.art.clone(),
    })
}

fn read_art(thumbnail: &IRandomAccessStreamReference, wic: &IWICImagingFactory) -> Result<Art> {
    let stream = thumbnail.OpenReadAsync()?.join()?;
    let size = stream.Size()? as u32;
    if size == 0 || size > 8 * 1024 * 1024 {
        return Err(windows::core::Error::empty());
    }
    let reader = DataReader::CreateDataReader(&stream)?;
    reader.LoadAsync(size)?.join()?;
    let mut bytes = vec![0u8; size as usize];
    reader.ReadBytes(&mut bytes)?;

    unsafe {
        let memory = SHCreateMemStream(Some(&bytes)).ok_or_else(windows::core::Error::empty)?;
        let decoder = wic.CreateDecoderFromStream(&memory, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
        let frame = decoder.GetFrame(0)?;
        let scaler = wic.CreateBitmapScaler()?;
        scaler.Initialize(&frame.cast::<IWICBitmapSource>()?, ART_PX, ART_PX, WICBitmapInterpolationModeFant)?;
        let converter = wic.CreateFormatConverter()?;
        converter.Initialize(
            &scaler,
            &GUID_WICPixelFormat32bppPBGRA,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeMedianCut,
        )?;
        let mut pixels = vec![0u8; (ART_PX * ART_PX * 4) as usize];
        converter.CopyPixels(std::ptr::null(), ART_PX * 4, &mut pixels)?;
        Ok(Art { size: ART_PX, pixels })
    }
}
