// USB capture view: transfers from usbmon (or a pcap file), decoded, with
// a hex view and a serial view for USB serial adapters. The last events
// are kept for re-filtering and pcap export.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gio, glib};

use crate::output::Out;
use crate::state::State;
use crate::term::{self, Launch, UsbLaunch};
use crate::usb::{self, Batch, Device, Event, Xfer};
use crate::usbdecode::{self, Decoder};

/// Events kept for re-filtering and export.
const KEEP: usize = 100_000;
const VIEWS: [&str; 3] = ["Transfers", "Transfers + data", "Serial"];

#[derive(Clone, Copy, PartialEq)]
enum ViewMode {
    Transfers,
    Data,
    Serial,
}

impl ViewMode {
    fn from(i: u32) -> Self {
        match i {
            1 => ViewMode::Data,
            2 => ViewMode::Serial,
            _ => ViewMode::Transfers,
        }
    }
}

struct Pane {
    term: glib::WeakRef<crate::vt::View>,
    out: Rc<RefCell<Out>>,
    launch: UsbLaunch,
    events: RefCell<VecDeque<Event>>,
    devices: RefCell<HashMap<(u16, u8), Device>>,
    devices_at: Cell<Option<Instant>>,
    decoder: RefCell<Decoder>,
    mode: Cell<ViewMode>,
    /// Show submissions without data and empty completions too.
    all: Cell<bool>,
    find: RefCell<String>,
    paused: Cell<bool>,
    dropped: Cell<u32>,
    /// Direction of the last serial bytes shown, to color the next ones.
    last_dir: Cell<Option<bool>>,
    count: gtk::Label,
    dot: gtk::Label,
    redraw: RefCell<Option<glib::SourceId>>,
    capture: RefCell<Option<usb::Capture>>,
}

fn time(e: &Event) -> String {
    match glib::DateTime::from_unix_local(e.sec) {
        Ok(t) => format!("{}.{:06}", t.format("%H:%M:%S").map(|s| s.to_string()).unwrap_or_default(), e.usec),
        Err(_) => format!("{}.{:06}", e.sec, e.usec),
    }
}

/// Hex dump lines, 16 bytes each, indented.
fn dump(data: &[u8]) -> String {
    let mut s = String::new();
    for (i, chunk) in data.chunks(16).enumerate().take(256) {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String = chunk.iter().map(|&b| if (0x20..0x7f).contains(&b) { b as char } else { '.' }).collect();
        s.push_str(&format!("\x1b[2m        {:04x}\x1b[0m  {:<48} \x1b[2m|\x1b[0m{ascii}\x1b[2m|\x1b[0m\r\n", i * 16, hex.join(" ")));
    }
    if data.len() > 4096 {
        s.push_str(&format!("\x1b[2m        … {} more bytes\x1b[0m\r\n", data.len() - 4096));
    }
    s
}

impl Pane {
    fn device(&self, bus: u16, dev: u8) -> Option<Device> {
        if let Some(d) = self.devices.borrow().get(&(bus, dev)) {
            return Some(d.clone());
        }
        // A new device (e.g. just plugged in): read sysfs again, once a second.
        let stale = self.devices_at.get().is_none_or(|t| t.elapsed() > Duration::from_secs(1));
        if stale && self.launch.file.is_none() {
            self.devices.replace(usb::devices().into_iter().map(|d| ((d.bus, d.dev), d)).collect());
            self.devices_at.set(Some(Instant::now()));
            return self.devices.borrow().get(&(bus, dev)).cloned();
        }
        None
    }

    fn wanted(&self, e: &Event) -> bool {
        let u = &self.launch;
        (u.bus == 0 || e.bus == u.bus) && u.dev.is_none_or(|d| d == e.dev)
    }

