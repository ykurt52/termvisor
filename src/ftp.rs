// FTP and explicit FTPS (AUTH TLS) backend for the Files panel and queue.

use std::io::{Read, Write};
use std::net::ToSocketAddrs;
use std::time::{Duration, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use suppaftp::list::{File, ListParser, PosixPexQuery};
use suppaftp::native_tls::TlsConnector;
use suppaftp::types::FileType;
use suppaftp::{Mode, NativeTlsConnector, NativeTlsFtpStream};

use crate::sftp::{Attrs, DirEntry, Progress};

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Tls {
    /// Plain FTP: password and data unencrypted.
    #[default]
    None,
    /// AUTH TLS on port 21 (FTPES).
    Explicit,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FtpCfg {
    pub host: String,
    pub port: u16,
    /// Empty means anonymous.
    pub user: String,
    pub tls: Tls,
    pub passive: bool,
    /// Accept self-signed or mismatched certificates (embedded devices).
    pub insecure: bool,
}

impl Default for FtpCfg {
    fn default() -> Self {
        Self { host: String::new(), port: 21, user: String::new(), tls: Tls::None, passive: true, insecure: false }
    }
}

impl FtpCfg {
    /// e.g. "ftps://yk@nas:21"; also the keyring id base.
    pub fn id(&self) -> String {
        let scheme = if self.tls == Tls::Explicit { "ftps" } else { "ftp" };
        let user = if self.user.is_empty() { "anonymous" } else { &self.user };
        format!("{scheme}://{user}@{}:{}", self.host, self.port)
    }

    pub fn check(&self) -> Result<(), String> {
        let h = self.host.trim();
        if h.is_empty() || h.contains(char::is_whitespace) {
            return Err(format!("invalid host '{}'", self.host));
        }
        if self.port == 0 {
            return Err("port must be 1-65535".into());
        }
        if self.user.contains(char::is_control) {
            return Err("invalid user".into());
        }
        Ok(())
    }

    pub fn anonymous(&self) -> bool {
        self.user.is_empty() || self.user == "anonymous"
    }
}

fn err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

/// Permission bits from a parsed listing line.
fn mode_of(f: &File) -> u32 {
    let mut m = 0;
    for (who, shift) in [(PosixPexQuery::Owner, 6), (PosixPexQuery::Group, 3), (PosixPexQuery::Others, 0)] {
        if f.can_read(who) {
            m |= 4 << shift;
        }
        if f.can_write(who) {
            m |= 2 << shift;
        }
        if f.can_execute(who) {
            m |= 1 << shift;
        }
    }
    let kind = if f.is_directory() {
        0o040000
    } else if f.is_symlink() {
        0o120000
    } else {
        0o100000
    };
    kind | m
}

fn entry_of(f: &File) -> DirEntry {
    let mtime = f.modified().duration_since(UNIX_EPOCH).ok().and_then(|d| u32::try_from(d.as_secs()).ok());
    DirEntry {
        name: f.name().to_string(),
        attrs: Attrs { size: Some(f.size() as u64), perm: Some(mode_of(f)), mtime, uid: f.uid(), gid: f.gid() },
    }
}

/// Rewrites "unix.mode=0o644" (some servers print the Python octal form)
/// to the plain "unix.mode=0644" the parser expects.
fn plain_mode(line: &str) -> String {
    let (facts, name) = match line.find("; ") {
        Some(i) => (&line[..i + 1], &line[i + 1..]),
        None => (line, ""),
    };
    let facts: Vec<String> = facts
        .split(';')
        .map(|f| match f.split_once('=') {
            Some((k, v)) if k.eq_ignore_ascii_case("unix.mode") => {
                format!("{k}={}", v.strip_prefix("0o").map(|r| format!("0{r}")).unwrap_or_else(|| v.to_string()))
            }
            _ => f.to_string(),
        })
        .collect();
    format!("{}{name}", facts.join(";"))
}

/// Parses LIST/MLSD output; lines that match no format are skipped.
pub fn parse_listing(lines: &[String], mlsd: bool) -> Vec<DirEntry> {
    let mut out: Vec<DirEntry> = lines
        .iter()
        .filter_map(|l| {
            let f = if mlsd {
                ListParser::parse_mlsd(&plain_mode(l)).ok()
            } else {
                ListParser::parse_posix(l).or_else(|_| ListParser::parse_dos(l)).ok()
            }?;
            (f.name() != "." && f.name() != "..").then(|| entry_of(&f))
        })
        .collect();
    out.sort_by(|a, b| b.attrs.is_dir().cmp(&a.attrs.is_dir()).then_with(|| a.name.cmp(&b.name)));
    out
}

pub struct Ftp {
    s: NativeTlsFtpStream,
    mlsd: bool,
    /// Set after an aborted transfer; the session is then dropped.
    broken: bool,
}

const CHUNK: usize = 64 * 1024;

impl Ftp {
    pub fn open(cfg: &FtpCfg, pass: &str) -> Result<Self, String> {
        cfg.check()?;
        let addr = (cfg.host.as_str(), cfg.port)
            .to_socket_addrs()
            .map_err(|e| format!("{}: {e}", cfg.host))?
            .next()
            .ok_or_else(|| format!("{}: no address", cfg.host))?;
        let mut s = NativeTlsFtpStream::connect_timeout(addr, Duration::from_secs(10)).map_err(err)?;
        if cfg.tls == Tls::Explicit {
            let tls = TlsConnector::builder()
                .danger_accept_invalid_certs(cfg.insecure)
                .danger_accept_invalid_hostnames(cfg.insecure)
                .build()
                .map_err(err)?;
            s = s.into_secure(NativeTlsConnector::from(tls), &cfg.host).map_err(err)?;
        }
        let (user, pass) = if cfg.anonymous() { ("anonymous", "anonymous@") } else { (cfg.user.as_str(), pass) };
        s.login(user, pass).map_err(|e| format!("login failed: {e}"))?;
        s.transfer_type(FileType::Binary).map_err(err)?;
        s.set_mode(if cfg.passive { Mode::Passive } else { Mode::Active });
        // Ask for the mode in MLSD listings; without it permissions are
        // unknown. Servers that do not support it just refuse.
        let _ = s.opts("MLST", Some("type;size;modify;UNIX.mode;UNIX.uid;UNIX.gid;"));
        Ok(Self { s, mlsd: true, broken: false })
    }

    fn listing(&mut self, path: &str) -> Result<Vec<DirEntry>, String> {
        if self.mlsd {
            match self.s.mlsd(Some(path)) {
                Ok(lines) => return Ok(parse_listing(&lines, true)),
                // Old servers lack MLSD; use LIST from now on.
                Err(_) => self.mlsd = false,
            }
        }
        let lines = self.s.list(Some(path)).map_err(err)?;
        Ok(parse_listing(&lines, false))
    }
}

impl crate::remote::Remote for Ftp {
    fn healthy(&self) -> bool {
        !self.broken
    }

    fn realpath(&mut self, path: &str) -> Result<String, String> {
        if path != "." {
            self.s.cwd(path).map_err(err)?;
        }
        self.s.pwd().map_err(err)
    }

    fn list(&mut self, path: &str) -> Result<Vec<DirEntry>, String> {
        self.listing(path)
    }

    /// FTP has no stat: look the name up in its parent's listing.
    fn stat(&mut self, path: &str) -> Result<Attrs, String> {
        let t = path.trim_end_matches('/');
        if t.is_empty() {
            return Ok(Attrs { perm: Some(0o040755), ..Default::default() });
        }
        let parent = crate::sftp::parent(t);
        let name = t.rsplit('/').next().unwrap_or(t);
        self.listing(&parent)?
            .into_iter()
            .find(|e| e.name == name)
            .map(|e| e.attrs)
            .ok_or_else(|| "no such file".to_string())
    }

    fn mkdir(&mut self, path: &str) -> Result<(), String> {
        self.s.mkdir(path).map_err(err)
    }

    fn rmdir(&mut self, path: &str) -> Result<(), String> {
        self.s.rmdir(path).map_err(err)
    }

    fn remove(&mut self, path: &str) -> Result<(), String> {
        self.s.rm(path).map_err(err)
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<(), String> {
        self.s.rename(from, to).map_err(err)
    }

    fn chmod(&mut self, path: &str, mode: u32) -> Result<(), String> {
        self.s.site(format!("CHMOD {:o} {path}", mode & 0o7777)).map(|_| ()).map_err(err)
    }

    fn download(&mut self, path: &str, start: u64, out: &mut dyn Write, progress: Progress) -> Result<u64, String> {
        if start > 0 {
            self.s.resume_transfer(usize::try_from(start).map_err(err)?).map_err(err)?;
        }
        let mut data = self.s.retr_as_stream(path).map_err(err)?;
        let mut buf = vec![0u8; CHUNK];
        let mut done = 0u64;
        loop {
            let n = match data.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => return Err(e.to_string()),
            };
            out.write_all(&buf[..n]).map_err(err)?;
            done += n as u64;
            if !progress(done) {
                let _ = self.s.abort(data);
                self.broken = true;
                return Err("cancelled".into());
            }
        }
        data.finish().map_err(err)?;
        out.flush().map_err(err)?;
        Ok(done)
    }

    fn upload(&mut self, input: &mut dyn Read, path: &str, mode: u32, start: u64, progress: Progress) -> Result<u64, String> {
        let _ = mode; // FTP sets no mode on upload; use chmod afterwards if needed.
        let mut data = if start > 0 {
            self.s.append_with_stream(path).map_err(err)?
        } else {
            self.s.put_with_stream(path).map_err(err)?
        };
        let mut buf = vec![0u8; CHUNK];
        let mut done = 0u64;
        loop {
            let n = match input.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) => return Err(e.to_string()),
            };
            data.write_all(&buf[..n]).map_err(err)?;
            done += n as u64;
            if !progress(done) {
                let _ = self.s.abort(data);
                self.broken = true;
                return Err("cancelled".into());
            }
        }
        data.finish().map_err(err)?;
        Ok(done)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_checks() {
        let mut c = FtpCfg { host: "nas".into(), ..Default::default() };
        assert_eq!(c.id(), "ftp://anonymous@nas:21");
        c.user = "yk".into();
        c.tls = Tls::Explicit;
        assert_eq!(c.id(), "ftps://yk@nas:21");
        assert!(c.check().is_ok());
        c.host = "a b".into();
        assert!(c.check().is_err());
    }

    #[test]
    fn parses_posix_and_dos_listings() {
        let lines: Vec<String> = [
            "drwxr-xr-x    2 1000     1000         4096 Nov 02 09:43 bin",
            "-rw-r--r--    1 1000     1000        12345 Nov 02 09:40 app.log",
            "lrwxrwxrwx    1 0        0              7 Jan 01  2025 lib -> usr/lib",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let e = parse_listing(&lines, false);
        assert_eq!(e.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["bin", "app.log", "lib"]);
        assert!(e[0].attrs.is_dir());
        assert_eq!(e[1].attrs.size, Some(12345));
        assert_eq!(e[1].attrs.mode_text(), "-rw-r--r--");
        assert!(e[2].attrs.is_link());

        let dos = vec!["10-19-20  03:19PM       <DIR>          pub".to_string(), "04-08-14  03:09PM                  403 readme.txt".to_string()];
        let d = parse_listing(&dos, false);
        assert_eq!(d.len(), 2);
        assert!(d[0].attrs.is_dir());
        assert_eq!(d[1].attrs.size, Some(403));
    }

    /// Talks to a real server: TV_FTP_TEST="host:port:user:pass:tls" with
    /// tls 0 or 1. Run with `cargo test -- --ignored live_ftp`.
    #[test]
    #[ignore]
    fn live_ftp() {
        use crate::remote::Remote;
        use crate::sftp::join;
        let Ok(spec) = std::env::var("TV_FTP_TEST") else { return };
        let p: Vec<&str> = spec.split(':').collect();
        let cfg = FtpCfg {
            host: p[0].into(),
            port: p[1].parse().expect("port"),
            user: p[2].into(),
            tls: if p[4] == "1" { Tls::Explicit } else { Tls::None },
            insecure: true,
            ..Default::default()
        };
        let mut f = Ftp::open(&cfg, p[3]).expect("open");
        let home = f.realpath(".").expect("pwd");
        let d = join(&home, "tvtest");
        let _ = f.remove_all(&d);
        f.mkdir(&d).expect("mkdir");

        let data: Vec<u8> = (0..300_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let file = join(&d, "a.bin");
        let half = 123_456usize;
        f.upload(&mut &data[..half], &file, 0o644, 0, &mut |_| true).expect("first half");
        f.upload(&mut &data[half..], &file, 0o644, half as u64, &mut |_| true).expect("append");
        assert_eq!(f.stat(&file).expect("stat").size, Some(data.len() as u64));

        let mut all = Vec::new();
        f.download(&file, 0, &mut all, &mut |_| true).expect("download");
        assert!(all == data);
        let mut tail = Vec::new();
        f.download(&file, half as u64, &mut tail, &mut |_| true).expect("resume");
        assert!(tail[..] == data[half..]);

        let cancelled = f.download(&file, 0, &mut Vec::new(), &mut |n| n < 50_000);
        assert_eq!(cancelled, Err("cancelled".into()));
        // An aborted session is not reused.
        assert!(!f.healthy());
        let mut f = Ftp::open(&cfg, p[3]).expect("reopen");
        let names: Vec<String> = f.list(&d).expect("list").into_iter().map(|e| e.name).collect();
        assert_eq!(names, ["a.bin"]);

        f.rename(&file, &join(&d, "b.bin")).expect("rename");
        f.chmod(&join(&d, "b.bin"), 0o600).expect("chmod");
        let a = f.stat(&join(&d, "b.bin")).expect("stat b");
        assert_eq!(a.perm.map(|m| m & 0o777), Some(0o600));
        f.mkdir(&join(&d, "sub")).expect("sub");
        f.remove_all(&d).expect("remove_all");
        assert!(f.stat(&d).is_err());
    }

    #[test]
    fn parses_mlsd() {
        let lines = vec![
            "type=dir;modify=20261102094300;perm=flcdmpe; logs".to_string(),
            "type=file;size=42;modify=20261102094000;perm=adfrw; a.txt".to_string(),
            "type=cdir;modify=20261102094300; .".to_string(),
        ];
        let e = parse_listing(&lines, true);
        assert_eq!(e.iter().map(|x| x.name.as_str()).collect::<Vec<_>>(), ["logs", "a.txt"]);
        assert_eq!(e[1].attrs.size, Some(42));

        let py = vec!["modify=20261102074501;size=7;type=file;unix.mode=0o640;unix.uid=1000; my file.txt".to_string()];
        let e = parse_listing(&py, true);
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].name, "my file.txt");
        assert_eq!(e[0].attrs.mode_text(), "-rw-r-----");
    }
}
