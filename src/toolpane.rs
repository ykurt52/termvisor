// Tool tabs: TFTP and HTTP servers, port scanner, ping with a graph and
// traceroute. Each has its controls above a terminal that shows its log.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};
use vte::prelude::*;

use crate::nettools::{self, Ping, Scan, ServerPrefs, Stats, Trace};
use crate::output::Out;
use crate::state::State;
use crate::term::{self, Launch, Tool};
use crate::tunnels::human;
use crate::{httpd, tftp};

/// Ping samples kept for the graph.
const SAMPLES: usize = 120;

/// Writes log lines to the tab's terminal (and its log file).
#[derive(Clone)]
struct Feed {
    term: glib::WeakRef<vte::Terminal>,
    out: Rc<RefCell<Out>>,
}

impl Feed {
    fn line(&self, sgr: &str, text: &str) {
        let Some(t) = self.term.upgrade() else { return };
        let s = if sgr.is_empty() { format!("{text}\r\n") } else { format!("\x1b[{sgr}m{text}\x1b[0m\r\n") };
        let data = self.out.borrow_mut().process(s.as_bytes()).into_owned();
        t.feed(&data);
    }
}

fn entry(hint: &str, width: i32) -> gtk::Entry {
    let e = gtk::Entry::new();
    e.set_placeholder_text(Some(hint));
    e.update_property(&[gtk::accessible::Property::Label(hint)]);
    e.set_width_chars(width);
    e
}

fn label(text: &str) -> gtk::Label {
    let l = gtk::Label::new(Some(text));
    l.add_css_class("dim-label");
    l
}

fn bar(widgets: &[&gtk::Widget]) -> gtk::Box {
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 4);
    b.add_css_class("serialbar");
    for w in widgets {
        b.append(*w);
    }
    b
}

fn spacer() -> gtk::Box {
    let s = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    s.set_hexpand(true);
    s
}

fn start_button() -> gtk::ToggleButton {
    let b = gtk::ToggleButton::with_label("Start");
    b.add_css_class("suggested-action");
    b.connect_toggled(|b| {
        b.set_label(if b.is_active() { "Stop" } else { "Start" });
        if b.is_active() {
            b.remove_css_class("suggested-action");
            b.add_css_class("destructive-action");
        } else {
            b.remove_css_class("destructive-action");
            b.add_css_class("suggested-action");
        }
    });
    b
}

/// Sets a toggle without running its handler's effect twice.
fn reset(b: &gtk::ToggleButton, busy: &Cell<bool>) {
    busy.set(true);
    b.set_active(false);
    busy.set(false);
}

pub fn build(state: &Rc<State>, launch: &Launch) -> (vte::Terminal, gtk::Widget) {
    let t = term::spawn(state, launch);
    let out = crate::output::of(&t).unwrap_or_else(|| crate::output::attach(&t, &launch.title, &state.cfg.borrow()));
    let feed = Feed { term: t.downgrade(), out };
    let tool = launch.tool.unwrap_or(Tool::Ping);
    let top: Vec<gtk::Widget> = match tool {
        Tool::Tftp | Tool::Http => vec![server(tool, &feed).upcast()],
        Tool::Scan => vec![scanner(&feed).upcast()],
        Tool::Ping => pinger(&feed),
        Tool::Trace => vec![tracer(&feed).upcast()],
    };
    let leaf = gtk::Box::new(gtk::Orientation::Vertical, 0);
    for w in &top {
        leaf.append(w);
    }
    leaf.append(&term::scroll(&t));
    (t, leaf.upcast())
}

