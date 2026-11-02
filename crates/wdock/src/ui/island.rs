//! Dynamic Island: a black pill at the top centre of a monitor, sitting on
//! the top bar. Idle it is a small capsule; with music playing it shows the
//! cover and an equalizer, with a Pomodoro running the time left. Pointing at
//! it expands it into a panel with the media controls and the timer, and
//! system changes (volume, brightness, charging, network, audio device) make
//! it briefly announce them.
//!
//! The window is resized with its content (see `View::frame`), so the empty
//! area around a small pill never intercepts clicks. Frames are drawn only
//! while something moves: the size animation, the equalizer, and one-shot
//! ticks for the progress bar and the countdown.

use std::cell::RefCell;
use std::mem::ManuallyDrop;
use std::sync::OnceLock;
use std::time::Instant;

use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Direct2D::Common::{
    D2D_RECT_F, D2D_SIZE_U, D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT,
};
use windows::Win32::Graphics::Direct2D::{
    D2D1_BITMAP_OPTIONS_NONE, D2D1_BITMAP_PROPERTIES1, D2D1_BRUSH_PROPERTIES, D2D1_ELLIPSE, ID2D1Bitmap1,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER,
    IDWriteTextFormat,
};
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM;
use windows::Win32::System::Diagnostics::Debug::MessageBeep;
use windows::Win32::UI::Input::KeyboardAndMouse::{GetCapture, ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, HWND_TOPMOST, KillTimer, MB_ICONASTERISK, SW_HIDE, SW_SHOWNOACTIVATE,
    SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetTimer, SetWindowPos, ShowWindow,
};
use windows_numerics::Matrix3x2;

use crate::app;
use crate::model::config::{IslandConfig, IslandStyle};
use crate::model::pomodoro::{self, Phase, Pomodoro};
use crate::model::island_files;
use crate::platform::island_drag;
use crate::platform::monitors::Monitor;
use crate::services::media::{self, Command, Media};
use crate::services::privacy::{self, Sensors};
use crate::services::{self, NetState, SystemStatus};
use crate::ui::backdrop::Corners;
use crate::ui::bar::{self, BarSpec, Button, PaintCx, View};
use crate::ui::eq_layer::EqLayer;
use crate::ui::gfx::{Gfx, rect, rgba, rounded};
use crate::ui::popup;
use crate::platform::wininfo;
use crate::ui::text;
use crate::ui::theme::Theme;
use crate::ui::thumbs::Thumbnail;

use std::rc::Rc;
use windows::core::Result;

const TIMER_ANIMATE: usize = 1;
const TIMER_NOTICE: usize = 2;
const TIMER_TICK: usize = 3;
const TIMER_EQ: usize = 4;
const TIMER_LEAVE: usize = 5;
const TIMER_LYRICS_MOTION: usize = 6;
const TIMER_PIN_TOP: usize = 7;
/// Posted to a view with `bar::command` when a Pomodoro phase ended.
const CMD_WORK_DONE: usize = 1;
const CMD_BREAK_DONE: usize = 2;
const CMD_REFRESH_APPEARANCE: usize = 3;
const CMD_LYRICS_CHANGED: usize = 4;

const FRAME_MS: u32 = 15;
const EQ_MS: u32 = 150;
const NOTICE_MS: u32 = 2000;
const LEAVE_DELAY_MS: u32 = 200;
const HOVER_MARGIN: f32 = 8.0;
/// Fraction of the remaining distance covered per animation frame.
const EASE: f32 = 0.28;

const IDLE: (f32, f32) = (92.0, 26.0);
const WIDTH_MEDIA: f32 = 24.0;
const WIDTH_TIMER: f32 = 64.0;
/// Extra pill width per active privacy dot (microphone, camera).
const WIDTH_DOT: f32 = 14.0;
const NOTICE: (f32, f32) = (280.0, 46.0);
const EXPANDED: (f32, f32) = (680.0, 240.0);
const EXPANDED_RADIUS: f32 = 24.0;

const WORK_COLOR: D2D1_COLOR_F = rgba(255, 159, 10, 1.0);
const BREAK_COLOR: D2D1_COLOR_F = rgba(48, 209, 88, 1.0);

const GLYPH_PREV: &str = "\u{E892}";
const GLYPH_NEXT: &str = "\u{E893}";
const GLYPH_PLAY: &str = "\u{E768}";
const GLYPH_PAUSE: &str = "\u{E769}";
const GLYPH_MUSIC: &str = "\u{E8D6}";
const GLYPH_TIMER: &str = "\u{E916}";
const GLYPH_RESET: &str = "\u{E777}";
const GLYPH_HEADPHONES: &str = "\u{E7F6}";
const GLYPH_MIC: &str = "\u{E720}";
const GLYPH_CAMERA: &str = "\u{E722}";

const MIC_COLOR: D2D1_COLOR_F = rgba(255, 159, 10, 1.0);
const CAMERA_COLOR: D2D1_COLOR_F = rgba(48, 209, 88, 1.0);

// ---- shared state: the Pomodoro and the island windows ----

struct Shared {
    pomodoro: Pomodoro,
    lengths: (u32, u32),
    views: Vec<HWND>,
}

thread_local! {
    static SHARED: RefCell<Option<Shared>> = const { RefCell::new(None) };
}

/// Milliseconds on a monotonic clock, the Pomodoro's time base.
fn now_ms() -> u64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_millis() as u64
}

