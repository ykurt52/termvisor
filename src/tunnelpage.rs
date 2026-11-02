// Tunnels page of the side panel: list with status, start/stop, editor.

use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;

use crate::state::State;
use crate::tunnels::{self, Dir, Status, Tunnel};

fn status_text(s: &Status, moved: u64) -> (String, &'static str) {
    match s {
        Status::Stopped => ("stopped".into(), "dim-label"),
        Status::Starting => ("◌ connecting".into(), "tint-yellow"),
        Status::Up => (format!("● up · ≈ {} moved", tunnels::human(moved)), "tint-green"),
        Status::Retry(why, secs) if why.is_empty() => (format!("◌ retrying in {secs} s"), "tint-yellow"),
        Status::Retry(why, secs) => (format!("◌ {why}; retrying in {secs} s"), "tint-yellow"),
        Status::Failed(why) => (format!("○ {why}"), "tint-red"),
    }
}

fn row(state: &Rc<State>, t: &Tunnel) -> adw::ActionRow {
    let m = &state.tunnels;
    let (status, moved) = m.status(&t.name);
    let r = adw::ActionRow::new();
    r.set_title(&glib::markup_escape_text(&t.name));
    r.set_subtitle(&glib::markup_escape_text(&t.summary()));
    r.set_subtitle_lines(2);

    let on = gtk::Switch::new();
    on.set_valign(gtk::Align::Center);
    on.set_active(m.running(&t.name));
    on.set_tooltip_text(Some("Start or stop"));
    r.add_prefix(&on);

    let (text, tint) = status_text(&status, moved);
    let state_label = gtk::Label::new(Some(&text));
    state_label.add_css_class("caption");
    state_label.add_css_class(tint);
    state_label.set_ellipsize(gtk::pango::EllipsizeMode::End);
    state_label.set_max_width_chars(28);
    state_label.set_tooltip_text(Some(&text));
    let edit = gtk::Button::from_icon_name("document-edit-symbolic");
    edit.set_tooltip_text(Some("Edit"));
    let del = gtk::Button::from_icon_name("user-trash-symbolic");
    del.set_tooltip_text(Some("Delete"));
    for b in [&edit, &del] {
        b.add_css_class("flat");
        b.set_valign(gtk::Align::Center);
    }
    let side = gtk::Box::new(gtk::Orientation::Vertical, 0);
    side.set_valign(gtk::Align::Center);
    side.append(&state_label);
    let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    buttons.set_halign(gtk::Align::End);
    buttons.append(&edit);
    buttons.append(&del);
    side.append(&buttons);
    r.add_suffix(&side);

    let (st, name) = (state.clone(), t.name.clone());
    on.connect_state_set(move |_, v| {
        if v {
            st.tunnels.start(&name);
        } else {
            st.tunnels.stop(&name);
        }
        glib::Propagation::Proceed
    });
    let (st, tun) = (state.clone(), t.clone());
    edit.connect_clicked(move |b| editor(b, &st, Some(tun.clone())));
    let (st, name) = (state.clone(), t.name.clone());
    del.connect_clicked(move |_| {
        st.tunnels.stop(&name);
        let list: Vec<Tunnel> = st.tunnels.list().into_iter().filter(|x| x.name != name).collect();
        if let Err(e) = st.tunnels.set(list) {
            crate::warn(&e);
        }
    });
    r
}

pub fn page(state: &Rc<State>) -> gtk::Box {
    let add = gtk::Button::with_label("+ New Tunnel");
    add.set_halign(gtk::Align::Start);
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::None);
    let empty = gtk::Label::new(Some("No tunnels. Forward a port through an SSH server without opening a terminal."));
    empty.add_css_class("dim-label");
    empty.set_wrap(true);
    empty.set_margin_top(12);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.set_margin_start(8);
    body.set_margin_end(8);
    body.set_margin_top(6);
    body.append(&add);
    body.append(&list);
    body.append(&empty);
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroll.set_vexpand(true);
    scroll.set_child(Some(&body));
    let widget = gtk::Box::new(gtk::Orientation::Vertical, 0);
    widget.append(&scroll);

    let fill = {
        let (st, list, empty) = (Rc::downgrade(state), list.downgrade(), empty.downgrade());
        move || {
            let (Some(st), Some(list), Some(empty)) = (st.upgrade(), list.upgrade(), empty.upgrade()) else { return };
            list.remove_all();
            let all = st.tunnels.list();
            for t in &all {
                list.append(&row(&st, t));
            }
            list.set_visible(!all.is_empty());
            empty.set_visible(all.is_empty());
        }
    };
    fill();
    state.tunnels.on_change(fill);
    let st = state.clone();
    add.connect_clicked(move |b| editor(b, &st, None));
    widget
}

fn entry(title: &str, text: &str) -> adw::EntryRow {
    let r = adw::EntryRow::new();
    r.set_title(title);
    r.set_text(text);
    r
}

