// Session tree data and quick connect parsing.

use std::path::Path;

use gtk::glib;

use crate::term::Launch;

#[derive(Clone)]
pub enum Node {
    Folder { name: String, icon: &'static str, kids: Vec<Node> },
    Item { name: String, icon: &'static str, launch: Launch },
}

impl Node {
    pub fn name(&self) -> &str {
        match self {
            Node::Folder { name, .. } | Node::Item { name, .. } => name,
        }
    }

    pub fn icon(&self) -> &'static str {
        match self {
            Node::Folder { icon, .. } | Node::Item { icon, .. } => icon,
        }
    }

    /// Copy keeping only items whose name contains `query` (lowercase).
    fn filter(&self, query: &str) -> Option<Node> {
        match self {
            Node::Item { name, .. } => name.to_lowercase().contains(query).then(|| self.clone()),
            Node::Folder { name, icon, kids } => {
                let kids: Vec<Node> = kids.iter().filter_map(|k| k.filter(query)).collect();
                (!kids.is_empty()).then(|| Node::Folder { name: name.clone(), icon, kids })
            }
        }
    }
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

/// Current session tree.
pub fn tree() -> Vec<Node> {
    let local = Node::Item {
        name: "Local Shell".into(),
        icon: "utilities-terminal-symbolic",
        launch: Launch::shell(None),
    };
    let ssh: Vec<Node> = ssh_hosts(&glib::home_dir().join(".ssh/config"))
        .into_iter()
        .map(|h| Node::Item {
            launch: Launch::ssh(vec![h.clone()], &h),
            name: h,
            icon: "network-server-symbolic",
        })
        .collect();

    let mut nodes = vec![local];
    if !ssh.is_empty() {
        nodes.push(Node::Folder {
            name: "SSH Config".into(),
            icon: "folder-symbolic",
            kids: ssh,
        });
    }
    nodes
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
    Serial(String),
}

/// Parses quick connect input: `[user@]host[:port]`, `ssh <args>` or `/dev/...`.
pub fn quick(input: &str) -> Option<Quick> {
    let s = input.trim();
    if s.is_empty() {
        return None;
    }
    if s.starts_with("/dev/") {
        return Some(Quick::Serial(s.to_string()));
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
        assert_eq!(quick("/dev/ttyUSB0"), Some(Quick::Serial("/dev/ttyUSB0".into())));
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
