//! Terminal input tokenizer and key classification.
//!
//! Everything read from the user's terminal is split into whole tokens (text,
//! CSI/OSC/DCS/SS3 sequences, Alt+key) so that replies to the child's queries
//! are forwarded intact and are never mistaken for key presses. Keys arrive
//! either in legacy encoding or, once Codex pushes kitty keyboard flags, as
//! `CSI code[:shifted[:base]] ; mods[:event] u`.

use std::collections::HashSet;

/// One complete input unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub bytes: Vec<u8>,
    pub kind: Kind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// The peek hotkey (Alt+P), press or repeat.
    Hotkey,
    /// Escape key press.
    Esc,
    PageUp,
    PageDown,
    /// Any other key press, paste or text.
    Key,
    /// A key release event (kitty event type 3).
    Release,
    /// Terminal replies, focus reports and other non-key sequences.
    Passive,
}

/// Identity of a physical key, used to pair presses with their releases.
pub type KeyId = (u8, u32);

/// Splits a chunk into tokens. An incomplete trailing escape sequence is
/// returned in `carry` so the caller can prepend it to the next read.
pub fn tokenize(input: &[u8], carry: &mut Vec<u8>, in_paste: &mut bool) -> Vec<Token> {
    let mut data = std::mem::take(carry);
    data.extend_from_slice(input);
    let mut out = Vec::new();
    let mut i = 0;
    let mut text_start = None::<usize>;

    let flush_text = |out: &mut Vec<Token>, start: &mut Option<usize>, end: usize, data: &[u8]| {
        if let Some(s) = start.take()
            && s < end
        {
            out.push(Token {
                bytes: data[s..end].to_vec(),
                kind: Kind::Key,
            });
        }
    };

    while i < data.len() {
        if data[i] != 0x1b {
            text_start.get_or_insert(i);
            i += 1;
            continue;
        }
        flush_text(&mut out, &mut text_start, i, &data);
        match seq_len(&data[i..]) {
            SeqLen::Complete(n) => {
                let bytes = data[i..i + n].to_vec();
                let kind = if *in_paste {
                    if bytes == b"\x1b[201~" {
                        *in_paste = false;
                    }
                    Kind::Key
                } else if bytes == b"\x1b[200~" {
                    *in_paste = true;
                    Kind::Key
                } else {
                    classify(&bytes)
                };
                out.push(Token { bytes, kind });
                i += n;
            }
            SeqLen::Incomplete => {
                // A lone ESC at the very end of a read is the Escape key: terminals
                // deliver a whole key press per write, so nothing else is coming.
                if data.len() - i == 1 {
                    out.push(Token {
                        bytes: vec![0x1b],
                        kind: if *in_paste { Kind::Key } else { Kind::Esc },
                    });
                    i += 1;
                } else {
                    carry.extend_from_slice(&data[i..]);
                    i = data.len();
                }
            }
        }
    }
    flush_text(&mut out, &mut text_start, data.len(), &data);
    out
}

enum SeqLen {
    Complete(usize),
    Incomplete,
}

/// Length of the escape sequence at the start of `s` (which begins with ESC).
fn seq_len(s: &[u8]) -> SeqLen {
    if s.len() < 2 {
        return SeqLen::Incomplete;
    }
    match s[1] {
        b'[' => {
            // CSI: parameters and intermediates, then a final byte 0x40..=0x7e.
            for (j, &b) in s.iter().enumerate().skip(2) {
                if (0x40..=0x7e).contains(&b) {
                    return SeqLen::Complete(j + 1);
                }
            }
            SeqLen::Incomplete
        }
        b']' | b'P' | b'_' | b'^' => {
            // OSC / DCS / APC / PM: terminated by BEL or ST (ESC \).
            let mut j = 2;
            while j < s.len() {
                if s[j] == 0x07 {
                    return SeqLen::Complete(j + 1);
                }
                if s[j] == 0x1b && j + 1 < s.len() && s[j + 1] == b'\\' {
                    return SeqLen::Complete(j + 2);
                }
                j += 1;
            }
            SeqLen::Incomplete
        }
        b'O' => {
            if s.len() >= 3 {
                SeqLen::Complete(3)
            } else {
                SeqLen::Incomplete
            }
        }
        _ => {
            // Alt + a UTF-8 character.
            let width = utf8_len(s[1]);
            if s.len() > width {
                SeqLen::Complete(1 + width)
            } else {
                SeqLen::Incomplete
            }
        }
    }
}

