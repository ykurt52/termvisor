// Network tools without root: TCP port scanner, ping (system ping) and
// traceroute (tracepath) with parsed output, and Wake-on-LAN.

use std::io::{BufRead, BufReader, Read};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpStream, ToSocketAddrs, UdpSocket};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};


/// Hosts at most in one scan.
const MAX_HOSTS: usize = 4096;
const WORKERS: usize = 256;

pub const COMMON: &[(u16, &str)] = &[
    (21, "ftp"),
    (22, "ssh"),
    (23, "telnet"),
    (25, "smtp"),
    (53, "dns"),
    (69, "tftp"),
    (80, "http"),
    (110, "pop3"),
    (111, "rpcbind"),
    (139, "netbios"),
    (143, "imap"),
    (161, "snmp"),
    (443, "https"),
    (445, "smb"),
    (502, "modbus"),
    (554, "rtsp"),
    (993, "imaps"),
    (1883, "mqtt"),
    (2049, "nfs"),
    (2323, "telnet-alt"),
    (3306, "mysql"),
    (3389, "rdp"),
    (4840, "opc-ua"),
    (5000, "upnp"),
    (5432, "postgres"),
    (5555, "adb"),
    (5760, "mavlink"),
    (5900, "vnc"),
    (6379, "redis"),
    (8000, "http-alt"),
    (8080, "http-alt"),
    (8443, "https-alt"),
    (8883, "mqtts"),
    (9100, "printer"),
    (20000, "dnp3"),
    (44818, "ethernet/ip"),
    (47808, "bacnet"),
];

pub fn service(port: u16) -> &'static str {
    COMMON.iter().find(|(p, _)| *p == port).map(|(_, n)| *n).unwrap_or("")
}

/// "10.0.0.5", "host.name", "192.168.1.0/24" or "192.168.1.10-20".
pub fn hosts(spec: &str) -> Result<Vec<IpAddr>, String> {
    let s = spec.trim();
    if s.is_empty() {
        return Err("enter a host".into());
    }
    if let Some((net, bits)) = s.split_once('/') {
        let base: Ipv4Addr = net.parse().map_err(|_| format!("'{net}' is not an IPv4 address"))?;
        let bits: u32 = bits.parse().ok().filter(|b| *b <= 32).ok_or("prefix must be 0..32")?;
        let size = 1u64 << (32 - bits);
        if size as usize > MAX_HOSTS * 2 {
            return Err(format!("/{bits} is too large; at most {MAX_HOSTS} hosts"));
        }
        let start = u32::from(base) & !((size as u32).wrapping_sub(1));
        // Leave out network and broadcast addresses of real subnets.
        let (first, last) = if size > 2 { (1, size - 1) } else { (0, size) };
        return Ok((first..last).map(|i| IpAddr::V4(Ipv4Addr::from(start + i as u32))).collect());
    }
    if let Some((a, b)) = s.rsplit_once('-') {
        if let (Ok(start), Ok(end)) = (a.parse::<Ipv4Addr>(), b.parse::<u8>()) {
            let o = start.octets();
            if end < o[3] {
                return Err("range end is below its start".into());
            }
            return Ok((o[3]..=end).map(|x| IpAddr::V4(Ipv4Addr::new(o[0], o[1], o[2], x))).collect());
        }
    }
    if let Ok(ip) = s.parse::<IpAddr>() {
        return Ok(vec![ip]);
    }
    let found: Vec<IpAddr> = (s, 0).to_socket_addrs().map_err(|e| format!("{s}: {e}"))?.map(|a| a.ip()).collect();
    found.first().map(|ip| vec![*ip]).ok_or_else(|| format!("{s}: no address"))
}

