// Shared parts of byte-stream sessions (serial, raw TCP, telnet):
// events, Enter handling, input parsing and the hex view.

use serde::{Deserialize, Serialize};

/// Thread-safe writer into an open link (used by file transfer engines).
pub type Sink = Box<dyn FnMut(&[u8]) -> std::io::Result<()> + Send>;

fn closed() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::BrokenPipe, "link closed")
}

/// Sink that forwards each chunk with `send` until it fails.
pub fn sink<T: Send + 'static>(tx: std::sync::mpsc::Sender<T>, wrap: fn(Vec<u8>) -> T) -> Sink {
    Box::new(move |d: &[u8]| tx.send(wrap(d.to_vec())).map_err(|_| closed()))
}

/// From an I/O thread to the pane.
pub enum Event {
    /// Connection established, with a short description.
    Up(String),
    Data(Vec<u8>),
    Closed(String),
}

/// Bytes sent for Enter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Eol {
    #[default]
    Cr,
    Lf,
    Crlf,
}

impl Eol {
    pub const ALL: [Eol; 3] = [Eol::Cr, Eol::Lf, Eol::Crlf];

    pub fn label(self) -> &'static str {
        match self {
            Eol::Cr => "CR",
            Eol::Lf => "LF",
            Eol::Crlf => "CRLF",
        }
    }

    pub fn bytes(self) -> &'static [u8] {
        match self {
            Eol::Cr => b"\r",
            Eol::Lf => b"\n",
            Eol::Crlf => b"\r\n",
        }
    }
}

/// How incoming bytes are shown.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum View {
    #[default]
    Text,
    Hex,
    Mavlink,
}

impl View {
    pub const ALL: [View; 3] = [View::Text, View::Hex, View::Mavlink];

    pub fn label(self) -> &'static str {
        match self {
            View::Text => "Text",
            View::Hex => "Hex",
            View::Mavlink => "MAVLink",
        }
    }
}

/// Input and display options of a stream session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamOpts {
    pub eol: Eol,
    pub echo: bool,
    /// Start in hex view.
    pub hex: bool,
    /// Start in MAVLink view (wins over `hex`).
    pub mavlink: bool,
    pub reconnect: bool,
}

impl Default for StreamOpts {
    fn default() -> Self {
        Self { eol: Eol::Cr, echo: false, hex: false, mavlink: false, reconnect: true }
    }
}

impl StreamOpts {
    pub fn view(&self) -> View {
        match (self.mavlink, self.hex) {
            (true, _) => View::Mavlink,
            (false, true) => View::Hex,
            _ => View::Text,
        }
    }
}

/// Maps typed text to wire bytes: Enter (CR) becomes the configured EOL.
pub fn outgoing(text: &str, eol: Eol) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() + 1);
    for b in text.bytes() {
        if b == b'\r' {
            out.extend_from_slice(eol.bytes());
        } else {
            out.push(b);
        }
    }
    out
}

/// Local echo text: CR shown as a new line.
pub fn echo(text: &str) -> String {
    text.replace('\r', "\r\n")
}

/// Parses hex bytes: "55 AA 01", "55AA01", "0x55,0xaa".
pub fn parse_hex(s: &str) -> Result<Vec<u8>, String> {
    let digits: String = s
        .split(|c: char| c.is_whitespace() || c == ',' || c == ':' || c == '-')
        .filter(|t| !t.is_empty())
        .map(|t| t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")).unwrap_or(t))
        .map(|t| if t.len() % 2 == 1 { format!("0{t}") } else { t.to_string() })
        .collect();
    if let Some(bad) = digits.chars().find(|c| !c.is_ascii_hexdigit()) {
        return Err(format!("'{bad}' is not a hex digit"));
    }
    (0..digits.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&digits[i..i + 2], 16).map_err(|e| e.to_string()))
        .collect()
}

