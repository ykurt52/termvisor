// SFTP v3 client (draft-ietf-secsh-filexfer-02) over any byte pipe.
//
// In the app the pipe is `ssh -s <host> sftp` riding on the ControlMaster
// connection of an open SSH tab, so no second login is needed. Blocking;
// each instance is used from one worker thread. Bulk reads and writes keep
// several requests in flight.

use std::collections::{BTreeMap, HashMap};
use std::io::{self, BufReader, BufWriter, Read, Write};
use std::process::{Child, Command, Stdio};

const INIT: u8 = 1;
const VERSION: u8 = 2;
const OPEN: u8 = 3;
const CLOSE: u8 = 4;
const READ: u8 = 5;
const WRITE: u8 = 6;
const LSTAT: u8 = 7;
const SETSTAT: u8 = 9;
const OPENDIR: u8 = 11;
const READDIR: u8 = 12;
const REMOVE: u8 = 13;
const MKDIR: u8 = 14;
const RMDIR: u8 = 15;
const REALPATH: u8 = 16;
const STAT: u8 = 17;
const RENAME: u8 = 18;
const STATUS: u8 = 101;
const HANDLE: u8 = 102;
const DATA: u8 = 103;
const NAME: u8 = 104;
const ATTRS: u8 = 105;

const FX_OK: u32 = 0;
const FX_EOF: u32 = 1;

const ATTR_SIZE: u32 = 0x1;
const ATTR_UIDGID: u32 = 0x2;
const ATTR_PERMISSIONS: u32 = 0x4;
const ATTR_ACMODTIME: u32 = 0x8;
const ATTR_EXTENDED: u32 = 0x8000_0000;

const OPEN_READ: u32 = 0x1;
const OPEN_WRITE: u32 = 0x2;
const OPEN_CREAT: u32 = 0x8;
const OPEN_TRUNC: u32 = 0x10;

/// Bytes per READ/WRITE request; OpenSSH accepts up to 255 KiB.
const CHUNK: u32 = 32 * 1024;
/// Requests kept in flight during bulk transfers.
const WINDOW: usize = 16;
/// Upper bound for one incoming packet.
const MAX_PACKET: usize = 1 << 20;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Attrs {
    pub size: Option<u64>,
    pub perm: Option<u32>,
    pub mtime: Option<u32>,
    pub uid: Option<u32>,
    pub gid: Option<u32>,
}

impl Attrs {
    pub fn is_dir(&self) -> bool {
        self.perm.is_some_and(|p| p & 0o170000 == 0o040000)
    }

    pub fn is_link(&self) -> bool {
        self.perm.is_some_and(|p| p & 0o170000 == 0o120000)
    }

    /// "drwxr-xr-x" style text.
    pub fn mode_text(&self) -> String {
        let Some(p) = self.perm else { return "?".repeat(10) };
        let kind = match p & 0o170000 {
            0o040000 => 'd',
            0o120000 => 'l',
            0o020000 => 'c',
            0o060000 => 'b',
            0o010000 => 'p',
            0o140000 => 's',
            _ => '-',
        };
        let mut s = String::from(kind);
        for shift in [6, 3, 0] {
            let b = (p >> shift) & 7;
            s.push(if b & 4 != 0 { 'r' } else { '-' });
            s.push(if b & 2 != 0 { 'w' } else { '-' });
            s.push(if b & 1 != 0 { 'x' } else { '-' });
        }
        s
    }
}

#[derive(Clone, Debug)]
pub struct DirEntry {
    pub name: String,
    pub attrs: Attrs,
}

/// Reads fields from a packet body.
struct Cur<'a> {
    b: &'a [u8],
}

impl<'a> Cur<'a> {
    fn u32(&mut self) -> io::Result<u32> {
        if self.b.len() < 4 {
            return Err(short());
        }
        let (h, rest) = self.b.split_at(4);
        self.b = rest;
        Ok(u32::from_be_bytes([h[0], h[1], h[2], h[3]]))
    }

    fn u64(&mut self) -> io::Result<u64> {
        let hi = u64::from(self.u32()?);
        let lo = u64::from(self.u32()?);
        Ok(hi << 32 | lo)
    }

