// TFTP server (RFC 1350) with the blksize, tsize and timeout options
// (RFC 2347-2349), for boot loaders like U-Boot. Each transfer runs on its
// own socket and thread; paths stay inside the served folder.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{SocketAddr, UdpSocket};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

const RRQ: u16 = 1;
const WRQ: u16 = 2;
const DATA: u16 = 3;
const ACK: u16 = 4;
const ERROR: u16 = 5;
const OACK: u16 = 6;
const RETRIES: u32 = 5;

#[derive(Clone, Debug)]
pub struct Cfg {
    pub root: PathBuf,
    pub addr: SocketAddr,
    /// Accept uploads (WRQ).
    pub upload: bool,
}

/// What the server reports.
#[derive(Clone, Debug, PartialEq)]
pub enum Log {
    Listening(SocketAddr),
    Start { peer: SocketAddr, file: String, upload: bool, size: Option<u64>, block: usize },
    Done { peer: SocketAddr, file: String, bytes: u64, secs: f64 },
    Fail { peer: SocketAddr, file: String, why: String },
}

/// Running server; dropping it stops listening (running transfers finish).
pub struct Server {
    stop: Arc<AtomicBool>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// Path of `name` inside `root`, or None when it would leave it.
pub fn inside(root: &Path, name: &str) -> Option<PathBuf> {
    let mut p = root.to_path_buf();
    for c in Path::new(name.trim_start_matches('/')).components() {
        match c {
            Component::Normal(x) => p.push(x),
            Component::CurDir => {}
            _ => return None,
        }
    }
    // Symlinks may still point outside: the nearest existing part of the
    // path must really be inside (the rest has no "..").
    let real_root = root.canonicalize().ok()?;
    let mut probe = p.as_path();
    while !probe.exists() {
        probe = probe.parent()?;
    }
    probe.canonicalize().ok()?.starts_with(&real_root).then_some(p)
}

fn error(code: u16, msg: &str) -> Vec<u8> {
    let mut b = Vec::with_capacity(5 + msg.len());
    b.extend_from_slice(&ERROR.to_be_bytes());
    b.extend_from_slice(&code.to_be_bytes());
    b.extend_from_slice(msg.as_bytes());
    b.push(0);
    b
}

fn ack(block: u16) -> [u8; 4] {
    let mut b = [0u8; 4];
    b[0..2].copy_from_slice(&ACK.to_be_bytes());
    b[2..4].copy_from_slice(&block.to_be_bytes());
    b
}

/// Request: file name, mode and options (lower case names).
struct Request {
    op: u16,
    file: String,
    opts: Vec<(String, String)>,
}

fn parse_request(b: &[u8]) -> Option<Request> {
    if b.len() < 4 {
        return None;
    }
    let op = u16::from_be_bytes([b[0], b[1]]);
    if op != RRQ && op != WRQ {
        return None;
    }
    let mut parts = b[2..].split(|&c| c == 0).map(|s| String::from_utf8_lossy(s).into_owned());
    let file = parts.next()?;
    let _mode = parts.next()?;
    let rest: Vec<String> = parts.collect();
    let opts = rest.chunks(2).filter(|c| c.len() == 2 && !c[0].is_empty()).map(|c| (c[0].to_lowercase(), c[1].clone())).collect();
    Some(Request { op, file, opts })
}

/// Options we accept, with the values to confirm.
struct Opts {
    block: usize,
    timeout: Duration,
    tsize: Option<u64>,
    reply: Vec<(String, String)>,
}

fn negotiate(req: &Request, size: Option<u64>) -> Opts {
    let mut o = Opts { block: 512, timeout: Duration::from_secs(1), tsize: None, reply: Vec::new() };
    for (k, v) in &req.opts {
        match k.as_str() {
            "blksize" => {
                if let Ok(n) = v.parse::<usize>() {
                    o.block = n.clamp(8, 65464);
                    o.reply.push((k.clone(), o.block.to_string()));
                }
            }
            "timeout" => {
                if let Ok(n) = v.parse::<u64>() {
                    if (1..=255).contains(&n) {
                        o.timeout = Duration::from_secs(n);
                        o.reply.push((k.clone(), n.to_string()));
                    }
                }
            }
            "tsize" => {
                // Download: we tell the size; upload: the client did.
                let n = if req.op == RRQ { size } else { v.parse().ok() };
                if let Some(n) = n {
                    o.tsize = Some(n);
                    o.reply.push((k.clone(), n.to_string()));
                }
            }
            _ => {}
        }
    }
    o
}

fn oack(reply: &[(String, String)]) -> Vec<u8> {
    let mut b = OACK.to_be_bytes().to_vec();
    for (k, v) in reply {
        b.extend_from_slice(k.as_bytes());
        b.push(0);
        b.extend_from_slice(v.as_bytes());
        b.push(0);
    }
    b
}

/// Sends `pkt` until `ok` accepts an answer; returns that answer.
fn exchange(sock: &UdpSocket, pkt: &[u8], timeout: Duration, ok: impl Fn(&[u8]) -> bool) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; 65536];
    for _ in 0..=RETRIES {
        sock.send(pkt).map_err(|e| e.to_string())?;
        let end = Instant::now() + timeout;
        loop {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            let _ = sock.set_read_timeout(Some(left));
            match sock.recv(&mut buf) {
                Ok(n) => {
                    let p = &buf[..n];
                    if p.len() >= 4 && u16::from_be_bytes([p[0], p[1]]) == ERROR {
                        let msg = String::from_utf8_lossy(&p[4..]).trim_end_matches('\0').to_string();
                        return Err(format!("client error: {msg}"));
                    }
                    if ok(p) {
                        return Ok(p.to_vec());
                    }
                }
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => break,
                Err(e) => return Err(e.to_string()),
            }
        }
    }
    Err("no answer from the client".into())
}

