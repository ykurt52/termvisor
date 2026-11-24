// Modbus master and slave panes. A master polls tables of a slave and
// shows them; a slave answers requests from its own four tables. Both
// log the frames in the terminal below their tables.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::glib;
use serde::{Deserialize, Serialize};

use crate::mblink::{self, Ev, Link, ModbusCfg, Role, Transport};
use crate::modbus::{self, Bank, Format, Got, Reader, Request, Response, Table};
use crate::output::Out;
use crate::state::State;
use crate::term::{self, Launch};

/// Rows shown at most in one table, to keep the page light.
const MAX_ROWS: u16 = 250;

/// One poll of a master: what to read and how to show it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PollCfg {
    pub unit: u8,
    pub table: Table,
    pub addr: u16,
    pub count: u16,
    pub rate_ms: u32,
    pub format: Format,
    pub on: bool,
}

impl Default for PollCfg {
    fn default() -> Self {
        Self { unit: 1, table: Table::Holding, addr: 0, count: 10, rate_ms: 1000, format: Format::Unsigned, on: true }
    }
}

/// The view of one slave table.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ViewCfg {
    pub addr: u16,
    pub count: u16,
    pub format: Format,
}

impl Default for ViewCfg {
    fn default() -> Self {
        Self { addr: 0, count: 20, format: Format::Unsigned }
    }
}

/// What a session remembers, by its title.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    polls: Vec<PollCfg>,
    views: Vec<ViewCfg>,
    /// Slave values that are not zero, per table, as address → value.
    values: BTreeMap<String, BTreeMap<u16, u16>>,
}

const SAVED: &str = "modbus.json";

fn load(key: &str) -> Saved {
    let text = std::fs::read_to_string(crate::recent::path(SAVED)).unwrap_or_default();
    let all: BTreeMap<String, Saved> = serde_json::from_str(&text).unwrap_or_default();
    all.get(key).cloned().unwrap_or_default()
}

fn store(key: &str, s: &Saved) {
    let path = crate::recent::path(SAVED);
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let mut all: BTreeMap<String, Saved> = serde_json::from_str(&text).unwrap_or_default();
    all.insert(key.to_string(), s.clone());
    let result = serde_json::to_string_pretty(&all).map_err(std::io::Error::other).and_then(|t| {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        crate::store::write_private(&path, &t)
    });
    if let Err(e) = result {
        crate::warn(&format!("{}: {e}", path.display()));
    }
}

/// Writes log lines to the pane's terminal (and its log file).
#[derive(Clone)]
struct Log {
    term: glib::WeakRef<crate::vt::View>,
    out: Rc<RefCell<Out>>,
    start: Instant,
}

impl Log {
    fn line(&self, sgr: &str, text: &str) {
        let Some(t) = self.term.upgrade() else { return };
        let stamp = format!("{:9.3}", self.start.elapsed().as_secs_f64());
        let s = if sgr.is_empty() { format!("\x1b[2m{stamp}\x1b[0m {text}\r\n") } else { format!("\x1b[2m{stamp}\x1b[0m \x1b[{sgr}m{text}\x1b[0m\r\n") };
        let data = self.out.borrow_mut().process(s.as_bytes()).into_owned();
        t.feed(&data);
    }

    fn tx(&self, raw: &[u8]) {
        self.line("36", &format!("TX {}", modbus::hex(raw)));
    }

    fn rx(&self, raw: &[u8]) {
        self.line("32", &format!("RX {}", modbus::hex(raw)));
    }

    /// Sends a line of values to the pane's plot (not shown in the terminal).
    fn plot(&self, line: &str) {
        let watchers: Vec<_> = self.out.borrow().watchers.iter().filter_map(|w| w.upgrade()).collect();
        for w in watchers {
            w.text(line);
        }
    }
}

fn spin(min: f64, max: f64, value: f64, tip: &str) -> gtk::SpinButton {
    let s = gtk::SpinButton::with_range(min, max, 1.0);
    s.set_value(value);
    s.set_tooltip_text(Some(tip));
    s.update_property(&[gtk::accessible::Property::Label(tip)]);
    s
}

fn dropdown(items: &[&str], tip: &str, selected: usize) -> gtk::DropDown {
    let d = gtk::DropDown::from_strings(items);
    d.set_tooltip_text(Some(tip));
    d.set_selected(selected as u32);
    d
}

fn caption(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("dim-label");
    l
}

fn bar() -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    b.add_css_class("serialbar");
    b
}

fn state_dot(dot: &gtk::Label, text: &str, tint: &str) {
    dot.set_text(text);
    for c in ["tint-green", "tint-red", "tint-yellow"] {
        dot.remove_css_class(c);
    }
    dot.add_css_class(tint);
}

fn format_names() -> Vec<&'static str> {
    Format::ALL.iter().map(|f| f.label()).collect()
}

fn table_names() -> Vec<String> {
    Table::ALL.iter().map(|t| format!("{:02} {}", t.read_fc(), t.label())).collect()
}

/// Wait after a reply before the next RTU request: 3.5 characters, at
/// least 2 ms.
fn rtu_gap(baud: u32) -> Duration {
    Duration::from_secs_f64((3.5 * 11.0 / f64::from(baud.max(1))).max(0.002))
}

/// Applies a change to a poll's settings.
type Edit = dyn Fn(&dyn Fn(&mut PollCfg));

/// Builds the pane for `launch.modbus`.
pub fn build(state: &Rc<State>, launch: &Launch) -> (crate::vt::View, gtk::Widget) {
    let t = term::spawn(state, launch);
    let out = crate::output::of(&t).unwrap_or_else(|| crate::output::attach(&t, &launch.title, &state.cfg.borrow()));
    let log = Log { term: t.downgrade(), out, start: Instant::now() };
    let cfg = launch.modbus.clone().unwrap_or_default();
    let key = launch.title.clone();
    let top: gtk::Widget = match cfg.role {
        Role::Master => master(&t, cfg, key, log),
        Role::Slave => slave(&t, cfg, key, log),
    };
    let split = gtk::Paned::new(gtk::Orientation::Vertical);
    split.set_wide_handle(true);
    split.set_shrink_start_child(false);
    // Extra height (window grows, bottom panel hides) goes to the log.
    split.set_resize_start_child(false);
    split.set_start_child(Some(&top));
    split.set_end_child(Some(&term::scroll(&t)));
    split.set_position(420);
    split.set_vexpand(true);
    (t, split.upcast())
}

