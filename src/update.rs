// Update check: asks GitHub for the latest release and tells the caller
// when it is newer than this build. At most once a day, silent on errors.

use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use gtk::glib;

const HOST: &str = "api.github.com";
const PATH: &str = "/repos/ykurt52/termvisor/releases/latest";
const DAY: i64 = 24 * 60 * 60;

pub struct Release {
    /// Version without the leading "v", e.g. "1.0.2".
    pub version: String,
    /// Release page.
    pub url: String,
}

/// Seconds since the Unix epoch.
pub fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// True when the last check was a day or more ago (or in the future).
pub fn due(last: i64, now: i64) -> bool {
    now - last >= DAY || now < last
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

/// Body of a 200 response; anything else is an error.
fn body(response: &[u8]) -> Result<&[u8], String> {
    let end = response.windows(4).position(|w| w == b"\r\n\r\n").ok_or("no header end")?;
    let status = response[..end].split(|&b| b == b'\n').next().unwrap_or_default();
    let status = String::from_utf8_lossy(status);
    if status.split_whitespace().nth(1) != Some("200") {
        return Err(format!("status: {}", status.trim()));
    }
    Ok(&response[end + 4..])
}

fn parse(json: &[u8]) -> Result<Release, String> {
    let v: serde_json::Value = serde_json::from_slice(json).map_err(|e| e.to_string())?;
    let tag = v["tag_name"].as_str().ok_or("no tag_name")?;
    let url = v["html_url"].as_str().ok_or("no html_url")?;
    Ok(Release { version: tag.trim_start_matches('v').to_string(), url: url.to_string() })
}

/// Latest release from GitHub. Blocks for up to about 15 seconds.
fn fetch() -> Result<Release, String> {
    let addr = (HOST, 443).to_socket_addrs().map_err(|e| e.to_string())?.next().ok_or("no address")?;
    let tcp = TcpStream::connect_timeout(&addr, Duration::from_secs(5)).map_err(|e| e.to_string())?;
    tcp.set_read_timeout(Some(Duration::from_secs(10))).map_err(|e| e.to_string())?;
    let tls = native_tls::TlsConnector::new().map_err(|e| e.to_string())?;
    let mut s = tls.connect(HOST, tcp).map_err(|e| e.to_string())?;
    // HTTP/1.0 so the reply is never chunked.
    let req = format!(
        "GET {PATH} HTTP/1.0\r\nHost: {HOST}\r\nUser-Agent: termvisor/{}\r\nAccept: application/vnd.github+json\r\n\r\n",
        env!("CARGO_PKG_VERSION")
    );
    s.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    let mut response = Vec::new();
    s.take(1 << 20).read_to_end(&mut response).map_err(|e| e.to_string())?;
    parse(body(&response)?)
}

/// Checks in the background; `found` runs on the GTK thread with a release
/// newer than this build. `checked` runs after any answer from GitHub.
pub fn check(checked: impl FnOnce() + 'static, found: impl FnOnce(Release) + 'static) {
    let (tx, rx) = async_channel::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send_blocking(fetch());
    });
    glib::spawn_future_local(async move {
        match rx.recv().await {
            Ok(Ok(r)) => {
                checked();
                if newer(&r.version, env!("CARGO_PKG_VERSION")) {
                    found(r);
                }
            }
            Ok(Err(e)) => crate::warn(&format!("update check: {e}")),
            Err(_) => {}
        }
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
    fn schedule() {
        assert!(due(0, DAY));
        assert!(!due(100, 100 + DAY - 1));
        assert!(due(100 + DAY, 100));
    }

    #[test]
    fn response() {
        let ok = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{\"tag_name\":\"v1.2.3\",\"html_url\":\"https://x/y\"}";
        let r = parse(body(ok).unwrap()).unwrap();
        assert_eq!((r.version.as_str(), r.url.as_str()), ("1.2.3", "https://x/y"));
        assert!(body(b"HTTP/1.1 403 Forbidden\r\n\r\n{}").is_err());
        assert!(body(b"HTTP/1.1 200 OK").is_err());
    }

    /// Talks to GitHub; run with `cargo test -- --ignored update`.
    #[test]
    #[ignore]
    fn update_live() {
        let r = fetch().expect("fetch");
        assert!(parts(&r.version).is_some());
        assert!(r.url.starts_with("https://github.com/"));
    }
}
