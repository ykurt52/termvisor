// Live plot of numbers a pane prints: each line is one sample, its
// fields (split by a separator other than space) are the channels.
// Shown above the pane's terminal; Ctrl+Shift+G toggles it. Up to four
// charts share the data; each channel is drawn in one of them.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, VecDeque};
use std::io::Write;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gio, glib};
use serde::{Deserialize, Serialize};

use crate::output::{self, Watch};

/// Separators tried in this order when none is chosen.
pub const AUTO: [char; 5] = [',', '|', ';', '\t', ':'];
/// Samples kept.
const KEEP: usize = 200_000;
/// Channels at most.
const MAX_CH: usize = 32;
/// Charts at most.
const MAX_CHARTS: usize = 4;

const COLORS: [(f64, f64, f64); 10] = [
    (0.30, 0.69, 0.95),
    (0.95, 0.55, 0.20),
    (0.40, 0.80, 0.40),
    (0.90, 0.30, 0.35),
    (0.70, 0.50, 0.95),
    (0.95, 0.85, 0.30),
    (0.30, 0.85, 0.80),
    (0.95, 0.45, 0.75),
    (0.65, 0.65, 0.65),
    (0.60, 0.80, 0.25),
];
/// Cursor lines A and B.
const CURSOR: (f64, f64, f64) = (0.95, 0.80, 0.25);
const THRESHOLD: (f64, f64, f64) = (0.95, 0.30, 0.30);

#[derive(Debug, PartialEq)]
pub enum Line {
    /// One sample; fields that are not numbers are NaN.
    Values(Vec<f64>),
    /// A line of names only: channel names.
    Names(Vec<String>),
    Skip,
}

fn number(s: &str) -> Option<f64> {
    let t = s.trim();
    if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        return i64::from_str_radix(h, 16).ok().map(|v| v as f64);
    }
    t.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Parses one line. `sep` None picks the first of AUTO in the line.
pub fn parse(line: &str, sep: Option<char>) -> Line {
    let l = line.trim();
    if l.is_empty() {
        return Line::Skip;
    }
    let sep = sep.filter(|c| l.contains(*c)).or_else(|| AUTO.iter().copied().find(|c| l.contains(*c)));
    let Some(sep) = sep else {
        // A single value per line.
        return number(l).map(|v| Line::Values(vec![v])).unwrap_or(Line::Skip);
    };
    let mut fields: Vec<&str> = l.split(sep).map(str::trim).collect();
    // "1,2,3," ends with an empty field.
    if fields.len() > 1 && fields.last().is_some_and(|f| f.is_empty()) {
        fields.pop();
    }
    let nums: Vec<Option<f64>> = fields.iter().map(|f| number(f)).collect();
    let count = nums.iter().filter(|n| n.is_some()).count();
    if count == 0 {
        let names: Vec<String> = fields.iter().map(|f| f.to_string()).collect();
        let plain = names.iter().all(|n| !n.is_empty() && n.len() <= 32);
        return if fields.len() > 1 && plain { Line::Names(names) } else { Line::Skip };
    }
    Line::Values(nums.into_iter().map(|n| n.unwrap_or(f64::NAN)).take(MAX_CH).collect())
}

/// Turns stream text into lines, keeping a partial last line.
#[derive(Default)]
pub struct Lines {
    rest: String,
}

impl Lines {
    pub fn feed(&mut self, text: &str) -> Vec<String> {
        self.rest.push_str(text);
        let mut out = Vec::new();
        while let Some(i) = self.rest.find('\n') {
            let line: String = self.rest.drain(..=i).collect();
            out.push(line.trim_end_matches(['\n', '\r']).to_string());
        }
        // A line without an end for this long is not data.
        if self.rest.len() > 4096 {
            self.rest.clear();
        }
        out
    }
}

/// "1.5", "2.5k", "-3" for axis labels.
fn tick_label(v: f64, step: f64) -> String {
    let a = step.abs();
    if a >= 1.0 || a == 0.0 {
        format!("{v:.0}")
    } else {
        let d = (-a.log10().floor()) as usize;
        format!("{v:.d$}")
    }
}

/// Round step for about `n` ticks over `span`.
fn nice_step(span: f64, n: f64) -> f64 {
    if span <= 0.0 || !span.is_finite() {
        return 1.0;
    }
    let raw = span / n;
    let mag = 10f64.powf(raw.log10().floor());
    let f = raw / mag;
    let nice = if f < 1.5 {
        1.0
    } else if f < 3.5 {
        2.0
    } else if f < 7.5 {
        5.0
    } else {
        10.0
    };
    nice * mag
}

/// A number for a readout: few digits, no float noise.
fn short(v: f64) -> String {
    if v == 0.0 || !v.is_finite() {
        return format!("{v}");
    }
    let digits = (4 - v.abs().log10().floor() as i32).clamp(0, 9) as usize;
    let s = format!("{v:.digits$}");
    if s.contains('.') { s.trim_end_matches('0').trim_end_matches('.').to_string() } else { s }
}

#[derive(Clone, Copy, PartialEq)]
enum Span {
    Secs(f64),
    Samples(usize),
}

const SPANS: [(&str, Span); 8] = [
    ("1 s", Span::Secs(1.0)),
    ("5 s", Span::Secs(5.0)),
    ("10 s", Span::Secs(10.0)),
    ("30 s", Span::Secs(30.0)),
    ("60 s", Span::Secs(60.0)),
    ("200 samples", Span::Samples(200)),
    ("1000 samples", Span::Samples(1000)),
    ("5000 samples", Span::Samples(5000)),
];
const SEPS: [(&str, Option<char>); 6] =
    [("Auto", None), (", comma", Some(',')), ("| bar", Some('|')), ("; semicolon", Some(';')), ("Tab", Some('\t')), (": colon", Some(':'))];

/// How the charts sit next to each other.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub enum Layout {
    #[default]
    One,
    /// Two side by side.
    Side,
    /// Two, one above the other.
    Stack,
    /// Two side by side above a third.
    TwoOverOne,
    /// Two stacked on the left, a third on the right.
    TwoBesideOne,
    /// Four, 2×2.
    Grid,
}

const LAYOUTS: [(&str, Layout); 6] = [
    ("1 chart", Layout::One),
    ("2 side by side", Layout::Side),
    ("2 stacked", Layout::Stack),
    ("3: two above one", Layout::TwoOverOne),
    ("3: two beside one", Layout::TwoBesideOne),
    ("4: 2×2", Layout::Grid),
];

impl Layout {
    pub fn count(self) -> usize {
        match self {
            Layout::One => 1,
            Layout::Side | Layout::Stack => 2,
            Layout::TwoOverOne | Layout::TwoBesideOne => 3,
            Layout::Grid => 4,
        }
    }

    /// The first layout with `n` charts.
    pub fn for_count(n: usize) -> Layout {
        match n {
            0 | 1 => Layout::One,
            2 => Layout::Side,
            3 => Layout::TwoOverOne,
            _ => Layout::Grid,
        }
    }
}

/// Range `(a, b)` scaled by `k` around the point at `frac` (0..1) of it;
/// that point stays where it is on screen.
pub fn zoomed((a, b): (f64, f64), frac: f64, k: f64) -> (f64, f64) {
    let at = a + frac.clamp(0.0, 1.0) * (b - a);
    (at - (at - a) * k, at + (b - at) * k)
}

/// A shown range on one axis.
type Range = (f64, f64);

/// Plot area margins (axis labels).
const LEFT: f64 = 58.0;
const RIGHT: f64 = 10.0;
const TOP: f64 = 8.0;
const BOTTOM: f64 = 20.0;

struct Sample {
    /// Seconds since the plot started.
    t: f64,
    /// Sample number.
    n: u64,
    v: Vec<f64>,
}

