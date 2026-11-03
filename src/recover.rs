// Recovery: find and release resources left by crashed or hung instances.
//
// A crashed process frees its fds and its ptys hang up its children, so the
// leftovers are: stale UUCP locks, stale ControlMaster sockets, live shared
// SSH connections, and other termvisor processes that hang while holding ports.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(unix)]
use std::thread;
#[cfg(unix)]
use std::time::Duration;

use crate::askpass;
use crate::serial;
use crate::term;

/// Another termvisor process and the serial ports it holds.
pub struct Instance {
    pub pid: u32,
    pub ports: Vec<String>,
}

#[derive(Default)]
pub struct Report {
    pub locks: usize,
    pub sockets: usize,
    pub masters: usize,
    pub killed: Vec<u32>,
    pub failed: Vec<u32>,
}

impl Report {
    pub fn is_empty(&self) -> bool {
        self.locks + self.sockets + self.masters + self.killed.len() + self.failed.len() == 0
    }

    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if self.locks > 0 {
            parts.push(format!("{} stale lock(s) removed", self.locks));
        }
        if self.sockets > 0 {
            parts.push(format!("{} stale socket(s) removed", self.sockets));
        }
        if self.masters > 0 {
            parts.push(format!("{} shared SSH connection(s) closed", self.masters));
        }
        if !self.killed.is_empty() {
            parts.push(format!("stopped PID {}", join(&self.killed)));
        }
        if !self.failed.is_empty() {
            parts.push(format!("could not stop PID {}", join(&self.failed)));
        }
        if parts.is_empty() {
            "Nothing to release".into()
        } else {
            parts.join(", ")
        }
    }
}

fn join(pids: &[u32]) -> String {
    pids.iter().map(u32::to_string).collect::<Vec<_>>().join(", ")
}

/// True for our helper processes (askpass, watchdog, CLI), which are not
/// instances.
fn is_helper(pid: u32) -> bool {
    let env = fs::read(format!("/proc/{pid}/environ")).unwrap_or_default();
    let marker = format!("{}=", askpass::ENV_MODE);
    if env.split(|b| *b == 0).any(|v| v.starts_with(marker.as_bytes())) {
        return true;
    }
    let cmd = fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
    cmd.split(|b| *b == 0).skip(1).any(|a| a.starts_with(b"--"))
}

/// Serial-type devices `pid` has open (ptys excluded).
fn tty_fds(pid: u32) -> Vec<String> {
    let Ok(fds) = fs::read_dir(format!("/proc/{pid}/fd")) else {
        return Vec::new();
    };
    let mut out: Vec<String> = fds
        .flatten()
        .filter_map(|fd| fs::read_link(fd.path()).ok())
        .filter_map(|p| p.to_str().map(str::to_string))
        .filter(|p| p.starts_with("/dev/tty") && p != "/dev/tty")
        .collect();
    out.sort();
    out.dedup();
    out
}

/// Other running termvisor instances of this user.
pub fn instances() -> Vec<Instance> {
    let me = std::process::id();
    let Ok(procs) = fs::read_dir("/proc") else {
        return Vec::new();
    };
    procs
        .flatten()
        .filter_map(|p| p.file_name().to_str()?.parse::<u32>().ok())
        .filter(|&pid| pid != me && serial::comm(pid) == "termvisor" && !is_helper(pid))
        .map(|pid| Instance { pid, ports: tty_fds(pid) })
        .collect()
}

/// SIGTERM, then SIGKILL after 2 s. True when the process is gone.
#[cfg(unix)]
pub fn stop(pid: u32) -> bool {
    let Ok(id) = i32::try_from(pid) else { return false };
    // SAFETY: kill(2) with a plain pid and signal has no memory effects.
    unsafe {
        libc::kill(id, libc::SIGTERM);
    }
    for _ in 0..20 {
        if !serial::alive(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    // SAFETY: as above.
    unsafe {
        libc::kill(id, libc::SIGKILL);
    }
    for _ in 0..10 {
        if !serial::alive(pid) {
            return true;
        }
        thread::sleep(Duration::from_millis(100));
    }
    false
}

/// Other instances are not looked up on Windows (no /proc).
#[cfg(not(unix))]
pub fn stop(_pid: u32) -> bool {
    false
}

/// UUCP locks pointing at dead processes.
fn stale_locks(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("LCK..")))
        .filter(|p| {
            let pid = fs::read_to_string(p).ok().and_then(|t| t.trim().parse::<u32>().ok());
            pid.is_some_and(|pid| !serial::alive(pid))
        })
        .collect()
}