fn utf8_len(first: u8) -> usize {
    match first {
        0x00..=0x7f => 1,
        0xc0..=0xdf => 2,
        0xe0..=0xef => 3,
        0xf0..=0xf7 => 4,
        _ => 1,
    }
}

/// Parsed `CSI ... u` / `CSI ... ~` key event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyEvent {
    pub id: KeyId,
    pub base: Option<u32>,
    /// Modifier bits (shift 1, alt 2, ctrl 4, super 8, ...), lock bits removed.
    pub mods: u32,
    /// 1 press, 2 repeat, 3 release.
    pub event: u8,
}

pub fn parse_key(bytes: &[u8]) -> Option<KeyEvent> {
    if bytes.len() < 3 || &bytes[..2] != b"\x1b[" {
        return None;
    }
    let fin = *bytes.last()?;
    if fin != b'u' && fin != b'~' {
        return None;
    }
    let body = std::str::from_utf8(&bytes[2..bytes.len() - 1]).ok()?;
    if body.starts_with(['?', '>', '<', '=']) {
        return None;
    }
    let mut groups = body.split(';');
    let key_group = groups.next()?;
    let mut key_parts = key_group.split(':');
    let code: u32 = key_parts.next()?.parse().ok()?;
    let _shifted = key_parts.next();
    let base = key_parts.next().and_then(|b| b.parse().ok());
    let (mods, event) = match groups.next() {
        Some(m) => {
            let mut mp = m.split(':');
            let raw: u32 = mp
                .next()
                .filter(|s| !s.is_empty())
                .map_or(Some(1), |s| s.parse().ok())?;
            let ev: u8 = mp.next().map_or(Some(1), |s| s.parse().ok())?;
            (raw.saturating_sub(1) & !(64 | 128), ev)
        }
        None => (0, 1),
    };
    Some(KeyEvent {
        id: (fin, code),
        base,
        mods,
        event,
    })
}

const ALT: u32 = 2;

fn classify(bytes: &[u8]) -> Kind {
    // Legacy encodings.
    match bytes {
        b"\x1bp" => return Kind::Hotkey,
        b"\x1b[5~" => return Kind::PageUp,
        b"\x1b[6~" => return Kind::PageDown,
        b"\x1b[I" | b"\x1b[O" => return Kind::Passive,
        _ => {}
    }
    if let Some(k) = parse_key(bytes) {
        if k.event == 3 {
            return Kind::Release;
        }
        let is_p = k.id == (b'u', 112) || k.base == Some(112);
        return match (k.id, k.mods) {
            _ if is_p && k.id.0 == b'u' && k.mods == ALT => Kind::Hotkey,
            ((b'u', 27), 0) => Kind::Esc,
            ((b'~', 5), 0) => Kind::PageUp,
            ((b'~', 6), 0) => Kind::PageDown,
            _ => Kind::Key,
        };
    }
    if bytes.len() >= 2 && bytes[1] != b'[' && bytes[1] != b'O' {
        // OSC/DCS/APC replies, or Alt+<char> other than p.
        return if matches!(bytes[1], b']' | b'P' | b'_' | b'^') {
            Kind::Passive
        } else {
            Kind::Key
        };
    }
    if bytes.starts_with(b"\x1b[") {
        let fin = *bytes.last().unwrap_or(&0);
        let private = bytes
            .get(2)
            .is_some_and(|b| matches!(b, b'?' | b'>' | b'<' | b'='));
        // Replies: cursor position (R), mode report ($y), DA (c), kitty flags (? u).
        if fin == b'R' || fin == b'c' || bytes.ends_with(b"$y") || (private && fin == b'u') {
            return Kind::Passive;
        }
    }
    Kind::Key
}

/// Tracks key presses consumed by peekme so that their repeat and release
/// events are swallowed too; the child then always sees balanced key events.
#[derive(Default)]
pub struct Consumed {
    keys: HashSet<KeyId>,
}

