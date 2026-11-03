// SSH tunnels without a terminal: one `ssh -N` per tunnel, restarted when
// it drops. Stored in tunnels.toml; shared by all windows.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use gtk::glib;
use serde::{Deserialize, Serialize};

use crate::store::write_private;
use crate::{settings, term};

const VERSION: u32 = 1;
/// Alive this long counts as up (ssh -N prints nothing when it works).
const SETTLE: Duration = Duration::from_secs(3);

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dir {
    /// -L: a local port reaches `dest` through the server.
    #[default]
    Local,
    /// -R: a port on the server reaches `dest` on this side.
    Remote,
    /// -D: a local SOCKS proxy through the server.
    Dynamic,
}

impl Dir {
    pub const ALL: [Dir; 3] = [Dir::Local, Dir::Remote, Dir::Dynamic];

    pub fn label(self) -> &'static str {
        match self {
            Dir::Local => "Local (-L)",
            Dir::Remote => "Remote (-R)",
            Dir::Dynamic => "Dynamic SOCKS (-D)",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Tunnel {
    pub name: String,
    /// ssh destination: user@host[:port] or a ~/.ssh/config alias.
    pub host: String,
    /// Jump hosts, comma separated (ssh -J).
    #[serde(skip_serializing_if = "String::is_empty")]
    pub jump: String,
    pub dir: Dir,
    /// Listening side: "8080" or "127.0.0.1:8080".
    pub bind: String,
    /// "host:port" the connections go to; unused for SOCKS.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub dest: String,
    pub autostart: bool,
    pub reconnect: bool,
}

impl Default for Tunnel {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            jump: String::new(),
            dir: Dir::Local,
            bind: String::new(),
            dest: String::new(),
            autostart: false,
            reconnect: true,
        }
    }
}

/// True for "port", "addr:port" or "[v6]:port".
fn has_port(s: &str) -> bool {
    let port = s.rsplit(':').next().unwrap_or(s);
    port.parse::<u16>().is_ok_and(|p| p > 0)
}

impl Tunnel {
    pub fn check(&self) -> Result<(), String> {
        if self.name.trim().is_empty() {
            return Err("Give the tunnel a name".into());
        }
        if self.host.trim().is_empty() || self.host.contains(char::is_whitespace) || self.host.starts_with('-') {
            return Err("SSH host: user@host[:port] or an ssh config alias".into());
        }
        if !has_port(self.bind.trim()) {
            return Err("Listen: a port, or address:port".into());
        }
        if self.dir != Dir::Dynamic && !(self.dest.contains(':') && has_port(self.dest.trim())) {
            return Err("Destination: host:port".into());
        }
        Ok(())
    }

    /// "L 8080 → 10.0.0.5:80 via user@host".
    pub fn summary(&self) -> String {
        match self.dir {
            Dir::Local => format!("L {} → {} via {}", self.bind, self.dest, self.host),
            Dir::Remote => format!("R {} on {} → {}", self.bind, self.host, self.dest),
            Dir::Dynamic => format!("SOCKS {} via {}", self.bind, self.host),
        }
    }

