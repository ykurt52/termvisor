// Output pipeline of a pane: per-line timestamps, highlight rules and
// logging to a file. Runs on the GTK thread before bytes reach the screen.

use std::borrow::Cow;
use std::cell::RefCell;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::{glib, prelude::*};
use regex::bytes::{Regex, RegexBuilder};
use serde::{Deserialize, Serialize};

use crate::settings;
use crate::store::write_private;

const ESC: u8 = 0x1b;
const RESET: &[u8] = b"\x1b[0m";
const VERSION: u32 = 1;

/// Rule colors, in the order the UI lists them.
pub const COLORS: [&str; 6] = ["red", "green", "yellow", "blue", "magenta", "cyan"];

fn fg(color: &str) -> u8 {
    match color {
        "green" => 32,
        "yellow" => 33,
        "blue" => 34,
        "magenta" => 35,
        "cyan" => 36,
        _ => 31,
    }
}

/// Highlight rule: matches of `pattern` are shown in `color`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Rule {
    pub pattern: String,
    pub color: String,
    pub bold: bool,
    pub ignore_case: bool,
    pub enabled: bool,
}

impl Default for Rule {
    fn default() -> Self {
        Self { pattern: String::new(), color: "red".into(), bold: false, ignore_case: true, enabled: true }
    }
}

impl Rule {
    fn new(pattern: &str, color: &str, bold: bool) -> Self {
        Self { pattern: pattern.into(), color: color.into(), bold, ..Default::default() }
    }

    fn compile(&self) -> Result<Mark, String> {
        let re = RegexBuilder::new(&self.pattern)
            .case_insensitive(self.ignore_case)
            .build()
            .map_err(|e| format!("{}: {e}", self.pattern))?;
        let bold = if self.bold { "1;" } else { "" };
        Ok(Mark { re, sgr: format!("\x1b[{bold}{}m", fg(&self.color)).into_bytes() })
    }
}

pub fn default_rules() -> Vec<Rule> {
    vec![
        Rule::new(r"\b(error|errors|fail|failed|failure|fatal|panic|exception|denied|refused)\b", "red", true),
        Rule::new(r"\b(warn|warning|warnings|deprecated)\b", "yellow", false),
        Rule::new(r"\b(ok|done|success|successful|pass|passed)\b", "green", false),
        Rule::new(r"\b\d{1,3}(\.\d{1,3}){3}\b", "cyan", false),
    ]
}

/// Checks a rule's pattern.
pub fn check(rule: &Rule) -> Result<(), String> {
    rule.compile().map(|_| ())
}

#[derive(Serialize, Deserialize)]
struct RulesFile {
    version: u32,
    #[serde(default, rename = "rule")]
    rules: Vec<Rule>,
}

pub fn rules_path() -> PathBuf {
    settings::dir().join("highlight.toml")
}

fn parse_rules(text: &str) -> Result<Vec<Rule>, String> {
    let f: RulesFile = toml::from_str(text).map_err(|e| e.to_string())?;
    if f.version > VERSION {
        return Err(format!("unsupported version {}", f.version));
    }
    Ok(f.rules)
}

/// Rules from highlight.toml; the defaults when missing. A broken file is
/// moved aside, never overwritten.
pub fn load_rules() -> Vec<Rule> {
    let path = rules_path();
    match fs::read_to_string(&path) {
        Ok(t) => parse_rules(&t).unwrap_or_else(|e| {
            crate::warn(&format!("{}: {e}", path.display()));
            let _ = fs::rename(&path, path.with_extension("toml.bad"));
            default_rules()
        }),
        Err(e) => {
            if e.kind() != io::ErrorKind::NotFound {
                crate::warn(&format!("{}: {e}", path.display()));
            }
            default_rules()
        }
    }
}