    fn bytes(&mut self) -> io::Result<&'a [u8]> {
        let n = self.u32()? as usize;
        if self.b.len() < n {
            return Err(short());
        }
        let (h, rest) = self.b.split_at(n);
        self.b = rest;
        Ok(h)
    }

    fn string(&mut self) -> io::Result<String> {
        Ok(String::from_utf8_lossy(self.bytes()?).into_owned())
    }

    fn attrs(&mut self) -> io::Result<Attrs> {
        let flags = self.u32()?;
        let mut a = Attrs::default();
        if flags & ATTR_SIZE != 0 {
            a.size = Some(self.u64()?);
        }
        if flags & ATTR_UIDGID != 0 {
            a.uid = Some(self.u32()?);
            a.gid = Some(self.u32()?);
        }
        if flags & ATTR_PERMISSIONS != 0 {
            a.perm = Some(self.u32()?);
        }
        if flags & ATTR_ACMODTIME != 0 {
            let _atime = self.u32()?;
            a.mtime = Some(self.u32()?);
        }
        if flags & ATTR_EXTENDED != 0 {
            for _ in 0..self.u32()? {
                self.bytes()?;
                self.bytes()?;
            }
        }
        Ok(a)
    }
}

fn short() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "short sftp packet")
}

/// Builds a packet body.
#[derive(Default)]
struct Out(Vec<u8>);

impl Out {
    fn u32(mut self, v: u32) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }

    fn u64(mut self, v: u64) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }

    fn bytes(mut self, v: &[u8]) -> Self {
        self = self.u32(v.len() as u32);
        self.0.extend_from_slice(v);
        self
    }

    fn str(self, v: &str) -> Self {
        self.bytes(v.as_bytes())
    }
}

fn status_text(code: u32, msg: &str) -> String {
    let what = match code {
        2 => "no such file",
        3 => "permission denied",
        4 => "failure",
        5 => "bad message",
        6 => "no connection",
        7 => "connection lost",
        8 => "operation not supported",
        _ => "error",
    };
    if msg.is_empty() {
        what.to_string()
    } else {
        format!("{what}: {msg}")
    }
}

/// Progress callback: bytes done so far; return false to cancel.
pub type Progress<'a> = &'a mut dyn FnMut(u64) -> bool;

pub struct Sftp {
    r: BufReader<Box<dyn Read + Send>>,
    w: BufWriter<Box<dyn Write + Send>>,
    next: u32,
    child: Option<Child>,
}

impl Drop for Sftp {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

impl Sftp {
    /// Runs `argv` (e.g. `ssh … -s host sftp` or a local sftp-server) and
    /// speaks SFTP over its stdin/stdout.
    pub fn spawn(argv: &[String], env: &[(String, String)]) -> Result<Self, String> {
        let (prog, args) = argv.split_first().ok_or("empty command")?;
        let mut child = Command::new(prog)
            .args(args)
            .envs(env.iter().map(|(k, v)| (k, v)))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("{prog}: {e}"))?;
        let (Some(w), Some(r)) = (child.stdin.take(), child.stdout.take()) else {
            return Err("no pipes".into());
        };
        let err = child.stderr.take();
        let mut s = Self { r: BufReader::new(Box::new(r)), w: BufWriter::new(Box::new(w)), next: 1, child: Some(child) };
        match s.init() {
            Ok(()) => Ok(s),
            Err(e) => {
                // ssh explains failures on stderr; prefer that text.
                let _ = s.child.as_mut().map(Child::kill);
                let mut text = String::new();
                if let Some(mut err) = err {
                    let _ = err.read_to_string(&mut text);
                }
                let text = text.trim();
                Err(if text.is_empty() { e } else { text.to_string() })
            }
        }
    }

    fn init(&mut self) -> Result<(), String> {
        self.put(INIT, &Out::default().u32(3).0).map_err(|e| e.to_string())?;
        let (t, _) = self.get().map_err(|e| format!("sftp start: {e}"))?;
        if t != VERSION {
            return Err("sftp: unexpected reply to init".into());
        }
        Ok(())
    }

