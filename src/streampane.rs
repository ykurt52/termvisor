// Stream pane: serial, raw TCP and telnet sessions.
//
// Layout: toolbar, terminal, send bar. Incoming bytes are shown as text,
// hex dump or decoded MAVLink. A serial port is owned by one pane; opening
// it again gives a read-only mirror of the owner's incoming data.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::{Rc, Weak};
use std::time::Duration;

use gtk::{gdk, glib, prelude::*};
use vte::prelude::*;

use crate::link::{self, Eol, Event, Hex, View};
use crate::mav::Decoder;
use crate::state::State;
use crate::term::{self, Launch, Stream};
use crate::{net, serial};

/// An open transport.
enum Link {
    Serial(serial::Conn),
    Net(net::Conn),
}

impl Link {
    fn write(&self, data: Vec<u8>) {
        match self {
            Link::Serial(c) => c.write(data),
            Link::Net(c) => c.write(data),
        }
    }

    fn send_break(&self) {
        match self {
            Link::Serial(c) => c.send_break(),
            Link::Net(c) => c.send_break(),
        }
    }

    fn dtr(&self, on: bool) {
        if let Link::Serial(c) = self {
            c.dtr(on);
        }
    }

    fn rts(&self, on: bool) {
        if let Link::Serial(c) = self {
            c.rts(on);
        }
    }
}

fn open_link(src: &Stream) -> Result<(Link, async_channel::Receiver<Event>), String> {
    match src {
        Stream::Serial(c) => serial::open(c).map(|(c, rx)| (Link::Serial(c), rx)),
        Stream::Tcp(c) => net::open(c, false).map(|(c, rx)| (Link::Net(c), rx)),
        Stream::Telnet(c) => net::open(c, true).map(|(c, rx)| (Link::Net(c), rx)),
    }
}

struct Pane {
    term: glib::WeakRef<vte::Terminal>,
    src: Stream,
    link: RefCell<Option<Link>>,
    hex: RefCell<Hex>,
    mav: RefCell<Decoder>,
    view: Cell<View>,
    echo_on: Cell<bool>,
    eol: Cell<Eol>,
    mirror: bool,
    dot: gtk::Label,
    stats: gtk::Label,
    retry: RefCell<Option<glib::SourceId>>,
    /// Last status note, to avoid repeating the same line on every retry.
    last_note: RefCell<String>,
}

/// Owner and mirrors per serial device path.
#[derive(Default)]
struct Port {
    owner: Weak<Pane>,
    mirrors: Vec<Weak<Pane>>,
}

thread_local! {
    static PORTS: RefCell<HashMap<String, Port>> = RefCell::new(HashMap::new());
}

fn device(src: &Stream) -> Option<&str> {
    match src {
        Stream::Serial(c) => Some(&c.device),
        _ => None,
    }
}

impl Pane {
    fn feed(&self, s: &[u8]) {
        if let Some(t) = self.term.upgrade() {
            t.feed(s);
        }
    }

    fn show(&self, data: &[u8]) {
        match self.view.get() {
            View::Text => self.feed(data),
            View::Hex => self.feed(self.hex.borrow_mut().feed(data).as_bytes()),
            View::Mavlink => {
                let text = self.mav.borrow_mut().feed(data);
                self.feed(text.as_bytes());
                let m = self.mav.borrow();
                self.stats.set_text(&format!("{} frames · {} bad", m.frames, m.bad_crc));
            }
        }
    }

    /// Colored status line inside the terminal (`sgr` like "33" for yellow).
    fn note(&self, sgr: &str, msg: &str) {
        if *self.last_note.borrow() == msg {
            return;
        }
        self.last_note.replace(msg.to_string());
        self.hex.borrow_mut().reset();
        self.feed(format!("\r\n\x1b[{sgr}m[{msg}]\x1b[0m\r\n").as_bytes());
    }