/// Min, max and mean of the finite values in `vals`.
fn stats(vals: impl Iterator<Item = f64>) -> Option<(f64, f64, f64)> {
    let (mut lo, mut hi, mut sum, mut n) = (f64::INFINITY, f64::NEG_INFINITY, 0.0, 0u64);
    for v in vals.filter(|v| v.is_finite()) {
        lo = lo.min(v);
        hi = hi.max(v);
        sum += v;
        n += 1;
    }
    (n > 0).then(|| (lo, hi, sum / n as f64))
}

/// What the legend shows for one channel.
#[derive(Clone, Copy, Default)]
struct Reading {
    now: Option<f64>,
    stats: Option<(f64, f64, f64)>,
}

/// Plot setup remembered per session, by its title.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    layout: Layout,
    /// Chart of each channel.
    assign: Vec<usize>,
    link: bool,
    /// Threshold of each chart.
    thresholds: Vec<Option<f64>>,
}

const SAVED: &str = "plots.json";

fn load_saved(key: &str) -> Saved {
    let text = std::fs::read_to_string(crate::recent::path(SAVED)).unwrap_or_default();
    let all: BTreeMap<String, Saved> = serde_json::from_str(&text).unwrap_or_default();
    all.get(key).cloned().unwrap_or_default()
}

fn store_saved(key: &str, s: &Saved) {
    let path = crate::recent::path(SAVED);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut all: BTreeMap<String, Saved> = serde_json::from_str(&text).unwrap_or_default();
    all.insert(key.to_string(), s.clone());
    let result = serde_json::to_string_pretty(&all)
        .map_err(std::io::Error::other)
        .and_then(|t| {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            crate::store::write_private(&path, &t)
        });
    if let Err(e) = result {
        crate::warn(&format!("{}: {e}", path.display()));
    }
}

/// One chart: its own drawing area, zoom, Y range and threshold.
struct Chart {
    idx: usize,
    area: gtk::DrawingArea,
    /// Zoomed X range when the time axes are not linked.
    zoom: Cell<Option<Range>>,
    lock: Cell<Option<Range>>,
    /// Last drawn Y range, for Lock Y and the threshold.
    drawn: Cell<Range>,
    /// Last drawn X range, for cursors.
    shown_x: Cell<Range>,
    hover: Cell<Option<f64>>,
    /// Last pointer position over the chart.
    pointer: Cell<(f64, f64)>,
    threshold: Cell<Option<f64>>,
}

impl Chart {
    /// Plot area size, or None when too small to draw.
    fn inner(&self) -> Option<(f64, f64)> {
        let pw = f64::from(self.area.width()) - LEFT - RIGHT;
        let ph = f64::from(self.area.height()) - TOP - BOTTOM;
        (pw >= 20.0 && ph >= 20.0).then_some((pw, ph))
    }
}

pub struct Plot {
    pub widget: gtk::Box,
    /// Holds the charts in their layout.
    host: gtk::Box,
    legend: gtk::FlowBox,
    rate: gtk::Label,
    cursor_info: gtk::Label,
    charts: RefCell<Vec<Rc<Chart>>>,
    layout: Cell<Layout>,
    data: RefCell<VecDeque<Sample>>,
    names: RefCell<Vec<String>>,
    shown: RefCell<Vec<bool>>,
    /// Chart of each channel.
    assign: RefCell<Vec<usize>>,
    /// Value and statistics labels of the legend, per channel.
    values: RefCell<Vec<(gtk::Label, gtk::Label)>>,
    lines: RefCell<Lines>,
    sep: Cell<Option<char>>,
    span: Cell<Span>,
    paused: Cell<bool>,
    /// Plot time frozen by Pause.
    frozen: Cell<f64>,
    /// Zoom on all charts at once while the time axes are linked.
    link: Cell<bool>,
    zoom: Cell<Option<Range>>,
    /// Cursors A and B on the X axis.
    cursors: Cell<[Option<f64>; 2]>,
    pause_button: RefCell<Option<gtk::ToggleButton>>,
    lock_button: RefCell<Option<gtk::ToggleButton>>,
    /// Values for the legend, found while drawing and shown by the timer
    /// (changing labels while drawing would relayout mid-frame).
    readout: RefCell<Vec<Reading>>,
    /// Session title: the setup is remembered under it.
    key: String,
    start: Instant,
    last: Cell<f64>,
    count: Cell<u64>,
    dirty: Cell<bool>,
}

impl Watch for Plot {
    fn text(&self, text: &str) {
        let lines = self.lines.borrow_mut().feed(text);
        if lines.is_empty() {
            return;
        }
        // Lines read together arrived over the time since the last read:
        // spread them evenly, so a steady rate draws as one.
        let now = self.start.elapsed().as_secs_f64();
        let prev = self.last.replace(now);
        let span = if prev > 0.0 && now - prev < 0.5 { now - prev } else { 0.0 };
        let total = lines.len() as f64;
        for (i, l) in lines.iter().enumerate() {
            match parse(l, self.sep.get()) {
                Line::Values(v) => {
                    let t = now - span * (total - 1.0 - i as f64) / total;
                    self.push(t, v);
                }
                Line::Names(n) => self.rename(n),
                Line::Skip => {}
            }
        }
        self.dirty.set(true);
    }
}

impl Plot {
    fn push(&self, t: f64, v: Vec<f64>) {
        let n = self.count.get();
        self.count.set(n + 1);
        if v.len() > self.names.borrow().len() {
            self.grow(v.len());
        }
        let mut d = self.data.borrow_mut();
        d.push_back(Sample { t, n, v });
        if d.len() > KEEP {
            d.pop_front();
        }
    }

    fn rename(&self, names: Vec<String>) {
        let have = self.names.borrow().len();
        // A text line only names channels before data, or with the same count.
        if have != 0 && have != names.len() && !self.data.borrow().is_empty() {
            return;
        }
        if names.len() > have {
            self.grow(names.len().min(MAX_CH));
        }
        for (i, n) in names.into_iter().take(MAX_CH).enumerate() {
            if let Some(slot) = self.names.borrow_mut().get_mut(i) {
                *slot = n;
            }
        }
        self.fill_legend();
    }

    fn grow(&self, n: usize) {
        let n = n.min(MAX_CH);
        {
            let mut names = self.names.borrow_mut();
            while names.len() < n {
                let n = names.len() + 1;
                names.push(format!("ch{n}"));
            }
            let mut shown = self.shown.borrow_mut();
            while shown.len() < n {
                shown.push(true);
            }
            let mut assign = self.assign.borrow_mut();
            while assign.len() < n {
                assign.push(0);
            }
        }
        self.fill_legend();
    }