    fn put(&mut self, kind: u8, body: &[u8]) -> io::Result<()> {
        self.w.write_all(&((body.len() + 1) as u32).to_be_bytes())?;
        self.w.write_all(&[kind])?;
        self.w.write_all(body)?;
        Ok(())
    }

    fn get(&mut self) -> io::Result<(u8, Vec<u8>)> {
        self.w.flush()?;
        let mut len = [0u8; 4];
        self.r.read_exact(&mut len)?;
        let n = u32::from_be_bytes(len) as usize;
        if n == 0 || n > MAX_PACKET {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "bad sftp packet length"));
        }
        let mut buf = vec![0u8; n];
        self.r.read_exact(&mut buf)?;
        let kind = buf.remove(0);
        Ok((kind, buf))
    }

    fn id(&mut self) -> u32 {
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        id
    }

    /// Sends one request and waits for its reply: (type, body after id).
    fn call(&mut self, kind: u8, body: Out) -> Result<(u8, Vec<u8>), String> {
        let id = self.id();
        let mut pkt = Out::default().u32(id).0;
        pkt.extend_from_slice(&body.0);
        self.put(kind, &pkt).map_err(|e| e.to_string())?;
        let (t, b) = self.get().map_err(|e| e.to_string())?;
        let mut c = Cur { b: &b };
        if c.u32().map_err(|e| e.to_string())? != id {
            return Err("sftp: reply id mismatch".into());
        }
        Ok((t, c.b.to_vec()))
    }

    /// Expects STATUS OK.
    fn ok(&mut self, kind: u8, body: Out) -> Result<(), String> {
        let (t, b) = self.call(kind, body)?;
        status(t, &b)
    }

    fn handle(&mut self, kind: u8, body: Out) -> Result<Vec<u8>, String> {
        let (t, b) = self.call(kind, body)?;
        match t {
            HANDLE => Cur { b: &b }.bytes().map(<[u8]>::to_vec).map_err(|e| e.to_string()),
            _ => Err(status(t, &b).err().unwrap_or_else(|| "sftp: expected handle".into())),
        }
    }

    fn close(&mut self, h: &[u8]) -> Result<(), String> {
        self.ok(CLOSE, Out::default().bytes(h))
    }

    /// Absolute form of `path` ("." gives the home directory).
    pub fn realpath(&mut self, path: &str) -> Result<String, String> {
        let (t, b) = self.call(REALPATH, Out::default().str(path))?;
        if t != NAME {
            return Err(status(t, &b).err().unwrap_or_else(|| "sftp: expected name".into()));
        }
        let mut c = Cur { b: &b };
        let _count = c.u32().map_err(|e| e.to_string())?;
        c.string().map_err(|e| e.to_string())
    }

    fn attrs_of(&mut self, kind: u8, path: &str) -> Result<Attrs, String> {
        let (t, b) = self.call(kind, Out::default().str(path))?;
        if t != ATTRS {
            return Err(status(t, &b).err().unwrap_or_else(|| "sftp: expected attrs".into()));
        }
        Cur { b: &b }.attrs().map_err(|e| e.to_string())
    }

    /// Follows symlinks.
    pub fn stat(&mut self, path: &str) -> Result<Attrs, String> {
        self.attrs_of(STAT, path)
    }

    pub fn lstat(&mut self, path: &str) -> Result<Attrs, String> {
        self.attrs_of(LSTAT, path)
    }

    /// Directory entries without "." and "..", directories first, by name.
    pub fn list(&mut self, path: &str) -> Result<Vec<DirEntry>, String> {
        let h = self.handle(OPENDIR, Out::default().str(path))?;
        let mut out = Vec::new();
        let res = loop {
            let (t, b) = match self.call(READDIR, Out::default().bytes(&h)) {
                Ok(r) => r,
                Err(e) => break Err(e),
            };
            if t != NAME {
                break match t {
                    STATUS if Cur { b: &b }.u32().ok() == Some(FX_EOF) => Ok(()),
                    _ => status(t, &b),
                };
            }
            let parsed = (|| -> io::Result<()> {
                let mut c = Cur { b: &b };
                for _ in 0..c.u32()? {
                    let name = c.string()?;
                    let _long = c.bytes()?;
                    let attrs = c.attrs()?;
                    if name != "." && name != ".." {
                        out.push(DirEntry { name, attrs });
                    }
                }
                Ok(())
            })();
            if let Err(e) = parsed {
                break Err(e.to_string());
            }
        };
        let _ = self.close(&h);
        res?;
        out.sort_by(|a, b| b.attrs.is_dir().cmp(&a.attrs.is_dir()).then_with(|| a.name.cmp(&b.name)));
        Ok(out)
    }

    pub fn mkdir(&mut self, path: &str) -> Result<(), String> {
        self.ok(MKDIR, Out::default().str(path).u32(0))
    }

    pub fn rmdir(&mut self, path: &str) -> Result<(), String> {
        self.ok(RMDIR, Out::default().str(path))
    }

    pub fn remove(&mut self, path: &str) -> Result<(), String> {
        self.ok(REMOVE, Out::default().str(path))
    }

    pub fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        self.ok(RENAME, Out::default().str(from).str(to))
    }

    pub fn chmod(&mut self, path: &str, mode: u32) -> Result<(), String> {
        self.ok(SETSTAT, Out::default().str(path).u32(ATTR_PERMISSIONS).u32(mode & 0o7777))
    }

    /// Deletes a file, or a directory with everything inside.
    pub fn remove_all(&mut self, path: &str) -> Result<(), String> {
        let a = self.lstat(path)?;
        if !a.is_dir() {
            return self.remove(path);
        }
        for e in self.list(path)? {
            self.remove_all(&join(path, &e.name))?;
        }
        self.rmdir(path)
    }

    /// Copies a remote file into `out`. Returns the byte count.
    pub fn download(&mut self, remote: &str, out: &mut dyn Write, progress: Progress) -> Result<u64, String> {
        let h = self.handle(OPEN, Out::default().str(remote).u32(OPEN_READ).u32(0))?;
        let res = self.read_all(&h, out, progress);
        let _ = self.close(&h);
        res
    }

    fn read_all(&mut self, h: &[u8], out: &mut dyn Write, progress: Progress) -> Result<u64, String> {
        // id -> (offset, length) of requests in flight.
        let mut flight: HashMap<u32, (u64, u32)> = HashMap::new();
        // Received but not yet written, keyed by offset.
        let mut parked: BTreeMap<u64, Vec<u8>> = BTreeMap::new();
        // Ranges that came back short and must be asked again.
        let mut again: Vec<(u64, u32)> = Vec::new();
        let (mut next, mut done, mut eof_at) = (0u64, 0u64, u64::MAX);
        // On cancel we stop asking but still drain the replies in flight,
        // so the next request on this session gets its own reply.
        let mut cancel = false;

        loop {
            while !cancel && flight.len() < WINDOW {
                let (off, len) = match again.pop() {
                    Some(r) => r,
                    None if next < eof_at => {
                        let r = (next, CHUNK);
                        next += u64::from(CHUNK);
                        r
                    }
                    None => break,
                };
                let id = self.id();
                let pkt = Out::default().u32(id).bytes(h).u64(off).u32(len).0;
                self.put(READ, &pkt).map_err(|e| e.to_string())?;
                flight.insert(id, (off, len));
            }
            if flight.is_empty() {
                break;
            }
            let (t, b) = self.get().map_err(|e| e.to_string())?;
            let mut c = Cur { b: &b };
            let id = c.u32().map_err(|e| e.to_string())?;
            let (off, len) = flight.remove(&id).ok_or("sftp: unknown reply id")?;
            match t {
                DATA => {
                    let data = c.bytes().map_err(|e| e.to_string())?;
                    if (data.len() as u32) < len && !data.is_empty() {
                        again.push((off + data.len() as u64, len - data.len() as u32));
                    }
                    if !data.is_empty() {
                        parked.insert(off, data.to_vec());
                    }
                }
                STATUS if c.u32().ok() == Some(FX_EOF) => eof_at = eof_at.min(off),
                _ => return Err(status(t, &b[4..]).err().unwrap_or_else(|| "sftp: read failed".into())),
            }
            if cancel {
                continue;
            }
            while let Some(chunk) = parked.remove(&done) {
                out.write_all(&chunk).map_err(|e| e.to_string())?;
                done += chunk.len() as u64;
                if !progress(done) {
                    cancel = true;
                    break;
                }
            }
        }
        if cancel {
            return Err("cancelled".into());
        }
        out.flush().map_err(|e| e.to_string())?;
        Ok(done)
    }

    /// Writes `input` to a remote file (created or truncated, `mode` for new
    /// files). Returns the byte count.
    pub fn upload(&mut self, input: &mut dyn Read, remote: &str, mode: u32, progress: Progress) -> Result<u64, String> {
        let flags = OPEN_WRITE | OPEN_CREAT | OPEN_TRUNC;
        let h = self.handle(OPEN, Out::default().str(remote).u32(flags).u32(ATTR_PERMISSIONS).u32(mode & 0o7777))?;
        let res = self.write_all(&h, input, progress);
        let closed = self.close(&h);
        let n = res?;
        closed?;
        Ok(n)
    }

    fn write_all(&mut self, h: &[u8], input: &mut dyn Read, progress: Progress) -> Result<u64, String> {
        let mut flight = 0usize;
        let (mut off, mut acked) = (0u64, 0u64);
        let mut sizes: HashMap<u32, u64> = HashMap::new();
        let mut buf = vec![0u8; CHUNK as usize];
        let mut end = false;
        let mut cancel = false;
        let mut fail: Option<String> = None;
        loop {
            while !end && !cancel && flight < WINDOW {
                let n = read_full(input, &mut buf).map_err(|e| e.to_string())?;
                if n == 0 {
                    end = true;
                    break;
                }
                let id = self.id();
                let pkt = Out::default().u32(id).bytes(h).u64(off).bytes(&buf[..n]).0;
                self.put(WRITE, &pkt).map_err(|e| e.to_string())?;
                sizes.insert(id, n as u64);
                off += n as u64;
                flight += 1;
            }
            if flight == 0 {
                break;
            }
            let (t, b) = self.get().map_err(|e| e.to_string())?;
            let mut c = Cur { b: &b };
            let id = c.u32().map_err(|e| e.to_string())?;
            flight -= 1;
            if let Err(e) = status(t, c.b) {
                // Stop sending, drain what is in flight, then report.
                cancel = true;
                fail.get_or_insert(e);
                continue;
            }
            acked += sizes.remove(&id).unwrap_or(0);
            if !cancel && !progress(acked) {
                cancel = true;
            }
        }
        match (fail, cancel) {
            (Some(e), _) => Err(e),
            (None, true) => Err("cancelled".into()),
            (None, false) => Ok(acked),
        }
    }
}

