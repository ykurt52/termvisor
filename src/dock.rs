// Bottom dock: command panel, transfers, log.

use std::rc::Rc;

use gtk::prelude::*;

use crate::state::State;
use crate::xfers;

/// id, title, icon, placeholder text (None = real page).
const PAGES: &[(&str, &str, &str, Option<&str>)] = &[
    ("commands", "Commands", "input-keyboard-symbolic", Some("Command buttons, up to 50 per page. Planned for milestone 9.")),
    ("transfers", "Transfers", "folder-download-symbolic", None),
    ("log", "Log", "text-x-generic-symbolic", Some("Session log. Planned for milestone 9.")),
];

pub fn build(state: &Rc<State>) -> gtk::Box {
    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    for &(id, title, icon, text) in PAGES {
        let page: gtk::Widget = match text {
            None => xfers::page(&state.xfers).upcast(),
            Some(text) => {
                let p = adw::StatusPage::new();
                p.set_icon_name(Some(icon));
                p.set_description(Some(text));
                p.add_css_class("compact");
                p.upcast()
            }
        };
        stack.add_titled(&page, Some(id), title);
    }

    let switcher = gtk::StackSwitcher::new();
    switcher.set_stack(Some(&stack));
    switcher.set_hexpand(true);
    switcher.set_halign(gtk::Align::Start);

    let hide = gtk::Button::from_icon_name("pan-down-symbolic");
    hide.add_css_class("flat");
    hide.set_tooltip_text(Some("Hide panel (Ctrl+Shift+J)"));
    hide.set_action_name(Some("win.toggle-dock"));

    let head = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    head.set_margin_start(6);
    head.set_margin_end(4);
    head.set_margin_top(2);
    head.append(&switcher);
    head.append(&hide);

    let dock = gtk::Box::new(gtk::Orientation::Vertical, 0);
    dock.add_css_class("dock");
    dock.set_size_request(-1, 170);
    dock.append(&head);
    dock.append(&stack);
    dock
}