pub fn save_rules(rules: &[Rule]) -> Result<(), String> {
    let text = toml::to_string_pretty(&RulesFile { version: VERSION, rules: rules.to_vec() }).map_err(|e| e.to_string())?;
    let p = rules_path();
    if let Some(d) = p.parent() {
        fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    write_private(&p, &text).map_err(|e| e.to_string())
}

struct Mark {
    re: Regex,
    sgr: Vec<u8>,
}

/// Pipelines by terminal.
type Outs = Vec<(glib::WeakRef<crate::vt::View>, Rc<RefCell<Out>>)>;

thread_local! {
    static MARKS: RefCell<Rc<Vec<Mark>>> = RefCell::new(Rc::new(Vec::new()));
    static OUTS: RefCell<Outs> = const { RefCell::new(Vec::new()) };
}

/// Makes `rules` active in all panes. Rules that do not compile are
/// skipped; their errors are returned.
pub fn set_rules(rules: &[Rule]) -> Vec<String> {
    let mut marks = Vec::new();
    let mut errors = Vec::new();
    for r in rules.iter().filter(|r| r.enabled && !r.pattern.is_empty()) {
        match r.compile() {
            Ok(m) => marks.push(m),
            Err(e) => errors.push(e),
        }
    }
    MARKS.with(|m| m.replace(Rc::new(marks)));
    errors
}

/// Length of the escape sequence at the start of `s` (which starts with
/// ESC). An unfinished sequence takes the rest of `s`.
fn esc_len(s: &[u8]) -> usize {
    match s.get(1) {
        None => 1,
        Some(b'[') => s[2..].iter().position(|b| (0x40..=0x7e).contains(b)).map_or(s.len(), |i| i + 3),
        Some(b']' | b'P' | b'_' | b'^') => {
            let mut i = 2;
            while i < s.len() {
                if s[i] == 0x07 {
                    return i + 1;
                }
                if s[i] == ESC && s.get(i + 1) == Some(&b'\\') {
                    return i + 2;
                }
                i += 1;
            }
            s.len()
        }
        Some(b'(' | b')' | b'#' | b'%') => s.len().min(3),
        Some(_) => 2,
    }
}

const ALT_ON: [&[u8]; 3] = [b"\x1b[?1049h", b"\x1b[?1047h", b"\x1b[?47h"];
const ALT_OFF: [&[u8]; 3] = [b"\x1b[?1049l", b"\x1b[?1047l", b"\x1b[?47l"];

/// Alternate screen switches in `s`: (end offset, on).
fn alt_switches(s: &[u8]) -> Vec<(usize, bool)> {
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(off) = s[i..].iter().position(|&b| b == ESC) {
        let at = i + off;
        let rest = &s[at..];
        if let Some(p) = ALT_ON.iter().find(|p| rest.starts_with(p)) {
            out.push((at + p.len(), true));
        } else if let Some(p) = ALT_OFF.iter().find(|p| rest.starts_with(p)) {
            out.push((at + p.len(), false));
        }
        i = at + 1;
    }
    out
}

fn now_stamp() -> String {
    match glib::DateTime::now_local() {
        Ok(t) => format!("{}.{:03}", t.format("%H:%M:%S").map(|s| s.to_string()).unwrap_or_default(), t.microsecond() / 1000),
        Err(_) => "--:--:--.---".into(),
    }
}

/// Screen-side processing: timestamps and highlight. Both stay off while
/// a full-screen program uses the alternate screen.
pub struct Filter {
    pub stamps: bool,
    pub marks: bool,
    at_start: bool,
    alt: bool,
    /// SGR sequences in effect since the last reset; restored after a mark.
    sgr: Vec<u8>,
    /// Last bytes of the previous chunk, to find a switch split in two.
    tail: Vec<u8>,
    clock: fn() -> String,
    /// Stamp of the chunk being processed; its lines arrived together.
    now: Option<String>,
}

impl Filter {
    pub fn new(stamps: bool, marks: bool) -> Self {
        Self { stamps, marks, at_start: true, alt: false, sgr: Vec::new(), tail: Vec::new(), clock: now_stamp, now: None }
    }

    pub fn apply<'a>(&mut self, data: &'a [u8]) -> Cow<'a, [u8]> {
        self.now = None;
        // A switch that started in the previous chunk.
        if !self.tail.is_empty() {
            let mut joined = self.tail.clone();
            joined.extend_from_slice(&data[..data.len().min(8)]);
            let cut = self.tail.len();
            if let Some(&(_, on)) = alt_switches(&joined).iter().rev().find(|(end, _)| *end > cut) {
                self.set_alt(on);
            }
        }
        let keep = data.len().min(7);
        self.tail = data[data.len() - keep..].to_vec();

        let marks = if self.marks { MARKS.with(|m| m.borrow().clone()) } else { Rc::new(Vec::new()) };
        let switches = alt_switches(data);
        if !self.stamps && marks.is_empty() {
            if let Some(&(_, on)) = switches.last() {
                self.set_alt(on);
            }
            self.track(data);
            return Cow::Borrowed(data);
        }
        let mut out = Vec::with_capacity(data.len() + 64);
        let mut pos = 0;
        for (end, on) in switches {
            self.segment(&data[pos..end], &marks, &mut out);
            self.set_alt(on);
            pos = end;
        }
        self.segment(&data[pos..], &marks, &mut out);
        Cow::Owned(out)
    }

    fn set_alt(&mut self, on: bool) {
        if self.alt && !on {
            self.at_start = false;
        }
        self.alt = on;
    }

    /// Keeps line start and SGR state while nothing is changed.
    fn track(&mut self, data: &[u8]) {
        if let Some(&last) = data.last() {
            self.at_start = last == b'\n';
        }
    }

    fn segment(&mut self, seg: &[u8], marks: &[Mark], out: &mut Vec<u8>) {
        if self.alt {
            out.extend_from_slice(seg);
            return;
        }
        for piece in seg.split_inclusive(|&b| b == b'\n') {
            self.mark(piece, marks, out);
            if piece.ends_with(b"\n") {
                self.at_start = true;
            }
        }
    }

    /// Copies `piece`, coloring rule matches in the text between escapes.
    /// The timestamp goes before the first text of a line, so escapes
    /// alone (like a screen switch) never get one.
    fn mark(&mut self, piece: &[u8], marks: &[Mark], out: &mut Vec<u8>) {
        let mut i = 0;
        while i < piece.len() {
            if piece[i] == ESC {
                let n = esc_len(&piece[i..]);
                let seq = &piece[i..i + n];
                if seq.starts_with(b"\x1b[") && seq.ends_with(b"m") {
                    if seq == b"\x1b[m" || seq == RESET {
                        self.sgr.clear();
                    } else {
                        self.sgr.extend_from_slice(seq);
                    }
                }
                out.extend_from_slice(seq);
                i += n;
                continue;
            }
            let end = piece[i..].iter().position(|&b| b == ESC).map_or(piece.len(), |o| i + o);
            if self.stamps && self.at_start {
                out.extend_from_slice(b"\x1b[2m");
                let clock = self.clock;
                out.extend_from_slice(self.now.get_or_insert_with(clock).as_bytes());
                out.extend_from_slice(RESET);
                out.extend_from_slice(&self.sgr);
                out.push(b' ');
            }
            self.at_start = false;
            self.paint(&piece[i..end], marks, out);
            i = end;
        }
    }

    fn paint(&self, run: &[u8], marks: &[Mark], out: &mut Vec<u8>) {
        if marks.is_empty() {
            out.extend_from_slice(run);
            return;
        }
        let mut hits: Vec<(usize, usize, usize)> = Vec::new();
        for (k, m) in marks.iter().enumerate() {
            hits.extend(m.re.find_iter(run).filter(|h| !h.is_empty()).map(|h| (h.start(), h.end(), k)));
        }
        if hits.is_empty() {
            out.extend_from_slice(run);
            return;
        }
        // Earlier start wins; on a tie the earlier rule wins.
        hits.sort_by_key(|&(s, _, k)| (s, k));
        let mut at = 0;
        for (s, e, k) in hits {
            if s < at {
                continue;
            }
            out.extend_from_slice(&run[at..s]);
            out.extend_from_slice(&marks[k].sgr);
            out.extend_from_slice(&run[s..e]);
            out.extend_from_slice(RESET);
            out.extend_from_slice(&self.sgr);
            at = e;
        }
        out.extend_from_slice(&run[at..]);
    }
}

