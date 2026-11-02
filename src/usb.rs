// USB capture through the kernel's usbmon binary interface (read only; the
// traffic is not touched), pcap files in the same format, and device
// information from sysfs.
//
// Layout of an event, from drivers/usb/mon/mon_bin.c (struct mon_bin_hdr,
// 64 bytes, host byte order), followed by len_cap bytes: iso descriptors
// (16 bytes each) then data. pcap's LINKTYPE_USB_LINUX_MMAPPED uses the
// same 64 byte header.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

pub const HDR: usize = 64;
const ISODESC: usize = 16;
/// pcap link type of usbmon's 64 byte header.
pub const LINKTYPE_USB_LINUX_MMAPPED: u32 = 220;

/// Kernel ring size we ask for; one transfer is cut at a fifth of it.
const RING: libc::c_ulong = 16 * 1024 * 1024;
/// Data buffer per event (the kernel caps an event at RING / 5).
const DATA_MAX: usize = (RING / 5) as usize;

const MON_IOC_MAGIC: libc::c_ulong = 0x92;

const fn ioc(dir: libc::c_ulong, nr: libc::c_ulong, size: libc::c_ulong) -> libc::c_ulong {
    (dir << 30) | (size << 16) | (MON_IOC_MAGIC << 8) | nr
}

/// struct mon_bin_get { hdr *, data *, size_t alloc }
#[repr(C)]
struct MonGet {
    hdr: *mut u8,
    data: *mut u8,
    alloc: libc::size_t,
}

/// struct mon_bin_stats { u32 queued; u32 dropped; }
#[repr(C)]
#[derive(Default)]
struct MonStats {
    queued: u32,
    dropped: u32,
}

const MON_IOCG_STATS: libc::c_ulong = ioc(2, 3, std::mem::size_of::<MonStats>() as libc::c_ulong);
const MON_IOCT_RING_SIZE: libc::c_ulong = ioc(0, 4, 0);
const MON_IOCX_GETX: libc::c_ulong = ioc(1, 10, std::mem::size_of::<MonGet>() as libc::c_ulong);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Xfer {
    Iso,
    Interrupt,
    Control,
    Bulk,
}

impl Xfer {
    fn from(n: u8) -> Self {
        match n {
            0 => Xfer::Iso,
            1 => Xfer::Interrupt,
            2 => Xfer::Control,
            _ => Xfer::Bulk,
        }
    }

    pub fn short(self) -> &'static str {
        match self {
            Xfer::Iso => "ISO",
            Xfer::Interrupt => "INT",
            Xfer::Control => "CTRL",
            Xfer::Bulk => "BULK",
        }
    }
}

/// One usbmon event: a submission (S), completion (C) or error (E).
#[derive(Clone, Debug, PartialEq)]
pub struct Event {
    pub id: u64,
    pub kind: u8,
    pub xfer: Xfer,
    /// Endpoint number with 0x80 for IN.
    pub ep: u8,
    pub dev: u8,
    pub bus: u16,
    pub setup: Option<[u8; 8]>,
    pub sec: i64,
    pub usec: i32,
    pub status: i32,
    pub len_urb: u32,
    /// Data, without iso descriptors.
    pub data: Vec<u8>,
    /// Header and captured bytes as read, for pcap.
    pub raw: Vec<u8>,
}

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_ne_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_ne_bytes(b[i..i + 4].try_into().unwrap_or_default())
}

fn i64_at(b: &[u8], i: usize) -> i64 {
    i64::from_ne_bytes(b[i..i + 8].try_into().unwrap_or_default())
}

