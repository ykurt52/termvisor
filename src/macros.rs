// Macros: steps that send text, wait, or wait for output, run on a pane.
// Stored in macros.toml. Recording turns typed input into steps.

use std::cell::{Cell, RefCell};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::{glib, prelude::*};
use regex::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::commands::glob;
use crate::store::write_private;
use crate::{link, multi, output, settings};

const VERSION: u32 = 1;
/// A pause this long while recording becomes a Wait step.
const PAUSE: Duration = Duration::from_millis(1000);

fn default_timeout() -> u32 {
    10_000
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "lowercase")]
pub enum Step {
    /// Text with escapes (\r is Enter), sent as if typed.
    Send { text: String },
    Wait { ms: u32 },
    /// Waits until the output since the last Send matches `pattern`.
    Expect {
        pattern: String,
        #[serde(default = "default_timeout")]
        timeout_ms: u32,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Macro {
    pub name: String,
    /// Shortcut in GTK accelerator form.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// Pane title or target globs it is meant for; empty: every pane.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sessions: Vec<String>,
    #[serde(rename = "step", skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Step>,
}

impl Macro {
    pub fn matches(&self, title: &str, target: &str) -> bool {
        self.sessions.is_empty() || self.sessions.iter().any(|g| glob(g, title) || glob(g, target))
    }

    /// Checks escapes and patterns.
    pub fn check(&self) -> Result<(), String> {
        for (i, s) in self.steps.iter().enumerate() {
            let at = |e: String| format!("step {}: {e}", i + 1);
            match s {
                Step::Send { text } => {
                    link::unescape(text).map_err(at)?;
                }
                Step::Expect { pattern, .. } => {
                    Regex::new(pattern).map_err(|e| at(e.to_string()))?;
                }
                Step::Wait { .. } => {}
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    #[serde(default, rename = "macro")]
    macros: Vec<Macro>,
}

pub fn path() -> PathBuf {
    settings::dir().join("macros.toml")
}

pub fn parse(text: &str) -> Result<Vec<Macro>, String> {
    let f: File = toml::from_str(text).map_err(|e| e.to_string())?;
    if f.version > VERSION {
        return Err(format!("unsupported version {}", f.version));
    }
    Ok(f.macros)
}

pub fn dump(macros: &[Macro]) -> Result<String, String> {
    toml::to_string_pretty(&File { version: VERSION, macros: macros.to_vec() }).map_err(|e| e.to_string())
}

/// Macros from macros.toml; a broken file is moved aside.
pub fn load() -> Vec<Macro> {
    load_from(&path())
}

fn load_from(path: &Path) -> Vec<Macro> {
    match fs::read_to_string(path) {
        Ok(t) => parse(&t).unwrap_or_else(|e| {
            crate::warn(&format!("{}: {e}", path.display()));
            let _ = fs::rename(path, path.with_extension("toml.bad"));
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

pub fn save(macros: &[Macro]) -> io::Result<()> {
    let p = path();
    if let Some(d) = p.parent() {
        fs::create_dir_all(d)?;
    }
    write_private(&p, &dump(macros).map_err(io::Error::other)?)
}

/// Typed text in the escaped form of Send steps.
pub fn escape(text: &str) -> String {
    let mut s = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\r' => s.push_str("\\r"),
            '\n' => s.push_str("\\n"),
            '\t' => s.push_str("\\t"),
            '\\' => s.push_str("\\\\"),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => s.push_str(&format!("\\x{:02x}", c as u32)),
            c => s.push(c),
        }
    }
    s
}

/// Steps from recorded input (time since start, text): one Send per line,
/// a Wait for each pause of a second or more.
pub fn steps_from(keys: &[(Duration, String)]) -> Vec<Step> {
    let mut steps = Vec::new();
    let mut text = String::new();
    let mut last: Option<Duration> = None;
    for (at, t) in keys {
        if let Some(prev) = last {
            let gap = at.saturating_sub(prev);
            if gap >= PAUSE {
                if !text.is_empty() {
                    steps.push(Step::Send { text: escape(&std::mem::take(&mut text)) });
                }
                let ms = (gap.as_millis() as u32 / 100) * 100;
                steps.push(Step::Wait { ms });
            }
        }
        last = Some(*at);
        for c in t.chars() {
            text.push(c);
            if c == '\r' {
                steps.push(Step::Send { text: escape(&std::mem::take(&mut text)) });
            }
        }
    }
    if !text.is_empty() {
        steps.push(Step::Send { text: escape(&text) });
    }
    steps
}

struct Rec {
    term: glib::WeakRef<vte::Terminal>,
    start: Instant,
    keys: Vec<(Duration, String)>,
}

thread_local! {
    static REC: RefCell<Option<Rec>> = const { RefCell::new(None) };
}

pub fn recording() -> Option<vte::Terminal> {
    REC.with(|r| r.borrow().as_ref().and_then(|r| r.term.upgrade()))
}

/// Starts recording what is typed in `t`.
pub fn record(t: &vte::Terminal) {
    REC.with(|r| r.replace(Some(Rec { term: t.downgrade(), start: Instant::now(), keys: Vec::new() })));
}

/// Stops recording; returns the steps.
pub fn stop_recording() -> Vec<Step> {
    REC.with(|r| r.take()).map(|r| steps_from(&r.keys)).unwrap_or_default()
}

/// Input typed in `t` (terminal replies already left out).
pub fn recorded(t: &vte::Terminal, text: &str) {
    REC.with(|r| {
        if let Some(rec) = r.borrow_mut().as_mut() {
            if rec.term.upgrade().as_ref() == Some(t) {
                rec.keys.push((rec.start.elapsed(), text.to_string()));
            }
        }
    });
}

async fn pause(ms: u64, stop: &Cell<bool>) -> Result<(), String> {
    let end = Instant::now() + Duration::from_millis(ms);
    while Instant::now() < end {
        if stop.get() {
            return Err("stopped".into());
        }
        let left = end.saturating_duration_since(Instant::now()).min(Duration::from_millis(50));
        glib::timeout_future(left).await;
    }
    Ok(())
}

/// Runs `m` on `t` until done, failed or `stop` is set.
pub async fn run(t: &vte::Terminal, m: &Macro, stop: Rc<Cell<bool>>) -> Result<(), String> {
    m.check()?;
    let out = output::of(t).ok_or("this pane has no output to watch")?;
    let tap = Rc::new(RefCell::new(String::new()));
    out.borrow_mut().taps.push(tap.clone());
    let w = t.downgrade();
    let mut res = Ok(());
    for step in &m.steps {
        let Some(t) = w.upgrade() else {
            res = Err("the pane was closed".into());
            break;
        };
        res = match step {
            Step::Send { text } => {
                // Expect looks only at what comes after this.
                tap.borrow_mut().clear();
                let bytes = link::unescape(text).unwrap_or_default();
                multi::type_into(&t, &String::from_utf8_lossy(&bytes));
                Ok(())
            }
            Step::Wait { ms } => pause(u64::from(*ms), &stop).await,
            Step::Expect { pattern, timeout_ms } => expect(&tap, pattern, *timeout_ms, &stop).await,
        };
        if res.is_err() {
            break;
        }
    }
    out.borrow_mut().taps.retain(|x| !Rc::ptr_eq(x, &tap));
    res
}

async fn expect(tap: &RefCell<String>, pattern: &str, timeout_ms: u32, stop: &Cell<bool>) -> Result<(), String> {
    // ^ and $ match at line starts and ends.
    let re = RegexBuilder::new(pattern).multi_line(true).build().map_err(|e| e.to_string())?;
    let end = Instant::now() + Duration::from_millis(u64::from(timeout_ms));
    loop {
        let hit = re.find(&tap.borrow()).map(|m| m.end());
        if let Some(at) = hit {
            tap.borrow_mut().drain(..at);
            return Ok(());
        }
        if stop.get() {
            return Err("stopped".into());
        }
        if Instant::now() >= end {
            return Err(format!("no match for /{pattern}/ within {} s", timeout_ms / 1000));
        }
        glib::timeout_future(Duration::from_millis(50)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_roundtrip() {
        let m = Macro {
            name: "boot".into(),
            key: Some("<Alt>b".into()),
            sessions: vec!["/dev/ttyUSB*".into()],
            steps: vec![
                Step::Send { text: "\\r".into() },
                Step::Expect { pattern: "=> $".into(), timeout_ms: 5000 },
                Step::Send { text: "printenv\\r".into() },
                Step::Wait { ms: 300 },
            ],
        };
        let text = dump(std::slice::from_ref(&m)).expect("dump");
        assert!(text.contains("[[macro.step]]"));
        assert!(text.contains("do = \"expect\""));
        assert_eq!(parse(&text), Ok(vec![m.clone()]));
        assert!(m.check().is_ok());
        let bad = Macro { steps: vec![Step::Expect { pattern: "(".into(), timeout_ms: 1 }], ..Default::default() };
        assert!(bad.check().unwrap_err().starts_with("step 1:"));
        assert!(m.matches("x", "/dev/ttyUSB0 115200 8N1"));
        assert!(!m.matches("x", "local"));
    }

    #[test]
    fn recording_to_steps() {
        let ms = Duration::from_millis;
        let keys = vec![
            (ms(0), "l".to_string()),
            (ms(80), "s".into()),
            (ms(150), "\r".into()),
            (ms(2450), "\x03".into()),
            (ms(2500), "cd /tmp\r".into()),
        ];
        assert_eq!(
            steps_from(&keys),
            vec![
                Step::Send { text: "ls\\r".into() },
                Step::Wait { ms: 2300 },
                Step::Send { text: "\\x03cd /tmp\\r".into() },
            ]
        );
        assert_eq!(link::unescape(&escape("a\\b\t\x1b[A\r")).expect("roundtrip"), b"a\\b\t\x1b[A\r");
    }
}