/// Removes escape sequences and carriage returns, across chunks.
#[derive(Default)]
struct Strip {
    /// Bytes of an escape sequence not finished in the last chunk.
    pending: Vec<u8>,
}

impl Strip {
    fn feed(&mut self, data: &[u8], out: &mut Vec<u8>) {
        let joined;
        let data = if self.pending.is_empty() {
            data
        } else {
            joined = [std::mem::take(&mut self.pending).as_slice(), data].concat();
            &joined[..]
        };
        let mut i = 0;
        while i < data.len() {
            let b = data[i];
            if b == ESC {
                let n = esc_len(&data[i..]);
                if i + n == data.len() && !finished(&data[i..]) {
                    self.pending = data[i..].to_vec();
                    return;
                }
                i += n;
                continue;
            }
            if b == b'\n' || b == b'\t' || (b >= 0x20 && b != 0x7f) {
                out.push(b);
            }
            i += 1;
        }
    }
}

/// True when the escape sequence `s` is complete.
fn finished(s: &[u8]) -> bool {
    match s.get(1) {
        None => false,
        Some(b'[') => s.len() > 2 && (0x40..=0x7e).contains(&s[s.len() - 1]),
        Some(b']' | b'P' | b'_' | b'^') => s.ends_with(&[0x07]) || s.ends_with(b"\x1b\\"),
        Some(b'(' | b')' | b'#' | b'%') => s.len() >= 3,
        Some(_) => true,
    }
}