/// ControlMaster sockets, split into (live, stale).
fn sockets() -> (Vec<PathBuf>, Vec<PathBuf>) {
    let Some(dir) = term::mux_dir() else {
        return (Vec::new(), Vec::new());
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return (Vec::new(), Vec::new());
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("cm-")))
        .partition(|p| live(p))
}

/// True when an ssh master answers on `sock`.
fn live(sock: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::net::UnixStream::connect(sock).is_ok()
    }
    // No connection sharing without Unix sockets.
    #[cfg(not(unix))]
    {
        let _ = sock;
        false
    }
}

/// Asks the ssh master behind `sock` to exit.
fn exit_master(sock: &Path) -> bool {
    Command::new("ssh")
        .arg("-o")
        .arg(format!("ControlPath={}", sock.display()))
        .args(["-O", "exit", "termvisor-release"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// Removes stale locks and sockets. Safe at any time.
pub fn tidy() -> Report {
    let mut r = Report::default();
    for p in stale_locks(Path::new(serial::LOCK_DIR)) {
        if fs::remove_file(&p).is_ok() {
            r.locks += 1;
        }
    }
    for p in sockets().1 {
        if fs::remove_file(&p).is_ok() {
            r.sockets += 1;
        }
    }
    r
}

/// tidy() plus closing every shared SSH connection and, if asked, stopping
/// the other instances.
pub fn release(stop_others: bool) -> Report {
    let mut r = tidy();
    for s in sockets().0 {
        if exit_master(&s) {
            r.masters += 1;
        }
    }
    if stop_others {
        for i in instances() {
            if stop(i.pid) {
                r.killed.push(i.pid);
            } else {
                r.failed.push(i.pid);
            }
        }
        // Their locks are stale now.
        let after = tidy();
        r.locks += after.locks;
        r.sockets += after.sockets;
    }
    r
}

/// Live shared SSH connections.
pub fn masters() -> usize {
    sockets().0.len()
}

/// `termvisor --release-all`: release everything and print what was done.
pub fn cli() -> i32 {
    for i in instances() {
        let ports = if i.ports.is_empty() { "no serial ports".into() } else { i.ports.join(", ") };
        println!("stopping termvisor PID {} ({ports})", i.pid);
    }
    let r = release(true);
    println!("{}", r.summary());
    i32::from(!r.failed.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_only_dead_locks() {
        let dir = std::env::temp_dir().join(format!("tv-recover-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("dir");
        fs::write(dir.join("LCK..ttyDEAD"), format!("{:>10}\n", u32::MAX - 1)).expect("dead");
        fs::write(dir.join("LCK..ttyLIVE"), format!("{:>10}\n", std::process::id())).expect("live");
        fs::write(dir.join("other"), "1\n").expect("other");
        let stale = stale_locks(&dir);
        assert_eq!(stale.len(), 1);
        assert!(stale[0].ends_with("LCK..ttyDEAD"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn report_text() {
        assert_eq!(Report::default().summary(), "Nothing to release");
        let r = Report { locks: 1, killed: vec![42], ..Default::default() };
        assert_eq!(r.summary(), "1 stale lock(s) removed, stopped PID 42");
    }

    #[test]
    fn stops_a_child_process() {
        let mut child = Command::new("sleep").arg("30").spawn().expect("spawn");
        let pid = child.id();
        // Reap in the background so the pid disappears from /proc.
        let reaper = thread::spawn(move || child.wait());
        assert!(stop(pid));
        let _ = reaper.join();
    }
}
