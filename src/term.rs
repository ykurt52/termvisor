// Terminal creation and child process launch.

use std::fs;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::{gio, glib, pango, prelude::*};
use vte::prelude::*;

use crate::askpass;
use crate::serial::SerialCfg;
use crate::state::State;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Shell,
    Ssh,
    Serial,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Shell => "Shell",
            Kind::Ssh => "SSH",
            Kind::Serial => "Serial",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Kind::Shell => "utilities-terminal-symbolic",
            Kind::Ssh => "network-server-symbolic",
            Kind::Serial => "media-removable-symbolic",
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
    /// Allow the keyring for passwords (ssh only).
    pub keyring: bool,
    /// Port settings (serial only).
    pub serial: Option<SerialCfg>,
}

impl Launch {
    pub fn shell(dir: Option<String>) -> Self {
        Self {
            kind: Kind::Shell,
            argv: vec![shell()],
            dir,
            title: "Shell".into(),
            target: "local".into(),
            keyring: false,
            serial: None,
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
            keyring: true,
            serial: None,
        }
    }

    pub fn serial(cfg: SerialCfg) -> Self {
        let name = cfg.device.rsplit('/').next().unwrap_or(&cfg.device).to_string();
        Self {
            kind: Kind::Serial,
            argv: Vec::new(),
            dir: None,
            title: name,
            target: format!("{} {}", cfg.device, cfg.summary()),
            keyring: false,
            serial: Some(cfg),
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

/// Socket dir for ssh connection sharing, created 0700. None if unusable.
fn mux_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join("termvisor");
    // Unix socket paths are limited to ~108 bytes; %C expands to 40 chars.
    if dir.as_os_str().len() > 60 {
        return None;
    }
    fs::DirBuilder::new().recursive(true).mode(0o700).create(&dir).ok()?;
    Some(dir)
}

/// Final argv and extra environment for `launch`.
fn command(launch: &Launch) -> (Vec<String>, Vec<String>) {
    let mut argv = launch.argv.clone();
    let mut env = Vec::new();
    if launch.kind != Kind::Ssh {
        return (argv, env);
    }
    if let Some(dir) = mux_dir() {
        let opts = [
            "-o".to_string(),
            "ControlMaster=auto".into(),
            "-o".into(),
            format!("ControlPath={}/cm-%C", dir.display()),
            "-o".into(),
            "ControlPersist=60".into(),
        ];
        argv.splice(1..1, opts);
    }
    match std::env::current_exe() {
        Ok(exe) => {
            env.push(format!("SSH_ASKPASS={}", exe.display()));
            env.push("SSH_ASKPASS_REQUIRE=force".into());
            env.push(format!("{}=1", askpass::ENV_MODE));
            env.push(format!("{}={}", askpass::ENV_TARGET, launch.target));
            if !launch.keyring {
                env.push(format!("{}=1", askpass::ENV_NO_STORE));
            }
        }
        Err(e) => crate::warn(&format!("askpass disabled: {e}")),
    }
    (argv, env)
}

/// Starts (or restarts) the child process of `term`.
pub fn run(term: &vte::Terminal, launch: &Launch) {
    // Serial panes have no child process; the serial pane feeds them.
    if launch.kind == Kind::Serial {
        return;
    }
    let (full, env) = command(launch);
    let argv: Vec<&str> = full.iter().map(String::as_str).collect();
    let envv: Vec<&str> = env.iter().map(String::as_str).collect();
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
        &envv,
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

/// Wraps a terminal in a scrolled window.
pub fn scroll(term: &vte::Terminal) -> gtk::ScrolledWindow {
    let sw = gtk::ScrolledWindow::new();
    sw.set_hscrollbar_policy(gtk::PolicyType::Never);
    sw.set_hexpand(true);
    sw.set_vexpand(true);
    sw.set_child(Some(term));
    sw
}