fn shared<R>(f: impl FnOnce(&mut Shared) -> R) -> Option<R> {
    SHARED.with(|s| s.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Applies the configured Pomodoro lengths. A running timer survives bar
/// rebuilds; changed lengths start over.
pub fn configure(config: &IslandConfig) {
    let lengths = (config.pomodoro_work, config.pomodoro_break);
    SHARED.with_borrow_mut(|shared| match shared {
        Some(shared) if shared.lengths == lengths => {}
        Some(shared) => {
            shared.pomodoro = Pomodoro::new(lengths.0, lengths.1);
            shared.lengths = lengths;
        }
        None => {
            // The lengths picked on the island win over the config defaults.
            let saved = std::fs::read_to_string(saved_path()).map_or(lengths, |text| pomodoro::parse_saved(&text, lengths));
            *shared = Some(Shared { pomodoro: Pomodoro::new(saved.0, saved.1), lengths, views: Vec::new() });
        }
    });
}

fn saved_path() -> std::path::PathBuf {
    crate::paths::local_dir().join("pomodoro.txt")
}

fn pomodoro_state() -> Pomodoro {
    shared(|s| s.pomodoro.clone()).unwrap_or_else(|| Pomodoro::new(25, 5))
}

pub(crate) fn notify_views() {
    let views = shared(|s| s.views.clone()).unwrap_or_default();
    for hwnd in views {
        bar::notify_model_changed(hwnd);
    }
}

/// Refreshes appearance and tab order without disturbing AppBars.
pub fn refresh_appearance() {
    let views = shared(|s| s.views.clone()).unwrap_or_default();
    for hwnd in views {
        bar::command(hwnd, CMD_REFRESH_APPEARANCE);
    }
}

pub fn lyrics_changed() {
    let views = shared(|s| s.views.clone()).unwrap_or_default();
    for hwnd in views { bar::post_command(hwnd, CMD_LYRICS_CHANGED); }
}

/// Re-arms the main window's one-shot timer for the end of the running phase.
fn reschedule_pomodoro() {
    let wake = shared(|s| s.pomodoro.next_wake()).flatten();
    app::schedule_pomodoro(wake.map(|at| at.saturating_sub(now_ms()).min(u64::from(u32::MAX - 1)) as u32 + 20));
}

fn pomodoro_act(act: impl FnOnce(&mut Pomodoro, u64)) {
    let changed = shared(|s| {
        let before = s.pomodoro.lengths();
        act(&mut s.pomodoro, now_ms());
        let after = s.pomodoro.lengths();
        (before != after).then_some(after)
    })
    .flatten();
    if let Some(lengths) = changed {
        let path = saved_path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Err(err) = std::fs::write(&path, pomodoro::format_saved(lengths)) {
            log::warn!("cannot save pomodoro lengths: {err}");
        }
    }
    reschedule_pomodoro();
    notify_views();
}

/// The main window's Pomodoro timer fired.
pub fn pomodoro_due() {
    let finished = shared(|s| s.pomodoro.tick(now_ms())).flatten();
    reschedule_pomodoro();
    let Some(phase) = finished else {
        return;
    };
    log::info!("pomodoro: {phase:?} finished");
    if !popup::game_mode() {
        let _ = unsafe { MessageBeep(MB_ICONASTERISK) };
    }
    let views = shared(|s| s.views.clone()).unwrap_or_default();
    for hwnd in views {
        bar::command(hwnd, if phase == Phase::Work { CMD_WORK_DONE } else { CMD_BREAK_DONE });
    }
    notify_views();
}

/// The media snapshot changed (`WM_APP_MEDIA`).
pub fn media_changed() {
    notify_views();
}

pub fn shutdown() {
    SHARED.set(None);
}

// ---- pure layout and notice logic ----

/// What the pill shows when nothing is pointing at it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Content {
    Idle,
    Media { timer: bool },
    Timer,
}

fn dots_width(sensors: Sensors) -> f32 {
    WIDTH_DOT * (u8::from(sensors.mic) + u8::from(sensors.camera)) as f32
}

fn compact_size(content: Content) -> (f32, f32) {
    let extra = match content {
        Content::Idle => 0.0,
        Content::Media { timer: false } => WIDTH_MEDIA,
        Content::Timer => WIDTH_TIMER,
        Content::Media { timer: true } => WIDTH_MEDIA + WIDTH_TIMER,
    };
    (IDLE.0 + extra, IDLE.1)
}

/// The window rectangle for a pill of `size` pixels, hanging from the top of
/// `monitor` and centred on it.
fn frame_rect(monitor: RECT, size: (f32, f32), scale: f32, bar_height: f32) -> RECT {
    let centre = (monitor.left + monitor.right) as f32 / 2.0;
    let (w, h) = (size.0.round() as i32, size.1.round() as i32);
    let left = (centre - w as f32 / 2.0).round() as i32;
    // Centre the compact pill in the bar; expanded panels keep the same top.
    let bar_px = (bar_height * scale).round();
    let content_px = crate::ui::topbar::content_height(bar_px, scale);
    let compact_px = (IDLE.1.min(bar_height.max(0.0)) * scale).round().min(content_px);
    // If the remaining space is odd, put the extra pixel below the pill.
    let top = monitor.top + ((content_px - compact_px).max(0.0) / 2.0).floor() as i32;
    RECT { left, top, right: left + w, bottom: top + h }
}

/// Keep the panel open while crossing its growing footprint and nearby edges.
fn in_hover_area(monitor: RECT, scale: f32, bar_height: f32, point: POINT) -> bool {
    let bounds = frame_rect(monitor, (EXPANDED.0 * scale, EXPANDED.1 * scale), scale, bar_height);
    let margin = (HOVER_MARGIN * scale).ceil() as i32;
    point.x >= bounds.left - margin && point.x < bounds.right + margin
        && point.y >= bounds.top - margin && point.y < bounds.bottom + margin
}

/// Moves `current` towards `target`, snapping when close. Returns whether it arrived.
fn approach(current: &mut f32, target: f32) -> bool {
    *current += (target - *current) * EASE;
    if (target - *current).abs() < 0.5 {
        *current = target;
    }
    *current == target
}

/// How far a pill of width `w` has grown from the idle capsule to the
/// expanded panel, 0.0 ..= 1.0.
#[cfg(test)]
fn expansion(w: f32, scale: f32) -> f32 {
    expansion_from(w, IDLE.0, scale)
}

/// How far a pill `w` pixels wide is from its compact width `compact` (DIPs)
/// to fully expanded: 0 at rest, 1 open.
fn expansion_from(w: f32, compact: f32, scale: f32) -> f32 {
    // `w` is the integer client width; use the same pixel rounding as the
    // window frame so fractional-DPI compact widths really reach zero.
    let compact_px = (compact * scale).round();
    let span = ((EXPANDED.0 * scale).round() - compact_px).max(1.0);
    ((w - compact_px) / span).clamp(0.0, 1.0)
}

fn ramp(value: f32, from: f32, to: f32) -> f32 {
    ((value - from) / (to - from)).clamp(0.0, 1.0)
}

/// Rectangles of the expanded panel, in pixels.
#[derive(Default, Clone, Copy)]
struct PanelLayout {
    art: D2D_RECT_F,
    title: D2D_RECT_F,
    artist: D2D_RECT_F,
    bar: D2D_RECT_F,
    elapsed: D2D_RECT_F,
    length: D2D_RECT_F,
    prev: D2D_RECT_F,
    play: D2D_RECT_F,
    next: D2D_RECT_F,
    phase: D2D_RECT_F,
    dec: D2D_RECT_F,
    inc: D2D_RECT_F,
    time: D2D_RECT_F,
    toggle: D2D_RECT_F,
    reset: D2D_RECT_F,
}

impl PanelLayout {
    fn new(s: f32) -> Self {
        let r = |l: f32, t: f32, rr: f32, b: f32| rect(l * s, (t + 36.0) * s, rr * s, (b + 36.0) * s);
        Self {
            art: r(16.0, 20.0, 106.0, 110.0),
            title: r(118.0, 22.0, 380.0, 46.0),
            artist: r(118.0, 46.0, 380.0, 66.0),
            bar: r(156.0, 76.0, 380.0, 80.0),
            elapsed: r(156.0, 82.0, 220.0, 98.0),
            length: r(310.0, 82.0, 380.0, 98.0),
            prev: r(132.0, 116.0, 170.0, 150.0),
            play: r(181.0, 116.0, 219.0, 150.0),
            next: r(230.0, 116.0, 268.0, 150.0),
            phase: r(28.0, 158.0, 106.0, 192.0),
            dec: r(133.0, 158.0, 165.0, 192.0),
            time: r(166.0, 158.0, 224.0, 192.0),
            inc: r(225.0, 158.0, 257.0, 192.0),
            toggle: r(298.0, 158.0, 332.0, 192.0),
            reset: r(338.0, 158.0, 372.0, 192.0),
        }
    }
}

fn contains(r: D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

/// Trackpads can deliver fractional wheel steps across several messages.
fn wheel_step(remainder: &mut i32, delta: i16) -> i32 {
    *remainder += i32::from(delta);
    let steps = *remainder / 120;
    *remainder %= 120;
    steps
}

fn tab_rect(index: usize, scale: f32) -> D2D_RECT_F {
    let left = (16.0 + index as f32 * 92.0) * scale;
    rect(left, 9.0 * scale, left + 86.0 * scale, 33.0 * scale)
}

fn file_rect(index: usize, scale: f32) -> D2D_RECT_F {
    let top = (43.0 + index as f32 * 35.0) * scale;
    rect(16.0 * scale, top, (EXPANDED.0 - 16.0) * scale, top + 32.0 * scale)
}

fn files_action_rect(index: usize, scale: f32) -> D2D_RECT_F {
    let left = if index == 0 { 16.0 } else { 346.0 };
    rect(left * scale, 204.0 * scale, (left + 318.0) * scale, 232.0 * scale)
}

/// A short announcement.
#[derive(Debug, Clone, PartialEq)]
struct Notice {
    glyph: &'static str,
    text: String,
    /// Level shown as a bar, 0.0 ..= 1.0.
    level: Option<f32>,
    visual: NoticeVisual,
}

#[derive(Debug, Clone, PartialEq)]
enum NoticeVisual {
    Standard,
    Power(crate::services::Battery),
    Network { connected: bool, detail: String },
}

const BATTERY_LOW: u8 = 20;

/// The most relevant change from `prev` to `next`, if any is worth announcing.
fn notice_for(prev: &SystemStatus, next: &SystemStatus) -> Option<Notice> {
    use crate::ui::glyphs;
    if prev.audio_device != next.audio_device {
        return Some(Notice { glyph: GLYPH_HEADPHONES, text: "Audio output changed".into(), level: None, visual: NoticeVisual::Standard });
    }
    if let (Some(a), Some(b)) = (prev.brightness, next.brightness)
        && a != b
    {
        return Some(Notice { glyph: glyphs::BRIGHTNESS, text: format!("Brightness {b}%"), level: Some(f32::from(b) / 100.0), visual: NoticeVisual::Standard });
    }
    if let (Some(a), Some(b)) = (prev.battery, next.battery) {
        if a.plugged != b.plugged {
            let text = if b.plugged { format!("Charging {}%", b.percent) } else { format!("On battery {}%", b.percent) };
            return Some(Notice { glyph: glyphs::CHARGING, text, level: Some(f32::from(b.percent) / 100.0), visual: NoticeVisual::Power(b) });
        }
        if !b.plugged && a.percent > BATTERY_LOW && b.percent <= BATTERY_LOW {
            return Some(Notice {
                glyph: glyphs::CHARGING,
                text: format!("Battery low {}%", b.percent),
                level: Some(f32::from(b.percent) / 100.0),
                visual: NoticeVisual::Power(b),
            });
        }
        if b.plugged && a.percent < 100 && b.percent == 100 {
            return Some(Notice { glyph: glyphs::CHARGING, text: "Battery full".into(), level: Some(1.0), visual: NoticeVisual::Power(b) });
        }
    }
    if network_kind(&prev.network) != network_kind(&next.network) {
        let (glyph, text, connected, detail) = match &next.network {
            NetState::Wifi { ssid, .. } => (glyphs::WIFI, format!("Wi-Fi {ssid}"), true, ssid.clone()),
            NetState::Wired => (glyphs::ETHERNET, "Ethernet connected".to_owned(), true, "Ethernet".to_owned()),
            NetState::Offline => (glyphs::WIFI, "Offline".to_owned(), false, "No connection".to_owned()),
        };
        return Some(Notice { glyph, text, level: None, visual: NoticeVisual::Network { connected, detail } });
    }
    None
}

/// Announces a sensor that just came into use.
fn privacy_notice(prev: Sensors, next: Sensors) -> Option<Notice> {
    match (next.mic && !prev.mic, next.camera && !prev.camera) {
        (_, true) => Some(Notice { glyph: GLYPH_CAMERA, text: "Camera in use".into(), level: None, visual: NoticeVisual::Standard }),
        (true, false) => Some(Notice { glyph: GLYPH_MIC, text: "Microphone in use".into(), level: None, visual: NoticeVisual::Standard }),
        _ => None,
    }
}

/// Network identity without the signal strength, which changes constantly.
fn network_kind(state: &NetState) -> (u8, &str) {
    match state {
        NetState::Offline => (0, ""),
        NetState::Wired => (1, ""),
        NetState::Wifi { ssid, .. } => (2, ssid),
    }
}

/// Equalizer bar heights (0.2 ..= 1.0) for animation step `step`.
fn equalizer(step: u32) -> [f32; 4] {
    let mut out = [0.0; 4];
    for (i, h) in out.iter_mut().enumerate() {
        // A cheap deterministic scramble: no two bars move alike.
        let v = (step.wrapping_mul(2654435761).wrapping_add(i as u32 * 40503) >> 16) & 0xFF;
        *h = 0.2 + 0.8 * v as f32 / 255.0;
    }
    out
}

// ---- the view ----

struct Formats {
    icon: IDWriteTextFormat,
    icon_small: IDWriteTextFormat,
    title: IDWriteTextFormat,
    artist: IDWriteTextFormat,
    tab: IDWriteTextFormat,
    small: IDWriteTextFormat,
    small_center: IDWriteTextFormat,
    small_right: IDWriteTextFormat,
    time: IDWriteTextFormat,
    compact: IDWriteTextFormat,
    lyric: IDWriteTextFormat,
    lyric_active: IDWriteTextFormat,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hot {
    PinLyrics,
    Prev,
    Play,
    Next,
    Seek,
    Toggle,
    Reset,
    Label,
    Dec,
    Inc,
    Tab(usize),
    File(usize),
    RemoveFile(usize),
    Bookmarks,
    Clipboard,
}

#[derive(Default)]
struct LyricMotion {
    track: String,
    line: String,
    first: String,
    next: String,
    progress: Option<f32>,
    offset: f32,
    velocity: f32,
    frame: Option<Instant>,
}

impl LyricMotion {
    fn update(&mut self, track: &str, live: &crate::services::lyrics::LiveLyrics, playing: bool) -> Option<f32> {
        let now = Instant::now();
        let dt = self.frame.replace(now).map_or(0.0, |previous| now.duration_since(previous).as_secs_f32().min(0.1));
        let start = live.active.saturating_sub(2);
        let first = live.lines.get(start).map_or("", String::as_str);
        let next = live.lines.get(start + 1).map_or("", String::as_str);
        let line = live.lines.get(live.active).map_or("", String::as_str);
        if self.track != track {
            self.offset = 0.0;
            self.velocity = 0.0;
            self.progress = live.progress;
        } else {
            if first != self.first && first == self.next && !first.is_empty() {
                self.offset = 29.0;
                self.velocity = 0.0;
            }
            if self.line != line { self.progress = live.progress; }
            else if let (Some(current), Some(target)) = (self.progress, live.progress) {
                // Ease between bridge samples; seek backwards and large jumps immediately.
                self.progress = Some(if !playing || target < current - 0.5 || (target - current).abs() > 8.0 {
                    target
                } else { current + (target - current) * (1.0 - (-dt * 18.0).exp()) });
            } else { self.progress = live.progress; }
        }
        // Critically damped spring: smooth arrival without oscillation.
        let omega = 18.0;
        let decay = (-omega * dt).exp();
        let velocity = self.velocity + omega * self.offset;
        self.offset = (self.offset + velocity * dt) * decay;
        self.velocity = (self.velocity - omega * velocity * dt) * decay;
        self.track = track.to_owned(); self.line = line.to_owned();
        self.first = first.to_owned(); self.next = next.to_owned();
        self.progress
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tab { Media, Files, Lyrics }

fn configured_tabs(config: &IslandConfig) -> Vec<Tab> {
    let enabled = |tab| match tab { Tab::Media | Tab::Lyrics => true, Tab::Files => config.file_shelf };
    let mut tabs = Vec::new();
    for name in &config.widget_order {
        let tab = match name.as_str() { "media" => Tab::Media, "files" => Tab::Files, _ => continue };
        if enabled(tab) && !tabs.contains(&tab) { tabs.push(tab); }
    }
    for tab in [Tab::Media, Tab::Files, Tab::Lyrics] {
        if enabled(tab) && !tabs.contains(&tab) { tabs.push(tab); }
    }
    tabs
}

pub struct Island {
    hwnd: HWND,
    monitor: RECT,
    scale: f32,
    bar_height: f32,
    style: IslandStyle,
    notch_width: f32,
    /// Current window size in pixels.
    size: (f32, f32),
    animating: bool,
    hovered: bool,
    lyrics_pinned: bool,
    dismissed: bool,
    horizontal_wheel_remainder: i32,
    suspended: bool,
    notices: bool,
    animate_eq: bool,
    media: Option<Media>,
    pomodoro: Pomodoro,
    status: SystemStatus,
    privacy: Sensors,
    notice: Option<Notice>,
    art: Option<(u64, ID2D1Bitmap1)>,
    app_icon: Option<(u64, ID2D1Bitmap1)>,
    lyrics_background: Option<(u64, ID2D1Bitmap1)>,
    lyrics_background_previous: Option<ID2D1Bitmap1>,
    lyrics_background_changed: Option<Instant>,
    lyrics_background_track: String,
    lyrics_background_motion: bool,
    lyrics_warp_source: Vec<u8>,
    lyrics_warp_time: f32,
    lyrics_warp_frame: Option<Instant>,
    /// Live preview of the playing window, shown in the cover's place while
    /// expanded when the source set no cover; keyed by the track title.
    thumb: Option<(String, Option<Thumbnail>)>,
    eq_step: u32,
    eq_running: bool,
    lyrics_motion_running: bool,
    lyric_motion: LyricMotion,
    /// The composition-animated equalizer; `Err` once it failed to build, so
    /// the island falls back to repainting on a timer.
    eq_layer: Option<std::result::Result<EqLayer, ()>>,
    /// Where this paint wants the animated bars (left, middle), if anywhere.
    eq_at: Option<(f32, f32)>,
    formats: Option<Formats>,
    panel: PanelLayout,
    hot: Option<Hot>,
    tab: Tab,
    tabs: Vec<Tab>,
    file_shelf: bool,
    file_scroll: usize,
    lyrics_scroll: usize,
    file_pressed: Option<(usize, f32, f32)>,
    drop_registered: bool,
    drag_over: bool,
}

pub fn create(gfx: &Rc<Gfx>, theme: &Rc<Theme>, config: &IslandConfig, monitor: &Monitor, bar_height: f32) -> Result<HWND> {
    let mut island = Island {
        hwnd: HWND::default(),
        monitor: monitor.rect,
        scale: monitor.scale,
        bar_height,
        style: config.style,
        notch_width: config.notch_width(),
        size: (0.0, 0.0),
        animating: false,
        hovered: false,
        lyrics_pinned: false,
        dismissed: false,
        horizontal_wheel_remainder: 0,
        suspended: false,
        notices: config.notices,
        animate_eq: config.animate_equalizer,
        media: media::current(),
        pomodoro: pomodoro_state(),
        status: services::status(),
        privacy: privacy::current(),
        notice: None,
        art: None,
        app_icon: None,
        lyrics_background: None,
        lyrics_background_previous: None,
        lyrics_background_changed: None,
        lyrics_background_track: String::new(),
        lyrics_background_motion: config.lyrics_background_motion,
        lyrics_warp_source: Vec::new(),
        lyrics_warp_time: 0.0,
        lyrics_warp_frame: None,
        thumb: None,
        eq_step: 0,
        eq_running: false,
        lyrics_motion_running: false,
        lyric_motion: LyricMotion::default(),
        eq_layer: None,
        eq_at: None,
        formats: None,
        panel: PanelLayout::new(monitor.scale),
        hot: None,
        tab: configured_tabs(config)[0],
        tabs: configured_tabs(config),
        file_shelf: config.file_shelf,
        file_scroll: 0,
        lyrics_scroll: 0,
        file_pressed: None,
        drop_registered: false,
        drag_over: false,
    };
    let size = {
        let (w, h) = island.compact();
        (w * monitor.scale, h * monitor.scale)
    };
    island.size = size;
    let mut rect = frame_rect(monitor.rect, size, monitor.scale, bar_height);
    if island.style == IslandStyle::Notch { rect.bottom -= rect.top - monitor.rect.top; rect.top = monitor.rect.top; }
    let spec = BarSpec { rect, scale: monitor.scale, corners: Corners::None, activate: false };
    bar::create(gfx.clone(), theme.clone(), spec, Box::new(island))
}

impl Island {
    fn paint_week(&self, cx: &PaintCx, formats: &Formats, alpha: f32) {
        use windows::Win32::Globalization::{GetLocaleInfoEx, LOCALE_SABBREVDAYNAME1, LOCALE_SABBREVMONTHNAME1};
        use windows::Win32::System::SystemInformation::GetLocalTime;
        use windows::core::PCWSTR;
        use crate::model::calendar::{Date, month_grid};

        static NAMES: OnceLock<([String; 7], [String; 12])> = OnceLock::new();
        let names = NAMES.get_or_init(|| {
            let get = |kind| {
                let mut buf = [0u16; 64];
                let len = unsafe { GetLocaleInfoEx(PCWSTR::null(), kind, Some(&mut buf)) };
                String::from_utf16_lossy(&buf[..len.saturating_sub(1) as usize])
            };
            (std::array::from_fn(|i| get(LOCALE_SABBREVDAYNAME1 + i as u32)),
             std::array::from_fn(|i| get(LOCALE_SABBREVMONTHNAME1 + i as u32)))
        });
        let now = unsafe { GetLocalTime() };
        let today = Date::new(now.wYear.into(), now.wMonth.into(), now.wDay.into());
        let days = month_grid(today.year, today.month, 1);
        let Some(index) = days.iter().position(|(day, _)| *day == today) else { return; };
        let s = cx.scale;
        let header = format!("{}  {}", names.1[(today.month - 1) as usize], today.year);
        text::draw(cx, &header, &formats.title, rect(409.0*s, 54.0*s, 664.0*s, 78.0*s), with_alpha(TEXT, alpha));
        for i in 0..7 {
            let (day, _) = days[(index / 7) * 7 + i];
            let left = (409.0 + i as f32 * 36.0) * s;
            let top = 86.0 * s;
            let cell = rect(left, top, left + 32.0*s, 145.0*s);
            if day == today && let Ok(brush) = cx.gfx.brush(with_alpha(rgba(10, 132, 255, 0.28), alpha)) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(cell, 10.0*s), &brush) };
            }
            text::draw(cx, &names.0[i], &formats.small_center, rect(left, top + 4.0*s, left + 32.0*s, top + 24.0*s), with_alpha(DIM, alpha));
            text::draw(cx, &day.day.to_string(), &formats.time, rect(left, top + 25.0*s, left + 32.0*s, top + 54.0*s), with_alpha(if day == today { TEXT } else { DIM }, alpha));
        }
        text::draw(cx, "Calendar events not connected", &formats.artist, rect(409.0*s, 164.0*s, 664.0*s, 190.0*s), with_alpha(DIM, alpha));
    }

    fn paint_tabs(&self, cx: &PaintCx, formats: &Formats, alpha: f32) {
        for (i, tab) in self.tabs.iter().copied().enumerate() {
            let (label, glyph) = match tab {
                Tab::Media => ("Media", GLYPH_MUSIC),
                Tab::Files => ("Files", "\u{E8B7}"),
                Tab::Lyrics => ("Lyrics", "\u{E8D2}"),
            };
            let r = tab_rect(i, cx.scale);
            if (self.tab == tab || self.hot == Some(Hot::Tab(i)))
                && let Ok(brush) = cx.gfx.brush(with_alpha(if self.tab == tab { TAB_SELECTED } else { SURFACE }, alpha)) {
                    unsafe { cx.dc.FillRoundedRectangle(&rounded(r, 9.0 * cx.scale), &brush) };
                }
            let color = with_alpha(if self.tab == tab { TEXT } else { DIM }, alpha);
            text::draw(cx, glyph, &formats.icon_small, rect(r.left + 8.0*cx.scale, r.top, r.left + 25.0*cx.scale, r.bottom), color);
            text::draw(cx, label, &formats.tab, rect(r.left + 28.0*cx.scale, r.top, r.right - 4.0*cx.scale, r.bottom), color);
        }
    }

    fn paint_files(&self, cx: &PaintCx, formats: &Formats, alpha: f32) {
        let s = cx.scale;
        let paths = island_files::list();
        for (i, label, hot) in [(0, "Manage bookmarks", Hot::Bookmarks), (1, "Clipboard history", Hot::Clipboard)] {
            let r = files_action_rect(i, s);
            if let Ok(brush) = cx.gfx.brush(with_alpha(if self.hot == Some(hot) { TAB_SELECTED } else { SURFACE }, alpha)) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(r, 8.0*s), &brush) };
            }
            text::draw(cx, label, &formats.small, rect(r.left+12.0*s,r.top,r.right-8.0*s,r.bottom), with_alpha(TEXT,alpha));
        }
        if paths.is_empty() {
            text::draw(cx, "Drop files here to keep them close", &formats.artist,
                rect(22.0*s, 58.0*s, 378.0*s, 105.0*s), with_alpha(DIM, alpha));
            text::draw(cx, "Files stay in their original location", &formats.small,
                rect(22.0*s, 103.0*s, 378.0*s, 128.0*s), with_alpha(DIM, alpha));
            return;
        }
        for (row, index) in (self.file_scroll..paths.len()).take(4).enumerate() {
            let r = file_rect(row, s);
            if let Ok(brush) = cx.gfx.brush(with_alpha(if self.hot == Some(Hot::File(index)) { TAB_SELECTED } else { SURFACE }, alpha)) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(r, 9.0*s), &brush) };
            }
            let name = paths[index].file_name().unwrap_or(paths[index].as_os_str()).to_string_lossy();
            text::draw(cx, &name, &formats.artist, rect(r.left + 10.0*s, r.top, r.right - 38.0*s, r.bottom), with_alpha(TEXT, alpha));
            let close = rect(r.right - 30.0*s, r.top, r.right, r.bottom);
            text::draw(cx, "×", &formats.time, close, with_alpha(if self.hot == Some(Hot::RemoveFile(index)) { TEXT } else { DIM }, alpha));
        }
        if paths.len() > 4 {
            text::draw(cx, &format!("{} files · scroll for more", paths.len()), &formats.small,
                rect(20.0*s, 184.0*s, 380.0*s, 198.0*s), with_alpha(DIM, alpha));
        }
    }

    fn paint_lyrics(&mut self, cx: &PaintCx, formats: &Formats, alpha: f32) {
        let s = cx.scale;
        let background = rect(16.0*s, 40.0*s, 664.0*s, 228.0*s);
        if let Some(bitmap) = self.lyrics_background_bitmap(cx) {
            let props = D2D1_BRUSH_PROPERTIES {
                opacity: alpha,
                transform: Matrix3x2 { M11: (background.right-background.left)/LYRICS_BG_WIDTH as f32, M12: 0.0,
                    M21: 0.0, M22: (background.bottom-background.top)/LYRICS_BG_HEIGHT as f32,
                    M31: background.left, M32: background.top },
            };
            let blend = self.lyrics_background_changed.map_or(1.0, |at| (at.elapsed().as_secs_f32() / 0.25).clamp(0.0, 1.0));
            if blend < 1.0 && let Some(previous) = &self.lyrics_background_previous
                && let Ok(brush) = unsafe { cx.dc.CreateBitmapBrush(previous, None, Some(&props)) } {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(background, 16.0*s), &brush) };
            }
            let mut current_props = props;
            if self.lyrics_background_previous.is_some() { current_props.opacity *= blend; }
            if let Ok(brush) = unsafe { cx.dc.CreateBitmapBrush(&bitmap, None, Some(&current_props)) } {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(background, 16.0*s), &brush) };
            }
        }
        let area = |top: f32, bottom: f32| rect(34.0*s, top*s, 645.0*s, bottom*s);
        if let Some((title, artist)) = self.media.as_ref().map(|m| (m.title.clone(), m.artist.clone())) {
            self.paint_art(cx, formats, rect(34.0*s, 50.0*s, 72.0*s, 88.0*s), 8.0*s, alpha);
            text::draw(cx, &title, &formats.title, rect(84.0*s, 49.0*s, 598.0*s, 70.0*s), with_alpha(TEXT, alpha));
            text::draw(cx, &artist, &formats.lyric, rect(84.0*s, 70.0*s, 598.0*s, 90.0*s), with_alpha(DIM, alpha));
        }
        let pin = rect(612.0*s, 52.0*s, 646.0*s, 86.0*s);
        if let Ok(brush) = cx.gfx.brush(with_alpha(if self.lyrics_pinned || self.hot == Some(Hot::PinLyrics) { TAB_SELECTED } else { SURFACE }, alpha)) {
            unsafe { cx.dc.FillRoundedRectangle(&rounded(pin, 17.0*s), &brush) };
        }
        text::draw(cx, "\u{E718}", &formats.icon_small, pin, with_alpha(if self.lyrics_pinned { TEXT } else { DIM }, alpha));
        let Some(media) = &self.media else {
            text::draw(cx, "Play a song to see lyrics", &formats.artist, area(62.0, 95.0), with_alpha(DIM, alpha));
            return;
        };
        if let Some(live) = crate::services::lyrics::timed(media).or_else(|| crate::services::lyrics::live(media)) {
            let track = format!("{}\u{0}{}", media.title, media.artist);
            let progress = self.lyric_motion.update(&track, &live, media.playing);
            let start = live.active.saturating_sub(1);
            let previous_height = if start < live.active {
                text::measure(&cx.gfx.dwrite, &live.lines[start], &formats.lyric_active, 611.0*s).1 / s + 10.0
            } else { 0.0 };
            let mut top = 132.0 - previous_height + self.lyric_motion.offset;
            unsafe { cx.dc.PushAxisAlignedClip(&area(96.0, 220.0), windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED); }
            for (index, line) in live.lines.iter().enumerate().skip(start).take(5) {
                let selected = index == live.active;
                let height = (text::measure(&cx.gfx.dwrite, line, &formats.lyric_active, 611.0*s).1 / s).max(28.0);
                let bounds = area(top, top + height);
                top += height + 10.0;
                if selected && line.chars().all(|ch| ch == '•' || ch.is_whitespace()) {
                    let time = now_ms() as f32 / 350.0;
                    for i in 0..3 {
                        let strength = 0.35 + 0.65 * (0.5 + 0.5 * (time - i as f32 * 0.85).sin());
                        let dot = D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: bounds.left + (6.0 + i as f32 * 14.0) * s, Y: (bounds.top + bounds.bottom) / 2.0 }, radiusX: 3.5*s, radiusY: 3.5*s };
                        if let Ok(brush) = cx.gfx.brush(with_alpha(TEXT, alpha * strength)) { unsafe { cx.dc.FillEllipse(&dot, &brush) }; }
                    }
                    continue;
                }
                if selected && let Some(progress) = progress {
                    text::draw_karaoke(cx, line, &formats.lyric_active, bounds, progress, alpha);
                } else {
                    let distance = index.abs_diff(live.active);
                    let opacity = if selected { 1.0 } else if distance == 1 { 0.48 } else { 0.25 };
                    text::draw(cx, line, &formats.lyric_active, bounds,
                        with_alpha(TEXT, alpha * opacity));
                }
            }
            unsafe { cx.dc.PopAxisAlignedClip(); }
            return;
        }
        text::draw(cx, "Waiting for Spicy Lyrics", &formats.artist, area(105.0, 135.0), with_alpha(DIM, alpha));
    }

    fn content(&self) -> Content {
        let timer = self.pomodoro.is_active();
        if self.media.is_some() {
            Content::Media { timer }
        } else if timer {
            Content::Timer
        } else {
            Content::Idle
        }
    }

    /// The compact size in DIPs: the content plus a dot per sensor in use.
    fn compact(&self) -> (f32, f32) {
        let (w, h) = compact_size(self.content());
        let bar_px = (self.bar_height * self.scale).round();
        let content = crate::ui::topbar::content_height(bar_px, self.scale) / self.scale.max(0.01);
        let width = w + dots_width(self.privacy);
        (if self.style == IslandStyle::Notch { width.max(self.notch_width) } else { width }, h.min(content.max(1.0)))
    }

    /// The size the pill is heading for, in pixels.
    fn target(&self) -> (f32, f32) {
        let (w, h) = if self.hovered {
            EXPANDED
        } else if let Some(notice) = &self.notice {
            let width = if matches!(notice.visual, NoticeVisual::Power(_)) { 220.0 } else { NOTICE.0 };
            (width.max(self.compact().0), NOTICE.1)
        } else {
            self.compact()
        };
        (w * self.scale, h * self.scale)
    }

    fn animate(&mut self) {
        if !self.animating && !self.suspended && self.size != self.target() {
            self.animating = true;
            unsafe { SetTimer(Some(self.hwnd), TIMER_ANIMATE, FRAME_MS, None) };
        }
    }

    fn raise(&self) {
        if !self.suspended {
            unsafe { let _ = SetWindowPos(self.hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE); }
        }
    }

    fn step(&mut self) -> bool {
        let (tw, th) = self.target();
        let done_w = approach(&mut self.size.0, tw);
        let done_h = approach(&mut self.size.1, th);
        if done_w && done_h {
            self.animating = false;
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_ANIMATE);
            }
            self.schedule();
        }
        true
    }

    fn expanded(&self) -> bool {
        self.hovered && expansion_from(self.size.0, self.compact().0, self.scale) > 0.9
    }

    /// Whether the equalizer should be moving.
    fn wants_eq(&self) -> bool {
        // Only the fallback needs a timer; the composition layer animates itself.
        self.animate_eq
            && matches!(self.eq_layer, Some(Err(())))
            && !self.suspended
            && !self.hovered
            && self.notice.is_none()
            && self.media.as_ref().is_some_and(|m| m.playing)
    }

    /// Arms the one-shot ticks the current state needs (progress, countdown,
    /// equalizer) and stops the ones it does not.
    fn schedule(&mut self) {
        let now = now_ms();
        let tick = if self.suspended {
            None
        } else if self.hovered {
            let media = self.media.as_ref().is_some_and(|m| m.playing && m.has_timeline());
            (media || self.pomodoro.is_running() || self.tab == Tab::Lyrics).then_some(1000)
        } else {
            // Compact: the countdown shows minutes only, so wake at the boundary.
            self.pomodoro.is_running().then(|| pomodoro::tick_delay(self.pomodoro.remaining_ms(now)))
        };
        unsafe {
            match tick {
                Some(ms) => {
                    SetTimer(Some(self.hwnd), TIMER_TICK, ms, None);
                }
                None => {
                    let _ = KillTimer(Some(self.hwnd), TIMER_TICK);
                }
            }
        }
        let eq = self.wants_eq();
        if eq != self.eq_running {
            self.eq_running = eq;
            unsafe {
                if eq {
                    SetTimer(Some(self.hwnd), TIMER_EQ, EQ_MS, None);
                } else {
                    let _ = KillTimer(Some(self.hwnd), TIMER_EQ);
                }
            }
        }
        let motion = !self.suspended && self.hovered && self.tab == Tab::Lyrics && self.media.as_ref().is_some_and(|m| m.playing);
        if motion != self.lyrics_motion_running {
            self.lyrics_motion_running = motion;
            unsafe {
                if motion { SetTimer(Some(self.hwnd), TIMER_LYRICS_MOTION, 16, None); }
                else { let _ = KillTimer(Some(self.hwnd), TIMER_LYRICS_MOTION); }
            }
        }
    }

    fn show_notice(&mut self, notice: Notice) {
        if self.suspended {
            return;
        }
        self.notice = Some(notice);
        unsafe { SetTimer(Some(self.hwnd), TIMER_NOTICE, NOTICE_MS, None) };
    }

    fn hot_at(&self, x: f32, y: f32) -> Option<Hot> {
        if !self.expanded() {
            return None;
        }
        for i in 0..self.tabs.len() {
            if contains(tab_rect(i, self.scale), x, y) { return Some(Hot::Tab(i)); }
        }
        if self.tab == Tab::Files {
            if contains(files_action_rect(0, self.scale), x, y) { return Some(Hot::Bookmarks); }
            if contains(files_action_rect(1, self.scale), x, y) { return Some(Hot::Clipboard); }
            let files = island_files::list();
            for (row, index) in (self.file_scroll..files.len()).take(4).enumerate() {
                let r = file_rect(row, self.scale);
                if contains(rect(r.right - 30.0 * self.scale, r.top, r.right, r.bottom), x, y) { return Some(Hot::RemoveFile(index)); }
                if contains(r, x, y) { return Some(Hot::File(index)); }
            }
            return None;
        }
        if self.tab == Tab::Lyrics {
            return contains(rect(612.0*self.scale, 52.0*self.scale, 646.0*self.scale, 86.0*self.scale), x, y).then_some(Hot::PinLyrics);
        }
        let p = &self.panel;
        let media = self.media.as_ref();
        [
            (p.prev, Hot::Prev, media.is_some_and(|m| m.can_prev)),
            (p.play, Hot::Play, media.is_some_and(|m| m.can_play_pause)),
            (p.next, Hot::Next, media.is_some_and(|m| m.can_next)),
            (p.toggle, Hot::Toggle, true),
            (p.reset, Hot::Reset, self.pomodoro.is_active()),
            (p.phase, Hot::Label, !self.pomodoro.is_active()),
            (p.dec, Hot::Dec, !self.pomodoro.is_active()),
            (p.inc, Hot::Inc, !self.pomodoro.is_active()),
        ]
        .into_iter()
        .find(|&(r, _, enabled)| enabled && contains(r, x, y))
        .map(|(_, hot, _)| hot)
        .or_else(|| {
            let slop = 6.0 * self.scale;
            let bar = rect(p.bar.left, p.bar.top - slop, p.bar.right, p.bar.bottom + slop);
            (media.is_some_and(|m| m.can_seek && m.has_timeline()) && contains(bar, x, y)).then_some(Hot::Seek)
        })
    }
}

