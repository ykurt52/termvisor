// Modbus protocol: requests and responses (function codes 01–06, 15, 16),
// RTU framing (CRC-16) and TCP framing (MBAP header), frame assembly from
// a byte stream, a slave's four tables, and value formats. No GTK here.

use serde::{Deserialize, Serialize};

/// The four Modbus data tables.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Table {
    Coils,
    Discrete,
    #[default]
    Holding,
    Input,
}

impl Table {
    pub const ALL: [Table; 4] = [Table::Coils, Table::Discrete, Table::Holding, Table::Input];

    pub fn label(self) -> &'static str {
        match self {
            Table::Coils => "Coils",
            Table::Discrete => "Discrete Inputs",
            Table::Holding => "Holding Registers",
            Table::Input => "Input Registers",
        }
    }

    /// Function code that reads this table.
    pub fn read_fc(self) -> u8 {
        match self {
            Table::Coils => 1,
            Table::Discrete => 2,
            Table::Holding => 3,
            Table::Input => 4,
        }
    }

    pub fn bits(self) -> bool {
        matches!(self, Table::Coils | Table::Discrete)
    }

    /// A master can write it (coils and holding registers).
    pub fn writable(self) -> bool {
        matches!(self, Table::Coils | Table::Holding)
    }

    /// Most items one read may ask for.
    pub fn max_read(self) -> u16 {
        if self.bits() { 2000 } else { 125 }
    }
}

/// One request a master sends.
#[derive(Clone, Debug, PartialEq)]
pub enum Request {
    Read { table: Table, addr: u16, count: u16 },
    WriteCoil { addr: u16, on: bool },
    WriteRegister { addr: u16, value: u16 },
    WriteCoils { addr: u16, values: Vec<bool> },
    WriteRegisters { addr: u16, values: Vec<u16> },
}

/// A slave's answer.
#[derive(Clone, Debug, PartialEq)]
pub enum Response {
    Bits(Vec<bool>),
    Registers(Vec<u16>),
    /// Echo of a write (address and value or count).
    Written { addr: u16, value: u16 },
    Exception { fc: u8, code: u8 },
}

pub fn exception_text(code: u8) -> &'static str {
    match code {
        1 => "illegal function",
        2 => "illegal data address",
        3 => "illegal data value",
        4 => "slave device failure",
        5 => "acknowledge",
        6 => "slave device busy",
        8 => "memory parity error",
        10 => "gateway path unavailable",
        11 => "gateway target failed to respond",
        _ => "unknown exception",
    }
}

fn pack_bits(bits: &[bool]) -> Vec<u8> {
    let mut out = vec![0u8; bits.len().div_ceil(8)];
    for (i, &b) in bits.iter().enumerate() {
        if b {
            out[i / 8] |= 1 << (i % 8);
        }
    }
    out
}

fn unpack_bits(bytes: &[u8], count: usize) -> Vec<bool> {
    (0..count).map(|i| bytes.get(i / 8).is_some_and(|b| b & (1 << (i % 8)) != 0)).collect()
}

fn be(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes([b[at], b[at + 1]])
}

impl Request {
    pub fn fc(&self) -> u8 {
        match self {
            Request::Read { table, .. } => table.read_fc(),
            Request::WriteCoil { .. } => 5,
            Request::WriteRegister { .. } => 6,
            Request::WriteCoils { .. } => 15,
            Request::WriteRegisters { .. } => 16,
        }
    }

