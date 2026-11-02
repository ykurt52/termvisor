// USB page of the side panel: devices from sysfs, capture buttons, pcap
// files, and the one-time setup that allows capturing.

use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use gtk::{gio, glib};

use crate::sidebar::OpenFn;
use crate::term::{Launch, UsbLaunch};
use crate::usb::{self, Device};

fn icon_button(icon: &str, tip: &str) -> gtk::Button {
    let b = gtk::Button::from_icon_name(icon);
    b.add_css_class("flat");
    b.set_valign(gtk::Align::Center);
    b.set_tooltip_text(Some(tip));
    b
}

fn row(d: &Device, open: &OpenFn) -> adw::ActionRow {
    let r = adw::ActionRow::new();
    let title = if d.name.is_empty() { format!("{:04x}:{:04x}", d.vid, d.pid) } else { d.name.clone() };
    r.set_title(&glib::markup_escape_text(&title));
    let mut note = format!("{:04x}:{:04x} · bus {} dev {}", d.vid, d.pid, d.bus, d.dev);
    if !d.speed.is_empty() {
        note.push_str(&format!(" · {} Mb/s", d.speed));
    }
    if !d.drivers.is_empty() {
        note.push_str(&format!(" · {}", d.drivers.join(", ")));
    }
    r.set_subtitle(&glib::markup_escape_text(&note));
    let capture = icon_button("media-record-symbolic", "Capture this device");
    let serial = icon_button("utilities-terminal-symbolic", "Capture as serial text (USB serial adapters)");
    r.add_suffix(&capture);
    r.add_suffix(&serial);
    for (b, as_serial) in [(&capture, false), (&serial, true)] {
        let (o, bus, dev) = (open.clone(), d.bus, d.dev);
        let name = format!("USB {}", d.label().trim());
        b.connect_clicked(move |_| o(Ok(Launch::usb(UsbLaunch { bus, dev: Some(dev), serial: as_serial, file: None }, &name))));
    }
    r
}

pub fn page(open: OpenFn) -> gtk::Box {
    let all = gtk::Button::with_label("Capture All Buses");
    all.set_tooltip_text(Some("Every USB device on every bus"));
    let file = gtk::Button::with_label("Open pcap…");
    file.set_tooltip_text(Some("View a saved capture (Linux USB pcap, link type 220)"));
    let head = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    head.append(&all);
    head.append(&file);

    let setup = adw::PreferencesGroup::new();
    setup.set_title("USB capture is not enabled");
    let why = gtk::Label::new(None);
    why.set_wrap(true);
    why.set_xalign(0.0);
    let info = gtk::Label::new(Some(&format!(
        "One time, as administrator: loads usbmon at boot ({}) and lets the logged-in user read /dev/usbmon* ({}). \
         Capturing only reads a copy of the traffic. To undo, delete both files.",
        usb::MODULES_FILE,
        usb::RULES_FILE
    )));
    info.add_css_class("dim-label");
    info.add_css_class("caption");
    info.set_wrap(true);
    info.set_xalign(0.0);
    let enable = gtk::Button::with_label("Enable USB Capture…");
    enable.add_css_class("suggested-action");
    enable.set_halign(gtk::Align::Start);
    let setup_box = gtk::Box::new(gtk::Orientation::Vertical, 6);
    setup_box.append(&why);
    setup_box.append(&info);
    setup_box.append(&enable);
    setup.add(&setup_box);

    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 10);
    body.set_margin_start(8);
    body.set_margin_end(8);
    body.set_margin_top(6);
    body.append(&head);
    body.append(&setup);
    body.append(&list);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroll.set_vexpand(true);
    scroll.set_child(Some(&body));
    let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
    widget.append(&scroll);

    let shown: Rc<std::cell::RefCell<Vec<(u16, u8, String)>>> = Rc::default();
    let fill = {
        let (list, setup, why, open, shown) = (list.downgrade(), setup.downgrade(), why.downgrade(), open.clone(), shown.clone());
        move || {
            let (Some(list), Some(setup), Some(why)) = (list.upgrade(), setup.upgrade(), why.upgrade()) else { return };
            let blocked = usb::blocked();
            setup.set_visible(blocked.is_some());
            why.set_text(blocked.as_deref().unwrap_or(""));
            // Root hubs (device 1) carry no traffic of their own.
            let devs: Vec<Device> = usb::devices().into_iter().filter(|d| d.dev > 1).collect();
            let key: Vec<(u16, u8, String)> = devs.iter().map(|d| (d.bus, d.dev, d.name.clone())).collect();
            if *shown.borrow() == key {
                return;
            }
            shown.replace(key);
            list.remove_all();
            for d in &devs {
                list.append(&row(d, &open));
            }
        }
    };
    fill();
    // Plugging in a device shows up within two seconds while visible.
    let (w, f) = (widget.downgrade(), fill.clone());
    glib::timeout_add_local(Duration::from_secs(2), move || {
        let Some(w) = w.upgrade() else { return glib::ControlFlow::Break };
        if w.is_mapped() {
            f();
        }
        glib::ControlFlow::Continue
    });
    let f = fill.clone();
    widget.connect_map(move |_| f());

    let o = open.clone();
    all.connect_clicked(move |_| o(Ok(Launch::usb(UsbLaunch::default(), "USB all buses"))));
    let o = open.clone();
    file.connect_clicked(move |b| {
        let dlg = gtk::FileDialog::new();
        let f = gtk::FileFilter::new();
        f.set_name(Some("Captures (*.pcap)"));
        f.add_pattern("*.pcap");
        dlg.set_default_filter(Some(&f));
        let o = o.clone();
        dlg.open(b.root().and_downcast::<gtk::Window>().as_ref(), None::<&gio::Cancellable>, move |res| {
            let Some(path) = res.ok().and_then(|f| f.path()) else { return };
            let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            o(Ok(Launch::usb(UsbLaunch { file: Some(path.to_string_lossy().into_owned()), ..Default::default() }, &name)));
        });
    });
    let (wy, f) = (why.downgrade(), fill.clone());
    enable.connect_clicked(move |b| {
        let Ok(exe) = std::env::current_exe() else { return };
        b.set_sensitive(false);
        let (b, wy, f) = (b.clone(), wy.clone(), f.clone());
        glib::spawn_future_local(async move {
            let res = gio::spawn_blocking(move || {
                std::process::Command::new("pkexec").arg(exe).arg("--usbmon-setup").output()
            })
            .await;
            b.set_sensitive(true);
            let msg = match res {
                Ok(Ok(o)) if o.status.success() => None,
                Ok(Ok(o)) if o.status.code() == Some(126) => Some("Cancelled.".to_string()),
                Ok(Ok(o)) => Some(format!("Setup failed: {}", String::from_utf8_lossy(&o.stderr).trim())),
                Ok(Err(e)) => Some(format!("pkexec: {e}")),
                Err(_) => Some("Setup failed.".into()),
            };
            f();
            if let (Some(m), Some(w)) = (msg, wy.upgrade()) {
                w.set_text(&m);
            }
        });
    });
    widget
}
