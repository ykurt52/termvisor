// mosh without its Perl script, for Windows where only mosh-client is
// bundled: ssh starts mosh-server on the host, its "MOSH CONNECT port key"
// line is read here, and mosh-client then runs in the pane with the key.

use std::io::{BufRead, BufReader, Read};
use std::net::{IpAddr, ToSocketAddrs};
use std::path::PathBuf;
use std::process::Stdio;
use std::thread;

/// Set to "builtin" to use this on other systems too (for testing).
pub const ENV: &str = "TERMVISOR_MOSH";

pub fn builtin() -> bool {
    cfg!(windows) || std::env::var(ENV).is_ok_and(|v| v == "builtin")
}

/// Run on the host through ssh. SSH_CONNECTION gives the server address
/// when the host name does not resolve here (an ssh config alias).
const REMOTE: &str = "sh -c 'printf \"MOSH SSH_CONNECTION %s\\n\" \"$SSH_CONNECTION\"; exec mosh-server new -c 256 -s -l LANG=C.UTF-8'";

/// What mosh-client needs.
#[derive(Debug, PartialEq)]
pub struct Connect {
    pub ip: IpAddr,
    pub port: u16,
    pub key: String,
}

impl Connect {
    /// mosh-client command line and its environment.
    pub fn command(&self) -> (Vec<String>, Vec<(String, String)>) {
        let argv = vec![client().to_string_lossy().into_owned(), self.ip.to_string(), self.port.to_string()];
        let env = vec![("MOSH_KEY".to_string(), self.key.clone()), ("LANG".into(), "C.UTF-8".into())];
        (argv, env)
    }
}

/// mosh-client next to termvisor.exe (with its MSYS runtime), else on PATH.
pub fn client() -> PathBuf {
    let bundled = std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("mosh").join("usr").join("bin").join("mosh-client.exe")))
        .filter(|p| p.is_file());
    bundled.unwrap_or_else(|| PathBuf::from("mosh-client"))
}

/// ssh command and host from `mosh --ssh="ssh <opts>" -- host`.
pub fn split(argv: &[String]) -> Option<(Vec<String>, String)> {
    let mut ssh = vec!["ssh".to_string()];
    let mut host = None;
    let mut it = argv.iter().skip(1);
    while let Some(a) = it.next() {
        if let Some(cmd) = a.strip_prefix("--ssh=") {
            ssh = cmd.split_whitespace().map(str::to_string).collect();
        } else if a == "--" {
            host = it.next().cloned();
            break;
        } else if !a.starts_with('-') {
            host = Some(a.clone());
            break;
        }
    }
    let host = host.filter(|h| !h.is_empty())?;
    (!ssh.is_empty()).then_some((ssh, host))
}

/// Port, key and the server address seen by sshd, from mosh-server's output.
pub fn parse(out: &str) -> Option<(u16, String, Option<IpAddr>)> {
    let mut seen = None;
    for line in out.lines().map(str::trim) {
        if let Some(rest) = line.strip_prefix("MOSH SSH_CONNECTION ") {
            seen = rest.split_whitespace().nth(2).and_then(|s| s.parse().ok());
        } else if let Some(rest) = line.strip_prefix("MOSH CONNECT ") {
            let mut f = rest.split_whitespace();
            let port = f.next()?.parse().ok()?;
            let key = f.next()?.to_string();
            return Some((port, key, seen));
        }
    }
    None
}

/// The host's address as resolved here, preferring the family sshd saw.
fn resolve(host: &str, seen: Option<IpAddr>) -> Option<IpAddr> {
    let name = host.rsplit('@').next().unwrap_or(host);
    let name = name.trim_start_matches('[').trim_end_matches(']');
    let all: Vec<IpAddr> = (name, 0).to_socket_addrs().ok()?.map(|a| a.ip()).collect();
    let same = all.iter().find(|ip| seen.is_some_and(|s| s.is_ipv4() == ip.is_ipv4()));
    same.or(all.first()).copied()
}

