// Decoders for captured USB transfers: control requests and descriptors,
// serial adapters (CDC-ACM, FTDI, CH34x, CP210x, PL2303), HID keyboards
// and mice, and USB mass storage (bulk-only SCSI).

use std::collections::HashMap;

use crate::usb::{Device, Event, Xfer};

const SERIAL_VENDORS: [u16; 4] = [0x0403, 0x1a86, 0x10c4, 0x067b];
const FTDI: u16 = 0x0403;

/// Text with control characters escaped, at most `max` characters.
pub fn printable(data: &[u8], max: usize) -> String {
    let mut s = String::new();
    for &b in data.iter().take(max) {
        match b {
            b'\r' => s.push_str("\\r"),
            b'\n' => s.push_str("\\n"),
            b'\t' => s.push_str("\\t"),
            0x20..=0x7e => s.push(b as char),
            _ => s.push_str(&format!("\\x{b:02x}")),
        }
    }
    if data.len() > max {
        s.push('…');
    }
    s
}

fn mostly_text(data: &[u8]) -> bool {
    !data.is_empty() && data.iter().filter(|b| matches!(b, 0x20..=0x7e | b'\r' | b'\n' | b'\t')).count() * 10 >= data.len() * 8
}

fn hex_short(data: &[u8], max: usize) -> String {
    let mut s: Vec<String> = data.iter().take(max).map(|b| format!("{b:02x}")).collect();
    if data.len() > max {
        s.push("…".into());
    }
    s.join(" ")
}