    fn fill_legend(&self) {
        self.legend.remove_all();
        let mut labels = Vec::new();
        let names = self.names.borrow().clone();
        let charts = self.layout.get().count();
        let numbers: Vec<String> = (1..=charts).map(|i| i.to_string()).collect();
        let numbers: Vec<&str> = numbers.iter().map(String::as_str).collect();
        for (i, name) in names.iter().enumerate() {
            let check = gtk::CheckButton::new();
            check.set_active(self.shown.borrow().get(i).copied().unwrap_or(true));
            check.set_focus_on_click(false);
            let (r, g, b) = COLORS[i % COLORS.len()];
            let dot = gtk::Label::new(Some("●"));
            let hex = |c: f64| (c * 255.0).round() as u8;
            dot.set_markup(&format!("<span foreground=\"#{:02x}{:02x}{:02x}\">●</span>", hex(r), hex(g), hex(b)));
            let label = gtk::Label::new(Some(name));
            let value = gtk::Label::new(Some("–"));
            value.add_css_class("numeric");
            value.set_width_chars(9);
            value.set_xalign(1.0);
            let stat = gtk::Label::new(None);
            stat.add_css_class("dim-label");
            stat.add_css_class("numeric");
            stat.add_css_class("caption");
            stat.set_tooltip_text(Some("Minimum, maximum and mean of what the chart shows"));
            let chart = gtk::DropDown::from_strings(&numbers);
            chart.set_tooltip_text(Some("Chart this channel is drawn in"));
            chart.set_selected(self.assign.borrow().get(i).copied().unwrap_or(0).min(charts - 1) as u32);
            chart.set_visible(charts > 1);
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            for w in [check.upcast_ref::<gtk::Widget>(), dot.upcast_ref(), label.upcast_ref(), value.upcast_ref(), stat.upcast_ref(), chart.upcast_ref()] {
                b.append(w);
            }
            let w = self.me();
            check.connect_toggled(move |c| {
                if let Some(p) = w.upgrade() {
                    if let Some(s) = p.shown.borrow_mut().get_mut(i) {
                        *s = c.is_active();
                    }
                    p.redraw();
                }
            });
            let w = self.me();
            chart.connect_selected_notify(move |d| {
                if let Some(p) = w.upgrade() {
                    if let Some(a) = p.assign.borrow_mut().get_mut(i) {
                        *a = d.selected() as usize;
                    }
                    p.remember();
                    p.redraw();
                }
            });
            self.legend.append(&b);
            labels.push((value, stat));
        }
        self.values.replace(labels);
    }

    fn me(&self) -> Weak<Plot> {
        SELF.with(|s| s.borrow().iter().find(|w| w.upgrade().is_some_and(|p| std::ptr::eq(&*p, self))).cloned().unwrap_or_default())
    }

    fn redraw(&self) {
        for c in self.charts.borrow().iter() {
            c.area.queue_draw();
        }
    }

    /// Stores the setup for this session.
    fn remember(&self) {
        let saved = Saved {
            layout: self.layout.get(),
            assign: self.assign.borrow().clone(),
            link: self.link.get(),
            thresholds: self.charts.borrow().iter().map(|c| c.threshold.get()).collect(),
        };
        store_saved(&self.key, &saved);
    }

    /// Right edge of the shown time.
    fn now(&self) -> f64 {
        if self.paused.get() { self.frozen.get() } else { self.start.elapsed().as_secs_f64() }
    }

    /// Position of a sample on the X axis: seconds or sample number.
    fn key_of(&self, s: &Sample) -> f64 {
        match self.span.get() {
            Span::Secs(_) => s.t,
            Span::Samples(_) => s.n as f64,
        }
    }

    fn xzoom(&self, c: &Chart) -> Option<Range> {
        if self.link.get() { self.zoom.get() } else { c.zoom.get() }
    }

    fn set_xzoom(&self, c: &Chart, r: Option<Range>) {
        if self.link.get() {
            self.zoom.set(r);
            self.redraw();
        } else {
            c.zoom.set(r);
        }
    }

    /// Shown X range of `c`: its zoom, else the span up to now (or the pause).
    fn window(&self, c: &Chart, d: &VecDeque<Sample>) -> Range {
        if let Some(z) = self.xzoom(c) {
            return z;
        }
        match self.span.get() {
            Span::Secs(w) => {
                let end = self.now();
                (end - w, end)
            }
            Span::Samples(n) => {
                let last = if self.paused.get() {
                    d.iter().rev().find(|s| s.t <= self.frozen.get()).map(|s| s.n).unwrap_or(0)
                } else {
                    d.back().map(|s| s.n).unwrap_or(0)
                } as f64;
                (last - (n.max(2) - 1) as f64, last)
            }
        }
    }

    /// Indices of the samples inside `window`.
    fn view(&self, d: &VecDeque<Sample>, (a, b): Range) -> (usize, usize) {
        let from = d.partition_point(|s| self.key_of(s) < a);
        let to = d.partition_point(|s| self.key_of(s) <= b);
        (from, to.max(from))
    }

    /// Stops following new data (Pause, zoom or pan).
    fn freeze(&self) {
        if self.paused.get() {
            return;
        }
        self.frozen.set(self.start.elapsed().as_secs_f64());
        self.paused.set(true);
        if let Some(b) = self.pause_button.borrow().as_ref() {
            b.set_active(true);
        }
    }

    /// Double-click: `c` back to live, unzoomed, Y automatic. Following new
    /// data again once no chart is zoomed.
    fn live(&self, c: &Chart) {
        self.set_xzoom(c, None);
        c.lock.set(None);
        let zoomed = self.zoom.get().is_some() || self.charts.borrow().iter().any(|c| c.zoom.get().is_some());
        if !zoomed {
            self.paused.set(false);
            if let Some(b) = self.pause_button.borrow().as_ref() {
                b.set_active(false);
            }
        }
        if self.charts.borrow().iter().all(|c| c.lock.get().is_none()) {
            if let Some(b) = self.lock_button.borrow().as_ref() {
                b.set_active(false);
            }
        }
        self.redraw();
    }

    fn mark_locked(&self) {
        if let Some(b) = self.lock_button.borrow().as_ref() {
            b.set_active(true);
        }
    }

    /// Mouse wheel on `c`: zoom both axes around the pointer, so the point
    /// under it stays put (Shift: time axis only, Ctrl: Y axis only).
    fn wheel(&self, c: &Chart, dy: f64, only_x: bool, only_y: bool) {
        let k = if dy < 0.0 { 0.8 } else { 1.25 };
        let Some((pw, ph)) = c.inner() else { return };
        let (x, y) = c.pointer.get();
        if !only_x {
            // A fixed Y range: auto scaling would move the point vertically.
            let (nlo, nhi) = zoomed(c.drawn.get(), 1.0 - (y - TOP) / ph, k);
            if nhi - nlo > 1e-9 {
                c.lock.set(Some((nlo, nhi)));
                self.mark_locked();
            }
        }
        if !only_y {
            self.freeze();
            let (na, nb) = zoomed(self.window(c, &self.data.borrow()), (x - LEFT) / pw, k);
            let least = match self.span.get() {
                Span::Secs(_) => 0.005,
                Span::Samples(_) => 4.0,
            };
            if nb - na >= least {
                self.set_xzoom(c, Some((na, nb)));
            }
        }
        c.area.queue_draw();
    }

    /// Drag on `c` by (`dx`, `dy`) pixels from the ranges at the start of
    /// the drag: `x` on the time axis, `y` on the value axis.
    fn pan(&self, c: &Chart, x: Range, y: Range, dx: f64, dy: f64) {
        let Some((pw, ph)) = c.inner() else { return };
        if dx != 0.0 {
            self.freeze();
            let shift = -dx / pw * (x.1 - x.0);
            self.set_xzoom(c, Some((x.0 + shift, x.1 + shift)));
        }
        if dy != 0.0 {
            // Dragging down moves the curves down: the range moves up.
            let shift = dy / ph * (y.1 - y.0);
            c.lock.set(Some((y.0 + shift, y.1 + shift)));
            self.mark_locked();
        }
        c.area.queue_draw();
    }

    /// X axis value under the pointer of `c`.
    fn x_at(&self, c: &Chart) -> Option<f64> {
        let (pw, _) = c.inner()?;
        let (a, b) = c.shown_x.get();
        Some(a + ((c.pointer.get().0 - LEFT) / pw).clamp(0.0, 1.0) * (b - a))
    }

    /// Value under the pointer of `c`, rounded to a tenth of a grid step.
    fn y_at(&self, c: &Chart) -> Option<f64> {
        let (_, ph) = c.inner()?;
        let (lo, hi) = c.drawn.get();
        let v = lo + (1.0 - (c.pointer.get().1 - TOP) / ph).clamp(0.0, 1.0) * (hi - lo);
        let q = nice_step(hi - lo, 5.0) / 10.0;
        Some((v / q).round() * q)
    }

