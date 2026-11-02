// HTTP file server for one folder: listings, downloads with Range, and
// uploads (form or PUT) when allowed. One thread per connection; paths stay
// inside the folder.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::tftp::inside;

/// Largest form upload kept in memory.
const FORM_MAX: usize = 512 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct Cfg {
    pub root: PathBuf,
    pub addr: SocketAddr,
    pub upload: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Log {
    Listening(SocketAddr),
    Request { peer: SocketAddr, method: String, path: String, status: u16, bytes: u64 },
}

pub struct Server {
    stop: Arc<AtomicBool>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

/// Decodes %XX and '+' is kept (it is a plus in paths).
pub fn unquote(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Some(v) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn quote(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn html(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn mime(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).map(|e| e.to_lowercase()).as_deref() {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("txt" | "log" | "md" | "conf" | "cfg" | "ini" | "sh" | "c" | "h" | "rs" | "py") => "text/plain; charset=utf-8",
        Some("json") => "application/json",
        Some("css") => "text/css",
        Some("js") => "text/javascript",
        Some("png") => "image/png",
        Some("jpg" | "jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("pdf") => "application/pdf",
        Some("gz" | "tgz") => "application/gzip",
        Some("zip") => "application/zip",
        _ => "application/octet-stream",
    }
}

fn human(n: u64) -> String {
    crate::tunnels::human(n)
}

struct Req {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl Req {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
}

fn read_request(r: &mut BufReader<TcpStream>) -> Option<Req> {
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let mut it = line.split_whitespace();
    let method = it.next()?.to_string();
    let target = it.next()?;
    let path = unquote(target.split('?').next().unwrap_or("/"));
    let mut headers = Vec::new();
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).ok()? == 0 {
            break;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            headers.push((k.trim().to_string(), v.trim().to_string()));
        }
        if headers.len() > 100 {
            return None;
        }
    }
    Some(Req { method, path, headers })
}

fn respond(w: &mut TcpStream, status: u16, reason: &str, kind: &str, extra: &[(&str, String)], body: &[u8], head: bool) -> io::Result<u64> {
    let mut h = format!("HTTP/1.1 {status} {reason}\r\nServer: termvisor\r\nConnection: close\r\nContent-Type: {kind}\r\nContent-Length: {}\r\n", body.len());
    for (k, v) in extra {
        h.push_str(&format!("{k}: {v}\r\n"));
    }
    h.push_str("\r\n");
    w.write_all(h.as_bytes())?;
    if !head {
        w.write_all(body)?;
    }
    Ok(if head { 0 } else { body.len() as u64 })
}

fn listing(dir: &Path, url: &str, upload: bool) -> String {
    let mut entries: Vec<(bool, String, u64)> = fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| {
                    let m = e.metadata().ok();
                    (m.as_ref().is_some_and(|m| m.is_dir()), e.file_name().to_string_lossy().into_owned(), m.map(|m| m.len()).unwrap_or(0))
                })
                .collect()
        })
        .unwrap_or_default();
    entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
    let mut rows = String::new();
    if url != "/" {
        rows.push_str("<tr><td><a href=\"../\">../</a></td><td></td></tr>\n");
    }
    for (dir, name, size) in entries {
        let shown = if dir { format!("{name}/") } else { name.clone() };
        let size = if dir { String::new() } else { human(size) };
        rows.push_str(&format!("<tr><td><a href=\"{}{}\">{}</a></td><td>{size}</td></tr>\n", quote(&name), if dir { "/" } else { "" }, html(&shown)));
    }
    let form = if upload {
        "<form method=\"post\" enctype=\"multipart/form-data\"><input type=\"file\" name=\"file\"> <button>Upload</button></form>"
    } else {
        ""
    };
    format!(
        "<!doctype html><meta charset=\"utf-8\"><title>{t}</title>\
         <style>body{{font:14px sans-serif;margin:2em}}td{{padding:2px 16px 2px 0}}</style>\
         <h2>{t}</h2>{form}<table>\n{rows}</table>",
        t = html(url)
    )
}

/// Byte range from a Range header for a file of `size` bytes.
fn range(h: Option<&str>, size: u64) -> Option<(u64, u64)> {
    let spec = h?.strip_prefix("bytes=")?;
    let (a, b) = spec.split_once('-')?;
    if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        return (n > 0 && size > 0).then(|| (size.saturating_sub(n), size - 1));
    }
    let start: u64 = a.parse().ok()?;
    let end = if b.is_empty() { size.checked_sub(1)? } else { b.parse::<u64>().ok()?.min(size.checked_sub(1)?) };
    (start <= end).then_some((start, end))
}

