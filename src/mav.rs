// MAVLink view: splits a byte stream into v1/v2 frames, checks the CRC
// and prints one decoded line per message (common dialect).

use mavlink::dialects::common::MavMessage;
use mavlink::{MavlinkVersion, Message};

const STX_V1: u8 = 0xFE;
const STX_V2: u8 = 0xFD;
const SIGNED: u8 = 0x01;
/// Bytes kept while waiting for a frame end before giving up.
const MAX_PENDING: usize = 4096;

const DIM: &str = "\x1b[2m";
const NAME: &str = "\x1b[1;36m";
const WARN: &str = "\x1b[33m";
const OFF: &str = "\x1b[0m";

#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
    skipped: usize,
    /// Show only messages whose name contains one of these (upper case).
    filter: Vec<String>,
    pub frames: u64,
    pub bad_crc: u64,
}

struct Frame {
    v2: bool,
    seq: u8,
    sys: u8,
    comp: u8,
    id: u32,
    len: usize,
    total: usize,
}

/// Header of a complete frame at the start of `buf`, None if more bytes
/// are needed.
fn frame(buf: &[u8]) -> Option<Frame> {
    let v2 = *buf.first()? == STX_V2;
    let len = usize::from(*buf.get(1)?);
    let (hdr, mut total) = if v2 { (10, 10 + len + 2) } else { (6, 6 + len + 2) };
    if v2 && buf.get(2)? & SIGNED != 0 {
        total += 13;
    }
    if buf.len() < total || buf.len() < hdr {
        return None;
    }
    let (seq, sys, comp, id) = if v2 {
        (buf[4], buf[5], buf[6], u32::from_le_bytes([buf[7], buf[8], buf[9], 0]))
    } else {
        (buf[2], buf[3], buf[4], u32::from(buf[5]))
    };
    Some(Frame { v2, seq, sys, comp, id, len, total })
}

/// Field part of the Debug output: "{ a: 1, b: 2 }".
fn fields(msg: &MavMessage) -> String {
    let dbg = format!("{msg:?}");
    match (dbg.find('{'), dbg.rfind('}')) {
        (Some(a), Some(b)) if b > a => dbg[a..=b].to_string(),
        _ => dbg,
    }
}

impl Decoder {
    /// Comma separated name parts, e.g. "heartbeat, attitude". Empty = all.
    pub fn set_filter(&mut self, text: &str) {
        self.filter = text
            .split(',')
            .map(|s| s.trim().to_uppercase())
            .filter(|s| !s.is_empty())
            .collect();
    }

    fn shown(&self, name: &str) -> bool {
        self.filter.is_empty() || self.filter.iter().any(|f| name.contains(f.as_str()))
    }

    fn flush_skipped(&mut self, out: &mut String) {
        if self.skipped > 0 {
            out.push_str(&format!("{DIM}[{} non-MAVLink byte(s)]{OFF}\r\n", self.skipped));
            self.skipped = 0;
        }
    }

    /// Consumes bytes and returns printable lines for complete frames.
    pub fn feed(&mut self, data: &[u8]) -> String {
        self.buf.extend_from_slice(data);
        let mut out = String::new();
        loop {
            match self.buf.iter().position(|&b| b == STX_V1 || b == STX_V2) {
                None => {
                    self.skipped += self.buf.len();
                    self.buf.clear();
                    break;
                }
                Some(0) => {}
                Some(n) => {
                    self.skipped += n;
                    self.buf.drain(..n);
                }
            }
            let Some(f) = frame(&self.buf) else {
                if self.buf.len() > MAX_PENDING {
                    self.skipped += 1;
                    self.buf.drain(..1);
                    continue;
                }
                break;
            };

            let hdr = if f.v2 { 10 } else { 6 };
            let body = &self.buf[1..hdr + f.len];
            let crc_rx = u16::from_le_bytes([self.buf[hdr + f.len], self.buf[hdr + f.len + 1]]);
            let version = if f.v2 { MavlinkVersion::V2 } else { MavlinkVersion::V1 };
            let payload = self.buf[hdr..hdr + f.len].to_vec();
            let parsed = MavMessage::parse(version, f.id, &payload);
            let known = parsed.is_ok();
            if known && mavlink::calculate_crc(body, MavMessage::extra_crc(f.id)) != crc_rx {
                // Not a frame after all, or corrupted: resync one byte later.
                self.bad_crc += 1;
                self.skipped += 1;
                self.buf.drain(..1);
                continue;
            }

            self.flush_skipped(&mut out);
            self.frames += 1;
            let head = format!("{DIM}#{:<3} v{} {}:{}{OFF}", f.seq, if f.v2 { 2 } else { 1 }, f.sys, f.comp);
            match parsed {
                Ok(msg) => {
                    let name = msg.message_name();
                    if self.shown(name) {
                        out.push_str(&format!("{head} {NAME}{name}{OFF} {}\r\n", fields(&msg)));
                    }
                }
                Err(_) => {
                    if self.filter.is_empty() {
                        let hex: Vec<String> = payload.iter().map(|b| format!("{b:02X}")).collect();
                        out.push_str(&format!(
                            "{head} {WARN}msg {} (not in common, CRC unchecked){OFF} {}\r\n",
                            f.id,
                            hex.join(" ")
                        ));
                    }
                }
            }
            self.buf.drain(..f.total);
        }
        out
    }