    /// Function code and data, without unit id or framing.
    pub fn pdu(&self) -> Vec<u8> {
        let mut p = vec![self.fc()];
        match self {
            Request::Read { addr, count, .. } => {
                p.extend(addr.to_be_bytes());
                p.extend(count.to_be_bytes());
            }
            Request::WriteCoil { addr, on } => {
                p.extend(addr.to_be_bytes());
                p.extend(if *on { [0xFF, 0x00] } else { [0x00, 0x00] });
            }
            Request::WriteRegister { addr, value } => {
                p.extend(addr.to_be_bytes());
                p.extend(value.to_be_bytes());
            }
            Request::WriteCoils { addr, values } => {
                let bytes = pack_bits(values);
                p.extend(addr.to_be_bytes());
                p.extend((values.len() as u16).to_be_bytes());
                p.push(bytes.len() as u8);
                p.extend(bytes);
            }
            Request::WriteRegisters { addr, values } => {
                p.extend(addr.to_be_bytes());
                p.extend((values.len() as u16).to_be_bytes());
                p.push((values.len() * 2) as u8);
                for v in values {
                    p.extend(v.to_be_bytes());
                }
            }
        }
        p
    }

    /// Parses a request PDU (slave side).
    pub fn parse(pdu: &[u8]) -> Result<Request, u8> {
        let fc = *pdu.first().ok_or(1u8)?;
        let need = |n: usize| if pdu.len() >= n { Ok(()) } else { Err(3u8) };
        match fc {
            1..=4 => {
                need(5)?;
                let table = Table::ALL[(fc - 1) as usize];
                let count = be(pdu, 3);
                if count == 0 || count > table.max_read() {
                    return Err(3);
                }
                Ok(Request::Read { table, addr: be(pdu, 1), count })
            }
            5 => {
                need(5)?;
                match be(pdu, 3) {
                    0xFF00 => Ok(Request::WriteCoil { addr: be(pdu, 1), on: true }),
                    0x0000 => Ok(Request::WriteCoil { addr: be(pdu, 1), on: false }),
                    _ => Err(3),
                }
            }
            6 => {
                need(5)?;
                Ok(Request::WriteRegister { addr: be(pdu, 1), value: be(pdu, 3) })
            }
            15 => {
                need(6)?;
                let count = be(pdu, 3) as usize;
                let bytes = pdu[5] as usize;
                if count == 0 || count > 1968 || bytes != count.div_ceil(8) {
                    return Err(3);
                }
                need(6 + bytes)?;
                Ok(Request::WriteCoils { addr: be(pdu, 1), values: unpack_bits(&pdu[6..6 + bytes], count) })
            }
            16 => {
                need(6)?;
                let count = be(pdu, 3) as usize;
                let bytes = pdu[5] as usize;
                if count == 0 || count > 123 || bytes != count * 2 {
                    return Err(3);
                }
                need(6 + bytes)?;
                Ok(Request::WriteRegisters { addr: be(pdu, 1), values: (0..count).map(|i| be(pdu, 6 + i * 2)).collect() })
            }
            _ => Err(1),
        }
    }

    /// Parses the response to this request.
    pub fn parse_response(&self, pdu: &[u8]) -> Result<Response, String> {
        let fc = *pdu.first().ok_or("empty response")?;
        if fc == self.fc() | 0x80 {
            let code = *pdu.get(1).ok_or("short exception")?;
            return Ok(Response::Exception { fc: self.fc(), code });
        }
        if fc != self.fc() {
            return Err(format!("function {fc:02X} in reply to {:02X}", self.fc()));
        }
        match self {
            Request::Read { table, count, .. } => {
                let n = *pdu.get(1).ok_or("short response")? as usize;
                let data = pdu.get(2..2 + n).ok_or("short response")?;
                if table.bits() {
                    if n != (*count as usize).div_ceil(8) {
                        return Err(format!("{n} data bytes for {count} bits"));
                    }
                    Ok(Response::Bits(unpack_bits(data, *count as usize)))
                } else {
                    if n != *count as usize * 2 {
                        return Err(format!("{n} data bytes for {count} registers"));
                    }
                    Ok(Response::Registers((0..*count as usize).map(|i| be(data, i * 2)).collect()))
                }
            }
            _ => {
                if pdu.len() < 5 {
                    return Err("short response".into());
                }
                Ok(Response::Written { addr: be(pdu, 1), value: be(pdu, 3) })
            }
        }
    }
}