/// Fills `buf` as far as the reader allows; 0 means end of input.
fn read_full(r: &mut dyn Read, buf: &mut [u8]) -> io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..]) {
            Ok(0) => break,
            Ok(k) => n += k,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(n)
}

/// Ok for STATUS OK, Err with text otherwise. `b` starts after the id.
fn status(kind: u8, b: &[u8]) -> Result<(), String> {
    if kind != STATUS {
        return Err(format!("sftp: unexpected reply type {kind}"));
    }
    let mut c = Cur { b };
    let code = c.u32().map_err(|e| e.to_string())?;
    if code == FX_OK {
        return Ok(());
    }
    let msg = c.string().unwrap_or_default();
    Err(status_text(code, &msg))
}

/// Joins a remote directory and a name with exactly one '/'.
pub fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{dir}{name}")
    } else {
        format!("{dir}/{name}")
    }
}

/// Parent of a remote path ("/" stays "/").
pub fn parent(path: &str) -> String {
    let t = path.trim_end_matches('/');
    match t.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => t[..i].to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::{Path, PathBuf};

    /// Local OpenSSH sftp-server (Debian or Arch path).
    fn server() -> Option<Vec<String>> {
        ["/usr/lib/openssh/sftp-server", "/usr/lib/ssh/sftp-server"]
            .iter()
            .find(|p| Path::new(p).exists())
            .map(|p| vec![p.to_string()])
    }

    fn scratch(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("tv-sftp-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).expect("dir");
        d
    }

    #[test]
    fn path_helpers() {
        assert_eq!(join("/a", "b"), "/a/b");
        assert_eq!(join("/", "b"), "/b");
        assert_eq!(parent("/a/b/"), "/a");
        assert_eq!(parent("/a"), "/");
        assert_eq!(parent("/"), "/");
        let a = Attrs { perm: Some(0o40755), ..Default::default() };
        assert_eq!(a.mode_text(), "drwxr-xr-x");
        assert!(a.is_dir());
    }

    #[test]
    fn against_real_sftp_server() {
        let Some(cmd) = server() else {
            eprintln!("sftp-server not installed, skipped");
            return;
        };
        let dir = scratch("ops");
        let d = dir.to_str().expect("utf8").to_string();
        let mut s = Sftp::spawn(&cmd, &[]).expect("spawn");

        assert_eq!(s.realpath(&d).expect("realpath"), d);
        s.mkdir(&join(&d, "sub")).expect("mkdir");
        fs::write(dir.join("b.txt"), b"hello").expect("write");
        let names: Vec<String> = s.list(&d).expect("list").into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["sub", "b.txt"]);

        s.rename(&join(&d, "b.txt"), &join(&d, "c.txt")).expect("rename");
        s.chmod(&join(&d, "c.txt"), 0o600).expect("chmod");
        let a = s.stat(&join(&d, "c.txt")).expect("stat");
        assert_eq!((a.size, a.perm.map(|p| p & 0o777)), (Some(5), Some(0o600)));

        let err = s.stat(&join(&d, "missing")).expect_err("missing");
        assert!(err.starts_with("no such file"), "{err}");

        fs::write(dir.join("sub/x"), b"x").expect("x");
        s.remove_all(&join(&d, "sub")).expect("remove_all");
        assert!(!dir.join("sub").exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn bulk_transfer_roundtrip() {
        let Some(cmd) = server() else { return };
        let dir = scratch("bulk");
        let d = dir.to_str().expect("utf8").to_string();
        // Larger than WINDOW * CHUNK and not a multiple of CHUNK.
        let data: Vec<u8> = (0..1_234_567u32).map(|i| (i * 31 % 251) as u8).collect();
        let mut s = Sftp::spawn(&cmd, &[]).expect("spawn");

        let mut seen = 0;
        let up = s
            .upload(&mut &data[..], &join(&d, "big.bin"), 0o644, &mut |n| {
                seen = n;
                true
            })
            .expect("upload");
        assert_eq!(up, data.len() as u64);
        assert_eq!(seen, up);
        assert_eq!(fs::read(dir.join("big.bin")).expect("read"), data);

        let mut back = Vec::new();
        let down = s.download(&join(&d, "big.bin"), &mut back, &mut |_| true).expect("download");
        assert_eq!(down, data.len() as u64);
        assert!(back == data);

        let mut empty = Vec::new();
        fs::write(dir.join("zero"), b"").expect("zero");
        assert_eq!(s.download(&join(&d, "zero"), &mut empty, &mut |_| true), Ok(0));

        let cancelled = s.download(&join(&d, "big.bin"), &mut Vec::new(), &mut |n| n < 100_000);
        assert_eq!(cancelled, Err("cancelled".into()));
        let up_cancel = s.upload(&mut &data[..], &join(&d, "c.bin"), 0o644, &mut |n| n < 100_000);
        assert_eq!(up_cancel, Err("cancelled".into()));
        let denied = s.upload(&mut &b"x"[..], "/proc/termvisor-denied", 0o644, &mut |_| true);
        assert!(denied.is_err());
        // The session still works after a cancel.
        assert!(s.stat(&join(&d, "big.bin")).is_ok());
        let _ = fs::remove_dir_all(&dir);
    }
}
