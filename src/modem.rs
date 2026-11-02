// Serial file transfer: XMODEM (checksum, CRC, 1K), YMODEM (batch) and
// ZMODEM (zmodem2 state machine, with resume).
//
// Engines are blocking and run on a worker thread. They talk to the other
// side through `Wire`, which the stream pane backs with the open link.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use zmodem2::{Action, Event as ZEvent, FileInfo, Position, Receiver, Sender};

use crate::transfers::free_name;

const SOH: u8 = 0x01;
const STX: u8 = 0x02;
const EOT: u8 = 0x04;
const ACK: u8 = 0x06;
const NAK: u8 = 0x15;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1A;
const CRC_C: u8 = b'C';

const RETRIES: usize = 10;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Proto {
    Xmodem,
    Xmodem1k,
    Ymodem,
    Zmodem,
}

impl Proto {
    pub fn label(self) -> &'static str {
        match self {
            Proto::Xmodem => "XMODEM",
            Proto::Xmodem1k => "XMODEM-1K",
            Proto::Ymodem => "YMODEM",
            Proto::Zmodem => "ZMODEM",
        }
    }
}

/// Byte pipe to the other side.
pub trait Wire {
    /// Bytes that arrived within `timeout`; empty on timeout.
    fn recv(&mut self, timeout: Duration) -> io::Result<Vec<u8>>;
    fn send(&mut self, data: &[u8]) -> io::Result<()>;
}

/// Wire over a channel of incoming chunks and a send function.
pub struct ChanWire {
    pub rx: mpsc::Receiver<Vec<u8>>,
    pub tx: crate::link::Sink,
}

impl Wire for ChanWire {
    fn recv(&mut self, timeout: Duration) -> io::Result<Vec<u8>> {
        match self.rx.recv_timeout(timeout) {
            Ok(d) => Ok(d),
            Err(mpsc::RecvTimeoutError::Timeout) => Ok(Vec::new()),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(io::Error::new(io::ErrorKind::BrokenPipe, "link closed")),
        }
    }

    fn send(&mut self, data: &[u8]) -> io::Result<()> {
        (self.tx)(data)
    }
}

/// Cancel flag and progress callback (file name, bytes done, total).
pub struct Ctl<'a> {
    pub cancel: &'a AtomicBool,
    pub progress: &'a mut dyn FnMut(&str, u64, u64),
}

impl Ctl<'_> {
    fn stopped(&self) -> bool {
        self.cancel.load(Ordering::Relaxed)
    }
}

/// A file to send.
pub struct Outgoing {
    pub path: PathBuf,
}

impl Outgoing {
    fn open(&self) -> Result<(String, u64, u32, File), String> {
        let f = File::open(&self.path).map_err(|e| format!("{}: {e}", self.path.display()))?;
        let meta = f.metadata().map_err(|e| e.to_string())?;
        let name = self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .and_then(|d| u32::try_from(d.as_secs()).ok())
            .unwrap_or(0);
        Ok((name, meta.len(), mtime, f))
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// CRC-16/XMODEM (poly 0x1021, init 0).
fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in data {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

fn checksum(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |a, &b| a.wrapping_add(b))
}

/// Buffered byte input over a Wire.
struct In<'a> {
    w: &'a mut dyn Wire,
    buf: VecDeque<u8>,
}

impl<'a> In<'a> {
    fn new(w: &'a mut dyn Wire) -> Self {
        Self { w, buf: VecDeque::new() }
    }

    fn byte(&mut self, timeout: Duration) -> Result<Option<u8>, String> {
        let end = Instant::now() + timeout;
        loop {
            if let Some(b) = self.buf.pop_front() {
                return Ok(Some(b));
            }
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Ok(None);
            }
            let d = self.w.recv(left).map_err(err)?;
            self.buf.extend(d);
        }
    }

    /// Exactly `n` bytes, or None when they do not all arrive in time.
    fn exact(&mut self, n: usize, timeout: Duration) -> Result<Option<Vec<u8>>, String> {
        let mut out = Vec::with_capacity(n);
        while out.len() < n {
            match self.byte(timeout)? {
                Some(b) => out.push(b),
                None => return Ok(None),
            }
        }
        Ok(Some(out))
    }

