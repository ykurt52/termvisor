// Session tree data and quick connect parsing.

use std::path::Path;

use gtk::glib;

use crate::ftp::{FtpCfg, Tls};
use crate::recent::Recent;
use crate::store::{self, Entry};
use crate::term::{Kind, Launch};

/// Row in the session tree.
#[derive(Clone)]
pub struct Node {
    pub name: String,
    pub icon: &'static str,
    /// CSS tint class.
    pub tint: &'static str,
    pub star: bool,
    /// Index path in the store; None for built-in rows.
    pub path: Option<Vec<usize>>,
    /// Host alias for ~/.ssh/config rows (can be copied into the store).
    pub host: Option<String>,
    pub kind: NodeKind,
}

#[derive(Clone)]
// Session trees are small; boxing every item is not worth it.
#[allow(clippy::large_enum_variant)]
pub enum NodeKind {
    Folder(Vec<Node>),
    Item(Result<Launch, String>),
}

impl Node {
    fn item(name: &str, icon: &'static str, tint: &'static str, launch: Result<Launch, String>) -> Self {
        Self { name: name.into(), icon, tint, star: false, path: None, host: None, kind: NodeKind::Item(launch) }
    }

    fn folder(name: &str, icon: &'static str, tint: &'static str, kids: Vec<Node>) -> Self {
        Self { name: name.into(), icon, tint, star: false, path: None, host: None, kind: NodeKind::Folder(kids) }
    }

    /// Copy keeping only items whose name contains `query` (lowercase).
    fn filter(&self, query: &str) -> Option<Node> {
        match &self.kind {
            NodeKind::Item(_) => self.name.to_lowercase().contains(query).then(|| self.clone()),
            NodeKind::Folder(kids) => {
                let kids: Vec<Node> = kids.iter().filter_map(|k| k.filter(query)).collect();
                (!kids.is_empty()).then(|| Node { kind: NodeKind::Folder(kids), ..self.clone() })
            }
        }
    }
}

/// Tint class for a stored color name, or `fallback`.
fn tint(color: Option<&str>, fallback: &'static str) -> &'static str {
    match color {
        Some("blue") => "tint-blue",
        Some("green") => "tint-green",
        Some("yellow") => "tint-yellow",
        Some("red") => "tint-red",
        Some("magenta") => "tint-magenta",
        Some("cyan") => "tint-cyan",
        _ => fallback,
    }
}

fn stored(root: &[Entry], list: &[Entry], prefix: &[usize]) -> Vec<Node> {
    list.iter()
        .enumerate()
        .map(|(i, e)| {
            let mut path = prefix.to_vec();
            path.push(i);
            let (icon, fallback, kind) = match e {
                Entry::Folder(f) => (
                    "folder-symbolic",
                    "tint-yellow",
                    NodeKind::Folder(stored(root, &f.children, &path)),
                ),
                Entry::Ssh(_) => ("network-server-symbolic", "tint-green", NodeKind::Item(store::resolve(root, &path))),
                Entry::Shell(_) => ("utilities-terminal-symbolic", "tint-blue", NodeKind::Item(store::resolve(root, &path))),
                Entry::Serial(_) => ("media-removable-symbolic", "tint-yellow", NodeKind::Item(store::resolve(root, &path))),
                Entry::Tcp(_) => ("network-transmit-receive-symbolic", "tint-magenta", NodeKind::Item(store::resolve(root, &path))),
                Entry::Telnet(_) => ("network-wired-symbolic", "tint-red", NodeKind::Item(store::resolve(root, &path))),
                Entry::Mosh(_) => ("network-wireless-symbolic", "tint-cyan", NodeKind::Item(store::resolve(root, &path))),
                Entry::Ftp(_) => ("folder-remote-symbolic", "tint-blue", NodeKind::Item(store::resolve(root, &path))),
            };
            Node {
                name: e.name().to_string(),
                icon,
                tint: tint(e.color(), fallback),
                star: e.favorite(),
                path: Some(path),
                host: None,
                kind,
            }
        })
        .collect()
}

/// Host aliases from an ssh config file. Wildcard patterns are skipped.
pub fn ssh_hosts(path: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut hosts = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        let Some((key, rest)) = line.split_once(|c: char| c.is_whitespace() || c == '=') else {
            continue;
        };
        if !key.eq_ignore_ascii_case("host") {
            continue;
        }
        for h in rest.split_whitespace() {
            let h = h.trim_matches('"');
            if h.is_empty() || h.contains(['*', '?', '!']) || hosts.iter().any(|x| x == h) {
                continue;
            }
            hosts.push(h.to_string());
        }
    }
    hosts
}