    fn draw(&self, c: &Chart, a: &gtk::DrawingArea, cr: &gtk::cairo::Context, w: i32, h: i32) {
        let fg = a.color();
        let (fr, fgc, fb) = (f64::from(fg.red()), f64::from(fg.green()), f64::from(fg.blue()));
        let (left, right, top, bottom) = (LEFT, RIGHT, TOP, BOTTOM);
        let (pw, ph) = (f64::from(w) - left - right, f64::from(h) - top - bottom);
        if pw < 20.0 || ph < 20.0 {
            return;
        }
        let d = self.data.borrow();
        let charts = self.layout.get().count();
        // Channels of this chart that are on.
        let mine: Vec<usize> = {
            let shown = self.shown.borrow();
            let assign = self.assign.borrow();
            (0..shown.len()).filter(|&i| shown[i] && assign.get(i).copied().unwrap_or(0).min(charts - 1) == c.idx).collect()
        };
        let (xa, xb) = self.window(c, &d);
        c.shown_x.set((xa, xb));
        let (from, to) = self.view(&d, (xa, xb));
        let xf = |s: &Sample| (self.key_of(s) - xa) / (xb - xa).max(1e-12);
        let vis = &d.range(from..to).collect::<Vec<_>>();

        // Y range: locked, else the shown values.
        let (lo, hi) = c.lock.get().unwrap_or_else(|| {
            let found = stats(vis.iter().flat_map(|s| mine.iter().filter_map(|&i| s.v.get(i).copied())));
            match found {
                None => (0.0, 1.0),
                Some((lo, hi, _)) if hi - lo < 1e-9 => (lo - 1.0, hi + 1.0),
                Some((lo, hi, _)) => {
                    let pad = (hi - lo) * 0.05;
                    (lo - pad, hi + pad)
                }
            }
        });
        c.drawn.set((lo, hi));
        let y = |v: f64| top + ph - (v - lo) / (hi - lo) * ph;

        // Grid and labels.
        cr.set_line_width(1.0);
        cr.set_font_size(10.0);
        // About one label per 28 px, so small charts stay readable.
        let step = nice_step(hi - lo, (ph / 28.0).clamp(1.5, 5.0));
        let mut v = (lo / step).ceil() * step;
        while v <= hi {
            let yy = y(v).round() + 0.5;
            cr.set_source_rgba(fr, fgc, fb, 0.10);
            cr.move_to(left, yy);
            cr.line_to(left + pw, yy);
            let _ = cr.stroke();
            cr.set_source_rgba(fr, fgc, fb, 0.65);
            let label = tick_label(v, step);
            let ext = cr.text_extents(&label).map(|e| e.width()).unwrap_or(0.0);
            cr.move_to(left - 6.0 - ext, yy + 3.5);
            let _ = cr.show_text(&label);
            v += step;
        }
        cr.set_source_rgba(fr, fgc, fb, 0.65);
        let xlabel = match (self.xzoom(c), self.span.get()) {
            (Some((a, b)), Span::Secs(_)) => format!("{:.3} s shown · drag: move · double-click: live · right-click: cursors, threshold", b - a),
            (Some((a, b)), Span::Samples(_)) => format!("samples {a:.0}–{b:.0} · drag: move · double-click: live"),
            (None, _) if self.paused.get() => "paused · wheel: zoom · drag: move · double-click: live".to_string(),
            (None, Span::Secs(w)) => format!("last {w:.0} s · wheel: zoom · right-click: cursors, threshold"),
            (None, Span::Samples(n)) => format!("last {n} samples · wheel: zoom"),
        };
        let xlabel = if charts > 1 { format!("{} · {xlabel}", c.idx + 1) } else { xlabel };
        cr.move_to(left, f64::from(h) - 5.0);
        let _ = cr.show_text(&xlabel);

        // Threshold: a dashed line; the frame turns red while the newest
        // value of a channel here is at or above it.
        let over = c.threshold.get().is_some_and(|th| {
            mine.iter().any(|&i| vis.iter().rev().find_map(|s| s.v.get(i).copied().filter(|v| v.is_finite())).is_some_and(|v| v >= th))
        });
        if over {
            cr.set_source_rgba(THRESHOLD.0, THRESHOLD.1, THRESHOLD.2, 0.9);
            cr.set_line_width(2.0);
        } else {
            cr.set_source_rgba(fr, fgc, fb, 0.25);
        }
        cr.rectangle(left + 0.5, top + 0.5, pw - 1.0, ph - 1.0);
        let _ = cr.stroke();
        cr.set_line_width(1.0);

        // Lines: per pixel column keep first, min, max and last, so dense
        // data draws in time proportional to the width.
        cr.save().ok();
        cr.rectangle(left, top, pw, ph);
        cr.clip();
        if let Some(th) = c.threshold.get() {
            let yy = y(th).round() + 0.5;
            cr.set_source_rgba(THRESHOLD.0, THRESHOLD.1, THRESHOLD.2, 0.08);
            cr.rectangle(left, top, pw, (yy - top).max(0.0));
            let _ = cr.fill();
            cr.set_source_rgba(THRESHOLD.0, THRESHOLD.1, THRESHOLD.2, 0.9);
            cr.set_dash(&[6.0, 4.0], 0.0);
            cr.move_to(left, yy);
            cr.line_to(left + pw, yy);
            let _ = cr.stroke();
            cr.set_dash(&[], 0.0);
            cr.move_to(left + pw - 70.0, yy - 4.0);
            let _ = cr.show_text(&format!("≥ {}", short(th)));
        }
        cr.set_line_width(1.4);
        for &ch in &mine {
            let (r, g, b) = COLORS[ch % COLORS.len()];
            cr.set_source_rgba(r, g, b, 0.95);
            let mut pen = false;
            let mut col = i64::MIN;
            let (mut mn, mut mx) = (0.0f64, 0.0f64);
            for s in vis.iter() {
                let Some(&val) = s.v.get(ch) else {
                    pen = false;
                    continue;
                };
                if !val.is_finite() {
                    pen = false;
                    continue;
                }
                let x = left + xf(s) * pw;
                let cx = x as i64;
                let yy = y(val);
                if cx == col && pen {
                    if yy < mn || yy > mx {
                        mn = mn.min(yy);
                        mx = mx.max(yy);
                        cr.line_to(x, yy);
                    }
                    continue;
                }
                col = cx;
                mn = yy;
                mx = yy;
                if pen {
                    cr.line_to(x, yy);
                } else {
                    cr.move_to(x, yy);
                    pen = true;
                }
            }
            let _ = cr.stroke();
        }

        // Cursors A and B.
        cr.set_line_width(1.0);
        for (i, cur) in self.cursors.get().iter().enumerate() {
            let Some(cx) = cur.filter(|cx| *cx >= xa && *cx <= xb) else { continue };
            let px = (left + (cx - xa) / (xb - xa).max(1e-12) * pw).round() + 0.5;
            cr.set_source_rgba(CURSOR.0, CURSOR.1, CURSOR.2, 0.95);
            cr.set_dash(&[4.0, 3.0], 0.0);
            cr.move_to(px, top);
            cr.line_to(px, top + ph);
            let _ = cr.stroke();
            cr.set_dash(&[], 0.0);
            cr.move_to(px + 3.0, top + 11.0);
            let _ = cr.show_text(if i == 0 { "A" } else { "B" });
        }
        cr.restore().ok();

        // Hover: a line and the values there; else the latest values.
        let at = c.hover.get().filter(|hx| *hx >= left && *hx <= left + pw);
        let pick = match at {
            Some(hx) => {
                let fx = (hx - left) / pw;
                vis.iter().min_by(|a, b| (xf(a) - fx).abs().total_cmp(&(xf(b) - fx).abs())).copied()
            }
            None => vis.last().copied().or(d.back()),
        };
        if let Some(hx) = at {
            cr.set_source_rgba(fr, fgc, fb, 0.45);
            cr.move_to(hx.round() + 0.5, top);
            cr.line_to(hx.round() + 0.5, top + ph);
            let _ = cr.stroke();
        }
        let n = self.names.borrow().len();
        let mut read = self.readout.borrow_mut();
        read.resize(n, Reading::default());
        for &i in &mine {
            if let Some(r) = read.get_mut(i) {
                r.now = pick.and_then(|s| s.v.get(i)).copied().filter(|v| v.is_finite());
                r.stats = stats(vis.iter().filter_map(|s| s.v.get(i).copied()));
            }
        }
    }