    /// Drops buffered input and whatever arrives until the line is quiet.
    fn purge(&mut self) -> Result<(), String> {
        self.buf.clear();
        while !self.w.recv(Duration::from_millis(200)).map_err(err)?.is_empty() {}
        Ok(())
    }

    fn send(&mut self, data: &[u8]) -> Result<(), String> {
        self.w.send(data).map_err(err)
    }

    fn cancel(&mut self) {
        let _ = self.w.send(&[CAN; 5]);
    }
}

// ---------------------------------------------------------------- sending

/// Waits for the receiver's start: 'C' asks for CRC, NAK for checksum.
fn wait_start(inp: &mut In, ctl: &Ctl, need_crc: bool) -> Result<bool, String> {
    let end = Instant::now() + Duration::from_secs(60);
    let mut cans = 0;
    while Instant::now() < end {
        if ctl.stopped() {
            inp.cancel();
            return Err("cancelled".into());
        }
        match inp.byte(Duration::from_millis(500))? {
            Some(CRC_C) => return Ok(true),
            Some(NAK) if !need_crc => return Ok(false),
            Some(CAN) => {
                cans += 1;
                if cans >= 2 {
                    return Err("cancelled by receiver".into());
                }
            }
            _ => {}
        }
    }
    Err("receiver did not start (no 'C' or NAK in 60 s)".into())
}

fn send_block(inp: &mut In, ctl: &Ctl, num: u8, data: &[u8], crc: bool) -> Result<(), String> {
    let mut pkt = Vec::with_capacity(data.len() + 5);
    pkt.push(if data.len() == 1024 { STX } else { SOH });
    pkt.push(num);
    pkt.push(!num);
    pkt.extend_from_slice(data);
    if crc {
        pkt.extend_from_slice(&crc16(data).to_be_bytes());
    } else {
        pkt.push(checksum(data));
    }
    let mut cans = 0;
    for _ in 0..RETRIES {
        if ctl.stopped() {
            inp.cancel();
            return Err("cancelled".into());
        }
        inp.send(&pkt)?;
        loop {
            match inp.byte(Duration::from_secs(10))? {
                Some(ACK) => return Ok(()),
                Some(CAN) => {
                    cans += 1;
                    if cans >= 2 {
                        return Err("cancelled by receiver".into());
                    }
                }
                // NAK, timeout, or a late 'C': send the block again.
                Some(NAK) | Some(CRC_C) | None => break,
                Some(_) => {}
            }
        }
    }
    inp.cancel();
    Err(format!("block {num}: no ACK after {RETRIES} tries"))
}

fn send_eot(inp: &mut In) -> Result<(), String> {
    for _ in 0..RETRIES {
        inp.send(&[EOT])?;
        if inp.byte(Duration::from_secs(10))? == Some(ACK) {
            return Ok(());
        }
    }
    Err("no ACK for end of transfer".into())
}

fn read_full(r: &mut dyn Read, buf: &mut [u8]) -> Result<usize, String> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e.to_string()),
        }
    }
    Ok(n)
}

/// How data blocks are framed.
#[derive(Clone, Copy)]
struct Framing {
    /// 1024-byte blocks (needs CRC).
    k1: bool,
    crc: bool,
    /// Number of the first data block.
    first: u8,
}

/// Data blocks numbered from `fr.first`. 1K blocks when allowed; a final
/// remainder of 128 bytes or less goes in a short block.
fn send_data(inp: &mut In, ctl: &mut Ctl, name: &str, f: &mut File, size: u64, fr: Framing) -> Result<(), String> {
    let (k1, crc) = (fr.k1, fr.crc);
    let mut num = fr.first;
    let mut done = 0u64;
    let mut buf = vec![0u8; 1024];
    loop {
        let left = size.saturating_sub(done);
        let bsize = if k1 && left > 128 { 1024 } else { 128 };
        let n = read_full(f, &mut buf[..bsize])?;
        if n == 0 {
            break;
        }
        buf[n..bsize].fill(SUB);
        send_block(inp, ctl, num, &buf[..bsize], crc)?;
        num = num.wrapping_add(1);
        done += n as u64;
        (ctl.progress)(name, done, size);
    }
    Ok(())
}

