// Logcat view of an Android device: lines are parsed, colored by level and
// filtered by level, tag, process and text. The last lines are kept, so a
// new filter applies to them too. The log file gets every line unfiltered.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Stdio};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};
use regex::Regex;

use crate::adb::{self, Adb};
use crate::output::Out;
use crate::state::State;
use crate::term::{self, Launch};

/// Lines kept for re-filtering.
const KEEP: usize = 20_000;
const LEVELS: [char; 6] = ['V', 'D', 'I', 'W', 'E', 'F'];
const LEVEL_NAMES: [&str; 6] = ["Verbose", "Debug", "Info", "Warn", "Error", "Fatal"];
/// Level of lines that are not log entries ("--------- beginning of main").
const OTHER: usize = usize::MAX;

#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    raw: String,
    /// "MM-DD HH:MM:SS.mmm"
    ts: String,
    pid: u32,
    tid: u32,
    level: usize,
    tag: String,
    msg: String,
}

fn pattern() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"^(\d\d-\d\d \d\d:\d\d:\d\d\.\d+)\s+(\d+)\s+(\d+)\s+([VDIWEFS])\s(.*?)\s*: (.*)$").expect("logcat pattern")
    })
}

/// Parses a `logcat -v threadtime` line.
pub fn parse(raw: &str) -> Line {
    let raw = raw.trim_end_matches(['\r', '\n']);
    let other = || Line { raw: raw.to_string(), ts: String::new(), pid: 0, tid: 0, level: OTHER, tag: String::new(), msg: String::new() };
    let Some(c) = pattern().captures(raw) else { return other() };
    let level = c[4].chars().next().and_then(|l| LEVELS.iter().position(|x| *x == l)).unwrap_or(LEVELS.len() - 1);
    Line {
        raw: raw.to_string(),
        ts: c[1].to_string(),
        pid: c[2].parse().unwrap_or(0),
        tid: c[3].parse().unwrap_or(0),
        level,
        tag: c[5].trim().to_string(),
        msg: c[6].to_string(),
    }
}

#[derive(Clone, Debug, Default)]
pub struct Filter {
    /// Lowest level shown (index into LEVELS).
    pub min: usize,
    /// Parts of tags, lowercase; empty shows all.
    pub tags: Vec<String>,
    /// Process ids; None shows all.
    pub pids: Option<Vec<u32>>,
    /// Text anywhere in the line, lowercase.
    pub text: String,
}

impl Filter {
    pub fn pass(&self, l: &Line) -> bool {
        let text = self.text.is_empty() || l.raw.to_lowercase().contains(&self.text);
        if l.level == OTHER {
            return text && self.tags.is_empty() && self.pids.is_none() && self.min == 0;
        }
        let tag = l.tag.to_lowercase();
        l.level >= self.min
            && (self.tags.is_empty() || self.tags.iter().any(|t| tag.contains(t.as_str())))
            && self.pids.as_ref().is_none_or(|p| p.contains(&l.pid))
            && text
    }
}

fn color(level: usize) -> &'static str {
    match level {
        0 => "90",
        1 => "36",
        2 => "32",
        3 => "33",
        4 => "31",
        _ => "1;31",
    }
}

/// Screen form of a line: dim time, ids, colored level and tag.
pub fn render(l: &Line) -> String {
    if l.level == OTHER {
        return format!("\x1b[2m{}\x1b[0m\r\n", l.raw);
    }
    let c = color(l.level);
    let msg = if l.level >= 3 { format!("\x1b[{c}m{}\x1b[0m", l.msg) } else { l.msg.clone() };
    format!(
        "\x1b[2m{}\x1b[0m {:>5} {:>5} \x1b[{c}m{} {}\x1b[0m: {msg}\r\n",
        l.ts, l.pid, l.tid, LEVELS[l.level], l.tag
    )
}

enum Ev {
    Line(String),
    /// adb ended; the message says why.
    Ended(String),
}

struct View {
    term: glib::WeakRef<crate::vt::View>,
    out: Rc<RefCell<Out>>,
    lines: RefCell<VecDeque<Line>>,
    filter: RefCell<Filter>,
    /// Package or process names in the process filter, resolved to pids.
    names: RefCell<Vec<String>>,
    numbers: RefCell<Vec<u32>>,
    paused: Cell<bool>,
    /// Time of the last line, to resume after the device returns.
    last: Arc<Mutex<Option<String>>>,
    /// Lines seen at the last time, to drop repeats after a restart.
    recent: RefCell<VecDeque<String>>,
    /// logcat was restarted; its first lines may repeat kept ones.
    resumed: Cell<bool>,
    dot: gtk::Label,
    count: gtk::Label,
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    redraw: RefCell<Option<glib::SourceId>>,
    bin: String,
    serial: String,
}

