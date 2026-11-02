// Serial pane: a toolbar over a terminal fed from a serial port.
//
// One pane owns a port. Opening the same port again gives a read-only
// mirror that sees the owner's incoming data.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::path::Path;
use std::rc::{Rc, Weak};
use std::time::Duration;

use gtk::{glib, prelude::*};
use vte::prelude::*;

use crate::serial::{self, Conn, Eol, Event, Hex, SerialCfg};
use crate::state::State;
use crate::term::{self, Launch};

struct View {
    term: glib::WeakRef<vte::Terminal>,
    cfg: SerialCfg,
    conn: RefCell<Option<Conn>>,
    hex: RefCell<Hex>,
    hex_on: Cell<bool>,
    echo_on: Cell<bool>,
    eol: Cell<Eol>,
    mirror: bool,
    dot: gtk::Label,
    retry: RefCell<Option<glib::SourceId>>,
}

/// Owner and mirrors per device path.
#[derive(Default)]
struct Port {
    owner: Weak<View>,
    mirrors: Vec<Weak<View>>,
}

thread_local! {
    static PORTS: RefCell<HashMap<String, Port>> = RefCell::new(HashMap::new());
}

impl View {
    fn show(&self, data: &[u8]) {
        let Some(t) = self.term.upgrade() else { return };
        if self.hex_on.get() {
            t.feed(self.hex.borrow_mut().feed(data).as_bytes());
        } else {
            t.feed(data);
        }
    }

    /// Colored status line inside the terminal (`sgr` like "33" for yellow).
    fn note(&self, sgr: &str, msg: &str) {
        self.hex.borrow_mut().reset();
        if let Some(t) = self.term.upgrade() {
            t.feed(format!("\r\n\x1b[{sgr}m[{msg}]\x1b[0m\r\n").as_bytes());
        }
    }

    fn set_up(&self, up: bool) {
        let (text, add, del) = match (self.mirror, up) {
            (true, _) => ("● mirror", "tint-cyan", "tint-red"),
            (false, true) => ("● connected", "tint-green", "tint-red"),
            (false, false) => ("○ disconnected", "tint-red", "tint-green"),
        };
        self.dot.set_text(text);
        self.dot.remove_css_class(del);
        self.dot.add_css_class(add);
    }
}

fn mirrors(device: &str) -> Vec<Rc<View>> {
    PORTS.with(|p| {
        p.borrow()
            .get(device)
            .map(|x| x.mirrors.iter().filter_map(Weak::upgrade).collect())
            .unwrap_or_default()
    })
}

fn connect(v: &Rc<View>) {
    if v.conn.borrow().is_some() {
        return;
    }
    match serial::open(&v.cfg) {
        Ok((conn, rx)) => {
            v.conn.replace(Some(conn));
            v.set_up(true);
            v.note("32", &format!("connected {} {}", v.cfg.device, v.cfg.summary()));
            let weak = Rc::downgrade(v);
            let device = v.cfg.device.clone();
            glib::spawn_future_local(async move {
                while let Ok(ev) = rx.recv().await {
                    let Some(v) = weak.upgrade() else { break };
                    match ev {
                        Event::Data(d) => {
                            v.show(&d);
                            for m in mirrors(&device) {
                                m.show(&d);
                            }
                        }
                        Event::Closed(why) => {
                            v.conn.take();
                            v.set_up(false);
                            v.note("33", &format!("disconnected: {why}"));
                            if v.cfg.reconnect {
                                retry(&v);
                            }
                            break;
                        }
                    }
                }
            });
        }
        Err(e) => {
            v.set_up(false);
            v.note("31", &e);
            // Permission problems do not fix themselves; anything else may.
            if v.cfg.reconnect && !e.contains("permission denied") {
                retry(v);
            }
        }
    }
}

/// Polls every 500 ms until the device is back, then reconnects.
fn retry(v: &Rc<View>) {
    if v.retry.borrow().is_some() {
        return;
    }
    v.note("33", "waiting for device, reconnects automatically");
    let weak = Rc::downgrade(v);
    let id = glib::timeout_add_local(Duration::from_millis(500), move || {
        let Some(v) = weak.upgrade() else {
            return glib::ControlFlow::Break;
        };
        if !Path::new(&v.cfg.device).exists() {
            return glib::ControlFlow::Continue;
        }
        // The source ends here; connect() may schedule a new one.
        v.retry.take();
        connect(&v);
        glib::ControlFlow::Break
    });
    v.retry.replace(Some(id));
}

/// Closes every port owned by a pane in this process. Panes stay open and
/// do not auto-reconnect; the reconnect button reopens them. Returns the
/// number of ports closed.
pub fn release_all() -> usize {
    let owners: Vec<Rc<View>> = PORTS.with(|p| p.borrow().values().filter_map(|x| x.owner.upgrade()).collect());
    let mut n = 0;
    for v in owners {
        if let Some(id) = v.retry.take() {
            id.remove();
        }
        if v.conn.take().is_some() {
            n += 1;
            v.set_up(false);
            v.note("33", "released; press the reconnect button to reopen");
        }
    }
    n
}