impl View for Island {
    fn attach(&mut self, hwnd: HWND) {
        self.hwnd = hwnd;
        if self.file_shelf {
            island_files::init();
            self.drop_registered = island_drag::register(hwnd);
        }
        shared(|s| s.views.push(hwnd));
        self.schedule();
    }

    fn destroy(&mut self) {
        if self.drop_registered { island_drag::unregister(self.hwnd); self.drop_registered = false; }
    }

    fn frame(&self) -> Option<RECT> {
        let mut frame = frame_rect(self.monitor, self.size, self.scale, self.bar_height);
        if self.style == IslandStyle::Notch { frame.bottom -= frame.top - self.monitor.top; frame.top = self.monitor.top; }
        Some(frame)
    }

    fn suspend(&mut self, suspended: bool) -> bool {
        self.suspended = suspended;
        unsafe {
            if suspended {
                for timer in [TIMER_ANIMATE, TIMER_NOTICE, TIMER_TICK, TIMER_EQ, TIMER_LEAVE, TIMER_LYRICS_MOTION, TIMER_PIN_TOP] {
                    let _ = KillTimer(Some(self.hwnd), timer);
                }
                self.animating = false;
                self.eq_running = false;
                self.lyrics_motion_running = false;
                self.notice = None;
                self.hovered = false;
                self.lyrics_pinned = false;
                self.dismissed = false;
                self.drag_over = false;
                self.thumb = None;
                let _ = ShowWindow(self.hwnd, SW_HIDE);
            } else {
                // State moved on while hidden.
                self.media = media::current();
                self.pomodoro = pomodoro_state();
                self.status = services::status();
                self.privacy = privacy::current();
                self.size = {
                    let (w, h) = self.target();
                    (w, h)
                };
                let _ = ShowWindow(self.hwnd, SW_SHOWNOACTIVATE);
                self.schedule();
            }
        }
        true
    }