/// Starts mosh-server through ssh. Blocks until ssh ends; `env` is for ssh
/// (askpass). `note` gets ssh's messages for the pane.
pub fn connect(argv: &[String], env: &[(String, String)], note: impl Fn(&str)) -> Result<Connect, String> {
    let (ssh, host) = split(argv).ok_or("mosh: no host given")?;
    // No console window for ssh on Windows.
    let mut cmd = crate::sys::command(&ssh[0]);
    cmd.args(&ssh[1..]).args(["-n", "-tt", "--", &host, REMOTE]);
    cmd.envs(env.iter().map(|(k, v)| (k, v))).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = cmd.spawn().map_err(|e| format!("{}: {e}", ssh[0]))?;
    let mut err = child.stderr.take().ok_or("ssh: no stderr")?;
    let errs = thread::spawn(move || {
        let mut s = String::new();
        let _ = err.read_to_string(&mut s);
        s
    });
    let mut out = String::new();
    if let Some(o) = child.stdout.take() {
        for line in BufReader::new(o).lines().map_while(Result::ok) {
            out.push_str(&line);
            out.push('\n');
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let errs = errs.join().unwrap_or_default();
    for line in errs.lines().map(str::trim_end).filter(|l| !l.is_empty()) {
        note(line);
    }
    let Some((port, key, seen)) = parse(&out) else {
        for line in out.lines().map(str::trim_end).filter(|l| !l.is_empty()) {
            note(line);
        }
        return Err(if status.success() { "mosh-server gave no MOSH CONNECT line (is mosh installed on the host?)".into() } else { format!("ssh ended with {status}") });
    };
    let ip = resolve(&host, seen).or(seen).ok_or_else(|| format!("cannot resolve {host}"))?;
    Ok(Connect { ip, port, key })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|a| a.to_string()).collect()
    }

    #[test]
    fn splits_the_launch() {
        let argv = v(&["mosh", "--ssh=ssh -p 2222 -l yk", "--", "box.lan"]);
        assert_eq!(split(&argv), Some((v(&["ssh", "-p", "2222", "-l", "yk"]), "box.lan".into())));
        assert_eq!(split(&v(&["mosh", "yk@box"])), Some((v(&["ssh"]), "yk@box".into())));
        assert_eq!(split(&v(&["mosh", "--"])), None);
    }

    #[test]
    fn reads_mosh_server_output() {
        let out = "MOSH SSH_CONNECTION 10.0.0.9 51234 10.0.0.5 22\r\n\r\nMOSH CONNECT 60001 4NeCCgvZFe2RnPgrcU1PQw\r\n\r\nmosh-server (mosh 1.4.0)\r\n";
        let (port, key, seen) = parse(out).unwrap();
        assert_eq!((port, key.as_str()), (60001, "4NeCCgvZFe2RnPgrcU1PQw"));
        assert_eq!(seen, Some("10.0.0.5".parse().unwrap()));
        assert_eq!(parse("Permission denied\n"), None);
    }

    #[test]
    fn resolves_literals_and_users() {
        assert_eq!(resolve("yk@127.0.0.1", None), Some("127.0.0.1".parse().unwrap()));
        assert_eq!(resolve("[::1]", None), Some("::1".parse().unwrap()));
    }

    /// A fake ssh that prints what mosh-server would.
    #[cfg(unix)]
    #[test]
    fn connects_through_a_fake_ssh() {
        let dir = std::env::temp_dir().join(format!("tv-mosh-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("ssh");
        std::fs::write(&fake, "#!/bin/sh\necho 'warning: added host' >&2\nprintf 'MOSH SSH_CONNECTION 1.2.3.4 1 127.0.0.1 22\\r\\nMOSH CONNECT 60002 abcKEY\\r\\n'\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        let argv = v(&["mosh", &format!("--ssh={}", fake.display()), "--", "no-such-alias.invalid"]);
        let notes = std::cell::RefCell::new(Vec::new());
        let c = connect(&argv, &[], |l| notes.borrow_mut().push(l.to_string())).unwrap();
        assert_eq!(c, Connect { ip: "127.0.0.1".parse().unwrap(), port: 60002, key: "abcKEY".into() });
        assert_eq!(notes.borrow().as_slice(), ["warning: added host"]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