    /// Shows the values found by the last draw in the legend.
    fn show_readout(&self) {
        let values = self.values.borrow();
        let read = self.readout.borrow();
        for (i, (now, stat)) in values.iter().enumerate() {
            let r = read.get(i).copied().unwrap_or_default();
            let text = r.now.map(short).unwrap_or_else(|| "–".into());
            if now.text() != text {
                now.set_text(&text);
            }
            let text = r.stats.map(|(lo, hi, avg)| format!("min {} max {} avg {}", short(lo), short(hi), short(avg))).unwrap_or_default();
            if stat.text() != text {
                stat.set_text(&text);
            }
        }
        let text = self.cursor_text();
        self.cursor_info.set_visible(!text.is_empty());
        if self.cursor_info.text() != text {
            self.cursor_info.set_text(&text);
        }
    }

    /// "A→B Δt …, Δ per channel" once both cursors are set.
    fn cursor_text(&self) -> String {
        let [Some(a), Some(b)] = self.cursors.get() else {
            return match self.cursors.get() {
                [Some(_), None] => "Cursor A set · right-click: Cursor B here".into(),
                [None, Some(_)] => "Cursor B set · right-click: Cursor A here".into(),
                _ => String::new(),
            };
        };
        let d = self.data.borrow();
        let near = |x: f64| d.iter().min_by(|s, t| (self.key_of(s) - x).abs().total_cmp(&(self.key_of(t) - x).abs()));
        let unit = match self.span.get() {
            Span::Secs(_) => " s",
            Span::Samples(_) => " samples",
        };
        let mut parts = vec![format!("A→B Δt {}{unit}", short(b - a))];
        if let (Some(sa), Some(sb)) = (near(a), near(b)) {
            let names = self.names.borrow();
            let shown = self.shown.borrow();
            for (i, name) in names.iter().enumerate() {
                if !shown.get(i).copied().unwrap_or(true) {
                    continue;
                }
                if let (Some(va), Some(vb)) = (sa.v.get(i).filter(|v| v.is_finite()), sb.v.get(i).filter(|v| v.is_finite())) {
                    parts.push(format!("{name} Δ {}", short(vb - va)));
                }
            }
        }
        parts.join(" · ")
    }

    fn export(&self, path: &std::path::Path) -> std::io::Result<usize> {
        let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
        let names = self.names.borrow();
        writeln!(f, "time_s,sample,{}", names.join(","))?;
        let d = self.data.borrow();
        for s in d.iter() {
            let vals: Vec<String> = (0..names.len()).map(|i| s.v.get(i).filter(|v| v.is_finite()).map(|v| v.to_string()).unwrap_or_default()).collect();
            writeln!(f, "{:.6},{},{}", s.t, s.n, vals.join(","))?;
        }
        f.flush()?;
        Ok(d.len())
    }

    /// Puts the charts in `layout`, adding or dropping charts as needed.
    /// Channels of dropped charts move to the first one.
    fn set_layout(self: &Rc<Self>, layout: Layout) {
        let n = layout.count();
        {
            let mut charts = self.charts.borrow_mut();
            while charts.len() < n {
                let idx = charts.len();
                charts.push(make_chart(self, idx));
            }
            charts.truncate(n);
        }
        for a in self.assign.borrow_mut().iter_mut() {
            if *a >= n {
                *a = 0;
            }
        }
        self.layout.set(layout);
        // Take the charts out of the old arrangement before building the new one.
        while let Some(old) = self.host.first_child() {
            detach(&old);
            self.host.remove(&old);
        }
        let areas: Vec<gtk::Widget> = self.charts.borrow().iter().map(|c| c.area.clone().upcast()).collect();
        self.host.append(&arrange(layout, &areas));
        self.fill_legend();
        self.redraw();
    }
}

/// Splits `a` and `b` along `o`, both resizable.
fn pair(o: gtk::Orientation, a: &gtk::Widget, b: &gtk::Widget) -> gtk::Widget {
    let p = gtk::Paned::new(o);
    p.set_wide_handle(true);
    p.set_shrink_start_child(false);
    p.set_shrink_end_child(false);
    p.set_start_child(Some(a));
    p.set_end_child(Some(b));
    p.upcast()
}

/// The charts `areas` (as many as `layout` has) in nested panes.
fn arrange(layout: Layout, areas: &[gtk::Widget]) -> gtk::Widget {
    use gtk::Orientation::{Horizontal as H, Vertical as V};
    let a = |i: usize| areas[i].clone();
    match layout {
        Layout::One => a(0),
        Layout::Side => pair(H, &a(0), &a(1)),
        Layout::Stack => pair(V, &a(0), &a(1)),
        Layout::TwoOverOne => pair(V, &pair(H, &a(0), &a(1)), &a(2)),
        Layout::TwoBesideOne => pair(H, &pair(V, &a(0), &a(1)), &a(2)),
        Layout::Grid => pair(V, &pair(H, &a(0), &a(1)), &pair(H, &a(2), &a(3))),
    }
}

/// Empties the panes under `w`, so its charts can be placed again.
fn detach(w: &gtk::Widget) {
    if let Some(p) = w.downcast_ref::<gtk::Paned>() {
        let (s, e) = (p.start_child(), p.end_child());
        p.set_start_child(None::<&gtk::Widget>);
        p.set_end_child(None::<&gtk::Widget>);
        for c in [s, e].into_iter().flatten() {
            detach(&c);
        }
    }
}

/// Renders `w` as it is on screen into a PNG at `path`, on a solid
/// background so the text stays readable.
fn save_png(w: &gtk::Widget, path: &std::path::Path) -> Result<(), String> {
    let (wd, ht) = (w.width(), w.height());
    if wd <= 0 || ht <= 0 {
        return Err("nothing to save".into());
    }
    let fg = w.color();
    let light_text = 0.299 * fg.red() + 0.587 * fg.green() + 0.114 * fg.blue() > 0.5;
    let bg = if light_text { gtk::gdk::RGBA::new(0.12, 0.12, 0.12, 1.0) } else { gtk::gdk::RGBA::WHITE };
    let snap = gtk::Snapshot::new();
    snap.append_color(&bg, &gtk::graphene::Rect::new(0.0, 0.0, wd as f32, ht as f32));
    gtk::WidgetPaintable::new(Some(w)).snapshot(&snap, f64::from(wd), f64::from(ht));
    let node = snap.to_node().ok_or("nothing to save")?;
    let renderer = w.native().and_then(|n| n.renderer()).ok_or("no renderer")?;
    renderer.render_texture(&node, None).save_to_png(path).map_err(|e| e.to_string())
}

