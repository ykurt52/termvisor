// GUI freeze watchdog.
//
// The GTK thread bumps a heartbeat every second. A plain thread checks it;
// after 5 s without a beat it starts `termvisor --watchdog <pid>`, a separate
// process that asks the user to wait or to stop the frozen instance. When
// the beat comes back the helper is closed again.

use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use adw::prelude::*;
use gtk::{gio, glib};

use crate::recover;

pub const FLAG: &str = "--watchdog";
const STALL: u64 = 5_000;

pub fn start() {
    let base = Instant::now();
    let beat = Arc::new(AtomicU64::new(0));

    let b = beat.clone();
    glib::timeout_add_local(Duration::from_secs(1), move || {
        b.store(base.elapsed().as_millis() as u64, Ordering::Relaxed);
        glib::ControlFlow::Continue
    });

    let spawned = thread::Builder::new().name("watchdog".into()).spawn(move || {
        let mut helper: Option<Child> = None;
        loop {
            thread::sleep(Duration::from_secs(1));
            let age = (base.elapsed().as_millis() as u64).saturating_sub(beat.load(Ordering::Relaxed));
            match (&mut helper, age > STALL) {
                (None, true) => helper = spawn_helper(),
                (Some(h), false) => {
                    let _ = h.kill();
                    let _ = h.wait();
                    helper = None;
                }
                (Some(h), true) => {
                    // Helper finished (user chose Wait): ask again later only
                    // after a fresh stall.
                    if matches!(h.try_wait(), Ok(Some(_))) {
                        helper = None;
                        thread::sleep(Duration::from_secs(10));
                    }
                }
                (None, false) => {}
            }
        }
    });
    if let Err(e) = spawned {
        crate::warn(&format!("watchdog: {e}"));
    }
}

fn spawn_helper() -> Option<Child> {
    let exe = std::env::current_exe().ok()?;
    Command::new(exe).arg(FLAG).arg(std::process::id().to_string()).spawn().ok()
}

/// Helper process: the dialog shown for a frozen instance.
pub fn helper(pid: u32) -> glib::ExitCode {
    let app = adw::Application::builder().flags(gio::ApplicationFlags::NON_UNIQUE).build();
    app.connect_activate(move |app| {
        let ports = recover::instances()
            .into_iter()
            .find(|i| i.pid == pid)
            .map(|i| i.ports)
            .unwrap_or_default();
        let held = if ports.is_empty() {
            String::new()
        } else {
            format!("\nIt holds: {}", ports.join(", "))
        };

        let label = gtk::Label::new(Some(&format!(
            "termvisor (PID {pid}) has not responded for 5 seconds.{held}\n\n\
             Stop it to free its serial ports and connections. Open sessions in it will close."
        )));
        label.set_wrap(true);
        label.set_xalign(0.0);

        let wait = gtk::Button::with_label("Wait");
        let kill = gtk::Button::with_label("Stop and Release");
        kill.add_css_class("destructive-action");
        let buttons = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        buttons.set_halign(gtk::Align::End);
        buttons.append(&wait);
        buttons.append(&kill);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 16);
        body.set_margin_top(12);
        body.set_margin_bottom(12);
        body.set_margin_start(12);
        body.set_margin_end(12);
        body.append(&label);
        body.append(&buttons);

        let tv = adw::ToolbarView::new();
        tv.add_top_bar(&adw::HeaderBar::new());
        tv.set_content(Some(&body));
        let win = adw::ApplicationWindow::builder()
            .application(app)
            .title("termvisor is not responding")
            .default_width(440)
            .content(&tv)
            .build();

        let w = win.clone();
        wait.connect_clicked(move |_| w.close());
        let w = win.clone();
        kill.connect_clicked(move |_| {
            if recover::stop(pid) {
                let r = recover::tidy();
                if !r.is_empty() {
                    println!("{}", r.summary());
                }
            }
            w.close();
        });
        win.present();
        wait.grab_focus();
    });
    app.run_with_args::<&str>(&[]);
    glib::ExitCode::SUCCESS
}