impl Response {
    /// Function code and data (slave side).
    pub fn pdu(&self, fc: u8) -> Vec<u8> {
        match self {
            Response::Bits(b) => {
                let bytes = pack_bits(b);
                let mut p = vec![fc, bytes.len() as u8];
                p.extend(bytes);
                p
            }
            Response::Registers(r) => {
                let mut p = vec![fc, (r.len() * 2) as u8];
                for v in r {
                    p.extend(v.to_be_bytes());
                }
                p
            }
            Response::Written { addr, value } => {
                let mut p = vec![fc];
                p.extend(addr.to_be_bytes());
                p.extend(value.to_be_bytes());
                p
            }
            Response::Exception { fc, code } => vec![fc | 0x80, *code],
        }
    }
}

/// Modbus RTU CRC-16 (poly 0xA001, init 0xFFFF), sent low byte first.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc = 0xFFFFu16;
    for &b in data {
        crc ^= u16::from(b);
        for _ in 0..8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xA001 } else { crc >> 1 };
        }
    }
    crc
}

/// Unit id, PDU and CRC.
pub fn rtu_frame(unit: u8, pdu: &[u8]) -> Vec<u8> {
    let mut f = vec![unit];
    f.extend(pdu);
    let crc = crc16(&f);
    f.extend(crc.to_le_bytes());
    f
}

/// MBAP header (transaction, protocol 0, length, unit) and PDU.
pub fn tcp_frame(transaction: u16, unit: u8, pdu: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(7 + pdu.len());
    f.extend(transaction.to_be_bytes());
    f.extend([0, 0]);
    f.extend(((pdu.len() + 1) as u16).to_be_bytes());
    f.push(unit);
    f.extend(pdu);
    f
}

/// Length of a complete RTU frame at the start of `buf` (unit .. CRC), if
/// its size can be told yet. `request`: the bytes are a request (slave
/// side), else a response (master side).
fn rtu_len(buf: &[u8], request: bool) -> Option<usize> {
    let fc = *buf.get(1)?;
    let pdu = if fc & 0x80 != 0 {
        2
    } else {
        match (fc, request) {
            (1..=6, true) => 5,
            (15 | 16, true) => 6 + *buf.get(6)? as usize,
            (1..=4, false) => 2 + *buf.get(2)? as usize,
            (5 | 6 | 15 | 16, false) => 5,
            // Unknown function: wait for the gap to end it.
            _ => return None,
        }
    };
    Some(1 + pdu + 2)
}

/// A frame read from a stream.
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub unit: u8,
    /// TCP transaction id (0 for RTU).
    pub transaction: u16,
    pub pdu: Vec<u8>,
    /// The bytes as they came, for the traffic log.
    pub raw: Vec<u8>,
}

/// Collects RTU or TCP frames from received chunks.
#[derive(Default)]
pub struct Reader {
    buf: Vec<u8>,
    tcp: bool,
    request: bool,
}

/// What a fed chunk produced.
#[derive(Debug, PartialEq)]
pub enum Got {
    Frame(Frame),
    /// Bytes dropped: bad CRC, bad header, or garbage between frames.
    Junk(Vec<u8>),
}

impl Reader {
    pub fn new(tcp: bool, request: bool) -> Self {
        Self { buf: Vec::new(), tcp, request }
    }

    /// Drops a partial frame (silence on the line ended it).
    pub fn flush(&mut self) -> Option<Got> {
        (!self.buf.is_empty()).then(|| Got::Junk(std::mem::take(&mut self.buf)))
    }

