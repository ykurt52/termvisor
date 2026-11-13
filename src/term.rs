// Terminal creation and child process launch.

use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::{gio, glib, pango, prelude::*};

use crate::adb::{self, AdbLaunch};
use crate::askpass;
use serde::{Deserialize, Serialize};

use crate::ftp::FtpCfg;
use crate::link::StreamOpts;
use crate::net::NetCfg;
use crate::serial::SerialCfg;
use crate::state::State;
use crate::{output, pty};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Shell,
    Ssh,
    Serial,
    Tcp,
    Telnet,
    Mosh,
    Ftp,
    /// `adb shell` on an Android device.
    Adb,
    /// Device log view of an Android device.
    Logcat,
    /// USB traffic capture.
    Usb,
    /// Network tool tab (servers, scanner, ping).
    Tool,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Shell => "Shell",
            Kind::Ssh => "SSH",
            Kind::Serial => "Serial",
            Kind::Tcp => "TCP",
            Kind::Telnet => "Telnet",
            Kind::Mosh => "Mosh",
            Kind::Ftp => "FTP",
            Kind::Adb => "ADB",
            Kind::Logcat => "Logcat",
            Kind::Usb => "USB",
            Kind::Tool => "Tool",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Kind::Shell => "utilities-terminal-symbolic",
            Kind::Ssh => "network-server-symbolic",
            Kind::Serial => "media-removable-symbolic",
            Kind::Tcp => "network-transmit-receive-symbolic",
            Kind::Telnet => "network-wired-symbolic",
            Kind::Mosh => "network-wireless-symbolic",
            Kind::Ftp => "folder-remote-symbolic",
            Kind::Adb => "phone-symbolic",
            Kind::Logcat => "view-list-symbolic",
            Kind::Usb => "drive-removable-media-symbolic",
            Kind::Tool => "applications-utilities-symbolic",
        }
    }
}

/// Byte-stream session handled in-process (no child program).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Stream {
    Serial(SerialCfg),
    Tcp(NetCfg),
    Telnet(NetCfg),
}

impl Stream {
    pub fn opts(&self) -> &StreamOpts {
        match self {
            Stream::Serial(c) => &c.opts,
            Stream::Tcp(c) | Stream::Telnet(c) => &c.opts,
        }
    }

    pub fn kind(&self) -> Kind {
        match self {
            Stream::Serial(_) => Kind::Serial,
            Stream::Tcp(_) => Kind::Tcp,
            Stream::Telnet(_) => Kind::Telnet,
        }
    }

    /// e.g. "/dev/ttyUSB0 115200 8N1" or "10.0.0.2:5760".
    pub fn desc(&self) -> String {
        match self {
            Stream::Serial(c) => format!("{} {}", c.device, c.summary()),
            Stream::Tcp(c) | Stream::Telnet(c) => c.addr(),
        }
    }