    /// ssh arguments. The connection is not shared with terminal sessions,
    /// so stopping the tunnel really closes its ports.
    pub fn args(&self) -> Vec<String> {
        let mut a: Vec<String> = [
            "-N",
            "-S",
            "none",
            "-o",
            "ExitOnForwardFailure=yes",
            "-o",
            "ServerAliveInterval=15",
            "-o",
            "ServerAliveCountMax=3",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let bind = self.bind.trim();
        match self.dir {
            Dir::Local => a.extend(["-L".into(), format!("{bind}:{}", self.dest.trim())]),
            Dir::Remote => a.extend(["-R".into(), format!("{bind}:{}", self.dest.trim())]),
            Dir::Dynamic => a.extend(["-D".into(), bind.to_string()]),
        }
        let jumps: Vec<&str> = self.jump.split(',').map(str::trim).filter(|j| !j.is_empty()).collect();
        if !jumps.is_empty() {
            a.extend(["-J".into(), jumps.join(",")]);
        }
        // user@host:port: ssh takes the port separately.
        let host = self.host.trim();
        let (dest, port) = match host.rsplit_once(':') {
            Some((h, p)) if !h.contains(':') && p.parse::<u16>().is_ok() => (h, Some(p)),
            _ => (host, None),
        };
        if let Some(p) = port {
            a.extend(["-p".into(), p.to_string()]);
        }
        a.extend(["--".into(), dest.to_string()]);
        a
    }
}

#[derive(Serialize, Deserialize)]
struct File {
    version: u32,
    #[serde(default, rename = "tunnel")]
    tunnels: Vec<Tunnel>,
}

pub fn path() -> PathBuf {
    settings::dir().join("tunnels.toml")
}

pub fn parse(text: &str) -> Result<Vec<Tunnel>, String> {
    let f: File = toml::from_str(text).map_err(|e| e.to_string())?;
    if f.version > VERSION {
        return Err(format!("unsupported version {}", f.version));
    }
    Ok(f.tunnels)
}

fn load_from(path: &Path) -> Vec<Tunnel> {
    match fs::read_to_string(path) {
        Ok(t) => parse(&t).unwrap_or_else(|e| {
            crate::warn(&format!("{}: {e}", path.display()));
            let _ = fs::rename(path, path.with_extension("toml.bad"));
            Vec::new()
        }),
        Err(e) => {
            if e.kind() != io::ErrorKind::NotFound {
                crate::warn(&format!("{}: {e}", path.display()));
            }
            Vec::new()
        }
    }
}

fn save(list: &[Tunnel]) -> io::Result<()> {
    let text = toml::to_string_pretty(&File { version: VERSION, tunnels: list.to_vec() }).map_err(io::Error::other)?;
    let p = path();
    if let Some(d) = p.parent() {
        fs::create_dir_all(d)?;
    }
    write_private(&p, &text)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Stopped,
    Starting,
    Up,
    /// Dropped; retrying in n seconds.
    Retry(String, u64),
    Failed(String),
}

enum Ev {
    Started,
    Exited(String),
    Retry(u64),
    Done,
}

/// Runs `prog args` until `stop`, restarting after it ends when
/// `reconnect` (2, 5, 10, then 30 s apart; back to 2 after a long run).
fn supervise(prog: String, args: Vec<String>, env: Vec<(String, String)>, reconnect: bool, stop: Arc<AtomicBool>, pid: Arc<AtomicI32>, tx: async_channel::Sender<Ev>) {
    thread::spawn(move || {
        let waits = [2u64, 5, 10, 30];
        let mut fails = 0usize;
        loop {
            let spawned = crate::sys::spawn_tied(
                crate::sys::command(&prog)
                    .args(&args)
                    .envs(env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::piped()),
            );
            let mut ch = match spawned {
                Ok(ch) => ch,
                Err(e) => {
                    let _ = tx.send_blocking(Ev::Exited(format!("{prog}: {e}")));
                    let _ = tx.send_blocking(Ev::Done);
                    return;
                }
            };
            let started = Instant::now();
            pid.store(ch.id() as i32, Ordering::Release);
            let _ = tx.send_blocking(Ev::Started);
            let mut err = ch.stderr.take();
            // stderr is read in the background so a chatty ssh never blocks.
            let errs = thread::spawn(move || {
                let mut s = String::new();
                if let Some(e) = err.as_mut() {
                    let _ = e.read_to_string(&mut s);
                }
                s
            });
            loop {
                if stop.load(Ordering::Acquire) {
                    let _ = ch.kill();
                }
                match ch.try_wait() {
                    Ok(Some(_)) | Err(_) => break,
                    Ok(None) => thread::sleep(Duration::from_millis(200)),
                }
            }
            let _ = ch.wait();
            pid.store(0, Ordering::Release);
            let text = errs.join().unwrap_or_default();
            if stop.load(Ordering::Acquire) {
                let _ = tx.send_blocking(Ev::Done);
                return;
            }
            let last = text.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("ssh ended").trim().to_string();
            let _ = tx.send_blocking(Ev::Exited(last));
            if !reconnect {
                let _ = tx.send_blocking(Ev::Done);
                return;
            }
            fails = if started.elapsed() > Duration::from_secs(60) { 0 } else { fails + 1 };
            let wait = waits[fails.saturating_sub(1).min(waits.len() - 1)];
            let _ = tx.send_blocking(Ev::Retry(wait));
            for _ in 0..wait * 10 {
                if stop.load(Ordering::Acquire) {
                    let _ = tx.send_blocking(Ev::Done);
                    return;
                }
                thread::sleep(Duration::from_millis(100));
            }
        }
    });
}

struct Run {
    stop: Arc<AtomicBool>,
    pid: Arc<AtomicI32>,
    status: Status,
    since: Instant,
    /// Bytes read and written by ssh (/proc/<pid>/io), all runs.
    moved: u64,
    base: u64,
}

/// Running tunnels and their status, for all windows.
pub struct Manager {
    list: RefCell<Vec<Tunnel>>,
    runs: RefCell<HashMap<String, Run>>,
    listeners: RefCell<Vec<Box<dyn Fn()>>>,
    prog: String,
}

/// rchar + wchar of a process, or 0.
fn io_bytes(pid: i32) -> u64 {
    let Ok(text) = fs::read_to_string(format!("/proc/{pid}/io")) else { return 0 };
    text.lines()
        .filter_map(|l| l.split_once(": "))
        .filter(|(k, _)| *k == "rchar" || *k == "wchar")
        .filter_map(|(_, v)| v.trim().parse::<u64>().ok())
        .sum()
}

pub fn human(n: u64) -> String {
    let units = ["B", "KB", "MB", "GB", "TB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < units.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 { format!("{n} B") } else { format!("{v:.1} {}", units[u]) }
}

impl Manager {
    pub fn new() -> Rc<Self> {
        let m = Rc::new(Self {
            list: RefCell::new(load_from(&path())),
            runs: RefCell::default(),
            listeners: RefCell::default(),
            prog: "ssh".into(),
        });
        // Status: up after it stayed alive a moment; traffic.
        let w = Rc::downgrade(&m);
        glib::timeout_add_seconds_local(1, move || {
            let Some(m) = w.upgrade() else { return glib::ControlFlow::Break };
            m.tick();
            glib::ControlFlow::Continue
        });
        m
    }

    pub fn on_change(&self, f: impl Fn() + 'static) {
        self.listeners.borrow_mut().push(Box::new(f));
    }

    fn notify(&self) {
        for f in self.listeners.borrow().iter() {
            f();
        }
    }

    pub fn list(&self) -> Vec<Tunnel> {
        self.list.borrow().clone()
    }

    pub fn status(&self, name: &str) -> (Status, u64) {
        self.runs.borrow().get(name).map(|r| (r.status.clone(), r.moved)).unwrap_or((Status::Stopped, 0))
    }

    fn tick(&self) {
        let mut changed = false;
        for r in self.runs.borrow_mut().values_mut() {
            if r.status == Status::Starting && r.since.elapsed() >= SETTLE {
                r.status = Status::Up;
                changed = true;
            }
            let pid = r.pid.load(Ordering::Acquire);
            if pid > 0 {
                let now = r.base + io_bytes(pid);
                if now != r.moved {
                    r.moved = now;
                    changed = true;
                }
            }
        }
        if changed {
            self.notify();
        }
    }

    /// Replaces the saved list; running tunnels that changed are restarted.
    pub fn set(self: &Rc<Self>, list: Vec<Tunnel>) -> Result<(), String> {
        let old = self.list.replace(list.clone());
        for t in &old {
            let same = list.iter().any(|n| n == t);
            if !same && self.runs.borrow().contains_key(&t.name) {
                self.stop(&t.name);
                if list.iter().any(|n| n.name == t.name) {
                    self.start(&t.name);
                }
            }
        }
        let res = save(&list).map_err(|e| format!("save tunnels: {e}"));
        self.notify();
        res
    }

    pub fn start_auto(self: &Rc<Self>) {
        for t in self.list().iter().filter(|t| t.autostart) {
            self.start(&t.name);
        }
    }

    pub fn start(self: &Rc<Self>, name: &str) {
        let Some(t) = self.list.borrow().iter().find(|t| t.name == name).cloned() else { return };
        if self.runs.borrow().contains_key(name) {
            return;
        }
        let env: Vec<(String, String)> = term::ssh_env(&t.host, true)
            .iter()
            .filter_map(|kv| kv.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
            .collect();
        let stop = Arc::new(AtomicBool::new(false));
        let pid = Arc::new(AtomicI32::new(0));
        let (tx, rx) = async_channel::unbounded();
        supervise(self.prog.clone(), t.args(), env, t.reconnect, stop.clone(), pid.clone(), tx);
        self.runs.borrow_mut().insert(
            name.to_string(),
            Run { stop, pid, status: Status::Starting, since: Instant::now(), moved: 0, base: 0 },
        );
        self.notify();
        let (w, name) = (Rc::downgrade(self), name.to_string());
        glib::spawn_future_local(async move {
            while let Ok(ev) = rx.recv().await {
                let Some(m) = w.upgrade() else { break };
                let mut runs = m.runs.borrow_mut();
                let Some(r) = runs.get_mut(&name) else { break };
                match ev {
                    Ev::Started => {
                        r.status = Status::Starting;
                        r.since = Instant::now();
                        r.base = r.moved;
                    }
                    Ev::Exited(why) => r.status = Status::Failed(why),
                    Ev::Retry(s) => {
                        let why = match &r.status {
                            Status::Failed(w) => w.clone(),
                            _ => String::new(),
                        };
                        r.status = Status::Retry(why, s);
                    }
                    Ev::Done => {
                        // A tunnel that failed for good stays listed as failed.
                        if !matches!(r.status, Status::Failed(_)) {
                            runs.remove(&name);
                        } else {
                            r.pid.store(0, Ordering::Release);
                        }
                        drop(runs);
                        m.notify();
                        break;
                    }
                }
                drop(runs);
                m.notify();
            }
        });
    }

    pub fn stop(&self, name: &str) {
        if let Some(r) = self.runs.borrow_mut().remove(name) {
            r.stop.store(true, Ordering::Release);
        }
        self.notify();
    }

    /// Tunnels started and not stopped.
    pub fn active(&self) -> usize {
        self.runs.borrow().len()
    }

    /// Stops every tunnel (Release All).
    pub fn stop_all(&self) {
        for (_, r) in self.runs.borrow_mut().drain() {
            r.stop.store(true, Ordering::Release);
        }
        self.notify();
    }

    pub fn running(&self, name: &str) -> bool {
        self.runs.borrow().get(name).is_some_and(|r| !matches!(r.status, Status::Failed(_)) || r.pid.load(Ordering::Acquire) > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tunnel(dir: Dir) -> Tunnel {
        Tunnel {
            name: "db".into(),
            host: "admin@gw.example:2222".into(),
            jump: "bastion, edge".into(),
            dir,
            bind: "127.0.0.1:5433".into(),
            dest: "10.0.0.5:5432".into(),
            ..Default::default()
        }
    }

    #[test]
    fn ssh_arguments() {
        let a = tunnel(Dir::Local).args();
        let s = a.join(" ");
        assert!(s.starts_with("-N -S none -o ExitOnForwardFailure=yes"));
        assert!(s.contains("-L 127.0.0.1:5433:10.0.0.5:5432"));
        assert!(s.contains("-J bastion,edge"));
        assert!(s.ends_with("-p 2222 -- admin@gw.example"));
        assert!(tunnel(Dir::Remote).args().join(" ").contains("-R 127.0.0.1:5433:10.0.0.5:5432"));
        let d = Tunnel { bind: "1080".into(), host: "box".into(), jump: String::new(), ..tunnel(Dir::Dynamic) };
        assert_eq!(d.args().join(" ").split(" -D ").nth(1), Some("1080 -- box"));
    }

    #[test]
    fn checks_and_file() {
        assert!(tunnel(Dir::Local).check().is_ok());
        assert!(Tunnel { bind: "x".into(), ..tunnel(Dir::Local) }.check().is_err());
        assert!(Tunnel { dest: "10.0.0.5".into(), ..tunnel(Dir::Local) }.check().is_err());
        assert!(Tunnel { dest: String::new(), ..tunnel(Dir::Dynamic) }.check().is_ok());
        assert!(Tunnel { host: "-oProxyCommand=x".into(), ..tunnel(Dir::Local) }.check().is_err());
        let text = toml::to_string_pretty(&File { version: 1, tunnels: vec![tunnel(Dir::Remote)] }).expect("dump");
        assert_eq!(parse(&text), Ok(vec![tunnel(Dir::Remote)]));
        assert_eq!(human(1536), "1.5 KB");
    }

    #[test]
    fn supervisor_restarts_and_stops() {
        let dir = std::env::temp_dir().join(format!("tv-tun-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("dir");
        let fake = dir.join("ssh");
        fs::write(&fake, "#!/bin/sh\necho 'bind: Address already in use' >&2\nexit 255\n").expect("write");
        fs::set_permissions(&fake, std::os::unix::fs::PermissionsExt::from_mode(0o755)).expect("chmod");
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = async_channel::unbounded();
        supervise(fake.to_string_lossy().into(), vec![], vec![], true, stop.clone(), Arc::default(), tx);
        assert!(matches!(rx.recv_blocking(), Ok(Ev::Started)));
        match rx.recv_blocking() {
            Ok(Ev::Exited(why)) => assert_eq!(why, "bind: Address already in use"),
            _ => panic!("no exit"),
        }
        assert!(matches!(rx.recv_blocking(), Ok(Ev::Retry(2))));
        stop.store(true, Ordering::Release);
        assert!(matches!(rx.recv_blocking(), Ok(Ev::Done)));

        // A long running one is killed on stop.
        fs::write(&fake, "#!/bin/sh\nexec sleep 30\n").expect("write");
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = async_channel::unbounded();
        supervise(fake.to_string_lossy().into(), vec![], vec![], false, stop.clone(), Arc::default(), tx);
        assert!(matches!(rx.recv_blocking(), Ok(Ev::Started)));
        let t = Instant::now();
        stop.store(true, Ordering::Release);
        assert!(matches!(rx.recv_blocking(), Ok(Ev::Done)));
        assert!(t.elapsed() < Duration::from_secs(2));
        let _ = fs::remove_dir_all(&dir);
    }
}
