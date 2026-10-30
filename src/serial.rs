// Serial port core: settings, port list, UUCP locks, I/O threads, hex view.
//
// Each open port runs a reader and a writer thread. The GTK side talks to
// them through channels only, so a stuck port never blocks the UI.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serialport::SerialPort;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Parity {
    #[default]
    None,
    Odd,
    Even,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Flow {
    #[default]
    None,
    Software,
    Hardware,
}

/// Bytes sent for Enter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Eol {
    #[default]
    Cr,
    Lf,
    Crlf,
}

impl Eol {
    pub const ALL: [Eol; 3] = [Eol::Cr, Eol::Lf, Eol::Crlf];

    pub fn label(self) -> &'static str {
        match self {
            Eol::Cr => "CR",
            Eol::Lf => "LF",
            Eol::Crlf => "CRLF",
        }
    }

    fn bytes(self) -> &'static [u8] {
        match self {
            Eol::Cr => b"\r",
            Eol::Lf => b"\n",
            Eol::Crlf => b"\r\n",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SerialCfg {
    pub device: String,
    pub baud: u32,
    pub data_bits: u8,
    pub parity: Parity,
    pub stop_bits: u8,
    pub flow: Flow,
    pub eol: Eol,
    pub echo: bool,
    pub hex: bool,
    pub reconnect: bool,
}

impl Default for SerialCfg {
    fn default() -> Self {
        Self {
            device: String::new(),
            baud: 115_200,
            data_bits: 8,
            parity: Parity::None,
            stop_bits: 1,
            flow: Flow::None,
            eol: Eol::Cr,
            echo: false,
            hex: false,
            reconnect: true,
        }
    }
}

impl SerialCfg {
    /// e.g. "115200 8N1".
    pub fn summary(&self) -> String {
        let p = match self.parity {
            Parity::None => 'N',
            Parity::Odd => 'O',
            Parity::Even => 'E',
        };
        format!("{} {}{}{}", self.baud, self.data_bits, p, self.stop_bits)
    }

    pub fn check(&self) -> Result<(), String> {
        if !self.device.starts_with("/dev/") {
            return Err(format!("device must be under /dev, got '{}'", self.device));
        }
        if self.baud == 0 {
            return Err("baud rate must be above 0".into());
        }
        if !(5..=8).contains(&self.data_bits) {
            return Err("data bits must be 5-8".into());
        }
        if !(1..=2).contains(&self.stop_bits) {
            return Err("stop bits must be 1 or 2".into());
        }
        Ok(())
    }
}

/// Maps typed text to wire bytes: Enter (CR) becomes the configured EOL.
pub fn outgoing(text: &str, eol: Eol) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 1);
    for b in text.bytes() {
        if b == b'\r' {
            out.extend_from_slice(eol.bytes());
        } else {
            out.push(b);
        }
    }
    out
}

/// Local echo text: CR shown as a new line.
pub fn echo(text: &str) -> String {
    text.replace('\r', "\r\n")
}

/// Hex dump formatter, 16 bytes per line, keeps the column across chunks.
#[derive(Default)]
pub struct Hex {
    col: usize,
}

impl Hex {
    pub fn feed(&mut self, data: &[u8]) -> String {
        let mut s = String::with_capacity(data.len() * 3 + 4);
        for b in data {
            s.push_str(&format!("{b:02X} "));
            self.col += 1;
            if self.col == 16 {
                s.push_str("\r\n");
                self.col = 0;
            }
        }
        s
    }

    pub fn reset(&mut self) {
        self.col = 0;
    }
}

pub struct PortInfo {
    pub path: String,
    pub label: String,
}

/// Serial ports on the system, USB ones with their product name.
pub fn ports() -> Vec<PortInfo> {
    let mut list: Vec<PortInfo> = serialport::available_ports()
        .unwrap_or_default()
        .into_iter()
        .map(|p| {
            let name = p.port_name.rsplit('/').next().unwrap_or(&p.port_name).to_string();
            let label = match &p.port_type {
                serialport::SerialPortType::UsbPort(u) => {
                    let what = u.product.clone().or(u.manufacturer.clone()).unwrap_or_else(|| "USB".into());
                    format!("{name} · {what} ({:04x}:{:04x})", u.vid, u.pid)
                }
                _ => name,
            };
            PortInfo { path: p.port_name, label }
        })
        .collect();
    list.sort_by(|a, b| a.path.cmp(&b.path));
    list
}