impl Event {
    /// Parses a 64 byte header and its captured bytes. None for filler
    /// records ('@') or a short header.
    pub fn parse(hdr: &[u8], cap: &[u8]) -> Option<Self> {
        if hdr.len() < HDR {
            return None;
        }
        let kind = hdr[8];
        if !matches!(kind, b'S' | b'C' | b'E') {
            return None;
        }
        let setup = (hdr[14] == 0).then(|| hdr[40..48].try_into().unwrap_or_default());
        let ndesc = u32_at(hdr, 60) as usize;
        let skip = (ndesc * ISODESC).min(cap.len());
        let data = if hdr[15] == 0 { cap[skip..].to_vec() } else { Vec::new() };
        let mut raw = hdr[..HDR].to_vec();
        raw.extend_from_slice(cap);
        Some(Event {
            id: u64::from_ne_bytes(hdr[0..8].try_into().unwrap_or_default()),
            kind,
            xfer: Xfer::from(hdr[9]),
            ep: hdr[10],
            dev: hdr[11],
            bus: u16_at(hdr, 12),
            setup,
            sec: i64_at(hdr, 16),
            usec: u32_at(hdr, 24) as i32,
            status: u32_at(hdr, 28) as i32,
            len_urb: u32_at(hdr, 32),
            data,
            raw,
        })
    }

    pub fn is_in(&self) -> bool {
        self.ep & 0x80 != 0
    }

    pub fn ep_num(&self) -> u8 {
        self.ep & 0x7f
    }

    /// Status in words; empty when fine.
    pub fn status_text(&self) -> &'static str {
        match -self.status {
            0 => "",
            libc::EINPROGRESS => "",
            libc::EPIPE => "STALL",
            libc::ENOENT | libc::ECONNRESET => "cancelled",
            libc::ESHUTDOWN => "device gone",
            libc::EPROTO => "protocol error",
            libc::EILSEQ => "CRC/timeout",
            libc::ETIME | libc::ETIMEDOUT => "timeout",
            libc::EOVERFLOW => "babble",
            libc::EREMOTEIO => "short packet",
            libc::ENODEV => "no device",
            libc::EXDEV => "partial iso",
            _ => "error",
        }
    }
}

/// Writes events as a pcap file (microseconds, link type 220).
pub struct PcapWriter<W: Write> {
    w: W,
}

impl<W: Write> PcapWriter<W> {
    pub fn new(mut w: W) -> io::Result<Self> {
        w.write_all(&0xa1b2c3d4u32.to_ne_bytes())?;
        w.write_all(&2u16.to_ne_bytes())?;
        w.write_all(&4u16.to_ne_bytes())?;
        w.write_all(&0i32.to_ne_bytes())?;
        w.write_all(&0u32.to_ne_bytes())?;
        w.write_all(&(HDR as u32 + DATA_MAX as u32).to_ne_bytes())?;
        w.write_all(&LINKTYPE_USB_LINUX_MMAPPED.to_ne_bytes())?;
        Ok(Self { w })
    }

    pub fn write(&mut self, e: &Event) -> io::Result<()> {
        let incl = e.raw.len() as u32;
        let descs = (e.raw.len() - HDR).saturating_sub(e.data.len());
        let orig = (HDR + descs) as u32 + e.len_urb.max(e.data.len() as u32);
        self.w.write_all(&(e.sec as u32).to_ne_bytes())?;
        self.w.write_all(&(e.usec as u32).to_ne_bytes())?;
        self.w.write_all(&incl.to_ne_bytes())?;
        self.w.write_all(&orig.to_ne_bytes())?;
        self.w.write_all(&e.raw)
    }

    pub fn finish(mut self) -> io::Result<W> {
        self.w.flush()?;
        Ok(self.w)
    }
}

pub fn save_pcap(path: &Path, events: &[Event]) -> io::Result<()> {
    let f = OpenOptions::new().create(true).truncate(true).write(true).mode(0o600).open(path)?;
    let mut w = PcapWriter::new(BufWriter::new(f))?;
    for e in events {
        w.write(e)?;
    }
    w.finish()?;
    Ok(())
}