fn is_ack(p: &[u8], block: u16) -> bool {
    p.len() >= 4 && u16::from_be_bytes([p[0], p[1]]) == ACK && u16::from_be_bytes([p[2], p[3]]) == block
}

fn send_file(sock: &UdpSocket, mut f: File, o: &Opts) -> Result<u64, String> {
    let mut block: u16 = 1;
    let mut sent = 0u64;
    let mut buf = vec![0u8; o.block];
    loop {
        let mut n = 0;
        while n < o.block {
            let r = f.read(&mut buf[n..]).map_err(|e| e.to_string())?;
            if r == 0 {
                break;
            }
            n += r;
        }
        let mut pkt = DATA.to_be_bytes().to_vec();
        pkt.extend_from_slice(&block.to_be_bytes());
        pkt.extend_from_slice(&buf[..n]);
        exchange(sock, &pkt, o.timeout, |p| is_ack(p, block))?;
        sent += n as u64;
        if n < o.block {
            return Ok(sent);
        }
        block = block.wrapping_add(1);
    }
}

fn receive_file(sock: &UdpSocket, mut f: File, first: &[u8], o: &Opts) -> Result<u64, String> {
    let mut block: u16 = 1;
    let mut got = 0u64;
    let mut reply = first.to_vec();
    loop {
        let want = block;
        let p = exchange(sock, &reply, o.timeout, |p| p.len() >= 4 && u16::from_be_bytes([p[0], p[1]]) == DATA && u16::from_be_bytes([p[2], p[3]]) == want)?;
        let data = &p[4..];
        f.write_all(data).map_err(|e| e.to_string())?;
        got += data.len() as u64;
        reply = ack(block).to_vec();
        if data.len() < o.block {
            // Final ACK; a lost one makes the client resend, which we ignore.
            sock.send(&reply).map_err(|e| e.to_string())?;
            return Ok(got);
        }
        block = block.wrapping_add(1);
    }
}