/// How a pane is logged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogMode {
    #[default]
    Off,
    /// Bytes as received.
    Raw,
    /// Text without escape sequences, with timestamps when they are on.
    Plain,
}

impl LogMode {
    pub const ALL: [LogMode; 3] = [LogMode::Off, LogMode::Raw, LogMode::Plain];

    pub fn label(self) -> &'static str {
        match self {
            LogMode::Off => "Off",
            LogMode::Raw => "Raw",
            LogMode::Plain => "Plain text",
        }
    }
}

pub fn log_dir() -> PathBuf {
    let custom = settings::Settings::load().log_dir;
    if !custom.is_empty() {
        return PathBuf::from(custom);
    }
    glib::user_data_dir().join("termvisor").join("logs")
}

/// File-name safe version of a pane title.
fn file_name(title: &str) -> String {
    let s: String = title
        .chars()
        .map(|c| if c.is_alphanumeric() || "-_.@".contains(c) { c } else { '_' })
        .collect();
    let s = s.trim_matches(['_', '.']).to_string();
    if s.is_empty() { "session".into() } else { s.chars().take(60).collect() }
}

pub struct Log {
    w: BufWriter<File>,
    pub path: PathBuf,
    plain: bool,
    strip: Strip,
    at_start: bool,
}

impl Log {
    pub fn open(dir: &Path, title: &str, plain: bool) -> io::Result<Self> {
        fs::DirBuilder::new().recursive(true).create(dir)?;
        let when = glib::DateTime::now_local()
            .ok()
            .and_then(|t| t.format("%Y%m%d-%H%M%S").ok())
            .map(|s| s.to_string())
            .unwrap_or_default();
        let path = dir.join(format!("{}-{when}.log", file_name(title)));
        let f = crate::sys::private(&mut OpenOptions::new()).create(true).append(true).open(&path)?;
        Ok(Self { w: BufWriter::new(f), path, plain, strip: Strip::default(), at_start: true })
    }