    fn short(&self) -> String {
        match self {
            Stream::Serial(c) => c.device.rsplit('/').next().unwrap_or(&c.device).to_string(),
            Stream::Tcp(c) | Stream::Telnet(c) => c.addr(),
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
    /// In-process byte stream (serial, TCP, telnet).
    pub stream: Option<Stream>,
    /// FTP server; opens in the Files panel instead of a tab.
    pub ftp: Option<FtpCfg>,
    /// Android device (shell or logcat).
    pub adb: Option<AdbLaunch>,
    /// USB capture.
    pub usb: Option<UsbLaunch>,
    /// Network tool.
    pub tool: Option<Tool>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tool {
    Tftp,
    Http,
    Scan,
    Ping,
    Trace,
}

impl Tool {
    pub fn title(self) -> &'static str {
        match self {
            Tool::Tftp => "TFTP Server",
            Tool::Http => "HTTP Server",
            Tool::Scan => "Port Scanner",
            Tool::Ping => "Ping",
            Tool::Trace => "Traceroute",
        }
    }
}

/// What a USB capture tab shows.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UsbLaunch {
    /// usbmon bus; 0 is every bus.
    pub bus: u16,
    /// Only this device address on the bus.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dev: Option<u8>,
    /// Start in the serial view.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub serial: bool,
    /// A saved pcap file instead of a live capture.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
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
            stream: None,
            ftp: None,
            adb: None,
            usb: None,
            tool: None,
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
            stream: None,
            ftp: None,
            adb: None,
            usb: None,
            tool: None,
        }
    }

    pub fn ftp(cfg: FtpCfg) -> Self {
        Self {
            kind: Kind::Ftp,
            argv: Vec::new(),
            dir: None,
            title: cfg.host.clone(),
            target: cfg.id(),
            keyring: true,
            stream: None,
            ftp: Some(cfg),
            adb: None,
            usb: None,
            tool: None,
        }
    }

    /// `mosh <args>`; ssh prompts go through askpass like ssh sessions.
    pub fn mosh(args: Vec<String>, target: &str) -> Self {
        let mut l = Self::ssh(args, target);
        l.kind = Kind::Mosh;
        l.argv[0] = "mosh".into();
        l
    }

    /// `adb shell` on `serial` (empty: the only device); `name` is shown.
    pub fn adb(serial: &str, name: &str, logcat: bool) -> Self {
        let shown = if name.is_empty() { if serial.is_empty() { "Android" } else { serial } } else { name };
        Self {
            kind: if logcat { Kind::Logcat } else { Kind::Adb },
            argv: Vec::new(),
            dir: None,
            title: if logcat { format!("logcat {shown}") } else { shown.to_string() },
            target: format!("adb:{serial}"),
            keyring: false,
            stream: None,
            ftp: None,
            adb: Some(AdbLaunch { serial: serial.to_string(), logcat }),
            usb: None,
            tool: None,
        }
    }

    /// USB capture; `title` names the device or file.
    pub fn usb(u: UsbLaunch, title: &str) -> Self {
        Self {
            kind: Kind::Usb,
            argv: Vec::new(),
            dir: None,
            title: title.to_string(),
            target: match (&u.file, u.dev) {
                (Some(f), _) => f.clone(),
                (None, Some(d)) => format!("usb:{}-{d}", u.bus),
                (None, None) => format!("usb:{}", u.bus),
            },
            keyring: false,
            stream: None,
            ftp: None,
            adb: None,
            usb: Some(u),
            tool: None,
        }
    }

    pub fn tool(t: Tool) -> Self {
        Self {
            kind: Kind::Tool,
            argv: Vec::new(),
            dir: None,
            title: t.title().into(),
            target: format!("tool:{}", t.title().to_lowercase()),
            keyring: false,
            stream: None,
            ftp: None,
            adb: None,
            usb: None,
            tool: Some(t),
        }
    }

    pub fn stream(s: Stream) -> Self {
        Self {
            kind: s.kind(),
            argv: Vec::new(),
            dir: None,
            title: s.short(),
            target: s.desc(),
            keyring: false,
            stream: Some(s),
            ftp: None,
            adb: None,
            usb: None,
            tool: None,
        }
    }
}

fn shell() -> String {
    if cfg!(windows) {
        return if found("pwsh") { "pwsh.exe" } else { "powershell.exe" }.into();
    }
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty() && Path::new(s).exists())
        .unwrap_or_else(|| "/bin/sh".into())
}

/// Working directory reported by the shell (OSC 7), if any.
pub fn cwd(term: &crate::vt::View) -> Option<String> {
    let uri = term.current_directory_uri()?;
    let path = gio::File::for_uri(&uri).path()?;
    path.to_str().map(str::to_string)
}

/// Creates a terminal and starts `launch` in it.
pub fn spawn(state: &Rc<State>, launch: &Launch) -> crate::vt::View {
    let term: crate::vt::View = pty::Term::default().upcast();
    term.set_hexpand(true);
    term.set_vexpand(true);
    {
        let cfg = state.cfg.borrow();
        term.set_scrollback_lines(cfg.scrollback as _);
        term.set_font(Some(&pango::FontDescription::from_string(&cfg.font)));
    }
    state.track(&term, launch);
    // Stream panes set up their own pipeline.
    if launch.stream.is_none() {
        output::attach(&term, &launch.title, &state.cfg.borrow());
    }
    let st = Rc::downgrade(state);
    term.connect_current_directory_uri_changed(move |t| {
        if let Some(s) = st.upgrade() {
            s.cwd_changed(t);
        }
    });
    run(&term, launch);
    term
}