/// Parses text with C escapes: \r \n \t \0 \\ \xHH.
pub fn unescape(s: &str) -> Result<Vec<u8>, String> {
    let mut out = Vec::with_capacity(s.len());
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c != '\\' {
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            continue;
        }
        match it.next() {
            Some('r') => out.push(b'\r'),
            Some('n') => out.push(b'\n'),
            Some('t') => out.push(b'\t'),
            Some('0') => out.push(0),
            Some('\\') => out.push(b'\\'),
            Some('x') => {
                let h: String = it.by_ref().take(2).collect();
                if h.len() != 2 {
                    return Err("\\x needs two hex digits".into());
                }
                out.push(u8::from_str_radix(&h, 16).map_err(|_| format!("bad escape \\x{h}"))?);
            }
            Some(o) => return Err(format!("unknown escape \\{o}")),
            None => return Err("trailing backslash".into()),
        }
    }
    Ok(out)
}

const DIM: &str = "\x1b[2m";
const OFF: &str = "\x1b[0m";

/// Hex dump, 16 bytes per line with offset and ASCII column. Keeps its
/// place across chunks; the ASCII column is printed when a line is full.
#[derive(Default)]
pub struct Hex {
    offset: usize,
    line: Vec<u8>,
}

impl Hex {
    pub fn feed(&mut self, data: &[u8]) -> String {
        let mut s = String::with_capacity(data.len() * 4 + 32);
        for &b in data {
            if self.line.is_empty() {
                s.push_str(&format!("{DIM}{:08X}{OFF}  ", self.offset));
            }
            s.push_str(&format!("{b:02X} "));
            if self.line.len() == 7 {
                s.push(' ');
            }
            self.line.push(b);
            self.offset += 1;
            if self.line.len() == 16 {
                let ascii: String = self
                    .line
                    .iter()
                    .map(|&c| if c.is_ascii_graphic() || c == b' ' { c as char } else { '.' })
                    .collect();
                s.push_str(&format!(" {DIM}|{OFF}{ascii}{DIM}|{OFF}\r\n"));
                self.line.clear();
            }
        }
        s
    }

    /// Starts a fresh line (offset restarts at 0).
    pub fn reset(&mut self) {
        self.offset = 0;
        self.line.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(s: &str) -> String {
        s.replace(DIM, "").replace(OFF, "")
    }

    #[test]
    fn eol_mapping() {
        assert_eq!(outgoing("ls\r", Eol::Cr), b"ls\r");
        assert_eq!(outgoing("ls\r", Eol::Lf), b"ls\n");
        assert_eq!(outgoing("ls\r", Eol::Crlf), b"ls\r\n");
        assert_eq!(echo("a\rb"), "a\r\nb");
    }

    #[test]
    fn hex_input_forms() {
        assert_eq!(parse_hex("55 AA 01"), Ok(vec![0x55, 0xAA, 0x01]));
        assert_eq!(parse_hex("55aa01"), Ok(vec![0x55, 0xAA, 0x01]));
        assert_eq!(parse_hex("0x55,0xaa, 0x1"), Ok(vec![0x55, 0xAA, 0x01]));
        assert_eq!(parse_hex("fe:09"), Ok(vec![0xFE, 0x09]));
        assert_eq!(parse_hex(""), Ok(vec![]));
        assert!(parse_hex("5g").is_err());
    }

    #[test]
    fn escapes() {
        assert_eq!(unescape("AT\\r\\n"), Ok(b"AT\r\n".to_vec()));
        assert_eq!(unescape("\\x55\\xaa\\0"), Ok(vec![0x55, 0xAA, 0]));
        assert_eq!(unescape("a\\\\b"), Ok(b"a\\b".to_vec()));
        assert!(unescape("\\x5").is_err());
        assert!(unescape("\\q").is_err());
        assert!(unescape("end\\").is_err());
    }

    #[test]
    fn hex_dump_lines() {
        let mut h = Hex::default();
        let first = plain(&h.feed(b"Hello, MAVLink!"));
        assert!(first.starts_with("00000000  48 65 6C 6C 6F 2C 20 4D  41"));
        assert!(!first.contains('|'));
        let rest = plain(&h.feed(&[0xFE, 0x00]));
        assert!(rest.starts_with("FE  |Hello, MAVLink!.|\r\n00000010  00 "));
    }

    #[test]
    fn view_choice() {
        let mut o = StreamOpts::default();
        assert_eq!(o.view(), View::Text);
        o.hex = true;
        assert_eq!(o.view(), View::Hex);
        o.mavlink = true;
        assert_eq!(o.view(), View::Mavlink);
    }
}
