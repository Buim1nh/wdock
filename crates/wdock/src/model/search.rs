//! Launcher search: what can be launched and how well it matches a query.
//!
//! Matching is case-insensitive and tiered: exact name, name prefix, word
//! prefix, acronym ("vsc" for Visual Studio Code), substring, then in-order
//! subsequence. How often the user launched something breaks ties, apps rank
//! above settings pages, and session commands need a deliberate match.

use std::collections::HashMap;

use crate::platform::session::SessionAction;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// `shell:AppsFolder\...`, a path or a URI handed to ShellExecute.
    App(String),
    /// `ms-settings:` page.
    Setting(String),
    Session(SessionAction),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub name: String,
    pub target: Target,
}

impl Candidate {
    /// Key for usage statistics.
    pub fn usage_key(&self) -> String {
        match &self.target {
            Target::App(t) | Target::Setting(t) => t.to_lowercase(),
            Target::Session(action) => format!("session:{action:?}"),
        }
    }
}

const SETTINGS: &[(&str, &str)] = &[
    ("Settings", "ms-settings:"),
    ("Display settings", "ms-settings:display"),
    ("Night light", "ms-settings:nightlight"),
    ("Sound settings", "ms-settings:sound"),
    ("Bluetooth & devices", "ms-settings:bluetooth"),
    ("Wi-Fi settings", "ms-settings:network-wifi"),
    ("Network & internet", "ms-settings:network"),
    ("VPN settings", "ms-settings:network-vpn"),
    ("Personalization", "ms-settings:personalization"),
    ("Background wallpaper", "ms-settings:personalization-background"),
    ("Colors and dark mode", "ms-settings:colors"),
    ("Installed apps", "ms-settings:appsfeatures"),
    ("Default apps", "ms-settings:defaultapps"),
    ("Startup apps", "ms-settings:startupapps"),
    ("Power & battery", "ms-settings:powersleep"),
    ("Storage", "ms-settings:storagesense"),
    ("Mouse settings", "ms-settings:mousetouchpad"),
    ("Keyboard and typing", "ms-settings:typing"),
    ("Language & region", "ms-settings:regionlanguage"),
    ("Date & time", "ms-settings:dateandtime"),
    ("Notifications", "ms-settings:notifications"),
    ("Privacy & security", "ms-settings:privacy"),
    ("Windows Update", "ms-settings:windowsupdate"),
    ("Accounts", "ms-settings:yourinfo"),
    ("Gaming and Game Mode", "ms-settings:gaming-gamemode"),
    ("Graphics settings", "ms-settings:display-advancedgraphics"),
    ("Multiple displays", "ms-settings:display"),
    ("About this PC", "ms-settings:about"),
];

const COMMANDS: &[(&str, SessionAction)] = &[
    ("Lock", SessionAction::Lock),
    ("Sleep", SessionAction::Sleep),
    ("Sign out", SessionAction::SignOut),
    ("Restart", SessionAction::Restart),
    ("Shut down", SessionAction::ShutDown),
];

/// Settings pages and session commands, always searchable.
pub fn builtins() -> Vec<Candidate> {
    SETTINGS
        .iter()
        .map(|(name, uri)| Candidate { name: (*name).into(), target: Target::Setting((*uri).into()) })
        .chain(COMMANDS.iter().map(|(name, action)| Candidate { name: (*name).into(), target: Target::Session(*action) }))
        .collect()
}

fn words(name: &str) -> impl Iterator<Item = &str> {
    name.split(|c: char| c.is_whitespace() || "-_.()&,:".contains(c)).filter(|w| !w.is_empty())
}

/// Match quality of `query` against `name`; both already lower-cased.
pub fn score(query: &str, name: &str) -> Option<i32> {
    if query.is_empty() {
        return None;
    }
    if name == query {
        return Some(1000);
    }
    if name.starts_with(query) {
        return Some(800 - (name.chars().count() - query.chars().count()).min(100) as i32);
    }
    if words(name).any(|w| w.starts_with(query)) {
        return Some(600);
    }
    let acronym: String = words(name).filter_map(|w| w.chars().next()).collect();
    if query.chars().count() >= 2 && acronym.starts_with(query) {
        return Some(500);
    }
    if let Some(position) = name.find(query) {
        return Some(400 - position.min(100) as i32);
    }
    // In-order subsequence, penalised by the gaps between matched characters.
    let mut gaps = 0;
    let mut last = None;
    let mut chars = name.char_indices();
    for q in query.chars() {
        let (index, _) = chars.by_ref().find(|&(_, c)| c == q)?;
        if let Some(prev) = last {
            gaps += index - prev - 1;
        }
        last = Some(index);
    }
    Some(200 - gaps.min(150) as i32)
}