pub fn xmodem_send(w: &mut dyn Wire, file: &Outgoing, k1: bool, ctl: &mut Ctl) -> Result<u64, String> {
    let (name, size, _, mut f) = file.open()?;
    let mut inp = In::new(w);
    let crc = wait_start(&mut inp, ctl, false)?;
    inp.purge()?;
    // 1K blocks need CRC.
    send_data(&mut inp, ctl, &name, &mut f, size, Framing { k1: k1 && crc, crc, first: 1 })?;
    send_eot(&mut inp)?;
    Ok(size)
}

/// YMODEM header block: "name\0size mtime mode\0", zero padded.
fn ymodem_header(name: &str, size: u64, mtime: u32) -> Vec<u8> {
    let mut b = name.as_bytes().to_vec();
    b.push(0);
    b.extend_from_slice(format!("{size} {mtime:o} 100644").as_bytes());
    b.push(0);
    let len = if b.len() <= 128 { 128 } else { 1024 };
    b.resize(len, 0);
    b.truncate(len);
    b
}

pub fn ymodem_send(w: &mut dyn Wire, files: &[Outgoing], ctl: &mut Ctl) -> Result<u64, String> {
    let mut inp = In::new(w);
    let mut total = 0;
    for file in files {
        let (name, size, mtime, mut f) = file.open()?;
        wait_start(&mut inp, ctl, true)?;
        inp.purge()?;
        send_block(&mut inp, ctl, 0, &ymodem_header(&name, size, mtime), true)?;
        wait_start(&mut inp, ctl, true)?;
        send_data(&mut inp, ctl, &name, &mut f, size, Framing { k1: true, crc: true, first: 1 })?;
        send_eot(&mut inp)?;
        total += size;
    }
    // Empty header ends the batch.
    wait_start(&mut inp, ctl, true)?;
    send_block(&mut inp, ctl, 0, &[0u8; 128], true)?;
    Ok(total)
}

// -------------------------------------------------------------- receiving

enum Got {
    Block(u8, Vec<u8>),
    Eot,
}

/// Reads one block (or EOT). Bad blocks are NAKed and read again.
fn read_block(inp: &mut In, ctl: &Ctl, crc: bool, poke: u8, first: bool) -> Result<Got, String> {
    let mut tries = 0;
    let mut cans = 0;
    loop {
        if ctl.stopped() {
            inp.cancel();
            return Err("cancelled".into());
        }
        let wait = if first { Duration::from_secs(3) } else { Duration::from_secs(10) };
        let size = match inp.byte(wait)? {
            Some(SOH) => 128,
            Some(STX) => 1024,
            Some(EOT) => return Ok(Got::Eot),
            Some(CAN) => {
                cans += 1;
                if cans >= 2 {
                    return Err("cancelled by sender".into());
                }
                continue;
            }
            Some(_) => continue,
            None => {
                tries += 1;
                if tries > RETRIES {
                    inp.cancel();
                    return Err("sender is silent".into());
                }
                inp.send(&[poke])?;
                continue;
            }
        };
        let tail = if crc { 2 } else { 1 };
        let rest = inp.exact(2 + size + tail, Duration::from_secs(2))?;
        let ok = rest.as_ref().and_then(|r| {
            let (num, inv, data) = (r[0], r[1], &r[2..2 + size]);
            let good = num == !inv
                && if crc {
                    crc16(data).to_be_bytes() == r[2 + size..]
                } else {
                    checksum(data) == r[2 + size]
                };
            good.then(|| Got::Block(num, data.to_vec()))
        });
        match ok {
            Some(b) => return Ok(b),
            None => {
                tries += 1;
                if tries > RETRIES {
                    inp.cancel();
                    return Err("too many bad blocks".into());
                }
                inp.purge()?;
                inp.send(&[NAK])?;
            }
        }
    }
}