/// Processes that have `device` open (only those we may inspect).
pub fn holders(device: &Path) -> Vec<(u32, String)> {
    let Ok(target) = fs::canonicalize(device) else {
        return Vec::new();
    };
    let me = std::process::id();
    let mut out = Vec::new();
    let Ok(procs) = fs::read_dir("/proc") else {
        return out;
    };
    for p in procs.flatten() {
        let Some(pid) = p.file_name().to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        if pid == me {
            continue;
        }
        let Ok(fds) = fs::read_dir(p.path().join("fd")) else {
            continue;
        };
        if fds.flatten().any(|fd| fs::read_link(fd.path()).is_ok_and(|l| l == target)) {
            out.push((pid, comm(pid)));
        }
    }
    out
}

fn comm(pid: u32) -> String {
    fs::read_to_string(format!("/proc/{pid}/comm"))
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "?".into())
}

fn alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

const LOCK_DIR: &str = "/run/lock";

fn lock_file(dir: &Path, device: &str) -> PathBuf {
    let base = device.rsplit('/').next().unwrap_or(device);
    dir.join(format!("LCK..{base}"))
}

/// Owner of a live UUCP lock for `device`, if any.
fn lock_owner(path: &Path) -> Option<u32> {
    let pid = fs::read_to_string(path).ok()?.trim().parse::<u32>().ok()?;
    alive(pid).then_some(pid)
}

/// Takes the UUCP lock (HDB format). Ok(None) when the lock dir is not
/// writable; Err when another live process holds it.
fn lock(dir: &Path, device: &str) -> Result<Option<PathBuf>, String> {
    let path = lock_file(dir, device);
    let me = std::process::id();
    match lock_owner(&path) {
        Some(pid) if pid != me => {
            return Err(format!("{device} is locked by {} (PID {pid})", comm(pid)));
        }
        Some(_) => return Ok(Some(path)),
        None => {
            // Stale or missing; a stale one is safe to remove.
            let _ = fs::remove_file(&path);
        }
    }
    let file = OpenOptions::new().write(true).create_new(true).open(&path);
    match file {
        Ok(mut f) => {
            f.write_all(format!("{me:>10}\n").as_bytes()).map_err(|e| e.to_string())?;
            Ok(Some(path))
        }
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied || e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("lock {}: {e}", path.display())),
    }
}

fn unlock(path: &Path) {
    if lock_owner(path) == Some(std::process::id()) {
        let _ = fs::remove_file(path);
    }
}

/// Group that grants serial access on this system.
fn serial_group() -> &'static str {
    let groups = fs::read_to_string("/etc/group").unwrap_or_default();
    if groups.lines().any(|l| l.starts_with("dialout:")) {
        "dialout"
    } else {
        "uucp"
    }
}

/// Why an open failed, phrased for the user.
fn explain(cfg: &SerialCfg, e: &serialport::Error) -> String {
    use serialport::ErrorKind as K;
    let dev = &cfg.device;
    match e.kind() {
        K::Io(io::ErrorKind::PermissionDenied) => format!(
            "permission denied on {dev}. Add yourself to the '{g}' group: \
             sudo usermod -aG {g} $USER  (then log out and in)",
            g = serial_group()
        ),
        K::Io(io::ErrorKind::NotFound) => format!("{dev} not found"),
        K::NoDevice | K::Io(_) => {
            let who = holders(Path::new(dev));
            if who.is_empty() {
                format!("{dev}: {e}")
            } else {
                let list: Vec<String> = who.iter().map(|(p, n)| format!("{n} (PID {p})")).collect();
                format!("{dev} is in use by {}", list.join(", "))
            }
        }
        _ => format!("{dev}: {e}"),
    }
}

pub enum Event {
    Data(Vec<u8>),
    Closed(String),
}

enum Cmd {
    Write(Vec<u8>),
    Break,
    Dtr(bool),
    Rts(bool),
}

