//! Segoe Fluent Icons code points used across the UI.

pub const WIFI: &str = "\u{E701}";
pub const ETHERNET: &str = "\u{E839}";
pub const BRIGHTNESS: &str = "\u{E706}";
pub const LOCK: &str = "\u{E72E}";
pub const SLEEP: &str = "\u{E708}";
pub const SIGN_OUT: &str = "\u{F3B1}";
pub const RESTART: &str = "\u{E777}";
pub const POWER: &str = "\u{E7E8}";
pub const SETTINGS: &str = "\u{E713}";
pub const CHARGING: &str = "\u{E945}";
pub const SEARCH: &str = "\u{E721}";

const MUTED: &str = "\u{E74F}";
const VOLUME_LEVELS: [&str; 4] = ["\u{E992}", "\u{E993}", "\u{E994}", "\u{E995}"];

/// Speaker with 0-3 waves, or crossed out when muted.
pub fn volume(level: f32, muted: bool) -> &'static str {
    if muted {
        return MUTED;
    }
    match level {
        l if l < 0.01 => VOLUME_LEVELS[0],
        l if l < 0.34 => VOLUME_LEVELS[1],
        l if l < 0.67 => VOLUME_LEVELS[2],
        _ => VOLUME_LEVELS[3],
    }
}
