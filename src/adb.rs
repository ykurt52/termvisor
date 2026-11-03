// Android devices through the adb command line tool: device list, live
// tracking and file access. Every call runs the configured adb binary;
// nothing talks to the adb server directly.

use std::io::{self, Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::remote::Remote;
use crate::settings;
use crate::sftp::{Attrs, DirEntry, Progress};

/// adb binary from settings ("adb" = search PATH).
pub fn bin() -> String {
    let b = settings::Settings::load().adb;
    if b.trim().is_empty() { "adb".into() } else { b }
}

/// True when `bin` can be run.
pub fn found(bin: &str) -> bool {
    if bin.contains(['/', '\\']) {
        return std::fs::metadata(bin).is_ok_and(|m| m.is_file());
    }
    crate::term::found(bin)
}

/// Device to open; an empty serial means "the only device".
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct AdbLaunch {
    pub serial: String,
    /// Logcat view instead of a shell.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub logcat: bool,
}

/// `-s <serial>` when a serial is given.
pub fn select(serial: &str) -> Vec<String> {
    if serial.is_empty() { Vec::new() } else { vec!["-s".into(), serial.into()] }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Device {
    pub serial: String,
    /// device, unauthorized, offline, recovery, ...
    pub state: String,
    pub model: Option<String>,
}

impl Device {
    pub fn ready(&self) -> bool {
        self.state == "device"
    }

    /// Model with underscores shown as spaces, else the serial.
    pub fn name(&self) -> String {
        self.model.as_ref().map(|m| m.replace('_', " ")).unwrap_or_else(|| self.serial.clone())
    }
}

/// Parses `adb devices -l` output or a track-devices payload.
pub fn parse_devices(text: &str) -> Vec<Device> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with("List of devices") && !l.starts_with('*'))
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let serial = it.next()?.to_string();
            let state = it.next()?.to_string();
            let model = it.find_map(|kv| kv.strip_prefix("model:")).map(str::to_string);
            Some(Device { serial, state, model })
        })
        .collect()
}

/// Follows the device list with `adb track-devices -l`. Updates go to
/// `send`; adb is restarted when it ends (e.g. the server was killed).
/// Dropping the tracker stops it.
pub struct Tracker {
    stop: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
}

pub enum Track {
    Devices(Vec<Device>),
    /// adb failed to start or ended; retried in 2 s.
    Error(String),
}

impl Tracker {
    pub fn start(bin: String, send: async_channel::Sender<Track>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let child: Arc<Mutex<Option<Child>>> = Arc::default();
        let (s, c) = (stop.clone(), child.clone());
        thread::spawn(move || {
            while !s.load(Ordering::Acquire) {
                let spawned = crate::sys::spawn_tied(
                    Command::new(&bin).args(["track-devices", "-l"]).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()),
                );
                let mut ch = match spawned {
                    Ok(ch) => ch,
                    Err(e) => {
                        let _ = send.send_blocking(Track::Error(format!("{bin}: {e}")));
                        thread::sleep(Duration::from_secs(2));
                        continue;
                    }
                };
                let out = ch.stdout.take();
                let err = ch.stderr.take();
                if let Ok(mut g) = c.lock() {
                    *g = Some(ch);
                }
                if let Some(mut out) = out {
                    while let Some(payload) = frame(&mut out) {
                        if send.send_blocking(Track::Devices(parse_devices(&payload))).is_err() {
                            return;
                        }
                    }
                }
                let mut msg = String::new();
                if let Some(mut e) = err {
                    let _ = e.read_to_string(&mut msg);
                }
                if let Some(mut ch) = c.lock().ok().and_then(|mut g| g.take()) {
                    let _ = ch.kill();
                    let _ = ch.wait();
                }
                if s.load(Ordering::Acquire) {
                    break;
                }
                let msg = msg.trim();
                let why = if msg.is_empty() { "adb ended".to_string() } else { msg.to_string() };
                if send.send_blocking(Track::Error(why)).is_err() {
                    return;
                }
                thread::sleep(Duration::from_secs(2));
            }
        });
        Self { stop, child }
    }
}

impl Drop for Tracker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(mut ch) = self.child.lock().ok().and_then(|mut g| g.take()) {
            let _ = ch.kill();
            let _ = ch.wait();
        }
    }
}

