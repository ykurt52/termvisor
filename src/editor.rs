// Session and folder editor dialog.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::gio;

use crate::ftp::{FtpCfg, Tls};
use crate::link::{Eol, StreamOpts, View};
use crate::net::NetCfg;
use crate::serial::{self, Flow, Parity, SerialCfg};
use crate::state::State;
use crate::mblink::{ModbusCfg, Role, Transport};
use crate::store::{self, Entry, Folder, Shell, Ssh, SshOpts};

const COLORS: &[&str] = &["Default", "Blue", "Green", "Yellow", "Red", "Magenta", "Cyan"];
const TRISTATE: &[&str] = &["Inherit", "On", "Off"];
const KINDS: &[&str] = &["SSH", "Shell", "Serial", "Telnet", "Raw TCP", "Mosh", "FTP", "Modbus"];
const MB_ROLES: &[&str] = &["Master (polls a slave)", "Slave (answers a master)"];
const MB_LINKS: &[&str] = &["RTU (serial port)", "TCP"];
const FTP_TLS: &[&str] = &["None (plain FTP)", "Explicit (FTPES, AUTH TLS)"];
const BAUDS: &[u32] = &[
    1200, 2400, 4800, 9600, 19_200, 38_400, 57_600, 115_200, 230_400, 460_800, 921_600, 1_000_000, 1_500_000,
    2_000_000, 3_000_000,
];
const DATA_BITS: &[&str] = &["5", "6", "7", "8"];
const PARITY: &[&str] = &["None", "Odd", "Even"];
const STOP_BITS: &[&str] = &["1", "2"];
const FLOW: &[&str] = &["None", "XON/XOFF", "RTS/CTS"];

/// Where the result goes.
pub enum Mode {
    /// Append to the folder at this path (root when empty).
    New(Vec<usize>),
    /// Replace the entry at this path.
    Edit(Vec<usize>),
}

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Ssh,
    Shell,
    Serial,
    Telnet,
    Tcp,
    Mosh,
    Ftp,
    Modbus,
    Folder,
}

fn entry_row(title: &str) -> adw::EntryRow {
    let r = adw::EntryRow::new();
    r.set_title(title);
    r
}

fn combo(title: &str, items: &[&str]) -> adw::ComboRow {
    let r = adw::ComboRow::new();
    r.set_title(title);
    r.set_model(Some(&gtk::StringList::new(items)));
    r
}

/// Trimmed text, None when empty.
fn text(r: &adw::EntryRow) -> Option<String> {
    let t = r.text().trim().to_string();
    (!t.is_empty()).then_some(t)
}

/// Adds a drop-down to `row` that fills it from `items()` (label, value),
/// rebuilt each time it opens.
fn chooser(row: &adw::EntryRow, tip: &str, items: impl Fn() -> Vec<(String, String)> + 'static) {
    let list = gtk::ListBox::new();
    list.add_css_class("navigation-sidebar");
    let scroll = gtk::ScrolledWindow::new();
    scroll.set_max_content_height(320);
    scroll.set_propagate_natural_height(true);
    scroll.set_propagate_natural_width(true);
    scroll.set_child(Some(&list));
    let pop = gtk::Popover::new();
    pop.set_child(Some(&scroll));

    let btn = gtk::MenuButton::new();
    btn.set_icon_name("pan-down-symbolic");
    btn.add_css_class("flat");
    btn.set_valign(gtk::Align::Center);
    btn.set_tooltip_text(Some(tip));
    btn.set_popover(Some(&pop));
    row.add_suffix(&btn);

    let values: Rc<RefCell<Vec<String>>> = Rc::default();
    let (l, vals) = (list.clone(), values.clone());
    pop.connect_show(move |_| {
        l.remove_all();
        let mut vs = vals.borrow_mut();
        vs.clear();
        let found = items();
        if found.is_empty() {
            let none = gtk::Label::new(Some("Nothing found"));
            none.add_css_class("dim-label");
            l.append(&none);
        }
        for (label, value) in found {
            let lab = gtk::Label::new(Some(&label));
            lab.set_xalign(0.0);
            l.append(&lab);
            vs.push(value);
        }
    });
    let (r, p) = (row.clone(), pop.clone());
    list.connect_row_activated(move |_, item| {
        let Ok(i) = usize::try_from(item.index()) else { return };
        if let Some(v) = values.borrow().get(i) {
            r.set_text(v);
            p.popdown();
        }
    });
}