enum Running {
    Tftp(#[allow(dead_code)] tftp::Server),
    Http(#[allow(dead_code)] httpd::Server),
}

fn server(tool: Tool, feed: &Feed) -> gtk::Box {
    let tftp_tab = tool == Tool::Tftp;
    let tools = nettools::load_tools();
    let prefs = if tftp_tab { tools.tftp.clone() } else { tools.http.clone() }.unwrap_or_else(|| ServerPrefs::new(if tftp_tab { 69 } else { 8000 }));
    let dir = Rc::new(RefCell::new(PathBuf::from(&prefs.dir)));

    let folder = gtk::Button::with_label(&prefs.dir);
    folder.set_tooltip_text(Some("Folder to serve"));
    let addr = entry("Address", 12);
    addr.set_text(&prefs.addr);
    addr.set_tooltip_text(Some("Listen address; 0.0.0.0 is every interface"));
    let port = gtk::SpinButton::with_range(1.0, 65535.0, 1.0);
    port.set_value(f64::from(prefs.port));
    let upload = gtk::CheckButton::with_label("Allow uploads");
    upload.set_active(prefs.upload);
    let allow = gtk::Button::with_label("Allow Low Ports…");
    allow.set_tooltip_text(Some("One time, as administrator: let termvisor listen on ports below 1024 (setcap cap_net_bind_service)"));
    allow.set_visible(false);
    let go = start_button();
    let b = bar(&[
        label(if tftp_tab { "TFTP" } else { "HTTP" }).upcast_ref(),
        folder.upcast_ref(),
        spacer().upcast_ref(),
        addr.upcast_ref(),
        label("port").upcast_ref(),
        port.upcast_ref(),
        upload.upcast_ref(),
        allow.upcast_ref(),
        go.upcast_ref(),
    ]);

    let (d, f) = (dir.clone(), folder.clone());
    folder.connect_clicked(move |btn| {
        let dlg = gtk::FileDialog::new();
        dlg.set_initial_folder(Some(&gio::File::for_path(&*d.borrow())));
        let (d, f) = (d.clone(), f.clone());
        dlg.select_folder(btn.root().and_downcast::<gtk::Window>().as_ref(), None::<&gio::Cancellable>, move |res| {
            if let Some(p) = res.ok().and_then(|g| g.path()) {
                f.set_label(&p.to_string_lossy());
                d.replace(p);
            }
        });
    });

    let fd = feed.clone();
    allow.connect_clicked(move |b| {
        let Ok(exe) = std::env::current_exe() else { return };
        b.set_sensitive(false);
        let (b, fd) = (b.clone(), fd.clone());
        glib::spawn_future_local(async move {
            let shown = exe.display().to_string();
            let res = gio::spawn_blocking(move || std::process::Command::new("pkexec").args(["setcap", "cap_net_bind_service=+ep"]).arg(exe).output()).await;
            b.set_sensitive(true);
            match res {
                Ok(Ok(o)) if o.status.success() => fd.line("32", &format!("Allowed for {shown}. Restart termvisor, then start the server on port 69.")),
                Ok(Ok(o)) if o.status.code() == Some(126) => fd.line("33", "Cancelled."),
                Ok(Ok(o)) => fd.line("31", &format!("setcap failed: {}", String::from_utf8_lossy(&o.stderr).trim())),
                _ => fd.line("31", "pkexec could not run"),
            }
        });
    });

    let running: Rc<RefCell<Option<Running>>> = Rc::default();
    let busy = Rc::new(Cell::new(false));
    let (fd, run, bz) = (feed.clone(), running.clone(), busy.clone());
    let (addr2, port2, up2, allow2) = (addr.clone(), port.clone(), upload.clone(), allow.clone());
    go.connect_toggled(move |b| {
        if bz.get() {
            return;
        }
        if !b.is_active() {
            if run.take().is_some() {
                fd.line("33", "stopped");
            }
            return;
        }
        let ip: IpAddr = match addr2.text().trim().parse() {
            Ok(ip) => ip,
            Err(_) => {
                fd.line("31", &format!("'{}' is not an IP address", addr2.text()));
                return reset(b, &bz);
            }
        };
        let port = port2.value() as u16;
        let root = dir.borrow().clone();
        let mut tools = nettools::load_tools();
        let prefs = ServerPrefs { dir: root.to_string_lossy().into_owned(), addr: ip.to_string(), port, upload: up2.is_active() };
        if tftp_tab {
            tools.tftp = Some(prefs.clone());
        } else {
            tools.http = Some(prefs.clone());
        }
        if let Err(e) = nettools::save_tools(&tools) {
            fd.line("33", &format!("save settings: {e}"));
        }
        let at = SocketAddr::new(ip, port);
        let res = if tftp_tab {
            let (tx, rx) = async_channel::unbounded();
            tftp::serve(tftp::Cfg { root: root.clone(), addr: at, upload: prefs.upload }, tx).map(|(s, _)| {
                show_tftp(fd.clone(), rx, port);
                Running::Tftp(s)
            })
        } else {
            let (tx, rx) = async_channel::unbounded();
            httpd::serve(httpd::Cfg { root: root.clone(), addr: at, upload: prefs.upload }, tx).map(|(s, _)| {
                show_http(fd.clone(), rx);
                Running::Http(s)
            })
        };
        match res {
            Ok(r) => {
                run.replace(Some(r));
                fd.line("2", &format!("serving {} ({})", root.display(), if prefs.upload { "downloads and uploads" } else { "downloads only" }));
            }
            Err(e) if e.kind() == io::ErrorKind::PermissionDenied && port < 1024 => {
                fd.line("31", &format!("Port {port} needs permission."));
                fd.line("", if tftp_tab {
                    "  Use port 6969 and tell U-Boot: setenv tftpdstp 6969, or press \"Allow Low Ports…\" once."
                } else {
                    "  Use a port above 1023 (e.g. 8000), or press \"Allow Low Ports…\" once."
                });
                allow2.set_visible(true);
                reset(b, &bz);
            }
            Err(e) => {
                fd.line("31", &format!("{at}: {e}"));
                reset(b, &bz);
            }
        }
    });
    b
}

/// Addresses to tell clients: the listen address, or every local one.
fn shown_ips(addr: SocketAddr) -> Vec<IpAddr> {
    if addr.ip().is_unspecified() { nettools::local_ips() } else { vec![addr.ip()] }
}

fn rate(bytes: u64, secs: f64) -> String {
    if secs <= 0.0 { String::new() } else { format!(" ({}/s)", human((bytes as f64 / secs) as u64)) }
}

fn show_tftp(fd: Feed, rx: async_channel::Receiver<tftp::Log>, port: u16) {
    glib::spawn_future_local(async move {
        while let Ok(ev) = rx.recv().await {
            match ev {
                tftp::Log::Listening(a) => {
                    fd.line("32", &format!("listening on {a}"));
                    for ip in shown_ips(a).into_iter().filter(|ip| !ip.is_loopback()) {
                        let dst = if port == 69 { String::new() } else { format!("setenv tftpdstp {port}; ") };
                        fd.line("2", &format!("  U-Boot: {dst}setenv serverip {ip}; tftpboot ${{loadaddr}} <file>"));
                    }
                }
                tftp::Log::Start { peer, file, upload, size, block } => {
                    let arrow = if upload { "↑ receiving" } else { "↓ sending" };
                    let size = size.map(|s| format!(", {}", human(s))).unwrap_or_default();
                    fd.line("36", &format!("{} {arrow} {file}{size}, {block} B blocks", peer.ip()));
                }
                tftp::Log::Done { peer, file, bytes, secs } => {
                    fd.line("32", &format!("{} ✓ {file} {} in {secs:.2} s{}", peer.ip(), human(bytes), rate(bytes, secs)));
                }
                tftp::Log::Fail { peer, file, why } => fd.line("31", &format!("{} ✗ {file}: {why}", peer.ip())),
            }
        }
    });
}

fn show_http(fd: Feed, rx: async_channel::Receiver<httpd::Log>) {
    glib::spawn_future_local(async move {
        while let Ok(ev) = rx.recv().await {
            match ev {
                httpd::Log::Listening(a) => {
                    fd.line("32", &format!("listening on {a}"));
                    for ip in shown_ips(a) {
                        fd.line("2", &format!("  http://{ip}:{}/", a.port()));
                    }
                }
                httpd::Log::Request { peer, method, path, status, bytes } => {
                    let sgr = match status {
                        200..=299 => "",
                        300..=399 => "2",
                        _ => "31",
                    };
                    fd.line(sgr, &format!("{} {method} {path} {status} {}", peer.ip(), human(bytes)));
                }
            }
        }
    });
}

fn scanner(feed: &Feed) -> gtk::Box {
    let hosts = entry("192.168.1.0/24, 10.0.0.5-20 or a host", 22);
    let ports = entry("common, or 22,80,8000-8100", 18);
    let timeout = gtk::SpinButton::with_range(50.0, 5000.0, 50.0);
    timeout.set_value(400.0);
    timeout.set_tooltip_text(Some("Connect timeout (ms)"));
    let grab = gtk::CheckButton::with_label("Banners");
    grab.set_tooltip_text(Some("Read what an open port says first (SSH, FTP, SMTP versions)"));
    let progress = label("");
    let go = start_button();
    let b = bar(&[
        hosts.upcast_ref(),
        ports.upcast_ref(),
        label("timeout ms").upcast_ref(),
        timeout.upcast_ref(),
        grab.upcast_ref(),
        spacer().upcast_ref(),
        progress.upcast_ref(),
        go.upcast_ref(),
    ]);
    let stop: Rc<RefCell<Arc<AtomicBool>>> = Rc::default();
    let busy = Rc::new(Cell::new(false));
    let (fd, st, bz, prog) = (feed.clone(), stop.clone(), busy.clone(), progress.clone());
    go.connect_toggled(move |b| {
        if bz.get() {
            return;
        }
        if !b.is_active() {
            st.borrow().store(true, Ordering::Release);
            return;
        }
        let parsed = nettools::hosts(&hosts.text()).and_then(|h| nettools::ports(&ports.text()).map(|p| (h, p)));
        let (h, p) = match parsed {
            Ok(x) => x,
            Err(e) => {
                fd.line("31", &e);
                return reset(b, &bz);
            }
        };
        fd.line("2", &format!("scanning {} host(s) × {} port(s)", h.len(), p.len()));
        let flag = Arc::new(AtomicBool::new(false));
        st.replace(flag.clone());
        let (tx, rx) = async_channel::unbounded();
        nettools::scan(h, p, Duration::from_millis(timeout.value() as u64), grab.is_active(), flag, tx);
        let (fd, b, bz, prog) = (fd.clone(), b.clone(), bz.clone(), prog.clone());
        glib::spawn_future_local(async move {
            while let Ok(ev) = rx.recv().await {
                match ev {
                    Scan::Open { addr, banner } => {
                        let svc = nettools::service(addr.port());
                        fd.line("32", &format!("{:<22} open  {:<12} {banner}", addr.to_string(), svc));
                    }
                    Scan::Progress { done, total } => prog.set_text(&format!("{done}/{total}")),
                    Scan::Done { open, total, secs } => {
                        prog.set_text("");
                        fd.line("2", &format!("{open} open of {total} checked in {secs:.1} s"));
                        reset(&b, &bz);
                        break;
                    }
                }
            }
        });
    });
    b
}

fn pinger(feed: &Feed) -> Vec<gtk::Widget> {
    let host = entry("Host or IP", 22);
    let every = gtk::DropDown::from_strings(&["0.2 s", "0.5 s", "1 s", "2 s"]);
    every.set_selected(2);
    every.set_tooltip_text(Some("Interval (below 0.2 s needs root)"));
    let stats = label("");
    let go = start_button();
    let b = bar(&[host.upcast_ref(), every.upcast_ref(), spacer().upcast_ref(), stats.upcast_ref(), go.upcast_ref()]);

    let samples: Rc<RefCell<VecDeque<Option<f64>>>> = Rc::default();
    let graph = gtk::DrawingArea::new();
    graph.set_content_height(110);
    graph.add_css_class("pinggraph");
    let s = samples.clone();
    graph.set_draw_func(move |a, cr, w, h| draw(a, cr, w, h, &s.borrow()));

    let stop: Rc<RefCell<Arc<AtomicBool>>> = Rc::default();
    let busy = Rc::new(Cell::new(false));
    let (fd, st, bz) = (feed.clone(), stop.clone(), busy.clone());
    let graph2 = graph.clone();
    go.connect_toggled(move |b| {
        if bz.get() {
            return;
        }
        if !b.is_active() {
            st.borrow().store(true, Ordering::Release);
            return;
        }
        let target = host.text().trim().to_string();
        if target.is_empty() || target.starts_with('-') {
            fd.line("31", "enter a host");
            return reset(b, &bz);
        }
        let interval = [0.2, 0.5, 1.0, 2.0][every.selected().min(3) as usize];
        let flag = Arc::new(AtomicBool::new(false));
        st.replace(flag.clone());
        samples.borrow_mut().clear();
        let (tx, rx) = async_channel::unbounded();
        if let Err(e) = nettools::ping(&target, interval, flag, tx) {
            fd.line("31", &e);
            return reset(b, &bz);
        }
        let (fd, b, bz, samples, graph, stats) = (fd.clone(), b.clone(), bz.clone(), samples.clone(), graph2.clone(), stats.clone());
        glib::spawn_future_local(async move {
            let mut st = Stats::default();
            while let Ok(ev) = rx.recv().await {
                let sample = match &ev {
                    Ping::Reply { ms, .. } => {
                        st.reply(*ms);
                        Some(Some(*ms))
                    }
                    Ping::Lost { .. } => {
                        st.lost();
                        Some(None)
                    }
                    _ => None,
                };
                match ev {
                    Ping::Reply { seq, ttl, ms } => fd.line("", &format!("seq {seq:<5} ttl {ttl:<4} {ms:.2} ms")),
                    Ping::Lost { seq } => fd.line("31", &format!("seq {seq:<5} no reply")),
                    Ping::Text(t) if !t.trim().is_empty() => fd.line("2", &t),
                    Ping::Text(_) => {}
                    Ping::Ended => {
                        reset(&b, &bz);
                        break;
                    }
                }
                if let Some(x) = sample {
                    let mut s = samples.borrow_mut();
                    s.push_back(x);
                    if s.len() > SAMPLES {
                        s.pop_front();
                    }
                    graph.queue_draw();
                    stats.set_text(&format!(
                        "sent {} · loss {:.0}% · min {:.2} · avg {:.2} · max {:.2} · jitter {:.2} ms",
                        st.sent,
                        st.loss(),
                        st.min,
                        st.avg(),
                        st.max,
                        st.jitter()
                    ));
                }
            }
        });
    });
    vec![b.upcast(), graph.upcast()]
}

/// Round trip times: a line per reply, red marks for lost ones.
fn draw(a: &gtk::DrawingArea, cr: &gtk::cairo::Context, w: i32, h: i32, s: &VecDeque<Option<f64>>) {
    let fg = a.color();
    let (w, h) = (f64::from(w), f64::from(h));
    let top = s.iter().flatten().fold(0.0f64, |m, v| m.max(*v)).max(0.1) * 1.2;
    cr.set_source_rgba(f64::from(fg.red()), f64::from(fg.green()), f64::from(fg.blue()), 0.12);
    cr.set_line_width(1.0);
    for i in 1..4 {
        let y = (h * f64::from(i) / 4.0).round() + 0.5;
        cr.move_to(0.0, y);
        cr.line_to(w, y);
    }
    let _ = cr.stroke();
    cr.set_source_rgba(f64::from(fg.red()), f64::from(fg.green()), f64::from(fg.blue()), 0.6);
    cr.set_font_size(10.0);
    cr.move_to(6.0, 12.0);
    let _ = cr.show_text(&format!("{top:.1} ms"));
    if s.is_empty() {
        return;
    }
    let step = w / (SAMPLES - 1) as f64;
    let x0 = w - step * (s.len() - 1) as f64;
    cr.set_source_rgba(0.85, 0.25, 0.25, 0.8);
    for (i, v) in s.iter().enumerate() {
        if v.is_none() {
            let x = x0 + step * i as f64;
            cr.rectangle(x - 1.0, 0.0, 2.0, h);
        }
    }
    let _ = cr.fill();
    cr.set_source_rgba(0.3, 0.8, 0.5, 0.95);
    cr.set_line_width(1.6);
    let mut pen = false;
    for (i, v) in s.iter().enumerate() {
        let x = x0 + step * i as f64;
        match v {
            Some(ms) => {
                let y = h - (ms / top) * (h - 4.0) - 2.0;
                if pen {
                    cr.line_to(x, y);
                } else {
                    cr.move_to(x, y);
                }
                pen = true;
            }
            None => pen = false,
        }
    }
    let _ = cr.stroke();
}

fn tracer(feed: &Feed) -> gtk::Box {
    let host = entry("Host or IP", 22);
    let go = start_button();
    let b = bar(&[host.upcast_ref(), label("tracepath, no root needed").upcast_ref(), spacer().upcast_ref(), go.upcast_ref()]);
    let stop: Rc<RefCell<Arc<AtomicBool>>> = Rc::default();
    let busy = Rc::new(Cell::new(false));
    let (fd, st, bz) = (feed.clone(), stop.clone(), busy.clone());
    go.connect_toggled(move |b| {
        if bz.get() {
            return;
        }
        if !b.is_active() {
            st.borrow().store(true, Ordering::Release);
            return;
        }
        let target = host.text().trim().to_string();
        if target.is_empty() || target.starts_with('-') {
            fd.line("31", "enter a host");
            return reset(b, &bz);
        }
        let flag = Arc::new(AtomicBool::new(false));
        st.replace(flag.clone());
        let (tx, rx) = async_channel::unbounded();
        if let Err(e) = nettools::trace(&target, flag, tx) {
            fd.line("31", &e);
            return reset(b, &bz);
        }
        fd.line("2", &format!("tracepath {target}"));
        let (fd, b, bz) = (fd.clone(), b.clone(), bz.clone());
        glib::spawn_future_local(async move {
            while let Ok(ev) = rx.recv().await {
                match ev {
                    Trace::Line(l) if l.contains("no reply") => fd.line("33", &l),
                    Trace::Line(l) => fd.line("", &l),
                    Trace::Ended => {
                        reset(&b, &bz);
                        break;
                    }
                }
            }
        });
    });
    b
}