fn transfer(cfg: &Cfg, peer: SocketAddr, req: Request, log: &async_channel::Sender<Log>) {
    let local = SocketAddr::new(cfg.addr.ip(), 0);
    let Ok(sock) = UdpSocket::bind(local) else { return };
    if sock.connect(peer).is_err() {
        return;
    }
    let fail = |why: String| {
        let _ = log.send_blocking(Log::Fail { peer, file: req.file.clone(), why });
    };
    let Some(path) = inside(&cfg.root, &req.file) else {
        let _ = sock.send(&error(2, "access violation"));
        return fail("path outside the served folder".into());
    };
    let started = Instant::now();
    let res = if req.op == RRQ {
        let f = match File::open(&path) {
            Ok(f) if path.is_file() => f,
            Ok(_) | Err(_) => {
                let _ = sock.send(&error(1, "file not found"));
                return fail("file not found".into());
            }
        };
        let size = f.metadata().ok().map(|m| m.len());
        let o = negotiate(&req, size);
        let _ = log.send_blocking(Log::Start { peer, file: req.file.clone(), upload: false, size, block: o.block });
        let go = if o.reply.is_empty() { Ok(Vec::new()) } else { exchange(&sock, &oack(&o.reply), o.timeout, |p| is_ack(p, 0)) };
        go.and_then(|_| send_file(&sock, f, &o))
    } else {
        if !cfg.upload {
            let _ = sock.send(&error(2, "uploads are off"));
            return fail("upload refused (uploads are off)".into());
        }
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let f = match OpenOptions::new().create(true).truncate(true).write(true).open(&path) {
            Ok(f) => f,
            Err(e) => {
                let _ = sock.send(&error(2, "cannot write"));
                return fail(e.to_string());
            }
        };
        let o = negotiate(&req, None);
        let _ = log.send_blocking(Log::Start { peer, file: req.file.clone(), upload: true, size: o.tsize, block: o.block });
        let first = if o.reply.is_empty() { ack(0).to_vec() } else { oack(&o.reply) };
        receive_file(&sock, f, &first, &o)
    };
    match res {
        Ok(bytes) => {
            let _ = log.send_blocking(Log::Done { peer, file: req.file.clone(), bytes, secs: started.elapsed().as_secs_f64() });
        }
        Err(e) => fail(e),
    }
}