/// "22,80,8000-8100" or "common".
pub fn ports(spec: &str) -> Result<Vec<u16>, String> {
    let s = spec.trim();
    if s.is_empty() || s.eq_ignore_ascii_case("common") {
        return Ok(COMMON.iter().map(|(p, _)| *p).collect());
    }
    let mut out = Vec::new();
    for part in s.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let bad = || format!("'{part}' is not a port or range");
        match part.split_once('-') {
            Some((a, b)) => {
                let (a, b): (u16, u16) = (a.trim().parse().map_err(|_| bad())?, b.trim().parse().map_err(|_| bad())?);
                if a == 0 || b < a {
                    return Err(bad());
                }
                out.extend(a..=b);
            }
            None => out.push(part.parse().ok().filter(|p| *p > 0).ok_or_else(bad)?),
        }
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

#[derive(Clone, Debug, PartialEq)]
pub enum Scan {
    Open { addr: SocketAddr, banner: String },
    Progress { done: usize, total: usize },
    Done { open: usize, total: usize, secs: f64 },
}

/// First bytes a service sends by itself (SSH, FTP, SMTP, …).
fn banner(s: &mut TcpStream) -> String {
    let _ = s.set_read_timeout(Some(Duration::from_millis(800)));
    let mut buf = [0u8; 160];
    let n = s.read(&mut buf).unwrap_or(0);
    String::from_utf8_lossy(&buf[..n]).lines().next().unwrap_or("").chars().filter(|c| !c.is_control()).collect()
}

/// TCP connect scan of every host and port; `stop` ends it early.
pub fn scan(hosts: Vec<IpAddr>, ports: Vec<u16>, timeout: Duration, grab: bool, stop: Arc<AtomicBool>, tx: async_channel::Sender<Scan>) {
    let total = hosts.len() * ports.len();
    let next = Arc::new(AtomicUsize::new(0));
    let done = Arc::new(AtomicUsize::new(0));
    let open = Arc::new(AtomicUsize::new(0));
    let started = Instant::now();
    let jobs = Arc::new((hosts, ports));
    let workers = WORKERS.min(total.max(1));
    let left = Arc::new(Mutex::new(workers));
    for _ in 0..workers {
        let (next, done, open, stop, tx, jobs, left) = (next.clone(), done.clone(), open.clone(), stop.clone(), tx.clone(), jobs.clone(), left.clone());
        thread::spawn(move || {
            let (hosts, ports) = &*jobs;
            loop {
                let i = next.fetch_add(1, Ordering::AcqRel);
                if i >= total || stop.load(Ordering::Acquire) {
                    break;
                }
                let addr = SocketAddr::new(hosts[i / ports.len()], ports[i % ports.len()]);
                if let Ok(mut s) = TcpStream::connect_timeout(&addr, timeout) {
                    open.fetch_add(1, Ordering::AcqRel);
                    let b = if grab { banner(&mut s) } else { String::new() };
                    let _ = tx.send_blocking(Scan::Open { addr, banner: b });
                }
                let d = done.fetch_add(1, Ordering::AcqRel) + 1;
                if d % 64 == 0 {
                    let _ = tx.send_blocking(Scan::Progress { done: d, total });
                }
            }
            let mut l = left.lock().unwrap_or_else(|e| e.into_inner());
            *l -= 1;
            if *l == 0 {
                let _ = tx.send_blocking(Scan::Done { open: open.load(Ordering::Acquire), total: done.load(Ordering::Acquire), secs: started.elapsed().as_secs_f64() });
            }
        });
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum Ping {
    Reply { seq: u32, ttl: u32, ms: f64 },
    Lost { seq: u32 },
    /// Any other line (headers, errors, summary).
    Text(String),
    Ended,
}

#[cfg(unix)]
fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace().find_map(|w| w.strip_prefix(key))
}

/// A line of `LC_ALL=C ping -n -O`.
#[cfg(unix)]
pub fn parse_ping(line: &str) -> Ping {
    let l = line.trim_end();
    if l.contains(" bytes from ") {
        let seq = field(l, "icmp_seq=").and_then(|v| v.parse().ok());
        let ttl = field(l, "ttl=").and_then(|v| v.parse().ok()).unwrap_or(0);
        let ms = field(l, "time=").and_then(|v| v.parse().ok());
        if let (Some(seq), Some(ms)) = (seq, ms) {
            return Ping::Reply { seq, ttl, ms };
        }
    }
    if let Some(rest) = l.strip_prefix("no answer yet for icmp_seq=") {
        if let Ok(seq) = rest.trim().parse() {
            return Ping::Lost { seq };
        }
    }
    Ping::Text(l.to_string())
}

/// Runs `prog args` with C locale; each output line goes through `parse`.
fn lines<T: Send + 'static>(prog: &str, args: &[String], parse: fn(&str) -> T, end: T, stop: Arc<AtomicBool>, tx: async_channel::Sender<T>) -> Result<(), String> {
    let mut ch = crate::sys::spawn_tied(
        Command::new(prog).args(args).env("LC_ALL", "C").stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()),
    )
    .map_err(|e| format!("{prog}: {e}"))?;
    let out = ch.stdout.take();
    let err = ch.stderr.take();
    let child = Arc::new(Mutex::new(ch));
    let c = child.clone();
    // Stop: kill the program; its pipes then close.
    thread::spawn(move || {
        while !stop.load(Ordering::Acquire) {
            if c.lock().map(|mut g| g.try_wait().ok().flatten().is_some()).unwrap_or(true) {
                return;
            }
            thread::sleep(Duration::from_millis(100));
        }
        if let Ok(mut g) = c.lock() {
            let _ = g.kill();
        }
    });
    let etx = tx.clone();
    if let Some(e) = err {
        thread::spawn(move || {
            for l in BufReader::new(e).lines().map_while(Result::ok) {
                let _ = etx.send_blocking(parse(&l));
            }
        });
    }
    thread::spawn(move || {
        if let Some(o) = out {
            for l in BufReader::new(o).lines().map_while(Result::ok) {
                if tx.send_blocking(parse(&l)).is_err() {
                    break;
                }
            }
        }
        if let Ok(mut g) = child.lock() {
            let _ = g.wait();
        }
        let _ = tx.send_blocking(end);
    });
    Ok(())
}

