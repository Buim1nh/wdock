//! Line-based edits of `config.toml`: one key changes, while comments, unknown
//! keys and the rest of the layout stay as the user wrote them.

use std::fs;
use std::io;

use crate::model::config;

/// The value of `key` in `[section]`, without quotes.
#[cfg(test)]
pub fn get_key(text: &str, section: &str, key: &str) -> Option<String> {
    let mut current = "";
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            current = name.trim();
            continue;
        }
        if current == section
            && let Some((k, v)) = trimmed.split_once('=')
            && k.trim() == key
        {
            let v = v.split('#').next().unwrap_or("").trim();
            return Some(v.trim_matches('"').trim_matches('\'').to_owned());
        }
    }
    None
}

/// Sets `key = value` (`value` already TOML, e.g. `"dark"` or `true`) in
/// `[section]`: the existing line is replaced, or the key is added under the
/// header, or the section is added at the end.
pub fn set_key(text: &str, section: &str, key: &str, value: &str) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_owned).collect();
    let mut current = String::new();
    let mut header = None;
    for (i, line) in lines.iter_mut().enumerate() {
        let trimmed = line.trim();
        if let Some(name) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            current = name.trim().to_owned();
            if current == section {
                header = Some(i);
            }
            continue;
        }
        if current == section
            && let Some((k, _)) = trimmed.split_once('=')
            && k.trim() == key
            && !trimmed.starts_with('#')
        {
            *line = format!("{key} = {value}");
            return lines.join("\n") + "\n";
        }
    }
    match header {
        Some(i) => lines.insert(i + 1, format!("{key} = {value}")),
        None => {
            lines.push(String::new());
            lines.push(format!("[{section}]"));
            lines.push(format!("{key} = {value}"));
        }
    }
    lines.join("\n") + "\n"
}

/// Sets one key in the user's `config.toml`, starting from the template if
/// the file does not exist. Written as UTF-8 without a BOM, which the TOML
/// parser rejects.
pub fn save_key(section: &str, key: &str, value: &str) -> io::Result<()> {
    let path = config::path();
    let text = fs::read_to_string(&path).unwrap_or_else(|_| config::DEFAULT_CONFIG.to_owned());
    let text = text.trim_start_matches('\u{feff}');
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    fs::write(&path, set_key(text, section, key, value))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::config::Config;

    #[test]
    fn set_keeps_comments_and_other_keys() {
        let text = "# top\n[appearance]\n# theme\ntheme = \"auto\"\nbackdrop = \"acrylic\"\n\n[island]\nenabled = true\n";
        let changed = set_key(text, "appearance", "theme", "\"dark\"");
        assert_eq!(get_key(&changed, "appearance", "theme").as_deref(), Some("dark"));
        assert_eq!(get_key(&changed, "appearance", "backdrop").as_deref(), Some("acrylic"));
        assert!(changed.contains("# top\n") && changed.contains("# theme\n"));
        assert_eq!(get_key(&changed, "island", "enabled").as_deref(), Some("true"));
    }

    #[test]
    fn set_adds_missing_key_and_section() {
        let text = "[island]\nenabled = true\n";
        let key = set_key(text, "island", "animate_equalizer", "false");
        assert_eq!(get_key(&key, "island", "animate_equalizer").as_deref(), Some("false"));
        let section = set_key(&key, "dock", "auto_hide", "\"off\"");
        assert_eq!(get_key(&section, "dock", "auto_hide").as_deref(), Some("off"));
        assert_eq!(get_key(&section, "island", "enabled").as_deref(), Some("true"));
    }

    #[test]
    fn every_setting_round_trips_through_the_template() {
        let mut text = config::DEFAULT_CONFIG.to_owned();
        text = set_key(&text, "appearance", "theme", "\"light\"");
        text = set_key(&text, "appearance", "backdrop", "\"solid\"");
        text = set_key(&text, "topbar", "monitors", "\"primary\"");
        text = set_key(&text, "dock", "auto_hide", "\"on\"");
        text = set_key(&text, "island", "enabled", "false");
        text = set_key(&text, "island", "animate_equalizer", "false");
        let config: Config = toml::from_str(&text).expect("edited template parses");
        assert_eq!(config.appearance.theme, config::ThemeMode::Light);
        assert_eq!(config.appearance.backdrop, config::Backdrop::Solid);
        assert_eq!(config.topbar.monitors, config::MonitorScope::Primary);
        assert_eq!(config.dock.auto_hide, config::AutoHide::On);
        assert!(!config.island.enabled && !config.island.animate_equalizer);
        // Comments from the template survive the edits.
        assert!(text.contains("# \"auto\" follows Windows"));
    }

    #[test]
    fn output_has_no_bom() {
        let out = set_key("[island]\nenabled = true\n", "island", "enabled", "false");
        assert!(!out.starts_with('\u{feff}'));
        toml::from_str::<Config>(&out).expect("parses");
    }
}
