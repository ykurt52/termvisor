// Recently opened sessions, kept in $XDG_STATE_HOME/termvisor/recent.json.

use std::fs;
use std::io;
use std::path::PathBuf;

use gtk::glib;
use serde::{Deserialize, Serialize};

use crate::store;
use crate::term::{Kind, Launch};

const MAX: usize = 10;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recent {
    pub title: String,
    pub target: String,
    /// ssh arguments (without the program); None for a local shell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
}

impl Recent {
    pub fn of(l: &Launch) -> Self {
        let ssh = (l.kind == Kind::Ssh).then(|| l.argv.iter().skip(1).cloned().collect());
        Self { title: l.title.clone(), target: l.target.clone(), ssh, dir: l.dir.clone() }
    }

    /// Rebuilt launch. The program is always ssh or the user shell.
    pub fn launch(&self) -> Launch {
        let mut l = match &self.ssh {
            Some(args) => Launch::ssh(args.clone(), &self.target),
            None => Launch::shell(self.dir.clone()),
        };
        l.title = self.title.clone();
        l
    }

    fn same(&self, o: &Recent) -> bool {
        self.ssh == o.ssh && self.dir == o.dir && self.title == o.title
    }
}

fn path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| glib::home_dir().join(".local/state"));
    base.join("termvisor").join("recent.json")
}

pub fn load() -> Vec<Recent> {
    let path = path();
    match fs::read_to_string(&path) {
        Ok(t) => serde_json::from_str(&t).unwrap_or_else(|e| {
            crate::warn(&format!("{}: {e}", path.display()));
            Vec::new()
        }),
        Err(e) => {
            if e.kind() != io::ErrorKind::NotFound {
                crate::warn(&format!("{}: {e}", path.display()));
            }
            Vec::new()
        }
    }
}

pub fn save(list: &[Recent]) -> io::Result<()> {
    let path = path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let text = serde_json::to_string_pretty(list).map_err(io::Error::other)?;
    store::write_private(&path, &text)
}

/// Moves `r` to the front, dropping duplicates and old entries.
pub fn push(list: &mut Vec<Recent>, r: Recent) {
    list.retain(|x| !x.same(&r));
    list.insert(0, r);
    list.truncate(MAX);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn push_dedupes_and_caps() {
        let mut list = Vec::new();
        for i in 0..12 {
            push(&mut list, Recent::of(&Launch::ssh(vec![format!("h{i}")], "x")));
        }
        assert_eq!(list.len(), MAX);
        push(&mut list, Recent::of(&Launch::ssh(vec!["h5".into()], "x")));
        assert_eq!(list.len(), MAX);
        assert_eq!(list[0].ssh.as_deref(), Some(&["h5".to_string()][..]));
        assert_eq!(list.iter().filter(|r| r.ssh == list[0].ssh).count(), 1);
    }

    #[test]
    fn launch_roundtrip() {
        let mut l = Launch::ssh(vec!["-p".into(), "2222".into(), "--".into(), "b".into()], "b:2222");
        l.title = "Board".into();
        let back = Recent::of(&l).launch();
        assert_eq!(back.argv, l.argv);
        assert_eq!(back.title, "Board");
    }
}