    fn model_changed(&mut self) -> bool {
        if self.suspended {
            return false;
        }
        let previous_track = self.media.as_ref().map(|m| (&m.title, &m.artist, &m.album));
        let latest = media::current();
        if previous_track != latest.as_ref().map(|m| (&m.title, &m.artist, &m.album)) { self.lyrics_scroll = 0; }
        self.media = latest;
        self.pomodoro = pomodoro_state();
        let status = services::status();
        let sensors = privacy::current();
        let level_notice = notice_for(&self.status, &status).filter(|_| !services::own_change_recent());
        let notice = level_notice.or_else(|| privacy_notice(self.privacy, sensors));
        if self.notices
            && !self.hovered
            && let Some(notice) = notice
        {
            self.show_notice(notice);
        }
        self.privacy = sensors;
        self.status = status;
        self.animate();
        self.schedule();
        true
    }

    fn command(&mut self, id: usize) -> bool {
        match id {
            CMD_LYRICS_CHANGED => return !self.suspended && self.hovered && self.tab == Tab::Lyrics,
            CMD_REFRESH_APPEARANCE => {
                let config = crate::model::config::load().island;
                let tabs = configured_tabs(&config);
                if self.tabs != tabs { self.tab = tabs[0]; }
                self.tabs = tabs;
                self.style = config.style;
                self.notch_width = config.notch_width();
                self.hot = None;
                self.animate();
                return true;
            }
            island_drag::DRAG_ENTER => {
                self.drag_over = true;
                self.hovered = true;
                self.tab = Tab::Files;
                self.animate();
                self.schedule();
                return true;
            }
            island_drag::DRAG_LEAVE => {
                self.drag_over = false;
                unsafe { SetTimer(Some(self.hwnd), TIMER_LEAVE, LEAVE_DELAY_MS, None); }
                return true;
            }
            island_drag::DRAG_DROP => {
                self.drag_over = false;
                island_files::accept_pending();
                self.tab = Tab::Files;
                self.file_scroll = 0;
                self.hovered = true;
                self.animate();
                notify_views();
                return true;
            }
            CMD_WORK_DONE | CMD_BREAK_DONE => {}
            _ => return false,
        }
        if self.notices {
            let text = if id == CMD_WORK_DONE { "Focus done, take a break" } else { "Break over" };
            self.show_notice(Notice { glyph: GLYPH_TIMER, text: text.into(), level: None, visual: NoticeVisual::Standard });
            self.animate();
        }
        false
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        if self.dismissed { return false; }
        unsafe { let _ = KillTimer(Some(self.hwnd), TIMER_LEAVE); }
        if let Some((index, ox, oy)) = self.file_pressed
            && ((x - ox).abs() > 6.0*self.scale || (y - oy).abs() > 6.0*self.scale) {
                self.file_pressed = None;
                unsafe { if GetCapture() == self.hwnd { let _ = ReleaseCapture(); } }
                if let Some(path) = island_files::list().get(index).cloned() { island_drag::drag_file(self.hwnd, path); }
                return true;
            }
        let hot = self.hot_at(x, y);
        let hover_changed = !std::mem::replace(&mut self.hovered, true);
        if hover_changed {
            self.raise();
            unsafe {
                let _ = KillTimer(Some(self.hwnd), TIMER_NOTICE);
            }
            self.notice = None;
            self.animate();
            self.schedule();
        }
        std::mem::replace(&mut self.hot, hot) != hot
    }

