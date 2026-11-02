// Bottom dock: command panel, transfers, log.

use std::rc::Rc;

use gtk::prelude::*;

use crate::state::State;
use crate::{cmdpanel, logpanel, xfers};

pub struct Dock {
    pub widget: gtk::Box,
    pub commands: Rc<cmdpanel::Panel>,
    pub logs: Rc<logpanel::Panel>,
}

pub fn build(state: &Rc<State>) -> Dock {
    let commands = cmdpanel::build(state);
    let logs = logpanel::build(state);
    let stack = gtk::Stack::new();
    stack.set_vexpand(true);
    stack.add_titled(&commands.widget, Some("commands"), "Commands");
    stack.add_titled(&xfers::page(&state.xfers), Some("transfers"), "Transfers");
    stack.add_titled(&logs.widget, Some("log"), "Log");

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
    Dock { widget: dock, commands, logs }
}