// ---------------------------------------------------------------- master

/// A request on its way, with what to do with the answer.
struct Job {
    unit: u8,
    req: Request,
    /// Poll that asked; None for a write.
    poll: Option<Weak<Poll>>,
}

struct Pending {
    job: Job,
    transaction: u16,
    timer: Option<glib::SourceId>,
}

struct Poll {
    cfg: RefCell<PollCfg>,
    /// Last values read (registers, or bits as 0/1).
    values: RefCell<Vec<u16>>,
    queued: Cell<bool>,
    timer: RefCell<Option<glib::SourceId>>,
    ok: Cell<u64>,
    errors: Cell<u64>,
    timeouts: Cell<u64>,
    status: gtk::Label,
    grid: gtk::Grid,
    /// Value labels of the grid, one per row.
    cells: RefCell<Vec<gtk::Label>>,
    plot: gtk::CheckButton,
}

struct Master {
    cfg: ModbusCfg,
    key: String,
    log: Log,
    link: RefCell<Option<Link>>,
    up: Cell<bool>,
    reader: RefCell<Reader>,
    polls: RefCell<Vec<Rc<Poll>>>,
    queue: RefCell<VecDeque<Job>>,
    busy: RefCell<Option<Pending>>,
    transaction: Cell<u16>,
    tabs: gtk::Notebook,
    dot: gtk::Label,
    stats: gtk::Label,
    sent: Cell<u64>,
    got: Cell<u64>,
    /// Next connect attempt after the link dropped or failed.
    retry: RefCell<Option<glib::SourceId>>,
    last_note: RefCell<String>,
    me: RefCell<Weak<Master>>,
}

fn master(t: &crate::vt::View, cfg: ModbusCfg, key: String, log: Log) -> gtk::Widget {
    let saved = load(&key);
    let tcp = cfg.transport == Transport::Tcp;
    let name = gtk::Label::new(Some(&format!("Master {}", cfg.desc())));
    name.add_css_class("heading");
    let dot = gtk::Label::new(None);
    let stats = caption("");
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let add = gtk::Button::with_label("Add Poll");
    add.set_tooltip_text(Some("Another table to read, in its own tab"));
    let again = gtk::Button::from_icon_name("view-refresh-symbolic");
    again.set_tooltip_text(Some("Reconnect"));
    let clear = gtk::Button::from_icon_name("edit-clear-symbolic");
    clear.set_tooltip_text(Some("Clear the log"));
    for b in [&add, &again, &clear] {
        b.add_css_class("flat");
    }
    let top = bar();
    for w in [name.upcast_ref::<gtk::Widget>(), dot.upcast_ref(), stats.upcast_ref(), spacer.upcast_ref(), add.upcast_ref(), again.upcast_ref(), clear.upcast_ref()] {
        top.append(w);
    }
    let tabs = gtk::Notebook::new();
    tabs.set_scrollable(true);
    tabs.set_vexpand(true);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&term::fit(&top));
    body.append(&tabs);

    let m = Rc::new(Master {
        cfg,
        key,
        log,
        link: RefCell::new(None),
        up: Cell::new(false),
        reader: RefCell::new(Reader::new(tcp, false)),
        polls: RefCell::default(),
        queue: RefCell::default(),
        busy: RefCell::new(None),
        transaction: Cell::new(0),
        tabs,
        dot,
        stats,
        sent: Cell::new(0),
        got: Cell::new(0),
        retry: RefCell::new(None),
        last_note: RefCell::default(),
        me: RefCell::new(Weak::new()),
    });
    m.me.replace(Rc::downgrade(&m));
    let polls = if saved.polls.is_empty() { vec![PollCfg { unit: m.cfg.unit, ..PollCfg::default() }] } else { saved.polls };
    for p in polls {
        m.add_poll(p);
    }
    let w = Rc::downgrade(&m);
    add.connect_clicked(move |_| {
        if let Some(m) = w.upgrade() {
            let last = m.polls.borrow().last().map(|p| p.cfg.borrow().clone()).unwrap_or_default();
            m.add_poll(PollCfg { addr: last.addr.saturating_add(last.count), ..last });
            m.tabs.set_current_page(Some(m.tabs.n_pages().saturating_sub(1)));
            m.remember();
        }
    });
    let w = Rc::downgrade(&m);
    again.connect_clicked(move |_| {
        if let Some(m) = w.upgrade() {
            m.connect();
        }
    });
    let tw = t.downgrade();
    clear.connect_clicked(move |_| {
        if let Some(t) = tw.upgrade() {
            t.reset(true, true);
        }
    });
    let mm = m.clone();
    t.connect_destroy(move |_| {
        mm.link.take();
        if let Some(id) = mm.retry.take() {
            id.remove();
        }
        for p in mm.polls.borrow().iter() {
            if let Some(id) = p.timer.take() {
                id.remove();
            }
        }
        if let Some(p) = mm.busy.take() {
            if let Some(id) = p.timer {
                id.remove();
            }
        }
    });
    m.connect();
    body.upcast()
}

impl Master {
    fn me(&self) -> Weak<Master> {
        self.me.borrow().clone()
    }

    fn remember(&self) {
        let mut s = load(&self.key);
        s.polls = self.polls.borrow().iter().map(|p| p.cfg.borrow().clone()).collect();
        store(&self.key, &s);
    }

    fn show_stats(&self) {
        let (mut errors, mut timeouts) = (0, 0);
        for p in self.polls.borrow().iter() {
            errors += p.errors.get();
            timeouts += p.timeouts.get();
        }
        self.stats.set_text(&format!("TX {} · RX {} · errors {errors} · timeouts {timeouts}", self.sent.get(), self.got.get()));
    }

