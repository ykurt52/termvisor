// Command panel data: pages of up to 50 buttons, stored in commands.toml.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::link;
use crate::settings;
use crate::store::write_private;

pub const MAX_BUTTONS: usize = 50;
const VERSION: u32 = 1;

/// Line ending appended to each line of a text payload.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Ending {
    #[default]
    Cr,
    Lf,
    Crlf,
    None,
}

impl Ending {
    pub const ALL: [Ending; 4] = [Ending::Cr, Ending::Lf, Ending::Crlf, Ending::None];

    pub fn label(self) -> &'static str {
        match self {
            Ending::Cr => "CR",
            Ending::Lf => "LF",
            Ending::Crlf => "CRLF",
            Ending::None => "None",
        }
    }

    fn bytes(self) -> &'static [u8] {
        match self {
            Ending::Cr => b"\r",
            Ending::Lf => b"\n",
            Ending::Crlf => b"\r\n",
            Ending::None => b"",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Button {
    pub label: String,
    /// Text (one or more lines) or hex bytes when `hex` is set.
    pub text: String,
    pub hex: bool,
    pub eol: Ending,
    /// Pause between lines of a multi-line text payload.
    pub delay_ms: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
    /// Keyboard shortcut in GTK accelerator form, e.g. "<Ctrl>1" or "F5".
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

impl Button {
    /// Byte chunks to send, one per line; sent `delay_ms` apart.
    pub fn chunks(&self) -> Result<Vec<Vec<u8>>, String> {
        if self.hex {
            return Ok(vec![link::parse_hex(&self.text)?]);
        }
        let lines: Vec<&str> = self.text.split('\n').map(|l| l.trim_end_matches('\r')).collect();
        lines
            .iter()
            .map(|l| {
                let mut b = link::unescape(l)?;
                b.extend_from_slice(self.eol.bytes());
                Ok(b)
            })
            .collect()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Page {
    pub name: String,
    /// Shown when a pane whose title or target matches one of these globs
    /// gets the focus, e.g. "U-Boot*", "/dev/ttyUSB*", "root@10.0.0.*".
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<String>,
    #[serde(rename = "button", skip_serializing_if = "Vec::is_empty")]
    pub buttons: Vec<Button>,
}

impl Page {
    pub fn matches(&self, title: &str, target: &str) -> bool {
        self.sessions.iter().any(|g| glob(g, title) || glob(g, target))
    }
}

/// Case-insensitive glob with `*` (any run) and `?` (one character).
pub fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.trim().to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    if p.is_empty() {
        return false;
    }
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    #[serde(default, rename = "page")]
    pages: Vec<Page>,
}

pub fn path() -> PathBuf {
    settings::dir().join("commands.toml")
}

pub fn parse(text: &str) -> Result<Vec<Page>, String> {
    let f: File = toml::from_str(text).map_err(|e| e.to_string())?;
    if f.version > VERSION {
        return Err(format!("unsupported version {}", f.version));
    }
    for p in &f.pages {
        if p.buttons.len() > MAX_BUTTONS {
            return Err(format!("page '{}' has more than {MAX_BUTTONS} buttons", p.name));
        }
    }
    Ok(f.pages)
}

pub fn dump(pages: &[Page]) -> Result<String, String> {
    toml::to_string_pretty(&File { version: VERSION, pages: pages.to_vec() }).map_err(|e| e.to_string())
}

/// Pages from commands.toml; one empty page when missing. A broken file is
/// moved aside, never overwritten.
pub fn load() -> Vec<Page> {
    load_from(&path())
}

fn load_from(path: &Path) -> Vec<Page> {
    let fresh = || vec![Page { name: "Commands".into(), ..Default::default() }];
    match fs::read_to_string(path) {
        Ok(t) => match parse(&t) {
            Ok(p) if !p.is_empty() => p,
            Ok(_) => fresh(),
            Err(e) => {
                crate::warn(&format!("{}: {e}", path.display()));
                let _ = fs::rename(path, path.with_extension("toml.bad"));
                fresh()
            }
        },
        Err(e) => {
            if e.kind() != io::ErrorKind::NotFound {
                crate::warn(&format!("{}: {e}", path.display()));
            }
            fresh()
        }
    }
}

pub fn save(pages: &[Page]) -> io::Result<()> {
    let p = path();
    if let Some(d) = p.parent() {
        fs::create_dir_all(d)?;
    }
    write_private(&p, &dump(pages).map_err(io::Error::other)?)
}

/// Pages from an export file (same format as commands.toml).
pub fn import(path: &Path) -> Result<Vec<Page>, String> {
    parse(&fs::read_to_string(path).map_err(|e| e.to_string())?)
}

pub fn export(pages: &[Page], path: &Path) -> Result<(), String> {
    write_private(path, &dump(pages)?).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_and_hex_payloads() {
        let b = Button { text: "setenv a 1\nsaveenv".into(), eol: Ending::Cr, ..Default::default() };
        assert_eq!(b.chunks(), Ok(vec![b"setenv a 1\r".to_vec(), b"saveenv\r".to_vec()]));
        let b = Button { text: "AT\\r\\n".into(), eol: Ending::None, ..Default::default() };
        assert_eq!(b.chunks(), Ok(vec![b"AT\r\n".to_vec()]));
        let b = Button { text: "FE 09 00".into(), hex: true, eol: Ending::Crlf, ..Default::default() };
        assert_eq!(b.chunks(), Ok(vec![vec![0xFE, 0x09, 0x00]]));
        let b = Button { text: "zz".into(), hex: true, ..Default::default() };
        assert!(b.chunks().is_err());
    }

    #[test]
    fn roundtrip_and_limits() {
        let pages = vec![Page {
            name: "U-Boot".into(),
            sessions: vec!["U-Boot*".into()],
            buttons: vec![Button { label: "env".into(), text: "printenv".into(), color: Some("green".into()), ..Default::default() }],
        }];
        let text = dump(&pages).expect("dump");
        assert!(text.contains("[[page.button]]"));
        assert_eq!(parse(&text), Ok(pages));

        let mut big = String::from("version = 1\n[[page]]\nname = \"x\"\n");
        for _ in 0..51 {
            big.push_str("[[page.button]]\nlabel = \"b\"\n");
        }
        assert!(parse(&big).is_err());
    }

    #[test]
    fn session_globs() {
        assert!(glob("U-Boot*", "u-boot ttyUSB0"));
        assert!(glob("/dev/ttyUSB?", "/dev/ttyUSB1"));
        assert!(glob("root@10.0.0.*", "root@10.0.0.17"));
        assert!(glob("*board*", "my-board-2"));
        assert!(!glob("/dev/ttyUSB?", "/dev/ttyUSB10"));
        assert!(!glob("", "x"));
        let p = Page { name: "x".into(), sessions: vec!["*ttyS4*".into()], ..Default::default() };
        assert!(p.matches("Console", "/dev/ttyS4@115200"));
        assert!(!p.matches("Console", "/dev/ttyS1"));
    }

    #[test]
    fn broken_file_is_kept() {
        let d = std::env::temp_dir().join(format!("tv-cmd-{}", std::process::id()));
        fs::create_dir_all(&d).expect("dir");
        let p = d.join("commands.toml");
        fs::write(&p, "version = [").expect("w");
        let pages = load_from(&p);
        assert_eq!(pages.len(), 1);
        assert!(p.with_extension("toml.bad").exists());
        let _ = fs::remove_dir_all(&d);
    }
}
