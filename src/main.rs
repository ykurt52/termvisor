// termvisor entry point.

mod dock;
mod pane;
mod sessions;
mod settings;
mod sidebar;
mod state;
mod status;
mod term;
mod theme;
mod welcome;
mod window;

use std::cell::OnceCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gdk, gio, glib};

use state::State;

pub const APP_ID: &str = "io.github.ykurt52.termvisor";

pub fn warn(msg: &str) {
    glib::g_warning!("termvisor", "{}", msg);
}

fn main() -> glib::ExitCode {
    if let Err(e) = gio::resources_register_include!("termvisor.gresource") {
        warn(&format!("resources: {e}"));
    }

    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();

    app.connect_startup(|app| {
        if let Some(d) = gdk::Display::default() {
            gtk::IconTheme::for_display(&d).add_resource_path("/io/github/ykurt52/termvisor/icons");
        }
        gtk::Window::set_default_icon_name(APP_ID);
        for (action, keys) in window::ACCELS {
            app.set_accels_for_action(action, keys);
        }
    });

    let state: Rc<OnceCell<Rc<State>>> = Rc::new(OnceCell::new());
    app.connect_activate(move |app| {
        let st = state.get_or_init(State::new);
        window::build(app, st, true);
    });

    app.run()
}
