// Modbus session setup and its byte link: a serial port (RTU), a TCP
// connection to a slave (master over TCP) or a TCP server that masters
// connect to (slave over TCP).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::link::Event;
use crate::net::NetCfg;
use crate::serial::SerialCfg;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    #[default]
    Master,
    Slave,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    Rtu,
    #[default]
    Tcp,
}

/// A Modbus session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModbusCfg {
    pub role: Role,
    pub transport: Transport,
    /// RTU port.
    pub serial: SerialCfg,
    /// TCP: the slave's address (master), or the address to listen on (slave).
    pub host: String,
    pub port: u16,
    /// Master: unit id of new polls. Slave: the id it answers to (0: any).
    pub unit: u8,
    /// Master: how long to wait for a reply.
    pub timeout_ms: u32,
}

impl Default for ModbusCfg {
    fn default() -> Self {
        Self {
            role: Role::Master,
            transport: Transport::Tcp,
            serial: SerialCfg { baud: 9600, ..SerialCfg::default() },
            host: "127.0.0.1".into(),
            port: 502,
            unit: 1,
            timeout_ms: 1000,
        }
    }
}

impl ModbusCfg {
    fn net(&self) -> NetCfg {
        NetCfg { host: self.host.clone(), port: self.port, ..NetCfg::default() }
    }

    /// "COM3 9600 8N1" or "10.0.0.5:502".
    pub fn desc(&self) -> String {
        match self.transport {
            Transport::Rtu => format!("{} {}", self.serial.device, self.serial.summary()),
            Transport::Tcp => self.net().addr(),
        }
    }

    /// Tab title: "Modbus master 10.0.0.5:502".
    pub fn title(&self) -> String {
        let role = match self.role {
            Role::Master => "master",
            Role::Slave => "slave",
        };
        let at = match self.transport {
            Transport::Rtu => self.serial.device.rsplit(['/', '\\']).next().unwrap_or(&self.serial.device).to_string(),
            Transport::Tcp => self.net().addr(),
        };
        format!("Modbus {role} {at}")
    }

    pub fn check(&self) -> Result<(), String> {
        match self.transport {
            Transport::Rtu => self.serial.check(),
            Transport::Tcp => self.net().check(),
        }?;
        if self.role == Role::Master && self.unit == 0 {
            return Err("a master polls units 1-247".into());
        }
        if self.unit > 247 {
            return Err("unit id must be 0-247".into());
        }
        Ok(())
    }
}

/// From the link threads to the pane. `client` tells TCP masters apart on
/// a slave server; it is 0 elsewhere.
pub enum Ev {
    Up(String),
    Data(u64, Vec<u8>),
    /// A note for the log (a master connected or left).
    Note(String),
    Closed(String),
}

/// An open link. Dropping it closes it.
pub enum Link {
    Serial(crate::serial::Conn),
    Tcp(crate::net::Conn),
    Server(Server),
}

impl Link {
    pub fn write(&self, client: u64, data: Vec<u8>) {
        match self {
            Link::Serial(c) => c.write(data),
            Link::Tcp(c) => c.write(data),
            Link::Server(s) => s.write(client, &data),
        }
    }
}

/// Passes stream events on as Modbus link events.
fn bridge(rx: async_channel::Receiver<Event>) -> async_channel::Receiver<Ev> {
    let (tx, out) = async_channel::unbounded();
    thread::spawn(move || {
        while let Ok(e) = rx.recv_blocking() {
            let e = match e {
                Event::Up(s) => Ev::Up(s),
                Event::Data(d) => Ev::Data(0, d),
                Event::Closed(s) => Ev::Closed(s),
            };
            if tx.send_blocking(e).is_err() {
                break;
            }
        }
    });
    out
}

pub fn open(cfg: &ModbusCfg) -> Result<(Link, async_channel::Receiver<Ev>), String> {
    cfg.check()?;
    match (cfg.transport, cfg.role) {
        (Transport::Rtu, _) => {
            let (c, rx) = crate::serial::open(&cfg.serial)?;
            // Many RS-485 adapters need DTR and RTS up; Windows opens with both low.
            c.dtr(true);
            c.rts(true);
            Ok((Link::Serial(c), bridge(rx)))
        }
        (Transport::Tcp, Role::Master) => {
            let (c, rx) = crate::net::open(&cfg.net(), false)?;
            Ok((Link::Tcp(c), bridge(rx)))
        }
        (Transport::Tcp, Role::Slave) => {
            let (s, rx) = Server::start(&cfg.host, cfg.port)?;
            Ok((Link::Server(s), rx))
        }
    }
}

/// TCP server for a slave; each connected master gets an id.
pub struct Server {
    stop: Arc<AtomicBool>,
    clients: Arc<Mutex<HashMap<u64, TcpStream>>>,
}