/// Reads a pcap file with link type 220 (as written by termvisor,
/// Wireshark or tcpdump on usbmon).
pub fn load_pcap(path: &Path) -> Result<Vec<Event>, String> {
    let mut r = BufReader::new(File::open(path).map_err(|e| e.to_string())?);
    let mut g = [0u8; 24];
    r.read_exact(&mut g).map_err(|_| "not a pcap file".to_string())?;
    let magic = u32::from_le_bytes(g[0..4].try_into().unwrap_or_default());
    let swap = match magic {
        0xa1b2c3d4 | 0xa1b23c4d => false,
        0xd4c3b2a1 | 0x4d3cb2a1 => true,
        _ => return Err("not a pcap file (pcapng is not supported; save as pcap)".into()),
    };
    let nanos = matches!(magic, 0xa1b23c4d | 0x4d3cb2a1);
    let rd = |b: &[u8]| {
        let v = u32::from_le_bytes(b.try_into().unwrap_or_default());
        if swap { v.swap_bytes() } else { v }
    };
    // usbmon headers are in the byte order of the capturing machine.
    if swap {
        return Err("captured on a big-endian machine; not supported".into());
    }
    if rd(&g[20..24]) != LINKTYPE_USB_LINUX_MMAPPED {
        return Err(format!("link type {} is not Linux USB (220)", rd(&g[20..24])));
    }
    let mut out = Vec::new();
    let mut rec = [0u8; 16];
    while r.read_exact(&mut rec).is_ok() {
        let incl = rd(&rec[8..12]) as usize;
        if incl > 256 * 1024 * 1024 {
            return Err("corrupt pcap record".into());
        }
        let mut buf = vec![0u8; incl];
        r.read_exact(&mut buf).map_err(|_| "truncated pcap file".to_string())?;
        if buf.len() < HDR {
            continue;
        }
        let (hdr, cap) = buf.split_at(HDR);
        if let Some(mut e) = Event::parse(hdr, cap) {
            e.sec = i64::from(rd(&rec[0..4]));
            let frac = rd(&rec[4..8]);
            e.usec = if nanos { (frac / 1000) as i32 } else { frac as i32 };
            out.push(e);
        }
    }
    Ok(out)
}

pub const MODULES_FILE: &str = "/etc/modules-load.d/termvisor-usbmon.conf";
pub const RULES_FILE: &str = "/etc/udev/rules.d/70-termvisor-usbmon.rules";

/// One-time system setup, run as root through pkexec: load usbmon at boot
/// and give the logged-in user (udev "uaccess") read access to its device
/// files. usbmon shows all USB traffic, keyboards included, so the files
/// are never made readable for everyone.
pub fn setup_system() -> Result<(), String> {
    // SAFETY: plain getter.
    if unsafe { libc::geteuid() } != 0 {
        return Err("run as root (termvisor starts this through pkexec)".into());
    }
    fs::write(MODULES_FILE, "# Loaded for termvisor USB capture\nusbmon\n").map_err(|e| format!("{MODULES_FILE}: {e}"))?;
    fs::write(RULES_FILE, "# termvisor: the logged-in user may capture USB traffic\nSUBSYSTEM==\"usbmon\", TAG+=\"uaccess\"\n")
        .map_err(|e| format!("{RULES_FILE}: {e}"))?;
    let run = |prog: &str, args: &[&str]| -> Result<(), String> {
        let st = std::process::Command::new(prog).args(args).status().map_err(|e| format!("{prog}: {e}"))?;
        if st.success() { Ok(()) } else { Err(format!("{prog} {}: {st}", args.join(" "))) }
    };
    run("udevadm", &["control", "--reload-rules"])?;
    run("modprobe", &["usbmon"])?;
    run("udevadm", &["trigger", "--subsystem-match=usbmon", "--action=add"])?;
    run("udevadm", &["settle"])
}

pub fn monitor_path(bus: u16) -> PathBuf {
    PathBuf::from(format!("/dev/usbmon{bus}"))
}

/// Why capturing is not possible yet, or None.
pub fn blocked() -> Option<String> {
    let p = monitor_path(0);
    if !p.exists() {
        return Some("The usbmon kernel module is not loaded.".into());
    }
    match File::open(&p) {
        Ok(_) => None,
        Err(e) if e.kind() == io::ErrorKind::PermissionDenied => Some("No permission to read /dev/usbmon*.".into()),
        Err(e) => Some(format!("/dev/usbmon0: {e}")),
    }
}