/// Serial port rows.
#[derive(Clone)]
struct SerialRows {
    group: adw::PreferencesGroup,
    device: adw::EntryRow,
    baud: adw::EntryRow,
    data: adw::ComboRow,
    parity: adw::ComboRow,
    stop: adw::ComboRow,
    flow: adw::ComboRow,
}

/// Input and display rows shared by serial, telnet and raw TCP.
#[derive(Clone)]
struct StreamRows {
    group: adw::PreferencesGroup,
    eol: adw::ComboRow,
    echo: adw::SwitchRow,
    view: adw::ComboRow,
    reconnect: adw::SwitchRow,
}

impl StreamRows {
    fn new() -> Self {
        let eols: Vec<&str> = Eol::ALL.iter().map(|e| e.label()).collect();
        let views: Vec<&str> = View::ALL.iter().map(|v| v.label()).collect();
        let rows = Self {
            group: adw::PreferencesGroup::new(),
            eol: combo("Enter Sends", &eols),
            echo: switch("Local Echo", "Show typed text locally"),
            view: combo("Start View", &views),
            reconnect: switch("Auto Reconnect", "Reconnect when the device or host comes back"),
        };
        rows.group.set_title("Input and Display");
        rows.group.add(&rows.eol);
        rows.group.add(&rows.echo);
        rows.group.add(&rows.view);
        rows.group.add(&rows.reconnect);
        rows
    }

    fn set(&self, o: &StreamOpts) {
        self.eol.set_selected(Eol::ALL.iter().position(|e| *e == o.eol).unwrap_or(0) as u32);
        self.echo.set_active(o.echo);
        self.view.set_selected(View::ALL.iter().position(|v| *v == o.view()).unwrap_or(0) as u32);
        self.reconnect.set_active(o.reconnect);
    }

    fn get(&self) -> StreamOpts {
        let view = View::ALL.get(self.view.selected() as usize).copied().unwrap_or_default();
        StreamOpts {
            eol: Eol::ALL.get(self.eol.selected() as usize).copied().unwrap_or_default(),
            echo: self.echo.is_active(),
            hex: view == View::Hex,
            mavlink: view == View::Mavlink,
            reconnect: self.reconnect.is_active(),
        }
    }
}

/// FTP server rows.
#[derive(Clone)]
struct FtpRows {
    group: adw::PreferencesGroup,
    host: adw::EntryRow,
    port: adw::EntryRow,
    user: adw::EntryRow,
    tls: adw::ComboRow,
    passive: adw::SwitchRow,
    insecure: adw::SwitchRow,
    remember: adw::SwitchRow,
}

impl FtpRows {
    fn new() -> Self {
        let rows = Self {
            group: adw::PreferencesGroup::new(),
            host: entry_row("Host"),
            port: entry_row("Port"),
            user: entry_row("User (empty = anonymous)"),
            tls: combo("Encryption", FTP_TLS),
            passive: switch("Passive Mode", "Needed behind most NAT and firewalls"),
            insecure: switch("Accept Any Certificate", "For devices with self-signed certificates"),
            remember: switch("Remember Password", "Store the password in the system keyring"),
        };
        rows.group.set_title("FTP Server");
        rows.port.set_input_purpose(gtk::InputPurpose::Digits);
        for r in [
            rows.host.upcast_ref::<gtk::Widget>(),
            rows.port.upcast_ref(),
            rows.user.upcast_ref(),
            rows.tls.upcast_ref(),
            rows.passive.upcast_ref(),
            rows.insecure.upcast_ref(),
            rows.remember.upcast_ref(),
        ] {
            rows.group.add(r);
        }
        rows.set(&FtpCfg::default(), false);
        rows
    }

    fn set(&self, c: &FtpCfg, no_store: bool) {
        self.host.set_text(&c.host);
        self.port.set_text(&c.port.to_string());
        self.user.set_text(&c.user);
        self.tls.set_selected(if c.tls == Tls::Explicit { 1 } else { 0 });
        self.passive.set_active(c.passive);
        self.insecure.set_active(c.insecure);
        self.remember.set_active(!no_store);
    }