/// Socket dir for ssh connection sharing, created 0700. None if unusable.
pub fn mux_dir() -> Option<PathBuf> {
    let dir = PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join("termvisor");
    // Unix socket paths are limited to ~108 bytes; %C expands to 40 chars.
    if dir.as_os_str().len() > 60 {
        return None;
    }
    crate::sys::private_dir(&dir).ok()?;
    Some(dir)
}

/// Final argv and extra environment for `launch`.
fn command(launch: &Launch) -> (Vec<String>, Vec<String>) {
    let mut argv = launch.argv.clone();
    let mut env = Vec::new();
    if let (Kind::Adb, Some(a)) = (launch.kind, &launch.adb) {
        // Waits for the device, so Enter after an unplug reconnects.
        argv = vec![adb::bin()];
        argv.extend(adb::select(&a.serial));
        argv.extend(["wait-for-device".to_string(), "shell".into()]);
        return (argv, env);
    }
    if launch.kind != Kind::Ssh && launch.kind != Kind::Mosh {
        return (argv, env);
    }
    if let (Kind::Ssh, Some(dir)) = (launch.kind, mux_dir()) {
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
    env.extend(ssh_env(&launch.target, launch.keyring));
    (argv, env)
}

/// Environment ("KEY=value") that makes ssh ask for passwords through
/// termvisor's dialogs; `target` names the host in them.
pub fn ssh_env(target: &str, keyring: bool) -> Vec<String> {
    let mut env = Vec::new();
    match std::env::current_exe() {
        Ok(exe) => {
            env.push(format!("SSH_ASKPASS={}", exe.display()));
            env.push("SSH_ASKPASS_REQUIRE=force".into());
            env.push(format!("{}=1", askpass::ENV_MODE));
            env.push(format!("{}={target}", askpass::ENV_TARGET));
            if !keyring {
                env.push(format!("{}=1", askpass::ENV_NO_STORE));
            }
        }
        Err(e) => crate::warn(&format!("askpass disabled: {e}")),
    }
    env
}

/// Starts (or restarts) the child process of `term` on its own pty.
pub fn run(term: &crate::vt::View, launch: &Launch) {
    // Stream and logcat panes have no child here; they feed the terminal.
    if launch.stream.is_some() || matches!(launch.kind, Kind::Logcat | Kind::Usb | Kind::Tool) {
        return;
    }
    if launch.kind == Kind::Adb {
        let bin = adb::bin();
        if !adb::found(&bin) {
            term.feed(format!("\r\n\x1b[31m{bin} not found. Install adb (Debian/Ubuntu: adb, Arch: android-tools) or set \"adb\" in settings.json.\x1b[0m\r\n").as_bytes());
            return;
        }
        let who = launch.adb.as_ref().map(|a| a.serial.as_str()).filter(|s| !s.is_empty()).unwrap_or("the device");
        term.feed(format!("\x1b[2mwaiting for {who}…\x1b[0m\r\n").as_bytes());
    }
    let (Some(t), Some(out)) = (term.downcast_ref::<pty::Term>(), output::of(term)) else {
        crate::warn("terminal without pty support");
        return;
    };
    let (argv, env) = command(launch);
    let env: Vec<(String, String)> =
        env.iter().filter_map(|kv| kv.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let dir = launch
        .dir
        .clone()
        .filter(|d| Path::new(d).is_dir())
        .unwrap_or_else(|| glib::home_dir().to_string_lossy().into_owned());
    if let Err(e) = t.start(&argv, &env, &dir, out) {
        let name = argv.first().cloned().unwrap_or_default();
        crate::warn(&format!("spawn {name}: {e}"));
        term.feed(format!("\r\nfailed to start {name}: {e}\r\n").as_bytes());
    }
}

/// Wraps a terminal in a scrolled window.
pub fn scroll(term: &crate::vt::View) -> gtk::Overlay {
    let sw = gtk::ScrolledWindow::new();
    sw.set_hscrollbar_policy(gtk::PolicyType::Never);
    sw.set_hexpand(true);
    sw.set_vexpand(true);
    sw.set_child(Some(term));
    // The Plot button sits on the terminal itself, clear of pane toolbars
    // and send bars.
    let o = gtk::Overlay::new();
    o.set_hexpand(true);
    o.set_vexpand(true);
    o.set_child(Some(&sw));
    if crate::output::of(term).is_some() {
        // Shown while the pointer is on the terminal or the plot is open,
        // so it does not hide text otherwise.
        let b = crate::pane::plot_button(term);
        b.set_visible(false);
        o.add_overlay(&b);
        let m = gtk::EventControllerMotion::new();
        let w = b.downgrade();
        m.connect_enter(move |_, _, _| {
            if let Some(b) = w.upgrade() {
                b.set_visible(true);
            }
        });
        let w = b.downgrade();
        m.connect_leave(move |_| {
            if let Some(b) = w.upgrade() {
                b.set_visible(b.has_css_class("on"));
            }
        });
        o.add_controller(m);
    }
    o
}

/// A pane toolbar that scrolls sideways instead of being cut off when the
/// pane is narrow; its start (name, mode) stays in view.
pub fn fit(bar: &impl IsA<gtk::Widget>) -> gtk::ScrolledWindow {
    let sw = gtk::ScrolledWindow::new();
    sw.set_policy(gtk::PolicyType::Automatic, gtk::PolicyType::Never);
    sw.set_propagate_natural_height(true);
    sw.set_child(Some(bar));
    sw
}

/// True when `prog` is an executable on PATH.
pub fn found(prog: &str) -> bool {
    let names = if cfg!(windows) { vec![prog.to_string(), format!("{prog}.exe")] } else { vec![prog.to_string()] };
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|d| names.iter().any(|n| fs::metadata(d.join(n)).is_ok_and(|m| crate::sys::executable(&m))))
    })
}