fn kind_look(k: Kind) -> (&'static str, &'static str) {
    match k {
        Kind::Shell => ("utilities-terminal-symbolic", "tint-blue"),
        Kind::Ssh => ("network-server-symbolic", "tint-green"),
        Kind::Serial => ("media-removable-symbolic", "tint-yellow"),
        Kind::Tcp => ("network-transmit-receive-symbolic", "tint-magenta"),
        Kind::Telnet => ("network-wired-symbolic", "tint-red"),
        Kind::Mosh => ("network-wireless-symbolic", "tint-cyan"),
        Kind::Ftp => ("folder-remote-symbolic", "tint-blue"),
        Kind::Adb => ("phone-symbolic", "tint-green"),
        Kind::Logcat => ("view-list-symbolic", "tint-yellow"),
        Kind::Usb => ("drive-removable-media-symbolic", "tint-magenta"),
        Kind::Tool => ("applications-utilities-symbolic", "tint-blue"),
    }
}

/// Local shell, stored sessions, then ~/.ssh/config hosts.
pub fn base(root: &[Entry]) -> Vec<Node> {
    let mut nodes = vec![Node::item("Local Shell", "utilities-terminal-symbolic", "tint-blue", Ok(Launch::shell(None)))];
    nodes.extend(stored(root, root, &[]));

    let ssh: Vec<Node> = ssh_hosts(&glib::home_dir().join(".ssh/config"))
        .into_iter()
        .map(|h| Node {
            host: Some(h.clone()),
            ..Node::item(&h, "network-server-symbolic", "tint-green", Ok(Launch::ssh(vec![h.clone()], &h)))
        })
        .collect();
    if !ssh.is_empty() {
        nodes.push(Node::folder("SSH Config", "folder-symbolic", "tint-cyan", ssh));
    }
    nodes
}

fn starred(nodes: &[Node], out: &mut Vec<Node>) {
    for n in nodes {
        match &n.kind {
            NodeKind::Folder(kids) => starred(kids, out),
            NodeKind::Item(_) if n.star => out.push(n.clone()),
            NodeKind::Item(_) => {}
        }
    }
}

/// Full tree: Favorites and Recent sections on top of `base`.
pub fn tree(root: &[Entry], recent: &[Recent]) -> Vec<Node> {
    let base = base(root);
    let mut out = Vec::new();

    let mut favs = Vec::new();
    starred(&base, &mut favs);
    if !favs.is_empty() {
        out.push(Node::folder("Favorites", "starred-symbolic", "tint-yellow", favs));
    }
    if !recent.is_empty() {
        let items = recent
            .iter()
            .map(|r| {
                let l = r.launch();
                let (icon, tint) = kind_look(l.kind);
                Node::item(&r.title, icon, tint, Ok(l))
            })
            .collect();
        out.push(Node::folder("Recent", "document-open-recent-symbolic", "tint-magenta", items));
    }
    out.extend(base);
    out
}

/// Every launchable row with its folder path as label, e.g. "Lab / board".
pub fn flat(nodes: &[Node]) -> Vec<(String, Node)> {
    fn walk(nodes: &[Node], prefix: &str, out: &mut Vec<(String, Node)>) {
        for n in nodes {
            let label = if prefix.is_empty() { n.name.clone() } else { format!("{prefix} / {}", n.name) };
            match &n.kind {
                NodeKind::Folder(kids) => walk(kids, &label, out),
                NodeKind::Item(_) => out.push((label, n.clone())),
            }
        }
    }
    let mut out = Vec::new();
    walk(nodes, "", &mut out);
    out
}