    /// Tries to connect again in 2 s, until it works or the pane closes.
    fn retry_later(&self) {
        if self.retry.borrow().is_some() {
            return;
        }
        let w = self.me();
        self.retry.replace(Some(glib::timeout_add_local_once(Duration::from_secs(2), move || {
            if let Some(m) = w.upgrade() {
                m.retry.take();
                m.connect();
            }
        })));
    }

    fn connect(&self) {
        if let Some(id) = self.retry.take() {
            id.remove();
        }
        self.link.take();
        self.up.set(false);
        self.queue.borrow_mut().clear();
        if let Some(p) = self.busy.take() {
            if let Some(id) = p.timer {
                id.remove();
            }
        }
        for p in self.polls.borrow().iter() {
            p.queued.set(false);
        }
        state_dot(&self.dot, "● connecting", "tint-yellow");
        match mblink::open(&self.cfg) {
            Err(e) => {
                state_dot(&self.dot, "○ disconnected · retrying", "tint-red");
                if self.last_note.replace(e.clone()) != e {
                    self.log.line("31", &e);
                }
                self.retry_later();
            }
            Ok((link, rx)) => {
                self.link.replace(Some(link));
                let w = self.me();
                glib::spawn_future_local(async move {
                    while let Ok(ev) = rx.recv().await {
                        let Some(m) = w.upgrade() else { break };
                        m.event(ev);
                    }
                });
                // A serial port is ready once open; it sends no Up.
                if self.cfg.transport == Transport::Rtu {
                    self.event(Ev::Up(format!("opened {}", self.cfg.desc())));
                }
            }
        }
    }

    fn event(&self, ev: Ev) {
        match ev {
            Ev::Up(note) => {
                self.up.set(true);
                self.last_note.borrow_mut().clear();
                state_dot(&self.dot, "● connected", "tint-green");
                self.log.line("2", &note);
                self.pump();
            }
            Ev::Note(note) => self.log.line("2", &note),
            Ev::Closed(why) => {
                self.up.set(false);
                self.link.take();
                state_dot(&self.dot, "○ disconnected · retrying", "tint-red");
                // Same note on every retry: show it once.
                if self.last_note.replace(why.clone()) != why {
                    self.log.line("31", &why);
                }
                self.retry_later();
            }
            Ev::Data(_, data) => {
                let got = self.reader.borrow_mut().feed(&data);
                for g in got {
                    match g {
                        Got::Frame(f) => self.answer(f),
                        Got::Junk(b) => self.log.line("33", &format!("?? {}", modbus::hex(&b))),
                    }
                }
            }
        }
    }

    /// Starts the poll timers and keeps them in step with the settings.
    fn schedule(&self, p: &Rc<Poll>) {
        if let Some(id) = p.timer.take() {
            id.remove();
        }
        let cfg = p.cfg.borrow().clone();
        if !cfg.on {
            return;
        }
        let (w, wp) = (self.me(), Rc::downgrade(p));
        let tick = move || {
            let (Some(m), Some(p)) = (w.upgrade(), wp.upgrade()) else { return glib::ControlFlow::Break };
            if m.up.get() && !p.queued.get() {
                p.queued.set(true);
                let c = p.cfg.borrow().clone();
                m.queue.borrow_mut().push_back(Job { unit: c.unit, req: Request::Read { table: c.table, addr: c.addr, count: c.count }, poll: Some(Rc::downgrade(&p)) });
                m.pump();
            }
            glib::ControlFlow::Continue
        };
        let first = tick.clone();
        first();
        p.timer.replace(Some(glib::timeout_add_local(Duration::from_millis(u64::from(cfg.rate_ms.max(20))), tick)));
    }

    /// Sends the next request when none is waiting for its answer.
    fn pump(&self) {
        if self.busy.borrow().is_some() || !self.up.get() {
            return;
        }
        let Some(job) = self.queue.borrow_mut().pop_front() else { return };
        let pdu = job.req.pdu();
        let transaction = self.transaction.get().wrapping_add(1);
        self.transaction.set(transaction);
        let raw = match self.cfg.transport {
            Transport::Rtu => modbus::rtu_frame(job.unit, &pdu),
            Transport::Tcp => modbus::tcp_frame(transaction, job.unit, &pdu),
        };
        if let Some(l) = self.link.borrow().as_ref() {
            l.write(0, raw.clone());
        }
        self.sent.set(self.sent.get() + 1);
        self.log.tx(&raw);
        let w = self.me();
        let timer = glib::timeout_add_local_once(Duration::from_millis(u64::from(self.cfg.timeout_ms.max(50))), move || {
            if let Some(m) = w.upgrade() {
                m.timed_out();
            }
        });
        self.busy.replace(Some(Pending { job, transaction, timer: Some(timer) }));
        self.show_stats();
    }

    fn timed_out(&self) {
        let Some(p) = self.busy.take() else { return };
        if let Some(Got::Junk(b)) = self.reader.borrow_mut().flush() {
            self.log.line("33", &format!("?? {}", modbus::hex(&b)));
        }
        self.log.line("31", &format!("timeout (unit {})", p.job.unit));
        if let Some(poll) = p.job.poll.and_then(|w| w.upgrade()) {
            poll.queued.set(false);
            poll.timeouts.set(poll.timeouts.get() + 1);
            poll.show_status("timeout");
        }
        self.show_stats();
        self.next();
    }

    /// The next request, after the RTU gap.
    fn next(&self) {
        if self.cfg.transport == Transport::Rtu {
            let w = self.me();
            glib::timeout_add_local_once(rtu_gap(self.cfg.serial.baud), move || {
                if let Some(m) = w.upgrade() {
                    m.pump();
                }
            });
        } else {
            self.pump();
        }
    }