    pub fn feed(&mut self, data: &[u8]) -> Vec<Got> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        loop {
            let step = if self.tcp { self.next_tcp() } else { self.next_rtu() };
            match step {
                Some(g) => out.push(g),
                None => break,
            }
        }
        out
    }

    fn next_tcp(&mut self) -> Option<Got> {
        if self.buf.len() < 7 {
            return None;
        }
        let len = be(&self.buf, 4) as usize;
        if be(&self.buf, 2) != 0 || !(2..=254).contains(&len) {
            return Some(Got::Junk(std::mem::take(&mut self.buf)));
        }
        if self.buf.len() < 6 + len {
            return None;
        }
        let raw: Vec<u8> = self.buf.drain(..6 + len).collect();
        Some(Got::Frame(Frame { unit: raw[6], transaction: be(&raw, 0), pdu: raw[7..].to_vec(), raw }))
    }

    fn next_rtu(&mut self) -> Option<Got> {
        let len = rtu_len(&self.buf, self.request)?;
        if len > 256 {
            return Some(Got::Junk(std::mem::take(&mut self.buf)));
        }
        if self.buf.len() < len {
            return None;
        }
        let (body, crc) = self.buf[..len].split_at(len - 2);
        if crc16(body).to_le_bytes() != crc {
            // Lost sync: drop one byte and look again.
            return Some(Got::Junk(self.buf.drain(..1).collect()));
        }
        let raw: Vec<u8> = self.buf.drain(..len).collect();
        Some(Got::Frame(Frame { unit: raw[0], transaction: 0, pdu: raw[1..len - 2].to_vec(), raw }))
    }
}

/// A slave's four tables, every address 0–65535.
pub struct Bank {
    pub coils: Vec<bool>,
    pub discrete: Vec<bool>,
    pub holding: Vec<u16>,
    pub input: Vec<u16>,
}

impl Default for Bank {
    fn default() -> Self {
        Self { coils: vec![false; 65536], discrete: vec![false; 65536], holding: vec![0; 65536], input: vec![0; 65536] }
    }
}

impl Bank {
    /// Range `addr..addr+count`, or exception 02 when it runs past the end.
    fn span(addr: u16, count: usize) -> Result<std::ops::Range<usize>, u8> {
        let end = addr as usize + count;
        if end > 65536 { Err(2) } else { Ok(addr as usize..end) }
    }

    /// Answers a request PDU; a write changes the tables.
    pub fn handle(&mut self, pdu: &[u8]) -> Vec<u8> {
        let fc = pdu.first().copied().unwrap_or(0);
        let resp = match Request::parse(pdu) {
            Err(code) => Response::Exception { fc, code },
            Ok(r) => self.apply(&r).unwrap_or_else(|code| Response::Exception { fc, code }),
        };
        resp.pdu(fc)
    }

    fn apply(&mut self, r: &Request) -> Result<Response, u8> {
        Ok(match r {
            Request::Read { table, addr, count } => {
                let s = Self::span(*addr, *count as usize)?;
                match table {
                    Table::Coils => Response::Bits(self.coils[s].to_vec()),
                    Table::Discrete => Response::Bits(self.discrete[s].to_vec()),
                    Table::Holding => Response::Registers(self.holding[s].to_vec()),
                    Table::Input => Response::Registers(self.input[s].to_vec()),
                }
            }
            Request::WriteCoil { addr, on } => {
                self.coils[*addr as usize] = *on;
                Response::Written { addr: *addr, value: if *on { 0xFF00 } else { 0 } }
            }
            Request::WriteRegister { addr, value } => {
                self.holding[*addr as usize] = *value;
                Response::Written { addr: *addr, value: *value }
            }
            Request::WriteCoils { addr, values } => {
                let s = Self::span(*addr, values.len())?;
                self.coils[s].copy_from_slice(values);
                Response::Written { addr: *addr, value: values.len() as u16 }
            }
            Request::WriteRegisters { addr, values } => {
                let s = Self::span(*addr, values.len())?;
                self.holding[s].copy_from_slice(values);
                Response::Written { addr: *addr, value: values.len() as u16 }
            }
        })
    }
}