/// One track-devices message: 4 hex digits of length, then the text.
fn frame(r: &mut impl Read) -> Option<String> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len).ok()?;
    let n = usize::from_str_radix(std::str::from_utf8(&len).ok()?, 16).ok()?;
    let mut body = vec![0u8; n];
    r.read_exact(&mut body).ok()?;
    Some(String::from_utf8_lossy(&body).into_owned())
}

/// Single-quotes `s` for the device shell.
pub fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

const RC: &str = "__TV_RC__";
const STAT: &str = "%f/%s/%Y/%u/%g";

/// Parses "<hex mode>/<size>/<mtime>/<uid>/<gid>[/<path>]".
fn parse_stat(line: &str) -> Option<(Attrs, Option<&str>)> {
    let mut it = line.splitn(6, '/');
    let perm = u32::from_str_radix(it.next()?, 16).ok()?;
    let size = it.next()?.parse().ok()?;
    let mtime = it.next()?.parse().ok()?;
    let uid = it.next()?.parse().ok()?;
    let gid = it.next()?.parse().ok()?;
    let attrs = Attrs { size: Some(size), perm: Some(perm), mtime: Some(mtime), uid: Some(uid), gid: Some(gid) };
    Some((attrs, it.next()))
}

/// File access on one device.
pub struct Adb {
    bin: String,
    serial: String,
}

impl Adb {
    pub fn new(bin: &str, serial: &str) -> Self {
        Self { bin: bin.to_string(), serial: serial.to_string() }
    }

    fn command(&self, verb: &str, line: &str) -> Command {
        let mut c = Command::new(&self.bin);
        c.args(select(&self.serial)).arg(verb).arg(line);
        c
    }

    fn spawn_err(&self, e: io::Error) -> String {
        if e.kind() == io::ErrorKind::NotFound {
            format!("{} not found; install adb", self.bin)
        } else {
            format!("{}: {e}", self.bin)
        }
    }

    /// Runs `line` in the device shell. Returns its output; a non-zero
    /// exit becomes an error with the output as the message.
    fn run(&self, line: &str) -> Result<String, String> {
        // The status is printed, since old adb shells always exit 0.
        let full = format!("{line} 2>&1; echo {RC}$?");
        let out = self.command("shell", &full).stdin(Stdio::null()).output().map_err(|e| self.spawn_err(e))?;
        let text = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
        let Some(at) = text.rfind(RC) else {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(if err.is_empty() { "no answer from adb".into() } else { err });
        };
        let rc: i32 = text[at + RC.len()..].trim().parse().unwrap_or(-1);
        let body = text[..at].to_string();
        if rc != 0 {
            let msg = body.trim();
            return Err(if msg.is_empty() { format!("exit status {rc}") } else { msg.lines().last().unwrap_or(msg).to_string() });
        }
        Ok(body)
    }

    /// Process ids of a package or process name (`pidof`).
    pub fn pids(&self, name: &str) -> Vec<u32> {
        self.run(&format!("pidof {}", quote(name)))
            .map(|o| o.split_whitespace().filter_map(|p| p.parse().ok()).collect())
            .unwrap_or_default()
    }