    fn mouse_leave(&mut self) -> bool {
        // Posted test input has no real pointer, so Windows reports a leave at once.
        if cfg!(debug_assertions) && std::env::var_os("WDOCK_TEST_INPUT").is_some() {
            return false;
        }
        self.hot = None;
        if unsafe { GetCapture() } != self.hwnd { self.file_pressed = None; }
        // Resizing the window and moving through the space around the rounded
        // corners can briefly produce WM_MOUSELEAVE during expansion.
        unsafe { SetTimer(Some(self.hwnd), TIMER_LEAVE, LEAVE_DELAY_MS, None); }
        true
    }

    fn mouse_down(&mut self, x: f32, y: f32, button: Button) -> bool {
        if button != Button::Left {
            return false;
        }
        match self.hot_at(x, y) {
            Some(Hot::Tab(i)) => {
                self.tab = self.tabs[i];
                if self.tab != Tab::Lyrics { self.lyrics_pinned = false; unsafe { let _ = KillTimer(Some(self.hwnd), TIMER_PIN_TOP); } }
                self.hot = None;
                self.thumb = None;
                self.schedule();
                return true;
            }
            Some(Hot::PinLyrics) => {
                self.lyrics_pinned = !self.lyrics_pinned;
                unsafe {
                    if self.lyrics_pinned { SetTimer(Some(self.hwnd), TIMER_PIN_TOP, 1000, None); }
                    else { let _ = KillTimer(Some(self.hwnd), TIMER_PIN_TOP); }
                }
                if self.lyrics_pinned { self.raise(); }
                return true;
            }
            Some(Hot::Bookmarks) => { popup::toggle_bookmarks(self.frame().unwrap_or(self.monitor), self.scale); return true; }
            Some(Hot::Clipboard) => { popup::toggle_clipboard(self.frame().unwrap_or(self.monitor), self.scale); return true; }
            Some(Hot::File(i)) => { self.file_pressed = Some((i, x, y)); unsafe { SetCapture(self.hwnd); } return false; }
            Some(Hot::RemoveFile(i)) => {
                island_files::remove(i);
                self.file_scroll = self.file_scroll.min(island_files::list().len().saturating_sub(4));
                notify_views();
                return true;
            }
            Some(Hot::Prev) => media::send(Command::Previous),
            Some(Hot::Play) => media::send(Command::PlayPause),
            Some(Hot::Next) => media::send(Command::Next),
            Some(Hot::Seek) => {
                if let Some(m) = &self.media {
                    let bar = self.panel.bar;
                    let fraction = ((x - bar.left) / (bar.right - bar.left)).clamp(0.0, 1.0);
                    media::send(Command::Seek((fraction * m.length_ms as f32) as u64));
                }
            }
            Some(Hot::Toggle) => pomodoro_act(|p, now| p.toggle(now)),
            Some(Hot::Reset) => pomodoro_act(|p, _| p.reset()),
            Some(Hot::Label) => pomodoro_act(|p, _| p.select(if p.phase() == Phase::Work { Phase::Break } else { Phase::Work })),
            Some(Hot::Dec) => pomodoro_act(|p, _| {
                p.step(false);
            }),
            Some(Hot::Inc) => pomodoro_act(|p, _| {
                p.step(true);
            }),
            None => {}
        }
        false
    }

    fn mouse_up(&mut self, x: f32, y: f32, button: Button) -> bool {
        let pressed = self.file_pressed.take();
        if pressed.is_some() { unsafe { if GetCapture() == self.hwnd { let _ = ReleaseCapture(); } } }
        if button == Button::Left && let Some((index, _, _)) = pressed
            && self.hot_at(x, y) == Some(Hot::File(index))
            && let Some(path) = island_files::list().get(index) {
                crate::platform::launch::open(&path.to_string_lossy());
        }
        false
    }

    fn wheel(&mut self, _x: f32, y: f32, delta: i16) -> bool {
        if self.dismissed && delta < 0 {
            self.dismissed = false;
            self.hovered = true;
            self.animate();
            self.schedule();
            return true;
        }
        if self.expanded() && y < 36.0 * self.scale && delta > 0 {
            self.dismissed = true;
            self.hovered = false;
            self.lyrics_pinned = false;
            unsafe { let _ = KillTimer(Some(self.hwnd), TIMER_PIN_TOP); }
            self.hot = None;
            self.animate();
            self.schedule();
            return true;
        }

        if self.tab != Tab::Files { return false; }
        let max = island_files::list().len().saturating_sub(4);
        let next = if delta > 0 { self.file_scroll.saturating_sub(1) } else { (self.file_scroll + 1).min(max) };
        std::mem::replace(&mut self.file_scroll, next) != next
    }

    fn horizontal_wheel(&mut self, _x: f32, _y: f32, delta: i16) -> bool {
        if !self.expanded() || self.tabs.len() < 2 || delta == 0 { return false; }
        let steps = wheel_step(&mut self.horizontal_wheel_remainder, delta);
        if steps == 0 { return false; }
        let index = self.tabs.iter().position(|&tab| tab == self.tab).unwrap_or(0);
        let next = (index as i32 + steps).clamp(0, self.tabs.len() as i32 - 1) as usize;
        if next == index { return false; }
        self.tab = self.tabs[next];
        if self.tab != Tab::Lyrics { self.lyrics_pinned = false; unsafe { let _ = KillTimer(Some(self.hwnd), TIMER_PIN_TOP); } }
        self.hot = None;
        self.thumb = None;
        self.schedule();
        true
    }

    fn timer(&mut self, id: usize) -> bool {
        match id {
            TIMER_PIN_TOP => { if self.lyrics_pinned { self.raise(); } return false; }
            TIMER_LEAVE => {
                if self.drag_over { return false; }
                if self.lyrics_pinned && self.tab == Tab::Lyrics {
                    unsafe { let _ = KillTimer(Some(self.hwnd), TIMER_LEAVE); }
                    return false;
                }
                let mut point = POINT::default();
                if !self.dismissed && unsafe { GetCursorPos(&mut point) }.is_ok() && in_hover_area(self.monitor, self.scale, self.bar_height, point) {
                    return false;
                }
                unsafe { let _ = KillTimer(Some(self.hwnd), TIMER_LEAVE); }
                self.dismissed = false;
                self.hovered = false;
                self.hot = None;
                self.animate();
                self.schedule();
                return true;
            }
            TIMER_ANIMATE => return self.step(),
            TIMER_NOTICE => {
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_NOTICE);
                }
                self.notice = None;
                self.animate();
                self.schedule();
                return true;
            }
            TIMER_EQ => {
                self.eq_step = self.eq_step.wrapping_add(1);
                return true;
            }
            TIMER_LYRICS_MOTION => return self.lyrics_motion_running,
            TIMER_TICK => {
                // One-shot: re-armed below if still needed.
                unsafe {
                    let _ = KillTimer(Some(self.hwnd), TIMER_TICK);
                }
                self.pomodoro = pomodoro_state();
                self.schedule();
                return self.hovered || self.pomodoro.is_active();
            }
            _ => {}
        }
        false
    }

    fn paint(&mut self, cx: &PaintCx) {
        let s = cx.scale;
        if self.formats.is_none() {
            self.formats = build_formats(cx);
        }
        let (w, h) = (cx.width, cx.height);
        // From this content's own compact width, so a pill at rest is fully opaque.
        let e = expansion_from(w, self.compact().0, s);
        let radius = (h / 2.0).min(lerp(h / 2.0, EXPANDED_RADIUS * s, e));

        let pill = rect(0.0, 0.0, w, h);
        if let Ok(brush) = cx.gfx.brush(rgba(0, 0, 0, 1.0)) {
            unsafe { cx.dc.FillRoundedRectangle(&rounded(pill, radius), &brush) };
            if self.style == IslandStyle::Notch {
                unsafe { cx.dc.FillRectangle(&rect(0.0, 0.0, w, radius), &brush) };
            }
        }
        if self.style == IslandStyle::Island && let Ok(brush) = cx.gfx.brush(rgba(255, 255, 255, 0.06)) {
            unsafe { cx.dc.DrawRoundedRectangle(&rounded(rect(0.5, 0.5, w - 0.5, h - 0.5), radius), &brush, 1.0, None) };
        }

        let Some(formats) = self.formats.take() else {
            return;
        };
        self.eq_at = None;
        let small_alpha = 1.0 - ramp(e, 0.0, 0.45);
        let panel_alpha = ramp(e, 0.6, 1.0);
        if small_alpha > 0.0 {
            match self.notice.clone() {
                Some(notice) if !self.hovered => self.paint_notice(cx, &formats, &notice, small_alpha),
                _ => self.paint_compact(cx, &formats, small_alpha),
            }
        }
        if panel_alpha > 0.0 {
            self.paint_tabs(cx, &formats, panel_alpha);
            match self.tab {
                Tab::Media => self.paint_panel(cx, &formats, panel_alpha),
                Tab::Files => self.paint_files(cx, &formats, panel_alpha),
                Tab::Lyrics => self.paint_lyrics(cx, &formats, panel_alpha),
            }
        }
        self.sync_eq_layer(cx);
        self.sync_thumbnail(if self.tab == Tab::Media { panel_alpha } else { 0.0 });
        self.formats = Some(formats);
    }
}

/// The visible, unminimized top-level window whose title contains the track
/// title (browsers title their window after the playing tab), topmost first.
fn playing_window(title: &str) -> Option<HWND> {
    use windows::Win32::Foundation::LPARAM;
    use windows::Win32::UI::WindowsAndMessaging::EnumWindows;
    use windows::core::BOOL;
    struct Search {
        needle: String,
        own: u32,
        found: Option<HWND>,
    }
    unsafe extern "system" fn each(hwnd: HWND, lparam: LPARAM) -> BOOL {
        let search = unsafe { &mut *(lparam.0 as *mut Search) };
        if wininfo::is_visible(hwnd)
            && !wininfo::is_minimized(hwnd)
            && wininfo::process_id(hwnd) != search.own
            && wininfo::title(hwnd).to_lowercase().contains(&search.needle)
        {
            search.found = Some(hwnd);
            return false.into();
        }
        true.into()
    }
    let mut search = Search {
        needle: title.to_lowercase(),
        own: unsafe { windows::Win32::System::Threading::GetCurrentProcessId() },
        found: None,
    };
    let _ = unsafe { EnumWindows(Some(each), LPARAM(&mut search as *mut Search as isize)) };
    search.found
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn with_alpha(color: D2D1_COLOR_F, alpha: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { a: color.a * alpha, ..color }
}

fn phase_color(phase: Phase) -> D2D1_COLOR_F {
    match phase {
        Phase::Work => WORK_COLOR,
        Phase::Break => BREAK_COLOR,
    }
}

fn build_formats(cx: &PaintCx) -> Option<Formats> {
    let s = cx.scale;
    let make = |family, size: f32, weight, align, ellipsis: bool| {
        let format = text::format_px(&cx.gfx.dwrite, family, size * s, weight, align, false).ok()?;
        if ellipsis {
            let trimming = DWRITE_TRIMMING { granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER, ..Default::default() };
            unsafe {
                let sign = cx.gfx.dwrite.CreateEllipsisTrimmingSign(&format).ok();
                let _ = format.SetTrimming(&trimming, sign.as_ref());
            }
        }
        Some(format)
    };
    Some(Formats {
        icon: make(text::ICON_FONT, 16.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER, false)?,
        icon_small: make(text::ICON_FONT, 13.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER, false)?,
        title: make(text::UI_FONT, 15.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING, true)?,
        artist: make(text::UI_FONT, 13.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING, true)?,
        tab: make(text::UI_FONT, 12.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING, false)?,
        small: make(text::UI_FONT, 11.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING, false)?,
        small_center: make(text::UI_FONT, 11.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_CENTER, false)?,
        small_right: make(text::UI_FONT, 11.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_TRAILING, false)?,
        time: make(text::UI_FONT, 15.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER, false)?,
        compact: make(text::UI_FONT, 12.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_TRAILING, false)?,
        lyric: make(text::UI_FONT, 17.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING, true)?,
        lyric_active: text::format_px(&cx.gfx.dwrite, text::UI_FONT, 20.0*s, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING, true).ok()?,
    })
}

const TEXT: D2D1_COLOR_F = rgba(255, 255, 255, 0.95);
const DIM: D2D1_COLOR_F = rgba(255, 255, 255, 0.55);
const CONTROL: D2D1_COLOR_F = rgba(255, 255, 255, 0.14);
const SURFACE: D2D1_COLOR_F = rgba(255, 255, 255, 0.07);
const TAB_SELECTED: D2D1_COLOR_F = rgba(255, 255, 255, 0.16);
const LYRICS_BG_WIDTH: u32 = 40;
const LYRICS_BG_HEIGHT: u32 = 20;

/// A small blurred, darkened cover-derived background, built once per track.
fn lyrics_background_pixels(art: Option<&media::Art>, colors: &[[u8; 3]]) -> Vec<u8> {
    let w = LYRICS_BG_WIDTH as usize;
    let h = LYRICS_BG_HEIGHT as usize;
    let mut pixels = vec![0u8; w * h * 4];
    let art = art.filter(|art| art.width > 0 && art.height > 0
        && (art.width as usize).checked_mul(art.height as usize).and_then(|n| n.checked_mul(4)).is_some_and(|n| n <= art.pixels.len()));
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) * 4;
            if matches!(colors.len(), 12 | 96) {
                let (grid_w, grid_h) = if colors.len() == 96 { (12, 8) } else { (4, 3) };
                let fx = x as f32 * (grid_w - 1) as f32 / (w - 1) as f32;
                let fy = y as f32 * (grid_h - 1) as f32 / (h - 1) as f32;
                let (x0, y0) = (fx.floor() as usize, fy.floor() as usize);
                let (x1, y1) = ((x0 + 1).min(grid_w - 1), (y0 + 1).min(grid_h - 1));
                for c in 0..3 {
                    let sample = |xx: usize, yy: usize| f32::from(colors[yy * grid_w + xx][2 - c]);
                    let upper = lerp(sample(x0, y0), sample(x1, y0), fx.fract());
                    let lower = lerp(sample(x0, y1), sample(x1, y1), fx.fract());
                    pixels[i + c] = lerp(upper, lower, fy.fract()) as u8;
                }
            } else if let Some(art) = art {
                let sx = x * art.width as usize / w;
                let sy = y * art.height as usize / h;
                let source = (sy * art.width as usize + sx) * 4;
                pixels[i..i + 3].copy_from_slice(&art.pixels[source..source + 3]);
            } else {
                pixels[i] = (64 + x * 22 / w) as u8;
                pixels[i + 1] = (33 + y * 18 / h) as u8;
                pixels[i + 2] = (53 + x * 16 / w) as u8;
            }
            pixels[i + 3] = 255;
        }
    }
    // Repeated box passes soften artwork without holding a rendering effect
    // or doing work on every animation frame.
    for _ in 0..if colors.len() == 96 { 1 } else { 8 } {
        let previous = pixels.clone();
        for y in 0..h {
            for x in 0..w {
                let mut total = [0u32; 3];
                let mut count = 0;
                for yy in y.saturating_sub(1)..=(y + 1).min(h - 1) {
                    for xx in x.saturating_sub(1)..=(x + 1).min(w - 1) {
                        let i = (yy * w + xx) * 4;
                        for c in 0..3 { total[c] += u32::from(previous[i + c]); }
                        count += 1;
                    }
                }
                let i = (y * w + x) * 4;
                for c in 0..3 { pixels[i + c] = (total[c] / count) as u8; }
            }
        }
    }
    for pixel in pixels.as_chunks_mut::<4>().0 {
        // Boost saturation before dimming, retaining contrast for white text.
        let luminance = f32::from(pixel[0]) * 0.114 + f32::from(pixel[1]) * 0.587 + f32::from(pixel[2]) * 0.299;
        let intensity = if colors.len() == 96 { 0.80 } else { 0.62 };
        for channel in &mut pixel[..3] {
            let saturated = luminance + (f32::from(*channel) - luminance) * 1.45;
            *channel = (saturated * intensity + 9.0).clamp(0.0, 255.0) as u8;
        }
    }
    pixels
}