    fn get(&self) -> Result<FtpCfg, String> {
        let c = FtpCfg {
            host: text(&self.host).ok_or("Host is required")?,
            port: text(&self.port).and_then(|p| p.parse().ok()).filter(|p| *p > 0).ok_or("Port must be 1-65535")?,
            user: text(&self.user).unwrap_or_default(),
            tls: if self.tls.selected() == 1 { Tls::Explicit } else { Tls::None },
            passive: self.passive.is_active(),
            insecure: self.insecure.is_active(),
        };
        c.check()?;
        Ok(c)
    }
}

/// Host and port rows for telnet and raw TCP.
#[derive(Clone)]
struct NetRows {
    group: adw::PreferencesGroup,
    host: adw::EntryRow,
    port: adw::EntryRow,
}

impl NetRows {
    fn new() -> Self {
        let rows = Self { group: adw::PreferencesGroup::new(), host: entry_row("Host"), port: entry_row("Port") };
        rows.group.set_title("Network");
        rows.port.set_input_purpose(gtk::InputPurpose::Digits);
        rows.group.add(&rows.host);
        rows.group.add(&rows.port);
        rows
    }

    fn set(&self, c: &NetCfg) {
        self.host.set_text(&c.host);
        self.port.set_text(&c.port.to_string());
    }

    fn get(&self, opts: StreamOpts) -> Result<NetCfg, String> {
        let host = text(&self.host).ok_or("Host is required")?;
        let port = text(&self.port)
            .and_then(|p| p.parse::<u16>().ok())
            .filter(|p| *p > 0)
            .ok_or("Port must be 1-65535")?;
        let c = NetCfg { host, port, opts };
        c.check()?;
        Ok(c)
    }
}

fn switch(title: &str, sub: &str) -> adw::SwitchRow {
    let r = adw::SwitchRow::new();
    r.set_title(title);
    r.set_subtitle(sub);
    r
}

impl SerialRows {
    fn new() -> Self {
        let rows = Self {
            group: adw::PreferencesGroup::new(),
            device: entry_row("Device"),
            baud: entry_row("Baud Rate"),
            data: combo("Data Bits", DATA_BITS),
            parity: combo("Parity", PARITY),
            stop: combo("Stop Bits", STOP_BITS),
            flow: combo("Flow Control", FLOW),
        };
        rows.group.set_title("Serial Port");
        rows.baud.set_input_purpose(gtk::InputPurpose::Digits);
        chooser(&rows.device, "Detected Ports", || {
            serial::ports().into_iter().map(|p| (p.label, p.path)).collect()
        });
        chooser(&rows.baud, "Common Rates", || {
            BAUDS.iter().map(|b| (b.to_string(), b.to_string())).collect()
        });
        for r in [
            rows.device.upcast_ref::<gtk::Widget>(),
            rows.baud.upcast_ref(),
            rows.data.upcast_ref(),
            rows.parity.upcast_ref(),
            rows.stop.upcast_ref(),
            rows.flow.upcast_ref(),
        ] {
            rows.group.add(r);
        }
        rows
    }

    fn set(&self, c: &SerialCfg) {
        self.device.set_text(&c.device);
        self.baud.set_text(&c.baud.to_string());
        self.data.set_selected(u32::from(c.data_bits.clamp(5, 8) - 5));
        self.parity.set_selected(match c.parity {
            Parity::None => 0,
            Parity::Odd => 1,
            Parity::Even => 2,
        });
        self.stop.set_selected(if c.stop_bits == 2 { 1 } else { 0 });
        self.flow.set_selected(match c.flow {
            Flow::None => 0,
            Flow::Software => 1,
            Flow::Hardware => 2,
        });
    }

    fn get(&self, opts: StreamOpts) -> Result<SerialCfg, String> {
        let device = text(&self.device).ok_or("Device is required")?;
        let baud = text(&self.baud)
            .and_then(|b| b.parse::<u32>().ok())
            .filter(|b| *b > 0)
            .ok_or("Baud rate must be a positive number")?;
        let c = SerialCfg {
            device,
            baud,
            data_bits: 5 + self.data.selected().min(3) as u8,
            parity: match self.parity.selected() {
                1 => Parity::Odd,
                2 => Parity::Even,
                _ => Parity::None,
            },
            stop_bits: if self.stop.selected() == 1 { 2 } else { 1 },
            flow: match self.flow.selected() {
                1 => Flow::Software,
                2 => Flow::Hardware,
                _ => Flow::None,
            },
            opts,
        };
        c.check()?;
        Ok(c)
    }