fn le16(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn le32(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn be32(b: &[u8], i: usize) -> u32 {
    u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

fn class_name(c: u8) -> &'static str {
    match c {
        0x00 => "per interface",
        0x01 => "audio",
        0x02 => "CDC control",
        0x03 => "HID",
        0x05 => "physical",
        0x06 => "image",
        0x07 => "printer",
        0x08 => "mass storage",
        0x09 => "hub",
        0x0a => "CDC data",
        0x0b => "smart card",
        0x0e => "video",
        0x0f => "healthcare",
        0x10 => "audio/video",
        0xdc => "diagnostic",
        0xe0 => "wireless",
        0xef => "misc",
        0xfe => "application",
        0xff => "vendor",
        _ => "class",
    }
}

fn descriptor_name(t: u8) -> String {
    match t {
        1 => "DEVICE".into(),
        2 => "CONFIGURATION".into(),
        3 => "STRING".into(),
        6 => "DEVICE_QUALIFIER".into(),
        7 => "OTHER_SPEED_CONFIG".into(),
        15 => "BOS".into(),
        0x21 => "HID".into(),
        0x22 => "HID REPORT".into(),
        _ => format!("type 0x{t:02x}"),
    }
}

/// Line coding (CDC SET/GET_LINE_CODING data): "115200 8N1".
fn line_coding(d: &[u8]) -> Option<String> {
    if d.len() < 7 {
        return None;
    }
    let stop = match d[4] {
        0 => "1",
        1 => "1.5",
        _ => "2",
    };
    let parity = ['N', 'O', 'E', 'M', 'S'].get(d[5] as usize).copied().unwrap_or('?');
    Some(format!("{} {}{parity}{stop}", le32(d, 0), d[6]))
}

/// A control request in words.
pub fn setup_text(s: &[u8; 8], dev: Option<&Device>, out: &[u8]) -> String {
    let (rt, req) = (s[0], s[1]);
    let (value, index, len) = (le16(s, 2), le16(s, 4), le16(s, 6));
    let kind = (rt >> 5) & 3;
    let classes: Vec<u8> = dev.map(|d| d.eps.values().map(|e| e.class).collect()).unwrap_or_default();
    // Without device information (a file from another machine), class
    // requests are named by number; CDC, HID and MSC numbers do not overlap.
    let has = |c: u8| dev.is_none() || classes.contains(&c);
    match (kind, req) {
        (0, 0) => format!("GET_STATUS len {len}"),
        (0, 1) if rt & 0x1f == 2 && value == 0 => format!("CLEAR_FEATURE ENDPOINT_HALT ep 0x{index:02x}"),
        (0, 1) => format!("CLEAR_FEATURE {value}"),
        (0, 3) => format!("SET_FEATURE {value}"),
        (0, 5) => format!("SET_ADDRESS {value}"),
        (0, 6) => {
            let t = descriptor_name((value >> 8) as u8);
            let i = value & 0xff;
            if i == 0 { format!("GET_DESCRIPTOR {t} len {len}") } else { format!("GET_DESCRIPTOR {t} #{i} len {len}") }
        }
        (0, 8) => "GET_CONFIGURATION".into(),
        (0, 9) => format!("SET_CONFIGURATION {value}"),
        (0, 10) => format!("GET_INTERFACE {index}"),
        (0, 11) => format!("SET_INTERFACE if {index} alt {value}"),
        (1, 0x20) if has(0x02) || has(0x0a) => {
            format!("CDC SET_LINE_CODING {}", line_coding(out).unwrap_or_else(|| format!("len {len}")))
        }
        (1, 0x21) if has(0x02) || has(0x0a) => "CDC GET_LINE_CODING".into(),
        (1, 0x22) if has(0x02) || has(0x0a) => {
            let on = |b: u16| if value & b != 0 { "on" } else { "off" };
            format!("CDC SET_CONTROL_LINE_STATE DTR {} RTS {}", on(1), on(2))
        }
        (1, 0x23) if has(0x02) || has(0x0a) => format!("CDC SEND_BREAK {value} ms"),
        (1, 0x01) if has(0x03) => format!("HID GET_REPORT id {} len {len}", value & 0xff),
        (1, 0x09) if has(0x03) => format!("HID SET_REPORT id {} {}", value & 0xff, hex_short(out, 8)),
        (1, 0x0a) if has(0x03) => format!("HID SET_IDLE {} ms", (value >> 8) * 4),
        (1, 0x0b) if has(0x03) => format!("HID SET_PROTOCOL {}", if value == 0 { "boot" } else { "report" }),
        (1, 0xfe) if has(0x08) => "MSC GET_MAX_LUN".into(),
        (1, 0xff) if has(0x08) => "MSC BULK_ONLY_RESET".into(),
        (2, r) if dev.is_some_and(|d| d.vid == FTDI) => {
            let name = match r {
                0 => "RESET",
                1 => "SET_MODEM_CTRL",
                2 => "SET_FLOW_CTRL",
                3 => "SET_BAUDRATE",
                4 => "SET_DATA",
                5 => "GET_MODEM_STATUS",
                9 => "SET_LATENCY",
                _ => "request",
            };
            format!("FTDI {name} value 0x{value:04x} index 0x{index:04x}")
        }
        (k, r) => {
            let who = ["", "class ", "vendor ", "reserved "][k as usize];
            format!("{who}request 0x{r:02x} value 0x{value:04x} index 0x{index:04x} len {len}")
        }
    }
}

/// Descriptor returned for GET_DESCRIPTOR.
fn descriptor_text(s: &[u8; 8], d: &[u8]) -> Option<String> {
    let t = (le16(s, 2) >> 8) as u8;
    match t {
        1 if d.len() >= 18 => Some(format!(
            "DEVICE USB {}.{:x} class {} ({}) {:04x}:{:04x} max packet {} configs {}",
            d[3],
            d[2] >> 4,
            d[4],
            class_name(d[4]),
            le16(d, 8),
            le16(d, 10),
            d[7],
            d[17]
        )),
        2 if d.len() >= 9 => {
            let mut ifs = Vec::new();
            let mut i = 0;
            while i + 2 <= d.len() && d[i] >= 2 {
                if d[i + 1] == 4 && i + 9 <= d.len() {
                    ifs.push(format!("if {} {}", d[i + 2], class_name(d[i + 5])));
                }
                i += d[i] as usize;
            }
            let total = le16(d, 2);
            if ifs.is_empty() { Some(format!("CONFIGURATION total {total}")) } else { Some(format!("CONFIGURATION total {total}: {}", ifs.join(", "))) }
        }
        3 if d.len() >= 2 && le16(s, 2) & 0xff == 0 => Some("STRING languages".into()),
        3 if d.len() >= 2 => {
            let n = (d[0] as usize).min(d.len());
            let units: Vec<u16> = d[2..n].as_chunks::<2>().0.iter().map(|c| u16::from_le_bytes(*c)).collect();
            Some(format!("STRING \"{}\"", String::from_utf16_lossy(&units)))
        }
        _ => None,
    }
}

fn hid_key(code: u8) -> String {
    match code {
        4..=29 => ((b'a' + code - 4) as char).to_string(),
        30..=38 => ((b'1' + code - 30) as char).to_string(),
        39 => "0".into(),
        40 => "Enter".into(),
        41 => "Esc".into(),
        42 => "Backspace".into(),
        43 => "Tab".into(),
        44 => "Space".into(),
        45 => "-".into(),
        46 => "=".into(),
        47 => "[".into(),
        48 => "]".into(),
        49 => "\\".into(),
        51 => ";".into(),
        52 => "'".into(),
        53 => "`".into(),
        54 => ",".into(),
        55 => ".".into(),
        56 => "/".into(),
        57 => "CapsLock".into(),
        58..=69 => format!("F{}", code - 57),
        73 => "Insert".into(),
        74 => "Home".into(),
        75 => "PgUp".into(),
        76 => "Delete".into(),
        77 => "End".into(),
        78 => "PgDn".into(),
        79 => "Right".into(),
        80 => "Left".into(),
        81 => "Down".into(),
        82 => "Up".into(),
        _ => format!("0x{code:02x}"),
    }
}

/// HID boot keyboard (8 bytes) or mouse (3+ bytes) report.
fn hid_text(protocol: u8, d: &[u8]) -> Option<String> {
    match protocol {
        1 if d.len() >= 8 => {
            let mods = ["LCtrl", "LShift", "LAlt", "LSuper", "RCtrl", "RShift", "RAlt", "RSuper"];
            let mut keys: Vec<String> = (0..8).filter(|b| d[0] & (1 << b) != 0).map(|b| mods[b].to_string()).collect();
            keys.extend(d[2..8].iter().filter(|&&k| k > 1).map(|&k| hid_key(k)));
            Some(if keys.is_empty() { "keyboard: released".into() } else { format!("keyboard: {}", keys.join("+")) })
        }
        2 if d.len() >= 3 => {
            let b = ["left", "right", "middle"];
            let pressed: Vec<&str> = (0..3).filter(|i| d[0] & (1 << i) != 0).map(|i| b[i]).collect();
            let wheel = d.get(3).map(|w| *w as i8).filter(|w| *w != 0).map(|w| format!(" wheel {w}")).unwrap_or_default();
            Some(format!("mouse: dx {} dy {}{}{}", d[1] as i8, d[2] as i8, wheel, if pressed.is_empty() { String::new() } else { format!(" [{}]", pressed.join(",")) }))
        }
        _ => None,
    }
}

fn scsi(cdb: &[u8]) -> String {
    let op = cdb.first().copied().unwrap_or(0);
    let rw10 = |name: &str| {
        if cdb.len() >= 10 {
            format!("{name} lba {} blocks {}", be32(cdb, 2), u16::from_be_bytes([cdb[7], cdb[8]]))
        } else {
            name.into()
        }
    };
    match op {
        0x00 => "TEST_UNIT_READY".into(),
        0x03 => "REQUEST_SENSE".into(),
        0x12 => "INQUIRY".into(),
        0x1a => "MODE_SENSE(6)".into(),
        0x1b => "START_STOP_UNIT".into(),
        0x1e => "PREVENT_ALLOW_REMOVAL".into(),
        0x23 => "READ_FORMAT_CAPACITIES".into(),
        0x25 => "READ_CAPACITY(10)".into(),
        0x28 => rw10("READ(10)"),
        0x2a => rw10("WRITE(10)"),
        0x35 => "SYNCHRONIZE_CACHE".into(),
        0x5a => "MODE_SENSE(10)".into(),
        0x88 => "READ(16)".into(),
        0x8a => "WRITE(16)".into(),
        0x9e => "READ_CAPACITY(16)".into(),
        _ => format!("SCSI 0x{op:02x}"),
    }
}

/// Bulk-only transport wrappers.
fn storage_text(d: &[u8]) -> Option<String> {
    if d.len() == 31 && &d[0..4] == b"USBC" {
        let len = le32(d, 8);
        let dir = if d[12] & 0x80 != 0 { "in" } else { "out" };
        let n = (d[14] as usize).min(16);
        return Some(format!("CBW tag {} LUN {} {} ({len} bytes {dir})", le32(d, 4), d[13] & 0x0f, scsi(&d[15..15 + n])));
    }
    if d.len() == 13 && &d[0..4] == b"USBS" {
        let st = match d[12] {
            0 => "ok",
            1 => "failed",
            2 => "phase error",
            _ => "?",
        };
        return Some(format!("CSW tag {} {st} residue {}", le32(d, 4), le32(d, 8)));
    }
    None
}

/// FTDI IN data: each max-packet chunk starts with 2 status bytes.
fn ftdi_payload(d: &[u8], max_packet: u16) -> Vec<u8> {
    let step = if max_packet >= 8 { max_packet as usize } else { 64 };
    d.chunks(step).flat_map(|c| c.iter().skip(2).copied()).collect()
}

/// True when the endpoint carries serial data.
fn serial_ep(dev: Option<&Device>, e: &Event) -> bool {
    let Some(d) = dev else { return false };
    if e.xfer != Xfer::Bulk {
        return false;
    }
    let class = d.eps.get(&e.ep).map(|x| x.class).unwrap_or(0);
    class == 0x0a || (class == 0xff && SERIAL_VENDORS.contains(&d.vid))
}

/// Serial bytes of a transfer (IN true for device to host), for the
/// serial view. FTDI status bytes are removed.
pub fn serial_bytes(e: &Event, dev: Option<&Device>) -> Option<(bool, Vec<u8>)> {
    if !serial_ep(dev, e) || e.data.is_empty() {
        return None;
    }
    let d = dev?;
    let data = if d.vid == FTDI && e.is_in() {
        ftdi_payload(&e.data, d.eps.get(&e.ep).map(|x| x.max_packet).unwrap_or(64))
    } else {
        e.data.clone()
    };
    (!data.is_empty()).then_some((e.is_in(), data))
}

/// Keeps control requests between submission and completion.
#[derive(Default)]
pub struct Decoder {
    pending: HashMap<u64, [u8; 8]>,
}

impl Decoder {
    pub fn reset(&mut self) {
        self.pending.clear();
    }

    /// What the transfer means; empty when there is nothing to say.
    pub fn describe(&mut self, e: &Event, dev: Option<&Device>) -> String {
        if let Some(s) = e.setup {
            if e.kind == b'S' {
                if self.pending.len() > 4096 {
                    self.pending.clear();
                }
                self.pending.insert(e.id, s);
            }
            return setup_text(&s, dev, &e.data);
        }
        if e.xfer == Xfer::Control && e.kind == b'C' {
            let Some(s) = self.pending.remove(&e.id) else { return String::new() };
            if s[1] == 6 && s[0] & 0xe0 == 0x80 {
                if let Some(t) = descriptor_text(&s, &e.data) {
                    return t;
                }
            }
            if s[0] & 0x60 == 0x20 && s[1] == 0x21 {
                if let Some(t) = line_coding(&e.data) {
                    return format!("line coding {t}");
                }
            }
            return String::new();
        }
        if e.data.is_empty() {
            return String::new();
        }
        if let Some(t) = storage_text(&e.data) {
            return t;
        }
        if let Some((_, bytes)) = serial_bytes(e, dev) {
            return format!("\"{}\"", printable(&bytes, 80));
        }
        if dev.is_some_and(|d| d.vid == FTDI) && e.is_in() && e.data.len() == 2 {
            return "FTDI status".into();
        }
        if e.xfer == Xfer::Interrupt {
            let ep = dev.and_then(|d| d.eps.get(&e.ep));
            if let Some(t) = ep.filter(|x| x.class == 0x03).and_then(|x| hid_text(x.protocol, &e.data)) {
                return t;
            }
        }
        if mostly_text(&e.data) {
            return format!("\"{}\"", printable(&e.data, 80));
        }
        hex_short(&e.data, 16)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usb::tests::event;
    use crate::usb::Endpoint;

    fn serial_dev(vid: u16, class: u8) -> Device {
        let mut d = Device { bus: 3, dev: 6, vid, pid: 0x6001, name: "x".into(), ..Default::default() };
        d.eps.insert(0, Endpoint { class: 0, protocol: 0, max_packet: 64 });
        d.eps.insert(0x81, Endpoint { class, protocol: 0, max_packet: 64 });
        d.eps.insert(0x02, Endpoint { class, protocol: 0, max_packet: 64 });
        d
    }

    #[test]
    fn control_requests() {
        let mut dec = Decoder::default();
        let get = [0x80, 6, 0, 1, 0, 0, 18, 0];
        let s = event(b'S', 2, 0x80, 6, Some(get), -libc::EINPROGRESS, &[]);
        assert_eq!(dec.describe(&s, None), "GET_DESCRIPTOR DEVICE len 18");
        let desc = [18, 1, 0x00, 0x02, 0x02, 0, 0, 64, 0x86, 0x1a, 0xd2, 0x55, 0, 1, 1, 2, 3, 1];
        let c = event(b'C', 2, 0x80, 6, None, 0, &desc);
        assert_eq!(dec.describe(&c, None), "DEVICE USB 2.0 class 2 (CDC control) 1a86:55d2 max packet 64 configs 1");

        let str_get = [0x80, 6, 2, 3, 0x09, 0x04, 255, 0];
        let s = event(b'S', 2, 0x80, 6, Some(str_get), -libc::EINPROGRESS, &[]);
        dec.describe(&s, None);
        let c = event(b'C', 2, 0x80, 6, None, 0, &[8, 3, b'U', 0, b'S', 0, b'B', 0]);
        assert_eq!(dec.describe(&c, None), "STRING \"USB\"");

        let acm = serial_dev(0x1a86, 0x0a);
        let lc = [0x21, 0x20, 0, 0, 0, 0, 7, 0];
        let s = event(b'S', 2, 0x00, 6, Some(lc), -libc::EINPROGRESS, &[0x00, 0xc2, 0x01, 0x00, 0, 0, 8]);
        assert_eq!(dec.describe(&s, Some(&acm)), "CDC SET_LINE_CODING 115200 8N1");
        let cls = [0x21, 0x22, 3, 0, 0, 0, 0, 0];
        assert_eq!(setup_text(&cls, Some(&acm), &[]), "CDC SET_CONTROL_LINE_STATE DTR on RTS on");
        assert_eq!(setup_text(&[0x02, 1, 0, 0, 0x81, 0, 0, 0], None, &[]), "CLEAR_FEATURE ENDPOINT_HALT ep 0x81");
    }

    #[test]
    fn serial_data() {
        let mut dec = Decoder::default();
        let acm = serial_dev(0x1a86, 0x0a);
        let e = event(b'C', 3, 0x81, 6, None, 0, b"U-Boot 2024.01\r\n");
        assert_eq!(dec.describe(&e, Some(&acm)), "\"U-Boot 2024.01\\r\\n\"");
        assert_eq!(serial_bytes(&e, Some(&acm)), Some((true, b"U-Boot 2024.01\r\n".to_vec())));

        let ftdi = serial_dev(0x0403, 0xff);
        // Two 64 byte packets, each with 2 status bytes.
        let mut d = vec![0x01, 0x60];
        d.extend_from_slice(&[b'a'; 62]);
        d.extend_from_slice(&[0x01, 0x60, b'b', b'c']);
        let e = event(b'C', 3, 0x81, 6, None, 0, &d);
        let (inbound, bytes) = serial_bytes(&e, Some(&ftdi)).expect("ftdi");
        assert!(inbound);
        assert_eq!(bytes.len(), 64);
        assert!(bytes.ends_with(b"abc"));
        let st = event(b'C', 3, 0x81, 6, None, 0, &[0x01, 0x60]);
        assert_eq!(serial_bytes(&st, Some(&ftdi)), None);
        assert_eq!(dec.describe(&st, Some(&ftdi)), "FTDI status");
    }

    #[test]
    fn hid_and_storage() {
        let mut dec = Decoder::default();
        let mut kbd = Device { vid: 0x046d, ..Default::default() };
        kbd.eps.insert(0x81, Endpoint { class: 3, protocol: 1, max_packet: 8 });
        let e = event(b'C', 1, 0x81, 4, None, 0, &[0x02, 0, 0x0b, 0, 0, 0, 0, 0]);
        assert_eq!(dec.describe(&e, Some(&kbd)), "keyboard: LShift+h");
        let mut mouse = Device::default();
        mouse.eps.insert(0x81, Endpoint { class: 3, protocol: 2, max_packet: 4 });
        let e = event(b'C', 1, 0x81, 5, None, 0, &[1, 5, 0xfe, 0]);
        assert_eq!(dec.describe(&e, Some(&mouse)), "mouse: dx 5 dy -2 [left]");

        let mut cbw = b"USBC".to_vec();
        cbw.extend_from_slice(&7u32.to_le_bytes());
        cbw.extend_from_slice(&4096u32.to_le_bytes());
        cbw.extend_from_slice(&[0x80, 0, 10, 0x28, 0, 0, 0, 0, 0x10, 0, 0, 8, 0, 0, 0, 0, 0, 0, 0]);
        let e = event(b'S', 3, 0x02, 7, None, -libc::EINPROGRESS, &cbw);
        assert_eq!(dec.describe(&e, None), "CBW tag 7 LUN 0 READ(10) lba 16 blocks 8 (4096 bytes in)");
        let mut csw = b"USBS".to_vec();
        csw.extend_from_slice(&7u32.to_le_bytes());
        csw.extend_from_slice(&0u32.to_le_bytes());
        csw.push(0);
        let e = event(b'C', 3, 0x81, 7, None, 0, &csw);
        assert_eq!(dec.describe(&e, None), "CSW tag 7 ok residue 0");
    }
}