impl Island {
    fn lyrics_background_bitmap(&mut self, cx: &PaintCx) -> Option<ID2D1Bitmap1> {
        let media = self.media.as_ref();
        let track = media.map_or_else(String::new, |m| format!("{}\u{0}{}\u{0}{}", m.title, m.artist, m.album));
        if self.lyrics_background_track != track {
            self.lyrics_background = None;
            self.lyrics_background_previous = None;
            self.lyrics_background_changed = None;
            self.lyrics_background_track = track;
            self.lyrics_warp_source.clear();
        }
        let palette = media.and_then(|m| crate::services::lyrics::timed(m).or_else(|| crate::services::lyrics::live(m))).map_or_else(Vec::new, |live| live.colors);
        use std::hash::{Hash, Hasher};
        let mut hash = std::collections::hash_map::DefaultHasher::new();
        media.map_or(0, |m| m.art_id).hash(&mut hash);
        palette.hash(&mut hash);
        let id = hash.finish();
        if let Some((cached, bitmap)) = &self.lyrics_background && *cached == id {
            let now = Instant::now();
            let dt = self.lyrics_warp_frame.replace(now).map_or(0.0, |last| now.duration_since(last).as_secs_f32().min(0.1));
            if self.lyrics_background_motion && self.hovered && media.is_some_and(|m| m.playing) { self.lyrics_warp_time += dt * 0.35; }
            let pixels = if self.lyrics_background_motion {
                crate::ui::kawarp::warp(&self.lyrics_warp_source, LYRICS_BG_WIDTH as usize, LYRICS_BG_HEIGHT as usize, self.lyrics_warp_time)
            } else { self.lyrics_warp_source.clone() };
            if !pixels.is_empty() { let _ = unsafe { bitmap.CopyFromMemory(None, pixels.as_ptr().cast(), LYRICS_BG_WIDTH * 4) }; }
            return Some(bitmap.clone());
        }
        let art = media.filter(|m| !m.art_is_icon).and_then(|m| m.art.as_deref());
        let pixels = lyrics_background_pixels(art, &palette);
        self.lyrics_warp_source = pixels.clone();
        self.lyrics_warp_frame = Some(Instant::now());
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0, dpiY: 96.0, bitmapOptions: D2D1_BITMAP_OPTIONS_NONE, colorContext: ManuallyDrop::new(None),
        };
        let bitmap = unsafe { cx.dc.CreateBitmap(D2D_SIZE_U { width: LYRICS_BG_WIDTH, height: LYRICS_BG_HEIGHT },
            Some(pixels.as_ptr().cast()), LYRICS_BG_WIDTH * 4, &props) }.ok()?;
        self.lyrics_background_previous = self.lyrics_background.take().map(|(_, bitmap)| bitmap);
        self.lyrics_background_changed = Some(Instant::now());
        self.lyrics_background = Some((id, bitmap.clone()));
        Some(bitmap)
    }

    /// The cover as a bitmap for this window's device context, cached per track.
    fn art_bitmap(&mut self, cx: &PaintCx) -> Option<ID2D1Bitmap1> {
        let media = self.media.as_ref()?;
        if let Some((id, bitmap)) = &self.art
            && *id == media.art_id
        {
            return Some(bitmap.clone());
        }
        let art = media.art.as_ref()?;
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0,
            dpiY: 96.0,
            bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            colorContext: ManuallyDrop::new(None),
        };
        let bitmap = unsafe {
            cx.dc.CreateBitmap(D2D_SIZE_U { width: art.width, height: art.height }, Some(art.pixels.as_ptr().cast()), art.width * 4, &props)
        };
        let id = media.art_id;
        match bitmap {
            Ok(bitmap) => {
                self.art = Some((id, bitmap.clone()));
                Some(bitmap)
            }
            Err(err) => {
                log::warn!("album art bitmap: {err}");
                None
            }
        }
    }

    fn app_icon_bitmap(&mut self, cx: &PaintCx) -> Option<ID2D1Bitmap1> {
        let media = self.media.as_ref()?;
        if let Some((id, bitmap)) = &self.app_icon && *id == media.art_id {
            return Some(bitmap.clone());
        }
        let icon = media.app_icon.as_ref()?;
        let props = D2D1_BITMAP_PROPERTIES1 {
            pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
            dpiX: 96.0, dpiY: 96.0, bitmapOptions: D2D1_BITMAP_OPTIONS_NONE,
            colorContext: ManuallyDrop::new(None),
        };
        let bitmap = unsafe { cx.dc.CreateBitmap(D2D_SIZE_U { width: icon.width, height: icon.height }, Some(icon.pixels.as_ptr().cast()), icon.width * 4, &props) }.ok()?;
        self.app_icon = Some((media.art_id, bitmap.clone()));
        Some(bitmap)
    }

    /// The cover in `dest` with rounded corners, the playing app's icon on a
    /// dim tile, or a music note on one.
    fn paint_art(&mut self, cx: &PaintCx, formats: &Formats, dest: D2D_RECT_F, radius: f32, alpha: f32) {
        let bitmap = self.art_bitmap(cx);
        let is_icon = self.media.as_ref().is_some_and(|m| m.art_is_icon);
        if let (Some(bitmap), true) = (&bitmap, is_icon) {
            if let Ok(brush) = cx.gfx.brush(with_alpha(CONTROL, alpha)) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(dest, radius), &brush) };
            }
            // Inset, drawn as a picture rather than filling the tile.
            let side = (dest.right - dest.left) * 0.62;
            let left = ((dest.left + dest.right - side) / 2.0).round();
            let top = ((dest.top + dest.bottom - side) / 2.0).round();
            unsafe {
                cx.dc.DrawBitmap(
                    bitmap,
                    Some(&rect(left, top, left + side, top + side)),
                    alpha,
                    windows::Win32::Graphics::Direct2D::D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC,
                    None,
                    None,
                )
            };
            return;
        }
        if let Some(bitmap) = bitmap {
            let size = unsafe { bitmap.GetPixelSize() }.width as f32;
            let scale = (dest.right - dest.left) / size;
            let props = D2D1_BRUSH_PROPERTIES {
                opacity: alpha,
                transform: Matrix3x2 { M11: scale, M12: 0.0, M21: 0.0, M22: scale, M31: dest.left, M32: dest.top },
            };
            if let Ok(brush) = unsafe { cx.dc.CreateBitmapBrush(&bitmap, None, Some(&props)) } {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(dest, radius), &brush) };
                return;
            }
        }
        if let Ok(brush) = cx.gfx.brush(with_alpha(CONTROL, alpha)) {
            unsafe { cx.dc.FillRoundedRectangle(&rounded(dest, radius), &brush) };
        }
        text::draw(cx, GLYPH_MUSIC, &formats.icon_small, dest, with_alpha(DIM, alpha));
    }

    /// Shows the playing window live over the cover tile while the panel is
    /// up and the source set no cover of its own. DWM composes it, so it
    /// costs wdock nothing per frame; it is dropped as soon as the panel goes.
    fn sync_thumbnail(&mut self, panel_alpha: f32) {
        let wanted = self.media.as_ref().filter(|m| m.art_is_icon && !m.title.is_empty() && panel_alpha > 0.0);
        let Some(media) = wanted else {
            self.thumb = None;
            return;
        };
        if self.thumb.as_ref().is_none_or(|(title, _)| *title != media.title) {
            let source = playing_window(&media.title);
            let thumb = source.and_then(|source| Thumbnail::register(self.hwnd, source));
            self.thumb = Some((media.title.clone(), thumb));
        }
        if let Some((_, Some(thumb))) = &self.thumb {
            let opacity = (panel_alpha.clamp(0.0, 1.0) * 255.0) as u8;
            if thumb.place(self.panel.art, opacity).is_none() {
                thumb.hide();
            }
        }
    }

    /// Puts the composition equalizer where this paint asked for it, building
    /// it on first use, or takes it off the window.
    fn sync_eq_layer(&mut self, cx: &PaintCx) {
        let s = cx.scale;
        match (self.eq_at, &mut self.eq_layer) {
            (Some((x, mid)), Some(Ok(layer))) => layer.place(cx.gfx, x, mid, 12.0 * s),
            (Some((x, mid)), slot @ None) => {
                match EqLayer::new(cx.gfx, cx.root, 2.5 * s, 2.0 * s, 12.0 * s, rgba(255, 255, 255, 0.9)) {
                    Ok(mut layer) => {
                        layer.place(cx.gfx, x, mid, 12.0 * s);
                        *slot = Some(Ok(layer));
                    }
                    Err(err) => {
                        log::warn!("animated equalizer unavailable, repainting instead: {err}");
                        *slot = Some(Err(()));
                        self.schedule();
                    }
                }
            }
            (None, Some(Ok(layer))) => layer.detach(cx.gfx),
            _ => {}
        }
    }

    fn paint_compact(&mut self, cx: &PaintCx, formats: &Formats, alpha: f32) {
        let s = cx.scale;
        let (w, h) = (cx.width, cx.height);
        let pad = 9.0 * s;
        let mid = h / 2.0;
        let content = self.content();
        let mut left = pad;
        let mut right = w - pad;
        // Privacy dots sit at the far left; everything else moves over.
        let dot = 3.5 * s;
        for (on, color) in [(self.privacy.mic, MIC_COLOR), (self.privacy.camera, CAMERA_COLOR)] {
            if !on {
                continue;
            }
            if let Ok(brush) = cx.gfx.brush(with_alpha(color, alpha)) {
                let ellipse = D2D1_ELLIPSE { point: windows_numerics::Vector2 { X: left + dot, Y: mid }, radiusX: dot, radiusY: dot };
                unsafe { cx.dc.FillEllipse(&ellipse, &brush) };
            }
            left += WIDTH_DOT * s;
        }

        if let Content::Media { .. } = content {
            let side = 18.0 * s;
            let dest = rect(left, mid - side / 2.0, left + side, mid + side / 2.0);
            if let Some(bitmap) = self.app_icon_bitmap(cx) {
                unsafe { cx.dc.DrawBitmap(&bitmap, Some(&dest), alpha, windows::Win32::Graphics::Direct2D::D2D1_INTERPOLATION_MODE_HIGH_QUALITY_CUBIC, None, None) };
            } else {
                text::draw(cx, GLYPH_MUSIC, &formats.icon_small, dest, with_alpha(DIM, alpha));
            }
            left += side + 8.0 * s;
        }
        if matches!(content, Content::Media { .. }) {
            // Equalizer: four bars, still when paused.
            let playing = self.media.as_ref().is_some_and(|m| m.playing);
            let (bar_w, gap, max_h) = (2.5 * s, 2.0 * s, 12.0 * s);
            let total = 4.0 * bar_w + 3.0 * gap;
            let x0 = if matches!(content, Content::Media { timer: true }) { left + 4.0 * s } else { right - total };
            // Fully compact and playing: the composition layer draws the moving
            // bars (it cannot fade, so it waits until the island has settled).
            let layered = playing && self.animate_eq && alpha >= 1.0 && !matches!(self.eq_layer, Some(Err(())));
            if layered {
                self.eq_at = Some((x0, mid));
            }
            let heights = match (playing, self.animate_eq) {
                _ if layered => [0.0; 4],
                // Only the timer fallback animates by repainting.
                (true, true) if matches!(self.eq_layer, Some(Err(()))) => equalizer(self.eq_step),
                // Still but clearly "playing": staggered bars, bright.
                (true, _) => [0.55, 0.95, 0.4, 0.75],
                (false, _) => [0.25; 4],
            };
            let color = if playing { rgba(255, 255, 255, 0.9 * alpha) } else { rgba(255, 255, 255, 0.4 * alpha) };
            if let Ok(brush) = cx.gfx.brush(color) {
                for (i, height) in heights.iter().enumerate().filter(|(_, h)| **h > 0.0) {
                    let x = x0 + i as f32 * (bar_w + gap);
                    let bh = max_h * height;
                    unsafe { cx.dc.FillRoundedRectangle(&rounded(rect(x, mid - bh / 2.0, x + bar_w, mid + bh / 2.0), bar_w / 2.0), &brush) };
                }
            }
        }
        if self.pomodoro.is_active() {
            let color = with_alpha(phase_color(self.pomodoro.phase()), alpha);
            let label = pomodoro::minutes(self.pomodoro.remaining_ms(now_ms()));
            let field = rect(w - pad - 40.0 * s, 0.0, right, h);
            text::draw(cx, &label, &formats.compact, field, color);
            if matches!(content, Content::Timer) {
                let glyph = rect(left, 0.0, left + 20.0 * s, h);
                text::draw(cx, GLYPH_TIMER, &formats.icon_small, glyph, color);
            }
            right = field.left;
        }
        let _ = right;
    }

    fn paint_notice(&mut self, cx: &PaintCx, formats: &Formats, notice: &Notice, alpha: f32) {
        let s = cx.scale;
        let (w, h) = (cx.width, cx.height);
        if !matches!(notice.visual, NoticeVisual::Standard) {
            let badge = rect(14.0 * s, (h - 32.0 * s) / 2.0, 46.0 * s, (h + 32.0 * s) / 2.0);
            let (accent, title, detail) = match &notice.visual {
                NoticeVisual::Power(battery) => {
                    let (accent, title) = if battery.plugged {
                        (rgba(48, 209, 88, 1.0), "Charging")
                    } else if battery.percent <= BATTERY_LOW {
                        (rgba(255, 69, 58, 1.0), "On battery")
                    } else {
                        (rgba(255, 255, 255, 0.8), "On battery")
                    };
                    (accent, title, None)
                }
                NoticeVisual::Network { connected, detail } => (
                    if *connected { rgba(64, 156, 255, 1.0) } else { rgba(255, 159, 10, 1.0) },
                    if *connected { "Network connected" } else { "Network disconnected" },
                    Some(detail.as_str()),
                ),
                NoticeVisual::Standard => unreachable!(),
            };
            if let Ok(brush) = cx.gfx.brush(with_alpha(accent, alpha * 0.18)) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(badge, 10.0 * s), &brush) };
            }
            if let NoticeVisual::Power(battery) = &notice.visual {
                // Draw a battery body and charge level, then overlay the bolt.
                let body = rect(badge.left + 5.0*s, badge.top + 10.0*s, badge.right - 7.0*s, badge.bottom - 10.0*s);
                if let Ok(brush) = cx.gfx.brush(with_alpha(accent, alpha)) {
                    unsafe {
                        cx.dc.DrawRoundedRectangle(&rounded(body, 2.5*s), &brush, 1.3*s, None);
                        cx.dc.FillRoundedRectangle(&rounded(rect(body.right + s, body.top + 4.0*s, body.right + 3.0*s, body.bottom - 4.0*s), s), &brush);
                        let inner = rect(body.left + 2.0*s, body.top + 2.0*s, body.left + 2.0*s + (body.right - body.left - 4.0*s) * f32::from(battery.percent.min(100)) / 100.0, body.bottom - 2.0*s);
                        if inner.right > inner.left { cx.dc.FillRoundedRectangle(&rounded(inner, s), &brush); }
                    }
                }
                if battery.plugged {
                    for (x, y) in [(-1.0, 0.0), (1.0, 0.0), (0.0, -1.0), (0.0, 1.0)] {
                        text::draw(cx, crate::ui::glyphs::CHARGING, &formats.icon_small,
                            rect(badge.left + x*s, badge.top + y*s, badge.right + x*s, badge.bottom + y*s),
                            with_alpha(rgba(0, 0, 0, 1.0), alpha));
                    }
                    text::draw(cx, crate::ui::glyphs::CHARGING, &formats.icon_small, badge, with_alpha(TEXT, alpha));
                }
                text::draw(cx, &format!("{}%", battery.percent), &formats.compact,
                    rect(w - 58.0*s, 0.0, w - 16.0*s, h), with_alpha(accent, alpha));
            } else {
                text::draw(cx, notice.glyph, &formats.icon, badge, with_alpha(accent, alpha));
            }
            let right = if matches!(notice.visual, NoticeVisual::Power(_)) { w - 62.0*s } else { w - 16.0*s };
            let left = badge.right + 10.0*s;
            if right > left {
                if let Some(detail) = detail {
                    text::draw(cx, title, &formats.tab, rect(left, h/2.0 - 17.0*s, right, h/2.0), with_alpha(TEXT, alpha));
                    text::draw(cx, detail, &formats.small, rect(left, h/2.0, right, h/2.0 + 16.0*s), with_alpha(DIM, alpha));
                } else {
                    text::draw(cx, title, &formats.artist, rect(left, 0.0, right, h), with_alpha(TEXT, alpha));
                }
            }
            return;
        }
        let pad = 16.0 * s;
        let glyph = rect(pad, 0.0, pad + 22.0 * s, h);
        text::draw(cx, notice.glyph, &formats.icon, glyph, with_alpha(TEXT, alpha));
        let mut text_right = w - pad;
        if let Some(level) = notice.level {
            let bar_w = 84.0 * s;
            let track = rect(w - pad - bar_w, h / 2.0 - 2.0 * s, w - pad, h / 2.0 + 2.0 * s);
            if let Ok(brush) = cx.gfx.brush(with_alpha(CONTROL, alpha)) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(track, 2.0 * s), &brush) };
            }
            if let Ok(brush) = cx.gfx.brush(with_alpha(TEXT, alpha)) {
                let fill = rect(track.left, track.top, track.left + bar_w * level.clamp(0.0, 1.0), track.bottom);
                unsafe { cx.dc.FillRoundedRectangle(&rounded(fill, 2.0 * s), &brush) };
            }
            text_right = track.left - 10.0 * s;
        }
        text::draw(cx, &notice.text, &formats.artist, rect(glyph.right + 8.0 * s, 0.0, text_right, h), with_alpha(TEXT, alpha));
    }

    fn paint_panel(&mut self, cx: &PaintCx, formats: &Formats, alpha: f32) {
        let s = cx.scale;
        let mut p = self.panel;
        if let Some(art) = self.media.as_ref().filter(|m| !m.art_is_icon).and_then(|m| m.art.as_ref()) {
            let ratio = art.width as f32 / art.height.max(1) as f32;
            let width = (90.0 * ratio).min(124.0);
            let height = (width / ratio).min(90.0);
            p.art = rect(16.0 * s, (56.0 + (90.0 - height) / 2.0) * s, (16.0 + width) * s, (56.0 + (90.0 + height) / 2.0) * s);
            p.title.left = p.art.right + 12.0 * s;
            p.artist.left = p.title.left;
        }
        let media = self.media.clone();
        let now = now_ms();

        // Row 1: cover and track.
        self.paint_art(cx, formats, p.art, 10.0 * s, alpha);
        self.paint_week(cx, formats, alpha);
        match &media {
            Some(m) => {
                let artist = if m.artist.is_empty() { m.album.as_str() } else { m.artist.as_str() };
                text::draw(cx, &m.title, &formats.title, p.title, with_alpha(TEXT, alpha));
                text::draw(cx, artist, &formats.artist, p.artist, with_alpha(DIM, alpha));
            }
            None => text::draw(cx, "Nothing playing", &formats.artist, rect(p.title.left, p.art.top, p.title.right, p.art.bottom), with_alpha(DIM, alpha)),
        }

        // Row 2: progress.
        if let Some(m) = media.as_ref().filter(|m| m.has_timeline()) {
            let fraction = (m.position_now() as f32 / m.length_ms as f32).clamp(0.0, 1.0);
            let r = p.bar;
            if let Ok(brush) = cx.gfx.brush(with_alpha(CONTROL, alpha)) {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(r, 2.0 * s), &brush) };
            }
            if let Ok(brush) = cx.gfx.brush(with_alpha(TEXT, alpha)) {
                let fill = rect(r.left, r.top, r.left + (r.right - r.left) * fraction, r.bottom);
                unsafe { cx.dc.FillRoundedRectangle(&rounded(fill, 2.0 * s), &brush) };
            }
            text::draw(cx, &pomodoro::clock(m.position_now()), &formats.small, p.elapsed, with_alpha(DIM, alpha));
            text::draw(cx, &pomodoro::clock(m.length_ms), &formats.small_right, p.length, with_alpha(DIM, alpha));
        }

        // Keep playback centred and give the timer its own quieter surface.
        if let Ok(brush) = cx.gfx.brush(with_alpha(SURFACE, alpha)) {
            let timer_card = rect(16.0 * s, p.phase.top - 3.0 * s, 384.0 * s, p.phase.bottom + 3.0 * s);
            unsafe { cx.dc.FillRoundedRectangle(&rounded(timer_card, 12.0 * s), &brush) };
        }
        if let Ok(brush) = cx.gfx.brush(with_alpha(SURFACE, alpha)) {
            unsafe { cx.dc.FillRoundedRectangle(&rounded(p.play, 12.0 * s), &brush) };
        }
        let button = |this: &Self, r: D2D_RECT_F, hot: Hot, glyph: &str, enabled: bool| {
            let color = if enabled { TEXT } else { rgba(255, 255, 255, 0.25) };
            if enabled && this.hot == Some(hot)
                && let Ok(brush) = cx.gfx.brush(with_alpha(CONTROL, alpha)) {
                    unsafe { cx.dc.FillRoundedRectangle(&rounded(r, 8.0 * s), &brush) };
                }
            text::draw(cx, glyph, &formats.icon, r, with_alpha(color, alpha));
        };
        let m = media.as_ref();
        let playing = m.is_some_and(|m| m.playing);
        button(self, p.prev, Hot::Prev, GLYPH_PREV, m.is_some_and(|m| m.can_prev));
        button(self, p.play, Hot::Play, if playing { GLYPH_PAUSE } else { GLYPH_PLAY }, m.is_some_and(|m| m.can_play_pause));
        button(self, p.next, Hot::Next, GLYPH_NEXT, m.is_some_and(|m| m.can_next));

        let phase = self.pomodoro.phase();
        let label = if phase == Phase::Work { "Focus" } else { "Break" };
        let idle = !self.pomodoro.is_active();
        let color = phase_color(phase);
        let hover = |hot: Hot, r: D2D_RECT_F| {
            if idle
                && self.hot == Some(hot)
                && let Ok(brush) = cx.gfx.brush(with_alpha(CONTROL, alpha))
            {
                unsafe { cx.dc.FillRoundedRectangle(&rounded(r, 8.0 * s), &brush) };
            }
        };
        hover(Hot::Label, p.phase);
        text::draw(cx, label, &formats.small, p.phase, with_alpha(color, alpha));
        for (hot, r, glyph) in [(Hot::Dec, p.dec, "−"), (Hot::Inc, p.inc, "+")] {
            hover(hot, r);
            text::draw(cx, glyph, &formats.time, r, with_alpha(if idle { TEXT } else { rgba(255, 255, 255, 0.2) }, alpha));
        }
        text::draw(cx, &pomodoro::clock(self.pomodoro.remaining_ms(now)), &formats.time, p.time, with_alpha(TEXT, alpha));
        let toggle = if self.pomodoro.is_running() { GLYPH_PAUSE } else { GLYPH_PLAY };
        button(self, p.toggle, Hot::Toggle, toggle, true);
        button(self, p.reset, Hot::Reset, GLYPH_RESET, self.pomodoro.is_active());
    }
}