/// Receives one XMODEM file into `out`. XMODEM has no length, so the last
/// block's 0x1A padding stays in the file.
pub fn xmodem_recv(w: &mut dyn Wire, out: &mut dyn Write, name: &str, ctl: &mut Ctl) -> Result<u64, String> {
    let mut inp = In::new(w);
    let mut expect: u8 = 1;
    let mut done = 0u64;
    inp.send(&[CRC_C])?;
    let mut first = true;
    loop {
        match read_block(&mut inp, ctl, true, if first { CRC_C } else { NAK }, first)? {
            Got::Eot => {
                inp.send(&[ACK])?;
                out.flush().map_err(err)?;
                return Ok(done);
            }
            Got::Block(num, data) if num == expect => {
                out.write_all(&data).map_err(err)?;
                done += data.len() as u64;
                expect = expect.wrapping_add(1);
                first = false;
                (ctl.progress)(name, done, 0);
                inp.send(&[ACK])?;
            }
            // A repeat of the last block: our ACK was lost.
            Got::Block(num, _) if num == expect.wrapping_sub(1) => inp.send(&[ACK])?,
            Got::Block(num, _) => {
                inp.cancel();
                return Err(format!("block {num} out of sequence (wanted {expect})"));
            }
        }
    }
}

/// Safe local name for a name sent by the other side.
fn local_name(sent: &str) -> String {
    let base = sent.rsplit('/').next().unwrap_or("").trim();
    if base.is_empty() || base == "." || base == ".." {
        "received".into()
    } else {
        base.to_string()
    }
}

fn free_path(dir: &Path, name: &str) -> PathBuf {
    let p = dir.join(name);
    if !p.exists() {
        return p;
    }
    dir.join(free_name(name, &mut |c| dir.join(c).exists()))
}

/// Receives a YMODEM batch into `dir`. Returns the saved paths.
pub fn ymodem_recv(w: &mut dyn Wire, dir: &Path, ctl: &mut Ctl) -> Result<Vec<PathBuf>, String> {
    let mut inp = In::new(w);
    let mut saved = Vec::new();
    loop {
        inp.send(&[CRC_C])?;
        let (name, size) = match read_block(&mut inp, ctl, true, CRC_C, true)? {
            Got::Block(0, data) => {
                let end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
                if end == 0 {
                    inp.send(&[ACK])?;
                    return Ok(saved);
                }
                let name = String::from_utf8_lossy(&data[..end]).into_owned();
                let meta = String::from_utf8_lossy(&data[end + 1..]);
                let size = meta.split([' ', '\0']).next().and_then(|s| s.parse::<u64>().ok());
                (name, size)
            }
            Got::Block(n, _) => return Err(format!("expected header block, got block {n}")),
            Got::Eot => {
                inp.send(&[ACK])?;
                continue;
            }
        };
        inp.send(&[ACK])?;
        let path = free_path(dir, &local_name(&name));
        let mut f = File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        inp.send(&[CRC_C])?;
        let mut expect: u8 = 1;
        let mut done = 0u64;
        let mut eots = 0;
        loop {
            match read_block(&mut inp, ctl, true, NAK, false)? {
                Got::Eot => {
                    // First EOT is NAKed, the second confirmed (YMODEM).
                    eots += 1;
                    if eots == 1 {
                        inp.send(&[NAK])?;
                        continue;
                    }
                    inp.send(&[ACK])?;
                    break;
                }
                Got::Block(num, data) if num == expect => {
                    // The header's size cuts off the last block's padding.
                    let keep = size.map_or(data.len() as u64, |s| s.saturating_sub(done).min(data.len() as u64));
                    f.write_all(&data[..keep as usize]).map_err(err)?;
                    done += keep;
                    expect = expect.wrapping_add(1);
                    (ctl.progress)(&name, done, size.unwrap_or(0));
                    inp.send(&[ACK])?;
                }
                Got::Block(num, _) if num == expect.wrapping_sub(1) => inp.send(&[ACK])?,
                Got::Block(num, _) => {
                    inp.cancel();
                    return Err(format!("block {num} out of sequence (wanted {expect})"));
                }
            }
        }
        f.flush().map_err(err)?;
        saved.push(path);
    }
}

// ----------------------------------------------------------------- ZMODEM

/// Sends whatever the machine still has queued (the closing ZFIN or "OO").
/// zmodem2 reports SessionCompleted before its last bytes are written; the
/// other side would otherwise wait for them until it times out.
fn drain(w: &mut dyn Wire, mut next: impl FnMut() -> Option<Vec<u8>>, mut done: impl FnMut(usize)) -> Result<(), String> {
    while let Some(b) = next() {
        w.send(&b).map_err(err)?;
        done(b.len());
    }
    Ok(())
}