impl View {
    fn set_state(&self, text: &str, tint: &str) {
        self.dot.set_text(text);
        for c in ["tint-green", "tint-red", "tint-yellow"] {
            self.dot.remove_css_class(c);
        }
        self.dot.add_css_class(tint);
    }

    fn count(&self) {
        let shown = {
            let f = self.filter.borrow();
            self.lines.borrow().iter().filter(|l| f.pass(l)).count()
        };
        let all = self.lines.borrow().len();
        let paused = if self.paused.get() { " · paused" } else { "" };
        self.count.set_text(&format!("{shown} of {all} lines{paused}"));
    }

    /// Takes new lines; shows those that pass unless paused.
    fn take(&self, batch: Vec<String>) {
        let mut shown = String::new();
        {
            let f = self.filter.borrow();
            let mut lines = self.lines.borrow_mut();
            let mut recent = self.recent.borrow_mut();
            let mut last = self.last.lock().ok();
            let mut raw = String::new();
            for text in batch {
                let l = parse(&text);
                if !l.ts.is_empty() {
                    let prev = last.as_ref().and_then(|g| (**g).clone());
                    if let (true, Some(prev)) = (self.resumed.get(), &prev) {
                        // After a restart logcat repeats lines from the resume time.
                        if l.ts < *prev || (l.ts == *prev && recent.contains(&l.raw)) {
                            continue;
                        }
                        self.resumed.set(false);
                    }
                    if prev.as_ref() != Some(&l.ts) {
                        recent.clear();
                    }
                    if let Some(g) = last.as_mut() {
                        **g = Some(l.ts.clone());
                    }
                    recent.push_back(l.raw.clone());
                    if recent.len() > 64 {
                        recent.pop_front();
                    }
                }
                raw.push_str(&l.raw);
                raw.push('\n');
                if !self.paused.get() && f.pass(&l) {
                    shown.push_str(&render(&l));
                }
                lines.push_back(l);
                if lines.len() > KEEP {
                    lines.pop_front();
                }
            }
            self.out.borrow_mut().record(raw.as_bytes());
        }
        if !shown.is_empty() {
            if let Some(t) = self.term.upgrade() {
                let data = self.out.borrow_mut().filter.apply(shown.as_bytes()).into_owned();
                t.feed(&data);
            }
        }
        self.count();
    }

    /// Clears the screen and shows the kept lines that pass the filter.
    fn redraw(&self) {
        let Some(t) = self.term.upgrade() else { return };
        t.reset(true, true);
        let mut text = String::new();
        {
            let f = self.filter.borrow();
            for l in self.lines.borrow().iter().filter(|l| f.pass(l)) {
                text.push_str(&render(l));
            }
        }
        let data = self.out.borrow_mut().filter.apply(text.as_bytes()).into_owned();
        t.feed(&data);
        self.count();
    }

    /// Redraws a moment after the last filter change.
    fn later(self: &Rc<Self>) {
        if let Some(id) = self.redraw.take() {
            id.remove();
        }
        let w = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(Duration::from_millis(200), move || {
            if let Some(v) = w.upgrade() {
                v.redraw.take();
                v.redraw();
            }
        });
        self.redraw.replace(Some(id));
    }

    /// Resolves package names to pids in the background.
    fn resolve(self: &Rc<Self>) {
        let names = self.names.borrow().clone();
        if names.is_empty() {
            let nums = self.numbers.borrow().clone();
            self.filter.borrow_mut().pids = (!nums.is_empty()).then_some(nums);
            self.later();
            return;
        }
        let (bin, serial) = (self.bin.clone(), self.serial.clone());
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(found) = gio::spawn_blocking(move || {
                let a = Adb::new(&bin, &serial);
                names.iter().flat_map(|n| a.pids(n)).collect::<Vec<u32>>()
            })
            .await
            else {
                return;
            };
            let Some(v) = w.upgrade() else { return };
            let mut pids = v.numbers.borrow().clone();
            pids.extend(found);
            let changed = v.filter.borrow().pids.as_ref() != Some(&pids);
            if changed {
                v.filter.borrow_mut().pids = Some(pids);
                v.later();
            }
        });
    }
}