/// `ping` every `interval` seconds until stopped.
#[cfg(unix)]
pub fn ping(host: &str, interval: f64, stop: Arc<AtomicBool>, tx: async_channel::Sender<Ping>) -> Result<(), String> {
    let args = vec!["-n".into(), "-O".into(), "-i".into(), format!("{interval:.1}"), "--".into(), host.to_string()];
    lines("ping", &args, parse_ping, Ping::Ended, stop, tx)
}

/// Echo requests through the ICMP API: the ping program's output depends
/// on the system language.
#[cfg(windows)]
pub fn ping(host: &str, interval: f64, stop: Arc<AtomicBool>, tx: async_channel::Sender<Ping>) -> Result<(), String> {
    use std::net::ToSocketAddrs;
    use windows_sys::Win32::NetworkManagement::IpHelper::{IcmpCloseHandle, IcmpCreateFile, IcmpSendEcho, ICMP_ECHO_REPLY};
    let ip = (host, 0)
        .to_socket_addrs()
        .map_err(|e| format!("{host}: {e}"))?
        .find_map(|a| match a.ip() {
            IpAddr::V4(v) => Some(v),
            IpAddr::V6(_) => None,
        })
        .ok_or_else(|| format!("{host}: no IPv4 address"))?;
    let _ = tx.send_blocking(Ping::Text(format!("PING {host} ({ip})")));
    thread::spawn(move || {
        // SAFETY: one handle for this thread, closed at the end; buffers outlive each call.
        unsafe {
            let h = IcmpCreateFile();
            let data = [0x61u8; 32];
            let mut reply = vec![0u8; std::mem::size_of::<ICMP_ECHO_REPLY>() + data.len() + 8];
            let mut seq = 0u32;
            while !stop.load(Ordering::Acquire) {
                seq += 1;
                let dest = u32::from_ne_bytes(ip.octets());
                let n = IcmpSendEcho(h, dest, data.as_ptr() as *const _, data.len() as u16, std::ptr::null(), reply.as_mut_ptr() as *mut _, reply.len() as u32, 1000);
                let r = &*(reply.as_ptr() as *const ICMP_ECHO_REPLY);
                let ev = if n > 0 && r.Status == 0 {
                    Ping::Reply { seq, ttl: u32::from(r.Options.Ttl), ms: f64::from(r.RoundTripTime) }
                } else {
                    Ping::Lost { seq }
                };
                if tx.send_blocking(ev).is_err() {
                    break;
                }
                let wait = Duration::from_secs_f64(interval.max(0.2));
                let waited = if n > 0 { Duration::from_millis(u64::from(r.RoundTripTime)) } else { Duration::from_secs(1) };
                thread::sleep(wait.saturating_sub(waited));
            }
            IcmpCloseHandle(h);
        }
        let _ = tx.send_blocking(Ping::Ended);
    });
    Ok(())
}

