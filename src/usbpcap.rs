// USB capture on Windows through USBPcap (experimental). USBPcapCMD.exe
// reads one root hub and writes pcap with link type 249 to its stdout; it
// asks for administrator rights itself (UAC). Each packet becomes a
// usbmon style event, so the pane, decoder and pcap files work as on
// Linux. Link type 249 files (Wireshark on Windows) open on any system.

use crate::usb::{self, Event, HDR};

pub const LINKTYPE_USBPCAP: u32 = 249;
/// USBPCAP_BUFFER_PACKET_HEADER; control packets add a stage byte.
const BASE: usize = 27;
const STAGE_SETUP: u8 = 0;

/// Linux status (negated errno) for a USBD_STATUS.
fn status(s: u32) -> i32 {
    match s {
        0 => 0,
        // STALL_PID, ENDPOINT_HALTED
        0xC000_0004 | 0xC000_0030 => -usb::EPIPE,
        0xC001_0000 => -usb::ENOENT,
        0xC000_7000 => -usb::ESHUTDOWN,
        // CRC, DEV_NOT_RESPONDING, XACT_ERROR
        0xC000_0001 | 0xC000_0005 | 0xC000_0011 => -usb::EILSEQ,
        // DATA_OVERRUN, BABBLE_DETECTED
        0xC000_0008 | 0xC000_0012 => -usb::EOVERFLOW,
        0xC000_0009 => -usb::EREMOTEIO,
        0xC000_6000 => -usb::ETIMEDOUT,
        _ => -usb::EPROTO,
    }
}

/// One USBPcap packet (header and data) as a usbmon event. `bus` replaces
/// the packet's bus number when given.
pub fn event(pkt: &[u8], sec: i64, usec: i32, bus: Option<u16>) -> Option<Event> {
    if pkt.len() < BASE {
        return None;
    }
    let hlen = u16::from_le_bytes([pkt[0], pkt[1]]) as usize;
    if hlen < BASE || hlen > pkt.len() {
        return None;
    }
    let irp = u64::from_le_bytes(pkt[2..10].try_into().ok()?);
    let st = u32::from_le_bytes(pkt[10..14].try_into().ok()?);
    let done = pkt[16] & 1 != 0;
    let bus = bus.unwrap_or(u16::from_le_bytes([pkt[17], pkt[18]]));
    let dev = u16::from_le_bytes([pkt[19], pkt[20]]);
    let (ep, xfer) = (pkt[21], pkt[22].min(3));
    let len = u32::from_le_bytes(pkt[23..27].try_into().ok()?) as usize;
    let mut data = &pkt[hlen..];
    data = &data[..len.min(data.len())];
    let mut setup = None;
    if xfer == 2 && hlen > BASE && pkt[BASE] == STAGE_SETUP && data.len() >= 8 {
        setup = Some(&data[..8]);
        data = &data[8..];
    }
    let mut h = [0u8; HDR];
    h[0..8].copy_from_slice(&irp.to_ne_bytes());
    h[8] = if done { b'C' } else { b'S' };
    h[9] = xfer;
    h[10] = ep;
    h[11] = dev as u8;
    h[12..14].copy_from_slice(&bus.to_ne_bytes());
    h[14] = if setup.is_some() { 0 } else { b'-' };
    h[15] = if !data.is_empty() { 0 } else if ep & 0x80 != 0 { b'<' } else { b'>' };
    h[16..24].copy_from_slice(&sec.to_ne_bytes());
    h[24..28].copy_from_slice(&usec.to_ne_bytes());
    let st = if done { status(st) } else { -usb::EINPROGRESS };
    h[28..32].copy_from_slice(&st.to_ne_bytes());
    h[32..36].copy_from_slice(&(data.len() as u32).to_ne_bytes());
    h[36..40].copy_from_slice(&(data.len() as u32).to_ne_bytes());
    if let Some(s) = setup {
        h[40..48].copy_from_slice(s);
    }
    Event::parse(&h, data)
}

#[cfg_attr(not(windows), allow(dead_code))]
/// Splits a pcap byte stream (USBPcapCMD's stdout) into events.
pub struct Reader {
    buf: Vec<u8>,
    head: bool,
    bus: Option<u16>,
}

#[cfg_attr(not(windows), allow(dead_code))]
impl Reader {
    pub fn new(bus: Option<u16>) -> Self {
        Self { buf: Vec::new(), head: false, bus }
    }