/// Number of ports open in this process.
pub fn open_count() -> usize {
    PORTS.with(|p| p.borrow().values().filter_map(|x| x.owner.upgrade()).filter(|v| v.conn.borrow().is_some()).count())
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

/// Builds the pane. Returns the terminal and the leaf widget.
pub fn build(state: &Rc<State>, launch: &Launch, cfg: SerialCfg) -> (vte::Terminal, gtk::Widget) {
    let t = term::spawn(state, launch);
    let device = cfg.device.clone();
    let mirror = PORTS.with(|p| p.borrow().get(&device).and_then(|x| x.owner.upgrade()).is_some());

    let dot = gtk::Label::new(None);
    let v = Rc::new(View {
        term: t.downgrade(),
        hex_on: Cell::new(cfg.hex),
        echo_on: Cell::new(cfg.echo),
        eol: Cell::new(cfg.eol),
        cfg,
        conn: RefCell::new(None),
        hex: RefCell::new(Hex::default()),
        mirror,
        dot: dot.clone(),
        retry: RefCell::new(None),
    });

    // Toolbar.
    let name = gtk::Label::new(Some(&format!("{}  {}", v.cfg.device, v.cfg.summary())));
    name.add_css_class("heading");
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let hex = toggle("Hex", "Show incoming bytes as hex", v.hex_on.get());
    let echo = toggle("Echo", "Local echo of typed text", v.echo_on.get());
    let eol = gtk::DropDown::from_strings(&Eol::ALL.map(Eol::label));
    eol.set_tooltip_text(Some("Bytes sent for Enter"));
    eol.set_selected(Eol::ALL.iter().position(|e| *e == v.eol.get()).unwrap_or(0) as u32);
    let dtr = toggle("DTR", "DTR line level", true);
    let rts = toggle("RTS", "RTS line level", true);
    let brk = gtk::Button::with_label("Break");
    brk.add_css_class("flat");
    brk.set_tooltip_text(Some("Send a 250 ms break"));
    let again = button("view-refresh-symbolic", "Reconnect");
    let clear = button("edit-clear-symbolic", "Clear screen and scrollback");

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    bar.add_css_class("serialbar");
    for w in [
        name.upcast_ref::<gtk::Widget>(),
        dot.upcast_ref(),
        spacer.upcast_ref(),
        hex.upcast_ref(),
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
    if mirror {
        for w in [echo.upcast_ref::<gtk::Widget>(), eol.upcast_ref(), dtr.upcast_ref(), rts.upcast_ref(), brk.upcast_ref(), again.upcast_ref()] {
            w.set_visible(false);
        }
    }

    let vv = v.clone();
    hex.connect_toggled(move |b| {
        vv.hex_on.set(b.is_active());
        vv.hex.borrow_mut().reset();
    });
    let vv = v.clone();
    echo.connect_toggled(move |b| vv.echo_on.set(b.is_active()));
    let vv = v.clone();
    eol.connect_selected_notify(move |d| {
        if let Some(e) = Eol::ALL.get(d.selected() as usize) {
            vv.eol.set(*e);
        }
    });
    let vv = v.clone();
    dtr.connect_toggled(move |b| {
        if let Some(c) = vv.conn.borrow().as_ref() {
            c.dtr(b.is_active());
        }
    });
    let vv = v.clone();
    rts.connect_toggled(move |b| {
        if let Some(c) = vv.conn.borrow().as_ref() {
            c.rts(b.is_active());
        }
    });
    let vv = v.clone();
    brk.connect_clicked(move |_| {
        if let Some(c) = vv.conn.borrow().as_ref() {
            c.send_break();
        }
    });
    let vv = v.clone();
    again.connect_clicked(move |_| {
        vv.conn.take();
        if let Some(id) = vv.retry.take() {
            id.remove();
        }
        connect(&vv);
    });
    let vv = v.clone();
    clear.connect_clicked(move |_| {
        if let Some(t) = vv.term.upgrade() {
            t.reset(true, true);
        }
        vv.hex.borrow_mut().reset();
    });

    // Typed text goes to the port.
    let vv = v.clone();
    t.connect_commit(move |_, text, _| {
        if vv.mirror {
            return;
        }
        if let Some(c) = vv.conn.borrow().as_ref() {
            c.write(serial::outgoing(text, vv.eol.get()));
            if vv.echo_on.get() {
                if let Some(t) = vv.term.upgrade() {
                    t.feed(serial::echo(text).as_bytes());
                }
            }
        }
    });

    // Closing the pane releases the port and tells the mirrors.
    let vv = v.clone();
    t.connect_destroy(move |_| {
        vv.conn.take();
        if let Some(id) = vv.retry.take() {
            id.remove();
        }
        let orphans = PORTS.with(|p| {
            let mut p = p.borrow_mut();
            let Some(port) = p.get_mut(&vv.cfg.device) else { return Vec::new() };
            if vv.mirror {
                port.mirrors.retain(|m| m.upgrade().is_some_and(|m| !Rc::ptr_eq(&m, &vv)));
                return Vec::new();
            }
            let left: Vec<Rc<View>> = port.mirrors.iter().filter_map(Weak::upgrade).collect();
            p.remove(&vv.cfg.device);
            left
        });
        for m in orphans {
            m.note("33", "owner pane closed; mirror stopped");
        }
    });

    PORTS.with(|p| {
        let mut p = p.borrow_mut();
        let port = p.entry(device.clone()).or_default();
        if mirror {
            port.mirrors.push(Rc::downgrade(&v));
        } else {
            port.owner = Rc::downgrade(&v);
        }
    });

    if mirror {
        v.set_up(true);
        v.note("36", &format!("read-only mirror of {device}; type in the owner pane"));
    } else {
        connect(&v);
    }

    let leaf = gtk::Box::new(gtk::Orientation::Vertical, 0);
    leaf.append(&bar);
    leaf.append(&term::scroll(&t));
    (t, leaf.upcast())
}