/// Line of `tracepath -n` output, or Ended.
#[derive(Clone, Debug, PartialEq)]
pub enum Trace {
    Line(String),
    Ended,
}

fn trace_line(l: &str) -> Trace {
    Trace::Line(l.to_string())
}

pub fn trace(host: &str, stop: Arc<AtomicBool>, tx: async_channel::Sender<Trace>) -> Result<(), String> {
    if cfg!(windows) {
        return lines("tracert", &["-d".into(), host.to_string()], trace_line, Trace::Ended, stop, tx);
    }
    lines("tracepath", &["-n".into(), "--".into(), host.to_string()], trace_line, Trace::Ended, stop, tx)
}

/// Round trip statistics of a ping run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Stats {
    pub sent: u32,
    pub received: u32,
    pub min: f64,
    pub max: f64,
    sum: f64,
    last: Option<f64>,
    jitter_sum: f64,
}

impl Stats {
    pub fn reply(&mut self, ms: f64) {
        self.sent += 1;
        self.received += 1;
        if self.received == 1 || ms < self.min {
            self.min = ms;
        }
        self.max = self.max.max(ms);
        self.sum += ms;
        if let Some(prev) = self.last.replace(ms) {
            self.jitter_sum += (ms - prev).abs();
        }
    }

    pub fn lost(&mut self) {
        self.sent += 1;
    }

    pub fn avg(&self) -> f64 {
        if self.received == 0 { 0.0 } else { self.sum / f64::from(self.received) }
    }

    /// Mean difference between consecutive replies.
    pub fn jitter(&self) -> f64 {
        if self.received < 2 { 0.0 } else { self.jitter_sum / f64::from(self.received - 1) }
    }

    pub fn loss(&self) -> f64 {
        if self.sent == 0 { 0.0 } else { 100.0 * f64::from(self.sent - self.received) / f64::from(self.sent) }
    }
}

/// "aa:bb:cc:dd:ee:ff", "aa-bb-…" or "aabbccddeeff".
pub fn parse_mac(s: &str) -> Result<[u8; 6], String> {
    let hex: String = s.chars().filter(|c| !matches!(c, ':' | '-' | '.' | ' ')).collect();
    if hex.len() != 12 {
        return Err(format!("'{s}' is not a MAC address"));
    }
    let mut mac = [0u8; 6];
    for (i, b) in mac.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).map_err(|_| format!("'{s}' is not a MAC address"))?;
    }
    Ok(mac)
}

/// Magic packet: 6 x 0xff, then the MAC 16 times.
pub fn magic(mac: [u8; 6]) -> [u8; 102] {
    let mut p = [0xffu8; 102];
    for i in 0..16 {
        p[6 + i * 6..12 + i * 6].copy_from_slice(&mac);
    }
    p
}

pub fn wake(mac: [u8; 6], to: SocketAddr) -> Result<(), String> {
    let s = UdpSocket::bind("0.0.0.0:0").map_err(|e| e.to_string())?;
    s.set_broadcast(true).map_err(|e| e.to_string())?;
    s.send_to(&magic(mac), to).map_err(|e| e.to_string())?;
    Ok(())
}

/// Saved Wake-on-LAN targets.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Wake {
    pub name: String,
    pub mac: String,
    /// Broadcast address; empty means 255.255.255.255.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub broadcast: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
}

impl Wake {
    pub fn target(&self) -> Result<SocketAddr, String> {
        let ip: IpAddr = if self.broadcast.trim().is_empty() {
            IpAddr::V4(Ipv4Addr::BROADCAST)
        } else {
            self.broadcast.trim().parse().map_err(|_| format!("'{}' is not an IP address", self.broadcast))?
        };
        Ok(SocketAddr::new(ip, self.port.unwrap_or(9)))
    }
}

/// Remembered settings of a file server tab.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerPrefs {
    pub dir: String,
    pub addr: String,
    pub port: u16,
    pub upload: bool,
}

