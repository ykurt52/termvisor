// termvisor entry point.

mod adb;
mod askpass;
mod cmdpanel;
mod commands;
mod devices;
mod dock;
mod editor;
mod files;
mod ftp;
mod keyring;
mod link;
mod logcat;
mod logpanel;
mod mav;
mod modem;
mod palette;
mod pty;
mod pane;
mod net;
mod output;
mod recent;
mod recover;
mod render;
mod remote;
mod sessions;
mod serial;
mod streampane;
mod settings;
mod sftp;
mod sidebar;
mod state;
mod status;
mod store;
mod term;
mod theme;
mod transfers;
mod watchdog;
mod welcome;
mod window;
mod xfers;

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
    // ssh calls us as SSH_ASKPASS with the prompt as the only argument.
    let args: Vec<String> = std::env::args().collect();
    if std::env::var_os(askpass::ENV_MODE).is_some() && args.len() == 2 {
        return askpass::run(&args[1]);
    }
    match args.get(1).map(String::as_str) {
        Some("--release-all") => return glib::ExitCode::from(u8::try_from(recover::cli()).unwrap_or(1)),
        Some(watchdog::FLAG) => {
            return match args.get(2).and_then(|p| p.parse::<u32>().ok()) {
                Some(pid) => watchdog::helper(pid),
                None => glib::ExitCode::FAILURE,
            };
        }
        _ => {}
    }

    if let Err(e) = gio::resources_register_include!("termvisor.gresource") {
        warn(&format!("resources: {e}"));
    }

    let app = adw::Application::builder()
        .application_id(APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();

    app.connect_startup(|app| {
        if let Some(d) = gdk::Display::default() {
            if let Some(why) = render::choose(&d, &settings::Settings::load().renderer) {
                glib::g_info!("termvisor", "renderer: {}", why);
            }
            gtk::IconTheme::for_display(&d).add_resource_path("/io/github/ykurt52/termvisor/icons");
        }
        gtk::Window::set_default_icon_name(APP_ID);
        for e in output::set_rules(&output::load_rules()) {
            warn(&format!("highlight rule skipped: {e}"));
        }
        for (action, keys) in window::ACCELS {
            app.set_accels_for_action(action, keys);
        }
        watchdog::start();
    });

    let state: Rc<OnceCell<Rc<State>>> = Rc::new(OnceCell::new());
    app.connect_activate(move |app| {
        let st = state.get_or_init(|| {
            let st = State::new();
            let r = recover::tidy();
            if !r.is_empty() {
                st.notice.replace(Some(format!("Startup cleanup: {}", r.summary())));
            }
            st
        });
        window::build(app, st, true);
    });

    app.run()
}
