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

use windows::Win32::Foundation::{HWND, RECT};
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
use windows::Win32::UI::WindowsAndMessaging::{
    KillTimer, MB_ICONASTERISK, SW_HIDE, SW_SHOWNOACTIVATE, SetTimer, ShowWindow,
};
use windows_numerics::Matrix3x2;

use crate::app;
use crate::model::config::IslandConfig;
use crate::model::pomodoro::{self, Phase, Pomodoro};
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
/// Posted to a view with `bar::command` when a Pomodoro phase ended.
const CMD_WORK_DONE: usize = 1;
const CMD_BREAK_DONE: usize = 2;

const FRAME_MS: u32 = 15;
const EQ_MS: u32 = 150;
const NOTICE_MS: u32 = 2000;
/// Fraction of the remaining distance covered per animation frame.
const EASE: f32 = 0.28;

/// Gap between the monitor's top edge and the pill, DIPs.
const TOP_GAP: f32 = 2.0;
const IDLE: (f32, f32) = (92.0, 26.0);
const WIDTH_MEDIA: f32 = 56.0;
const WIDTH_TIMER: f32 = 64.0;
/// Extra pill width per active privacy dot (microphone, camera).
const WIDTH_DOT: f32 = 14.0;
const NOTICE: (f32, f32) = (280.0, 46.0);
const EXPANDED: (f32, f32) = (400.0, 158.0);
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