    fn answer(&self, f: modbus::Frame) {
        self.log.rx(&f.raw);
        self.got.set(self.got.get() + 1);
        let matches = self.busy.borrow().as_ref().is_some_and(|p| match self.cfg.transport {
            Transport::Tcp => p.transaction == f.transaction,
            Transport::Rtu => p.job.unit == f.unit,
        });
        if !matches {
            self.log.line("33", "reply nobody asked for");
            return;
        }
        let Some(mut p) = self.busy.take() else { return };
        if let Some(id) = p.timer.take() {
            id.remove();
        }
        let result = p.job.req.parse_response(&f.pdu);
        match (&result, p.job.poll.as_ref().and_then(Weak::upgrade)) {
            (Ok(Response::Exception { code, .. }), poll) => {
                let text = format!("exception {code:02X}: {}", modbus::exception_text(*code));
                self.log.line("31", &text);
                if let Some(poll) = poll {
                    poll.errors.set(poll.errors.get() + 1);
                    poll.show_status(&text);
                }
            }
            (Err(e), poll) => {
                self.log.line("31", e);
                if let Some(poll) = poll {
                    poll.errors.set(poll.errors.get() + 1);
                    poll.show_status(e);
                }
            }
            (Ok(r), Some(poll)) => {
                let values = match r {
                    Response::Bits(b) => b.iter().map(|&b| u16::from(b)).collect(),
                    Response::Registers(v) => v.clone(),
                    _ => Vec::new(),
                };
                poll.ok.set(poll.ok.get() + 1);
                poll.values.replace(values);
                poll.refresh();
                poll.show_status("");
                if poll.plot.is_active() {
                    self.log.plot(&poll.plot_line());
                }
            }
            (Ok(_), None) => self.log.line("2", "written"),
        }
        if let Some(poll) = p.job.poll.and_then(|w| w.upgrade()) {
            poll.queued.set(false);
        }
        self.show_stats();
        self.next();
    }

    /// Writes `regs` (or one coil) at `addr` of the poll's table, ahead of
    /// the reads waiting.
    fn write(&self, unit: u8, table: Table, addr: u16, regs: Vec<u16>) {
        let req = match (table, regs.len()) {
            (Table::Coils, _) => Request::WriteCoil { addr, on: regs.first().copied().unwrap_or(0) != 0 },
            (Table::Holding, 1) => Request::WriteRegister { addr, value: regs[0] },
            (Table::Holding, _) => Request::WriteRegisters { addr, values: regs },
            _ => return,
        };
        self.queue.borrow_mut().push_front(Job { unit, req, poll: None });
        self.pump();
    }

    fn add_poll(&self, cfg: PollCfg) {
        let status = caption("");
        status.set_xalign(0.0);
        let grid = gtk::Grid::new();
        grid.set_column_spacing(24);
        grid.set_row_spacing(2);
        grid.set_margin_start(12);
        grid.set_margin_end(12);
        grid.set_margin_top(6);
        grid.set_margin_bottom(6);
        let plot = gtk::CheckButton::with_label("Plot");
        plot.set_tooltip_text(Some("Send these values to the pane's plot (one poll at a time)"));
        let p = Rc::new(Poll {
            cfg: RefCell::new(cfg.clone()),
            values: RefCell::default(),
            queued: Cell::new(false),
            timer: RefCell::new(None),
            ok: Cell::new(0),
            errors: Cell::new(0),
            timeouts: Cell::new(0),
            status: status.clone(),
            grid: grid.clone(),
            cells: RefCell::default(),
            plot: plot.clone(),
        });

        let unit = spin(1.0, 247.0, f64::from(cfg.unit), "Unit (slave) id");
        let tables = table_names();
        let tables: Vec<&str> = tables.iter().map(String::as_str).collect();
        let table = dropdown(&tables, "Table and function code", Table::ALL.iter().position(|t| *t == cfg.table).unwrap_or(2));
        let addr = spin(0.0, 65535.0, f64::from(cfg.addr), "Start address (0-based)");
        let count = spin(1.0, f64::from(cfg.table.max_read().min(MAX_ROWS)), f64::from(cfg.count), "How many");
        let rate = spin(20.0, 3_600_000.0, f64::from(cfg.rate_ms), "Scan rate in ms");
        let format = dropdown(&format_names(), "How values are shown and typed", Format::ALL.iter().position(|f| *f == cfg.format).unwrap_or(0));
        format.set_sensitive(!cfg.table.bits());
        let run = gtk::ToggleButton::with_label("Scan");
        run.set_active(cfg.on);
        run.set_tooltip_text(Some("Read at the scan rate"));
        // Two rows, so the controls fit next to a sidebar.
        let what = bar();
        for w in [caption("Unit").upcast_ref::<gtk::Widget>(), unit.upcast_ref(), table.upcast_ref(), caption("Address").upcast_ref(), addr.upcast_ref(), caption("Count").upcast_ref(), count.upcast_ref()] {
            what.append(w);
        }
        let how = bar();
        for w in [caption("Every").upcast_ref::<gtk::Widget>(), rate.upcast_ref(), caption("ms").upcast_ref(), format.upcast_ref(), plot.upcast_ref(), run.upcast_ref()] {
            how.append(w);
        }
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_vexpand(true);
        scroll.set_child(Some(&grid));
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page.append(&term::fit(&what));
        page.append(&term::fit(&how));
        status.set_margin_start(8);
        page.append(&status);
        page.append(&scroll);

        let title = gtk::Label::new(None);
        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("flat");
        close.set_tooltip_text(Some("Remove this poll"));
        let tab = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        tab.append(&title);
        tab.append(&close);
        self.tabs.append_page(&page, Some(&tab));
        self.polls.borrow_mut().push(p.clone());
        let set_title = {
            let (title, wp) = (title.clone(), Rc::downgrade(&p));
            move || {
                if let Some(p) = wp.upgrade() {
                    let c = p.cfg.borrow();
                    title.set_text(&format!("{} {}@{}", match c.table {
                        Table::Coils => "C",
                        Table::Discrete => "DI",
                        Table::Holding => "HR",
                        Table::Input => "IR",
                    }, c.addr, c.unit));
                }
            }
        };
        set_title();
        p.layout();
        self.schedule(&p);

        // Any change: store it, redraw the table, restart the timer.
        let changed: Rc<Edit> = {
            let (w, wp) = (self.me(), Rc::downgrade(&p));
            Rc::new(move |f: &dyn Fn(&mut PollCfg)| {
                let (Some(m), Some(p)) = (w.upgrade(), wp.upgrade()) else { return };
                f(&mut p.cfg.borrow_mut());
                p.values.borrow_mut().clear();
                p.layout();
                set_title();
                m.schedule(&p);
                m.remember();
            })
        };
        let ch = changed.clone();
        unit.connect_value_changed(move |s| ch(&|c| c.unit = s.value() as u8));
        let (ch, cnt, fmt) = (changed.clone(), count.clone(), format.clone());
        table.connect_selected_notify(move |d| {
            let t = Table::ALL[d.selected() as usize % 4];
            cnt.set_range(1.0, f64::from(t.max_read().min(MAX_ROWS)));
            fmt.set_sensitive(!t.bits());
            ch(&|c| {
                c.table = t;
                c.count = c.count.min(t.max_read().min(MAX_ROWS));
            })
        });
        let ch = changed.clone();
        addr.connect_value_changed(move |s| ch(&|c| c.addr = s.value() as u16));
        let ch = changed.clone();
        count.connect_value_changed(move |s| ch(&|c| c.count = s.value() as u16));
        let ch = changed.clone();
        rate.connect_value_changed(move |s| ch(&|c| c.rate_ms = s.value() as u32));
        let ch = changed.clone();
        format.connect_selected_notify(move |d| ch(&|c| c.format = Format::ALL[d.selected() as usize % Format::ALL.len()]));
        let ch = changed.clone();
        run.connect_toggled(move |b| ch(&|c| c.on = b.is_active()));
        // One poll feeds the plot at a time.
        let (w, wp) = (self.me(), Rc::downgrade(&p));
        plot.connect_toggled(move |b| {
            let (Some(m), Some(p)) = (w.upgrade(), wp.upgrade()) else { return };
            if !b.is_active() {
                return;
            }
            for o in m.polls.borrow().iter().filter(|o| !Rc::ptr_eq(o, &p)) {
                o.plot.set_active(false);
            }
            m.log.plot(&p.plot_names());
        });
        let (w, wp, pg) = (self.me(), Rc::downgrade(&p), page.clone());
        close.connect_clicked(move |_| {
            let (Some(m), Some(p)) = (w.upgrade(), wp.upgrade()) else { return };
            if m.polls.borrow().len() <= 1 {
                return;
            }
            if let Some(id) = p.timer.take() {
                id.remove();
            }
            m.polls.borrow_mut().retain(|o| !Rc::ptr_eq(o, &p));
            if let Some(n) = m.tabs.page_num(&pg) {
                m.tabs.remove_page(Some(n));
            }
            m.remember();
        });
        // Double-click a value to write it.
        let click = gtk::GestureClick::new();
        let (w, wp) = (self.me(), Rc::downgrade(&p));
        click.connect_pressed(move |g, n, x, y| {
            if n != 2 {
                return;
            }
            let (Some(m), Some(p)) = (w.upgrade(), wp.upgrade()) else { return };
            let Some(row) = p.row_at(x, y) else { return };
            m.edit(&p, row, g.widget().as_ref());
        });
        grid.add_controller(click);
    }

