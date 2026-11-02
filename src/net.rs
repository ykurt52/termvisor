// Raw TCP and telnet transports.
//
// Connecting happens on a background thread so a slow host never blocks
// the UI. Telnet option negotiation is answered in the reader thread.

use std::collections::HashSet;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::link::{Event, StreamOpts};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetCfg {
    pub host: String,
    pub port: u16,
    #[serde(flatten)]
    pub opts: StreamOpts,
}

impl Default for NetCfg {
    fn default() -> Self {
        Self { host: String::new(), port: 23, opts: StreamOpts::default() }
    }
}

impl NetCfg {
    /// "host:port", IPv6 literals in brackets.
    pub fn addr(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }

    pub fn check(&self) -> Result<(), String> {
        let h = self.host.trim();
        if h.is_empty() || h.contains(char::is_whitespace) {
            return Err(format!("invalid host '{}'", self.host));
        }
        if self.port == 0 {
            return Err("port must be 1-65535".into());
        }
        Ok(())
    }
}

const IAC: u8 = 255;
const DONT: u8 = 254;
const DO: u8 = 253;
const WONT: u8 = 252;
const WILL: u8 = 251;
const SB: u8 = 250;
const BRK: u8 = 243;
const SE: u8 = 240;
const OPT_ECHO: u8 = 1;
const OPT_SGA: u8 = 3;

enum St {
    Data,
    Iac,
    Cmd(u8),
    Sub,
    SubIac,
}

/// Telnet stream filter. We accept server ECHO and SGA, offer SGA, refuse
/// everything else, and answer each request only once.
pub struct Telnet {
    st: St,
    sent: HashSet<(u8, u8)>,
}

impl Default for Telnet {
    fn default() -> Self {
        Self { st: St::Data, sent: HashSet::new() }
    }
}

impl Telnet {
    /// Returns (payload for the screen, replies for the server).
    pub fn feed(&mut self, data: &[u8]) -> (Vec<u8>, Vec<u8>) {
        let mut out = Vec::with_capacity(data.len());
        let mut reply = Vec::new();
        for &b in data {
            self.st = match self.st {
                St::Data if b == IAC => St::Iac,
                St::Data => {
                    out.push(b);
                    St::Data
                }
                St::Iac => match b {
                    IAC => {
                        out.push(IAC);
                        St::Data
                    }
                    WILL | WONT | DO | DONT => St::Cmd(b),
                    SB => St::Sub,
                    _ => St::Data,
                },
                St::Cmd(cmd) => {
                    let answer = match cmd {
                        WILL if b == OPT_ECHO || b == OPT_SGA => Some(DO),
                        WILL => Some(DONT),
                        DO if b == OPT_SGA => Some(WILL),
                        DO => Some(WONT),
                        _ => None,
                    };
                    if let Some(a) = answer {
                        if self.sent.insert((a, b)) {
                            reply.extend_from_slice(&[IAC, a, b]);
                        }
                    }
                    St::Data
                }
                St::Sub if b == IAC => St::SubIac,
                St::Sub => St::Sub,
                St::SubIac if b == SE => St::Data,
                St::SubIac => St::Sub,
            };
        }
        (out, reply)
    }
}

/// Doubles IAC bytes for sending over telnet.
pub fn escape(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() + 2);
    for &b in data {
        out.push(b);
        if b == IAC {
            out.push(IAC);
        }
    }
    out
}

enum Cmd {
    Write(Vec<u8>),
    Break,
}

/// A TCP connection. Dropping it closes the socket and stops the threads.
pub struct Conn {
    cmds: mpsc::Sender<Cmd>,
    stop: Arc<AtomicBool>,
    sock: Arc<Mutex<Option<TcpStream>>>,
}

impl Conn {
    pub fn write(&self, data: Vec<u8>) {
        let _ = self.cmds.send(Cmd::Write(data));
    }