/// Asks for a file name, then saves `w` as PNG.
fn ask_png(w: &gtk::Widget, name: &str) {
    let dlg = gtk::FileDialog::new();
    dlg.set_initial_name(Some(name));
    let w2 = w.clone();
    dlg.save(w.root().and_downcast::<gtk::Window>().as_ref(), None::<&gio::Cancellable>, move |res| {
        let Some(path) = res.ok().and_then(|f| f.path()) else { return };
        match save_png(&w2, &path) {
            Ok(()) => toast(&w2, &format!("Saved {}", path.display())),
            Err(e) => toast(&w2, &format!("Saving PNG failed: {e}")),
        }
    });
}

thread_local! {
    /// Open plots, for callbacks that need a handle to their plot.
    static SELF: RefCell<Vec<Weak<Plot>>> = const { RefCell::new(Vec::new()) };
}

fn toast(w: &impl IsA<gtk::Widget>, msg: &str) {
    match w.ancestor(adw::ToastOverlay::static_type()).and_downcast::<adw::ToastOverlay>() {
        Some(t) => t.add_toast(adw::Toast::new(msg)),
        None => crate::warn(msg),
    }
}

/// Handler of a chart menu entry.
type ChartFn = Box<dyn Fn(&Rc<Plot>, &Rc<Chart>)>;