    fn texts(&self) -> [&adw::EntryRow; 2] {
        [&self.device, &self.baud]
    }
}

/// SSH option rows, shared by sessions and folder defaults.
#[derive(Clone)]
struct OptRows {
    port: adw::EntryRow,
    user: adw::EntryRow,
    key: adw::EntryRow,
    jump: adw::EntryRow,
    x11: adw::ComboRow,
    args: adw::EntryRow,
}

impl OptRows {
    fn new() -> Self {
        let rows = Self {
            port: entry_row("Port"),
            user: entry_row("User"),
            key: entry_row("Identity File"),
            jump: entry_row("Jump Host"),
            x11: combo("X11 Forwarding", TRISTATE),
            args: entry_row("Extra Arguments"),
        };
        rows.port.set_input_purpose(gtk::InputPurpose::Digits);

        let pick = gtk::Button::from_icon_name("document-open-symbolic");
        pick.add_css_class("flat");
        pick.set_valign(gtk::Align::Center);
        pick.set_tooltip_text(Some("Choose File"));
        let key = rows.key.clone();
        pick.connect_clicked(move |b| {
            let win = b.root().and_downcast::<gtk::Window>();
            let key = key.clone();
            gtk::FileDialog::new().open(win.as_ref(), None::<&gio::Cancellable>, move |res| {
                if let Some(p) = res.ok().and_then(|f| f.path()) {
                    key.set_text(&p.to_string_lossy());
                }
            });
        });
        rows.key.add_suffix(&pick);
        rows
    }

    fn add_to(&self, g: &adw::PreferencesGroup) {
        g.add(&self.port);
        g.add(&self.user);
        g.add(&self.key);
        g.add(&self.jump);
        g.add(&self.x11);
        g.add(&self.args);
    }

    fn texts(&self) -> [&adw::EntryRow; 5] {
        [&self.port, &self.user, &self.key, &self.jump, &self.args]
    }

    fn set(&self, o: &SshOpts) {
        self.port.set_text(&o.port.map(|p| p.to_string()).unwrap_or_default());
        self.user.set_text(o.user.as_deref().unwrap_or(""));
        self.key.set_text(o.key.as_deref().unwrap_or(""));
        self.jump.set_text(o.jump.as_deref().unwrap_or(""));
        self.x11.set_selected(match o.x11 {
            None => 0,
            Some(true) => 1,
            Some(false) => 2,
        });
        self.args.set_text(&o.args.as_ref().map(|a| a.join(" ")).unwrap_or_default());
    }

    fn get(&self) -> Result<SshOpts, String> {
        let port = match text(&self.port) {
            None => None,
            Some(t) => match t.parse::<u16>() {
                Ok(p) if p > 0 => Some(p),
                _ => return Err("Port must be 1-65535".into()),
            },
        };
        let args = text(&self.args).map(|t| t.split_whitespace().map(str::to_string).collect());
        Ok(SshOpts {
            port,
            user: text(&self.user),
            key: text(&self.key),
            jump: text(&self.jump),
            x11: match self.x11.selected() {
                1 => Some(true),
                2 => Some(false),
                _ => None,
            },
            args,
        })
    }
}

/// Modbus rows; the port comes from the serial or network rows.
#[derive(Clone)]
struct ModbusRows {
    group: adw::PreferencesGroup,
    role: adw::ComboRow,
    link: adw::ComboRow,
    unit: adw::EntryRow,
    timeout: adw::EntryRow,
}

