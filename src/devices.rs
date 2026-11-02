// Android page of the side panel: devices seen by adb, live. adb runs
// only once this page is first shown.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::adb::{self, Device, Track, Tracker};
use crate::sidebar::OpenFn;
use crate::term::Launch;

fn icon_button(icon: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.add_css_class("flat");
    b.set_valign(gtk::Align::Center);
    b.set_tooltip_text(Some(tip));
    b
}

fn row(d: &Device, open: &OpenFn) -> adw::ActionRow {
    let r = adw::ActionRow::new();
    r.set_title(&glib::markup_escape_text(&d.name()));
    let note = match d.state.as_str() {
        "device" => d.serial.clone(),
        "unauthorized" => format!("{} · allow USB debugging on the device", d.serial),
        other => format!("{} · {other}", d.serial),
    };
    r.set_subtitle(&glib::markup_escape_text(&note));
    let icon = gtk::Image::from_icon_name("phone-symbolic");
    icon.add_css_class(if d.ready() { "tint-green" } else { "tint-yellow" });
    r.add_prefix(&icon);

    let shell = icon_button("utilities-terminal-symbolic", "Shell");
    let logcat = icon_button("view-list-symbolic", "Logcat");
    let files = icon_button("folder-symbolic", "Files");
    for b in [&shell, &logcat, &files] {
        b.set_sensitive(d.ready());
        r.add_suffix(b);
    }
    let (serial, name) = (d.serial.clone(), d.name());
    let o = open.clone();
    shell.connect_clicked(move |_| o(Ok(Launch::adb(&serial, &name, false))));
    let (serial, name) = (d.serial.clone(), d.name());
    let o = open.clone();
    logcat.connect_clicked(move |_| o(Ok(Launch::adb(&serial, &name, true))));
    let (serial, name) = (d.serial.clone(), d.name());
    files.connect_clicked(move |b| {
        let _ = WidgetExt::activate_action(b, "win.adb-files", Some(&(serial.as_str(), name.as_str()).to_variant()));
    });
    r
}

struct Page {
    list: gtk::ListBox,
    note: gtk::Label,
    retry: gtk::Button,
    tracker: RefCell<Option<Tracker>>,
    open: OpenFn,
}

impl Page {
    fn show(&self, devs: &[Device]) {
        self.list.remove_all();
        for d in devs {
            self.list.append(&row(d, &self.open));
        }
        self.list.set_visible(!devs.is_empty());
        self.note.set_text(if devs.is_empty() { "No devices. Connect one with USB debugging on." } else { "" });
        self.note.set_visible(devs.is_empty());
    }

    fn start(self: &Rc<Self>) {
        if self.tracker.borrow().is_some() {
            return;
        }
        let bin = adb::bin();
        if !adb::found(&bin) {
            self.note.set_text(&format!("{bin} not found.\nInstall adb (Debian/Ubuntu: adb, Arch: android-tools) or set \"adb\" in settings.json."));
            self.note.set_visible(true);
            self.retry.set_visible(true);
            return;
        }
        self.retry.set_visible(false);
        self.note.set_text("Looking for devices…");
        self.note.set_visible(true);
        let (tx, rx) = async_channel::unbounded();
        self.tracker.replace(Some(Tracker::start(bin, tx)));
        let w = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok(ev) = rx.recv().await {
                let Some(p) = w.upgrade() else { break };
                match ev {
                    Track::Devices(d) => p.show(&d),
                    Track::Error(e) => {
                        p.list.set_visible(false);
                        p.note.set_text(&format!("adb: {e}\nRetrying…"));
                        p.note.set_visible(true);
                    }
                }
            }
        });
    }
}

pub fn page(open: OpenFn) -> gtk::Box {
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    list.set_visible(false);
    let note = gtk::Label::new(Some("Shown when this page opens."));
    note.add_css_class("dim-label");
    note.set_wrap(true);
    note.set_margin_top(12);
    note.set_justify(gtk::Justification::Center);
    let retry = gtk::Button::with_label("Retry");
    retry.set_halign(gtk::Align::Center);
    retry.set_visible(false);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.set_margin_start(8);
    body.set_margin_end(8);
    body.set_margin_top(6);
    body.append(&list);
    body.append(&note);
    body.append(&retry);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroll.set_vexpand(true);
    scroll.set_child(Some(&body));

    let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
    widget.append(&scroll);

    let p = Rc::new(Page { list, note, retry, tracker: RefCell::new(None), open });
    let w = Rc::downgrade(&p);
    p.retry.connect_clicked(move |_| {
        if let Some(p) = w.upgrade() {
            p.start();
        }
    });
    // adb starts the first time the page is shown, not with the app.
    let w = Rc::downgrade(&p);
    widget.connect_map(move |_| {
        if let Some(p) = w.upgrade() {
            p.start();
        }
    });
    // The page owns the tracker; closing the window stops adb.
    let keep = RefCell::new(Some(p));
    widget.connect_destroy(move |_| drop(keep.take()));
    widget
}
