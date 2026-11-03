// Window actions with their labels and default shortcuts; user changes
// in keybindings.json. The command palette lists the same actions.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use gtk::prelude::*;

use crate::settings;
use crate::store::write_private;

/// Action, label, default shortcuts.
pub const ACTIONS: &[(&str, &str, &[&str])] = &[
    ("win.new-tab", "New Tab", &["<Ctrl><Shift>t"]),
    ("win.close-pane", "Close Pane", &["<Ctrl><Shift>w"]),
    ("win.split-right", "Split Right", &["<Ctrl><Shift>e"]),
    ("win.split-down", "Split Down", &["<Ctrl><Shift>o"]),
    ("win.plot", "Plot Values", &["<Ctrl><Shift>g"]),
    ("win.copy", "Copy", &["<Ctrl><Shift>c"]),
    ("win.paste", "Paste", &["<Ctrl><Shift>v"]),
    ("win.zoom-in", "Zoom In", &["<Ctrl>plus", "<Ctrl>equal"]),
    ("win.zoom-out", "Zoom Out", &["<Ctrl>minus"]),
    ("win.zoom-reset", "Reset Zoom", &["<Ctrl>0"]),
    ("win.quick-connect", "Quick Connect", &["<Ctrl><Shift>l"]),
    ("win.find-session", "Find Session…", &["<Ctrl><Shift>k"]),
    ("win.palette", "Command Palette…", &["<Ctrl><Shift>p"]),
    ("win.toggle-sidebar", "Toggle Sidebar", &["<Ctrl><Shift>b"]),
    ("win.toggle-dock", "Toggle Bottom Panel", &["<Ctrl><Shift>j"]),
    ("win.dock-right", "Panel on Right", &[]),
    ("win.multi-exec", "Multi-Exec", &["<Ctrl><Shift>m"]),
    ("win.import", "Import Sessions…", &[]),
    ("win.export", "Export Sessions…", &[]),
    ("win.restore-tabs", "Restore Tabs on Start", &[]),
    ("win.edit-theme", "Edit Theme…", &[]),
    ("win.preferences", "Preferences…", &["<Ctrl>comma"]),
    ("win.release-all", "Release All Connections…", &["<Ctrl><Shift>r"]),
];

/// Shortcuts changed by the user: action -> keys (empty: none).
pub type Overrides = BTreeMap<String, Vec<String>>;

pub fn path() -> PathBuf {
    settings::dir().join("keybindings.json")
}

pub fn parse(text: &str) -> Result<Overrides, String> {
    let map: Overrides = serde_json::from_str(text).map_err(|e| e.to_string())?;
    for (action, keys) in &map {
        if !ACTIONS.iter().any(|(a, _, _)| a == action) {
            return Err(format!("unknown action {action}"));
        }
        if let Some(bad) = keys.iter().find(|k| gtk::accelerator_parse(k.as_str()).is_none()) {
            return Err(format!("{action}: '{bad}' is not a shortcut"));
        }
    }
    Ok(map)
}

pub fn load() -> Overrides {
    match fs::read_to_string(path()) {
        Ok(t) => parse(&t).unwrap_or_else(|e| {
            crate::warn(&format!("{}: {e}", path().display()));
            Overrides::new()
        }),
        Err(_) => Overrides::new(),
    }
}

pub fn save(map: &Overrides) -> Result<(), String> {
    let p = path();
    if let Some(d) = p.parent() {
        fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    if map.is_empty() {
        let _ = fs::remove_file(&p);
        return Ok(());
    }
    let text = serde_json::to_string_pretty(map).map_err(|e| e.to_string())?;
    write_private(&p, &(text + "\n")).map_err(|e| e.to_string())
}

/// Shortcuts of `action` after the user's changes.
pub fn keys(map: &Overrides, action: &str) -> Vec<String> {
    if let Some(k) = map.get(action) {
        return k.clone();
    }
    ACTIONS.iter().find(|(a, _, _)| *a == action).map(|(_, _, k)| k.iter().map(|s| s.to_string()).collect()).unwrap_or_default()
}

/// Same key on more than one action: (label of the key, labels of the actions).
pub fn conflicts(map: &Overrides) -> Vec<(String, Vec<String>)> {
    let mut by_key: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (action, label, _) in ACTIONS {
        for k in keys(map, action) {
            // The same key can be written in different ways.
            let norm = gtk::accelerator_parse(k.as_str()).map(|(key, m)| gtk::accelerator_name(key, m).to_string()).unwrap_or(k);
            by_key.entry(norm).or_default().push(label.to_string());
        }
    }
    by_key.into_iter().filter(|(_, v)| v.len() > 1).map(|(k, v)| (crate::keys::label(&k), v)).collect()
}

pub fn apply(app: &impl IsA<gtk::Application>, map: &Overrides) {
    for (action, _, _) in ACTIONS {
        let k = keys(map, action);
        let refs: Vec<&str> = k.iter().map(String::as_str).collect();
        app.set_accels_for_action(action, &refs);
    }
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// Needs GTK; run from the GTK test in state.rs.
    pub fn check() {
        let map = parse(r#"{"win.split-right": ["<Ctrl><Alt>Right"], "win.copy": []}"#).expect("parse");
        assert_eq!(keys(&map, "win.split-right"), ["<Ctrl><Alt>Right"]);
        assert!(keys(&map, "win.copy").is_empty());
        assert_eq!(keys(&map, "win.paste"), ["<Ctrl><Shift>v"]);
        assert!(conflicts(&map).is_empty());
        assert!(parse(r#"{"win.nope": []}"#).is_err());
        assert!(parse(r#"{"win.copy": ["<Ctrl>NotAKey"]}"#).is_err());
        let clash = parse(r#"{"win.split-down": ["<Shift><Control>T"]}"#).expect("parse");
        let c = conflicts(&clash);
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].1, ["New Tab", "Split Down"]);
    }
}