/// Chart `idx` of `p` with its pointer, wheel, drag and right-click menu.
fn make_chart(p: &Rc<Plot>, idx: usize) -> Rc<Chart> {
    let area = gtk::DrawingArea::new();
    area.set_vexpand(true);
    area.set_hexpand(true);
    area.set_content_height(70);
    area.set_content_width(160);
    let c = Rc::new(Chart {
        idx,
        area: area.clone(),
        zoom: Cell::new(None),
        lock: Cell::new(None),
        drawn: Cell::new((0.0, 1.0)),
        shown_x: Cell::new((0.0, 1.0)),
        hover: Cell::new(None),
        pointer: Cell::new((0.0, 0.0)),
        threshold: Cell::new(load_saved(&p.key).thresholds.get(idx).copied().flatten()),
    });
    let (wp, wc) = (Rc::downgrade(p), Rc::downgrade(&c));
    area.set_draw_func(move |a, cr, wd, ht| {
        if let (Some(p), Some(c)) = (wp.upgrade(), wc.upgrade()) {
            p.draw(&c, a, cr, wd, ht);
        }
    });

    let motion = gtk::EventControllerMotion::new();
    let wc = Rc::downgrade(&c);
    motion.connect_motion(move |_, x, y| {
        if let Some(c) = wc.upgrade() {
            c.pointer.set((x, y));
            c.hover.set(Some(x));
            c.area.queue_draw();
        }
    });
    let wc = Rc::downgrade(&c);
    motion.connect_leave(move |_| {
        if let Some(c) = wc.upgrade() {
            c.hover.set(None);
            c.area.queue_draw();
        }
    });
    area.add_controller(motion);

    // Wheel zooms around the pointer; Shift: time only, Ctrl: Y only.
    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL | gtk::EventControllerScrollFlags::DISCRETE);
    let (wp, wc) = (Rc::downgrade(p), Rc::downgrade(&c));
    scroll.connect_scroll(move |s, _, dy| {
        let (Some(p), Some(c)) = (wp.upgrade(), wc.upgrade()) else { return glib::Propagation::Proceed };
        let m = s.current_event_state();
        p.wheel(&c, dy, m.contains(gtk::gdk::ModifierType::SHIFT_MASK), m.contains(gtk::gdk::ModifierType::CONTROL_MASK));
        glib::Propagation::Stop
    });
    area.add_controller(scroll);

    // Drag pans; double-click goes back to live.
    let drag = gtk::GestureDrag::new();
    // X and Y ranges when the drag started.
    let start: Rc<Cell<(Range, Range)>> = Rc::default();
    let moving = Rc::new(Cell::new(false));
    let (wp, wc, st, mv) = (Rc::downgrade(p), Rc::downgrade(&c), start.clone(), moving.clone());
    drag.connect_drag_begin(move |_, _, _| {
        if let (Some(p), Some(c)) = (wp.upgrade(), wc.upgrade()) {
            let y = c.lock.get().unwrap_or(c.drawn.get());
            st.set((p.window(&c, &p.data.borrow()), y));
            mv.set(false);
        }
    });
    let (wp, wc) = (Rc::downgrade(p), Rc::downgrade(&c));
    drag.connect_drag_update(move |_, dx, dy| {
        let (Some(p), Some(c)) = (wp.upgrade(), wc.upgrade()) else { return };
        // A click (or a double-click) is not a drag.
        if !moving.get() && dx.abs() < 3.0 && dy.abs() < 3.0 {
            return;
        }
        moving.set(true);
        let (x, y) = start.get();
        p.pan(&c, x, y, dx, dy);
    });
    area.add_controller(drag);
    let click = gtk::GestureClick::new();
    let (wp, wc) = (Rc::downgrade(p), Rc::downgrade(&c));
    click.connect_pressed(move |_, n, _, _| {
        if let (2, Some(p), Some(c)) = (n, wp.upgrade(), wc.upgrade()) {
            p.live(&c);
        }
    });
    area.add_controller(click);

    // Right-click: cursors, threshold, PNG.
    let group = gio::SimpleActionGroup::new();
    let add = |name: &str, f: ChartFn| {
        let a = gio::SimpleAction::new(name, None);
        let (wp, wc) = (Rc::downgrade(p), Rc::downgrade(&c));
        a.connect_activate(move |_, _| {
            if let (Some(p), Some(c)) = (wp.upgrade(), wc.upgrade()) {
                f(&p, &c);
            }
        });
        group.add_action(&a);
    };
    for (name, slot) in [("cursor-a", 0usize), ("cursor-b", 1)] {
        add(
            name,
            Box::new(move |p, c| {
                let mut cur = p.cursors.get();
                cur[slot] = p.x_at(c);
                p.cursors.set(cur);
                p.redraw();
            }),
        );
    }
    add(
        "clear-cursors",
        Box::new(|p, _| {
            p.cursors.set([None, None]);
            p.redraw();
        }),
    );
    add(
        "threshold",
        Box::new(|p, c| {
            c.threshold.set(p.y_at(c));
            p.remember();
            c.area.queue_draw();
        }),
    );
    add(
        "clear-threshold",
        Box::new(|p, c| {
            c.threshold.set(None);
            p.remember();
            c.area.queue_draw();
        }),
    );
    add("png", Box::new(|_, c| ask_png(c.area.upcast_ref(), &format!("chart{}.png", c.idx + 1))));
    area.insert_action_group("chart", Some(&group));
    let menu = gio::Menu::new();
    let cursors = gio::Menu::new();
    cursors.append(Some("Cursor A Here"), Some("chart.cursor-a"));
    cursors.append(Some("Cursor B Here"), Some("chart.cursor-b"));
    cursors.append(Some("Clear Cursors"), Some("chart.clear-cursors"));
    menu.append_section(None, &cursors);
    let th = gio::Menu::new();
    th.append(Some("Threshold Here"), Some("chart.threshold"));
    th.append(Some("Clear Threshold"), Some("chart.clear-threshold"));
    menu.append_section(None, &th);
    let png = gio::Menu::new();
    png.append(Some("Save Chart as PNG…"), Some("chart.png"));
    menu.append_section(None, &png);
    let right = gtk::GestureClick::new();
    right.set_button(3);
    let wc = Rc::downgrade(&c);
    right.connect_pressed(move |_, _, x, y| {
        let Some(c) = wc.upgrade() else { return };
        c.pointer.set((x, y));
        let pop = gtk::PopoverMenu::from_model(Some(&menu));
        pop.set_parent(&c.area);
        pop.set_has_arrow(false);
        pop.set_pointing_to(Some(&gtk::gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        pop.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        pop.popup();
    });
    area.add_controller(right);
    c
}

/// Builds a plot fed by `out` (the pane's output).
pub fn build(out: &Rc<RefCell<output::Out>>) -> Rc<Plot> {
    let key = out.borrow().title.clone();
    let saved = load_saved(&key);
    let sep_names: Vec<&str> = SEPS.iter().map(|s| s.0).collect();
    let sep = gtk::DropDown::from_strings(&sep_names);
    sep.set_tooltip_text(Some("Separator between values (space cannot be one)"));
    let span_names: Vec<&str> = SPANS.iter().map(|s| s.0).collect();
    let span = gtk::DropDown::from_strings(&span_names);
    span.set_selected(2);
    span.set_tooltip_text(Some("How much is shown"));
    let layout_names: Vec<&str> = LAYOUTS.iter().map(|l| l.0).collect();
    let layout = gtk::DropDown::from_strings(&layout_names);
    layout.set_tooltip_text(Some("Number of charts and how they sit; drag the gaps to resize"));
    layout.set_selected(LAYOUTS.iter().position(|l| l.1 == saved.layout).unwrap_or(0) as u32);
    let split = gtk::Button::with_label("Split");
    split.set_tooltip_text(Some("One chart per channel (up to four)"));
    let link = gtk::ToggleButton::with_label("Link");
    link.set_tooltip_text(Some("Zoom and move the time axis of all charts together"));
    link.set_active(saved.link);
    let lock = gtk::ToggleButton::with_label("Lock Y");
    lock.set_tooltip_text(Some("Keep the current Y range of every chart"));
    let pause = gtk::ToggleButton::with_label("Pause");
    let names = gtk::Button::with_label("Names…");
    names.set_tooltip_text(Some("Channel names; a line of names from the device sets them too"));
    let clear = gtk::Button::from_icon_name("edit-clear-symbolic");
    clear.set_tooltip_text(Some("Clear"));
    let csv = gtk::Button::with_label("CSV");
    csv.set_tooltip_text(Some("Export samples as CSV"));
    let png = gtk::Button::with_label("PNG");
    png.set_tooltip_text(Some("Save all charts as one PNG image"));
    for b in [&split, &names, &clear, &csv, &png] {
        b.add_css_class("flat");
    }
    let rate = gtk::Label::new(Some("waiting for data"));
    rate.add_css_class("dim-label");
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let title = gtk::Label::new(Some("Plot"));
    title.add_css_class("heading");
    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    bar.add_css_class("serialbar");
    for w in [
        title.upcast_ref::<gtk::Widget>(),
        rate.upcast_ref(),
        spacer.upcast_ref(),
        sep.upcast_ref(),
        span.upcast_ref(),
        layout.upcast_ref(),
        split.upcast_ref(),
        link.upcast_ref(),
        lock.upcast_ref(),
        pause.upcast_ref(),
        names.upcast_ref(),
        clear.upcast_ref(),
        csv.upcast_ref(),
        png.upcast_ref(),
    ] {
        bar.append(w);
    }

    let host = gtk::Box::new(gtk::Orientation::Vertical, 0);
    host.set_vexpand(true);
    let legend = gtk::FlowBox::new();
    legend.set_selection_mode(gtk::SelectionMode::None);
    legend.set_max_children_per_line(MAX_CH as u32);
    legend.set_column_spacing(12);
    legend.set_margin_start(8);
    legend.set_margin_end(8);
    legend.set_margin_bottom(4);
    let cursor_info = gtk::Label::new(None);
    cursor_info.add_css_class("numeric");
    cursor_info.set_xalign(0.0);
    cursor_info.set_wrap(true);
    cursor_info.set_margin_start(8);
    cursor_info.set_margin_end(8);
    cursor_info.set_margin_bottom(4);
    cursor_info.set_visible(false);
    let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
    widget.add_css_class("plot");
    widget.append(&crate::term::fit(&bar));
    widget.append(&host);
    widget.append(&legend);
    widget.append(&cursor_info);

    let p = Rc::new(Plot {
        widget,
        host: host.clone(),
        legend,
        rate,
        cursor_info,
        charts: RefCell::default(),
        layout: Cell::new(Layout::One),
        data: RefCell::default(),
        names: RefCell::default(),
        shown: RefCell::default(),
        assign: RefCell::new(saved.assign.clone()),
        values: RefCell::default(),
        lines: RefCell::default(),
        sep: Cell::new(None),
        span: Cell::new(SPANS[2].1),
        paused: Cell::new(false),
        frozen: Cell::new(0.0),
        link: Cell::new(saved.link),
        zoom: Cell::new(None),
        cursors: Cell::new([None, None]),
        pause_button: RefCell::new(Some(pause.clone())),
        lock_button: RefCell::new(Some(lock.clone())),
        readout: RefCell::default(),
        key,
        start: Instant::now(),
        last: Cell::new(0.0),
        count: Cell::new(0),
        dirty: Cell::new(false),
    });
    SELF.with(|s| {
        let mut s = s.borrow_mut();
        s.retain(|w| w.upgrade().is_some());
        s.push(Rc::downgrade(&p));
    });
    p.set_layout(saved.layout);

    let w = Rc::downgrade(&p);
    sep.connect_selected_notify(move |d| {
        if let Some(p) = w.upgrade() {
            p.sep.set(SEPS.get(d.selected() as usize).and_then(|s| s.1));
        }
    });
    let w = Rc::downgrade(&p);
    span.connect_selected_notify(move |d| {
        if let Some(p) = w.upgrade() {
            p.span.set(SPANS.get(d.selected() as usize).map(|s| s.1).unwrap_or(Span::Secs(10.0)));
            p.redraw();
        }
    });
    let w = Rc::downgrade(&p);
    layout.connect_selected_notify(move |d| {
        if let Some(p) = w.upgrade() {
            p.set_layout(LAYOUTS.get(d.selected() as usize).map(|l| l.1).unwrap_or_default());
            p.remember();
        }
    });
    let (w, ld) = (Rc::downgrade(&p), layout.downgrade());
    split.connect_clicked(move |_| {
        let (Some(p), Some(ld)) = (w.upgrade(), ld.upgrade()) else { return };
        let channels = p.names.borrow().len().max(1);
        let n = channels.min(MAX_CHARTS);
        for (i, a) in p.assign.borrow_mut().iter_mut().enumerate() {
            *a = i % n;
        }
        // Keep the chosen arrangement when it already has n charts.
        let target = if p.layout.get().count() == n { p.layout.get() } else { Layout::for_count(n) };
        let pos = LAYOUTS.iter().position(|l| l.1 == target).unwrap_or(0) as u32;
        if ld.selected() == pos {
            p.set_layout(target);
            p.remember();
        } else {
            ld.set_selected(pos);
        }
    });
    let w = Rc::downgrade(&p);
    link.connect_toggled(move |b| {
        if let Some(p) = w.upgrade() {
            p.link.set(b.is_active());
            // Start from the first chart's range so nothing jumps.
            if b.is_active() {
                let first = p.charts.borrow().first().and_then(|c| c.zoom.get());
                p.zoom.set(first);
            }
            p.remember();
            p.redraw();
        }
    });
    let w = Rc::downgrade(&p);
    lock.connect_toggled(move |b| {
        if let Some(p) = w.upgrade() {
            for c in p.charts.borrow().iter() {
                // Ctrl+wheel sets its own range before turning this on.
                if !b.is_active() {
                    c.lock.set(None);
                } else if c.lock.get().is_none() {
                    c.lock.set(Some(c.drawn.get()));
                }
            }
            p.redraw();
        }
    });
    let w = Rc::downgrade(&p);
    pause.connect_toggled(move |b| {
        if let Some(p) = w.upgrade() {
            if b.is_active() {
                p.freeze();
            } else {
                // Unpausing goes back to the live view.
                p.zoom.set(None);
                for c in p.charts.borrow().iter() {
                    c.zoom.set(None);
                }
                p.paused.set(false);
            }
            p.redraw();
        }
    });
    let w = Rc::downgrade(&p);
    clear.connect_clicked(move |_| {
        if let Some(p) = w.upgrade() {
            p.data.borrow_mut().clear();
            p.redraw();
        }
    });
    let w = Rc::downgrade(&p);
    names.connect_clicked(move |b| {
        let Some(p) = w.upgrade() else { return };
        let d = adw::AlertDialog::new(Some("Channel Names"), Some("Comma separated, in channel order."));
        let e = gtk::Entry::new();
        e.set_text(&p.names.borrow().join(", "));
        e.set_activates_default(true);
        d.set_extra_child(Some(&e));
        d.add_responses(&[("cancel", "Cancel"), ("ok", "Set")]);
        d.set_response_appearance("ok", adw::ResponseAppearance::Suggested);
        d.set_default_response(Some("ok"));
        d.set_close_response("cancel");
        let w = Rc::downgrade(&p);
        d.connect_response(None, move |_, r| {
            let Some(p) = w.upgrade() else { return };
            if r != "ok" {
                return;
            }
            let list: Vec<String> = e.text().split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
            if list.len() > p.names.borrow().len() {
                p.grow(list.len());
            }
            for (i, n) in list.into_iter().enumerate() {
                if let Some(slot) = p.names.borrow_mut().get_mut(i) {
                    *slot = n;
                }
            }
            p.fill_legend();
        });
        d.present(Some(b));
    });
    let w = Rc::downgrade(&p);
    csv.connect_clicked(move |b| {
        let dlg = gtk::FileDialog::new();
        dlg.set_initial_name(Some("plot.csv"));
        let (w, b2) = (w.clone(), b.clone());
        dlg.save(b.root().and_downcast::<gtk::Window>().as_ref(), None::<&gio::Cancellable>, move |res| {
            let (Some(path), Some(p)) = (res.ok().and_then(|f| f.path()), w.upgrade()) else { return };
            match p.export(&path) {
                Ok(n) => toast(&b2, &format!("{n} samples saved to {}", path.display())),
                Err(e) => toast(&b2, &format!("Export failed: {e}")),
            }
        });
    });
    let h = host.downgrade();
    png.connect_clicked(move |_| {
        if let Some(h) = h.upgrade() {
            ask_png(h.upcast_ref(), "plot.png");
        }
    });

    // Redraw about 30 times a second while data comes; sample rate once a second.
    let w = Rc::downgrade(&p);
    let mut seen = (0u64, Instant::now());
    glib::timeout_add_local(Duration::from_millis(33), move || {
        let Some(p) = w.upgrade() else { return glib::ControlFlow::Break };
        if p.dirty.replace(false) && !p.paused.get() {
            p.redraw();
        }
        p.show_readout();
        if seen.1.elapsed() >= Duration::from_secs(1) {
            let n = p.count.get();
            let hz = (n - seen.0) as f64 / seen.1.elapsed().as_secs_f64();
            seen = (n, Instant::now());
            let ch = p.names.borrow().len();
            if n > 0 {
                p.rate.set_text(&format!("{hz:.0} Hz · {ch} ch · {n} samples"));
            }
        }
        glib::ControlFlow::Continue
    });

    let watcher: Rc<dyn Watch> = p.clone();
    out.borrow_mut().watchers.push(Rc::downgrade(&watcher));
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lines() {
        assert_eq!(parse("0,27200,1000,305,0,0", None), Line::Values(vec![0.0, 27200.0, 1000.0, 305.0, 0.0, 0.0]));
        assert_eq!(parse("0|2323|12312|232", None), Line::Values(vec![0.0, 2323.0, 12312.0, 232.0]));
        assert_eq!(parse(" 1.5 ; -2e3 ; 0x1F ", None), Line::Values(vec![1.5, -2000.0, 31.0]));
        assert_eq!(parse("1,2,3,", None), Line::Values(vec![1.0, 2.0, 3.0]));
        assert_eq!(parse("42", None), Line::Values(vec![42.0]));
        assert_eq!(parse("bla bla bla", None), Line::Skip);
        assert_eq!(parse("", None), Line::Skip);
        assert_eq!(parse("kanal,adc,pid", None), Line::Names(vec!["kanal".into(), "adc".into(), "pid".into()]));
        // A label among numbers leaves a gap in that column.
        match parse("ADC:512", None) {
            Line::Values(v) => assert!(v[0].is_nan() && v[1] == 512.0),
            other => panic!("{other:?}"),
        }
        // A chosen separator wins; spaces are only padding.
        assert_eq!(parse("1|2,5|3", Some('|')).replace_nan(), Line::Values(vec![1.0, -0.123, 3.0]));
        assert_eq!(parse("1 2 3", None), Line::Skip);
    }

    impl Line {
        /// NaN never equals itself; make it comparable in tests.
        fn replace_nan(self) -> Line {
            match self {
                Line::Values(v) => Line::Values(v.into_iter().map(|x| if x.is_nan() { -0.123 } else { x }).collect()),
                o => o,
            }
        }
    }

    #[test]
    fn lines_across_chunks() {
        let mut l = Lines::default();
        assert!(l.feed("1,2").is_empty());
        assert_eq!(l.feed(",3\r\n4,5,6\n7"), vec!["1,2,3".to_string(), "4,5,6".to_string()]);
        assert_eq!(l.feed(",8\n"), vec!["7,8".to_string()]);
    }

    #[test]
    fn zoom_keeps_the_pointed_spot() {
        let (a, b) = zoomed((0.0, 10.0), 0.25, 0.8);
        assert!((a - 0.5).abs() < 1e-9 && (b - 8.5).abs() < 1e-9);
        // The spot under the pointer (2.5) is at the same fraction after.
        assert!(((2.5 - a) / (b - a) - 0.25).abs() < 1e-9);
        let back = zoomed((a, b), 0.25, 1.25);
        assert!((back.0 - 0.0).abs() < 1e-9 && (back.1 - 10.0).abs() < 1e-9);
        assert_eq!(zoomed((0.0, 10.0), 2.0, 0.5), (5.0, 10.0));
    }

    #[test]
    fn ticks() {
        assert_eq!(nice_step(100.0, 5.0), 20.0);
        assert_eq!(nice_step(0.9, 5.0), 0.2);
        assert_eq!(nice_step(27000.0, 5.0), 5000.0);
        assert_eq!(tick_label(0.4, 0.2), "0.4");
        assert_eq!(tick_label(25000.0, 5000.0), "25000");
    }

    #[test]
    fn layouts() {
        for (_, l) in LAYOUTS {
            assert_eq!(Layout::for_count(l.count()).count(), l.count());
        }
        assert_eq!(Layout::for_count(9), Layout::Grid);
        assert_eq!(Layout::for_count(0), Layout::One);
    }

    #[test]
    fn statistics_and_numbers() {
        assert_eq!(stats([1.0, f64::NAN, 3.0, 5.0].into_iter()), Some((1.0, 5.0, 3.0)));
        assert_eq!(stats([f64::NAN].into_iter()), None);
        assert_eq!(short(0.1 + 0.2), "0.3");
        assert_eq!(short(27200.0), "27200");
        assert_eq!(short(-1.23456789), "-1.2346");
    }

    #[test]
    fn saved_setup_roundtrip() {
        let s = Saved { layout: Layout::TwoBesideOne, assign: vec![0, 2, 1], link: true, thresholds: vec![None, Some(3.5), None] };
        let text = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<Saved>(&text).unwrap(), s);
        // Older or partial entries still load.
        assert_eq!(serde_json::from_str::<Saved>("{}").unwrap(), Saved::default());
    }
}