    fn set_state(&self, text: &str, tint: &str) {
        self.dot.set_text(text);
        for c in ["tint-green", "tint-red", "tint-yellow", "tint-cyan"] {
            self.dot.remove_css_class(c);
        }
        self.dot.add_css_class(tint);
    }

    fn set_up(&self, up: bool) {
        match (self.mirror, up) {
            (true, _) => self.set_state("● mirror", "tint-cyan"),
            (false, true) => self.set_state("● connected", "tint-green"),
            (false, false) => self.set_state("○ disconnected", "tint-red"),
        }
    }

    fn set_view(&self, v: View) {
        self.view.set(v);
        self.hex.borrow_mut().reset();
        self.mav.borrow_mut().reset();
        self.stats.set_visible(v == View::Mavlink);
        self.feed(b"\r\n");
    }

    /// Sends bytes and echoes them when local echo is on.
    fn send(&self, bytes: Vec<u8>, shown: &str) {
        if self.mirror {
            return;
        }
        let sent = match self.link.borrow().as_ref() {
            Some(l) => {
                l.write(bytes);
                true
            }
            None => false,
        };
        if !sent {
            self.note("33", "not connected");
            return;
        }
        if self.echo_on.get() {
            self.feed(shown.as_bytes());
        }
    }
}

fn mirrors(src: &Stream) -> Vec<Rc<Pane>> {
    let Some(dev) = device(src) else { return Vec::new() };
    PORTS.with(|p| {
        p.borrow()
            .get(dev)
            .map(|x| x.mirrors.iter().filter_map(Weak::upgrade).collect())
            .unwrap_or_default()
    })
}

fn connect(p: &Rc<Pane>) {
    if p.link.borrow().is_some() {
        return;
    }
    match open_link(&p.src) {
        Ok((link, rx)) => {
            let serial = matches!(link, Link::Serial(_));
            p.link.replace(Some(link));
            if serial {
                p.set_up(true);
                p.note("32", &format!("connected {}", p.src.desc()));
            } else {
                p.set_state("◌ connecting", "tint-yellow");
            }
            let weak = Rc::downgrade(p);
            glib::spawn_future_local(async move {
                while let Ok(ev) = rx.recv().await {
                    let Some(p) = weak.upgrade() else { break };
                    match ev {
                        Event::Up(msg) => {
                            p.set_up(true);
                            p.note("32", &msg);
                        }
                        Event::Data(d) => {
                            p.show(&d);
                            for m in mirrors(&p.src) {
                                m.show(&d);
                            }
                        }
                        Event::Closed(why) => {
                            p.link.take();
                            p.set_up(false);
                            p.note("33", &format!("disconnected: {why}"));
                            if p.src.opts().reconnect {
                                retry(&p);
                            }
                            break;
                        }
                    }
                }
            });
        }
        Err(e) => {
            p.set_up(false);
            p.note("31", &e);
            // Permission problems do not fix themselves; anything else may.
            if p.src.opts().reconnect && !e.contains("permission denied") {
                retry(p);
            }
        }
    }
}

/// Serial: poll every 500 ms until the device is back. Network: try again
/// every 3 s.
fn retry(p: &Rc<Pane>) {
    if p.retry.borrow().is_some() {
        return;
    }
    let (every, wait_dev) = match &p.src {
        Stream::Serial(_) => (Duration::from_millis(500), true),
        _ => (Duration::from_secs(3), false),
    };
    let weak = Rc::downgrade(p);
    let id = glib::timeout_add_local(every, move || {
        let Some(p) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if wait_dev && !device(&p.src).is_some_and(|d| Path::new(d).exists()) {
            return glib::ControlFlow::Continue;
        }
        // The source ends here; connect() may schedule a new one.
        p.retry.take();
        connect(&p);
        glib::ControlFlow::Break
    });
    p.retry.replace(Some(id));
}