impl Server {
    fn start(host: &str, port: u16) -> Result<(Self, async_channel::Receiver<Ev>), String> {
        let bind = if host.contains(':') { format!("[{host}]:{port}") } else { format!("{host}:{port}") };
        let listener = TcpListener::bind(&bind).map_err(|e| {
            let hint = if port < 1024 && cfg!(unix) { " (ports below 1024 need extra rights on Linux; try 5020)" } else { "" };
            format!("listen on {bind}: {e}{hint}")
        })?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let (tx, rx) = async_channel::unbounded();
        let stop = Arc::new(AtomicBool::new(false));
        let clients: Arc<Mutex<HashMap<u64, TcpStream>>> = Arc::default();
        let at = listener.local_addr().map(|a| a.to_string()).unwrap_or(bind);
        let _ = tx.send_blocking(Ev::Up(format!("listening on {at}")));
        let (halt, list) = (stop.clone(), clients.clone());
        thread::Builder::new()
            .name("modbus-server".into())
            .spawn(move || accept_loop(listener, tx, halt, list))
            .map_err(|e| e.to_string())?;
        Ok((Self { stop, clients }, rx))
    }

    fn write(&self, client: u64, data: &[u8]) {
        if let Ok(mut list) = self.clients.lock() {
            if let Some(s) = list.get_mut(&client) {
                let _ = s.write_all(data);
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Ok(list) = self.clients.lock() {
            for s in list.values() {
                let _ = s.shutdown(Shutdown::Both);
            }
        }
    }
}

fn accept_loop(listener: TcpListener, tx: async_channel::Sender<Ev>, stop: Arc<AtomicBool>, clients: Arc<Mutex<HashMap<u64, TcpStream>>>) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((sock, peer)) => {
                let id = NEXT.fetch_add(1, Ordering::Relaxed);
                let _ = sock.set_nonblocking(false);
                let _ = sock.set_nodelay(true);
                let _ = sock.set_read_timeout(Some(Duration::from_millis(200)));
                let Ok(writer) = sock.try_clone() else { continue };
                if let Ok(mut list) = clients.lock() {
                    list.insert(id, writer);
                }
                let _ = tx.send_blocking(Ev::Note(format!("master {peer} connected")));
                let (tx, stop, clients) = (tx.clone(), stop.clone(), clients.clone());
                thread::spawn(move || client_loop(sock, id, &peer.to_string(), tx, stop, clients));
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => thread::sleep(Duration::from_millis(50)),
            Err(e) => {
                let _ = tx.send_blocking(Ev::Closed(format!("accept: {e}")));
                return;
            }
        }
    }
}

fn client_loop(mut sock: TcpStream, id: u64, peer: &str, tx: async_channel::Sender<Ev>, stop: Arc<AtomicBool>, clients: Arc<Mutex<HashMap<u64, TcpStream>>>) {
    let mut buf = [0u8; 1024];
    while !stop.load(Ordering::Relaxed) {
        match sock.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if tx.send_blocking(Ev::Data(id, buf[..n].to_vec())).is_err() {
                    break;
                }
            }
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut) => {}
            Err(_) => break,
        }
    }
    if let Ok(mut list) = clients.lock() {
        list.remove(&id);
    }
    if !stop.load(Ordering::Relaxed) {
        let _ = tx.send_blocking(Ev::Note(format!("master {peer} left")));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modbus::{tcp_frame, Bank, Got, Reader, Request, Response, Table};

    /// A master and a slave server talking over a real local socket.
    #[test]
    fn tcp_slave_answers_a_master() {
        let (server, rx) = Server::start("127.0.0.1", 0).unwrap_or_else(|e| panic!("{e}"));
        // Port 0 picked a free port; read it back from the Up note.
        let Ok(Ev::Up(note)) = rx.recv_blocking() else { panic!("no Up") };
        let port: u16 = note.rsplit(':').next().unwrap().parse().unwrap();
        let mut master = TcpStream::connect(("127.0.0.1", port)).unwrap();
        master.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let req = Request::Read { table: Table::Holding, addr: 5, count: 2 };
        master.write_all(&tcp_frame(42, 1, &req.pdu())).unwrap();

        // The slave side as the pane does it: read frames, answer from a bank.
        let mut bank = Bank::default();
        bank.holding[6] = 99;
        let mut reader = Reader::new(true, true);
        let mut answered = false;
        while !answered {
            match rx.recv_blocking().unwrap() {
                Ev::Data(client, data) => {
                    for g in reader.feed(&data) {
                        if let Got::Frame(f) = g {
                            server.write(client, &tcp_frame(f.transaction, f.unit, &bank.handle(&f.pdu)));
                            answered = true;
                        }
                    }
                }
                Ev::Note(_) => {}
                _ => panic!("unexpected event"),
            }
        }
        let mut buf = [0u8; 64];
        let n = master.read(&mut buf).unwrap();
        let mut back = Reader::new(true, false);
        match &back.feed(&buf[..n])[..] {
            [Got::Frame(f)] => {
                assert_eq!(f.transaction, 42);
                assert_eq!(req.parse_response(&f.pdu), Ok(Response::Registers(vec![0, 99])));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn setup_checks() {
        let mut c = ModbusCfg::default();
        assert!(c.check().is_ok());
        assert_eq!(c.title(), "Modbus master 127.0.0.1:502");
        c.unit = 0;
        assert!(c.check().is_err());
        c.role = Role::Slave;
        assert!(c.check().is_ok());
        c.transport = Transport::Rtu;
        assert!(c.check().is_err(), "no serial device set");
        let text = toml::to_string(&ModbusCfg::default()).unwrap();
        assert_eq!(toml::from_str::<ModbusCfg>(&text).unwrap(), ModbusCfg::default());
    }
}