/// Live capture; dropping it stops the reader.
pub struct Capture {
    stop: Arc<AtomicBool>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

pub enum Batch {
    Events(Vec<Event>),
    /// Events the kernel dropped because the ring was full (total).
    Dropped(u32),
    Error(String),
}

/// Starts reading `/dev/usbmon<bus>` (0: all buses).
pub fn capture(bus: u16, tx: async_channel::Sender<Batch>) -> io::Result<Capture> {
    let f = OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(monitor_path(bus))?;
    let fd = f.as_raw_fd();
    // SAFETY: plain ioctl on our fd; a failure keeps the default ring.
    unsafe { libc::ioctl(fd, MON_IOCT_RING_SIZE, RING) };
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    thread::spawn(move || {
        let fd = f.as_raw_fd();
        let mut hdr = [0u8; HDR];
        let mut data = vec![0u8; DATA_MAX];
        let mut batch = Vec::new();
        let mut sent = Instant::now();
        let mut dropped = 0u32;
        while !s.load(Ordering::Acquire) {
            let mut pfd = libc::pollfd { fd, events: libc::POLLIN, revents: 0 };
            // SAFETY: one valid pollfd.
            unsafe { libc::poll(&mut pfd, 1, 100) };
            loop {
                let mut get = MonGet { hdr: hdr.as_mut_ptr(), data: data.as_mut_ptr(), alloc: data.len() };
                // SAFETY: the buffers outlive the call and their sizes are given.
                let rc = unsafe { libc::ioctl(fd, MON_IOCX_GETX, &mut get) };
                if rc < 0 {
                    let e = io::Error::last_os_error();
                    if e.raw_os_error() != Some(libc::EAGAIN) && e.kind() != io::ErrorKind::Interrupted {
                        let _ = tx.send_blocking(Batch::Error(e.to_string()));
                        return;
                    }
                    break;
                }
                let cap = (u32_at(&hdr, 36) as usize).min(data.len());
                if let Some(e) = Event::parse(&hdr, &data[..cap]) {
                    batch.push(e);
                }
                if batch.len() >= 2000 {
                    break;
                }
            }
            if !batch.is_empty() && (batch.len() >= 2000 || sent.elapsed() >= Duration::from_millis(50)) {
                if tx.send_blocking(Batch::Events(std::mem::take(&mut batch))).is_err() {
                    return;
                }
                sent = Instant::now();
            }
            let mut st = MonStats::default();
            // SAFETY: MonStats matches struct mon_bin_stats.
            if unsafe { libc::ioctl(fd, MON_IOCG_STATS, &mut st) } == 0 && st.dropped != dropped {
                dropped = st.dropped;
                let _ = tx.send_blocking(Batch::Dropped(dropped));
            }
        }
    });
    Ok(Capture { stop })
}

/// Endpoint of a device, from sysfs.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Endpoint {
    pub class: u8,
    pub protocol: u8,
    pub max_packet: u16,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Device {
    pub bus: u16,
    pub dev: u8,
    pub vid: u16,
    pub pid: u16,
    pub name: String,
    /// Port path like "3-2" (sysfs name).
    pub port: String,
    /// Mb/s.
    pub speed: String,
    /// Driver names of the interfaces.
    pub drivers: Vec<String>,
    pub eps: HashMap<u8, Endpoint>,
}

impl Device {
    pub fn label(&self) -> String {
        format!("{:04x}:{:04x} {}", self.vid, self.pid, self.name)
    }
}

fn read(dir: &Path, name: &str) -> String {
    fs::read_to_string(dir.join(name)).map(|s| s.trim().to_string()).unwrap_or_default()
}

fn hex(s: &str) -> u32 {
    u32::from_str_radix(s.trim(), 16).unwrap_or(0)
}

/// USB devices under `root` (normally /sys/bus/usb/devices).
pub fn devices_in(root: &Path) -> Vec<Device> {
    let Ok(dir) = fs::read_dir(root) else { return Vec::new() };
    let mut out: Vec<Device> = Vec::new();
    for e in dir.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        // Interfaces are "3-2:1.0"; devices and root hubs have no colon.
        if name.contains(':') {
            continue;
        }
        let p = e.path();
        let bus: u16 = read(&p, "busnum").parse().unwrap_or(0);
        let dev: u8 = read(&p, "devnum").parse().unwrap_or(0);
        if bus == 0 {
            continue;
        }
        let maker = read(&p, "manufacturer");
        let product = read(&p, "product");
        let shown = match (maker.is_empty(), product.is_empty()) {
            (_, true) => maker,
            (true, false) => product,
            (false, false) if product.contains(&maker) => product,
            _ => format!("{maker} {product}"),
        };
        let mut d = Device {
            bus,
            dev,
            vid: hex(&read(&p, "idVendor")) as u16,
            pid: hex(&read(&p, "idProduct")) as u16,
            name: shown,
            port: name.clone(),
            speed: read(&p, "speed"),
            drivers: Vec::new(),
            eps: HashMap::new(),
        };
        d.eps.insert(0, Endpoint { class: hex(&read(&p, "bDeviceClass")) as u8, protocol: 0, max_packet: hex(&read(&p, "bMaxPacketSize0")) as u16 });
        if let Ok(ifs) = fs::read_dir(&p) {
            for i in ifs.flatten() {
                let iname = i.file_name().to_string_lossy().into_owned();
                if !iname.starts_with(&format!("{name}:")) {
                    continue;
                }
                let ip = i.path();
                let class = hex(&read(&ip, "bInterfaceClass")) as u8;
                let protocol = hex(&read(&ip, "bInterfaceProtocol")) as u8;
                if let Ok(link) = fs::read_link(ip.join("driver")) {
                    if let Some(n) = link.file_name() {
                        let n = n.to_string_lossy().into_owned();
                        if !d.drivers.contains(&n) {
                            d.drivers.push(n);
                        }
                    }
                }
                let Ok(eps) = fs::read_dir(&ip) else { continue };
                for ep in eps.flatten() {
                    let en = ep.file_name().to_string_lossy().into_owned();
                    if !en.starts_with("ep_") {
                        continue;
                    }
                    let addr = hex(&read(&ep.path(), "bEndpointAddress")) as u8;
                    let max_packet = hex(&read(&ep.path(), "wMaxPacketSize")) as u16 & 0x7ff;
                    d.eps.insert(addr, Endpoint { class, protocol, max_packet });
                }
            }
        }
        out.push(d);
    }
    out.sort_by_key(|d| (d.bus, d.dev));
    out
}