/// Closes every serial port owned by a pane in this process. Panes stay
/// open and do not auto-reconnect. Returns the number of ports closed.
pub fn release_all() -> usize {
    let owners: Vec<Rc<Pane>> = PORTS.with(|p| p.borrow().values().filter_map(|x| x.owner.upgrade()).collect());
    let mut n = 0;
    for p in owners {
        if let Some(id) = p.retry.take() {
            id.remove();
        }
        if p.link.take().is_some() {
            n += 1;
            p.set_up(false);
            p.note("33", "released; press the reconnect button to reopen");
        }
    }
    n
}

/// Number of serial ports open in this process.
pub fn open_count() -> usize {
    PORTS.with(|p| {
        p.borrow()
            .values()
            .filter_map(|x| x.owner.upgrade())
            .filter(|p| p.link.borrow().is_some())
            .count()
    })
}

fn toggle(label: &str, tip: &str, on: bool) -> gtk::ToggleButton {
    let b = gtk::ToggleButton::with_label(label);
    b.set_tooltip_text(Some(tip));
    b.set_active(on);
    b.add_css_class("flat");
    b
}

fn button(icon: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.set_tooltip_text(Some(tip));
    b.add_css_class("flat");
    b
}

fn dropdown(items: &[&str], tip: &str, selected: usize) -> gtk::DropDown {
    let d = gtk::DropDown::from_strings(items);
    d.set_tooltip_text(Some(tip));
    d.set_selected(selected as u32);
    d
}

/// Send bar: ASCII with escapes or hex bytes, with input history.
fn send_bar(p: &Rc<Pane>) -> gtk::Box {
    let entry = gtk::Entry::new();
    entry.set_hexpand(true);
    let mode = dropdown(&["ASCII", "Hex"], "How the input is read", 0);
    let add_eol = gtk::CheckButton::with_label("+ Enter");
    add_eol.set_tooltip_text(Some("Append the Enter bytes (ASCII mode)"));
    add_eol.set_active(true);
    let send = gtk::Button::with_label("Send");
    send.add_css_class("suggested-action");

    let hint = |hex: bool| {
        if hex {
            "Hex bytes: 55 AA 01, 55AA01 or 0x55,0xAA"
        } else {
            "Text, escapes: \\r \\n \\t \\0 \\\\ \\xHH"
        }
    };
    entry.set_placeholder_text(Some(hint(false)));
    let (e, a) = (entry.clone(), add_eol.clone());
    mode.connect_selected_notify(move |m| {
        let hex = m.selected() == 1;
        e.set_placeholder_text(Some(hint(hex)));
        a.set_sensitive(!hex);
    });

    let history: Rc<RefCell<Vec<String>>> = Rc::default();
    let pos: Rc<Cell<usize>> = Rc::default();

    let fire = {
        let (p, entry, mode, add_eol, history, pos) =
            (p.clone(), entry.clone(), mode.clone(), add_eol.clone(), history.clone(), pos.clone());
        move || {
            let text = entry.text().to_string();
            let hex = mode.selected() == 1;
            let parsed = if hex { link::parse_hex(&text) } else { link::unescape(&text) };
            let mut bytes = match parsed {
                Ok(b) => b,
                Err(e) => {
                    p.note("31", &format!("send: {e}"));
                    return;
                }
            };
            if !hex && add_eol.is_active() {
                bytes.extend_from_slice(p.eol.get().bytes());
            }
            if bytes.is_empty() {
                return;
            }
            let shown = if hex {
                let h: Vec<String> = bytes.iter().map(|b| format!("{b:02X}")).collect();
                format!("\x1b[2m» {}\x1b[0m\r\n", h.join(" "))
            } else {
                link::echo(&String::from_utf8_lossy(&bytes))
            };
            p.send(bytes, &shown);
            let mut h = history.borrow_mut();
            if h.last() != Some(&text) {
                h.push(text);
                if h.len() > 50 {
                    h.remove(0);
                }
            }
            pos.set(h.len());
            entry.set_text("");
        }
    };
    let f = fire.clone();
    entry.connect_activate(move |_| f());
    send.connect_clicked(move |_| fire());

    // Up/Down walk the history.
    let keys = gtk::EventControllerKey::new();
    let e = entry.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        let h = history.borrow();
        let next = match key {
            gdk::Key::Up if pos.get() > 0 => pos.get() - 1,
            gdk::Key::Down if pos.get() < h.len() => pos.get() + 1,
            gdk::Key::Up | gdk::Key::Down => return glib::Propagation::Stop,
            _ => return glib::Propagation::Proceed,
        };
        pos.set(next);
        e.set_text(h.get(next).map(String::as_str).unwrap_or(""));
        e.set_position(-1);
        glib::Propagation::Stop
    });
    entry.add_controller(keys);

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    bar.add_css_class("sendbar");
    bar.append(&mode);
    bar.append(&entry);
    bar.append(&add_eol);
    bar.append(&send);
    bar
}