    fn write(&mut self, data: &[u8], stamps: bool, clock: fn() -> String) -> io::Result<()> {
        if !self.plain {
            self.w.write_all(data)?;
        } else {
            let mut text = Vec::with_capacity(data.len());
            self.strip.feed(data, &mut text);
            let mut now: Option<String> = None;
            for piece in text.split_inclusive(|&b| b == b'\n') {
                if stamps && self.at_start {
                    write!(self.w, "[{}] ", now.get_or_insert_with(clock))?;
                }
                self.w.write_all(piece)?;
                self.at_start = piece.ends_with(b"\n");
            }
        }
        // Each chunk reaches the disk, so a crash loses nothing.
        self.w.flush()
    }
}

impl Drop for Log {
    fn drop(&mut self) {
        let _ = self.w.flush();
    }
}

/// Pipeline state of one pane.
pub struct Out {
    pub title: String,
    pub filter: Filter,
    pub log: Option<Log>,
    /// Text of the output without escape sequences, one buffer per running
    /// macro.
    pub taps: Vec<Rc<RefCell<String>>>,
    /// Get the same text as it arrives (the plot).
    pub watchers: Vec<std::rc::Weak<dyn Watch>>,
    tap_strip: Strip,
}

/// Receives a pane's output as text without escape sequences.
pub trait Watch {
    fn text(&self, text: &str);
}

impl Out {
    /// Logs `data` and returns what the screen shows. A log that fails to
    /// write is closed with a warning.
    pub fn process<'a>(&mut self, data: &'a [u8]) -> Cow<'a, [u8]> {
        self.record(data);
        self.filter.apply(data)
    }

    /// Logs without screen processing (hex and MAVLink views).
    pub fn record(&mut self, data: &[u8]) {
        self.watchers.retain(|w| w.strong_count() > 0);
        if !self.taps.is_empty() || !self.watchers.is_empty() {
            let mut text = Vec::with_capacity(data.len());
            self.tap_strip.feed(data, &mut text);
            let text = String::from_utf8_lossy(&text);
            for tap in &self.taps {
                tap.borrow_mut().push_str(&text);
            }
            let watchers: Vec<_> = self.watchers.iter().filter_map(|w| w.upgrade()).collect();
            for w in watchers {
                w.text(&text);
            }
        }
        let stamps = self.filter.stamps;
        let clock = self.filter.clock;
        if let Some(l) = self.log.as_mut() {
            if let Err(e) = l.write(data, stamps, clock) {
                crate::warn(&format!("log {}: {e}; logging stopped", l.path.display()));
                self.log = None;
            }
        }
    }

    pub fn mode(&self) -> LogMode {
        match &self.log {
            None => LogMode::Off,
            Some(l) if l.plain => LogMode::Plain,
            Some(_) => LogMode::Raw,
        }
    }

    pub fn set_mode(&mut self, mode: LogMode) -> io::Result<()> {
        if mode == self.mode() {
            return Ok(());
        }
        self.log = None;
        if mode != LogMode::Off {
            self.log = Some(Log::open(&log_dir(), &self.title, mode == LogMode::Plain)?);
        }
        Ok(())
    }
}

/// Creates the pipeline of terminal `t` with the defaults from settings.
pub fn attach(t: &crate::vt::View, title: &str, cfg: &settings::Settings) -> Rc<RefCell<Out>> {
    let mut out = Out {
        title: title.to_string(),
        filter: Filter::new(cfg.timestamps, cfg.highlight),
        log: None,
        taps: Vec::new(),
        watchers: Vec::new(),
        tap_strip: Strip::default(),
    };
    if let Err(e) = out.set_mode(cfg.log) {
        crate::warn(&format!("log for {title}: {e}"));
    }
    let out = Rc::new(RefCell::new(out));
    OUTS.with(|v| {
        let mut v = v.borrow_mut();
        v.retain(|(w, _)| w.upgrade().is_some());
        v.push((t.downgrade(), out.clone()));
    });
    out
}