    /// Screen text of one event, or empty when it is filtered out.
    fn render(&self, e: &Event, text: &str) -> String {
        let mode = self.mode.get();
        if mode == ViewMode::Serial {
            let dev = self.device(e.bus, e.dev);
            // Without device information every bulk payload counts as serial data.
            let fallback = || (dev.is_none() && e.xfer == Xfer::Bulk && !e.data.is_empty()).then(|| (e.is_in(), e.data.clone()));
            let Some((inbound, bytes)) = usbdecode::serial_bytes(e, dev.as_ref()).or_else(fallback) else { return String::new() };
            let mut s = String::new();
            if self.last_dir.replace(Some(inbound)) != Some(inbound) {
                s.push_str(if inbound { "\x1b[0;32m" } else { "\x1b[0;33m" });
            }
            s.push_str(&String::from_utf8_lossy(&bytes));
            return s;
        }
        let quiet = e.status_text().is_empty() && text.is_empty() && e.setup.is_none() && e.data.is_empty();
        if !self.all.get() && quiet {
            return String::new();
        }
        let find = self.find.borrow();
        let ep = if e.ep_num() == 0 { "ep 0  ".to_string() } else { format!("ep {:02x} ", e.ep) };
        let dir = if e.is_in() { "IN " } else { "OUT" };
        let line_plain = format!("{} {}:{} {ep}{dir} {} {} {}", time(e), e.bus, e.dev, e.xfer.short(), e.kind as char, text);
        if !find.is_empty() && !line_plain.to_lowercase().contains(find.as_str()) {
            return String::new();
        }
        let color = match (e.status_text(), e.xfer, e.is_in()) {
            (s, _, _) if !s.is_empty() => "31",
            (_, Xfer::Control, _) => "35",
            (_, _, true) => "36",
            _ => "33",
        };
        let status = match e.status_text() {
            "" => String::new(),
            s => format!(" \x1b[1;31m{s}\x1b[0m"),
        };
        let len = if e.kind == b'S' && e.is_in() { format!("{:>6}", format!("<{}", e.len_urb)) } else { format!("{:>6}", e.len_urb) };
        let mut s = format!(
            "\x1b[2m{}\x1b[0m {:>3}:{:<3} \x1b[{color}m{ep}{dir} {:<4} {}\x1b[0m {len} B{status}  {text}\r\n",
            time(e),
            e.bus,
            e.dev,
            e.xfer.short(),
            e.kind as char,
        );
        if mode == ViewMode::Data && !e.data.is_empty() {
            s.push_str(&dump(&e.data));
        }
        s
    }

    fn describe(&self, e: &Event) -> String {
        let dev = self.device(e.bus, e.dev);
        self.decoder.borrow_mut().describe(e, dev.as_ref())
    }

    fn count(&self) {
        let n = self.events.borrow().len();
        let dropped = match self.dropped.get() {
            0 => String::new(),
            d => format!(" · {d} dropped"),
        };
        let paused = if self.paused.get() { " · paused" } else { "" };
        self.count.set_text(&format!("{n} events{dropped}{paused}"));
    }

    fn take(&self, batch: Vec<Event>) {
        let mut shown = String::new();
        for e in batch.into_iter().filter(|e| self.wanted(e)) {
            let text = self.describe(&e);
            if !self.paused.get() {
                shown.push_str(&self.render(&e, &text));
            }
            let mut evs = self.events.borrow_mut();
            evs.push_back(e);
            if evs.len() > KEEP {
                evs.pop_front();
            }
        }
        self.show(&shown);
        self.count();
    }

    fn show(&self, text: &str) {
        if text.is_empty() {
            return;
        }
        if let Some(t) = self.term.upgrade() {
            let data = self.out.borrow_mut().process(text.as_bytes()).into_owned();
            t.feed(&data);
        }
    }

    fn redraw(&self) {
        let Some(t) = self.term.upgrade() else { return };
        t.reset(true, true);
        self.decoder.borrow_mut().reset();
        self.last_dir.set(None);
        let evs: Vec<Event> = self.events.borrow().iter().cloned().collect();
        let mut text = String::new();
        for e in &evs {
            let d = self.describe(e);
            text.push_str(&self.render(e, &d));
        }
        // A redraw is not new output: no log, only screen processing.
        let data = self.out.borrow_mut().filter.apply(text.as_bytes()).into_owned();
        t.feed(&data);
        self.count();
    }