/// Best matches for `query`, highest first. An empty query yields the most
/// used apps, then apps alphabetically.
pub fn search<'a>(query: &str, items: &'a [Candidate], usage: &HashMap<String, u32>, limit: usize) -> Vec<&'a Candidate> {
    let query = query.trim().to_lowercase();
    let uses = |c: &Candidate| usage.get(&c.usage_key()).copied().unwrap_or(0);

    if query.is_empty() {
        let mut apps: Vec<&Candidate> = items.iter().filter(|c| matches!(c.target, Target::App(_))).collect();
        apps.sort_by(|a, b| uses(b).cmp(&uses(a)).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        apps.truncate(limit);
        return apps;
    }

    let mut scored: Vec<(i32, &Candidate)> = items
        .iter()
        .filter_map(|c| {
            let base = score(&query, &c.name.to_lowercase())?;
            let adjusted = match c.target {
                Target::App(_) => base,
                Target::Setting(_) => base - 60,
                // Never surface "Shut down" for a stray subsequence match.
                Target::Session(action) if action.needs_confirmation() && base < 600 => return None,
                Target::Session(_) => base - 40,
            };
            Some((adjusted + uses(c).min(20) as i32 * 8, c))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.len().cmp(&b.1.name.len())));
    scored.into_iter().take(limit).map(|(_, c)| c).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str) -> Candidate {
        Candidate { name: name.into(), target: Target::App(format!(r"shell:AppsFolder\{name}")) }
    }

    #[test]
    fn score_tiers() {
        assert_eq!(score("edge", "edge"), Some(1000));
        assert!(score("mic", "microsoft edge").unwrap() > score("edg", "microsoft edge").unwrap(), "prefix beats word prefix");
        assert_eq!(score("edg", "microsoft edge"), Some(600));
        assert_eq!(score("vsc", "visual studio code"), Some(500));
        assert!(score("soft", "microsoft edge").unwrap() >= 300);
        assert!(score("mse", "microsoft edge").unwrap() < 300, "subsequence ranks last");
        assert_eq!(score("xyz", "microsoft edge"), None);
    }

    #[test]
    fn apps_rank_above_settings_and_usage_breaks_ties() {
        let mut items = vec![app("Display Fusion"), app("Discord")];
        items.extend(builtins());
        let usage = HashMap::new();
        let results = search("dis", &items, &usage, 5);
        assert_eq!(results[0].name, "Discord", "shorter app name first on equal score");
        assert!(results.iter().position(|c| c.name == "Display settings").unwrap() > 1);

        let mut usage = HashMap::new();
        usage.insert(app("Display Fusion").usage_key(), 10);
        assert_eq!(search("dis", &items, &usage, 5)[0].name, "Display Fusion");
    }

    #[test]
    fn destructive_commands_need_a_real_match() {
        let items = builtins();
        let usage = HashMap::new();
        assert!(search("sdn", &items, &usage, 10).iter().all(|c| c.name != "Shut down"));
        assert_eq!(search("shut", &items, &usage, 10)[0].name, "Shut down");
        assert!(search("lk", &items, &usage, 10).iter().any(|c| c.name == "Lock"), "harmless commands match loosely");
    }

    #[test]
    fn empty_query_lists_most_used_apps() {
        let items = vec![app("Zoom"), app("Alpha"), app("Mid")];
        let mut usage = HashMap::new();
        usage.insert(items[0].usage_key(), 3);
        let names: Vec<_> = search("", &items, &usage, 2).iter().map(|c| c.name.clone()).collect();
        assert_eq!(names, ["Zoom", "Alpha"]);
    }
}