    /// Telnet BRK; ignored on raw TCP.
    pub fn send_break(&self) {
        let _ = self.cmds.send(Cmd::Break);
    }

    pub fn sink(&self) -> crate::link::Sink {
        crate::link::sink(self.cmds.clone(), Cmd::Write)
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(guard) = self.sock.lock() {
            if let Some(s) = guard.as_ref() {
                let _ = s.shutdown(Shutdown::Both);
            }
        }
    }
}

fn connect(addr: &str) -> io::Result<TcpStream> {
    let mut last = io::Error::new(io::ErrorKind::NotFound, "no address");
    for a in addr.to_socket_addrs()? {
        match TcpStream::connect_timeout(&a, Duration::from_secs(5)) {
            Ok(s) => return Ok(s),
            Err(e) => last = e,
        }
    }
    Err(last)
}

/// Starts connecting in the background. Up, Data and Closed arrive on the
/// receiver; Closed also reports a failed connect.
pub fn open(cfg: &NetCfg, telnet: bool) -> Result<(Conn, async_channel::Receiver<Event>), String> {
    cfg.check()?;
    let addr = cfg.addr();
    let (ev_tx, ev_rx) = async_channel::unbounded();
    let (cmd_tx, cmd_rx) = mpsc::channel();
    let stop = Arc::new(AtomicBool::new(false));
    let sock: Arc<Mutex<Option<TcpStream>>> = Arc::default();

    let (halt, slot) = (stop.clone(), sock.clone());
    thread::Builder::new()
        .name("net-io".into())
        .spawn(move || io_main(addr, telnet, ev_tx, cmd_rx, halt, slot))
        .map_err(|e| e.to_string())?;
    Ok((Conn { cmds: cmd_tx, stop, sock }, ev_rx))
}

fn io_main(
    addr: String,
    telnet: bool,
    tx: async_channel::Sender<Event>,
    cmds: mpsc::Receiver<Cmd>,
    stop: Arc<AtomicBool>,
    slot: Arc<Mutex<Option<TcpStream>>>,
) {
    let stream = match connect(&addr) {
        Ok(s) => s,
        Err(e) => {
            let _ = tx.send_blocking(Event::Closed(format!("{addr}: {e}")));
            return;
        }
    };
    let (Ok(writer), Ok(replies)) = (stream.try_clone(), stream.try_clone()) else {
        let _ = tx.send_blocking(Event::Closed("socket clone failed".into()));
        return;
    };
    if let Ok(mut g) = slot.lock() {
        *g = stream.try_clone().ok();
    }
    if stop.load(Ordering::Relaxed) {
        let _ = stream.shutdown(Shutdown::Both);
        return;
    }
    let _ = stream.set_nodelay(true);
    let _ = tx.send_blocking(Event::Up(format!("connected {addr}")));

    let (wtx, halt) = (tx.clone(), stop.clone());
    let spawned = thread::Builder::new().name("net-tx".into()).spawn(move || write_loop(writer, telnet, cmds, wtx, halt));
    if spawned.is_err() {
        let _ = tx.send_blocking(Event::Closed("thread start failed".into()));
        return;
    }
    read_loop(stream, replies, telnet, tx, stop);
}

fn read_loop(
    mut stream: TcpStream,
    mut replies: TcpStream,
    telnet: bool,
    tx: async_channel::Sender<Event>,
    stop: Arc<AtomicBool>,
) {
    let mut filter = Telnet::default();
    let mut buf = [0u8; 4096];
    loop {
        let res = stream.read(&mut buf);
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let n = match res {
            Ok(0) => {
                let _ = tx.send_blocking(Event::Closed("connection closed by peer".into()));
                return;
            }
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => {
                let _ = tx.send_blocking(Event::Closed(e.to_string()));
                return;
            }
        };
        let data = if telnet {
            let (out, reply) = filter.feed(&buf[..n]);
            if !reply.is_empty() {
                let _ = replies.write_all(&reply);
            }
            out
        } else {
            buf[..n].to_vec()
        };
        if !data.is_empty() && tx.send_blocking(Event::Data(data)).is_err() {
            return;
        }
    }
}

