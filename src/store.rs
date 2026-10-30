// Saved sessions: sessions.toml model, load/save and launch resolution.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::settings;
use crate::term::Launch;

const VERSION: u32 = 1;

/// SSH options. Used on sessions and as folder defaults.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SshOpts {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jump: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x11: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
}

impl SshOpts {
    /// Fills unset fields from `parent`.
    fn inherit(&self, parent: &SshOpts) -> SshOpts {
        SshOpts {
            port: self.port.or(parent.port),
            user: self.user.clone().or_else(|| parent.user.clone()),
            key: self.key.clone().or_else(|| parent.key.clone()),
            jump: self.jump.clone().or_else(|| parent.jump.clone()),
            x11: self.x11.or(parent.x11),
            args: self.args.clone().or_else(|| parent.args.clone()),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Folder {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "is_default")]
    pub defaults: SshOpts,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub children: Vec<Entry>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Ssh {
    pub name: String,
    pub host: String,
    #[serde(flatten)]
    pub opts: SshOpts,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub favorite: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Shell {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    #[serde(skip_serializing_if = "is_false")]
    pub favorite: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Entry {
    Folder(Folder),
    Ssh(Ssh),
    Shell(Shell),
}

impl Entry {
    pub fn name(&self) -> &str {
        match self {
            Entry::Folder(f) => &f.name,
            Entry::Ssh(s) => &s.name,
            Entry::Shell(s) => &s.name,
        }
    }

    pub fn color(&self) -> Option<&str> {
        match self {
            Entry::Folder(f) => f.color.as_deref(),
            Entry::Ssh(s) => s.color.as_deref(),
            Entry::Shell(s) => s.color.as_deref(),
        }
    }

    pub fn set_name(&mut self, name: String) {
        match self {
            Entry::Folder(f) => f.name = name,
            Entry::Ssh(s) => s.name = name,
            Entry::Shell(s) => s.name = name,
        }
    }

    pub fn set_favorite(&mut self, on: bool) {
        match self {
            Entry::Folder(_) => {}
            Entry::Ssh(s) => s.favorite = on,
            Entry::Shell(s) => s.favorite = on,
        }
    }

    /// Number of sessions and folders below this entry.
    pub fn count(&self) -> usize {
        match self {
            Entry::Folder(f) => f.children.iter().map(|c| 1 + c.count()).sum(),
            _ => 0,
        }
    }

    pub fn favorite(&self) -> bool {
        match self {
            Entry::Folder(_) => false,
            Entry::Ssh(s) => s.favorite,
            Entry::Shell(s) => s.favorite,
        }
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

fn is_default(o: &SshOpts) -> bool {
    *o == SshOpts::default()
}

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    #[serde(default)]
    node: Vec<Entry>,
}

/// Rejects values that ssh would read as an option.
fn arg_ok(field: &str, v: &str) -> Result<(), String> {
    if v.starts_with('-') || v.chars().any(char::is_control) {
        return Err(format!("invalid {field} '{v}'"));
    }
    Ok(())
}

fn ssh_launch(s: &Ssh, opts: &SshOpts) -> Result<Launch, String> {
    let host = s.host.trim();
    if host.is_empty() {
        return Err(format!("'{}': host is empty", s.name));
    }
    arg_ok("host", host)?;

    let mut args = Vec::new();
    if let Some(p) = opts.port {
        if p == 0 {
            return Err(format!("'{}': port 0", s.name));
        }
        args.extend(["-p".to_string(), p.to_string()]);
    }
    if let Some(u) = &opts.user {
        arg_ok("user", u)?;
        args.extend(["-l".to_string(), u.clone()]);
    }
    if let Some(k) = &opts.key {
        args.extend(["-i".to_string(), expand(k)]);
    }
    if let Some(j) = &opts.jump {
        arg_ok("jump", j)?;
        args.extend(["-J".to_string(), j.clone()]);
    }
    if opts.x11 == Some(true) {
        args.push("-X".into());
    }
    if let Some(extra) = &opts.args {
        args.extend(extra.iter().cloned());
    }
    args.extend(["--".to_string(), host.to_string()]);

    let mut target = host.to_string();
    if let Some(u) = &opts.user {
        target = format!("{u}@{target}");
    }
    if let Some(p) = opts.port.filter(|p| *p != 22) {
        target = format!("{target}:{p}");
    }
    let mut l = Launch::ssh(args, &target);
    l.title = s.name.clone();
    Ok(l)
}

/// Expands a leading `~/`.
fn expand(p: &str) -> String {
    match p.strip_prefix("~/") {
        Some(rest) => gtk::glib::home_dir().join(rest).to_string_lossy().into_owned(),
        None => p.to_string(),
    }
}

/// Launch for the entry at `path`, with folder defaults applied.
pub fn resolve(root: &[Entry], path: &[usize]) -> Result<Launch, String> {
    let mut opts = SshOpts::default();
    let mut list = root;
    let (last, dirs) = path.split_last().ok_or("empty path")?;
    for &i in dirs {
        match list.get(i) {
            Some(Entry::Folder(f)) => {
                opts = f.defaults.inherit(&opts);
                list = &f.children;
            }
            _ => return Err("bad session path".into()),
        }
    }
    match list.get(*last) {
        Some(Entry::Ssh(s)) => ssh_launch(s, &s.opts.inherit(&opts)),
        Some(Entry::Shell(s)) => {
            let mut l = Launch::shell(s.dir.as_deref().map(expand));
            l.title = s.name.clone();
            Ok(l)
        }
        Some(Entry::Folder(_)) => Err("a folder is not a session".into()),
        None => Err("bad session path".into()),
    }
}

pub struct Store {
    pub root: Vec<Entry>,
    path: PathBuf,
}

impl Store {
    pub fn path() -> PathBuf {
        settings::dir().join("sessions.toml")
    }

    /// Loads sessions. A broken file is moved aside, never overwritten.
    pub fn load() -> Self {
        Self::load_from(Self::path())
    }

    fn load_from(path: PathBuf) -> Self {
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) => {
                if e.kind() != io::ErrorKind::NotFound {
                    crate::warn(&format!("{}: {e}", path.display()));
                }
                return Self { root: Vec::new(), path };
            }
        };
        match parse(&text) {
            Ok(root) => Self { root, path },
            Err(e) => {
                crate::warn(&format!("{}: {e}", path.display()));
                let bad = path.with_extension("toml.bad");
                if let Err(e) = fs::rename(&path, &bad) {
                    crate::warn(&format!("keep {}: {e}", bad.display()));
                }
                Self { root: Vec::new(), path }
            }
        }
    }

    /// Entry at `path`.
    pub fn get(&self, path: &[usize]) -> Option<&Entry> {
        let (last, dirs) = path.split_last()?;
        list(&self.root, dirs)?.get(*last)
    }

    /// Child list of the folder at `dir` (root when empty).
    fn kids_mut(&mut self, dir: &[usize]) -> Option<&mut Vec<Entry>> {
        let mut cur = &mut self.root;
        for &i in dir {
            match cur.get_mut(i)? {
                Entry::Folder(f) => cur = &mut f.children,
                _ => return None,
            }
        }
        Some(cur)
    }

    /// Inserts `e` into folder `dir` at `index` (end if None).
    pub fn insert(&mut self, dir: &[usize], index: Option<usize>, e: Entry) -> Result<(), String> {
        let kids = self.kids_mut(dir).ok_or("target folder not found")?;
        let at = index.unwrap_or(kids.len()).min(kids.len());
        kids.insert(at, e);
        Ok(())
    }

    pub fn remove(&mut self, path: &[usize]) -> Result<Entry, String> {
        let (last, dirs) = path.split_last().ok_or("empty path")?;
        let kids = self.kids_mut(dirs).ok_or("session not found")?;
        if *last >= kids.len() {
            return Err("session not found".into());
        }
        Ok(kids.remove(*last))
    }

    /// Moves the entry at `from` into folder `dir` at `index` (end if None).
    /// `dir` and `index` are given in the tree as it is before the move.
    pub fn move_to(&mut self, from: &[usize], dir: &[usize], index: Option<usize>) -> Result<(), String> {
        if dir.starts_with(from) {
            return Err("cannot move a folder into itself".into());
        }
        let (&last, parent) = from.split_last().ok_or("empty path")?;
        if self.get(from).is_none() || self.kids_mut(dir).is_none() {
            return Err("session not found".into());
        }

        // Removing `from` shifts later siblings up by one.
        let mut dir = dir.to_vec();
        let mut index = index;
        if dir.len() > parent.len() && dir.starts_with(parent) && dir[parent.len()] > last {
            dir[parent.len()] -= 1;
        }
        if dir == parent {
            if let Some(i) = index.as_mut() {
                if *i > last {
                    *i -= 1;
                }
            }
        }

        let e = self.remove(from)?;
        if let Err(err) = self.insert(&dir, index, e.clone()) {
            // Put it back where it was.
            let _ = self.insert(parent, Some(last), e);
            return Err(err);
        }
        Ok(())
    }

    /// Replaces the entry at `path`; a folder keeps its children.
    pub fn replace(&mut self, path: &[usize], mut e: Entry) -> Result<(), String> {
        let (last, dirs) = path.split_last().ok_or("empty path")?;
        let kids = self.kids_mut(dirs).ok_or("session not found")?;
        let slot = kids.get_mut(*last).ok_or("session not found")?;
        if let (Entry::Folder(old), Entry::Folder(new)) = (&*slot, &mut e) {
            new.children = old.children.clone();
        }
        *slot = e;
        Ok(())
    }

    /// Atomic write with mode 0600.
    pub fn save(&self) -> io::Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = dump(&self.root).map_err(io::Error::other)?;
        write_private(&self.path, &text)
    }
}

fn list<'a>(root: &'a [Entry], dir: &[usize]) -> Option<&'a [Entry]> {
    let mut cur = root;
    for &i in dir {
        match cur.get(i)? {
            Entry::Folder(f) => cur = &f.children,
            _ => return None,
        }
    }
    Some(cur)
}