impl ModbusRows {
    fn new() -> Self {
        let rows = Self {
            group: adw::PreferencesGroup::new(),
            role: combo("Role", MB_ROLES),
            link: combo("Link", MB_LINKS),
            unit: entry_row("Unit ID"),
            timeout: entry_row("Reply Timeout (ms)"),
        };
        rows.group.set_title("Modbus");
        rows.unit.set_input_purpose(gtk::InputPurpose::Digits);
        rows.timeout.set_input_purpose(gtk::InputPurpose::Digits);
        for r in [rows.role.upcast_ref::<gtk::Widget>(), rows.link.upcast_ref(), rows.unit.upcast_ref(), rows.timeout.upcast_ref()] {
            rows.group.add(r);
        }
        rows
    }

    fn slave(&self) -> bool {
        self.role.selected() == 1
    }

    fn rtu(&self) -> bool {
        self.link.selected() == 0
    }

    fn set(&self, c: &ModbusCfg) {
        self.role.set_selected(if c.role == Role::Slave { 1 } else { 0 });
        self.link.set_selected(if c.transport == Transport::Rtu { 0 } else { 1 });
        self.unit.set_text(&c.unit.to_string());
        self.timeout.set_text(&c.timeout_ms.to_string());
    }

    fn get(&self, serial: &SerialRows, net: &NetRows) -> Result<ModbusCfg, String> {
        let unit = text(&self.unit).and_then(|u| u.parse::<u8>().ok()).ok_or("Unit ID must be 0-247")?;
        let timeout_ms = text(&self.timeout).and_then(|t| t.parse::<u32>().ok()).filter(|t| *t >= 50).ok_or("Reply timeout must be at least 50 ms")?;
        let mut c = ModbusCfg {
            role: if self.slave() { Role::Slave } else { Role::Master },
            transport: if self.rtu() { Transport::Rtu } else { Transport::Tcp },
            unit,
            timeout_ms,
            ..ModbusCfg::default()
        };
        if self.rtu() {
            c.serial = serial.get(StreamOpts::default())?;
        } else {
            let n = net.get(StreamOpts::default())?;
            c.host = n.host;
            c.port = n.port;
        }
        c.check()?;
        Ok(c)
    }
}

#[derive(Clone)]
struct Form {
    kind: adw::ComboRow,
    name: adw::EntryRow,
    color: adw::ComboRow,
    favorite: adw::SwitchRow,
    remember: adw::SwitchRow,
    conn: adw::PreferencesGroup,
    host: adw::EntryRow,
    opts: OptRows,
    shell: adw::PreferencesGroup,
    dir: adw::EntryRow,
    serial: SerialRows,
    net: NetRows,
    stream: StreamRows,
    ftp: FtpRows,
    modbus: ModbusRows,
    folder: bool,
}

impl Form {
    fn kind(&self) -> Kind {
        if self.folder {
            Kind::Folder
        } else {
            match self.kind.selected() {
                1 => Kind::Shell,
                2 => Kind::Serial,
                3 => Kind::Telnet,
                4 => Kind::Tcp,
                5 => Kind::Mosh,
                6 => Kind::Ftp,
                7 => Kind::Modbus,
                _ => Kind::Ssh,
            }
        }
    }

    fn sync(&self) {
        let k = self.kind();
        let ssh_like = k == Kind::Ssh || k == Kind::Mosh;
        let net = k == Kind::Telnet || k == Kind::Tcp;
        let mb = k == Kind::Modbus;
        self.host.set_visible(ssh_like);
        self.conn.set_visible(ssh_like || k == Kind::Folder);
        self.serial.group.set_visible(k == Kind::Serial || mb && self.modbus.rtu());
        self.net.group.set_visible(net || mb && !self.modbus.rtu());
        self.stream.group.set_visible(net || k == Kind::Serial);
        self.ftp.group.set_visible(k == Kind::Ftp);
        self.modbus.group.set_visible(mb);
        if mb && self.net.port.text().is_empty() {
            self.net.port.set_text("502");
        }
        // A slave listens: the host is the address to listen on.
        self.net.host.set_title(if mb && self.modbus.slave() { "Listen Address (0.0.0.0: all)" } else { "Host" });
        if k == Kind::Telnet && self.net.port.text().is_empty() {
            self.net.port.set_text("23");
        }
        self.conn.set_title(if k == Kind::Folder { "Defaults for Sessions Inside" } else { "Connection" });
        self.shell.set_visible(k == Kind::Shell);
        self.favorite.set_visible(k != Kind::Folder);
        self.remember.set_visible(ssh_like);
    }

