// User settings stored in settings.json.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;

use gtk::glib;
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub theme: String,
    pub font: String,
    pub scrollback: i64,
    /// Reopen the last tabs on start.
    pub restore_tabs: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "Dark".into(),
            font: "Monospace 11".into(),
            scrollback: 10_000,
            restore_tabs: false,
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
        let mut f = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.sync_all()?;
        fs::rename(&tmp, &path)
    }
}