    pub fn feed(&mut self, data: &[u8]) -> Result<Vec<Event>, String> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        let mut at = 0;
        if !self.head {
            if self.buf.len() < 24 {
                return Ok(out);
            }
            let le = |i: usize| u32::from_le_bytes(self.buf[i..i + 4].try_into().unwrap_or_default());
            if le(0) != 0xa1b2_c3d4 {
                return Err("USBPcapCMD did not send pcap".into());
            }
            if le(20) != LINKTYPE_USBPCAP {
                return Err(format!("link type {} is not USBPcap (249)", le(20)));
            }
            self.head = true;
            at = 24;
        }
        while self.buf.len() - at >= 16 {
            let le = |i: usize| u32::from_le_bytes(self.buf[at + i..at + i + 4].try_into().unwrap_or_default());
            let incl = le(8) as usize;
            if self.buf.len() - at - 16 < incl {
                break;
            }
            let (sec, usec) = (i64::from(le(0)), le(4) as i32);
            if let Some(e) = event(&self.buf[at + 16..at + 16 + incl], sec, usec, self.bus) {
                out.push(e);
            }
            at += 16 + incl;
        }
        self.buf.drain(..at);
        Ok(out)
    }
}

#[cfg_attr(not(windows), allow(dead_code))]
/// "{name=value}" from an extcap line.
fn field<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let start = line.find(&format!("{{{name}="))? + name.len() + 2;
    let len = line[start..].find('}')?;
    Some(&line[start..start + len])
}

#[cfg_attr(not(windows), allow(dead_code))]
/// Root hub numbers from `USBPcapCMD --extcap-interfaces`.
pub fn parse_interfaces(text: &str) -> Vec<u16> {
    text.lines()
        .filter(|l| l.starts_with("interface "))
        .filter_map(|l| field(l, "value"))
        .filter_map(|v| v.rsplit("USBPcap").next()?.parse().ok())
        .collect()
}

#[cfg_attr(not(windows), allow(dead_code))]
/// Devices of one root hub from `USBPcapCMD --extcap-config`.
pub fn parse_devices(bus: u16, text: &str) -> Vec<usb::Device> {
    let mut out = Vec::new();
    for l in text.lines().filter(|l| l.starts_with("value ") && l.contains("{enabled=true}")) {
        let Some(dev) = field(l, "value").and_then(|v| v.parse::<u8>().ok()) else { continue };
        let shown = field(l, "display").unwrap_or("");
        let name = shown.split_once("] ").map_or(shown, |(_, n)| n).trim().to_string();
        out.push(usb::Device { bus, dev, name, ..Default::default() });
    }
    out
}

#[cfg(windows)]
pub use win::{blocked, capture, devices, source, Capture};

#[cfg(windows)]
mod win {
    use std::io::{self, Read};
    use std::path::PathBuf;
    use std::process::{Child, Stdio};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::{parse_devices, parse_interfaces, Reader};
    use crate::usb::{Batch, Device};

    /// USBPcapCMD.exe: USBPcap's folder, Wireshark's extcap folder, or PATH.
    fn cmd() -> Option<PathBuf> {
        let mut found = Vec::new();
        for var in ["ProgramFiles", "ProgramW6432", "ProgramFiles(x86)"] {
            if let Some(p) = std::env::var_os(var).map(PathBuf::from) {
                found.push(p.join("USBPcap").join("USBPcapCMD.exe"));
                found.push(p.join("Wireshark").join("extcap").join("USBPcapCMD.exe"));
            }
        }
        if let Some(path) = std::env::var_os("PATH") {
            found.extend(std::env::split_paths(&path).map(|d| d.join("USBPcapCMD.exe")));
        }
        found.into_iter().find(|p| p.is_file())
    }

    fn run(args: &[&str]) -> Option<String> {
        let o = crate::sys::command(cmd()?).args(args).stdin(Stdio::null()).stderr(Stdio::null()).output().ok()?;
        Some(String::from_utf8_lossy(&o.stdout).into_owned())
    }

    /// Why capturing is not possible, or None.
    pub fn blocked() -> Option<String> {
        cmd().is_none().then(|| "USBPcap is not installed (USBPcapCMD.exe not found).".into())
    }

    pub fn source(bus: u16) -> String {
        format!(r"\\.\USBPcap{bus}")
    }

