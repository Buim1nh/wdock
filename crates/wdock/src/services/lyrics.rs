//! Live lyrics received from the Spotify companion.
use std::sync::Mutex;

use super::media::Media;

#[derive(Clone, Debug, PartialEq)]
pub struct LiveLyrics { pub lines: Vec<String>, pub active: usize, pub source: String, pub colors: Vec<[u8; 3]>, pub progress: Option<f32> }

struct LiveState { title: String, artist: String, lyrics: LiveLyrics, seen: std::time::Instant, changed: std::time::Instant }
static LIVE: Mutex<Option<LiveState>> = Mutex::new(None);
#[derive(Clone, Debug)]
pub struct TimedLine { pub start: u64, pub end: u64, pub text: String, pub words: Vec<(u64, u64, String, bool)> }
static TIMED: Mutex<Option<(String, String, Vec<TimedLine>, Vec<[u8; 3]>)>> = Mutex::new(None);

pub fn accept_timed(title: String, artist: String, lines: Vec<TimedLine>, colors: Vec<[u8; 3]>) -> bool {
    let Some(media) = super::media::current() else { return false; };
    if !matches_spotify_track(&media.source_app_id, &media.title, &media.artist, &title, &artist) || lines.is_empty() || lines.len() > 1000 { return false; }
    if let Ok(mut state) = TIMED.lock() { *state = Some((title, artist, lines, colors)); crate::app::notify_lyrics(); return true; }
    false
}

pub fn timed(media: &Media) -> Option<LiveLyrics> {
    let state = TIMED.lock().ok()?;
    let (title, artist, lines, colors) = state.as_ref()?;
    if !matches_spotify_track(&media.source_app_id, &media.title, &media.artist, title, artist) { return None; }
    let pos = media.position_now();
    let index = lines.partition_point(|line| line.start <= pos).saturating_sub(1);
    let from = index.saturating_sub(4);
    let line = &lines[index];
    let mut progress = 0.0;
    for (i, (start, end, word, joined)) in line.words.iter().enumerate() {
        let amount = if pos >= *end { 1.0 } else if pos <= *start { 0.0 } else { (pos - start) as f32 / (end - start).max(1) as f32 };
        progress += word.chars().count() as f32 * amount;
        if amount < 1.0 { break; }
        if i + 1 < line.words.len() && !joined { progress += 1.0; }
    }
    Some(LiveLyrics { lines: lines.iter().skip(from).take(9).map(|line| line.text.clone()).collect(), active: index - from,
        source: "Spicy Lyrics".into(), colors: colors.clone(), progress: (!line.words.is_empty()).then_some(progress) })
}

fn matches_spotify_track(source: &str, expected_title: &str, expected_artist: &str, title: &str, artist: &str) -> bool {
    source.to_ascii_lowercase().contains("spotify")
        && title.trim().eq_ignore_ascii_case(expected_title.trim())
        && artist.trim().eq_ignore_ascii_case(expected_artist.trim())
}

/// Lyrics Plus sends only what it currently shows; expire when it is closed.
pub fn live(media: &Media) -> Option<LiveLyrics> {
    let state = LIVE.lock().ok()?;
    let live = state.as_ref()?;
    (live.seen.elapsed() < std::time::Duration::from_secs(6)
        && matches_spotify_track(&media.source_app_id, &media.title, &media.artist, &live.title, &live.artist))
        .then(|| live.lyrics.clone())
}

pub fn accept_live(title: String, artist: String, lyrics: LiveLyrics) -> bool {
    let Some(media) = super::media::current() else { return false; };
    if !matches_spotify_track(&media.source_app_id, &media.title, &media.artist, &title, &artist)
        || !matches!(lyrics.source.as_str(), "Spicy Lyrics" | "Lyrics Plus")
        || lyrics.colors.len() > 96 || lyrics.lines.is_empty() || lyrics.lines.len() > 12 || lyrics.active >= lyrics.lines.len()
        || lyrics.progress.is_some_and(|progress| !progress.is_finite() || progress < 0.0 || progress > 256.0)
        || lyrics.lines.iter().any(|line| line.is_empty() || line.len() > 1024)
    { return false; }
    let Ok(mut slot) = LIVE.lock() else { return false; };
    let changed = slot.as_ref().is_none_or(|old| old.title != title || old.artist != artist || old.lyrics != lyrics);
    let now = std::time::Instant::now();
    let motion_changed = slot.as_ref().is_none_or(|old| old.title != title || old.artist != artist
        || old.lyrics.lines.get(old.lyrics.active) != lyrics.lines.get(lyrics.active)
        || old.lyrics.progress != lyrics.progress);
    let last_motion = if motion_changed { now } else { slot.as_ref().map_or(now, |old| old.changed) };
    *slot = Some(LiveState { title, artist, lyrics, seen: now, changed: last_motion });
    drop(slot);
    if changed { crate::app::notify_lyrics(); }
    true
}