impl Default for ServerPrefs {
    fn default() -> Self {
        Self::new(8000)
    }
}

impl ServerPrefs {
    pub fn new(port: u16) -> Self {
        Self { dir: crate::files::downloads_dir().to_string_lossy().into_owned(), addr: "0.0.0.0".into(), port, upload: false }
    }
}

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ToolsFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tftp: Option<ServerPrefs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<ServerPrefs>,
    #[serde(default, rename = "wake")]
    pub wake: Vec<Wake>,
}

fn tools_path() -> std::path::PathBuf {
    crate::settings::dir().join("tools.toml")
}

pub fn load_tools() -> ToolsFile {
    std::fs::read_to_string(tools_path()).ok().and_then(|t| toml::from_str::<ToolsFile>(&t).ok()).unwrap_or_default()
}

pub fn save_tools(f: &ToolsFile) -> Result<(), String> {
    let text = toml::to_string_pretty(f).map_err(|e| e.to_string())?;
    let p = tools_path();
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    crate::store::write_private(&p, &text).map_err(|e| e.to_string())
}

/// This machine's IPv4 addresses (for server URLs), loopback last.
#[cfg(windows)]
pub fn local_ips() -> Vec<IpAddr> {
    // The address the system would send from; connecting UDP sends nothing.
    let main = std::net::UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| s.connect("192.0.2.1:9").and_then(|_| s.local_addr()))
        .ok()
        .map(|a| a.ip())
        .filter(|ip| !ip.is_unspecified());
    main.into_iter().chain([IpAddr::V4(Ipv4Addr::LOCALHOST)]).collect()
}

/// This machine's IPv4 addresses (for server URLs), loopback last.
#[cfg(unix)]
pub fn local_ips() -> Vec<IpAddr> {
    let mut out: Vec<IpAddr> = Vec::new();
    // SAFETY: getifaddrs/freeifaddrs pair; entries are read before freeing.
    unsafe {
        let mut list: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut list) == 0 {
            let mut cur = list;
            while !cur.is_null() {
                let a = (*cur).ifa_addr;
                if !a.is_null() && i32::from((*a).sa_family) == libc::AF_INET {
                    let sin = a as *const libc::sockaddr_in;
                    out.push(IpAddr::V4(Ipv4Addr::from(u32::from_be((*sin).sin_addr.s_addr))));
                }
                cur = (*cur).ifa_next;
            }
            libc::freeifaddrs(list);
        }
    }
    let mut seen = std::collections::HashMap::new();
    out.retain(|ip| seen.insert(*ip, ()).is_none());
    out.sort_by_key(|ip| ip.is_loopback());
    out
}