/// Program arguments and extra environment for an SFTP session.
pub type SftpCmd = (Vec<String>, Vec<(String, String)>);

/// `ssh … -s -- host sftp` for an SSH launch, sharing its connection and
/// prompts. None for other kinds.
pub fn sftp_command(launch: &Launch) -> Option<SftpCmd> {
    if launch.kind != Kind::Ssh {
        return None;
    }
    let (argv, env) = command(launch);
    let (prog, rest) = argv.split_first()?;
    let (opts, host) = match rest.iter().position(|a| a == "--") {
        Some(i) => (&rest[..i], rest.get(i + 1)?.clone()),
        None => {
            let (h, o) = rest.split_last()?;
            (o, h.clone())
        }
    };
    let mut cmd = vec![prog.clone()];
    cmd.extend(opts.iter().cloned());
    cmd.extend(["-s".to_string(), "--".into(), host, "sftp".into()]);
    let env = env
        .into_iter()
        .filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
        .collect();
    Some((cmd, env))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sftp_from_ssh_launch() {
        let l = Launch::ssh(vec!["-p".into(), "2222".into(), "--".into(), "board".into()], "board:2222");
        let (cmd, env) = sftp_command(&l).expect("cmd");
        let tail: Vec<&str> = cmd.iter().rev().take(4).rev().map(String::as_str).collect();
        assert_eq!(tail, ["-s", "--", "board", "sftp"]);
        assert!(cmd.windows(2).any(|w| w[0] == "-p" && w[1] == "2222"));
        assert!(env.iter().any(|(k, v)| k == "SSH_ASKPASS_REQUIRE" && v == "force"));

        let quick = Launch::ssh(vec!["root@10.0.0.2".into()], "root@10.0.0.2");
        let (cmd, _) = sftp_command(&quick).expect("cmd");
        assert_eq!(cmd.last().map(String::as_str), Some("sftp"));
        assert!(cmd.contains(&"root@10.0.0.2".to_string()));

        assert!(sftp_command(&Launch::shell(None)).is_none());
    }
}