fn zerr(e: zmodem2::Error) -> String {
    format!("zmodem: {e:?}")
}

/// Feeds pending wire input to a state machine; drops input after a CRC
/// error (the machine has already asked for a resend).
fn feed(pending: &mut Vec<u8>, submit: impl FnOnce(&[u8]) -> Result<usize, zmodem2::Error>) -> Result<bool, String> {
    match submit(pending) {
        Ok(n) => {
            pending.drain(..n);
            Ok(n > 0)
        }
        Err(zmodem2::Error::UnexpectedCrc16 | zmodem2::Error::UnexpectedCrc32) => {
            pending.clear();
            Ok(true)
        }
        Err(e) => Err(zerr(e)),
    }
}

pub fn zmodem_send(w: &mut dyn Wire, files: &[Outgoing], ctl: &mut Ctl) -> Result<u64, String> {
    let mut s = Sender::new().map_err(zerr)?;
    let mut queue = files.iter();
    let mut cur: Option<(String, u64, File)> = None;
    let mut total = 0u64;
    let mut start_next = |s: &mut Sender, cur: &mut Option<(String, u64, File)>| -> Result<bool, String> {
        let Some(next) = queue.next() else { return Ok(false) };
        let (name, size, mtime, f) = next.open()?;
        let size32 = u32::try_from(size).map_err(|_| format!("{name}: larger than 4 GiB"))?;
        let info = FileInfo::new(name.as_bytes(), Some(Position::new(size32))).with_modified(mtime).with_mode(0o100_644);
        s.start_file(info).map_err(zerr)?;
        *cur = Some((name, size, f));
        Ok(true)
    };
    if !start_next(&mut s, &mut cur)? {
        return Ok(0);
    }
    let mut pending: Vec<u8> = Vec::new();
    let mut quiet = 0;
    let mut aborted = false;
    loop {
        if ctl.stopped() && !aborted {
            aborted = true;
            s.abort();
        }
        match s.poll() {
            Action::WriteWire(b) => {
                let b = b.to_vec();
                w.send(&b).map_err(err)?;
                s.wire_written(b.len());
            }
            Action::ReadFile { offset, max_len } => {
                let Some((name, size, f)) = cur.as_mut() else { return Err("zmodem: read without a file".into()) };
                f.seek(SeekFrom::Start(u64::from(offset.get()))).map_err(err)?;
                let mut buf = vec![0u8; max_len];
                let n = read_full(f, &mut buf)?;
                s.submit_file(&buf[..n]).map_err(zerr)?;
                (ctl.progress)(name, u64::from(offset.get()) + n as u64, *size);
            }
            Action::Event(ZEvent::FileCompleted) => {
                if let Some((_, size, _)) = cur.take() {
                    total += size;
                }
                if !start_next(&mut s, &mut cur)? {
                    s.finish().map_err(zerr)?;
                }
            }
            Action::Event(ZEvent::SessionCompleted) => {
                let s = std::cell::RefCell::new(s);
                drain(
                    w,
                    || match s.borrow_mut().poll() {
                        Action::WriteWire(b) => Some(b.to_vec()),
                        _ => None,
                    },
                    |n| s.borrow_mut().wire_written(n),
                )?;
                return Ok(total);
            }
            Action::Event(ZEvent::Aborted) => {
                return Err(if aborted { "cancelled".into() } else { "aborted by receiver".into() });
            }
            Action::Event(_) | Action::WriteFile(_) => {}
            Action::Idle => {
                if !pending.is_empty() && feed(&mut pending, |p| s.submit_wire(p))? {
                    continue;
                }
                let d = w.recv(Duration::from_secs(5)).map_err(err)?;
                if d.is_empty() {
                    quiet += 1;
                    if quiet > 12 {
                        return Err("receiver is silent".into());
                    }
                    s.timeout().map_err(zerr)?;
                } else {
                    quiet = 0;
                    pending.extend(d);
                }
            }
            _ => {}
        }
    }
}

fn with_part(p: &Path) -> PathBuf {
    let mut s = p.as_os_str().to_owned();
    s.push(".part");
    PathBuf::from(s)
}