/// How register values are shown and typed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Format {
    #[default]
    Unsigned,
    Signed,
    Hex,
    Binary,
    /// 32-bit integer, first register high word (AB CD).
    LongBig,
    /// 32-bit integer, first register low word (CD AB).
    LongLittle,
    /// IEEE float, first register high word (AB CD).
    FloatBig,
    /// IEEE float, first register low word (CD AB).
    FloatLittle,
}

impl Format {
    pub const ALL: [Format; 8] = [
        Format::Unsigned,
        Format::Signed,
        Format::Hex,
        Format::Binary,
        Format::LongBig,
        Format::LongLittle,
        Format::FloatBig,
        Format::FloatLittle,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Format::Unsigned => "Unsigned",
            Format::Signed => "Signed",
            Format::Hex => "Hex",
            Format::Binary => "Binary",
            Format::LongBig => "Long AB CD",
            Format::LongLittle => "Long CD AB",
            Format::FloatBig => "Float AB CD",
            Format::FloatLittle => "Float CD AB",
        }
    }

    /// Registers per value.
    pub fn words(self) -> usize {
        match self {
            Format::LongBig | Format::LongLittle | Format::FloatBig | Format::FloatLittle => 2,
            _ => 1,
        }
    }

    fn pair(self, regs: &[u16]) -> u32 {
        let (a, b) = (u32::from(regs[0]), u32::from(regs[1]));
        match self {
            Format::LongLittle | Format::FloatLittle => (b << 16) | a,
            _ => (a << 16) | b,
        }
    }

    /// Text of the value that starts at `regs[0]`.
    pub fn show(self, regs: &[u16]) -> String {
        if regs.len() < self.words() {
            return "–".into();
        }
        let v = regs[0];
        match self {
            Format::Unsigned => v.to_string(),
            Format::Signed => (v as i16).to_string(),
            Format::Hex => format!("0x{v:04X}"),
            Format::Binary => format!("{:04b} {:04b} {:04b} {:04b}", v >> 12, (v >> 8) & 0xF, (v >> 4) & 0xF, v & 0xF),
            Format::LongBig | Format::LongLittle => (self.pair(regs) as i32).to_string(),
            Format::FloatBig | Format::FloatLittle => {
                let f = f32::from_bits(self.pair(regs));
                let s = format!("{f}");
                if s.len() > 14 { format!("{f:.6e}") } else { s }
            }
        }
    }

    /// Registers for a typed value.
    pub fn parse(self, text: &str) -> Result<Vec<u16>, String> {
        let t = text.trim().replace(['_', ' '], "");
        let bad = || format!("not a {} value: {}", self.label(), text.trim());
        let int = |t: &str| -> Option<i64> {
            if let Some(h) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                i64::from_str_radix(h, 16).ok()
            } else if let Some(b) = t.strip_prefix("0b") {
                i64::from_str_radix(b, 2).ok()
            } else {
                t.parse().ok()
            }
        };
        let split = |v: u32| -> Vec<u16> {
            let (hi, lo) = ((v >> 16) as u16, v as u16);
            match self {
                Format::LongLittle | Format::FloatLittle => vec![lo, hi],
                _ => vec![hi, lo],
            }
        };
        match self {
            Format::Unsigned | Format::Hex | Format::Signed => {
                let v = if self == Format::Hex && !t.starts_with("0x") && !t.starts_with("0X") {
                    i64::from_str_radix(&t, 16).ok()
                } else {
                    int(&t)
                }
                .ok_or_else(bad)?;
                if !(-32768..=65535).contains(&v) {
                    return Err(format!("{v} does not fit in 16 bits"));
                }
                Ok(vec![v as u16])
            }
            Format::Binary => {
                let v = u16::from_str_radix(t.trim_start_matches("0b"), 2).map_err(|_| bad())?;
                Ok(vec![v])
            }
            Format::LongBig | Format::LongLittle => {
                let v = int(&t).filter(|v| (-(1i64 << 31)..(1i64 << 32)).contains(v)).ok_or_else(bad)?;
                Ok(split(v as u32))
            }
            Format::FloatBig | Format::FloatLittle => {
                let f: f32 = t.parse().map_err(|_| bad())?;
                Ok(split(f.to_bits()))
            }
        }
    }

    /// Number for the plot.
    pub fn number(self, regs: &[u16]) -> Option<f64> {
        if regs.len() < self.words() {
            return None;
        }
        Some(match self {
            Format::Signed => f64::from(regs[0] as i16),
            Format::LongBig | Format::LongLittle => f64::from(self.pair(regs) as i32),
            Format::FloatBig | Format::FloatLittle => f64::from(f32::from_bits(self.pair(regs))),
            _ => f64::from(regs[0]),
        })
    }
}

