// Update check: asks GitHub for the latest release and tells the caller
// when it is newer than this build. Runs on every start, silent on errors.
// Also downloads a release file, checked against GitHub's SHA-256.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::Duration;

use gtk::glib;
use sha2::{Digest, Sha256};

const API: &str = "https://api.github.com/repos/ykurt52/termvisor/releases/latest";

pub struct Release {
    /// Version without the leading "v", e.g. "1.0.2".
    pub version: String,
    /// Release page.
    pub url: String,
    pub assets: Vec<Asset>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Asset {
    pub name: String,
    pub url: String,
    pub size: u64,
    /// Hex SHA-256 from GitHub, if it gave one.
    pub sha256: Option<String>,
}

/// Major, minor and patch of "v1.2.3", "1.2" or "1.2.3-rc1".
fn parts(v: &str) -> Option<(u64, u64, u64)> {
    let v = v.trim().trim_start_matches('v');
    let v = v.split(['-', '+']).next()?;
    let mut it = v.split('.').map(|p| p.parse::<u64>());
    let major = it.next()?.ok()?;
    let minor = it.next().unwrap_or(Ok(0)).ok()?;
    let patch = it.next().unwrap_or(Ok(0)).ok()?;
    Some((major, minor, patch))
}

/// True when `latest` is a higher version than `current`.
pub fn newer(latest: &str, current: &str) -> bool {
    match (parts(latest), parts(current)) {
        (Some(l), Some(c)) => l > c,
        _ => false,
    }
}

fn parse(json: &[u8]) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_slice(json).map_err(|e| e.to_string())?;
    let tag = v["tag_name"].as_str().ok_or("no tag_name")?;
    let url = v["html_url"].as_str().ok_or("no html_url")?;
    let assets = v["assets"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| {
                    Some(Asset {
                        name: x["name"].as_str()?.to_string(),
                        url: x["browser_download_url"].as_str()?.to_string(),
                        size: x["size"].as_u64().unwrap_or(0),
                        sha256: x["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).map(str::to_lowercase),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Release { version: tag.trim_start_matches('v').to_string(), url: url.to_string(), assets })
}

/// Host and path of an https URL.
fn split_url(url: &str) -> Result<(&str, &str), String> {
    let rest = url.strip_prefix("https://").ok_or_else(|| format!("not https: {url}"))?;
    Ok(match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    })
}

/// Status code, Location and Content-Length of a response head.
fn head(lines: &[String]) -> Result<(u16, Option<String>, Option<u64>), String> {
    let status = lines.first().ok_or("empty reply")?;
    let code = status.split_whitespace().nth(1).and_then(|c| c.parse().ok()).ok_or_else(|| format!("bad status: {status}"))?;
    let field = |name: &str| {
        lines.iter().skip(1).find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim().eq_ignore_ascii_case(name).then(|| v.trim().to_string())
        })
    };
    Ok((code, field("location"), field("content-length").and_then(|n| n.parse().ok())))
}

/// GET over HTTPS, following redirects. The body goes to `sink`;
/// `progress` gets bytes so far and the total (0 when unknown).
fn get(url: &str, accept: &str, sink: &mut dyn Write, progress: &mut dyn FnMut(u64, u64)) -> Result<(), String> {
    let mut url = url.to_string();
    for _ in 0..5 {
        let (host, path) = split_url(&url)?;
        let addr = (host, 443).to_socket_addrs().map_err(|e| format!("{host}: {e}"))?.next().ok_or("no address")?;
        let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(10)).map_err(|e| format!("{host}: {e}"))?;
        tcp.set_read_timeout(Some(Duration::from_secs(30))).map_err(|e| e.to_string())?;
        let tls = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
        let mut s = tls.connect(host, tcp).map_err(|e| format!("{host}: {e}"))?;
        // HTTP/1.0 so the reply is never chunked.
        let req = format!(
            "GET {path} HTTP/1.0\r\nHost: {host}\r\nUser-Agent: termvisor/{}\r\nAccept: {accept}\r\n\r\n",
            env!("CARGO_PKG_VERSION")
        );
        s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
        let mut r = BufReader::new(s);
        let mut lines = Vec::new();
        loop {
            let mut l = String::new();
            if r.read_line(&mut l).map_err(|e| e.to_string())? == 0 {
                return Err("connection closed".into());
            }
            let l = l.trim_end().to_string();
            if l.is_empty() {
                break;
            }
            lines.push(l);
        }
        let (code, location, length) = head(&lines)?;
        match code {
            200 => {
                let total = length.unwrap_or(0);
                let mut buf = vec![0u8; 64 * 1024];
                let mut got = 0u64;
                loop {
                    let n = r.read(&mut buf).map_err(|e| e.to_string())?;
                    if n == 0 {
                        break;
                    }
                    sink.write_all(&buf[..n]).map_err(|e| e.to_string())?;
                    got += n as u64;
                    progress(got, total);
                }
                if total != 0 && got != total {
                    return Err(format!("download cut short: {got} of {total} bytes"));
                }
                return Ok(());
            }
            301 | 302 | 303 | 307 | 308 => url = location.ok_or("redirect without Location")?,
            _ => return Err(format!("{}: {}", host, lines[0])),
        }
    }
    Err("too many redirects".into())
}

/// Latest release from GitHub. Blocks for up to about a minute.
fn fetch() -> Result<Release, String> {
    let mut body = Vec::new();
    get(API, "application/vnd.github+json", &mut Capped { out: &mut body, limit: 1 << 20 }, &mut |_, _| {})?;
    parse(&body)
}

/// Writer that fails past `limit` bytes.
struct Capped<'a> {
    out: &'a mut Vec<u8>,
    limit: usize,
}