/// An open port. Dropping it stops both threads and releases the lock.
pub struct Conn {
    cmds: mpsc::Sender<Cmd>,
    stop: Arc<AtomicBool>,
    lock: Option<PathBuf>,
}

impl Conn {
    pub fn write(&self, data: Vec<u8>) {
        let _ = self.cmds.send(Cmd::Write(data));
    }

    pub fn send_break(&self) {
        let _ = self.cmds.send(Cmd::Break);
    }

    pub fn dtr(&self, on: bool) {
        let _ = self.cmds.send(Cmd::Dtr(on));
    }

    pub fn rts(&self, on: bool) {
        let _ = self.cmds.send(Cmd::Rts(on));
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(p) = &self.lock {
            unlock(p);
        }
    }
}

/// Opens the port and starts its threads. Events arrive on the receiver.
pub fn open(cfg: &SerialCfg) -> Result<(Conn, async_channel::Receiver<Event>), String> {
    cfg.check()?;
    let lock = lock(Path::new(LOCK_DIR), &cfg.device)?;
    let opened = serialport::new(&cfg.device, cfg.baud)
        .data_bits(match cfg.data_bits {
            5 => serialport::DataBits::Five,
            6 => serialport::DataBits::Six,
            7 => serialport::DataBits::Seven,
            _ => serialport::DataBits::Eight,
        })
        .parity(match cfg.parity {
            Parity::None => serialport::Parity::None,
            Parity::Odd => serialport::Parity::Odd,
            Parity::Even => serialport::Parity::Even,
        })
        .stop_bits(if cfg.stop_bits == 2 { serialport::StopBits::Two } else { serialport::StopBits::One })
        .flow_control(match cfg.flow {
            Flow::None => serialport::FlowControl::None,
            Flow::Software => serialport::FlowControl::Software,
            Flow::Hardware => serialport::FlowControl::Hardware,
        })
        .timeout(Duration::from_millis(100))
        .open_native();
    let port = match opened {
        Ok(p) => p,
        Err(e) => {
            if let Some(l) = &lock {
                unlock(l);
            }
            return Err(explain(cfg, &e));
        }
    };
    let reader = port.try_clone_native().map_err(|e| explain(cfg, &e))?;

    let (ev_tx, ev_rx) = async_channel::unbounded();
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));

    let (tx, halt) = (ev_tx.clone(), stop.clone());
    thread::Builder::new()
        .name("serial-rx".into())
        .spawn(move || read_loop(reader, tx, halt))
        .map_err(|e| e.to_string())?;
    let halt = stop.clone();
    thread::Builder::new()
        .name("serial-tx".into())
        .spawn(move || write_loop(port, cmd_rx, ev_tx, halt))
        .map_err(|e| e.to_string())?;

    Ok((Conn { cmds: cmd_tx, stop, lock }, ev_rx))
}

fn read_loop(mut port: serialport::TTYPort, tx: async_channel::Sender<Event>, stop: Arc<AtomicBool>) {
    let mut buf = [0u8; 4096];
    while !stop.load(Ordering::Relaxed) {
        match port.read(&mut buf) {
            Ok(0) => {
                let _ = tx.send_blocking(Event::Closed("device disconnected".into()));
                return;
            }
            Ok(n) => {
                if tx.send_blocking(Event::Data(buf[..n].to_vec())).is_err() {
                    return;
                }
            }
            Err(e) if e.kind() == io::ErrorKind::TimedOut || e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => {
                let _ = tx.send_blocking(Event::Closed(e.to_string()));
                return;
            }
        }
    }
}