impl Drop for Island {
    fn drop(&mut self) {
        let hwnd = self.hwnd;
        SHARED.with(|shared| {
            if let Ok(mut shared) = shared.try_borrow_mut()
                && let Some(shared) = shared.as_mut()
            {
                shared.views.retain(|&v| v != hwnd);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lyrics_background_uses_art_and_handles_missing_or_invalid_pixels() {
        let fallback = lyrics_background_pixels(None, &[]);
        assert_eq!(fallback.len(), (LYRICS_BG_WIDTH * LYRICS_BG_HEIGHT * 4) as usize);
        let art = media::Art { width: 1, height: 1, pixels: vec![20, 40, 200, 255] };
        let background = lyrics_background_pixels(Some(&art), &[]);
        assert_ne!(background, fallback);
        assert!(background.chunks_exact(4).all(|pixel| pixel[3] == 255));
        let invalid = media::Art { width: 2, height: 2, pixels: vec![] };
        assert_eq!(lyrics_background_pixels(Some(&invalid), &[]), fallback);
        assert_ne!(lyrics_background_pixels(Some(&art), &[[240, 20, 10]; 12]), background);
    }

    #[test]
    fn horizontal_trackpad_steps_accumulate_and_change_direction() {
        let mut remainder = 0;
        assert_eq!(wheel_step(&mut remainder, 40), 0);
        assert_eq!(wheel_step(&mut remainder, 80), 1);
        assert_eq!(wheel_step(&mut remainder, -60), 0);
        assert_eq!(wheel_step(&mut remainder, -60), -1);
    }
    use crate::services::{Battery, Volume};

    fn monitor() -> RECT {
        RECT { left: 0, top: 0, right: 1920, bottom: 1080 }
    }

    #[test]
    fn frame_is_centred_and_hangs_from_the_top() {
        let r = frame_rect(monitor(), (400.0, 158.0), 1.0, 30.0);
        assert_eq!((r.left, r.right), (760, 1160));
        assert_eq!((r.top, r.bottom), (1, 159));
        let second = RECT { left: 1920, top: 0, right: 3840, bottom: 1080 };
        assert_eq!(frame_rect(second, (92.0, 26.0), 1.0, 30.0).left, 1920 + 960 - 46);
        // The gap scales with DPI.
        assert_eq!(frame_rect(monitor(), (92.0, 26.0), 2.0, 30.0).top, 3);
        assert_eq!(frame_rect(monitor(), (92.0, 26.0), 1.0, 40.0).top, 6);
        assert_eq!(frame_rect(monitor(), (115.0, 32.5), 1.25, 30.0).top, 1);
    }

    #[test]
    fn compact_widths_add_up() {
        assert_eq!(compact_size(Content::Idle).0, IDLE.0);
        assert_eq!(compact_size(Content::Media { timer: true }).0, IDLE.0 + WIDTH_MEDIA + WIDTH_TIMER);
        assert!(compact_size(Content::Timer).0 > compact_size(Content::Idle).0);
    }

    #[test]
    fn widget_order_respects_toggles_and_ignores_duplicates() {
        let mut config = IslandConfig::default();
        config.widget_order = vec!["notes".into(), "files".into(), "notes".into(), "invalid".into()];
        assert_eq!(configured_tabs(&config), vec![Tab::Files, Tab::Media, Tab::Lyrics]);
        config.file_shelf = false;
        assert_eq!(configured_tabs(&config), vec![Tab::Media, Tab::Lyrics]);
    }

    #[test]
    fn hover_area_covers_panel_and_scaled_margin() {
        assert!(in_hover_area(monitor(), 1.0, 30.0, POINT { x: 960, y: 100 }));
        assert!(in_hover_area(monitor(), 1.0, 30.0, POINT { x: 615, y: 4 }));
        assert!(!in_hover_area(monitor(), 1.0, 30.0, POINT { x: 610, y: 4 }));
        assert!(!in_hover_area(monitor(), 1.0, 30.0, POINT { x: 960, y: 260 }));
        assert!(in_hover_area(monitor(), 2.0, 30.0, POINT { x: 550, y: 100 }));
    }

    #[test]
    fn animation_converges_and_snaps() {
        let mut v = 92.0;
        let mut steps = 0;
        while !approach(&mut v, 400.0) {
            steps += 1;
            assert!(steps < 100, "never arrived");
        }
        assert_eq!(v, 400.0);
    }

    #[test]
    fn expansion_spans_idle_to_expanded() {
        assert_eq!(expansion(IDLE.0, 1.0), 0.0);
        assert_eq!(expansion(EXPANDED.0, 1.0), 1.0);
        assert_eq!(expansion(EXPANDED.0 * 2.0, 2.0), 1.0);
        let mid = expansion((IDLE.0 + EXPANDED.0) / 2.0, 1.0);
        assert!((mid - 0.5).abs() < 1e-5);
    }

    #[test]
    fn compact_fractional_dpi_is_fully_opaque() {
        for (width, scale) in [(201.0_f32, 1.25_f32), (203.0, 1.5), (92.0, 1.25)] {
            let actual_client_width = (width * scale).round();
            assert_eq!(expansion_from(actual_client_width, width, scale), 0.0);
            assert_eq!(1.0 - ramp(expansion_from(actual_client_width, width, scale), 0.0, 0.45), 1.0);
        }
    }

    #[test]
    fn panel_buttons_do_not_overlap() {
        let p = PanelLayout::new(1.0);
        let rects = [p.prev, p.play, p.next, p.phase, p.dec, p.time, p.inc, p.toggle, p.reset];
        for (i, a) in rects.iter().enumerate() {
            assert!(a.right <= EXPANDED.0 && a.bottom <= EXPANDED.1);
            for b in &rects[i + 1..] {
                assert!(a.right <= b.left || b.right <= a.left || a.bottom <= b.top || b.bottom <= a.top);
            }
        }
    }

    #[test]
    fn tabs_and_file_rows_stay_inside_expanded_panel() {
        for scale in [1.0, 1.25, 2.0] {
            for index in 0..3 {
                let r = tab_rect(index, scale);
                assert!(r.left >= 0.0 && r.right <= EXPANDED.0 * scale);
                assert!(r.top >= 0.0 && r.bottom <= file_rect(0, scale).top);
            }
            for index in 0..4 {
                let r = file_rect(index, scale);
                assert!(r.bottom <= EXPANDED.1 * scale);
            }
        }
    }

    #[test]
    fn equalizer_stays_in_range_and_moves() {
        for step in 0..50 {
            assert!(equalizer(step).iter().all(|h| (0.2..=1.0).contains(h)));
        }
        assert_ne!(equalizer(1), equalizer(2));
    }

    fn status() -> SystemStatus {
        SystemStatus {
            volume: Some(Volume { level: 0.5, muted: false }),
            battery: Some(Battery { percent: 80, plugged: false, charging: false }),
            brightness: Some(50),
            ..Default::default()
        }
    }

    #[test]
    fn no_notice_without_a_change() {
        assert_eq!(notice_for(&status(), &status()), None);
    }

    #[test]
    fn volume_and_mute_do_not_show_notices() {
        let mut next = status();
        next.volume = Some(Volume { level: 0.62, muted: false });
        assert_eq!(notice_for(&status(), &next), None);
        next.volume = Some(Volume { level: 0.5, muted: true });
        assert_eq!(notice_for(&status(), &next), None);
        next.brightness = Some(60);
        assert_eq!(notice_for(&status(), &next).unwrap().text, "Brightness 60%");
    }

    #[test]
    fn device_change_wins_over_volume() {
        let mut next = status();
        next.audio_device = 1;
        next.volume = Some(Volume { level: 0.9, muted: false });
        assert_eq!(notice_for(&status(), &next).unwrap().text, "Audio output changed");
    }

    #[test]
    fn power_notices() {
        let mut next = status();
        next.battery = Some(Battery { percent: 80, plugged: true, charging: true });
        assert_eq!(notice_for(&status(), &next).unwrap().text, "Charging 80%");
        assert_eq!(notice_for(&status(), &next).unwrap().visual, NoticeVisual::Power(next.battery.unwrap()));
        assert_eq!(notice_for(&next, &status()).unwrap().visual, NoticeVisual::Power(status().battery.unwrap()));

        let mut prev = status();
        prev.battery = Some(Battery { percent: 21, plugged: false, charging: false });
        let mut low = status();
        low.battery = Some(Battery { percent: 20, plugged: false, charging: false });
        assert_eq!(notice_for(&prev, &low).unwrap().text, "Battery low 20%");
        // Draining further does not repeat it.
        let mut lower = status();
        lower.battery = Some(Battery { percent: 19, plugged: false, charging: false });
        assert_eq!(notice_for(&low, &lower), None);

        let mut almost = status();
        almost.battery = Some(Battery { percent: 99, plugged: true, charging: true });
        let mut full = status();
        full.battery = Some(Battery { percent: 100, plugged: true, charging: false });
        assert_eq!(notice_for(&almost, &full).unwrap().text, "Battery full");
        assert_eq!(notice_for(&full, &full), None);
    }

    #[test]
    fn network_notice_ignores_signal() {
        let wifi = |signal| SystemStatus { network: NetState::Wifi { ssid: "home".into(), signal }, ..Default::default() };
        assert_eq!(notice_for(&wifi(40), &wifi(90)), None);
        let other = SystemStatus { network: NetState::Wired, ..Default::default() };
        assert_eq!(notice_for(&wifi(40), &other).unwrap().text, "Ethernet connected");
        assert_eq!(notice_for(&other, &SystemStatus::default()).unwrap().text, "Offline");
        assert_eq!(notice_for(&other, &wifi(40)).unwrap().visual,
            NoticeVisual::Network { connected: true, detail: "home".into() });
        assert_eq!(notice_for(&other, &SystemStatus::default()).unwrap().visual,
            NoticeVisual::Network { connected: false, detail: "No connection".into() });
    }

    #[test]
    fn privacy_notice_only_on_new_use() {
        let off = Sensors::default();
        let mic = Sensors { mic: true, camera: false };
        let both = Sensors { mic: true, camera: true };
        assert_eq!(privacy_notice(off, mic).unwrap().text, "Microphone in use");
        assert_eq!(privacy_notice(mic, both).unwrap().text, "Camera in use");
        assert_eq!(privacy_notice(both, mic), None);
        assert_eq!(privacy_notice(mic, mic), None);
        assert_eq!(dots_width(both), 2.0 * WIDTH_DOT);
    }

    #[test]
    fn brightness_first_reading_is_silent() {
        let mut prev = status();
        prev.brightness = None;
        assert_eq!(notice_for(&prev, &status()), None);
        let mut next = status();
        next.brightness = Some(70);
        assert_eq!(notice_for(&status(), &next).unwrap().text, "Brightness 70%");
    }
}