    fn load(&self, e: &Entry) {
        self.name.set_text(e.name());
        let color = e
            .color()
            .and_then(|c| COLORS.iter().position(|x| x.eq_ignore_ascii_case(c)))
            .unwrap_or(0);
        self.color.set_selected(color as u32);
        self.favorite.set_active(e.favorite());
        match e {
            Entry::Ssh(s) | Entry::Mosh(s) => {
                self.kind.set_selected(if matches!(e, Entry::Mosh(_)) { 5 } else { 0 });
                self.host.set_text(&s.host);
                self.opts.set(&s.opts);
                self.remember.set_active(!s.no_store);
            }
            Entry::Ftp(f) => {
                self.kind.set_selected(6);
                self.ftp.set(&f.cfg, f.no_store);
            }
            Entry::Telnet(n) | Entry::Tcp(n) => {
                self.kind.set_selected(if matches!(e, Entry::Telnet(_)) { 3 } else { 4 });
                self.net.set(&n.cfg);
                self.stream.set(&n.cfg.opts);
            }
            Entry::Shell(s) => {
                self.kind.set_selected(1);
                self.dir.set_text(s.dir.as_deref().unwrap_or(""));
            }
            Entry::Serial(s) => {
                self.kind.set_selected(2);
                self.serial.set(&s.cfg);
                self.stream.set(&s.cfg.opts);
            }
            Entry::Modbus(m) => {
                self.kind.set_selected(7);
                self.modbus.set(&m.cfg);
                self.serial.set(&m.cfg.serial);
                self.net.host.set_text(&m.cfg.host);
                self.net.port.set_text(&m.cfg.port.to_string());
            }
            Entry::Folder(f) => self.opts.set(&f.defaults),
        }
    }

    fn build(&self) -> Result<Entry, String> {
        let name = text(&self.name).ok_or("Name is required")?;
        let color = match self.color.selected() as usize {
            0 => None,
            i => COLORS.get(i).map(|c| c.to_lowercase()),
        };
        let favorite = self.favorite.is_active();
        let ssh = |name: String, color: Option<String>| -> Result<Ssh, String> {
            Ok(Ssh {
                name,
                host: text(&self.host).ok_or("Host is required")?,
                opts: self.opts.get()?,
                color,
                favorite,
                no_store: !self.remember.is_active(),
            })
        };
        let net = |name: String, color: Option<String>| -> Result<store::Net, String> {
            Ok(store::Net { name, cfg: self.net.get(self.stream.get())?, color, favorite })
        };
        let e = match self.kind() {
            Kind::Ssh => Entry::Ssh(ssh(name, color)?),
            Kind::Mosh => Entry::Mosh(ssh(name, color)?),
            Kind::Telnet => Entry::Telnet(net(name, color)?),
            Kind::Tcp => Entry::Tcp(net(name, color)?),
            Kind::Ftp => Entry::Ftp(store::FtpEntry {
                name,
                cfg: self.ftp.get()?,
                color,
                favorite,
                no_store: !self.ftp.remember.is_active(),
            }),
            Kind::Modbus => Entry::Modbus(store::ModbusEntry { name, cfg: self.modbus.get(&self.serial, &self.net)?, color, favorite }),
            Kind::Shell => Entry::Shell(Shell { name, dir: text(&self.dir), color, favorite }),
            Kind::Serial => Entry::Serial(store::Serial { name, cfg: self.serial.get(self.stream.get())?, color, favorite }),
            Kind::Folder => Entry::Folder(Folder {
                name,
                color,
                defaults: self.opts.get()?,
                children: Vec::new(),
            }),
        };
        // Same checks as launch time, so bad values never reach the file.
        if !matches!(e, Entry::Folder(_)) {
            store::resolve(std::slice::from_ref(&e), &[0])?;
        }
        Ok(e)
    }
}

/// Serial defaults with the first detected port.
pub fn default_serial() -> SerialCfg {
    let device = serial::ports().into_iter().next().map(|p| p.path).unwrap_or_default();
    SerialCfg { device, ..Default::default() }
}