    static CACHE: Mutex<Option<(Instant, Vec<Device>)>> = Mutex::new(None);
    static BUSY: AtomicBool = AtomicBool::new(false);

    fn scan() -> Vec<Device> {
        let hubs = run(&["--extcap-interfaces"]).map(|t| parse_interfaces(&t)).unwrap_or_default();
        let mut out = Vec::new();
        for bus in hubs {
            if let Some(t) = run(&["--extcap-interface", &source(bus), "--extcap-config"]) {
                out.extend(parse_devices(bus, &t));
            }
        }
        out
    }

    /// The devices seen last; a scan runs in the background when the list
    /// is older than two seconds, so callers on the GTK thread never wait.
    pub fn devices() -> Vec<Device> {
        let (fresh, list) = match CACHE.lock().ok().and_then(|g| g.clone()) {
            Some((at, list)) => (at.elapsed() < Duration::from_secs(2), list),
            None => (false, Vec::new()),
        };
        if !fresh && !BUSY.swap(true, Ordering::AcqRel) {
            thread::spawn(|| {
                let list = scan();
                if let Ok(mut g) = CACHE.lock() {
                    *g = Some((Instant::now(), list));
                }
                BUSY.store(false, Ordering::Release);
            });
        }
        list
    }

    /// Running USBPcapCMD; dropping it ends the capture.
    pub struct Capture {
        child: Arc<Mutex<Child>>,
        stop: Arc<AtomicBool>,
    }

