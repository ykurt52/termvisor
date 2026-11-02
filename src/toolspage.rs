// Tools page of the side panel: opens tool tabs; Wake-on-LAN list.

use adw::prelude::*;
use gtk::glib;

use crate::nettools::{self, Wake};
use crate::sidebar::OpenFn;
use crate::term::{Launch, Tool};

const TOOLS: [(Tool, &str, &str); 5] = [
    (Tool::Tftp, "network-server-symbolic", "Files for boot loaders (U-Boot tftpboot)"),
    (Tool::Http, "folder-publicshare-symbolic", "Share a folder over HTTP"),
    (Tool::Scan, "system-search-symbolic", "Open TCP ports on hosts or a subnet"),
    (Tool::Ping, "network-transmit-receive-symbolic", "Round trip times with a graph"),
    (Tool::Trace, "mark-location-symbolic", "Route to a host (tracepath)"),
];

fn wake_row(w: &Wake, i: usize, list: &gtk::ListBox, note: &gtk::Label) -> adw::ActionRow {
    let r = adw::ActionRow::new();
    r.set_title(&glib::markup_escape_text(if w.name.is_empty() { &w.mac } else { &w.name }));
    let to = w.target().map(|a| a.to_string()).unwrap_or_default();
    r.set_subtitle(&glib::markup_escape_text(&format!("{} → {to}", w.mac)));
    let go = gtk::Button::from_icon_name("media-playback-start-symbolic");
    go.set_tooltip_text(Some("Wake"));
    let del = gtk::Button::from_icon_name("user-trash-symbolic");
    del.set_tooltip_text(Some("Delete"));
    for b in [&go, &del] {
        b.add_css_class("flat");
        b.set_valign(gtk::Align::Center);
        r.add_suffix(b);
    }
    let (wk, n) = (w.clone(), note.clone());
    go.connect_clicked(move |_| {
        let res = nettools::parse_mac(&wk.mac).and_then(|m| wk.target().and_then(|t| nettools::wake(m, t)));
        n.set_text(&match res {
            Ok(()) => format!("Magic packet sent to {}", wk.mac),
            Err(e) => e,
        });
    });
    let (l, n) = (list.downgrade(), note.downgrade());
    del.connect_clicked(move |_| {
        let mut f = nettools::load_tools();
        if i < f.wake.len() {
            f.wake.remove(i);
        }
        if let Err(e) = nettools::save_tools(&f) {
            if let Some(n) = n.upgrade() {
                n.set_text(&e);
            }
        }
        if let (Some(l), Some(n)) = (l.upgrade(), n.upgrade()) {
            fill(&l, &n);
        }
    });
    r
}

/// Shows the saved Wake-on-LAN targets.
fn fill(list: &gtk::ListBox, note: &gtk::Label) {
    list.remove_all();
    let saved = nettools::load_tools().wake;
    list.set_visible(!saved.is_empty());
    for (i, w) in saved.iter().enumerate() {
        list.append(&wake_row(w, i, list, note));
    }
}

pub fn page(open: OpenFn) -> gtk::Box {
    let tools = gtk::ListBox::new();
    tools.add_css_class("boxed-list");
    tools.set_selection_mode(gtk::SelectionMode::None);
    for (tool, icon, about) in TOOLS {
        let r = adw::ActionRow::new();
        r.set_title(tool.title());
        r.set_subtitle(about);
        r.add_prefix(&gtk::Image::from_icon_name(icon));
        r.set_activatable(true);
        let o = open.clone();
        r.connect_activated(move |_| o(Ok(Launch::tool(tool))));
        tools.append(&r);
    }

    let wol = adw::PreferencesGroup::new();
    wol.set_title("Wake-on-LAN");
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    let name = gtk::Entry::new();
    name.set_placeholder_text(Some("Name"));
    let mac = gtk::Entry::new();
    mac.set_placeholder_text(Some("MAC, e.g. 00:11:22:33:44:55"));
    let bcast = gtk::Entry::new();
    bcast.set_placeholder_text(Some("Broadcast (default 255.255.255.255)"));
    let add = gtk::Button::with_label("Add");
    let note = gtk::Label::new(None);
    note.add_css_class("dim-label");
    note.add_css_class("caption");
    note.set_wrap(true);
    note.set_xalign(0.0);
    let form = gtk::Box::new(gtk::Orientation::Vertical, 4);
    form.append(&list);
    form.append(&name);
    form.append(&mac);
    form.append(&bcast);
    form.append(&add);
    form.append(&note);
    wol.add(&form);

    fill(&list, &note);
    let (l, n) = (list.clone(), note.clone());
    add.connect_clicked(move |_| {
        let w = Wake { name: name.text().trim().to_string(), mac: mac.text().trim().to_string(), broadcast: bcast.text().trim().to_string(), port: None };
        if let Err(e) = nettools::parse_mac(&w.mac).and_then(|_| w.target().map(|_| ())) {
            return n.set_text(&e);
        }
        let mut f = nettools::load_tools();
        f.wake.push(w);
        match nettools::save_tools(&f) {
            Ok(()) => {
                name.set_text("");
                mac.set_text("");
                bcast.set_text("");
                n.set_text("");
                fill(&l, &n);
            }
            Err(e) => n.set_text(&e),
        }
    });

    let body = gtk::Box::new(gtk::Orientation::Vertical, 12);
    body.set_margin_start(8);
    body.set_margin_end(8);
    body.set_margin_top(6);
    body.append(&tools);
    body.append(&wol);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroll.set_vexpand(true);
    scroll.set_child(Some(&body));
    let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
    widget.append(&scroll);
    widget
}
