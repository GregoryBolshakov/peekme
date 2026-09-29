//! Claude Code's full-screen mode owns the mouse and copies a drag selection
//! to the clipboard with OSC 52 (`ESC ] 52 ; c ; <base64> BEL`). peekme
//! reads the selected text from that sequence as it passes through to the
//! terminal; the bytes themselves are forwarded unchanged.

const PREFIX: &[u8] = b"\x1b]52;";
/// Longer payloads are not selections anyone wants explained.
const MAX_BODY: usize = 1 << 20;

/// Incremental scanner over the child's output. Sequences may be split
/// across reads at any byte.
#[derive(Default)]
pub struct Osc52 {
    matched: usize,
    body: Option<Vec<u8>>,
}

impl Osc52 {
    /// Feed output bytes; returns the text of the last clipboard write that
    /// completed in them.
    pub fn feed(&mut self, bytes: &[u8]) -> Option<String> {
        let mut got = None;
        for &b in bytes {
            if let Some(body) = &mut self.body {
                let after_esc = body.last() == Some(&0x1b);
                match b {
                    0x07 if !after_esc => {
                        got = decode(body).or(got);
                        self.body = None;
                    }
                    b'\\' if after_esc => {
                        body.pop();
                        got = decode(body).or(got);
                        self.body = None;
                    }
                    // CAN/SUB abort a sequence, and so does a new ESC sequence.
                    0x18 | 0x1a => self.body = None,
                    _ if after_esc => {
                        self.body = None;
                        self.matched = if b == b']' { 2 } else { 0 };
                    }
                    _ if body.len() >= MAX_BODY => self.body = None,
                    _ => body.push(b),
                }
                continue;
            }
            if b == PREFIX[self.matched] {
                self.matched += 1;
                if self.matched == PREFIX.len() {
                    self.matched = 0;
                    self.body = Some(Vec::new());
                }
            } else {
                self.matched = usize::from(b == 0x1b);
            }
        }
        got
    }
}

/// `<targets>;<base64>` to text. A `?` payload is a query, not a write.
fn decode(body: &[u8]) -> Option<String> {
    let semi = body.iter().position(|&b| b == b';')?;
    // `c;<b64>`, and Copilot's `p!;<b64>;` with a closing `;`.
    let data = body[semi + 1..]
        .split(|&b| b == b';')
        .next()
        .unwrap_or_default();
    if data == b"?" {
        return None;
    }
    let text = String::from_utf8_lossy(&base64(data)?).into_owned();
    (!text.trim().is_empty()).then_some(text)
}

fn base64(data: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for &c in data {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\n' | b'\r' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_claude_selection() {
        // Captured from Claude Code 2.1.283 after a drag over " in files—a mod".
        let mut s = Osc52::default();
        let got = s.feed(b"\x1b[?2026h..\x1b]52;c;IGluIGZpbGVz4oCUYSBtb2Q=\x07\x1b[?2026l");
        assert_eq!(got.as_deref(), Some(" in files—a mod"));
    }

    #[test]
    fn reads_copilot_selection() {
        // Captured from GitHub Copilot CLI 1.0.89 after a drag over its banner.
        let mut s = Osc52::default();
        let got = s.feed(b"\x1b]52;p!;Q29waWxvdCB2MS4wLjg5IHU=;\x07");
        assert_eq!(got.as_deref(), Some("Copilot v1.0.89 u"));
        // Inside tmux it comes wrapped in DCS passthrough, with the ESC doubled.
        let mut s = Osc52::default();
        let got = s.feed(b"\x1bPtmux;\x1b\x1b]52;p!;Q29waWxvdCB2MS4wLjg5IHU=;\x07\x1b\\");
        assert_eq!(got.as_deref(), Some("Copilot v1.0.89 u"));
    }

    #[test]
    fn split_anywhere_and_st_terminator() {
        let seq = b"xx\x1b]52;c;aGVsbG8gd29ybGQ=\x1b\\yy";
        for cut in 0..seq.len() {
            let mut s = Osc52::default();
            let a = s.feed(&seq[..cut]);
            let b = s.feed(&seq[cut..]);
            assert_eq!(a.or(b).as_deref(), Some("hello world"), "cut at {cut}");
        }
    }

    #[test]
    fn ignores_queries_other_oscs_and_aborts() {
        let mut s = Osc52::default();
        assert_eq!(s.feed(b"\x1b]52;c;?\x07"), None);
        assert_eq!(s.feed(b"\x1b]0;title\x07\x1b]8;;http://x\x07"), None);
        assert_eq!(s.feed(b"\x1b]52;c;aGk\x18=\x07"), None);
        // An ESC that starts another sequence ends the unterminated one.
        assert_eq!(s.feed(b"\x1b]52;c;aGk\x1b[0m"), None);
        assert_eq!(s.feed(b"\x1b]52;c;aGk=\x07").as_deref(), Some("hi"));
    }
}
