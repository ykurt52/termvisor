// User settings stored in settings.json.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;

use gtk::glib;
use serde::{Deserialize, Serialize};

use crate::output::LogMode;

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: String,
    pub font: String,
    pub scrollback: i64,
    /// Reopen the last tabs on start.
    pub restore_tabs: bool,
    /// GTK renderer: "auto" (cairo on software OpenGL), "cairo", "gl",
    /// "ngl" or "vulkan". GSK_RENDERER in the environment wins.
    pub renderer: String,
    /// Defaults for new panes: per-line timestamps, highlight rules, log.
    pub timestamps: bool,
    pub highlight: bool,
    pub log: LogMode,
    /// Log folder; empty uses ~/.local/share/termvisor/logs.
    pub log_dir: String,
    /// F1–F12 send command buttons 1–12 of the shown page.
    pub command_keys: bool,
    /// Bottom dock on the right side instead.
    pub dock_right: bool,
    /// adb program; "adb" searches PATH.
    pub adb: String,
    /// Use `theme_light` / `theme_dark` after the system style.
    pub follow_system: bool,
    pub theme_light: String,
    pub theme_dark: String,
    /// Themes for panes whose title or target matches.
    pub theme_rules: Vec<ThemeRule>,
    /// Look for a newer release on every start.
    pub update_check: bool,
}

/// Theme for panes matching comma separated globs, e.g. "root@prod*".
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ThemeRule {
    pub sessions: String,
    pub theme: String,
}

impl ThemeRule {
    pub fn matches(&self, title: &str, target: &str) -> bool {
        self.sessions
            .split(',')
            .map(str::trim)
            .filter(|g| !g.is_empty())
            .any(|g| crate::commands::glob(g, title) || crate::commands::glob(g, target))
    }
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "Dark".into(),
            font: "Monospace 11".into(),
            scrollback: 10_000,
            restore_tabs: false,
            renderer: "auto".into(),
            timestamps: false,
            highlight: true,
            log: LogMode::Off,
            log_dir: String::new(),
            command_keys: false,
            dock_right: false,
            adb: "adb".into(),
            follow_system: false,
            theme_light: "Light".into(),
            theme_dark: "Dark".into(),
            theme_rules: Vec::new(),
            update_check: true,
        }
    }
}

pub fn dir() -> PathBuf {
    glib::user_config_dir().join("termvisor")
}

fn path() -> PathBuf {
    dir().join("settings.json")
}

impl Settings {
    pub fn load() -> Self {
        let path = path();
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Self::default(),
            Err(e) => {
                crate::warn(&format!("{}: {e}", path.display()));
                return Self::default();
            }
        };
        serde_json::from_str(&text).unwrap_or_else(|e| {
            crate::warn(&format!("{}: {e}", path.display()));
            Self::default()
        })
    }

    /// Atomic write with mode 0600.
    pub fn save(&self) -> io::Result<()> {
        fs::create_dir_all(dir())?;
        let path = path();
        let tmp = path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        let mut f = crate::sys::private(&mut OpenOptions::new())
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        fs::rename(&tmp, &path)
    }
}