fn write_loop(
    mut port: serialport::TTYPort,
    cmds: mpsc::Receiver<Cmd>,
    tx: async_channel::Sender<Event>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Relaxed) {
        let cmd = match cmds.recv_timeout(Duration::from_millis(200)) {
            Ok(c) => c,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let res = match cmd {
            Cmd::Write(d) => port.write_all(&d).map_err(|e| e.to_string()),
            Cmd::Break => port
                .set_break()
                .and_then(|_| {
                    thread::sleep(Duration::from_millis(250));
                    port.clear_break()
                })
                .map_err(|e| e.to_string()),
            Cmd::Dtr(on) => port.write_data_terminal_ready(on).map_err(|e| e.to_string()),
            Cmd::Rts(on) => port.write_request_to_send(on).map_err(|e| e.to_string()),
        };
        if let Err(e) = res {
            let _ = tx.send_blocking(Event::Closed(e));
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eol_mapping() {
        assert_eq!(outgoing("ls\r", Eol::Cr), b"ls\r");
        assert_eq!(outgoing("ls\r", Eol::Lf), b"ls\n");
        assert_eq!(outgoing("ls\r", Eol::Crlf), b"ls\r\n");
        assert_eq!(echo("a\rb"), "a\r\nb");
    }

    #[test]
    fn hex_wraps_across_chunks() {
        let mut h = Hex::default();
        let a = h.feed(&[0u8; 10]);
        let b = h.feed(&[0xAB; 10]);
        assert!(!a.contains('\n'));
        assert_eq!(b.matches("\r\n").count(), 1);
        assert!(b.starts_with("AB AB AB AB AB AB \r\n"));
    }

    #[test]
    fn summary_and_check() {
        let mut c = SerialCfg { device: "/dev/ttyUSB0".into(), ..Default::default() };
        assert_eq!(c.summary(), "115200 8N1");
        assert!(c.check().is_ok());
        c.data_bits = 9;
        assert!(c.check().is_err());
        c.data_bits = 8;
        c.device = "ttyUSB0".into();
        assert!(c.check().is_err());
    }

    #[test]
    fn uucp_lock_cycle() {
        let dir = std::env::temp_dir().join(format!("tv-lock-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("dir");
        let path = lock(&dir, "/dev/ttyFAKE0").expect("lock").expect("path");
        let text = fs::read_to_string(&path).expect("read");
        assert_eq!(text.len(), 11);
        assert_eq!(text.trim().parse::<u32>().ok(), Some(std::process::id()));
        // Our own lock is reusable.
        assert!(lock(&dir, "/dev/ttyFAKE0").is_ok());
        unlock(&path);
        assert!(!path.exists());

        // A stale lock (dead PID) is replaced.
        fs::write(&path, format!("{:>10}\n", u32::MAX - 1)).expect("stale");
        assert!(lock(&dir, "/dev/ttyFAKE0").expect("lock").is_some());
        unlock(&path);

        // A live foreign PID (init) blocks.
        fs::write(&path, format!("{:>10}\n", 1)).expect("foreign");
        assert!(lock(&dir, "/dev/ttyFAKE0").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    /// Full open/read/write/close over a pseudo-terminal pair.
    #[test]
    fn pty_roundtrip() {
        let (mut master, slave) = serialport::TTYPort::pair().expect("pty pair");
        let device = slave.name().expect("slave name");
        drop(slave);
        let cfg = SerialCfg { device: device.clone(), ..Default::default() };
        let (conn, rx) = open(&cfg).expect("open");

        master.write_all(b"hello").expect("master write");
        let mut got = Vec::new();
        while got.len() < 5 {
            match rx.recv_blocking().expect("event") {
                Event::Data(d) => got.extend(d),
                Event::Closed(e) => panic!("closed: {e}"),
            }
        }
        assert_eq!(got, b"hello");

        conn.write(outgoing("ls\r", Eol::Crlf));
        master.set_timeout(Duration::from_secs(2)).expect("timeout");
        let mut buf = [0u8; 4];
        master.read_exact(&mut buf).expect("master read");
        assert_eq!(&buf, b"ls\r\n");

        // A second open of the same device is refused (exclusive).
        assert!(open(&cfg).is_err());
        drop(conn);
        thread::sleep(Duration::from_millis(300));
        let again = open(&cfg);
        assert!(again.is_ok(), "reopen after close: {:?}", again.err());
    }

    #[test]
    fn cfg_toml_roundtrip() {
        let c = SerialCfg { device: "/dev/ttyS4".into(), parity: Parity::Even, eol: Eol::Crlf, ..Default::default() };
        let text = toml::to_string(&c).expect("ser");
        assert!(text.contains("parity = \"even\""));
        assert_eq!(toml::from_str::<SerialCfg>(&text).expect("de"), c);
    }
}
