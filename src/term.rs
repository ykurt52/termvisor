// Terminal creation and child process launch.

use std::path::Path;
use std::rc::Rc;

use gtk::{gio, glib, pango, prelude::*};
use vte::prelude::*;

use crate::state::State;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Shell,
    Ssh,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Shell => "Shell",
            Kind::Ssh => "SSH",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Kind::Shell => "utilities-terminal-symbolic",
            Kind::Ssh => "network-server-symbolic",
        }
    }
}

/// What a terminal runs.
#[derive(Clone)]
pub struct Launch {
    pub kind: Kind,
    pub argv: Vec<String>,
    pub dir: Option<String>,
    pub title: String,
    pub target: String,
}

impl Launch {
    pub fn shell(dir: Option<String>) -> Self {
        Self {
            kind: Kind::Shell,
            argv: vec![shell()],
            dir,
            title: "Shell".into(),
            target: "local".into(),
        }
    }

    /// `ssh <args>`; `target` is shown in the tab and status bar.
    pub fn ssh(args: Vec<String>, target: &str) -> Self {
        let mut argv = vec!["ssh".to_string()];
        argv.extend(args);
        Self {
            kind: Kind::Ssh,
            argv,
            dir: None,
            title: target.to_string(),
            target: target.to_string(),
        }
    }
}

fn shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty() && Path::new(s).exists())
        .unwrap_or_else(|| "/bin/sh".into())
}

/// Working directory reported by the shell (OSC 7), if any.
pub fn cwd(term: &vte::Terminal) -> Option<String> {
    let uri = term.current_directory_uri()?;
    let path = gio::File::for_uri(&uri).path()?;
    path.to_str().map(str::to_string)
}

/// Creates a terminal and starts `launch` in it.
pub fn spawn(state: &Rc<State>, launch: &Launch) -> vte::Terminal {
    let term = vte::Terminal::new();
    term.set_hexpand(true);
    term.set_vexpand(true);
    term.set_mouse_autohide(true);
    term.set_audible_bell(false);
    {
        let cfg = state.cfg.borrow();
        term.set_scrollback_lines(cfg.scrollback as _);
        term.set_font(Some(&pango::FontDescription::from_string(&cfg.font)));
    }
    state.track(&term, launch);
    run(&term, launch);
    term
}

/// Starts (or restarts) the child process of `term`.
pub fn run(term: &vte::Terminal, launch: &Launch) {
    let argv: Vec<&str> = launch.argv.iter().map(String::as_str).collect();
    let dir = launch
        .dir
        .clone()
        .unwrap_or_else(|| glib::home_dir().to_string_lossy().into_owned());
    let weak = term.downgrade();
    let name = launch.argv.first().cloned().unwrap_or_default();
    term.spawn_async(
        vte::PtyFlags::DEFAULT,
        Some(&dir),
        &argv,
        &[],
        glib::SpawnFlags::SEARCH_PATH,
        || {},
        -1,
        None::<&gio::Cancellable>,
        move |res| {
            if let Err(e) = res {
                crate::warn(&format!("spawn {name}: {e}"));
                if let Some(t) = weak.upgrade() {
                    t.feed(format!("\r\nfailed to start {name}: {e}\r\n").as_bytes());
                }
            }
        },
    );
}

/// Wraps a terminal in a scrollable leaf widget.
pub fn leaf(term: &vte::Terminal) -> gtk::ScrolledWindow {
    let sw = gtk::ScrolledWindow::new();
    sw.set_hscrollbar_policy(gtk::PolicyType::Never);
    sw.set_hexpand(true);
    sw.set_vexpand(true);
    sw.set_child(Some(term));
    sw
}