    /// Asks for a new value of `row` of `p` and writes it.
    fn edit(&self, p: &Rc<Poll>, row: usize, anchor: Option<&gtk::Widget>) {
        let c = p.cfg.borrow().clone();
        if !c.table.writable() {
            self.log.line("33", &format!("{} cannot be written", c.table.label()));
            return;
        }
        let words = if c.table.bits() { 1 } else { c.format.words() };
        let addr = c.addr.wrapping_add((row * words) as u16);
        if c.table == Table::Coils {
            let now = p.values.borrow().get(row).copied().unwrap_or(0);
            self.write(c.unit, c.table, addr, vec![u16::from(now == 0)]);
            return;
        }
        let Some(anchor) = anchor else { return };
        let entry = gtk::Entry::new();
        let regs: Vec<u16> = p.values.borrow().iter().skip(row * words).take(words).copied().collect();
        entry.set_text(&c.format.show(&regs).replace("–", ""));
        entry.set_width_chars(16);
        let pop = gtk::Popover::new();
        let b = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        b.append(&caption(&format!("{addr} =")));
        b.append(&entry);
        let go = gtk::Button::with_label("Write");
        go.add_css_class("suggested-action");
        b.append(&go);
        pop.set_child(Some(&b));
        pop.set_parent(anchor);
        if let Some(cell) = p.cells.borrow().get(row) {
            if let Some(r) = cell.compute_bounds(anchor) {
                pop.set_pointing_to(Some(&gtk::gdk::Rectangle::new(r.x() as i32, r.y() as i32, r.width() as i32, r.height() as i32)));
            }
        }
        pop.connect_closed(|p| {
            let p = p.clone();
            glib::idle_add_local_once(move || p.unparent());
        });
        let send = {
            let (w, e, pop) = (self.me(), entry.clone(), pop.clone());
            move || {
                let Some(m) = w.upgrade() else { return };
                match c.format.parse(&e.text()) {
                    Ok(regs) => {
                        m.write(c.unit, c.table, addr, regs);
                        pop.popdown();
                    }
                    Err(err) => {
                        e.add_css_class("error");
                        e.set_tooltip_text(Some(&err));
                    }
                }
            }
        };
        let s = send.clone();
        entry.connect_activate(move |_| s());
        go.connect_clicked(move |_| send());
        pop.popup();
        entry.grab_focus();
    }
}

impl Poll {
    fn rows(&self) -> usize {
        let c = self.cfg.borrow();
        if c.table.bits() { c.count as usize } else { (c.count as usize).div_ceil(c.format.words()) }
    }

    /// Builds the grid rows for the current settings.
    fn layout(&self) {
        while let Some(w) = self.grid.first_child() {
            self.grid.remove(&w);
        }
        let head = |text: &str, col: i32| {
            let l = caption(text);
            l.set_xalign(0.0);
            self.grid.attach(&l, col, 0, 1, 1);
        };
        head("Address", 0);
        head("Value", 1);
        let c = self.cfg.borrow().clone();
        let words = if c.table.bits() { 1 } else { c.format.words() };
        let mut cells = Vec::new();
        for row in 0..self.rows() {
            let a = gtk::Label::new(Some(&c.addr.wrapping_add((row * words) as u16).to_string()));
            a.set_xalign(0.0);
            a.add_css_class("numeric");
            a.add_css_class("dim-label");
            let v = gtk::Label::new(Some("–"));
            v.set_xalign(0.0);
            v.add_css_class("numeric");
            v.set_selectable(false);
            self.grid.attach(&a, 0, row as i32 + 1, 1, 1);
            self.grid.attach(&v, 1, row as i32 + 1, 1, 1);
            cells.push(v);
        }
        self.cells.replace(cells);
        let tip = if c.table.writable() { "Double-click a value to write it" } else { "Read only" };
        self.grid.set_tooltip_text(Some(tip));
    }