/// Starts listening on `cfg.addr`.
pub fn serve(cfg: Cfg, log: async_channel::Sender<Log>) -> io::Result<(Server, SocketAddr)> {
    let sock = UdpSocket::bind(cfg.addr)?;
    sock.set_read_timeout(Some(Duration::from_millis(200)))?;
    let addr = sock.local_addr()?;
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    let _ = log.send_blocking(Log::Listening(addr));
    thread::spawn(move || {
        let mut buf = vec![0u8; 2048];
        while !s.load(Ordering::Acquire) {
            let Ok((n, peer)) = sock.recv_from(&mut buf) else { continue };
            let Some(req) = parse_request(&buf[..n]) else {
                let _ = sock.send_to(&error(4, "illegal operation"), peer);
                continue;
            };
            let (cfg, log) = (cfg.clone(), log.clone());
            thread::spawn(move || transfer(&cfg, peer, req, &log));
        }
    });
    Ok((Server { stop }, addr))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal client: downloads `file`, with blksize when given.
    fn get(server: SocketAddr, file: &str, blksize: Option<usize>) -> Result<Vec<u8>, String> {
        let s = UdpSocket::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        s.set_read_timeout(Some(Duration::from_secs(3))).map_err(|e| e.to_string())?;
        let mut req = RRQ.to_be_bytes().to_vec();
        req.extend_from_slice(file.as_bytes());
        req.extend_from_slice(b"\0octet\0");
        if let Some(b) = blksize {
            req.extend_from_slice(format!("blksize\0{b}\0tsize\00\0").as_bytes());
        }
        s.send_to(&req, server).map_err(|e| e.to_string())?;
        let mut out = Vec::new();
        let mut buf = vec![0u8; 70000];
        let mut size = 512;
        loop {
            let (n, from) = s.recv_from(&mut buf).map_err(|e| e.to_string())?;
            let p = &buf[..n];
            match u16::from_be_bytes([p[0], p[1]]) {
                OACK => {
                    let text = String::from_utf8_lossy(&p[2..]).to_string();
                    let f: Vec<&str> = text.split('\0').collect();
                    if let Some(i) = f.iter().position(|x| *x == "blksize") {
                        size = f[i + 1].parse().unwrap_or(512);
                    }
                    assert!(f.contains(&"tsize"));
                    s.send_to(&ack(0), from).map_err(|e| e.to_string())?;
                }
                DATA => {
                    out.extend_from_slice(&p[4..]);
                    s.send_to(&p[0..4].iter().enumerate().map(|(i, b)| if i == 1 { ACK as u8 } else { *b }).collect::<Vec<u8>>(), from)
                        .map_err(|e| e.to_string())?;
                    if p.len() - 4 < size {
                        return Ok(out);
                    }
                }
                ERROR => return Err(String::from_utf8_lossy(&p[4..]).trim_end_matches('\0').to_string()),
                _ => return Err("unexpected packet".into()),
            }
        }
    }

    fn put(server: SocketAddr, file: &str, data: &[u8]) -> Result<(), String> {
        let s = UdpSocket::bind("127.0.0.1:0").map_err(|e| e.to_string())?;
        s.set_read_timeout(Some(Duration::from_secs(3))).map_err(|e| e.to_string())?;
        let mut req = WRQ.to_be_bytes().to_vec();
        req.extend_from_slice(file.as_bytes());
        req.extend_from_slice(b"\0octet\0");
        s.send_to(&req, server).map_err(|e| e.to_string())?;
        let mut buf = [0u8; 600];
        let (n, from) = s.recv_from(&mut buf).map_err(|e| e.to_string())?;
        if u16::from_be_bytes([buf[0], buf[1]]) == ERROR {
            return Err(String::from_utf8_lossy(&buf[4..n]).trim_end_matches('\0').to_string());
        }
        let mut block: u16 = 1;
        for chunk in data.chunks(512).chain(if data.len().is_multiple_of(512) { Some(&[][..]) } else { None }) {
            let mut pkt = DATA.to_be_bytes().to_vec();
            pkt.extend_from_slice(&block.to_be_bytes());
            pkt.extend_from_slice(chunk);
            s.send_to(&pkt, from).map_err(|e| e.to_string())?;
            s.recv_from(&mut buf).map_err(|e| e.to_string())?;
            assert!(is_ack(&buf[..4], block));
            block = block.wrapping_add(1);
        }
        Ok(())
    }

    fn root() -> PathBuf {
        let d = std::env::temp_dir().join(format!("tv-tftp-{}", std::process::id()));
        fs::create_dir_all(d.join("boot")).expect("dir");
        d
    }

    #[test]
    fn paths_stay_inside() {
        let r = root();
        fs::write(r.join("boot/zImage"), b"x").expect("w");
        assert_eq!(inside(&r, "/boot/zImage"), Some(r.join("boot/zImage")));
        assert_eq!(inside(&r, "new.bin"), Some(r.join("new.bin")));
        assert_eq!(inside(&r, "a/b/new.bin"), Some(r.join("a/b/new.bin")));
        assert_eq!(inside(&r, "../etc/passwd"), None);
        assert_eq!(inside(&r, "boot/../../x"), None);
        #[cfg(unix)]
        {
            let _ = std::os::unix::fs::symlink("/etc", r.join("out"));
            assert_eq!(inside(&r, "out/hostname"), None);
            assert_eq!(inside(&r, "out/new/file"), None);
        }
    }

    #[test]
    fn download_upload() {
        let r = root();
        let image: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        fs::write(r.join("boot/image.bin"), &image).expect("w");
        fs::write(r.join("exact.bin"), vec![1u8; 1024]).expect("w");
        let (tx, rx) = async_channel::unbounded();
        let cfg = Cfg { root: r.clone(), addr: "127.0.0.1:0".parse().expect("addr"), upload: false };
        let (srv, addr) = serve(cfg, tx).expect("serve");
        assert!(matches!(rx.recv_blocking(), Ok(Log::Listening(_))));

        assert_eq!(get(addr, "boot/image.bin", None).expect("get 512"), image);
        assert_eq!(get(addr, "/boot/image.bin", Some(1468)).expect("get 1468"), image);
        // A multiple of the block size ends with an empty block.
        assert_eq!(get(addr, "exact.bin", None).expect("exact").len(), 1024);
        assert_eq!(get(addr, "missing", None), Err("file not found".into()));
        assert_eq!(get(addr, "../../etc/passwd", None), Err("access violation".into()));
        assert_eq!(put(addr, "up.bin", b"data"), Err("uploads are off".into()));
        drop(srv);

        let (tx, rx) = async_channel::unbounded();
        let cfg = Cfg { root: r.clone(), addr: "127.0.0.1:0".parse().expect("addr"), upload: true };
        let (_srv, addr) = serve(cfg, tx).expect("serve");
        let data: Vec<u8> = (0..2000u32).map(|i| i as u8).collect();
        put(addr, "logs/up.bin", &data).expect("put");
        let done = (0..10).find_map(|_| match rx.recv_blocking() {
            Ok(Log::Done { bytes, .. }) => Some(bytes),
            _ => None,
        });
        assert_eq!(done, Some(2000));
        assert_eq!(fs::read(r.join("logs/up.bin")).expect("read"), data);
        let _ = fs::remove_dir_all(&r);
    }
}