fn notify_views() {
    let views = shared(|s| s.views.clone()).unwrap_or_default();
    for hwnd in views {
        bar::notify_model_changed(hwnd);
    }
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
fn frame_rect(monitor: RECT, size: (f32, f32), scale: f32) -> RECT {
    let centre = (monitor.left + monitor.right) as f32 / 2.0;
    let (w, h) = (size.0.round() as i32, size.1.round() as i32);
    let left = (centre - w as f32 / 2.0).round() as i32;
    let top = monitor.top + (TOP_GAP * scale).round() as i32;
    RECT { left, top, right: left + w, bottom: top + h }
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
fn expansion(w: f32, scale: f32) -> f32 {
    expansion_from(w, IDLE.0, scale)
}

/// How far a pill `w` pixels wide is from its compact width `compact` (DIPs)
/// to fully expanded: 0 at rest, 1 open.
fn expansion_from(w: f32, compact: f32, scale: f32) -> f32 {
    let span = (EXPANDED.0 - compact).max(1.0) * scale;
    ((w - compact * scale) / span).clamp(0.0, 1.0)
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
        let r = |l: f32, t: f32, rr: f32, b: f32| rect(l * s, t * s, rr * s, b * s);
        let (w, _) = EXPANDED;
        Self {
            art: r(16.0, 16.0, 80.0, 80.0),
            title: r(92.0, 18.0, w - 16.0, 42.0),
            artist: r(92.0, 42.0, w - 16.0, 62.0),
            bar: r(16.0, 92.0, w - 16.0, 96.0),
            elapsed: r(16.0, 98.0, 100.0, 114.0),
            length: r(w - 100.0, 98.0, w - 16.0, 114.0),
            prev: r(40.0, 118.0, 76.0, 146.0),
            play: r(84.0, 118.0, 124.0, 146.0),
            next: r(132.0, 118.0, 168.0, 146.0),
            phase: r(176.0, 118.0, 228.0, 146.0),
            dec: r(228.0, 118.0, 252.0, 146.0),
            time: r(252.0, 118.0, 308.0, 146.0),
            inc: r(308.0, 118.0, 332.0, 146.0),
            toggle: r(338.0, 118.0, 364.0, 146.0),
            reset: r(366.0, 118.0, 392.0, 146.0),
        }
    }
}

fn contains(r: D2D_RECT_F, x: f32, y: f32) -> bool {
    x >= r.left && x < r.right && y >= r.top && y < r.bottom
}

/// A short announcement.
#[derive(Debug, Clone, PartialEq)]
struct Notice {
    glyph: &'static str,
    text: String,
    /// Level shown as a bar, 0.0 ..= 1.0.
    level: Option<f32>,
}

const BATTERY_LOW: u8 = 20;

/// The most relevant change from `prev` to `next`, if any is worth announcing.
fn notice_for(prev: &SystemStatus, next: &SystemStatus) -> Option<Notice> {
    use crate::ui::glyphs;
    if prev.audio_device != next.audio_device {
        return Some(Notice { glyph: GLYPH_HEADPHONES, text: "Audio output changed".into(), level: None });
    }
    if let (Some(a), Some(b)) = (prev.volume, next.volume)
        && ((a.level - b.level).abs() > 0.004 || a.muted != b.muted)
    {
        let text = if b.muted { "Muted".to_owned() } else { format!("Volume {}%", (b.level * 100.0).round() as u32) };
        return Some(Notice {
            glyph: glyphs::volume(b.level, b.muted),
            text,
            level: Some(if b.muted { 0.0 } else { b.level }),
        });
    }
    if let (Some(a), Some(b)) = (prev.brightness, next.brightness)
        && a != b
    {
        return Some(Notice { glyph: glyphs::BRIGHTNESS, text: format!("Brightness {b}%"), level: Some(f32::from(b) / 100.0) });
    }
    if let (Some(a), Some(b)) = (prev.battery, next.battery) {
        if a.plugged != b.plugged {
            let text = if b.plugged { format!("Charging {}%", b.percent) } else { format!("On battery {}%", b.percent) };
            return Some(Notice { glyph: glyphs::CHARGING, text, level: Some(f32::from(b.percent) / 100.0) });
        }
        if !b.plugged && a.percent > BATTERY_LOW && b.percent <= BATTERY_LOW {
            return Some(Notice {
                glyph: glyphs::CHARGING,
                text: format!("Battery low {}%", b.percent),
                level: Some(f32::from(b.percent) / 100.0),
            });
        }
    }
    if network_kind(&prev.network) != network_kind(&next.network) {
        let (glyph, text) = match &next.network {
            NetState::Wifi { ssid, .. } => (glyphs::WIFI, format!("Wi-Fi {ssid}")),
            NetState::Wired => (glyphs::ETHERNET, "Ethernet connected".to_owned()),
            NetState::Offline => (glyphs::WIFI, "Offline".to_owned()),
        };
        return Some(Notice { glyph, text, level: None });
    }
    None
}

/// Announces a sensor that just came into use.
fn privacy_notice(prev: Sensors, next: Sensors) -> Option<Notice> {
    match (next.mic && !prev.mic, next.camera && !prev.camera) {
        (_, true) => Some(Notice { glyph: GLYPH_CAMERA, text: "Camera in use".into(), level: None }),
        (true, false) => Some(Notice { glyph: GLYPH_MIC, text: "Microphone in use".into(), level: None }),
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
    small: IDWriteTextFormat,
    small_right: IDWriteTextFormat,
    time: IDWriteTextFormat,
    compact: IDWriteTextFormat,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Hot {
    Prev,
    Play,
    Next,
    Seek,
    Toggle,
    Reset,
    Label,
    Dec,
    Inc,
}

pub struct Island {
    hwnd: HWND,
    monitor: RECT,
    scale: f32,
    /// Current window size in pixels.
    size: (f32, f32),
    animating: bool,
    hovered: bool,
    suspended: bool,
    notices: bool,
    animate_eq: bool,
    media: Option<Media>,
    pomodoro: Pomodoro,
    status: SystemStatus,
    privacy: Sensors,
    notice: Option<Notice>,
    art: Option<(u64, ID2D1Bitmap1)>,
    /// Live preview of the playing window, shown in the cover's place while
    /// expanded when the source set no cover; keyed by the track title.
    thumb: Option<(String, Option<Thumbnail>)>,
    eq_step: u32,
    eq_running: bool,
    /// The composition-animated equalizer; `Err` once it failed to build, so
    /// the island falls back to repainting on a timer.
    eq_layer: Option<std::result::Result<EqLayer, ()>>,
    /// Where this paint wants the animated bars (left, middle), if anywhere.
    eq_at: Option<(f32, f32)>,
    formats: Option<Formats>,
    panel: PanelLayout,
    hot: Option<Hot>,
}

pub fn create(gfx: &Rc<Gfx>, theme: &Rc<Theme>, config: &IslandConfig, monitor: &Monitor) -> Result<HWND> {
    let mut island = Island {
        hwnd: HWND::default(),
        monitor: monitor.rect,
        scale: monitor.scale,
        size: (0.0, 0.0),
        animating: false,
        hovered: false,
        suspended: false,
        notices: config.notices,
        animate_eq: config.animate_equalizer,
        media: media::current(),
        pomodoro: pomodoro_state(),
        status: services::status(),
        privacy: privacy::current(),
        notice: None,
        art: None,
        thumb: None,
        eq_step: 0,
        eq_running: false,
        eq_layer: None,
        eq_at: None,
        formats: None,
        panel: PanelLayout::new(monitor.scale),
        hot: None,
    };
    let size = {
        let (w, h) = island.compact();
        (w * monitor.scale, h * monitor.scale)
    };
    island.size = size;
    let rect = frame_rect(monitor.rect, size, monitor.scale);
    let spec = BarSpec { rect, scale: monitor.scale, corners: Corners::None, activate: false };
    bar::create(gfx.clone(), theme.clone(), spec, Box::new(island))
}

impl Island {
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
        (w + dots_width(self.privacy), h)
    }

    /// The size the pill is heading for, in pixels.
    fn target(&self) -> (f32, f32) {
        let (w, h) = if self.hovered {
            EXPANDED
        } else if self.notice.is_some() {
            NOTICE
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
        self.hovered && expansion(self.size.0, self.scale) > 0.9
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
            (media || self.pomodoro.is_running()).then_some(1000)
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
        shared(|s| s.views.push(hwnd));
        self.schedule();
    }

    fn frame(&self) -> Option<RECT> {
        Some(frame_rect(self.monitor, self.size, self.scale))
    }

    fn suspend(&mut self, suspended: bool) -> bool {
        self.suspended = suspended;
        unsafe {
            if suspended {
                for timer in [TIMER_ANIMATE, TIMER_NOTICE, TIMER_TICK, TIMER_EQ] {
                    let _ = KillTimer(Some(self.hwnd), timer);
                }
                self.animating = false;
                self.eq_running = false;
                self.notice = None;
                self.hovered = false;
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
        self.media = media::current();
        self.pomodoro = pomodoro_state();
        let status = services::status();
        let sensors = privacy::current();
        let notice = notice_for(&self.status, &status).or_else(|| privacy_notice(self.privacy, sensors));
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
        if self.notices {
            let text = if id == CMD_WORK_DONE { "Focus done, take a break" } else { "Break over" };
            self.show_notice(Notice { glyph: GLYPH_TIMER, text: text.into(), level: None });
            self.animate();
        }
        false
    }

    fn mouse_move(&mut self, x: f32, y: f32) -> bool {
        let hot = self.hot_at(x, y);
        let hover_changed = !std::mem::replace(&mut self.hovered, true);
        if hover_changed {
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
        self.hovered = false;
        self.hot = None;
        self.animate();
        self.schedule();
        true
    }

    fn mouse_down(&mut self, x: f32, y: f32, button: Button) -> bool {
        if button != Button::Left {
            return false;
        }
        match self.hot_at(x, y) {
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

    fn timer(&mut self, id: usize) -> bool {
        match id {
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
        }
        if let Ok(brush) = cx.gfx.brush(rgba(255, 255, 255, 0.10)) {
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
            self.paint_panel(cx, &formats, panel_alpha);
        }
        self.sync_eq_layer(cx);
        self.sync_thumbnail(panel_alpha);
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
        small: make(text::UI_FONT, 11.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_LEADING, false)?,
        small_right: make(text::UI_FONT, 11.0, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_TEXT_ALIGNMENT_TRAILING, false)?,
        time: make(text::UI_FONT, 15.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER, false)?,
        compact: make(text::UI_FONT, 12.0, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_TRAILING, false)?,
    })
}

const TEXT: D2D1_COLOR_F = rgba(255, 255, 255, 0.95);
const DIM: D2D1_COLOR_F = rgba(255, 255, 255, 0.55);
const CONTROL: D2D1_COLOR_F = rgba(255, 255, 255, 0.14);

impl Island {
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
            cx.dc.CreateBitmap(D2D_SIZE_U { width: art.size, height: art.size }, Some(art.pixels.as_ptr().cast()), art.size * 4, &props)
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
            self.paint_art(cx, formats, dest, 5.0 * s, alpha);
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
        let p = self.panel;
        let media = self.media.clone();
        let now = now_ms();

        // Row 1: cover and track.
        self.paint_art(cx, formats, p.art, 10.0 * s, alpha);
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

        // Row 3: transport and Pomodoro.
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
    use crate::services::{Battery, Volume};

    fn monitor() -> RECT {
        RECT { left: 0, top: 0, right: 1920, bottom: 1080 }
    }

    #[test]
    fn frame_is_centred_and_hangs_from_the_top() {
        let r = frame_rect(monitor(), (400.0, 158.0), 1.0);
        assert_eq!((r.left, r.right), (760, 1160));
        assert_eq!((r.top, r.bottom), (2, 160));
        let second = RECT { left: 1920, top: 0, right: 3840, bottom: 1080 };
        assert_eq!(frame_rect(second, (92.0, 26.0), 1.0).left, 1920 + 960 - 46);
        // The gap scales with DPI.
        assert_eq!(frame_rect(monitor(), (92.0, 26.0), 2.0).top, 4);
    }

    #[test]
    fn compact_widths_add_up() {
        assert_eq!(compact_size(Content::Idle).0, IDLE.0);
        assert_eq!(compact_size(Content::Media { timer: true }).0, IDLE.0 + WIDTH_MEDIA + WIDTH_TIMER);
        assert!(compact_size(Content::Timer).0 > compact_size(Content::Idle).0);
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
    fn volume_and_mute_notices() {
        let mut next = status();
        next.volume = Some(Volume { level: 0.62, muted: false });
        let notice = notice_for(&status(), &next).unwrap();
        assert_eq!(notice.text, "Volume 62%");
        assert_eq!(notice.level, Some(0.62));
        next.volume = Some(Volume { level: 0.5, muted: true });
        assert_eq!(notice_for(&status(), &next).unwrap().text, "Muted");
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

        let mut prev = status();
        prev.battery = Some(Battery { percent: 21, plugged: false, charging: false });
        let mut low = status();
        low.battery = Some(Battery { percent: 20, plugged: false, charging: false });
        assert_eq!(notice_for(&prev, &low).unwrap().text, "Battery low 20%");
        // Draining further does not repeat it.
        let mut lower = status();
        lower.battery = Some(Battery { percent: 19, plugged: false, charging: false });
        assert_eq!(notice_for(&low, &lower), None);
    }

    #[test]
    fn network_notice_ignores_signal() {
        let wifi = |signal| SystemStatus { network: NetState::Wifi { ssid: "home".into(), signal }, ..Default::default() };
        assert_eq!(notice_for(&wifi(40), &wifi(90)), None);
        let other = SystemStatus { network: NetState::Wired, ..Default::default() };
        assert_eq!(notice_for(&wifi(40), &other).unwrap().text, "Ethernet connected");
        assert_eq!(notice_for(&other, &SystemStatus::default()).unwrap().text, "Offline");
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
