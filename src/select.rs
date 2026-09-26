//! What the user selected, and where it is on screen.

use crate::shadow::Snapshot;

/// Reads the user's current mouse selection.
///
/// `PEEKME_SELECTION` overrides it (used by tests and for manual debugging).
pub struct SelectionSource {
    #[cfg(target_os = "linux")]
    clipboard: Option<arboard::Clipboard>,
}

impl SelectionSource {
    pub fn new() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            clipboard: None,
        }
    }

    pub fn read(&mut self) -> Option<String> {
        if let Ok(s) = std::env::var("PEEKME_SELECTION") {
            return Some(s).filter(|s| !s.trim().is_empty());
        }
        self.read_system()
    }

    #[cfg(target_os = "linux")]
    fn read_system(&mut self) -> Option<String> {
        use arboard::{GetExtLinux, LinuxClipboardKind};
        if std::env::var_os("SSH_CONNECTION").is_some() {
            // The mouse selection lives on the user's machine, not this one.
            return None;
        }
        if self.clipboard.is_none() {
            self.clipboard = arboard::Clipboard::new().ok();
        }
        let cb = self.clipboard.as_mut()?;
        cb.get()
            .clipboard(LinuxClipboardKind::Primary)
            .text()
            .ok()
            .filter(|s| !s.trim().is_empty())
    }

    #[cfg(not(target_os = "linux"))]
    fn read_system(&mut self) -> Option<String> {
        // No PRIMARY selection outside X11/Wayland; the clipboard works when the
        // terminal copies on select.
        arboard::Clipboard::new()
            .ok()?
            .get_text()
            .ok()
            .filter(|s| !s.trim().is_empty())
    }
}

/// Collapse whitespace runs to one space; returns the text and, for each output
/// char, the index of the input char it came from.
pub fn normalize(chars: &[char]) -> (Vec<char>, Vec<usize>) {
    let mut out = Vec::with_capacity(chars.len());
    let mut map = Vec::with_capacity(chars.len());
    let mut in_space = false;
    for (i, &c) in chars.iter().enumerate() {
        if c.is_whitespace() || c == '\u{a0}' {
            if !in_space && !out.is_empty() {
                out.push(' ');
                map.push(i);
            }
            in_space = true;
        } else {
            out.push(c);
            map.push(i);
            in_space = false;
        }
    }
    if out.last() == Some(&' ') {
        out.pop();
        map.pop();
    }
    (out, map)
}

/// Where a selection is on screen.
pub struct Located {
    pub first_row: usize,
    pub last_row: usize,
    /// The whole screen as text, with the selection's range in it.
    pub screen: crate::context::ScreenSel,
}

/// Find the selection on screen, preferring the occurrence nearest the bottom.
pub fn locate(snap: &Snapshot, selection: &str) -> Option<Located> {
    let (screen, pos) = snap.text_with_positions();
    let (norm_screen, map) = normalize(&screen);
    let sel: Vec<char> = selection.chars().collect();
    let (norm_sel, _) = normalize(&sel);
    if norm_sel.is_empty() {
        return None;
    }
    let found = rfind(&norm_screen, &norm_sel)
        .map(|s| (s, s + norm_sel.len() - 1))
        .or_else(|| {
            // Part of a long selection may be off screen: anchor on its visible end or start.
            let n = norm_sel.len().min(40);
            if norm_sel.len() <= n {
                return None;
            }
            let tail = &norm_sel[norm_sel.len() - n..];
            let head = &norm_sel[..n];
            rfind(&norm_screen, tail)
                .map(|s| (s, s + n - 1))
                .or_else(|| rfind(&norm_screen, head).map(|s| (s, s + n - 1)))
        })?;
    let (start, last) = (map[found.0], map[found.1]);
    Some(Located {
        first_row: pos[start].0,
        last_row: pos[last].0,
        screen: crate::context::ScreenSel {
            text: screen,
            start,
            end: last + 1,
        },
    })
}

fn rfind(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len())
        .rev()
        .find(|&i| hay[i..i + needle.len()] == *needle)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shadow::Shadow;

    fn rows(s: &Snapshot, sel: &str) -> Option<(usize, usize)> {
        locate(s, sel).map(|l| (l.first_row, l.last_row))
    }

    #[test]
    fn located_range_is_the_selected_text() {
        let s = snap(b"Tip: Try the Desktop app on Linux\r\n", 40, 3);
        let l = locate(&s, "Desktop app").unwrap();
        assert_eq!(l.screen.selected(), "Desktop app");
    }

    fn snap(bytes: &[u8], cols: u16, rows: u16) -> Snapshot {
        let mut s = Shadow::new(cols, rows);
        s.advance(bytes);
        s.snapshot()
    }

    #[test]
    fn finds_text_across_rows_and_prefers_bottom() {
        let s = snap(b"alpha beta\r\ngamma delta\r\nfoo\r\nalpha beta\r\n", 20, 6);
        assert_eq!(rows(&s, "alpha beta"), Some((3, 3)));
        assert_eq!(rows(&s, "beta\ngamma"), Some((0, 1)));
        assert_eq!(rows(&s, "  gamma   delta "), Some((1, 1)));
        assert_eq!(rows(&s, "missing"), None);
    }

    #[test]
    fn soft_wrapped_selection_matches() {
        // 10 columns: "0123456789abc" wraps onto the next row.
        let s = snap(b"0123456789abc\r\n", 10, 4);
        assert_eq!(rows(&s, "789abc"), Some((0, 1)));
    }

    #[test]
    fn cursor_positioned_words_match() {
        // Codex draws words with explicit cursor moves rather than spaces.
        let s = snap(b"\x1b[1;1HDo\x1b[1;4Hyou\x1b[1;8Htrust", 20, 3);
        assert_eq!(rows(&s, "you trust"), Some((0, 0)));
    }
}