pub fn devices() -> Vec<Device> {
    devices_in(Path::new("/sys/bus/usb/devices"))
}

#[cfg(test)]
pub mod tests {
    use super::*;

    /// A header as the kernel writes it.
    #[allow(clippy::too_many_arguments)]
    pub fn header(kind: u8, xfer: u8, ep: u8, dev: u8, bus: u16, setup: Option<[u8; 8]>, status: i32, len_urb: u32, len_cap: u32) -> [u8; HDR] {
        let mut h = [0u8; HDR];
        h[0..8].copy_from_slice(&0xffff_8800_1234_5678u64.to_ne_bytes());
        h[8] = kind;
        h[9] = xfer;
        h[10] = ep;
        h[11] = dev;
        h[12..14].copy_from_slice(&bus.to_ne_bytes());
        h[14] = if setup.is_some() { 0 } else { b'-' };
        h[15] = if len_cap > 0 { 0 } else if ep & 0x80 != 0 { b'<' } else { b'>' };
        h[16..24].copy_from_slice(&1_700_000_000i64.to_ne_bytes());
        h[24..28].copy_from_slice(&123_456i32.to_ne_bytes());
        h[28..32].copy_from_slice(&status.to_ne_bytes());
        h[32..36].copy_from_slice(&len_urb.to_ne_bytes());
        h[36..40].copy_from_slice(&len_cap.to_ne_bytes());
        if let Some(s) = setup {
            h[40..48].copy_from_slice(&s);
        }
        h
    }

    pub fn event(kind: u8, xfer: u8, ep: u8, dev: u8, setup: Option<[u8; 8]>, status: i32, data: &[u8]) -> Event {
        let h = header(kind, xfer, ep, dev, 3, setup, status, data.len() as u32, data.len() as u32);
        Event::parse(&h, data).expect("event")
    }