/// Receives a ZMODEM batch into `dir`. Each file is written to
/// "<name>.part" first; an existing .part is resumed (ZRPOS).
pub fn zmodem_recv(w: &mut dyn Wire, dir: &Path, ctl: &mut Ctl) -> Result<Vec<PathBuf>, String> {
    let mut r = Receiver::new().map_err(zerr)?;
    r.set_manual_file_accept(true);
    let mut saved = Vec::new();
    // (name, final path, part path, file, size, done)
    let mut cur: Option<(String, PathBuf, PathBuf, File, u64, u64)> = None;
    let mut pending: Vec<u8> = Vec::new();
    let mut quiet = 0;
    let mut aborted = false;
    loop {
        if ctl.stopped() && !aborted {
            aborted = true;
            r.abort().map_err(zerr)?;
        }
        match r.poll() {
            Action::WriteWire(b) => {
                let b = b.to_vec();
                w.send(&b).map_err(err)?;
                r.wire_written(b.len());
            }
            Action::WriteFile(b) => {
                let b = b.to_vec();
                let Some((name, _, _, f, size, done)) = cur.as_mut() else { return Err("zmodem: data without a file".into()) };
                f.write_all(&b).map_err(err)?;
                *done += b.len() as u64;
                (ctl.progress)(name, *done, *size);
                r.file_written(b.len()).map_err(zerr)?;
            }
            Action::Event(ZEvent::FileStarted(info)) => {
                let name = local_name(&String::from_utf8_lossy(info.name));
                let size = info.size.map_or(0, |p| u64::from(p.get()));
                let target = dir.join(&name);
                let part = with_part(&target);
                let have = fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
                let start = if have > 0 && (size == 0 || have < size) { have } else { 0 };
                let final_path = if target.exists() { free_path(dir, &name) } else { target };
                let f = OpenOptions::new()
                    .create(true)
                    .write(true)
                    .append(start > 0)
                    .truncate(start == 0)
                    .open(&part)
                    .map_err(|e| format!("{}: {e}", part.display()))?;
                let start32 = u32::try_from(start).unwrap_or(0);
                cur = Some((name, final_path, part, f, size, start));
                r.accept_file_at(start32).map_err(zerr)?;
            }
            Action::Event(ZEvent::FileCompleted) => {
                if let Some((_, path, part, mut f, _, _)) = cur.take() {
                    f.flush().map_err(err)?;
                    drop(f);
                    fs::rename(&part, &path).map_err(|e| format!("{}: {e}", path.display()))?;
                    saved.push(path);
                }
            }
            Action::Event(ZEvent::SessionCompleted) => {
                let r = std::cell::RefCell::new(r);
                drain(
                    w,
                    || match r.borrow_mut().poll() {
                        Action::WriteWire(b) => Some(b.to_vec()),
                        _ => None,
                    },
                    |n| r.borrow_mut().wire_written(n),
                )?;
                return Ok(saved);
            }
            Action::Event(ZEvent::Aborted) => {
                if let Some((_, _, part, _, _, _)) = cur.take() {
                    if aborted {
                        let _ = fs::remove_file(&part);
                    }
                }
                return Err(if aborted { "cancelled".into() } else { "aborted by sender".into() });
            }
            Action::Event(_) | Action::ReadFile { .. } => {}
            Action::Idle => {
                if !pending.is_empty() && feed(&mut pending, |p| r.submit_wire(p))? {
                    continue;
                }
                let d = w.recv(Duration::from_secs(5)).map_err(err)?;
                if d.is_empty() {
                    quiet += 1;
                    if quiet > 12 {
                        return Err("sender is silent".into());
                    }
                    r.timeout().map_err(zerr)?;
                } else {
                    quiet = 0;
                    pending.extend(d);
                }
            }
            _ => {}
        }
    }
}

/// True when `data` holds the ZMODEM start a remote `sz` sends
/// ("**\x18B00", ZRQINIT).
pub fn sz_started(data: &[u8]) -> bool {
    data.windows(6).any(|w| w == b"**\x18B00")
}