    impl Drop for Capture {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Ok(mut c) = self.child.lock() {
                let _ = c.kill();
            }
        }
    }

    /// Captures one root hub, or only device `dev` on it.
    pub fn capture(bus: u16, dev: Option<u8>, tx: async_channel::Sender<Batch>) -> io::Result<Capture> {
        if bus == 0 {
            return Err(io::Error::other("Windows captures one root hub at a time; pick a device on the USB page"));
        }
        let exe = cmd().ok_or_else(|| io::Error::other("USBPcapCMD.exe not found"))?;
        let mut c = crate::sys::command(exe);
        c.args(["-d", &source(bus), "-o", "-", "--inject-descriptors"]);
        match dev {
            Some(d) => c.args(["--devices", &d.to_string()]),
            None => c.arg("-A"),
        };
        let mut child = c.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn()?;
        let mut out = child.stdout.take().ok_or_else(|| io::Error::other("no stdout"))?;
        let mut err = child.stderr.take().ok_or_else(|| io::Error::other("no stderr"))?;
        let errs = thread::spawn(move || {
            let mut s = String::new();
            let _ = err.read_to_string(&mut s);
            s
        });
        let child = Arc::new(Mutex::new(child));
        let stop = Arc::new(AtomicBool::new(false));
        let (ch, st) = (child.clone(), stop.clone());
        thread::spawn(move || {
            let mut reader = Reader::new(Some(bus));
            let mut buf = vec![0u8; 256 * 1024];
            loop {
                match out.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => match reader.feed(&buf[..n]) {
                        Ok(evs) if evs.is_empty() => {}
                        Ok(evs) => {
                            if tx.send_blocking(Batch::Events(evs)).is_err() {
                                return;
                            }
                        }
                        Err(e) => {
                            let _ = tx.send_blocking(Batch::Error(e));
                            return;
                        }
                    },
                }
            }
            if st.load(Ordering::Acquire) {
                return;
            }
            let code = ch.lock().ok().and_then(|mut c| c.wait().ok());
            let msg = errs.join().unwrap_or_default();
            let msg = msg.trim();
            let why = if !msg.is_empty() {
                msg.to_string()
            } else {
                format!("USBPcapCMD ended ({}); administrator rights refused?", code.map_or("?".into(), |c| c.to_string()))
            };
            let _ = tx.send_blocking(Batch::Error(why));
        });
        Ok(Capture { child, stop })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A USBPcap packet as the driver writes it.
    fn packet(done: bool, xfer: u8, ep: u8, stage: Option<u8>, status: u32, data: &[u8]) -> Vec<u8> {
        let hlen = BASE + usize::from(stage.is_some());
        let mut p = Vec::new();
        p.extend_from_slice(&(hlen as u16).to_le_bytes());
        p.extend_from_slice(&0x1234_5678_9abc_def0u64.to_le_bytes());
        p.extend_from_slice(&status.to_le_bytes());
        p.extend_from_slice(&0x0009u16.to_le_bytes());
        p.push(u8::from(done));
        p.extend_from_slice(&1u16.to_le_bytes());
        p.extend_from_slice(&5u16.to_le_bytes());
        p.push(ep);
        p.push(xfer);
        p.extend_from_slice(&(data.len() as u32).to_le_bytes());
        if let Some(s) = stage {
            p.push(s);
        }
        p.extend_from_slice(data);
        p
    }

    #[test]
    fn packets_become_events() {
        let e = event(&packet(true, 3, 0x82, None, 0, b"OK\r\n"), 10, 20, None).unwrap();
        assert_eq!((e.kind, e.xfer, e.ep, e.dev, e.bus), (b'C', usb::Xfer::Bulk, 0x82, 5, 1));
        assert_eq!((e.data.as_slice(), e.sec, e.usec, e.id), (&b"OK\r\n"[..], 10, 20, 0x1234_5678_9abc_def0));
        assert_eq!(e.status_text(), "");
        let setup = [0x80, 6, 0, 1, 0, 0, 18, 0];
        let e = event(&packet(false, 2, 0x80, Some(STAGE_SETUP), 0, &setup), 0, 0, Some(3)).unwrap();
        assert_eq!((e.kind, e.setup, e.bus, e.data.len()), (b'S', Some(setup), 3, 0));
        let e = event(&packet(true, 1, 0x81, None, 0xC000_0004, &[]), 0, 0, None).unwrap();
        assert_eq!(e.status_text(), "STALL");
        assert!(event(&[0; 10], 0, 0, None).is_none());
    }

    #[test]
    fn reads_a_split_stream() {
        let mut s = Vec::new();
        s.extend_from_slice(&0xa1b2_c3d4u32.to_le_bytes());
        s.extend_from_slice(&2u16.to_le_bytes());
        s.extend_from_slice(&4u16.to_le_bytes());
        s.extend_from_slice(&[0; 8]);
        s.extend_from_slice(&65535u32.to_le_bytes());
        s.extend_from_slice(&LINKTYPE_USBPCAP.to_le_bytes());
        for d in [&b"a"[..], b"bc"] {
            let p = packet(true, 3, 0x81, None, 0, d);
            s.extend_from_slice(&7u32.to_le_bytes());
            s.extend_from_slice(&8u32.to_le_bytes());
            s.extend_from_slice(&(p.len() as u32).to_le_bytes());
            s.extend_from_slice(&(p.len() as u32).to_le_bytes());
            s.extend_from_slice(&p);
        }
        let mut r = Reader::new(None);
        let mut got = Vec::new();
        for chunk in s.chunks(5) {
            got.extend(r.feed(chunk).unwrap());
        }
        assert_eq!(got.iter().map(|e| e.data.clone()).collect::<Vec<_>>(), [b"a".to_vec(), b"bc".to_vec()]);
        assert!(Reader::new(None).feed(&[0u8; 24]).is_err());
        // The same bytes as a file (Wireshark on Windows).
        let path = std::env::temp_dir().join(format!("tv-usbpcap-{}.pcap", std::process::id()));
        std::fs::write(&path, &s).unwrap();
        let loaded = usb::load_pcap(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!((loaded.len(), loaded[1].data.as_slice(), loaded[1].usec), (2, &b"bc"[..], 8));
    }

    #[test]
    fn reads_extcap_output() {
        let ifs = "extcap {version=1.5.4.0}\ninterface {value=\\\\.\\USBPcap1}{display=USBPcap1}\ninterface {value=\\\\.\\USBPcap2}{display=USBPcap2}\n";
        assert_eq!(parse_interfaces(ifs), [1, 2]);
        let cfg = "arg {number=99}{call=--devices}{display=Attached USB Devices}{type=multicheck}\n\
                   value {arg=99}{value=3}{display=[3] USB Composite Device}{enabled=true}\n\
                   value {arg=99}{value=3_1}{display=USB Serial Device (COM5)}{enabled=false}{parent=3}\n\
                   value {arg=99}{value=7}{display=[7] USB Input Device}{enabled=true}{parent=2}\n";
        let d = parse_devices(1, cfg);
        assert_eq!(d.iter().map(|d| (d.bus, d.dev, d.name.as_str())).collect::<Vec<_>>(), [(1, 3, "USB Composite Device"), (1, 7, "USB Input Device")]);
    }
}