/// Builds the pane. Returns the terminal and the leaf widget.
pub fn build(state: &Rc<State>, launch: &Launch, src: Stream) -> (vte::Terminal, gtk::Widget) {
    let t = term::spawn(state, launch);
    let serial = matches!(src, Stream::Serial(_));
    let mirror = device(&src)
        .is_some_and(|d| PORTS.with(|p| p.borrow().get(d).and_then(|x| x.owner.upgrade()).is_some()));

    let opts = src.opts().clone();
    let dot = gtk::Label::new(None);
    let stats = gtk::Label::new(None);
    stats.add_css_class("dim-label");
    let p = Rc::new(Pane {
        term: t.downgrade(),
        view: Cell::new(opts.view()),
        echo_on: Cell::new(opts.echo),
        eol: Cell::new(opts.eol),
        src,
        link: RefCell::new(None),
        hex: RefCell::new(Hex::default()),
        mav: RefCell::new(Decoder::default()),
        mirror,
        dot: dot.clone(),
        stats: stats.clone(),
        retry: RefCell::new(None),
        last_note: RefCell::new(String::new()),
    });
    stats.set_visible(p.view.get() == View::Mavlink);

    // Toolbar.
    let name = gtk::Label::new(Some(&p.src.desc()));
    name.add_css_class("heading");
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let views: Vec<&str> = View::ALL.iter().map(|v| v.label()).collect();
    let view = dropdown(&views, "How incoming bytes are shown", View::ALL.iter().position(|v| *v == p.view.get()).unwrap_or(0));
    let filter = gtk::SearchEntry::new();
    filter.set_placeholder_text(Some("Filter: heartbeat, attitude"));
    filter.set_width_chars(22);
    filter.set_visible(p.view.get() == View::Mavlink);
    let echo = toggle("Echo", "Local echo of typed text", p.echo_on.get());
    let eols: Vec<&str> = Eol::ALL.iter().map(|e| e.label()).collect();
    let eol = dropdown(&eols, "Bytes sent for Enter", Eol::ALL.iter().position(|e| *e == p.eol.get()).unwrap_or(0));
    let dtr = toggle("DTR", "DTR line level", true);
    let rts = toggle("RTS", "RTS line level", true);
    let brk = gtk::Button::with_label("Break");
    brk.add_css_class("flat");
    brk.set_tooltip_text(Some(if serial { "Send a 250 ms break" } else { "Send telnet BREAK" }));
    let again = button("view-refresh-symbolic", "Reconnect");
    let clear = button("edit-clear-symbolic", "Clear screen and scrollback");

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    bar.add_css_class("serialbar");
    for w in [
        name.upcast_ref::<gtk::Widget>(),
        dot.upcast_ref(),
        stats.upcast_ref(),
        spacer.upcast_ref(),
        filter.upcast_ref(),
        view.upcast_ref(),
        echo.upcast_ref(),
        eol.upcast_ref(),
        dtr.upcast_ref(),
        rts.upcast_ref(),
        brk.upcast_ref(),
        again.upcast_ref(),
        clear.upcast_ref(),
    ] {
        bar.append(w);
    }
    dtr.set_visible(serial && !mirror);
    rts.set_visible(serial && !mirror);
    brk.set_visible(!mirror && !matches!(p.src, Stream::Tcp(_)));
    for w in [echo.upcast_ref::<gtk::Widget>(), eol.upcast_ref(), again.upcast_ref()] {
        w.set_visible(!mirror);
    }

    let pp = p.clone();
    let f = filter.clone();
    view.connect_selected_notify(move |d| {
        let v = View::ALL.get(d.selected() as usize).copied().unwrap_or_default();
        f.set_visible(v == View::Mavlink);
        pp.set_view(v);
    });
    let pp = p.clone();
    filter.connect_search_changed(move |e| pp.mav.borrow_mut().set_filter(&e.text()));
    let pp = p.clone();
    echo.connect_toggled(move |b| pp.echo_on.set(b.is_active()));
    let pp = p.clone();
    eol.connect_selected_notify(move |d| {
        if let Some(e) = Eol::ALL.get(d.selected() as usize) {
            pp.eol.set(*e);
        }
    });
    let pp = p.clone();
    dtr.connect_toggled(move |b| {
        if let Some(l) = pp.link.borrow().as_ref() {
            l.dtr(b.is_active());
        }
    });
    let pp = p.clone();
    rts.connect_toggled(move |b| {
        if let Some(l) = pp.link.borrow().as_ref() {
            l.rts(b.is_active());
        }
    });
    let pp = p.clone();
    brk.connect_clicked(move |_| {
        if let Some(l) = pp.link.borrow().as_ref() {
            l.send_break();
        }
    });
    let pp = p.clone();
    again.connect_clicked(move |_| {
        pp.link.take();
        if let Some(id) = pp.retry.take() {
            id.remove();
        }
        pp.last_note.replace(String::new());
        connect(&pp);
    });
    let pp = p.clone();
    clear.connect_clicked(move |_| {
        if let Some(t) = pp.term.upgrade() {
            t.reset(true, true);
        }
        pp.hex.borrow_mut().reset();
        pp.mav.borrow_mut().reset();
    });

    // Typed text goes to the link.
    let pp = p.clone();
    t.connect_commit(move |_, text, _| {
        pp.send(link::outgoing(text, pp.eol.get()), &link::echo(text));
    });

    // Closing the pane drops the link and tells the mirrors.
    let pp = p.clone();
    t.connect_destroy(move |_| {
        pp.link.take();
        if let Some(id) = pp.retry.take() {
            id.remove();
        }
        let Some(dev) = device(&pp.src).map(str::to_string) else { return };
        let orphans = PORTS.with(|ports| {
            let mut ports = ports.borrow_mut();
            let Some(port) = ports.get_mut(&dev) else { return Vec::new() };
            if pp.mirror {
                port.mirrors.retain(|m| m.upgrade().is_some_and(|m| !Rc::ptr_eq(&m, &pp)));
                return Vec::new();
            }
            let left: Vec<Rc<Pane>> = port.mirrors.iter().filter_map(Weak::upgrade).collect();
            ports.remove(&dev);
            left
        });
        for m in orphans {
            m.note("33", "owner pane closed; mirror stopped");
        }
    });

    if let Some(dev) = device(&p.src).map(str::to_string) {
        PORTS.with(|ports| {
            let mut ports = ports.borrow_mut();
            let port = ports.entry(dev.clone()).or_default();
            if mirror {
                port.mirrors.push(Rc::downgrade(&p));
            } else {
                port.owner = Rc::downgrade(&p);
            }
        });
    }

    if mirror {
        p.set_up(true);
        p.note("36", &format!("read-only mirror of {}; type in the owner pane", p.src.desc()));
    } else {
        connect(&p);
    }

    let leaf = gtk::Box::new(gtk::Orientation::Vertical, 0);
    leaf.append(&bar);
    leaf.append(&term::scroll(&t));
    if !mirror {
        leaf.append(&send_bar(&p));
    }
    (t, leaf.upcast())
}
