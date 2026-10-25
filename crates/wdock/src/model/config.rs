//! `%APPDATA%\wdock\config.toml`. The file is created from the commented
//! template on first run; missing keys fall back to the template's values.

use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

const DEFAULT_CONFIG: &str = include_str!("default_config.toml");

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
    pub switcher: SwitcherConfig,
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
    pub auto_hide: AutoHide,
    pub pinned: Vec<String>,
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
pub struct SwitcherConfig {
    pub enabled: bool,
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

impl Default for SwitcherConfig {
    fn default() -> Self {
        Self { enabled: true }
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
}