fn write_loop(
    mut stream: TcpStream,
    telnet: bool,
    cmds: mpsc::Receiver<Cmd>,
    tx: async_channel::Sender<Event>,
    stop: Arc<AtomicBool>,
) {
    while let Ok(cmd) = cmds.recv() {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let bytes = match (cmd, telnet) {
            (Cmd::Write(d), true) => escape(&d),
            (Cmd::Write(d), false) => d,
            (Cmd::Break, true) => vec![IAC, BRK],
            (Cmd::Break, false) => continue,
        };
        if let Err(e) = stream.write_all(&bytes) {
            if !stop.load(Ordering::Relaxed) {
                let _ = tx.send_blocking(Event::Closed(e.to_string()));
            }
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn telnet_negotiation() {
        let mut t = Telnet::default();
        let input = [b'h', IAC, WILL, OPT_ECHO, IAC, DO, 24, IAC, IAC, b'i', IAC, SB, 24, 1, IAC, SE, b'!'];
        let (out, reply) = t.feed(&input);
        assert_eq!(out, [b'h', IAC, b'i', b'!']);
        assert_eq!(reply, [IAC, DO, OPT_ECHO, IAC, WONT, 24]);
        // Same request again: no second answer.
        let (_, again) = t.feed(&[IAC, WILL, OPT_ECHO]);
        assert!(again.is_empty());
    }

    #[test]
    fn telnet_split_across_chunks() {
        let mut t = Telnet::default();
        let (a, _) = t.feed(&[b'x', IAC]);
        let (b, r) = t.feed(&[DO, OPT_SGA, b'y']);
        assert_eq!(a, b"x");
        assert_eq!(b, b"y");
        assert_eq!(r, [IAC, WILL, OPT_SGA]);
    }

    #[test]
    fn escape_iac() {
        assert_eq!(escape(&[1, IAC, 2]), [1, IAC, IAC, 2]);
    }

    #[test]
    fn addr_forms() {
        let mut c = NetCfg { host: "10.0.0.2".into(), port: 5760, ..Default::default() };
        assert_eq!(c.addr(), "10.0.0.2:5760");
        c.host = "::1".into();
        assert_eq!(c.addr(), "[::1]:5760");
        c.host = "a b".into();
        assert!(c.check().is_err());
    }

    #[test]
    fn tcp_roundtrip() {
        let srv = TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = srv.local_addr().expect("addr").port();
        let cfg = NetCfg { host: "127.0.0.1".into(), port, ..Default::default() };
        let (conn, rx) = open(&cfg, false).expect("open");
        let (mut peer, _) = srv.accept().expect("accept");
        assert!(matches!(rx.recv_blocking(), Ok(Event::Up(_))));

        peer.write_all(&[0xFE, 0x09, 0x00]).expect("peer write");
        match rx.recv_blocking() {
            Ok(Event::Data(d)) => assert_eq!(d, [0xFE, 0x09, 0x00]),
            _ => panic!("expected data"),
        }
        conn.write(vec![0x55, 0xAA]);
        let mut buf = [0u8; 2];
        peer.read_exact(&mut buf).expect("peer read");
        assert_eq!(buf, [0x55, 0xAA]);

        drop(peer);
        assert!(matches!(rx.recv_blocking(), Ok(Event::Closed(_))));
    }

    #[test]
    fn refused_port_reports_closed() {
        let (_hold, port) = crate::nettools::closed_port();
        let cfg = NetCfg { host: "127.0.0.1".into(), port, ..Default::default() };
        let (_conn, rx) = open(&cfg, true).expect("open");
        assert!(matches!(rx.recv_blocking(), Ok(Event::Closed(_))));
    }
}