    fn refresh(&self) {
        let c = self.cfg.borrow().clone();
        let values = self.values.borrow();
        for (row, cell) in self.cells.borrow().iter().enumerate() {
            let text = if c.table.bits() {
                match values.get(row) {
                    Some(0) => "0  OFF".to_string(),
                    Some(_) => "1  ON".to_string(),
                    None => "–".into(),
                }
            } else {
                let w = c.format.words();
                let regs: Vec<u16> = values.iter().skip(row * w).take(w).copied().collect();
                c.format.show(&regs)
            };
            if cell.text() != text {
                cell.set_text(&text);
            }
        }
    }

    fn show_status(&self, last_error: &str) {
        let mut s = format!("ok {} · errors {} · timeouts {}", self.ok.get(), self.errors.get(), self.timeouts.get());
        if !last_error.is_empty() {
            s.push_str(&format!(" · last: {last_error}"));
        }
        self.status.set_text(&s);
    }

    /// Row of the grid at a point, if on a value row.
    fn row_at(&self, x: f64, y: f64) -> Option<usize> {
        let picked = self.grid.pick(x, y, gtk::PickFlags::DEFAULT)?;
        let cells = self.cells.borrow();
        let mut w = Some(picked);
        while let Some(cur) = w {
            if let Some(i) = cells.iter().position(|c| c.upcast_ref::<gtk::Widget>() == &cur) {
                return Some(i);
            }
            w = cur.parent();
        }
        None
    }

    /// Channel names for the plot: "HR40", "HR41", ...
    fn plot_names(&self) -> String {
        let c = self.cfg.borrow();
        let prefix = match c.table {
            Table::Coils => "C",
            Table::Discrete => "DI",
            Table::Holding => "HR",
            Table::Input => "IR",
        };
        let words = if c.table.bits() { 1 } else { c.format.words() };
        (0..self.rows()).map(|r| format!("{prefix}{}", c.addr as usize + r * words)).collect::<Vec<_>>().join(",") + "\n"
    }

    fn plot_line(&self) -> String {
        let c = self.cfg.borrow();
        let values = self.values.borrow();
        let words = if c.table.bits() { 1 } else { c.format.words() };
        (0..self.rows())
            .map(|r| {
                let regs: Vec<u16> = values.iter().skip(r * words).take(words).copied().collect();
                let n = if c.table.bits() { regs.first().map(|v| f64::from(*v)) } else { c.format.number(&regs) };
                n.map(|v| v.to_string()).unwrap_or_default()
            })
            .collect::<Vec<_>>()
            .join(",")
            + "\n"
    }
}

// ----------------------------------------------------------------- slave

struct View {
    table: Table,
    cfg: RefCell<ViewCfg>,
    grid: gtk::Grid,
    /// Value editors, one per row: entries for registers, checks for bits.
    cells: RefCell<Vec<gtk::Widget>>,
}

struct Slave {
    cfg: ModbusCfg,
    key: String,
    log: Log,
    link: RefCell<Option<Link>>,
    readers: RefCell<HashMap<u64, Reader>>,
    bank: RefCell<Bank>,
    views: RefCell<Vec<Rc<View>>>,
    dot: gtk::Label,
    stats: gtk::Label,
    requests: Cell<u64>,
    exceptions: Cell<u64>,
    /// Pending save of the values.
    save: RefCell<Option<glib::SourceId>>,
    me: RefCell<Weak<Slave>>,
}

fn table_key(t: Table) -> &'static str {
    match t {
        Table::Coils => "coils",
        Table::Discrete => "discrete",
        Table::Holding => "holding",
        Table::Input => "input",
    }
}

fn slave(t: &crate::vt::View, cfg: ModbusCfg, key: String, log: Log) -> gtk::Widget {
    let saved = load(&key);
    let mut bank = Bank::default();
    for (name, values) in &saved.values {
        for (&a, &v) in values {
            let a = a as usize;
            match name.as_str() {
                "coils" => bank.coils[a] = v != 0,
                "discrete" => bank.discrete[a] = v != 0,
                "holding" => bank.holding[a] = v,
                "input" => bank.input[a] = v,
                _ => {}
            }
        }
    }
    let who = if cfg.unit == 0 { "any unit".to_string() } else { format!("unit {}", cfg.unit) };
    let name = gtk::Label::new(Some(&format!("Slave {} · {who}", cfg.desc())));
    name.add_css_class("heading");
    let dot = gtk::Label::new(None);
    let stats = caption("");
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let again = gtk::Button::from_icon_name("view-refresh-symbolic");
    again.set_tooltip_text(Some("Restart"));
    let clear = gtk::Button::from_icon_name("edit-clear-symbolic");
    clear.set_tooltip_text(Some("Clear the log"));
    for b in [&again, &clear] {
        b.add_css_class("flat");
    }
    let top = bar();
    for w in [name.upcast_ref::<gtk::Widget>(), dot.upcast_ref(), stats.upcast_ref(), spacer.upcast_ref(), again.upcast_ref(), clear.upcast_ref()] {
        top.append(w);
    }
    let tabs = gtk::Notebook::new();
    tabs.set_vexpand(true);
    let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
    body.append(&term::fit(&top));
    body.append(&tabs);

    let s = Rc::new(Slave {
        cfg,
        key,
        log,
        link: RefCell::new(None),
        readers: RefCell::default(),
        bank: RefCell::new(bank),
        views: RefCell::default(),
        dot,
        stats,
        requests: Cell::new(0),
        exceptions: Cell::new(0),
        save: RefCell::new(None),
        me: RefCell::new(Weak::new()),
    });
    s.me.replace(Rc::downgrade(&s));
    for (i, table) in Table::ALL.into_iter().enumerate() {
        let vc = saved.views.get(i).cloned().unwrap_or_default();
        s.add_view(&tabs, table, vc);
    }
    let w = Rc::downgrade(&s);
    again.connect_clicked(move |_| {
        if let Some(s) = w.upgrade() {
            s.start();
        }
    });
    let tw = t.downgrade();
    clear.connect_clicked(move |_| {
        if let Some(t) = tw.upgrade() {
            t.reset(true, true);
        }
    });
    let ss = s.clone();
    t.connect_destroy(move |_| {
        ss.link.take();
        if let Some(id) = ss.save.take() {
            id.remove();
            ss.store_values();
        }
    });
    s.show_stats();
    s.start();
    body.upcast()
}