/// True when `data` holds the ZRINIT a remote `rz` sends ("**\x18B01").
pub fn rz_started(data: &[u8]) -> bool {
    data.windows(6).any(|w| w == b"**\x18B01")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command, Stdio};
    use std::thread;

    fn have(prog: &str) -> bool {
        crate::term::found(prog)
    }

    /// Runs `argv` in `dir` with its stdin/stdout as our wire.
    fn peer(argv: &[&str], dir: &Path) -> (ChanWire, Child) {
        let mut child = Command::new(argv[0])
            .args(&argv[1..])
            .current_dir(dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn");
        let mut out = child.stdout.take().expect("stdout");
        let mut input = child.stdin.take().expect("stdin");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            while let Ok(n) = out.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        let send = Box::new(move |d: &[u8]| {
            input.write_all(d)?;
            input.flush()
        });
        (ChanWire { rx, tx: send }, child)
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tv-modem-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).expect("dir");
        d
    }

    fn sample(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 37 % 256) as u8).collect()
    }

    fn quiet_ctl<'a>(flag: &'a AtomicBool, p: &'a mut dyn FnMut(&str, u64, u64)) -> Ctl<'a> {
        Ctl { cancel: flag, progress: p }
    }

    /// sz on one end of a pty, our serial layer on the other: the same path
    /// the stream pane uses (serial threads, link sink, raw mode).
    #[test]
    fn zmodem_over_serial_pty() {
        use crate::link::Event;
        use crate::serial::{self, SerialCfg};
        use serialport::SerialPort;
        use std::os::fd::{FromRawFd, IntoRawFd, OwnedFd};
        if !have("sz") {
            return;
        }
        let dir = scratch("pty");
        let data = sample(150_000);
        fs::write(dir.join("fw.bin"), &data).expect("src");

        let (master, slave) = serialport::TTYPort::pair().expect("pty");
        let device = slave.name().expect("name");
        drop(slave);
        let a = master.try_clone_native().expect("clone").into_raw_fd();
        let b = master.try_clone_native().expect("clone").into_raw_fd();
        // SAFETY: a and b are fresh descriptors owned by nothing else.
        let (inp, out) = unsafe { (OwnedFd::from_raw_fd(a), OwnedFd::from_raw_fd(b)) };
        let mut child = Command::new("sz")
            .args(["-q", "fw.bin"])
            .current_dir(&dir)
            .stdin(Stdio::from(inp))
            .stdout(Stdio::from(out))
            .stderr(Stdio::null())
            .spawn()
            .expect("sz");

        let cfg = SerialCfg { device, ..Default::default() };
        let (conn, events) = serial::open(&cfg).expect("open");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(ev) = events.recv_blocking() {
                if let Event::Data(d) = ev {
                    if tx.send(d).is_err() {
                        break;
                    }
                }
            }
        });
        let mut w = ChanWire { rx, tx: conn.sink() };
        let back = dir.join("back");
        fs::create_dir_all(&back).expect("back");
        let flag = AtomicBool::new(false);
        let mut p = |_: &str, _: u64, _: u64| {};
        let saved = zmodem_recv(&mut w, &back, &mut quiet_ctl(&flag, &mut p)).expect("recv");
        let _ = child.wait();
        drop(conn);
        assert_eq!(saved, [back.join("fw.bin")]);
        assert!(fs::read(back.join("fw.bin")).expect("read") == data);
        drop(master);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn crc_matches_reference() {
        // CRC-16/XMODEM check value for "123456789".
        assert_eq!(crc16(b"123456789"), 0x31C3);
    }

    #[test]
    fn detects_zmodem_start() {
        assert!(sz_started(b"rz waiting\r\n**\x18B00000000000000\r\n"));
        assert!(rz_started(b"**\x18B0100000023be50\r\n"));
        assert!(!sz_started(b"plain text"));
    }

    #[test]
    fn xmodem_to_rx() {
        if !have("rx") {
            return;
        }
        for k1 in [false, true] {
            let dir = scratch(if k1 { "x1k" } else { "x" });
            let data = sample(5000);
            fs::write(dir.join("src.bin"), &data).expect("src");
            let (mut w, mut child) = peer(&["rx", "-c", "-q", "out.bin"], &dir);
            let flag = AtomicBool::new(false);
            let mut p = |_: &str, _: u64, _: u64| {};
            let n = xmodem_send(&mut w, &Outgoing { path: dir.join("src.bin") }, k1, &mut quiet_ctl(&flag, &mut p)).expect("send");
            assert_eq!(n, 5000);
            drop(w);
        let _ = child.wait();
            let got = fs::read(dir.join("out.bin")).expect("out");
            assert_eq!(&got[..5000], &data[..]);
            assert!(got[5000..].iter().all(|&b| b == SUB));
            let _ = fs::remove_dir_all(&dir);
        }
    }

    #[test]
    fn xmodem_from_sx() {
        if !have("sx") {
            return;
        }
        let dir = scratch("xr");
        let data = sample(3000);
        fs::write(dir.join("src.bin"), &data).expect("src");
        let (mut w, mut child) = peer(&["sx", "-k", "-q", "src.bin"], &dir);
        let flag = AtomicBool::new(false);
        let mut p = |_: &str, _: u64, _: u64| {};
        let mut out = Vec::new();
        xmodem_recv(&mut w, &mut out, "src.bin", &mut quiet_ctl(&flag, &mut p)).expect("recv");
        drop(w);
        let _ = child.wait();
        assert_eq!(&out[..3000], &data[..]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn ymodem_both_ways() {
        if !have("rb") || !have("sb") {
            return;
        }
        let dir = scratch("y");
        let a = sample(4321);
        let b = sample(100);
        fs::create_dir_all(dir.join("in")).expect("in");
        fs::write(dir.join("a.bin"), &a).expect("a");
        fs::write(dir.join("b.txt"), &b).expect("b");

        let (mut w, mut child) = peer(&["rb", "-q"], &dir.join("in"));
        let flag = AtomicBool::new(false);
        let mut p = |_: &str, _: u64, _: u64| {};
        let files = [Outgoing { path: dir.join("a.bin") }, Outgoing { path: dir.join("b.txt") }];
        ymodem_send(&mut w, &files, &mut quiet_ctl(&flag, &mut p)).expect("send");
        drop(w);
        let _ = child.wait();
        assert_eq!(fs::read(dir.join("in/a.bin")).expect("a"), a);
        assert_eq!(fs::read(dir.join("in/b.txt")).expect("b"), b);

        let back = dir.join("back");
        fs::create_dir_all(&back).expect("back");
        let (mut w, mut child) = peer(&["sb", "-q", "a.bin", "b.txt"], &dir);
        let saved = ymodem_recv(&mut w, &back, &mut quiet_ctl(&flag, &mut p)).expect("recv");
        drop(w);
        let _ = child.wait();
        assert_eq!(saved.len(), 2);
        assert_eq!(fs::read(back.join("a.bin")).expect("a"), a);
        assert_eq!(fs::read(back.join("b.txt")).expect("b"), b);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn zmodem_both_ways() {
        if !have("rz") || !have("sz") {
            return;
        }
        let dir = scratch("z");
        let a = sample(200_000);
        fs::create_dir_all(dir.join("in")).expect("in");
        fs::write(dir.join("a.bin"), &a).expect("a");

        let (mut w, mut child) = peer(&["rz", "-q", "-y"], &dir.join("in"));
        let flag = AtomicBool::new(false);
        let mut p = |_: &str, _: u64, _: u64| {};
        zmodem_send(&mut w, &[Outgoing { path: dir.join("a.bin") }], &mut quiet_ctl(&flag, &mut p)).expect("send");
        drop(w);
        let _ = child.wait();
        assert!(fs::read(dir.join("in/a.bin")).expect("a") == a);

        // Receive with a .part already holding the first 50 000 bytes.
        let back = dir.join("back");
        fs::create_dir_all(&back).expect("back");
        fs::write(back.join("a.bin.part"), &a[..50_000]).expect("part");
        let (mut w, mut child) = peer(&["sz", "-q", "a.bin"], &dir);
        let mut seen_first = None;
        let mut p2 = |_: &str, d: u64, _: u64| {
            seen_first.get_or_insert(d);
        };
        let saved = zmodem_recv(&mut w, &back, &mut quiet_ctl(&flag, &mut p2)).expect("recv");
        drop(w);
        let _ = child.wait();
        assert_eq!(saved, [back.join("a.bin")]);
        assert!(fs::read(back.join("a.bin")).expect("a") == a);
        assert!(!back.join("a.bin.part").exists());
        assert!(seen_first.is_some_and(|d| d > 50_000), "resumed at {seen_first:?}");
        let _ = fs::remove_dir_all(&dir);
    }
}