/// Runs logcat until `stop`, restarting when it ends (device unplugged).
fn reader(bin: String, serial: String, last: Arc<Mutex<Option<String>>>, child: Arc<Mutex<Option<Child>>>, stop: Arc<AtomicBool>, tx: async_channel::Sender<Ev>) {
    thread::spawn(move || {
        while !stop.load(Ordering::Acquire) {
            let mut args = adb::select(&serial);
            args.extend(["wait-for-device".into(), "logcat".into(), "-v".into(), "threadtime".into()]);
            if let Some(ts) = last.lock().ok().and_then(|g| g.clone()) {
                args.extend(["-T".into(), ts]);
            }
            let spawned = crate::sys::spawn_tied(crate::sys::command(&bin).args(&args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()));
            let mut ch = match spawned {
                Ok(ch) => ch,
                Err(e) => {
                    let _ = tx.send_blocking(Ev::Ended(format!("{bin}: {e}")));
                    return;
                }
            };
            let out = ch.stdout.take();
            let mut err = ch.stderr.take();
            if let Ok(mut g) = child.lock() {
                *g = Some(ch);
            }
            if let Some(out) = out {
                let mut r = BufReader::new(out);
                let mut buf = Vec::new();
                loop {
                    buf.clear();
                    match r.read_until(b'\n', &mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            if tx.send_blocking(Ev::Line(String::from_utf8_lossy(&buf).into_owned())).is_err() {
                                return;
                            }
                        }
                    }
                }
            }
            let mut msg = String::new();
            if let Some(e) = err.as_mut() {
                let _ = e.read_to_string(&mut msg);
            }
            if let Some(mut ch) = child.lock().ok().and_then(|mut g| g.take()) {
                let _ = ch.kill();
                let _ = ch.wait();
            }
            if stop.load(Ordering::Acquire) {
                return;
            }
            let msg = msg.trim();
            let why = if msg.is_empty() { "logcat ended".to_string() } else { msg.lines().next().unwrap_or(msg).to_string() };
            if tx.send_blocking(Ev::Ended(why)).is_err() {
                return;
            }
            thread::sleep(Duration::from_secs(1));
        }
    });
}

fn entry(hint: &str, tip: &str, width: i32) -> gtk::Entry {
    let e = gtk::Entry::new();
    e.set_placeholder_text(Some(hint));
    e.set_tooltip_text(Some(tip));
    e.set_width_chars(width);
    e
}

/// Comma or space separated words, lowercase.
fn words(s: &str) -> Vec<String> {
    s.split([',', ' ']).map(|w| w.trim().to_lowercase()).filter(|w| !w.is_empty()).collect()
}