impl Slave {
    fn me(&self) -> Weak<Slave> {
        self.me.borrow().clone()
    }

    fn show_stats(&self) {
        self.stats.set_text(&format!("requests {} · exceptions {}", self.requests.get(), self.exceptions.get()));
    }

    fn start(&self) {
        self.link.take();
        self.readers.borrow_mut().clear();
        state_dot(&self.dot, "● starting", "tint-yellow");
        match mblink::open(&self.cfg) {
            Err(e) => {
                state_dot(&self.dot, "○ stopped", "tint-red");
                self.log.line("31", &e);
            }
            Ok((link, rx)) => {
                self.link.replace(Some(link));
                let w = self.me();
                glib::spawn_future_local(async move {
                    while let Ok(ev) = rx.recv().await {
                        let Some(s) = w.upgrade() else { break };
                        s.event(ev);
                    }
                });
                if self.cfg.transport == Transport::Rtu {
                    self.event(Ev::Up(format!("opened {}", self.cfg.desc())));
                }
            }
        }
    }

    fn event(&self, ev: Ev) {
        match ev {
            Ev::Up(note) => {
                state_dot(&self.dot, "● running", "tint-green");
                self.log.line("2", &note);
            }
            Ev::Note(note) => self.log.line("2", &note),
            Ev::Closed(why) => {
                self.link.take();
                state_dot(&self.dot, "○ stopped", "tint-red");
                self.log.line("31", &why);
            }
            Ev::Data(client, data) => {
                let tcp = self.cfg.transport == Transport::Tcp;
                let got = self.readers.borrow_mut().entry(client).or_insert_with(|| Reader::new(tcp, true)).feed(&data);
                for g in got {
                    match g {
                        Got::Frame(f) => self.request(client, f),
                        Got::Junk(b) => self.log.line("33", &format!("?? {}", modbus::hex(&b))),
                    }
                }
            }
        }
    }

    fn request(&self, client: u64, f: modbus::Frame) {
        let tcp = self.cfg.transport == Transport::Tcp;
        // RTU: answer our id; unit 0 is a broadcast write, done without a reply.
        let broadcast = !tcp && f.unit == 0;
        if !tcp && !broadcast && self.cfg.unit != 0 && f.unit != self.cfg.unit {
            return;
        }
        if tcp && self.cfg.unit != 0 && f.unit != self.cfg.unit && f.unit != 0xFF {
            return;
        }
        self.log.rx(&f.raw);
        self.requests.set(self.requests.get() + 1);
        let reply = self.bank.borrow_mut().handle(&f.pdu);
        if reply.first().is_some_and(|fc| fc & 0x80 != 0) {
            self.exceptions.set(self.exceptions.get() + 1);
            let code = reply.get(1).copied().unwrap_or(0);
            self.log.line("31", &format!("exception {code:02X}: {}", modbus::exception_text(code)));
        }
        let written = matches!(f.pdu.first(), Some(5 | 6 | 15 | 16));
        if !broadcast {
            let raw = if tcp { modbus::tcp_frame(f.transaction, f.unit, &reply) } else { modbus::rtu_frame(f.unit, &reply) };
            if let Some(l) = self.link.borrow().as_ref() {
                l.write(client, raw.clone());
            }
            self.log.tx(&raw);
        }
        if written {
            for v in self.views.borrow().iter() {
                if v.table.writable() {
                    self.fill(v);
                }
            }
            self.save_later();
        }
        self.show_stats();
    }

    /// Saves the values a second after the last change.
    fn save_later(&self) {
        if let Some(id) = self.save.take() {
            id.remove();
        }
        let w = self.me();
        self.save.replace(Some(glib::timeout_add_local_once(Duration::from_secs(1), move || {
            if let Some(s) = w.upgrade() {
                s.save.take();
                s.store_values();
            }
        })));
    }

    fn store_values(&self) {
        let bank = self.bank.borrow();
        let mut values = BTreeMap::new();
        for t in Table::ALL {
            let map: BTreeMap<u16, u16> = match t {
                Table::Coils => bank.coils.iter().enumerate().filter(|(_, v)| **v).map(|(a, _)| (a as u16, 1)).collect(),
                Table::Discrete => bank.discrete.iter().enumerate().filter(|(_, v)| **v).map(|(a, _)| (a as u16, 1)).collect(),
                Table::Holding => bank.holding.iter().enumerate().filter(|(_, v)| **v != 0).map(|(a, v)| (a as u16, *v)).collect(),
                Table::Input => bank.input.iter().enumerate().filter(|(_, v)| **v != 0).map(|(a, v)| (a as u16, *v)).collect(),
            };
            if !map.is_empty() {
                values.insert(table_key(t).to_string(), map);
            }
        }
        let mut s = load(&self.key);
        s.values = values;
        s.views = self.views.borrow().iter().map(|v| v.cfg.borrow().clone()).collect();
        store(&self.key, &s);
    }

    fn get(&self, t: Table, a: usize) -> u16 {
        let b = self.bank.borrow();
        match t {
            Table::Coils => u16::from(b.coils[a]),
            Table::Discrete => u16::from(b.discrete[a]),
            Table::Holding => b.holding[a],
            Table::Input => b.input[a],
        }
    }

    fn set(&self, t: Table, a: usize, v: u16) {
        let mut b = self.bank.borrow_mut();
        match t {
            Table::Coils => b.coils[a] = v != 0,
            Table::Discrete => b.discrete[a] = v != 0,
            Table::Holding => b.holding[a] = v,
            Table::Input => b.input[a] = v,
        }
    }