/// Fuzzy subsequence score; None when `query` does not match `text`.
/// Consecutive hits and word starts score higher, long texts slightly lower.
pub fn fuzzy(query: &str, text: &str) -> Option<i32> {
    let q: Vec<char> = query.to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let mut score = 0i32;
    let mut ti = 0usize;
    let mut prev: Option<usize> = None;
    for qc in q {
        while t.get(ti)? != &qc {
            ti += 1;
        }
        score += 1;
        if prev.is_some_and(|p| p + 1 == ti) {
            score += 5;
        }
        if ti == 0 || !t[ti - 1].is_alphanumeric() {
            score += 8;
        }
        prev = Some(ti);
        ti += 1;
    }
    Some(score - (t.len() / 10) as i32)
}

pub fn filter(nodes: &[Node], query: &str) -> Vec<Node> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return nodes.to_vec();
    }
    nodes.iter().filter_map(|n| n.filter(&q)).collect()
}

#[derive(Debug, PartialEq)]
pub enum Quick {
    Ssh { args: Vec<String>, target: String },
    Serial { device: String, baud: Option<u32> },
    Net { host: String, port: u16, telnet: bool },
    Mosh(String),
    Ftp(FtpCfg),
    /// adb://<serial>; empty serial: the only device.
    Adb(String),
}

/// "host[:port]" or "[v6]:port"; `default` when no port is given.
fn host_port(s: &str, default: Option<u16>) -> Option<(String, u16)> {
    if let Some(rest) = s.strip_prefix('[') {
        let (h, tail) = rest.split_once(']')?;
        let port = match tail.strip_prefix(':') {
            Some(p) => p.parse().ok()?,
            None if tail.is_empty() => default?,
            None => return None,
        };
        return Some((h.to_string(), port));
    }
    match s.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => Some((h.to_string(), p.parse().ok().filter(|p| *p > 0)?)),
        _ => Some((s.to_string(), default?)),
    }
}

