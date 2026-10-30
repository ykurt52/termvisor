// Fuzzy session finder (Ctrl+Shift+K).

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gdk;

use crate::sessions::{self, Node, NodeKind};
use crate::sidebar::OpenFn;

const LIMIT: usize = 50;

fn row(label: &str, node: &Node) -> gtk::ListBoxRow {
    let icon = gtk::Image::from_icon_name(node.icon);
    icon.add_css_class(node.tint);
    let text = gtk::Label::new(Some(label));
    text.set_xalign(0.0);
    text.set_ellipsize(gtk::pango::EllipsizeMode::End);
    let b = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    b.set_margin_top(6);
    b.set_margin_bottom(6);
    b.set_margin_start(8);
    b.set_margin_end(8);
    b.append(&icon);
    b.append(&text);
    let r = gtk::ListBoxRow::new();
    r.set_child(Some(&b));
    r
}

/// Shows the finder over `parent` for `items` (label, node).
pub fn open(parent: &impl IsA<gtk::Widget>, items: Vec<(String, Node)>, pick: OpenFn) {
    let entry = gtk::SearchEntry::new();
    entry.set_placeholder_text(Some("Find a session"));
    entry.set_hexpand(true);

    let list = gtk::ListBox::new();
    list.add_css_class("navigation-sidebar");
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_child(Some(&list));

    let body = gtk::Box::new(gtk::Orientation::Vertical, 8);
    body.set_margin_top(12);
    body.set_margin_bottom(12);
    body.set_margin_start(12);
    body.set_margin_end(12);
    body.append(&entry);
    body.append(&scroll);

    let dlg = adw::Dialog::new();
    dlg.set_title("Find Session");
    dlg.set_content_width(520);
    dlg.set_content_height(420);
    dlg.set_child(Some(&body));

    // Indices into `items` for the rows currently shown.
    let shown: Rc<RefCell<Vec<usize>>> = Rc::default();
    let items = Rc::new(items);

    let fill = {
        let (list, items, shown) = (list.clone(), items.clone(), shown.clone());
        move |q: &str| {
            list.remove_all();
            let mut ranked: Vec<(i32, usize)> = items
                .iter()
                .enumerate()
                .filter_map(|(i, (label, _))| sessions::fuzzy(q, label).map(|s| (s, i)))
                .collect();
            ranked.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
            ranked.truncate(LIMIT);
            let mut sh = shown.borrow_mut();
            sh.clear();
            for (_, i) in ranked {
                let (label, node) = &items[i];
                list.append(&row(label, node));
                sh.push(i);
            }
            if let Some(first) = list.row_at_index(0) {
                list.select_row(Some(&first));
            }
        }
    };
    fill("");
    let f = fill.clone();
    entry.connect_search_changed(move |e| f(&e.text()));

    let choose = {
        let (d, items, shown) = (dlg.clone(), items.clone(), shown.clone());
        Rc::new(move |r: &gtk::ListBoxRow| {
            let Ok(pos) = usize::try_from(r.index()) else { return };
            let Some(&i) = shown.borrow().get(pos) else { return };
            if let NodeKind::Item(l) = &items[i].1.kind {
                d.close();
                pick(l.clone());
            }
        })
    };
    let c = choose.clone();
    list.connect_row_activated(move |_, r| c(r));
    let (c, l) = (choose.clone(), list.clone());
    entry.connect_activate(move |_| {
        if let Some(r) = l.selected_row() {
            c(&r);
        }
    });

    // Up/Down move the selection while typing.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let l = list.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        let step = match key {
            gdk::Key::Down => 1,
            gdk::Key::Up => -1,
            _ => return gtk::glib::Propagation::Proceed,
        };
        let cur = l.selected_row().map(|r| r.index()).unwrap_or(-1);
        if let Some(r) = l.row_at_index((cur + step).max(0)) {
            l.select_row(Some(&r));
        }
        gtk::glib::Propagation::Stop
    });
    entry.add_controller(keys);

    dlg.present(Some(parent));
    entry.grab_focus();
}
