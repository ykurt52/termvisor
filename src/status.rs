// Status bar at the bottom of the window.

use gtk::prelude::*;

use crate::term::{Kind, Launch};

#[derive(Clone)]
pub struct Status {
    pub bar: gtk::Box,
    icon: gtk::Image,
    kind: gtk::Label,
    target: gtk::Label,
    theme: gtk::Label,
}

impl Status {
    pub fn new() -> Self {
        let bar = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        bar.add_css_class("statusbar");

        let icon = gtk::Image::new();
        let kind = gtk::Label::new(None);
        let target = gtk::Label::new(None);
        let spacer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        spacer.set_hexpand(true);
        let enc = gtk::Label::new(Some("UTF-8"));
        let theme = gtk::Label::new(None);

        bar.append(&icon);
        bar.append(&kind);
        bar.append(&target);
        bar.append(&spacer);
        bar.append(&enc);
        bar.append(&theme);
        Self { bar, icon, kind, target, theme }
    }

    pub fn show(&self, launch: Option<&Launch>, theme: &str) {
        for c in ["kind-shell", "kind-ssh", "kind-serial"] {
            self.bar.remove_css_class(c);
        }
        match launch {
            Some(l) => {
                self.bar.add_css_class(match l.kind {
                    Kind::Shell => "kind-shell",
                    Kind::Ssh => "kind-ssh",
                    Kind::Serial => "kind-serial",
                });
                self.icon.set_icon_name(Some(l.kind.icon()));
                self.icon.set_visible(true);
                self.kind.set_text(l.kind.label());
                self.target.set_text(&l.target);
            }
            None => {
                self.icon.set_visible(false);
                self.kind.set_text("No session");
                self.target.set_text("");
            }
        }
        self.theme.set_text(theme);
    }
}