/// Tunnel editor; `init` None makes a new one.
fn editor(parent: &impl IsA<gtk::Widget>, state: &Rc<State>, init: Option<Tunnel>) {
    let old_name = init.as_ref().map(|t| t.name.clone());
    let t = init.unwrap_or_default();
    let name = entry("Name", &t.name);
    let host = entry("SSH Host (user@host[:port] or config alias)", &t.host);
    let jump = entry("Jump Hosts (optional, comma separated)", &t.jump);
    let kind = adw::ComboRow::new();
    kind.set_title("Type");
    let labels: Vec<&str> = Dir::ALL.iter().map(|d| d.label()).collect();
    kind.set_model(Some(&gtk::StringList::new(&labels)));
    kind.set_selected(Dir::ALL.iter().position(|d| *d == t.dir).unwrap_or(0) as u32);
    let bind = entry("Listen (port or address:port)", &t.bind);
    let dest = entry("Destination (host:port)", &t.dest);
    let auto = adw::SwitchRow::new();
    auto.set_title("Start with termvisor");
    auto.set_active(t.autostart);
    let again = adw::SwitchRow::new();
    again.set_title("Reconnect when it drops");
    again.set_active(t.reconnect);

    let hint = adw::PreferencesGroup::new();
    let help = gtk::Label::new(None);
    help.add_css_class("dim-label");
    help.set_wrap(true);
    help.set_xalign(0.0);
    hint.add(&help);
    let sync = {
        let (kind, bind, dest, help) = (kind.clone(), bind.clone(), dest.clone(), help.clone());
        move || {
            let d = Dir::ALL.get(kind.selected() as usize).copied().unwrap_or_default();
            dest.set_visible(d != Dir::Dynamic);
            let (b, h) = match d {
                Dir::Local => ("Listen on this computer", "Connections to the listen port here go through the server to the destination."),
                Dir::Remote => ("Listen on the server", "Connections to the listen port on the server come back here to the destination."),
                Dir::Dynamic => ("SOCKS port on this computer", "A SOCKS5 proxy here; apps that use it reach anything the server reaches."),
            };
            bind.set_title(&format!("{b} (port or address:port)"));
            help.set_text(h);
        }
    };
    sync();
    kind.connect_selected_notify(move |_| sync());

    let general = adw::PreferencesGroup::new();
    for w in [name.upcast_ref::<gtk::Widget>(), host.upcast_ref(), jump.upcast_ref()] {
        general.add(w);
    }
    let fwd = adw::PreferencesGroup::new();
    fwd.set_title("Forward");
    for w in [kind.upcast_ref::<gtk::Widget>(), bind.upcast_ref(), dest.upcast_ref()] {
        fwd.add(w);
    }
    let opts = adw::PreferencesGroup::new();
    opts.add(&auto);
    opts.add(&again);
    let page = adw::PreferencesPage::new();
    page.add(&general);
    page.add(&fwd);
    page.add(&hint);
    page.add(&opts);
    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&page));

    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::with_label("Save");
    save.add_css_class("suggested-action");
    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    header.pack_start(&cancel);
    header.pack_end(&save);
    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&toasts));
    let dlg = adw::Dialog::new();
    dlg.set_title(if old_name.is_some() { "Edit Tunnel" } else { "New Tunnel" });
    dlg.set_content_width(480);
    dlg.set_content_height(640);
    dlg.set_child(Some(&view));

    let d = dlg.clone();
    cancel.connect_clicked(move |_| {
        d.close();
    });
    let (d, st) = (dlg.clone(), state.clone());
    save.connect_clicked(move |_| {
        let dir = Dir::ALL.get(kind.selected() as usize).copied().unwrap_or_default();
        let t = Tunnel {
            name: name.text().trim().to_string(),
            host: host.text().trim().to_string(),
            jump: jump.text().trim().to_string(),
            dir,
            bind: bind.text().trim().to_string(),
            dest: if dir == Dir::Dynamic { String::new() } else { dest.text().trim().to_string() },
            autostart: auto.is_active(),
            reconnect: again.is_active(),
        };
        if let Err(e) = t.check() {
            return toasts.add_toast(adw::Toast::new(&e));
        }
        let mut list = st.tunnels.list();
        if list.iter().any(|x| x.name == t.name && Some(&x.name) != old_name.as_ref()) {
            return toasts.add_toast(adw::Toast::new("A tunnel with this name exists"));
        }
        match old_name.as_ref().and_then(|o| list.iter().position(|x| &x.name == o)) {
            Some(i) => {
                // A rename stops the old one.
                if list[i].name != t.name {
                    st.tunnels.stop(&list[i].name);
                }
                list[i] = t;
            }
            None => list.push(t),
        }
        match st.tunnels.set(list) {
            Ok(()) => {
                d.close();
            }
            Err(e) => toasts.add_toast(adw::Toast::new(&e)),
        }
    });
    dlg.present(Some(parent));
}
