//! `%APPDATA%\wdock\config.toml`. The file is created from the commented
//! template on first run; missing keys fall back to the template's values.

use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

pub const DEFAULT_CONFIG: &str = include_str!("default_config.toml");

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
#[derive(Default)]
pub struct Config {
    pub appearance: Appearance,
    pub performance: Performance,
    pub topbar: TopBarConfig,
    pub tray: TrayConfig,
    pub dock: DockConfig,
    pub workspaces: WorkspacesConfig,
    pub island: IslandConfig,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Appearance {
    pub theme: ThemeMode,
    pub backdrop: Backdrop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    Auto,
    Dark,
    Light,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backdrop {
    Acrylic,
    Solid,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct Performance {
    pub renderer: Renderer,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Renderer {
    Software,
    Gpu,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MonitorScope {
    All,
    Primary,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct TopBarConfig {
    pub height: f32,
    pub monitors: MonitorScope,
    pub date_format: String,
    pub time_format: String,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct TrayConfig {
    pub max_visible: usize,
    pub overflow: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct DockConfig {
    pub icon_size: f32,
    pub monitors: MonitorScope,
    pub windows: DockWindows,
    /// Hovered icons grow up to this factor; 1 turns magnification off.
    pub magnification: f32,
    pub hover_effect: DockHoverEffect,
    /// Hovered icon size in DIPs; None keeps the legacy magnification factor.
    pub hover_size: Option<f32>,
    pub auto_hide: AutoHide,
    pub pinned: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum DockHoverEffect {
    #[default]
    Magnify,
    Glass,
}

impl DockConfig {
    pub fn base_icon_size(&self) -> f32 {
        if self.icon_size.is_finite() { self.icon_size.clamp(16.0, 96.0) } else { 32.0 }
    }

    pub fn hover_icon_size(&self) -> f32 {
        let base = self.base_icon_size();
        let value = self.hover_size.filter(|n| n.is_finite()).unwrap_or_else(|| {
            let factor = if self.magnification.is_finite() { self.magnification.clamp(1.0, 3.0) } else { 2.0 };
            base * factor
        });
        value.clamp(base, (base * 3.0).min(192.0))
    }

    pub fn icon_render_size(&self) -> f32 {
        match self.hover_effect {
            DockHoverEffect::Magnify => self.hover_icon_size(),
            DockHoverEffect::Glass => self.base_icon_size(),
        }
    }

    /// Request a high-resolution shell icon once, then reuse it for hover.
    pub fn icon_source_size(&self, scale: f32) -> u32 {
        let scale = if scale.is_finite() { scale.max(1.0) } else { 1.0 };
        let needed = self.icon_render_size() * scale;
        if needed <= 128.0 { 128 } else if needed <= 256.0 { 256 } else { 512 }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AutoHide {
    Off,
    On,
    /// Hide only while a window overlaps the dock.
    Smart,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct WorkspacesConfig {
    pub count: u8,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(default)]
pub struct IslandConfig {
    pub enabled: bool,
    pub style: IslandStyle,
    pub notch_width: f32,
    /// Pomodoro focus length, minutes.
    pub pomodoro_work: u32,
    /// Pomodoro break length, minutes.
    pub pomodoro_break: u32,
    /// Briefly expand for brightness, power, network and audio-device changes.
    pub notices: bool,
    /// Animate the compact equalizer while media plays. DirectComposition
    /// plays the animation, so wdock itself does no work per frame.
    pub animate_equalizer: bool,
    pub lyrics_background_motion: bool,
    pub file_shelf: bool,
    pub widget_order: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IslandStyle { Island, Notch }

impl IslandConfig {
    pub fn notch_width(&self) -> f32 {
        if self.notch_width.is_finite() { self.notch_width.clamp(160.0, 400.0) } else { 200.0 }
    }
}

// Defaults are spelled out here rather than parsed from the template: serde's
// container-level `default` calls `Default::default()` eagerly, so parsing the
// template inside it would recurse. A test keeps the two in sync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DockWindows {
    All,
    ThisMonitor,
}


impl Default for Appearance {
    fn default() -> Self {
        Self { theme: ThemeMode::Auto, backdrop: Backdrop::Acrylic }
    }
}

impl Default for Performance {
    fn default() -> Self {
        Self { renderer: Renderer::Software }
    }
}

impl Default for TopBarConfig {
    fn default() -> Self {
        Self {
            height: 30.0,
            monitors: MonitorScope::All,
            date_format: "ddd d MMM".into(),
            time_format: "HH:mm".into(),
        }
    }
}

impl Default for TrayConfig {
    fn default() -> Self {
        Self { max_visible: 12, overflow: Vec::new() }
    }
}

impl Default for DockConfig {
    fn default() -> Self {
        Self {
            icon_size: 32.0,
            monitors: MonitorScope::Primary,
            windows: DockWindows::All,
            magnification: 2.0,
            hover_effect: DockHoverEffect::Magnify,
            hover_size: None,
            auto_hide: AutoHide::Smart,
            pinned: [
                r"shell:AppsFolder\Microsoft.Windows.Explorer",
                r"shell:AppsFolder\MSEdge",
                r"shell:AppsFolder\Microsoft.WindowsTerminal_8wekyb3d8bbwe!App",
                r"shell:AppsFolder\windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel",
            ]
            .map(String::from)
            .to_vec(),
        }
    }
}

impl Default for WorkspacesConfig {
    fn default() -> Self {
        Self { count: 4 }
    }
}

impl Default for IslandConfig {
    fn default() -> Self {
        Self {
            enabled: true, style: IslandStyle::Island, notch_width: 200.0,
            pomodoro_work: 25, pomodoro_break: 5, notices: true, animate_equalizer: true,
            file_shelf: true, lyrics_background_motion: true,
            widget_order: vec!["media".into(), "files".into()],
        }
    }
}

pub fn path() -> PathBuf {
    std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(crate::paths::local_dir)
        .join("wdock")
        .join("config.toml")
}

/// Loads the config, writing the default template first if none exists.
/// A broken file is reported in the log and replaced by defaults in memory only.
pub fn load() -> Config {
    let path = path();
    if !path.exists() {
        let written = path
            .parent()
            .map_or(Ok(()), fs::create_dir_all)
            .and_then(|()| fs::write(&path, DEFAULT_CONFIG));
        match written {
            Ok(()) => log::info!("wrote default config to {}", path.display()),
            Err(err) => log::warn!("could not write default config: {err}"),
        }
    }

    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) => {
            log::warn!("could not read {}: {err}; using defaults", path.display());
            return Config::default();
        }
    };
    match toml::from_str(&text) {
        Ok(config) => config,
        Err(err) => {
            log::error!("invalid config {}: {err}; using defaults", path.display());
            Config::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_matches_defaults() {
        let template: Config = toml::from_str(DEFAULT_CONFIG).unwrap();
        assert_eq!(template, Config::default());
    }

    #[test]
    fn partial_file_keeps_defaults() {
        let config: Config = toml::from_str("[dock]\nicon_size = 64\n").unwrap();
        assert_eq!(config.dock.icon_size, 64.0);
        assert_eq!(config.topbar.time_format, "HH:mm");
        assert!(!config.dock.pinned.is_empty());
    }

    #[test]
    fn notch_settings_are_compatible_and_bounded() {
        let config: Config = toml::from_str("[island]\nstyle = 'notch'\nnotch_width = 900.0").unwrap();
        assert_eq!(config.island.style, IslandStyle::Notch);
        assert_eq!(config.island.notch_width(), 400.0);
        let legacy: Config = toml::from_str("[island]\nenabled = true").unwrap();
        assert_eq!(legacy.island.style, IslandStyle::Island);
        assert_eq!(legacy.island.notch_width(), 200.0);
    }

    #[test]
    fn dock_hover_sizes_are_bounded_and_preserve_legacy_factor() {
        let mut dock = DockConfig::default();
        assert_eq!(dock.base_icon_size(), 32.0);
        assert_eq!(dock.hover_icon_size(), 64.0);
        dock.magnification = 1.5;
        assert_eq!(dock.hover_icon_size(), 48.0);
        dock.hover_size = Some(400.0);
        assert_eq!(dock.hover_icon_size(), 96.0);
        dock.hover_size = Some(10.0);
        assert_eq!(dock.hover_icon_size(), 32.0);
        dock.hover_effect = DockHoverEffect::Glass;
        assert_eq!(dock.icon_render_size(), 32.0);
        assert_eq!(dock.icon_source_size(1.25), 128);
        dock.icon_size = f32::NAN;
        assert_eq!(dock.base_icon_size(), 32.0);
    }
}