/// First file of a multipart/form-data body: (name, bytes).
fn multipart(body: &[u8], boundary: &str) -> Option<(String, Vec<u8>)> {
    let sep = format!("--{boundary}");
    let start = find(body, sep.as_bytes())? + sep.len();
    let head_end = find(&body[start..], b"\r\n\r\n")? + start;
    let head = String::from_utf8_lossy(&body[start..head_end]);
    let name = head.split("filename=\"").nth(1)?.split('"').next()?.to_string();
    let data_start = head_end + 4;
    let end = find(&body[data_start..], format!("\r\n{sep}").as_bytes())? + data_start;
    // Browsers may send a path; keep the last part.
    let name = name.rsplit(['/', '\\']).next().unwrap_or("").to_string();
    (!name.is_empty()).then(|| (name, body[data_start..end].to_vec()))
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn handle(cfg: &Cfg, s: TcpStream, peer: SocketAddr, log: &async_channel::Sender<Log>) -> io::Result<()> {
    s.set_read_timeout(Some(Duration::from_secs(30)))?;
    let mut w = s.try_clone()?;
    let mut r = BufReader::new(s);
    let Some(req) = read_request(&mut r) else { return Ok(()) };
    let head = req.method == "HEAD";
    let mut status = 200;
    let note = |status: u16, bytes: u64| {
        let _ = log.send_blocking(Log::Request { peer, method: req.method.clone(), path: req.path.clone(), status, bytes });
    };
    let Some(path) = inside(&cfg.root, &req.path) else {
        let n = respond(&mut w, 403, "Forbidden", "text/plain", &[], b"outside the shared folder\n", head)?;
        note(403, n);
        return Ok(());
    };
    match req.method.as_str() {
        "GET" | "HEAD" => {
            if path.is_dir() {
                if !req.path.ends_with('/') {
                    let n = respond(&mut w, 301, "Moved Permanently", "text/plain", &[("Location", format!("{}/", quote(&req.path)))], b"", head)?;
                    note(301, n);
                    return Ok(());
                }
                let body = listing(&path, &req.path, cfg.upload);
                let n = respond(&mut w, 200, "OK", "text/html; charset=utf-8", &[], body.as_bytes(), head)?;
                note(200, n);
                return Ok(());
            }
            let Ok(mut f) = File::open(&path) else {
                let n = respond(&mut w, 404, "Not Found", "text/plain", &[], b"not found\n", head)?;
                note(404, n);
                return Ok(());
            };
            let size = f.metadata()?.len();
            let (start, end) = match range(req.header("Range"), size) {
                Some(r) => {
                    status = 206;
                    r
                }
                None => (0, size.saturating_sub(1)),
            };
            let len = if size == 0 { 0 } else { end - start + 1 };
            let mut h = format!(
                "HTTP/1.1 {status} {}\r\nServer: termvisor\r\nConnection: close\r\nContent-Type: {}\r\nContent-Length: {len}\r\nAccept-Ranges: bytes\r\n",
                if status == 206 { "Partial Content" } else { "OK" },
                mime(&path)
            );
            if status == 206 {
                h.push_str(&format!("Content-Range: bytes {start}-{end}/{size}\r\n"));
            }
            h.push_str("\r\n");
            w.write_all(h.as_bytes())?;
            let mut sent = 0;
            if !head && len > 0 {
                io::Seek::seek(&mut f, io::SeekFrom::Start(start))?;
                sent = io::copy(&mut f.take(len), &mut w)?;
            }
            note(status, sent);
        }
        "PUT" | "POST" if !cfg.upload => {
            let n = respond(&mut w, 403, "Forbidden", "text/plain", &[], b"uploads are off\n", false)?;
            note(403, n);
        }
        "PUT" => {
            let len: u64 = req.header("Content-Length").and_then(|v| v.parse().ok()).unwrap_or(0);
            let target = if req.path.ends_with('/') { None } else { Some(path) };
            let Some(target) = target else {
                let n = respond(&mut w, 400, "Bad Request", "text/plain", &[], b"PUT needs a file name\n", false)?;
                note(400, n);
                return Ok(());
            };
            if let Some(d) = target.parent() {
                fs::create_dir_all(d)?;
            }
            let mut f = OpenOptions::new().create(true).truncate(true).write(true).open(&target)?;
            let got = io::copy(&mut r.by_ref().take(len), &mut f)?;
            let n = respond(&mut w, 201, "Created", "text/plain", &[], format!("stored {got} bytes\n").as_bytes(), false)?;
            note(201, got.max(n));
        }
        "POST" => {
            let len: usize = req.header("Content-Length").and_then(|v| v.parse().ok()).unwrap_or(0);
            let boundary = req.header("Content-Type").and_then(|t| t.split("boundary=").nth(1)).map(|b| b.trim_matches('"').to_string());
            let (Some(boundary), true) = (boundary, len <= FORM_MAX && path.is_dir()) else {
                let n = respond(&mut w, 400, "Bad Request", "text/plain", &[], b"expected a form upload to a folder\n", false)?;
                note(400, n);
                return Ok(());
            };
            let mut body = vec![0u8; len];
            r.read_exact(&mut body)?;
            let Some((name, data)) = multipart(&body, &boundary) else {
                let n = respond(&mut w, 400, "Bad Request", "text/plain", &[], b"no file in the form\n", false)?;
                note(400, n);
                return Ok(());
            };
            let Some(target) = inside(&path, &name) else {
                let n = respond(&mut w, 403, "Forbidden", "text/plain", &[], b"bad file name\n", false)?;
                note(403, n);
                return Ok(());
            };
            fs::write(&target, &data)?;
            let n = respond(&mut w, 303, "See Other", "text/plain", &[("Location", quote(&req.path))], b"", false)?;
            note(303, n.max(data.len() as u64));
        }
        _ => {
            let n = respond(&mut w, 405, "Method Not Allowed", "text/plain", &[], b"method not allowed\n", false)?;
            note(405, n);
        }
    }
    Ok(())
}

pub fn serve(cfg: Cfg, log: async_channel::Sender<Log>) -> io::Result<(Server, SocketAddr)> {
    let l = TcpListener::bind(cfg.addr)?;
    l.set_nonblocking(true)?;
    let addr = l.local_addr()?;
    let stop = Arc::new(AtomicBool::new(false));
    let s = stop.clone();
    let _ = log.send_blocking(Log::Listening(addr));
    thread::spawn(move || {
        while !s.load(Ordering::Acquire) {
            match l.accept() {
                Ok((c, peer)) => {
                    let _ = c.set_nonblocking(false);
                    let (cfg, log) = (cfg.clone(), log.clone());
                    thread::spawn(move || {
                        let _ = handle(&cfg, c, peer, &log);
                    });
                }
                Err(_) => thread::sleep(Duration::from_millis(100)),
            }
        }
    });
    Ok((Server { stop }, addr))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(addr: SocketAddr, req: &[u8]) -> (u16, String, Vec<u8>) {
        let mut s = TcpStream::connect(addr).expect("connect");
        s.write_all(req).expect("send");
        let mut out = Vec::new();
        s.read_to_end(&mut out).expect("read");
        let split = find(&out, b"\r\n\r\n").expect("head end");
        let head = String::from_utf8_lossy(&out[..split]).into_owned();
        let status = head.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
        (status, head, out[split + 4..].to_vec())
    }

    #[test]
    fn helpers() {
        assert_eq!(unquote("/my%20dir/a%2Bb.txt"), "/my dir/a+b.txt");
        assert_eq!(unquote("%zz%2"), "%zz%2");
        assert_eq!(quote("a b/ç"), "a%20b/%C3%A7");
        assert_eq!(range(Some("bytes=100-"), 1000), Some((100, 999)));
        assert_eq!(range(Some("bytes=0-9"), 1000), Some((0, 9)));
        assert_eq!(range(Some("bytes=-10"), 1000), Some((990, 999)));
        assert_eq!(range(Some("bytes=2000-"), 1000), None);
    }

    #[test]
    fn serve_files() {
        let root = std::env::temp_dir().join(format!("tv-http-{}", std::process::id()));
        fs::create_dir_all(root.join("my dir")).expect("dir");
        let data: Vec<u8> = (0..5000u32).map(|i| i as u8).collect();
        fs::write(root.join("my dir/fw.bin"), &data).expect("w");
        let (tx, _rx) = async_channel::unbounded();
        let cfg = Cfg { root: root.clone(), addr: "127.0.0.1:0".parse().expect("addr"), upload: false };
        let (srv, addr) = serve(cfg, tx).expect("serve");

        let (st, _, body) = call(addr, b"GET / HTTP/1.1\r\nHost: x\r\n\r\n");
        assert_eq!(st, 200);
        assert!(String::from_utf8_lossy(&body).contains("href=\"my%20dir/\""));
        let (st, head, _) = call(addr, b"GET /my%20dir HTTP/1.1\r\n\r\n");
        assert_eq!(st, 301);
        assert!(head.contains("Location: /my%20dir/"));
        let (st, _, body) = call(addr, b"GET /my%20dir/fw.bin HTTP/1.1\r\n\r\n");
        assert_eq!((st, body.as_slice()), (200, data.as_slice()));
        let (st, head, body) = call(addr, b"GET /my%20dir/fw.bin HTTP/1.1\r\nRange: bytes=4000-\r\n\r\n");
        assert_eq!((st, body.as_slice()), (206, &data[4000..]));
        assert!(head.contains("Content-Range: bytes 4000-4999/5000"));
        assert_eq!(call(addr, b"GET /../../etc/passwd HTTP/1.1\r\n\r\n").0, 403);
        assert_eq!(call(addr, b"GET /%2e%2e/%2e%2e/etc/passwd HTTP/1.1\r\n\r\n").0, 403);
        assert_eq!(call(addr, b"PUT /x.bin HTTP/1.1\r\nContent-Length: 3\r\n\r\nabc").0, 403);
        drop(srv);

        let (tx, _rx) = async_channel::unbounded();
        let cfg = Cfg { root: root.clone(), addr: "127.0.0.1:0".parse().expect("addr"), upload: true };
        let (_srv, addr) = serve(cfg, tx).expect("serve");
        assert_eq!(call(addr, b"PUT /logs/x.bin HTTP/1.1\r\nContent-Length: 3\r\n\r\nabc").0, 201);
        assert_eq!(fs::read(root.join("logs/x.bin")).expect("read"), b"abc");
        let form = b"--XYZ\r\nContent-Disposition: form-data; name=\"file\"; filename=\"C:\\\\up.txt\"\r\nContent-Type: text/plain\r\n\r\nhello\r\n--XYZ--\r\n";
        let mut req = format!("POST /my%20dir/ HTTP/1.1\r\nContent-Type: multipart/form-data; boundary=XYZ\r\nContent-Length: {}\r\n\r\n", form.len()).into_bytes();
        req.extend_from_slice(form);
        assert_eq!(call(addr, &req).0, 303);
        assert_eq!(fs::read(root.join("my dir/up.txt")).expect("read"), b"hello");
        let _ = fs::remove_dir_all(&root);
    }
}
