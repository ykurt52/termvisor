// Live plot of numbers a pane prints: each line is one sample, its
// fields (split by a separator other than space) are the channels.
// Shown above the pane's terminal; Ctrl+Shift+G toggles it.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io::Write;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gio, glib};

use crate::output::{self, Watch};

/// Separators tried in this order when none is chosen.
pub const AUTO: [char; 5] = [',', '|', ';', '\t', ':'];
/// Samples kept.
const KEEP: usize = 200_000;
/// Channels at most.
const MAX_CH: usize = 32;

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

/// Position of a sample across the plot, 0..1.
type XPos = Box<dyn Fn(&Sample) -> f64>;

struct Sample {
    /// Seconds since the plot started.
    t: f64,
    /// Sample number.
    n: u64,
    v: Vec<f64>,
}

pub struct Plot {
    pub widget: gtk::Box,
    area: gtk::DrawingArea,
    legend: gtk::FlowBox,
    rate: gtk::Label,
    data: RefCell<VecDeque<Sample>>,
    names: RefCell<Vec<String>>,
    shown: RefCell<Vec<bool>>,
    /// Value labels of the legend, per channel.
    values: RefCell<Vec<gtk::Label>>,
    lines: RefCell<Lines>,
    sep: Cell<Option<char>>,
    span: Cell<Span>,
    paused: Cell<bool>,
    /// Plot time frozen by Pause.
    frozen: Cell<f64>,
    lock: Cell<Option<(f64, f64)>>,
    /// Last drawn Y range, for Lock Y.
    drawn: Cell<(f64, f64)>,
    hover: Cell<Option<f64>>,
    /// Values for the legend, found while drawing and shown by the timer
    /// (changing labels while drawing would relayout mid-frame).
    readout: RefCell<Vec<Option<f64>>>,
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
        }
        self.fill_legend();
    }

    fn fill_legend(&self) {
        self.legend.remove_all();
        let mut labels = Vec::new();
        let names = self.names.borrow().clone();
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
            value.add_css_class("dim-label");
            value.add_css_class("numeric");
            value.set_width_chars(9);
            value.set_xalign(1.0);
            let b = gtk::Box::new(gtk::Orientation::Horizontal, 4);
            b.append(&check);
            b.append(&dot);
            b.append(&label);
            b.append(&value);
            let w = self.me();
            check.connect_toggled(move |c| {
                if let Some(p) = w.upgrade() {
                    if let Some(s) = p.shown.borrow_mut().get_mut(i) {
                        *s = c.is_active();
                    }
                    p.area.queue_draw();
                }
            });
            self.legend.append(&b);
            labels.push(value);
        }
        self.values.replace(labels);
    }

    fn me(&self) -> Weak<Plot> {
        SELF.with(|s| s.borrow().iter().find(|w| w.upgrade().is_some_and(|p| std::ptr::eq(&*p, self))).cloned().unwrap_or_default())
    }

    /// Right edge of the shown time.
    fn now(&self) -> f64 {
        if self.paused.get() { self.frozen.get() } else { self.start.elapsed().as_secs_f64() }
    }

    /// Samples in view: (first index, x of a sample as 0..1).
    fn view(&self, d: &VecDeque<Sample>) -> (usize, XPos) {
        match self.span.get() {
            Span::Secs(w) => {
                let end = self.now();
                let from = d.partition_point(|s| s.t < end - w);
                (from, Box::new(move |s: &Sample| (s.t - (end - w)) / w))
            }
            Span::Samples(n) => {
                let last = if self.paused.get() {
                    d.iter().rev().find(|s| s.t <= self.frozen.get()).map(|s| s.n).unwrap_or(0)
                } else {
                    d.back().map(|s| s.n).unwrap_or(0)
                };
                let first = last.saturating_sub(n as u64 - 1);
                let from = d.partition_point(|s| s.n < first);
                let w = (n - 1).max(1) as f64;
                (from, Box::new(move |s: &Sample| (s.n as f64 - first as f64) / w))
            }
        }
    }

    fn draw(&self, a: &gtk::DrawingArea, cr: &gtk::cairo::Context, w: i32, h: i32) {
        let fg = a.color();
        let (fr, fgc, fb) = (f64::from(fg.red()), f64::from(fg.green()), f64::from(fg.blue()));
        let (left, right, top, bottom) = (58.0, 10.0, 8.0, 20.0);
        let (pw, ph) = (f64::from(w) - left - right, f64::from(h) - top - bottom);
        if pw < 20.0 || ph < 20.0 {
            return;
        }
        let d = self.data.borrow();
        let shown = self.shown.borrow().clone();
        let (from, xf) = self.view(&d);
        let to = match self.span.get() {
            Span::Secs(_) if self.paused.get() => d.partition_point(|s| s.t <= self.frozen.get()),
            Span::Samples(_) if self.paused.get() => d.partition_point(|s| s.t <= self.frozen.get()),
            _ => d.len(),
        };
        let vis = &d.range(from..to.max(from)).collect::<Vec<_>>();

        // Y range: locked, else the shown values.
        let (lo, hi) = self.lock.get().unwrap_or_else(|| {
            let mut lo = f64::INFINITY;
            let mut hi = f64::NEG_INFINITY;
            for s in vis.iter() {
                for (i, v) in s.v.iter().enumerate() {
                    if shown.get(i).copied().unwrap_or(true) && v.is_finite() {
                        lo = lo.min(*v);
                        hi = hi.max(*v);
                    }
                }
            }
            if !lo.is_finite() {
                (0.0, 1.0)
            } else if hi - lo < 1e-9 {
                (lo - 1.0, hi + 1.0)
            } else {
                let pad = (hi - lo) * 0.05;
                (lo - pad, hi + pad)
            }
        });
        self.drawn.set((lo, hi));
        let y = |v: f64| top + ph - (v - lo) / (hi - lo) * ph;

        // Grid and labels.
        cr.set_line_width(1.0);
        cr.set_font_size(10.0);
        let step = nice_step(hi - lo, 5.0);
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
        let xlabel = match self.span.get() {
            Span::Secs(w) => format!("last {w:.0} s"),
            Span::Samples(n) => format!("last {n} samples"),
        };
        cr.move_to(left, f64::from(h) - 5.0);
        let _ = cr.show_text(&xlabel);
        cr.set_source_rgba(fr, fgc, fb, 0.25);
        cr.rectangle(left + 0.5, top + 0.5, pw - 1.0, ph - 1.0);
        let _ = cr.stroke();

        // Lines: per pixel column keep first, min, max and last, so dense
        // data draws in time proportional to the width.
        cr.save().ok();
        cr.rectangle(left, top, pw, ph);
        cr.clip();
        cr.set_line_width(1.4);
        let chans = shown.len();
        for (ch, on) in shown.iter().enumerate().take(chans) {
            if !on {
                continue;
            }
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
                let c = x as i64;
                let yy = y(val);
                if c == col && pen {
                    if yy < mn || yy > mx {
                        mn = mn.min(yy);
                        mx = mx.max(yy);
                        cr.line_to(x, yy);
                    }
                    continue;
                }
                col = c;
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
        cr.restore().ok();

        // Hover: a line and the values there; else the latest values.
        let at = self.hover.get().filter(|hx| *hx >= left && *hx <= left + pw);
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
        self.readout.replace((0..n).map(|i| pick.and_then(|s| s.v.get(i)).copied().filter(|v| v.is_finite())).collect());
    }

    /// Shows the values found by the last draw in the legend.
    fn show_readout(&self) {
        let values = self.values.borrow();
        let read = self.readout.borrow();
        for (i, l) in values.iter().enumerate() {
            let text = read.get(i).copied().flatten().map(|v| format!("{v}")).unwrap_or_else(|| "–".into());
            if l.text() != text {
                l.set_text(&text);
            }
        }
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

/// Builds a plot fed by `out` (the pane's output).
pub fn build(out: &Rc<RefCell<output::Out>>) -> Rc<Plot> {
    let sep_names: Vec<&str> = SEPS.iter().map(|s| s.0).collect();
    let sep = gtk::DropDown::from_strings(&sep_names);
    sep.set_tooltip_text(Some("Separator between values (space cannot be one)"));
    let span_names: Vec<&str> = SPANS.iter().map(|s| s.0).collect();
    let span = gtk::DropDown::from_strings(&span_names);
    span.set_selected(2);
    span.set_tooltip_text(Some("How much is shown"));
    let lock = gtk::ToggleButton::with_label("Lock Y");
    lock.set_tooltip_text(Some("Keep the current Y range"));
    let pause = gtk::ToggleButton::with_label("Pause");
    let names = gtk::Button::with_label("Names…");
    names.set_tooltip_text(Some("Channel names; a line of names from the device sets them too"));
    let clear = gtk::Button::from_icon_name("edit-clear-symbolic");
    clear.set_tooltip_text(Some("Clear"));
    let save = gtk::Button::from_icon_name("document-save-symbolic");
    save.set_tooltip_text(Some("Export samples as CSV"));
    for b in [&names, &clear, &save] {
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
        lock.upcast_ref(),
        pause.upcast_ref(),
        names.upcast_ref(),
        clear.upcast_ref(),
        save.upcast_ref(),
    ] {
        bar.append(w);
    }

    let area = gtk::DrawingArea::new();
    area.set_vexpand(true);
    area.set_content_height(160);
    let legend = gtk::FlowBox::new();
    legend.set_selection_mode(gtk::SelectionMode::None);
    legend.set_max_children_per_line(MAX_CH as u32);
    legend.set_column_spacing(12);
    legend.set_margin_start(8);
    legend.set_margin_end(8);
    legend.set_margin_bottom(4);
    let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
    widget.add_css_class("plot");
    widget.append(&bar);
    widget.append(&area);
    widget.append(&legend);

    let p = Rc::new(Plot {
        widget,
        area: area.clone(),
        legend,
        rate,
        data: RefCell::default(),
        names: RefCell::default(),
        shown: RefCell::default(),
        values: RefCell::default(),
        lines: RefCell::default(),
        sep: Cell::new(None),
        span: Cell::new(SPANS[2].1),
        paused: Cell::new(false),
        frozen: Cell::new(0.0),
        lock: Cell::new(None),
        drawn: Cell::new((0.0, 1.0)),
        hover: Cell::new(None),
        readout: RefCell::default(),
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

    let w = Rc::downgrade(&p);
    area.set_draw_func(move |a, cr, wd, ht| {
        if let Some(p) = w.upgrade() {
            p.draw(a, cr, wd, ht);
        }
    });
    let motion = gtk::EventControllerMotion::new();
    let w = Rc::downgrade(&p);
    motion.connect_motion(move |_, x, _| {
        if let Some(p) = w.upgrade() {
            p.hover.set(Some(x));
            p.area.queue_draw();
        }
    });
    let w = Rc::downgrade(&p);
    motion.connect_leave(move |_| {
        if let Some(p) = w.upgrade() {
            p.hover.set(None);
            p.area.queue_draw();
        }
    });
    area.add_controller(motion);

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
            p.area.queue_draw();
        }
    });
    let w = Rc::downgrade(&p);
    lock.connect_toggled(move |b| {
        if let Some(p) = w.upgrade() {
            p.lock.set(b.is_active().then(|| p.drawn.get()));
            p.area.queue_draw();
        }
    });
    let w = Rc::downgrade(&p);
    pause.connect_toggled(move |b| {
        if let Some(p) = w.upgrade() {
            p.frozen.set(p.start.elapsed().as_secs_f64());
            p.paused.set(b.is_active());
            p.area.queue_draw();
        }
    });
    let w = Rc::downgrade(&p);
    clear.connect_clicked(move |_| {
        if let Some(p) = w.upgrade() {
            p.data.borrow_mut().clear();
            p.area.queue_draw();
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
    save.connect_clicked(move |b| {
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

    // Redraw about 30 times a second while data comes; sample rate once a second.
    let w = Rc::downgrade(&p);
    let mut seen = (0u64, Instant::now());
    glib::timeout_add_local(Duration::from_millis(33), move || {
        let Some(p) = w.upgrade() else { return glib::ControlFlow::Break };
        if p.dirty.replace(false) && !p.paused.get() {
            p.area.queue_draw();
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
    fn ticks() {
        assert_eq!(nice_step(100.0, 5.0), 20.0);
        assert_eq!(nice_step(0.9, 5.0), 0.2);
        assert_eq!(nice_step(27000.0, 5.0), 5000.0);
        assert_eq!(tick_label(0.4, 0.2), "0.4");
        assert_eq!(tick_label(25000.0, 5000.0), "25000");
    }
}