    fn add_view(&self, tabs: &gtk::Notebook, table: Table, vc: ViewCfg) {
        let grid = gtk::Grid::new();
        grid.set_column_spacing(24);
        grid.set_row_spacing(2);
        grid.set_margin_start(12);
        grid.set_margin_end(12);
        grid.set_margin_top(6);
        grid.set_margin_bottom(6);
        let v = Rc::new(View { table, cfg: RefCell::new(vc.clone()), grid: grid.clone(), cells: RefCell::default() });
        let addr = spin(0.0, 65535.0, f64::from(vc.addr), "First address shown");
        let count = spin(1.0, f64::from(MAX_ROWS), f64::from(vc.count), "How many shown");
        let format = dropdown(&format_names(), "How values are shown and typed", Format::ALL.iter().position(|f| *f == vc.format).unwrap_or(0));
        format.set_visible(!table.bits());
        let controls = bar();
        let hint = caption(if table.bits() { "Click to switch a bit" } else { "Type a value and press Enter" });
        for w in [caption("Address").upcast_ref::<gtk::Widget>(), addr.upcast_ref(), caption("Count").upcast_ref(), count.upcast_ref(), format.upcast_ref(), hint.upcast_ref()] {
            controls.append(w);
        }
        let scroll = gtk::ScrolledWindow::new();
        scroll.set_vexpand(true);
        scroll.set_child(Some(&grid));
        let page = gtk::Box::new(gtk::Orientation::Vertical, 0);
        page.append(&term::fit(&controls));
        page.append(&scroll);
        tabs.append_page(&page, Some(&gtk::Label::new(Some(table.label()))));
        self.views.borrow_mut().push(v.clone());
        self.layout(&v);

        let changed = {
            let (w, wv) = (self.me(), Rc::downgrade(&v));
            move |f: &dyn Fn(&mut ViewCfg)| {
                let (Some(s), Some(v)) = (w.upgrade(), wv.upgrade()) else { return };
                f(&mut v.cfg.borrow_mut());
                s.layout(&v);
                s.save_later();
            }
        };
        let ch = changed.clone();
        addr.connect_value_changed(move |sp| ch(&|c| c.addr = sp.value() as u16));
        let ch = changed.clone();
        count.connect_value_changed(move |sp| ch(&|c| c.count = sp.value() as u16));
        format.connect_selected_notify(move |d| changed(&|c| c.format = Format::ALL[d.selected() as usize % Format::ALL.len()]));
    }

    /// Builds the rows of `v`.
    fn layout(&self, v: &Rc<View>) {
        while let Some(w) = v.grid.first_child() {
            v.grid.remove(&w);
        }
        for (text, col) in [("Address", 0), ("Value", 1)] {
            let l = caption(text);
            l.set_xalign(0.0);
            v.grid.attach(&l, col, 0, 1, 1);
        }
        let c = v.cfg.borrow().clone();
        let words = if v.table.bits() { 1 } else { c.format.words() };
        let rows = (c.count as usize).div_ceil(words);
        let mut cells = Vec::new();
        for row in 0..rows {
            let a = c.addr as usize + row * words;
            if a + words > 65536 {
                break;
            }
            let label = gtk::Label::new(Some(&a.to_string()));
            label.set_xalign(0.0);
            label.add_css_class("numeric");
            label.add_css_class("dim-label");
            v.grid.attach(&label, 0, row as i32 + 1, 1, 1);
            let cell: gtk::Widget = if v.table.bits() {
                let check = gtk::CheckButton::new();
                let (w, t) = (self.me(), v.table);
                check.connect_toggled(move |b| {
                    if let Some(s) = w.upgrade() {
                        if s.get(t, a) != u16::from(b.is_active()) {
                            s.set(t, a, u16::from(b.is_active()));
                            s.save_later();
                        }
                    }
                });
                check.upcast()
            } else {
                let entry = gtk::Entry::new();
                entry.set_width_chars(14);
                entry.add_css_class("numeric");
                let (w, t, fmt) = (self.me(), v.table, c.format);
                entry.connect_activate(move |e| {
                    let Some(s) = w.upgrade() else { return };
                    match fmt.parse(&e.text()) {
                        Ok(regs) => {
                            for (i, r) in regs.iter().enumerate() {
                                if a + i < 65536 {
                                    s.set(t, a + i, *r);
                                }
                            }
                            e.remove_css_class("error");
                            e.set_tooltip_text(None);
                            // Show what is stored: "-5" as Unsigned is 65531.
                            let stored: Vec<u16> = (0..regs.len()).filter(|i| a + i < 65536).map(|i| s.get(t, a + i)).collect();
                            e.set_text(&fmt.show(&stored));
                            s.save_later();
                        }
                        Err(err) => {
                            e.add_css_class("error");
                            e.set_tooltip_text(Some(&err));
                        }
                    }
                });
                entry.upcast()
            };
            v.grid.attach(&cell, 1, row as i32 + 1, 1, 1);
            cells.push(cell);
        }
        v.cells.replace(cells);
        self.fill(v);
    }

    /// Shows the current values in the rows of `v` (not in a field being typed in).
    fn fill(&self, v: &View) {
        let c = v.cfg.borrow().clone();
        let words = if v.table.bits() { 1 } else { c.format.words() };
        for (row, cell) in v.cells.borrow().iter().enumerate() {
            let a = c.addr as usize + row * words;
            if let Some(check) = cell.downcast_ref::<gtk::CheckButton>() {
                let on = self.get(v.table, a) != 0;
                if check.is_active() != on {
                    check.set_active(on);
                }
            } else if let Some(entry) = cell.downcast_ref::<gtk::Entry>() {
                if entry.has_focus() || entry.text_length() > 0 && entry.has_css_class("error") {
                    continue;
                }
                let regs: Vec<u16> = (0..words).filter(|i| a + i < 65536).map(|i| self.get(v.table, a + i)).collect();
                let text = c.format.show(&regs);
                if entry.text() != text {
                    entry.set_text(&text);
                }
            }
        }
    }
}