/// Builds the pane. Returns the terminal and the leaf widget.
pub fn build(state: &Rc<State>, launch: &Launch) -> (crate::vt::View, gtk::Widget) {
    let t = term::spawn(state, launch);
    let out = crate::output::of(&t).unwrap_or_else(|| crate::output::attach(&t, &launch.title, &state.cfg.borrow()));
    let serial = launch.adb.as_ref().map(|a| a.serial.clone()).unwrap_or_default();
    let bin = adb::bin();

    let name = gtk::Label::new(Some(&launch.title));
    name.add_css_class("heading");
    let dot = gtk::Label::new(None);
    let count = gtk::Label::new(None);
    count.add_css_class("dim-label");
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let level = gtk::DropDown::from_strings(&LEVEL_NAMES);
    level.set_tooltip_text(Some("Lowest level shown"));
    let tags = entry("Tags", "Parts of tags, comma separated, e.g. ActivityManager, MyApp", 14);
    let procs = entry("PID or package", "Process ids or package names, comma separated", 16);
    let find = entry("Find", "Text anywhere in the line", 14);
    let pause = gtk::ToggleButton::with_label("Pause");
    pause.set_tooltip_text(Some("Stop scrolling; lines are still kept"));
    let wipe = gtk::Button::from_icon_name("user-trash-symbolic");
    wipe.add_css_class("flat");
    wipe.set_tooltip_text(Some("Clear the device log (logcat -c)"));
    let clear = gtk::Button::from_icon_name("edit-clear-symbolic");
    clear.add_css_class("flat");
    clear.set_tooltip_text(Some("Clear the screen and kept lines"));

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    bar.add_css_class("serialbar");
    for w in [
        name.upcast_ref::<gtk::Widget>(),
        dot.upcast_ref(),
        count.upcast_ref(),
        spacer.upcast_ref(),
        level.upcast_ref(),
        tags.upcast_ref(),
        procs.upcast_ref(),
        find.upcast_ref(),
        pause.upcast_ref(),
        wipe.upcast_ref(),
        clear.upcast_ref(),
    ] {
        bar.append(w);
    }

    let v = Rc::new(View {
        term: t.downgrade(),
        out,
        lines: RefCell::new(VecDeque::new()),
        filter: RefCell::new(Filter::default()),
        names: RefCell::new(Vec::new()),
        numbers: RefCell::new(Vec::new()),
        paused: Cell::new(false),
        last: Arc::default(),
        recent: RefCell::new(VecDeque::new()),
        resumed: Cell::new(false),
        dot,
        count,
        stop: Arc::new(AtomicBool::new(false)),
        child: Arc::default(),
        redraw: RefCell::new(None),
        bin: bin.clone(),
        serial: serial.clone(),
    });
    v.count();

    let w = Rc::downgrade(&v);
    level.connect_selected_notify(move |d| {
        if let Some(v) = w.upgrade() {
            v.filter.borrow_mut().min = d.selected() as usize;
            v.later();
        }
    });
    let w = Rc::downgrade(&v);
    tags.connect_changed(move |e| {
        if let Some(v) = w.upgrade() {
            v.filter.borrow_mut().tags = words(&e.text());
            v.later();
        }
    });
    let w = Rc::downgrade(&v);
    find.connect_changed(move |e| {
        if let Some(v) = w.upgrade() {
            v.filter.borrow_mut().text = e.text().trim().to_lowercase();
            v.later();
        }
    });
    let w = Rc::downgrade(&v);
    procs.connect_changed(move |e| {
        let Some(v) = w.upgrade() else { return };
        let (nums, names): (Vec<String>, Vec<String>) = words(&e.text()).into_iter().partition(|w| w.parse::<u32>().is_ok());
        v.numbers.replace(nums.iter().filter_map(|n| n.parse().ok()).collect());
        v.names.replace(names);
        v.resolve();
    });
    // Apps restart with new pids; look them up again.
    let w = Rc::downgrade(&v);
    glib::timeout_add_seconds_local(3, move || {
        let Some(v) = w.upgrade() else { return glib::ControlFlow::Break };
        if !v.names.borrow().is_empty() {
            v.resolve();
        }
        glib::ControlFlow::Continue
    });
    let w = Rc::downgrade(&v);
    pause.connect_toggled(move |b| {
        if let Some(v) = w.upgrade() {
            v.paused.set(b.is_active());
            if b.is_active() {
                v.count();
            } else {
                v.redraw();
            }
        }
    });
    let w = Rc::downgrade(&v);
    clear.connect_clicked(move |_| {
        if let Some(v) = w.upgrade() {
            v.lines.borrow_mut().clear();
            v.redraw();
        }
    });
    let w = Rc::downgrade(&v);
    wipe.connect_clicked(move |b| {
        let Some(v) = w.upgrade() else { return };
        let d = adw::AlertDialog::new(Some("Clear the Device Log?"), Some("Runs logcat -c on the device. The lines kept here stay."));
        d.add_responses(&[("cancel", "Cancel"), ("clear", "Clear")]);
        d.set_response_appearance("clear", adw::ResponseAppearance::Destructive);
        d.set_default_response(Some("cancel"));
        d.set_close_response("cancel");
        let (bin, serial) = (v.bin.clone(), v.serial.clone());
        let tw = v.term.clone();
        d.connect_response(None, move |_, r| {
            if r != "clear" {
                return;
            }
            let (bin, serial, tw) = (bin.clone(), serial.clone(), tw.clone());
            glib::spawn_future_local(async move {
                let res = gio::spawn_blocking(move || Adb::new(&bin, &serial).clear_log()).await;
                let msg = match res {
                    Ok(Ok(())) => "\x1b[2m[device log cleared]\x1b[0m\r\n".to_string(),
                    Ok(Err(e)) => format!("\x1b[31m[logcat -c: {e}]\x1b[0m\r\n"),
                    Err(_) => "\x1b[31m[logcat -c failed]\x1b[0m\r\n".to_string(),
                };
                if let Some(t) = tw.upgrade() {
                    t.feed(msg.as_bytes());
                }
            });
        });
        d.present(Some(b));
    });

    if adb::found(&bin) {
        let (tx, rx) = async_channel::bounded(4096);
        reader(bin.clone(), serial.clone(), v.last.clone(), v.child.clone(), v.stop.clone(), tx);
        v.set_state("◌ waiting for device", "tint-yellow");
        let w = Rc::downgrade(&v);
        glib::spawn_future_local(async move {
            while let Ok(ev) = rx.recv().await {
                let Some(v) = w.upgrade() else { break };
                let mut batch = Vec::new();
                let mut ended = None;
                match ev {
                    Ev::Line(l) => batch.push(l),
                    Ev::Ended(why) => ended = Some(why),
                }
                // Take what else is waiting, to draw once.
                while ended.is_none() && batch.len() < 4096 {
                    match rx.try_recv() {
                        Ok(Ev::Line(l)) => batch.push(l),
                        Ok(Ev::Ended(why)) => ended = Some(why),
                        Err(_) => break,
                    }
                }
                if !batch.is_empty() {
                    v.set_state("● streaming", "tint-green");
                    v.take(batch);
                }
                if let Some(why) = ended {
                    v.resumed.set(true);
                    v.set_state("◌ waiting for device", "tint-yellow");
                    if let Some(t) = v.term.upgrade() {
                        t.feed(format!("\x1b[33m[{why}; waiting for the device]\x1b[0m\r\n").as_bytes());
                    }
                }
            }
        });
    } else {
        v.set_state("○ adb not found", "tint-red");
        t.feed(format!("\x1b[31m{bin} not found. Install adb (Debian/Ubuntu: adb, Arch: android-tools) or set \"adb\" in settings.json.\x1b[0m\r\n").as_bytes());
    }

    // Closing the pane stops logcat.
    let vv = v.clone();
    t.connect_destroy(move |_| {
        vv.stop.store(true, Ordering::Release);
        if let Some(mut ch) = vv.child.lock().ok().and_then(|mut g| g.take()) {
            let _ = ch.kill();
            let _ = ch.wait();
        }
    });

    let leaf = gtk::Box::new(gtk::Orientation::Vertical, 0);
    leaf.append(&bar);
    leaf.append(&term::scroll(&t));
    (t, leaf.upcast())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lines() {
        let l = parse("11-02 14:30:01.300  4242  4250 W ExampleApp: slow frame: 48 ms\n");
        assert_eq!((l.ts.as_str(), l.pid, l.tid, l.level), ("11-02 14:30:01.300", 4242, 4250, 3));
        assert_eq!((l.tag.as_str(), l.msg.as_str()), ("ExampleApp", "slow frame: 48 ms"));
        let l = parse("11-02 14:30:01.300   700   710 I chatty  : uid=1000 expire 3 lines");
        assert_eq!(l.tag, "chatty");
        assert_eq!(parse("--------- beginning of main").level, OTHER);
    }

    #[test]
    fn filters() {
        let a = parse("11-02 14:30:01.200  4242  4242 D ExampleApp: onCreate");
        let b = parse("11-02 14:30:01.400  1000  1000 E ActivityManager: ANR in x");
        let other = parse("--------- beginning of crash");
        let mut f = Filter::default();
        assert!(f.pass(&a) && f.pass(&b) && f.pass(&other));
        f.min = 3;
        assert!(!f.pass(&a) && f.pass(&b) && !f.pass(&other));
        let f = Filter { tags: words("example"), ..Default::default() };
        assert!(f.pass(&a) && !f.pass(&b));
        let f = Filter { pids: Some(vec![1000]), ..Default::default() };
        assert!(!f.pass(&a) && f.pass(&b));
        let f = Filter { text: "anr".into(), ..Default::default() };
        assert!(!f.pass(&a) && f.pass(&b));
    }

    #[test]
    fn rendered_colors() {
        let s = render(&parse("11-02 14:30:01.400  4242  4242 E ExampleApp: failed"));
        assert_eq!(s, "\x1b[2m11-02 14:30:01.400\x1b[0m  4242  4242 \x1b[31mE ExampleApp\x1b[0m: \x1b[31mfailed\x1b[0m\r\n");
        let s = render(&parse("11-02 14:30:01.200  4242  4242 I ExampleApp: ok"));
        assert!(s.ends_with("\x1b[32mI ExampleApp\x1b[0m: ok\r\n"));
    }
}