/// Pipeline of terminal `t`, if it has one.
pub fn of(t: &crate::vt::View) -> Option<Rc<RefCell<Out>>> {
    OUTS.with(|v| v.borrow().iter().find(|(w, _)| w.upgrade().as_ref() == Some(t)).map(|(_, o)| o.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clock() -> String {
        "T".into()
    }

    fn filter(stamps: bool, marks: bool) -> Filter {
        let mut f = Filter::new(stamps, marks);
        f.clock = clock;
        f
    }

    fn rules() {
        let errs = set_rules(&[Rule::new(r"\berror\b", "red", true), Rule::new("ok", "green", false)]);
        assert!(errs.is_empty());
    }

    #[test]
    fn stamps_on_line_starts() {
        let mut f = filter(true, false);
        let a = f.apply(b"one\ntw").into_owned();
        let b = f.apply(b"o\nthree\n").into_owned();
        let s = String::from_utf8_lossy(&[a, b].concat()).replace("\x1b[2m", "").replace("\x1b[0m", "");
        assert_eq!(s, "T one\nT two\nT three\n");
    }

    #[test]
    fn marks_keep_existing_color() {
        rules();
        let mut f = filter(false, true);
        let out = f.apply(b"\x1b[36mboot: Error here, ok\n").into_owned();
        assert_eq!(
            String::from_utf8_lossy(&out),
            "\x1b[36mboot: \x1b[1;31mError\x1b[0m\x1b[36m here, \x1b[32mok\x1b[0m\x1b[36m\n"
        );
        // Escape sequences are never painted inside.
        let out = f.apply(b"\x1b]0;error ok\x07x\n").into_owned();
        assert_eq!(out, b"\x1b]0;error ok\x07x\n");
    }

    #[test]
    fn off_in_alternate_screen() {
        rules();
        let mut f = filter(true, true);
        let out = f.apply(b"\x1b[?1049herror\nerror\n\x1b[?1049").into_owned();
        assert_eq!(out, b"\x1b[?1049herror\nerror\n\x1b[?1049");
        // The switch back was split across chunks.
        let out = f.apply(b"l$ ok\n").into_owned();
        let s = String::from_utf8_lossy(&out);
        assert!(s.contains("\x1b[32mok"), "{s:?}");
        assert!(!s.starts_with("\x1b[2m"), "no stamp mid-line: {s:?}");
    }

    #[test]
    fn untouched_when_off() {
        let mut f = filter(false, false);
        assert!(matches!(f.apply(b"error\n"), Cow::Borrowed(_)));
    }

    #[test]
    fn strip_across_chunks() {
        let mut s = Strip::default();
        let mut out = Vec::new();
        s.feed(b"a\x1b[3", &mut out);
        s.feed(b"1mb\r\n\x1b]0;t\x07c\x08\n", &mut out);
        assert_eq!(out, b"ab\nc\n");
    }

    #[test]
    fn plain_log_file() {
        let d = std::env::temp_dir().join(format!("tv-log-{}", std::process::id()));
        let mut l = Log::open(&d, "root@board:22", true).expect("open");
        assert!(l.path.file_name().unwrap().to_string_lossy().starts_with("root@board_22-"));
        l.write(b"\x1b[31mhi\x1b[0m\r\nthere", true, clock).expect("w");
        l.write(b"\n", true, clock).expect("w");
        let path = l.path.clone();
        drop(l);
        assert_eq!(fs::read_to_string(&path).expect("read"), "[T] hi\n[T] there\n");
        let _ = fs::remove_dir_all(&d);
    }

    #[test]
    fn rules_roundtrip_and_defaults() {
        let text = toml::to_string_pretty(&RulesFile { version: 1, rules: default_rules() }).expect("dump");
        assert_eq!(parse_rules(&text), Ok(default_rules()));
        for r in default_rules() {
            assert!(check(&r).is_ok());
        }
        assert!(check(&Rule::new("(", "red", false)).is_err());
    }
}