impl Consumed {
    /// Record that the press in `token` was consumed.
    pub fn consume(&mut self, token: &Token) {
        if let Some(k) = parse_key(&token.bytes) {
            self.keys.insert(k.id);
        }
    }

    /// True if this repeat/release belongs to a consumed press (and should be dropped).
    pub fn swallow(&mut self, token: &Token) -> bool {
        let Some(k) = parse_key(&token.bytes) else {
            return false;
        };
        match k.event {
            2 => self.keys.contains(&k.id),
            3 => self.keys.remove(&k.id),
            _ => false,
        }
    }

    /// Focus changes lose releases, so forget everything rather than swallow later.
    pub fn clear(&mut self) {
        self.keys.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(input: &[u8]) -> Vec<Kind> {
        let mut carry = Vec::new();
        let mut paste = false;
        tokenize(input, &mut carry, &mut paste)
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn legacy_and_kitty_hotkey() {
        assert_eq!(kinds(b"\x1bp"), vec![Kind::Hotkey]);
        assert_eq!(kinds(b"\x1b[112;3u"), vec![Kind::Hotkey]);
        assert_eq!(kinds(b"\x1b[112;3:1u"), vec![Kind::Hotkey]);
        assert_eq!(kinds(b"\x1b[112;3:3u"), vec![Kind::Release]);
        // Cyrillic layout: key code for 'з' with base-layout key 'p'.
        assert_eq!(kinds(b"\x1b[1079::112;3u"), vec![Kind::Hotkey]);
        // Caps lock does not change the meaning.
        assert_eq!(kinds(b"\x1b[112;67u"), vec![Kind::Hotkey]);
        // Plain p, Ctrl+Alt+P are not the hotkey.
        assert_eq!(kinds(b"p"), vec![Kind::Key]);
        assert_eq!(kinds(b"\x1b[112;7u"), vec![Kind::Key]);
    }

    #[test]
    fn replies_are_passive() {
        assert_eq!(kinds(b"\x1b[24;1R"), vec![Kind::Passive]);
        assert_eq!(
            kinds(b"\x1b]11;rgb:0000/0000/0000\x1b\\"),
            vec![Kind::Passive]
        );
        assert_eq!(kinds(b"\x1b[?2026;2$y"), vec![Kind::Passive]);
        assert_eq!(kinds(b"\x1b[?7u"), vec![Kind::Passive]);
        assert_eq!(kinds(b"\x1b[I"), vec![Kind::Passive]);
    }

    #[test]
    fn esc_and_paging() {
        assert_eq!(kinds(b"\x1b"), vec![Kind::Esc]);
        assert_eq!(kinds(b"\x1b[27u"), vec![Kind::Esc]);
        assert_eq!(kinds(b"\x1b[5~"), vec![Kind::PageUp]);
        assert_eq!(kinds(b"\x1b[6;1:1~"), vec![Kind::PageDown]);
    }

    #[test]
    fn text_and_split_sequences() {
        let mut carry = Vec::new();
        let mut paste = false;
        let t = tokenize(b"ab\x1b[11", &mut carry, &mut paste);
        assert_eq!(t.len(), 1);
        assert_eq!(carry, b"\x1b[11");
        let t = tokenize(b"2;3u", &mut carry, &mut paste);
        assert_eq!(t[0].kind, Kind::Hotkey);
        assert!(carry.is_empty());
    }

    #[test]
    fn paste_never_triggers() {
        assert_eq!(
            kinds(b"\x1b[200~\x1bp\x1b[201~"),
            vec![Kind::Key, Kind::Key, Kind::Key]
        );
    }

    #[test]
    fn consumed_release_is_swallowed_once() {
        let mut c = Consumed::default();
        let press = Token {
            bytes: b"\x1b[27u".to_vec(),
            kind: Kind::Esc,
        };
        c.consume(&press);
        let repeat = Token {
            bytes: b"\x1b[27;1:2u".to_vec(),
            kind: Kind::Key,
        };
        let release = Token {
            bytes: b"\x1b[27;1:3u".to_vec(),
            kind: Kind::Release,
        };
        assert!(c.swallow(&repeat));
        assert!(c.swallow(&release));
        assert!(!c.swallow(&release));
    }
}