    fn later(self: &Rc<Self>) {
        if let Some(id) = self.redraw.take() {
            id.remove();
        }
        let w = Rc::downgrade(self);
        let id = glib::timeout_add_local_once(Duration::from_millis(200), move || {
            if let Some(p) = w.upgrade() {
                p.redraw.take();
                p.redraw();
            }
        });
        self.redraw.replace(Some(id));
    }

    fn set_state(&self, text: &str, tint: &str) {
        self.dot.set_text(text);
        for c in ["tint-green", "tint-red", "tint-yellow", "tint-cyan"] {
            self.dot.remove_css_class(c);
        }
        self.dot.add_css_class(tint);
    }

    fn note(&self, sgr: &str, msg: &str) {
        if let Some(t) = self.term.upgrade() {
            t.feed(format!("\x1b[{sgr}m{msg}\x1b[0m\r\n").as_bytes());
        }
    }
}

fn start(p: &Rc<Pane>) {
    if let Some(file) = p.launch.file.clone() {
        match usb::load_pcap(std::path::Path::new(&file)) {
            Ok(evs) => {
                p.set_state("● file", "tint-cyan");
                p.take(evs);
            }
            Err(e) => {
                p.set_state("○ error", "tint-red");
                p.note("31", &format!("{file}: {e}"));
            }
        }
        return;
    }
    if let Some(why) = usb::blocked() {
        p.set_state("○ not enabled", "tint-red");
        p.note("33", &format!("{why} Open the USB page in the side panel and press \"Enable USB Capture…\" once."));
        return;
    }
    let (tx, rx) = async_channel::bounded(64);
    match usb::capture(p.launch.bus, tx) {
        Ok(c) => {
            p.capture.replace(Some(c));
        }
        Err(e) => {
            p.set_state("○ error", "tint-red");
            p.note("31", &format!("{}: {e}", usb::monitor_path(p.launch.bus).display()));
            return;
        }
    }
    p.set_state("● capturing", "tint-green");
    let w = Rc::downgrade(p);
    glib::spawn_future_local(async move {
        while let Ok(b) = rx.recv().await {
            let Some(p) = w.upgrade() else { break };
            match b {
                Batch::Events(evs) => p.take(evs),
                Batch::Dropped(n) => {
                    p.dropped.set(n);
                    p.count();
                }
                Batch::Error(e) => {
                    p.set_state("○ stopped", "tint-red");
                    p.note("31", &format!("capture stopped: {e}"));
                }
            }
        }
    });
}

fn button(icon: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.add_css_class("flat");
    b.set_tooltip_text(Some(tip));
    b
}