/// Path as text, e.g. "0/3/1".
pub fn path_str(path: &[usize]) -> String {
    path.iter().map(usize::to_string).collect::<Vec<_>>().join("/")
}

pub fn path_parse(s: &str) -> Option<Vec<usize>> {
    if s.is_empty() {
        return Some(Vec::new());
    }
    s.split('/').map(|p| p.parse().ok()).collect()
}

pub fn parse(text: &str) -> Result<Vec<Entry>, String> {
    let file: File = toml::from_str(text).map_err(|e| e.to_string())?;
    if file.version > VERSION {
        return Err(format!("unsupported version {}", file.version));
    }
    Ok(file.node)
}

pub fn dump(root: &[Entry]) -> Result<String, String> {
    let file = File { version: VERSION, node: root.to_vec() };
    toml::to_string_pretty(&file).map_err(|e| e.to_string())
}

pub fn write_private(path: &Path, text: &str) -> io::Result<()> {
    let tmp = path.with_extension("tmp");
    let mut f = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(text.as_bytes())?;
    f.sync_all()?;
    fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
version = 1

[[node]]
type = "folder"
name = "Lab"
color = "green"
defaults = { user = "root", jump = "gw", port = 2222 }

  [[node.children]]
  type = "ssh"
  name = "T153 board"
  host = "192.168.1.50"
  favorite = true

  [[node.children]]
  type = "ssh"
  name = "Other"
  host = "10.0.0.9"
  user = "yk"
  port = 22

[[node]]
type = "shell"
name = "Work"
dir = "/tmp"
"#;

    #[test]
    fn parse_sample() {
        let root = parse(SAMPLE).expect("parse");
        assert_eq!(root.len(), 2);
        let Entry::Folder(f) = &root[0] else { panic!("folder") };
        assert_eq!(f.children.len(), 2);
        assert!(f.children[0].favorite());
    }

    #[test]
    fn roundtrip() {
        let root = parse(SAMPLE).expect("parse");
        let text = dump(&root).expect("dump");
        assert_eq!(parse(&text).expect("reparse"), root);
    }

    #[test]
    fn inherits_folder_defaults() {
        let root = parse(SAMPLE).expect("parse");
        let l = resolve(&root, &[0, 0]).expect("launch");
        assert_eq!(
            l.argv,
            ["ssh", "-p", "2222", "-l", "root", "-J", "gw", "--", "192.168.1.50"]
        );
        assert_eq!(l.target, "root@192.168.1.50:2222");
        assert_eq!(l.title, "T153 board");
    }

    #[test]
    fn own_values_win() {
        let root = parse(SAMPLE).expect("parse");
        let l = resolve(&root, &[0, 1]).expect("launch");
        assert_eq!(l.argv, ["ssh", "-p", "22", "-l", "yk", "-J", "gw", "--", "10.0.0.9"]);
        assert_eq!(l.target, "yk@10.0.0.9");
    }

    #[test]
    fn rejects_option_injection() {
        let root = vec![Entry::Ssh(Ssh { name: "x".into(), host: "-oProxyCommand=id".into(), ..Default::default() })];
        assert!(resolve(&root, &[0]).is_err());
    }

    #[test]
    fn folder_is_not_launchable() {
        let root = parse(SAMPLE).expect("parse");
        assert!(resolve(&root, &[0]).is_err());
        assert!(resolve(&root, &[5]).is_err());
    }

    #[test]
    fn edit_ops() {
        let mut st = Store { root: parse(SAMPLE).expect("parse"), path: PathBuf::new() };
        let e = st.remove(&[0, 1]).expect("remove");
        assert_eq!(e.name(), "Other");
        st.insert(&[], Some(0), e).expect("insert");
        assert_eq!(st.get(&[0]).map(Entry::name), Some("Other"));
        assert_eq!(st.get(&[1, 0]).map(Entry::name), Some("T153 board"));

        let renamed = Entry::Folder(Folder { name: "Lab2".into(), ..Default::default() });
        st.replace(&[1], renamed).expect("replace");
        let Some(Entry::Folder(f)) = st.get(&[1]) else { panic!("folder") };
        assert_eq!((f.name.as_str(), f.children.len()), ("Lab2", 1));

        assert!(st.insert(&[0], None, Entry::Shell(Shell::default())).is_err());
        assert!(st.remove(&[9]).is_err());
    }

    fn names(list: &[Entry]) -> Vec<&str> {
        list.iter().map(Entry::name).collect()
    }

    #[test]
    fn move_ops() {
        let mut st = Store { root: parse(SAMPLE).expect("parse"), path: PathBuf::new() };
        // root: [Lab{T153 board, Other}, Work]

        // Reorder down in the same folder: drop below "Other".
        st.move_to(&[0, 0], &[0], Some(2)).expect("down");
        let Some(Entry::Folder(f)) = st.get(&[0]) else { panic!("folder") };
        assert_eq!(names(&f.children), ["Other", "T153 board"]);

        // Shell into the folder that sits before it.
        st.move_to(&[1], &[0], Some(0)).expect("into");
        assert_eq!(names(&st.root), ["Lab"]);
        let Some(Entry::Folder(f)) = st.get(&[0]) else { panic!("folder") };
        assert_eq!(names(&f.children), ["Work", "Other", "T153 board"]);

        // Out to root end.
        st.move_to(&[0, 2], &[], None).expect("out");
        assert_eq!(names(&st.root), ["Lab", "T153 board"]);

        // Folder into itself is refused and nothing changes.
        assert!(st.move_to(&[0], &[0], None).is_err());
        assert!(st.move_to(&[7], &[], None).is_err());
        assert_eq!(names(&st.root), ["Lab", "T153 board"]);
    }

    #[test]
    fn move_into_later_folder() {
        let mut st = Store {
            root: vec![
                Entry::Shell(Shell { name: "a".into(), ..Default::default() }),
                Entry::Folder(Folder { name: "f".into(), ..Default::default() }),
            ],
            path: PathBuf::new(),
        };
        st.move_to(&[0], &[1], None).expect("move");
        assert_eq!(names(&st.root), ["f"]);
        let Some(Entry::Folder(f)) = st.get(&[0]) else { panic!("folder") };
        assert_eq!(names(&f.children), ["a"]);
    }

    #[test]
    fn path_text() {
        assert_eq!(path_str(&[0, 3, 1]), "0/3/1");
        assert_eq!(path_parse("0/3/1"), Some(vec![0, 3, 1]));
        assert_eq!(path_parse(""), Some(vec![]));
        assert_eq!(path_parse("a"), None);
    }

    #[test]
    fn broken_file_is_kept() {
        let dir = std::env::temp_dir().join(format!("tv-store-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("sessions.toml");
        fs::write(&path, "version = [").expect("write");
        let s = Store::load_from(path.clone());
        assert!(s.root.is_empty());
        assert!(path.with_extension("toml.bad").exists());
        let _ = fs::remove_dir_all(&dir);
    }
}