/// "01 03 00 00 00 0A C5 CD"
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn crc_of_known_frames() {
        // Read 10 holding registers from unit 1 at 0: a common example frame.
        assert_eq!(rtu_frame(1, &Request::Read { table: Table::Holding, addr: 0, count: 10 }.pdu()), [0x01, 0x03, 0x00, 0x00, 0x00, 0x0A, 0xC5, 0xCD]);
        // Write coil 0x00AC on in unit 0x11 (Modbus spec example).
        assert_eq!(rtu_frame(0x11, &Request::WriteCoil { addr: 0xAC, on: true }.pdu()), [0x11, 0x05, 0x00, 0xAC, 0xFF, 0x00, 0x4E, 0x8B]);
    }

    #[test]
    fn requests_roundtrip() {
        let reqs = [
            Request::Read { table: Table::Coils, addr: 19, count: 37 },
            Request::Read { table: Table::Input, addr: 8, count: 1 },
            Request::WriteCoil { addr: 3, on: false },
            Request::WriteRegister { addr: 1, value: 0xBEEF },
            Request::WriteCoils { addr: 19, values: vec![true, false, true, true, false, false, true, true, true, false] },
            Request::WriteRegisters { addr: 1, values: vec![0x000A, 0x0102] },
        ];
        for r in reqs {
            assert_eq!(Request::parse(&r.pdu()), Ok(r.clone()), "{r:?}");
        }
        assert_eq!(Request::parse(&[0x2B, 0x0E]), Err(1));
        assert_eq!(Request::parse(&[0x03, 0, 0, 0, 0]), Err(3));
        assert_eq!(Request::parse(&[0x03, 0, 0, 0, 126]), Err(3));
    }

    #[test]
    fn slave_answers_and_master_reads() {
        let mut bank = Bank::default();
        bank.holding[100] = 1234;
        bank.coils[5] = true;
        let read = Request::Read { table: Table::Holding, addr: 99, count: 3 };
        assert_eq!(read.parse_response(&bank.handle(&read.pdu())), Ok(Response::Registers(vec![0, 1234, 0])));
        let write = Request::WriteRegisters { addr: 10, values: vec![7, 8] };
        assert_eq!(write.parse_response(&bank.handle(&write.pdu())), Ok(Response::Written { addr: 10, value: 2 }));
        assert_eq!(bank.holding[10..12], [7, 8]);
        let coils = Request::Read { table: Table::Coils, addr: 0, count: 9 };
        match coils.parse_response(&bank.handle(&coils.pdu())) {
            Ok(Response::Bits(b)) => assert_eq!(b, [false, false, false, false, false, true, false, false, false]),
            other => panic!("{other:?}"),
        }
        // Past the end of the table: exception 02.
        let far = Request::Read { table: Table::Input, addr: 65535, count: 2 };
        assert_eq!(far.parse_response(&bank.handle(&far.pdu())), Ok(Response::Exception { fc: 4, code: 2 }));
        // Unknown function: exception 01.
        assert_eq!(bank.handle(&[0x41, 0]), vec![0xC1, 1]);
    }

    #[test]
    fn rtu_frames_from_a_stream() {
        let req = rtu_frame(1, &Request::Read { table: Table::Holding, addr: 0, count: 2 }.pdu());
        let resp = rtu_frame(1, &Response::Registers(vec![10, 20]).pdu(3));
        // Master side: the response arrives in pieces, after garbage.
        let mut r = Reader::new(false, false);
        assert!(r.feed(&resp[..3]).is_empty());
        let got = r.feed(&resp[3..]);
        assert_eq!(got, vec![Got::Frame(Frame { unit: 1, transaction: 0, pdu: resp[1..resp.len() - 2].to_vec(), raw: resp.clone() })]);
        let mut noisy = vec![0x55];
        noisy.extend(&resp);
        let got = r.feed(&noisy);
        assert!(matches!(got.last(), Some(Got::Frame(_))));
        // Slave side: two requests in one chunk.
        let mut s = Reader::new(false, true);
        let mut two = req.clone();
        two.extend(rtu_frame(2, &Request::WriteCoils { addr: 0, values: vec![true; 10] }.pdu()));
        let frames: Vec<_> = s.feed(&two).into_iter().filter_map(|g| if let Got::Frame(f) = g { Some(f.unit) } else { None }).collect();
        assert_eq!(frames, [1, 2]);
        // An exception response is five bytes.
        let mut e = Reader::new(false, false);
        assert!(matches!(e.feed(&rtu_frame(1, &[0x83, 0x02]))[..], [Got::Frame(_)]));
    }

    #[test]
    fn tcp_frames_from_a_stream() {
        let f = tcp_frame(0x1234, 7, &Request::Read { table: Table::Input, addr: 1, count: 1 }.pdu());
        assert_eq!(f[..7], [0x12, 0x34, 0, 0, 0, 6, 7]);
        let mut r = Reader::new(true, true);
        assert!(r.feed(&f[..4]).is_empty());
        match &r.feed(&f[4..])[..] {
            [Got::Frame(fr)] => assert_eq!((fr.transaction, fr.unit, fr.pdu.as_slice()), (0x1234, 7, &f[7..])),
            other => panic!("{other:?}"),
        }
        assert!(matches!(r.feed(&[1, 2, 9, 9, 0, 6, 1])[..], [Got::Junk(_)]));
    }

    #[test]
    fn formats() {
        assert_eq!(Format::Signed.show(&[0xFFFE]), "-2");
        assert_eq!(Format::Hex.show(&[0x00AB]), "0x00AB");
        assert_eq!(Format::Binary.show(&[0xA5F0]), "1010 0101 1111 0000");
        assert_eq!(Format::LongBig.show(&[0x0001, 0x0000]), "65536");
        assert_eq!(Format::LongLittle.show(&[0x0000, 0x0001]), "65536");
        assert_eq!(Format::FloatBig.show(&[0x4049, 0x0FDB]), "3.1415927");
        assert_eq!(Format::FloatLittle.parse("1.5"), Ok(vec![0x0000, 0x3FC0]));
        assert_eq!(Format::FloatBig.parse("1.5"), Ok(vec![0x3FC0, 0x0000]));
        assert_eq!(Format::Signed.parse("-1"), Ok(vec![0xFFFF]));
        assert_eq!(Format::Hex.parse("ff"), Ok(vec![0x00FF]));
        assert_eq!(Format::Unsigned.parse("0x10"), Ok(vec![16]));
        assert!(Format::Unsigned.parse("70000").is_err());
        assert_eq!(Format::LongBig.parse("-2"), Ok(vec![0xFFFF, 0xFFFE]));
        assert_eq!(Format::Binary.parse("1010 0101"), Ok(vec![0xA5]));
        assert_eq!(Format::FloatBig.number(&[0x3FC0, 0]), Some(1.5));
        assert_eq!(Format::Signed.number(&[0x8000]), Some(-32768.0));
    }
}