pub fn build(state: &Rc<State>, launch: &Launch) -> (crate::vt::View, gtk::Widget) {
    let t = term::spawn(state, launch);
    let out = crate::output::of(&t).unwrap_or_else(|| crate::output::attach(&t, &launch.title, &state.cfg.borrow()));
    let u = launch.usb.clone().unwrap_or_default();

    let name = gtk::Label::new(Some(&launch.title));
    name.add_css_class("heading");
    let dot = gtk::Label::new(None);
    let count = gtk::Label::new(None);
    count.add_css_class("dim-label");
    let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    spacer.set_hexpand(true);
    let view = gtk::DropDown::from_strings(&VIEWS);
    view.set_tooltip_text(Some("Transfers, transfers with a hex dump, or the text of USB serial adapters"));
    view.set_selected(if u.serial { 2 } else { 0 });
    let all = gtk::ToggleButton::with_label("All events");
    all.set_tooltip_text(Some("Also show submissions without data and empty completions"));
    let find = gtk::Entry::new();
    find.set_placeholder_text(Some("Find"));
    find.set_width_chars(16);
    let pause = gtk::ToggleButton::with_label("Pause");
    let save = button("document-save-symbolic", "Save the kept events as pcap (opens in Wireshark)");
    let clear = button("edit-clear-symbolic", "Clear the screen and kept events");

    let bar = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    bar.add_css_class("serialbar");
    for w in [
        name.upcast_ref::<gtk::Widget>(),
        dot.upcast_ref(),
        count.upcast_ref(),
        spacer.upcast_ref(),
        view.upcast_ref(),
        all.upcast_ref(),
        find.upcast_ref(),
        pause.upcast_ref(),
        save.upcast_ref(),
        clear.upcast_ref(),
    ] {
        bar.append(w);
    }

    let p = Rc::new(Pane {
        term: t.downgrade(),
        out,
        launch: u.clone(),
        events: RefCell::new(VecDeque::new()),
        // A file may come from another machine: its addresses mean nothing here.
        devices: RefCell::new(if u.file.is_some() { HashMap::new() } else { usb::devices().into_iter().map(|d| ((d.bus, d.dev), d)).collect() }),
        devices_at: Cell::new(Some(Instant::now())),
        decoder: RefCell::new(Decoder::default()),
        mode: Cell::new(if u.serial { ViewMode::Serial } else { ViewMode::Transfers }),
        all: Cell::new(false),
        find: RefCell::new(String::new()),
        paused: Cell::new(false),
        dropped: Cell::new(0),
        last_dir: Cell::new(None),
        count,
        dot,
        redraw: RefCell::new(None),
        capture: RefCell::new(None),
    });
    p.count();

    let w = Rc::downgrade(&p);
    view.connect_selected_notify(move |d| {
        if let Some(p) = w.upgrade() {
            p.mode.set(ViewMode::from(d.selected()));
            p.later();
        }
    });
    let w = Rc::downgrade(&p);
    all.connect_toggled(move |b| {
        if let Some(p) = w.upgrade() {
            p.all.set(b.is_active());
            p.later();
        }
    });
    let w = Rc::downgrade(&p);
    find.connect_changed(move |e| {
        if let Some(p) = w.upgrade() {
            p.find.replace(e.text().trim().to_lowercase());
            p.later();
        }
    });
    let w = Rc::downgrade(&p);
    pause.connect_toggled(move |b| {
        if let Some(p) = w.upgrade() {
            p.paused.set(b.is_active());
            if b.is_active() {
                p.count();
            } else {
                p.redraw();
            }
        }
    });
    let w = Rc::downgrade(&p);
    clear.connect_clicked(move |_| {
        if let Some(p) = w.upgrade() {
            p.events.borrow_mut().clear();
            p.redraw();
        }
    });
    let w = Rc::downgrade(&p);
    save.connect_clicked(move |b| {
        let Some(p) = w.upgrade() else { return };
        let dlg = gtk::FileDialog::new();
        dlg.set_initial_name(Some("capture.pcap"));
        let win = b.root().and_downcast::<gtk::Window>();
        let pw = Rc::downgrade(&p);
        dlg.save(win.as_ref(), None::<&gio::Cancellable>, move |res| {
            let (Some(path), Some(p)) = (res.ok().and_then(|f| f.path()), pw.upgrade()) else { return };
            let evs: Vec<Event> = p.events.borrow().iter().cloned().collect();
            match usb::save_pcap(&path, &evs) {
                Ok(()) => p.note("2", &format!("[saved {} events to {}]", evs.len(), path.display())),
                Err(e) => p.note("31", &format!("[save {}: {e}]", path.display())),
            }
        });
    });

    start(&p);

    // Closing the pane stops the capture.
    let pp = p.clone();
    t.connect_destroy(move |_| {
        pp.capture.take();
    });

    let leaf = gtk::Box::new(gtk::Orientation::Vertical, 0);
    leaf.append(&term::fit(&bar));
    leaf.append(&term::scroll(&t));
    (t, leaf.upcast())
}

#[cfg(test)]
mod tests {
    use super::dump;

    #[test]
    fn hex_dump() {
        let s = dump(b"hello, USB world!").replace("\x1b[2m", "").replace("\x1b[0m", "");
        assert!(s.starts_with("        0000  68 65 6c 6c 6f"));
        assert!(s.contains("|hello, USB world|"));
        assert!(s.contains("        0010  21"));
    }
}