    #[test]
    fn ioctl_numbers() {
        // Values a C program gets from the _IO* macros on 64 bit Linux.
        assert_eq!(MON_IOCX_GETX, 0x4018_920a);
        assert_eq!(MON_IOCG_STATS, 0x8008_9203);
        assert_eq!(MON_IOCT_RING_SIZE, 0x9204);
    }

    #[test]
    fn parse_events() {
        let setup = [0x80, 6, 0, 1, 0, 0, 18, 0];
        let e = event(b'S', 2, 0x80, 6, Some(setup), -libc::EINPROGRESS, &[]);
        assert_eq!((e.kind, e.xfer, e.dev, e.bus, e.setup), (b'S', Xfer::Control, 6, 3, Some(setup)));
        assert!(e.is_in());
        assert_eq!(e.status_text(), "");
        let e = event(b'C', 3, 0x82, 6, None, 0, b"hello\r\n");
        assert_eq!((e.xfer, e.ep_num(), e.data.as_slice()), (Xfer::Bulk, 2, &b"hello\r\n"[..]));
        let e = event(b'C', 1, 0x81, 6, None, -libc::EPIPE, &[]);
        assert_eq!(e.status_text(), "STALL");
        // Filler records are skipped.
        let mut h = header(b'S', 3, 1, 6, 3, None, 0, 0, 0);
        h[8] = b'@';
        assert!(Event::parse(&h, &[]).is_none());
        // Iso descriptors come before the data.
        let mut h = header(b'C', 0, 0x83, 6, 3, None, 0, 4, 20);
        h[60..64].copy_from_slice(&1u32.to_ne_bytes());
        let mut cap = vec![0u8; 16];
        cap.extend_from_slice(b"abcd");
        assert_eq!(Event::parse(&h, &cap).expect("iso").data, b"abcd");
    }

    #[test]
    fn pcap_roundtrip() {
        let evs = vec![
            event(b'S', 3, 0x02, 6, None, -libc::EINPROGRESS, b"AT\r"),
            event(b'C', 3, 0x82, 6, None, 0, b"OK\r\n"),
        ];
        let p = std::env::temp_dir().join(format!("tv-usb-{}.pcap", std::process::id()));
        save_pcap(&p, &evs).expect("save");
        let back = load_pcap(&p).expect("load");
        assert_eq!(back.len(), 2);
        assert_eq!(back[1].data, b"OK\r\n");
        assert_eq!((back[0].sec, back[0].usec), (1_700_000_000, 123_456));
        assert_eq!(back[0].raw, evs[0].raw);
        let _ = fs::remove_file(&p);
        assert!(load_pcap(Path::new("/nonexistent")).is_err());
    }

    #[test]
    fn sysfs_devices() {
        let root = std::env::temp_dir().join(format!("tv-sysfs-{}", std::process::id()));
        let d = root.join("3-2");
        let i = d.join("3-2:1.0");
        let ep = i.join("ep_82");
        fs::create_dir_all(&ep).expect("dirs");
        for (f, v) in [("busnum", "3"), ("devnum", "6"), ("idVendor", "1a86"), ("idProduct", "55d2"), ("product", "USB Dual_Serial"), ("speed", "12"), ("bDeviceClass", "02"), ("bMaxPacketSize0", "40")] {
            fs::write(d.join(f), format!("{v}\n")).expect("w");
        }
        fs::write(i.join("bInterfaceClass"), "0a\n").expect("w");
        fs::write(i.join("bInterfaceProtocol"), "00\n").expect("w");
        fs::write(ep.join("bEndpointAddress"), "82\n").expect("w");
        fs::write(ep.join("wMaxPacketSize"), "0040\n").expect("w");
        fs::create_dir_all(root.join("3-2:1.0")).expect("iface link stand-in");
        let devs = devices_in(&root);
        assert_eq!(devs.len(), 1);
        let dv = &devs[0];
        assert_eq!((dv.bus, dv.dev, dv.vid, dv.pid), (3, 6, 0x1a86, 0x55d2));
        assert_eq!(dv.label(), "1a86:55d2 USB Dual_Serial");
        assert_eq!(dv.eps.get(&0x82), Some(&Endpoint { class: 0x0a, protocol: 0, max_packet: 64 }));
        let _ = fs::remove_dir_all(&root);
    }
}