/// A local TCP port that refuses connections and stays reserved while the
/// returned socket is open (bound, not listening), so parallel tests
/// cannot take it.
#[cfg(test)]
pub fn closed_port() -> (std::os::fd::OwnedFd, u16) {
    use std::os::fd::FromRawFd;
    // SAFETY: a fresh socket, bound to an ephemeral loopback port and
    // owned by the returned fd.
    unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_STREAM, 0);
        assert!(fd >= 0, "socket");
        let mut a: libc::sockaddr_in = std::mem::zeroed();
        a.sin_family = libc::AF_INET as libc::sa_family_t;
        a.sin_addr.s_addr = u32::from(Ipv4Addr::LOCALHOST).to_be();
        let len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        assert_eq!(libc::bind(fd, &a as *const _ as *const libc::sockaddr, len), 0, "bind");
        let mut got: libc::sockaddr_in = std::mem::zeroed();
        let mut glen = len;
        libc::getsockname(fd, &mut got as *mut _ as *mut libc::sockaddr, &mut glen);
        (std::os::fd::OwnedFd::from_raw_fd(fd), u16::from_be(got.sin_port))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn host_and_port_specs() {
        assert_eq!(hosts("10.0.0.5"), Ok(vec!["10.0.0.5".parse().expect("ip")]));
        let net = hosts("192.168.1.0/30").expect("cidr");
        assert_eq!(net, vec!["192.168.1.1".parse::<IpAddr>().expect("ip"), "192.168.1.2".parse().expect("ip")]);
        assert_eq!(hosts("192.168.1.200/24").expect("cidr").len(), 254);
        assert_eq!(hosts("10.0.0.10-12").expect("range").len(), 3);
        assert!(hosts("10.0.0.0/8").is_err());
        assert_eq!(ports("22, 80,8000-8002,22"), Ok(vec![22, 80, 8000, 8001, 8002]));
        assert_eq!(ports("common").expect("common").len(), COMMON.len());
        assert!(ports("0").is_err());
        assert!(ports("90-80").is_err());
        assert_eq!(service(22), "ssh");
    }

    #[test]
    fn scan_localhost() {
        let open = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = open.local_addr().expect("addr").port();
        thread::spawn(move || {
            if let Ok((mut s, _)) = open.accept() {
                let _ = std::io::Write::write_all(&mut s, b"SSH-2.0-test\r\n");
                thread::sleep(Duration::from_millis(300));
            }
        });
        let (_hold, closed) = closed_port();
        let (tx, rx) = async_channel::unbounded();
        scan(vec!["127.0.0.1".parse().expect("ip")], vec![port, closed], Duration::from_millis(300), true, Arc::default(), tx);
        let mut found = Vec::new();
        loop {
            match rx.recv_blocking().expect("event") {
                Scan::Open { addr, banner } => found.push((addr.port(), banner)),
                Scan::Done { open, total, .. } => {
                    assert_eq!((open, total), (1, 2));
                    break;
                }
                Scan::Progress { .. } => {}
            }
        }
        assert_eq!(found, vec![(port, "SSH-2.0-test".to_string())]);
    }

    #[test]
    fn ping_lines() {
        assert_eq!(parse_ping("64 bytes from 10.0.0.1: icmp_seq=3 ttl=63 time=1.25 ms"), Ping::Reply { seq: 3, ttl: 63, ms: 1.25 });
        assert_eq!(parse_ping("no answer yet for icmp_seq=4"), Ping::Lost { seq: 4 });
        assert!(matches!(parse_ping("PING 10.0.0.1 (10.0.0.1) 56(84) bytes of data."), Ping::Text(_)));
        let mut s = Stats::default();
        s.reply(1.0);
        s.reply(3.0);
        s.lost();
        s.reply(2.0);
        assert_eq!((s.sent, s.received, s.min, s.max), (4, 3, 1.0, 3.0));
        assert!((s.avg() - 2.0).abs() < 1e-9);
        assert!((s.jitter() - 1.5).abs() < 1e-9);
        assert!((s.loss() - 25.0).abs() < 1e-9);
    }

    #[test]
    fn ping_loopback() {
        if !crate::term::found("ping") {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = async_channel::unbounded();
        ping("127.0.0.1", 0.2, stop.clone(), tx).expect("start");
        let reply = (0..20).find_map(|_| match rx.recv_blocking() {
            Ok(Ping::Reply { ms, .. }) => Some(ms),
            _ => None,
        });
        stop.store(true, Ordering::Release);
        assert!(reply.is_some_and(|ms| ms < 100.0));
        assert!((0..50).any(|_| matches!(rx.recv_blocking(), Ok(Ping::Ended))));
    }

    #[test]
    fn wake_packet() {
        assert_eq!(parse_mac("00-11-22-33-44-AA"), Ok([0, 0x11, 0x22, 0x33, 0x44, 0xaa]));
        assert_eq!(parse_mac("001122334455"), Ok([0, 0x11, 0x22, 0x33, 0x44, 0x55]));
        assert!(parse_mac("00:11:22").is_err());
        let rx = UdpSocket::bind("127.0.0.1:0").expect("bind");
        let to = rx.local_addr().expect("addr");
        wake([1, 2, 3, 4, 5, 6], to).expect("send");
        let mut buf = [0u8; 200];
        let n = rx.recv(&mut buf).expect("recv");
        assert_eq!(n, 102);
        assert_eq!(&buf[..6], &[0xff; 6]);
        assert_eq!(&buf[96..102], &[1, 2, 3, 4, 5, 6]);
        let w = Wake { broadcast: "192.168.1.255".into(), ..Default::default() };
        assert_eq!(w.target(), Ok("192.168.1.255:9".parse().expect("addr")));
        assert!(!local_ips().is_empty());
    }
}