    pub fn reset(&mut self) {
        self.buf.clear();
        self.skipped = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mavlink::dialects::common::{self, HEARTBEAT_DATA};
    use mavlink::MavHeader;

    fn plain(s: &str) -> String {
        let mut out = String::new();
        let mut esc = false;
        for c in s.chars() {
            match (esc, c) {
                (false, '\x1b') => esc = true,
                (true, 'm') => esc = false,
                (true, _) => {}
                (false, c) => out.push(c),
            }
        }
        out
    }

    fn heartbeat() -> MavMessage {
        MavMessage::HEARTBEAT(HEARTBEAT_DATA {
            custom_mode: 5,
            mavtype: common::MavType::MAV_TYPE_QUADROTOR,
            autopilot: common::MavAutopilot::MAV_AUTOPILOT_PX4,
            base_mode: common::MavModeFlag::empty(),
            system_status: common::MavState::MAV_STATE_ACTIVE,
            mavlink_version: 3,
        })
    }

    fn header(seq: u8) -> MavHeader {
        MavHeader { system_id: 1, component_id: 1, sequence: seq }
    }

    fn v2(seq: u8) -> Vec<u8> {
        let mut w = Vec::new();
        mavlink::write_v2_msg(&mut w, header(seq), &heartbeat()).expect("write v2");
        w
    }

    #[test]
    fn decodes_v1_and_v2() {
        let mut v1 = Vec::new();
        mavlink::write_v1_msg(&mut v1, header(7), &heartbeat()).expect("write v1");
        let mut d = Decoder::default();
        let text = plain(&d.feed(&[v1, v2(8)].concat()));
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(lines[0].starts_with("#7   v1 1:1 HEARTBEAT {"), "{}", lines[0]);
        assert!(lines[0].contains("custom_mode: 5"));
        assert!(lines[1].starts_with("#8   v2 1:1 HEARTBEAT"));
        assert_eq!(d.frames, 2);
    }

    #[test]
    fn frame_split_across_chunks_and_noise() {
        let f = v2(1);
        let mut d = Decoder::default();
        let mut text = d.feed(b"boot log\r\n");
        text += &d.feed(&f[..5]);
        assert!(plain(&text).is_empty());
        text += &d.feed(&f[5..]);
        let t = plain(&text);
        assert!(t.starts_with("[10 non-MAVLink byte(s)]"), "{t}");
        assert!(t.contains("HEARTBEAT"));
    }

    #[test]
    fn bad_crc_is_dropped() {
        let mut f = v2(1);
        let n = f.len();
        f[n - 1] ^= 0xFF;
        let mut d = Decoder::default();
        let t = plain(&d.feed(&[f, v2(2)].concat()));
        assert_eq!(t.matches("HEARTBEAT").count(), 1, "{t}");
        assert!(t.contains("#2 "));
        assert!(d.bad_crc >= 1);
    }

    #[test]
    fn filter_by_name() {
        let mut d = Decoder::default();
        d.set_filter("attitude");
        assert!(plain(&d.feed(&v2(1))).is_empty());
        d.set_filter("heart, attitude");
        assert!(plain(&d.feed(&v2(2))).contains("HEARTBEAT"));
    }
}