    /// Clears the device log buffers (`logcat -c`).
    pub fn clear_log(&self) -> Result<(), String> {
        let mut c = Command::new(&self.bin);
        c.args(select(&self.serial)).args(["logcat", "-c"]);
        let out = c.output().map_err(|e| self.spawn_err(e))?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
}

/// Copies `from` to `to` with progress; false from `progress` stops it.
fn pump(from: &mut dyn Read, to: &mut dyn Write, mut done: u64, progress: Progress) -> io::Result<(u64, bool)> {
    let mut buf = vec![0u8; 64 * 1024];
    let mut moved = 0;
    loop {
        let n = match from.read(&mut buf) {
            Ok(0) => return Ok((moved, true)),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        to.write_all(&buf[..n])?;
        moved += n as u64;
        done += n as u64;
        if !progress(done) {
            return Ok((moved, false));
        }
    }
}

impl Remote for Adb {
    fn realpath(&mut self, path: &str) -> Result<String, String> {
        // "." is the user storage when there is one.
        let line = if path == "." {
            "cd '/sdcard' 2>/dev/null || cd '/'; pwd".to_string()
        } else {
            format!("cd {} && pwd", quote(path))
        };
        Ok(self.run(&line)?.trim().to_string())
    }

    fn list(&mut self, path: &str) -> Result<Vec<DirEntry>, String> {
        let dir = if path.ends_with('/') { path.to_string() } else { format!("{path}/") };
        let line = format!("find {} -mindepth 1 -maxdepth 1 -exec stat -c '{STAT}/%n' {{}} +", quote(&dir));
        // Unreadable entries make find fail; keep what it did list.
        let (text, failed) = match self.run(&line) {
            Ok(t) => (t, None),
            Err(e) => (String::new(), Some(e)),
        };
        let mut out: Vec<DirEntry> = text
            .lines()
            .filter_map(parse_stat)
            .filter_map(|(attrs, p)| {
                let name = p?.trim_end_matches('/').rsplit('/').next()?.to_string();
                (!name.is_empty()).then_some(DirEntry { name, attrs })
            })
            .collect();
        if out.is_empty() {
            if let Some(e) = failed {
                return Err(e);
            }
        }
        out.sort_by(|a, b| b.attrs.is_dir().cmp(&a.attrs.is_dir()).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
        Ok(out)
    }

    fn stat(&mut self, path: &str) -> Result<Attrs, String> {
        let text = self.run(&format!("stat -L -c '{STAT}' {}", quote(path)))?;
        text.lines().find_map(parse_stat).map(|(a, _)| a).ok_or_else(|| format!("{path}: cannot read attributes"))
    }

    fn mkdir(&mut self, path: &str) -> Result<(), String> {
        self.run(&format!("mkdir {}", quote(path))).map(|_| ())
    }

    fn rmdir(&mut self, path: &str) -> Result<(), String> {
        self.run(&format!("rmdir {}", quote(path))).map(|_| ())
    }

    fn remove(&mut self, path: &str) -> Result<(), String> {
        self.run(&format!("rm {}", quote(path))).map(|_| ())
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        self.run(&format!("mv {} {}", quote(from), quote(to))).map(|_| ())
    }

    fn chmod(&mut self, path: &str, mode: u32) -> Result<(), String> {
        self.run(&format!("chmod {:o} {}", mode & 0o7777, quote(path))).map(|_| ())
    }

    fn download(&mut self, path: &str, start: u64, out: &mut dyn Write, progress: Progress) -> Result<u64, String> {
        let size = self.stat(path)?.size.unwrap_or(0);
        let line = if start == 0 { format!("cat {}", quote(path)) } else { format!("tail -c +{} {}", start + 1, quote(path)) };
        let mut ch = self
            .command("exec-out", &line)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| self.spawn_err(e))?;
        let mut pipe = ch.stdout.take().ok_or("no output pipe")?;
        let res = pump(&mut pipe, out, start, progress);
        if !matches!(res, Ok((_, true))) {
            let _ = ch.kill();
        }
        let _ = ch.wait();
        match res {
            Ok((n, true)) if start + n >= size => Ok(n),
            Ok((n, true)) => Err(format!("short read: {} of {size} bytes", start + n)),
            Ok((_, false)) => Err("cancelled".into()),
            Err(e) => Err(e.to_string()),
        }
    }

    fn upload(&mut self, input: &mut dyn Read, path: &str, mode: u32, start: u64, progress: Progress) -> Result<u64, String> {
        let op = if start == 0 { ">" } else { ">>" };
        let mut ch = self
            .command("exec-in", &format!("cat {op} {}", quote(path)))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| self.spawn_err(e))?;
        let mut pipe = ch.stdin.take().ok_or("no input pipe")?;
        let res = pump(input, &mut pipe, start, progress);
        drop(pipe);
        if !matches!(res, Ok((_, true))) {
            let _ = ch.kill();
        }
        let status = ch.wait().map_err(|e| e.to_string())?;
        let (n, done) = res.map_err(|e| e.to_string())?;
        if !done {
            return Err("cancelled".into());
        }
        if !status.success() {
            let mut msg = String::new();
            if let Some(mut e) = ch.stderr.take() {
                let _ = e.read_to_string(&mut msg);
            }
            return Err(if msg.trim().is_empty() { format!("upload failed ({status})") } else { msg.trim().to_string() });
        }
        if start == 0 {
            self.chmod(path, mode)?;
        }
        let got = self.stat(path)?.size.unwrap_or(0);
        if got != start + n {
            return Err(format!("{path}: {got} bytes on the device, {} sent", start + n));
        }
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn device_lines() {
        let text = "List of devices attached\n\
            R58M123ABC             device usb:1-3 product:a51 model:SM_A515F device:a51 transport_id:4\n\
            emulator-5554          offline transport_id:2\n\
            * daemon started successfully\n\n";
        let d = parse_devices(text);
        assert_eq!(d.len(), 2);
        assert_eq!(d[0].serial, "R58M123ABC");
        assert!(d[0].ready());
        assert_eq!(d[0].name(), "SM A515F");
        assert_eq!(d[1].state, "offline");
        assert_eq!(d[1].name(), "emulator-5554");
    }

    #[test]
    fn frames_and_quotes() {
        let mut r: &[u8] = b"000aABC\tdevice0000";
        assert_eq!(frame(&mut r).as_deref(), Some("ABC\tdevice"));
        assert_eq!(frame(&mut r).as_deref(), Some(""));
        assert_eq!(frame(&mut r), None);
        assert_eq!(quote("it's"), r"'it'\''s'");
        let (a, p) = parse_stat("41f9/3452/1700000000/1000/1000//sdcard/My Dir").expect("stat");
        assert!(a.is_dir());
        assert_eq!(p, Some("/sdcard/My Dir"));
    }

    /// Fake adb from tests/fixtures. Never the real adb.
    fn fake_bin() -> Option<String> {
        if !crate::term::found("python3") || !crate::term::found("bash") {
            return None;
        }
        let bin = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-adb");
        bin.to_str().map(str::to_string)
    }

    #[test]
    fn files_on_fake_device() {
        let Some(bin) = fake_bin() else { return };
        let root = std::env::temp_dir().join(format!("tv-adb-{}", std::process::id()));
        std::fs::create_dir_all(root.join("sdcard")).expect("root");
        // Only this test uses the device root.
        std::env::set_var("FAKE_ADB_ROOT", &root);
        let mut a = Adb::new(&bin, "FAKE001");
        assert_eq!(a.realpath(".").as_deref(), Ok("/sdcard"));
        a.mkdir("/sdcard/My Dir").expect("mkdir");
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let mut seen = 0;
        let n = a.upload(&mut &data[..], "/sdcard/My Dir/b.bin", 0o640, 0, &mut |d| {
            seen = d;
            true
        });
        assert_eq!(n, Ok(200_000));
        assert_eq!(seen, 200_000);
        let list = a.list("/sdcard").expect("list");
        assert_eq!(list[0].name, "My Dir");
        assert!(list[0].attrs.is_dir());
        let inner = a.list("/sdcard/My Dir").expect("list");
        assert_eq!(inner[0].name, "b.bin");
        assert_eq!(inner[0].attrs.size, Some(200_000));
        assert_eq!(inner[0].attrs.perm.map(|p| p & 0o777), Some(0o640));

        let mut back = Vec::new();
        assert_eq!(a.download("/sdcard/My Dir/b.bin", 0, &mut back, &mut |_| true), Ok(200_000));
        assert_eq!(back, data);
        // Resume from the middle.
        let mut tail = Vec::new();
        assert_eq!(a.download("/sdcard/My Dir/b.bin", 150_000, &mut tail, &mut |_| true), Ok(50_000));
        assert_eq!(tail, &data[150_000..]);
        // Upload resume appends.
        a.upload(&mut &data[..100], "/sdcard/c.bin", 0o644, 0, &mut |_| true).expect("up");
        a.upload(&mut &data[100..300], "/sdcard/c.bin", 0o644, 100, &mut |_| true).expect("append");
        assert_eq!(a.stat("/sdcard/c.bin").expect("stat").size, Some(300));

        a.rename("/sdcard/c.bin", "/sdcard/d.bin").expect("mv");
        assert!(a.stat("/sdcard/c.bin").is_err());
        a.remove_all("/sdcard/My Dir").expect("rm -r");
        assert!(a.list("/sdcard/missing").is_err());
        assert!(Adb::new(a.bin.as_str(), "FAKE002").list("/sdcard").is_err());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn tracker_on_fake() {
        let Some(bin) = fake_bin() else { return };
        let (tx, rx) = async_channel::unbounded();
        let t = Tracker::start(bin, tx);
        let first = match rx.recv_blocking() {
            Ok(Track::Devices(d)) => d,
            _ => panic!("no device list"),
        };
        assert_eq!(first.iter().map(|d| d.serial.as_str()).collect::<Vec<_>>(), ["FAKE001", "FAKE002"]);
        assert_eq!(first[0].name(), "Pixel Fake");
        let second = match rx.recv_blocking() {
            Ok(Track::Devices(d)) => d,
            _ => panic!("no update"),
        };
        assert_eq!(second.len(), 3);
        drop(t);
    }
}