/// Parses quick connect input: `[user@]host[:port]`, `ssh <args>`,
/// `/dev/<port>[@baud]`, `telnet://host[:port]`, `tcp://host:port`,
/// `mosh [user@]host`.
pub fn quick(input: &str) -> Option<Quick> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    if s.starts_with("/dev/") {
        let (device, baud) = match s.split_once('@') {
            Some((d, b)) => (d, Some(b.parse().ok().filter(|b| *b > 0)?)),
            None => (s, None),
        };
        return Some(Quick::Serial { device: device.to_string(), baud });
    }
    for (scheme, telnet, default) in [("telnet://", true, Some(23)), ("tcp://", false, None)] {
        if let Some(rest) = s.strip_prefix(scheme) {
            let (host, port) = host_port(rest.trim_end_matches('/'), default)?;
            if host.is_empty() || host.contains(char::is_whitespace) {
                return None;
            }
            return Some(Quick::Net { host, port, telnet });
        }
    }
    for (scheme, tls) in [("ftp://", Tls::None), ("ftps://", Tls::Explicit)] {
        if let Some(rest) = s.strip_prefix(scheme) {
            let rest = rest.trim_end_matches('/');
            let (user, hostport) = match rest.rsplit_once('@') {
                Some((u, h)) => (u.to_string(), h),
                None => (String::new(), rest),
            };
            let (host, port) = host_port(hostport, Some(21))?;
            let cfg = FtpCfg { host, port, user, tls, ..Default::default() };
            return cfg.check().ok().map(|_| Quick::Ftp(cfg));
        }
    }
    if let Some(rest) = s.strip_prefix("adb://") {
        let serial = rest.trim_end_matches('/');
        if serial.contains(char::is_whitespace) {
            return None;
        }
        return Some(Quick::Adb(serial.to_string()));
    }
    if let Some(rest) = s.strip_prefix("mosh ") {
        let target = rest.trim();
        if target.is_empty() || target.starts_with('-') || target.contains(char::is_whitespace) {
            return None;
        }
        return Some(Quick::Mosh(target.to_string()));
    }
    if let Some(rest) = s.strip_prefix("ssh ") {
        let args: Vec<String> = rest.split_whitespace().map(str::to_string).collect();
        let target = args.last()?.clone();
        return Some(Quick::Ssh { args, target });
    }
    if s.starts_with('-') || s.contains(char::is_whitespace) {
        return None;
    }
    if let Some((host, port)) = s.rsplit_once(':') {
        if !host.is_empty() && !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()) {
            return Some(Quick::Ssh {
                args: vec!["-p".into(), port.into(), host.into()],
                target: s.to_string(),
            });
        }
    }
    Some(Quick::Ssh { args: vec![s.to_string()], target: s.to_string() })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fuzzy_ranks() {
        assert!(fuzzy("tb", "T153 board").is_some());
        assert!(fuzzy("xyz", "T153 board").is_none());
        assert_eq!(fuzzy("", "anything").map(|s| s >= -1), Some(true));
        let word = fuzzy("lab", "Lab / board").unwrap_or(i32::MIN);
        let scattered = fuzzy("lab", "local shell abc").unwrap_or(i32::MIN);
        assert!(word > scattered);
    }

    #[test]
    fn quick_adb() {
        assert!(matches!(quick("adb://R58M123"), Some(Quick::Adb(s)) if s == "R58M123"));
        assert!(matches!(quick("adb://"), Some(Quick::Adb(s)) if s.is_empty()));
        assert!(quick("adb://a b").is_none());
    }

    #[test]
    fn quick_forms() {
        assert_eq!(
            quick("root@10.0.0.2:2222"),
            Some(Quick::Ssh {
                args: vec!["-p".into(), "2222".into(), "root@10.0.0.2".into()],
                target: "root@10.0.0.2:2222".into()
            })
        );
        assert_eq!(
            quick("board"),
            Some(Quick::Ssh { args: vec!["board".into()], target: "board".into() })
        );
        assert_eq!(quick("/dev/ttyUSB0"), Some(Quick::Serial { device: "/dev/ttyUSB0".into(), baud: None }));
        assert_eq!(
            quick("/dev/ttyS4@921600"),
            Some(Quick::Serial { device: "/dev/ttyS4".into(), baud: Some(921_600) })
        );
        assert_eq!(quick("/dev/ttyS4@fast"), None);
        assert_eq!(
            quick("telnet://router"),
            Some(Quick::Net { host: "router".into(), port: 23, telnet: true })
        );
        assert_eq!(
            quick("tcp://127.0.0.1:5760"),
            Some(Quick::Net { host: "127.0.0.1".into(), port: 5760, telnet: false })
        );
        assert_eq!(
            quick("tcp://[::1]:14550"),
            Some(Quick::Net { host: "::1".into(), port: 14550, telnet: false })
        );
        assert_eq!(quick("tcp://host"), None);
        assert_eq!(quick("mosh yk@box"), Some(Quick::Mosh("yk@box".into())));
        assert_eq!(quick("mosh -x"), None);
        let Some(Quick::Ftp(c)) = quick("ftps://yk@nas:2121") else { panic!("ftp") };
        assert_eq!((c.user.as_str(), c.host.as_str(), c.port, c.tls), ("yk", "nas", 2121, Tls::Explicit));
        let Some(Quick::Ftp(c)) = quick("ftp://10.0.0.5") else { panic!("ftp") };
        assert_eq!((c.user.as_str(), c.port, c.tls), ("", 21, Tls::None));
        assert_eq!(quick("-oProxyCommand=x"), None);
        assert_eq!(quick("   "), None);
    }

    #[test]
    fn ssh_config_hosts() {
        let dir = std::env::temp_dir().join(format!("tv-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("config");
        std::fs::write(&path, "Host a b\n  User x\nHost *\nhost=c\nHost a\n").expect("write");
        assert_eq!(ssh_hosts(&path), vec!["a", "b", "c"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