/// Opens the editor for `init` and saves into `state` on Save.
pub fn open(parent: &impl IsA<gtk::Widget>, state: &Rc<State>, mode: Mode, init: Entry) {
    let folder = matches!(init, Entry::Folder(_));
    let editing = matches!(mode, Mode::Edit(_));

    let general = adw::PreferencesGroup::new();
    let form = Form {
        kind: combo("Type", KINDS),
        name: entry_row("Name"),
        color: combo("Color", COLORS),
        favorite: adw::SwitchRow::new(),
        remember: adw::SwitchRow::new(),
        conn: adw::PreferencesGroup::new(),
        host: entry_row("Host"),
        opts: OptRows::new(),
        shell: adw::PreferencesGroup::new(),
        dir: entry_row("Working Directory"),
        serial: SerialRows::new(),
        net: NetRows::new(),
        stream: StreamRows::new(),
        ftp: FtpRows::new(),
        modbus: ModbusRows::new(),
        folder,
    };
    form.conn.add(&form.host);
    form.opts.add_to(&form.conn);
    form.remember.set_title("Remember Password");
    form.remember.set_subtitle("Store passwords and passphrases in the system keyring");
    form.remember.set_active(true);
    form.conn.add(&form.remember);
    form.favorite.set_title("Favorite");
    form.shell.set_title("Shell");
    form.shell.add(&form.dir);

    general.add(&form.name);
    if !folder {
        general.add(&form.kind);
        form.kind.set_sensitive(!editing);
    }
    general.add(&form.color);
    general.add(&form.favorite);

    let page = adw::PreferencesPage::new();
    page.add(&general);
    page.add(&form.conn);
    page.add(&form.shell);
    page.add(&form.serial.group);
    page.add(&form.net.group);
    page.add(&form.stream.group);
    page.add(&form.modbus.group);
    page.add(&form.ftp.group);

    form.serial.set(&default_serial());
    form.stream.set(&StreamOpts::default());
    form.modbus.set(&ModbusCfg::default());
    form.load(&init);
    form.sync();
    let f = form.clone();
    form.kind.connect_selected_notify(move |_| f.sync());
    for row in [&form.modbus.role, &form.modbus.link] {
        let f = form.clone();
        row.connect_selected_notify(move |_| f.sync());
    }

    let toasts = adw::ToastOverlay::new();
    toasts.set_child(Some(&page));

    let cancel = gtk::Button::with_label("Cancel");
    let save = gtk::Button::with_label("Save");
    save.add_css_class("suggested-action");
    let header = adw::HeaderBar::new();
    header.set_show_start_title_buttons(false);
    header.set_show_end_title_buttons(false);
    header.pack_start(&cancel);
    header.pack_end(&save);

    let body = adw::ToolbarView::new();
    body.add_top_bar(&header);
    body.set_content(Some(&toasts));

    let what = if folder { "Folder" } else { "Session" };
    let dlg = adw::Dialog::new();
    dlg.set_title(&format!("{} {what}", if editing { "Edit" } else { "New" }));
    dlg.set_content_width(480);
    dlg.set_content_height(620);
    dlg.set_child(Some(&body));

    let d = dlg.clone();
    cancel.connect_clicked(move |_| {
        d.close();
    });

    let d = dlg.clone();
    let st = state.clone();
    let mode = Rc::new(mode);
    let fm = form.clone();
    let submit = Rc::new(move || {
        let res = fm.build().and_then(|e| {
            st.edit_sessions(|s| match &*mode {
                Mode::New(dir) => s.insert(dir, None, e),
                Mode::Edit(path) => s.replace(path, e),
            })
        });
        match res {
            Ok(()) => {
                d.close();
            }
            Err(e) => toasts.add_toast(adw::Toast::new(&e)),
        }
    });
    let s = submit.clone();
    save.connect_clicked(move |_| s());
    // Enter in any text row saves.
    let mut texts = vec![&form.name, &form.host, &form.dir];
    texts.extend(form.opts.texts());
    texts.extend(form.serial.texts());
    texts.extend([&form.net.host, &form.net.port, &form.ftp.host, &form.ftp.port, &form.ftp.user, &form.modbus.unit, &form.modbus.timeout]);
    for r in texts {
        let s = submit.clone();
        r.connect_entry_activated(move |_| s());
    }

    dlg.present(Some(parent));
}
