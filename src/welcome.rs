// Start page shown when a window has no tabs.

use gtk::prelude::*;

/// icon, tint, label, action, action target.
const CARDS: &[(&str, &str, &str, &str, Option<&str>)] = &[
    ("utilities-terminal-symbolic", "tint-blue", "Local Shell", "win.new-tab", None),
    ("network-server-symbolic", "tint-green", "SSH", "win.quick-connect", None),
    ("media-removable-symbolic", "tint-yellow", "Serial", "win.new-session", Some("serial")),
    ("network-wired-symbolic", "tint-red", "Telnet", "win.new-session", Some("telnet")),
    ("network-transmit-receive-symbolic", "tint-magenta", "Raw TCP", "win.new-session", Some("tcp")),
    ("folder-remote-symbolic", "tint-cyan", "FTP", "win.new-session", Some("ftp")),
];

fn card(icon: &str, tint: &str, label: &str) -> gtk::Button {
    let img = gtk::Image::from_icon_name(icon);
    img.set_pixel_size(22);
    let bubble = gtk::Box::new(gtk::Orientation::Vertical, 0);
    bubble.set_halign(gtk::Align::Center);
    bubble.add_css_class("bubble");
    bubble.add_css_class(tint);
    bubble.append(&img);
    let text = gtk::Label::new(Some(label));
    let body = gtk::Box::new(gtk::Orientation::Vertical, 6);
    body.append(&bubble);
    body.append(&text);
    let b = gtk::Button::new();
    b.add_css_class("card");
    b.set_child(Some(&body));
    b
}

pub fn build() -> adw::StatusPage {
    let flow = gtk::FlowBox::new();
    flow.set_selection_mode(gtk::SelectionMode::None);
    flow.set_homogeneous(true);
    flow.set_max_children_per_line(6);
    flow.set_column_spacing(8);
    flow.set_row_spacing(8);
    flow.set_min_children_per_line(3);
    flow.set_halign(gtk::Align::Center);
    for &(icon, tint, label, action, target) in CARDS {
        let b = card(icon, tint, label);
        b.set_action_name(Some(action));
        if let Some(t) = target {
            b.set_action_target_value(Some(&t.to_variant()));
        }
        flow.insert(&b, -1);
    }

    let hint = gtk::Label::new(Some(
        "Ctrl+Shift+T new tab · Ctrl+Shift+K find session · Ctrl+Shift+L quick connect (/dev/ttyUSB0@115200) · Ctrl+Shift+B sidebar · Ctrl+Shift+J bottom panel",
    ));
    hint.add_css_class("dim-label");
    hint.set_wrap(true);

    let body = gtk::Box::new(gtk::Orientation::Vertical, 16);
    body.append(&flow);
    body.append(&hint);

    let page = adw::StatusPage::new();
    page.add_css_class("welcome");
    page.add_css_class("compact");
    page.set_icon_name(Some(crate::APP_ID));
    page.set_title("termvisor");
    page.set_description(Some("Start a session"));
    page.set_child(Some(&body));
    page
}