impl Write for Capped<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.out.len() + buf.len() > self.limit {
            return Err(std::io::Error::other("reply too large"));
        }
        self.out.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Writer that hashes what goes into a file.
struct Hashed {
    file: fs::File,
    hash: Sha256,
}

impl Write for Hashed {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.file.write(buf)?;
        self.hash.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.file.flush()
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Downloads `asset` to `dest`. It reaches `dest` only when whole and when
/// its SHA-256 matches GitHub's; a file without a digest is refused.
pub fn download(asset: &Asset, dest: &Path, progress: &mut dyn FnMut(u64, u64)) -> Result<(), String> {
    let want = asset.sha256.as_deref().ok_or("the release gives no SHA-256 for this file")?;
    let part = dest.with_extension("part");
    let file = fs::File::create(&part).map_err(|e| format!("{}: {e}", part.display()))?;
    let mut out = Hashed { file, hash: Sha256::new() };
    let got = get(&asset.url, "application/octet-stream", &mut out, progress)
        .and_then(|_| out.flush().map_err(|e| e.to_string()))
        .map(|_| hex(&out.hash.finalize()));
    let result = match got {
        Ok(h) if h == want => fs::rename(&part, dest).map_err(|e| format!("{}: {e}", dest.display())),
        Ok(h) => Err(format!("checksum mismatch: got {h}, expected {want}")),
        Err(e) => Err(e),
    };
    if result.is_err() {
        let _ = fs::remove_file(&part);
    }
    result
}

/// This build's version; TERMVISOR_PRETEND_VERSION overrides it so the
/// update path can be tried against the current release.
fn current() -> String {
    std::env::var("TERMVISOR_PRETEND_VERSION").unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string())
}

/// True when `r` is newer than this build.
pub fn is_newer(r: &Release) -> bool {
    newer(&r.version, &current())
}

/// Asks GitHub in the background; `done` runs on the GTK thread with the
/// latest release or the error.
pub fn latest(done: impl FnOnce(Result<Release, String>) + 'static) {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(fetch());
    });
    glib::spawn_future_local(async move {
        if let Ok(r) = rx.recv().await {
            done(r);
        }
    });
}

/// Checks in the background; `found` runs on the GTK thread with a release
/// newer than this build. Errors only go to the log.
pub fn check(found: impl FnOnce(Release) + 'static) {
    latest(move |r| match r {
        Ok(r) if is_newer(&r) => found(r),
        Ok(_) => {}
        Err(e) => crate::warn(&format!("update check: {e}")),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("v1.0.2", "1.0.1"));
        assert!(newer("1.1", "1.0.9"));
        assert!(newer("2.0.0", "1.99.99"));
        assert!(!newer("v1.0.1", "1.0.1"));
        assert!(!newer("1.0.0", "1.0.1"));
        assert!(!newer("1.0.2-rc1", "1.0.2"));
        assert!(!newer("garbage", "1.0.1"));
    }

    #[test]
    fn release_json() {
        let json = br#"{"tag_name":"v1.2.3","html_url":"https://x/y","assets":[
            {"name":"a_setup.exe","browser_download_url":"https://h/a","size":5,"digest":"sha256:ABcd"},
            {"name":"b.deb","browser_download_url":"https://h/b","size":7}]}"#;
        let r = parse(json).unwrap();
        assert_eq!((r.version.as_str(), r.url.as_str()), ("1.2.3", "https://x/y"));
        assert_eq!(r.assets.len(), 2);
        assert_eq!(r.assets[0].sha256.as_deref(), Some("abcd"));
        assert_eq!(r.assets[1].sha256, None);
    }

    #[test]
    fn reply_head() {
        let lines: Vec<String> = ["HTTP/1.1 302 Found", "Location: https://o/x?y=1", "content-length: 0"].map(String::from).into();
        assert_eq!(head(&lines).unwrap(), (302, Some("https://o/x?y=1".into()), Some(0)));
        assert!(head(&["garbage".to_string()]).is_err());
        assert_eq!(split_url("https://h.com/a/b?c").unwrap(), ("h.com", "/a/b?c"));
        assert!(split_url("http://h.com/").is_err());
    }

    /// Talks to GitHub; run with `cargo test -- --ignored update`.
    #[test]
    #[ignore]
    fn update_live() {
        let r = fetch().expect("fetch");
        assert!(parts(&r.version).is_some());
        assert!(r.url.starts_with("https://github.com/"));
        assert!(r.assets.iter().any(|a| a.sha256.is_some()));
    }

    /// Downloads the smallest release file through the redirect and checks
    /// its hash; run with `cargo test -- --ignored update_download`.
    #[test]
    #[ignore]
    fn update_download() {
        let r = fetch().expect("fetch");
        let a = r.assets.iter().filter(|a| a.sha256.is_some()).min_by_key(|a| a.size).expect("asset");
        let dir = std::env::temp_dir().join(format!("termvisor-dl-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let dest = dir.join(&a.name);
        let mut last = 0;
        download(a, &dest, &mut |got, _| last = got).expect("download");
        assert_eq!(fs::metadata(&dest).unwrap().len(), a.size);
        assert_eq!(last, a.size);
        let mut bad = a.clone();
        bad.sha256 = Some("00".into());
        assert!(download(&bad, &dir.join("bad"), &mut |_, _| {}).is_err());
        assert!(!dir.join("bad").exists() && !dir.join("bad.part").exists());
        fs::remove_dir_all(&dir).unwrap();
    }
}
