//! What the user selected, and where it is on screen.

use alacritty_terminal::term::cell::Flags;

use crate::shadow::Snapshot;

/// Reads the user's current mouse selection.
///
/// `PEEKME_SELECTION` overrides it (used by tests and for manual debugging).
pub struct SelectionSource {
    #[cfg(target_os = "linux")]
    clipboard: Option<arboard::Clipboard>,
    /// Read the system's selection (off in tests, which must not see the desktop's).
    system: bool,
    /// A fixed selection, for tests (they must not change the process environment:
    /// tests run in parallel threads, and that is not safe on macOS).
    fixed: Option<String>,
}

impl SelectionSource {
    pub fn new() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            clipboard: None,
            system: true,
            fixed: None,
        }
    }

    /// Only `PEEKME_SELECTION`, never the system's selection.
    pub fn without_system() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            clipboard: None,
            system: false,
            fixed: None,
        }
    }

    /// Always this text, never the system's selection or the environment.
    pub fn fixed(text: &str) -> Self {
        Self {
            fixed: Some(text.to_string()),
            ..Self::without_system()
        }
    }

    pub fn read(&mut self) -> Option<String> {
        if let Some(s) = &self.fixed {
            return Some(s.clone());
        }
        if let Ok(s) = std::env::var("PEEKME_SELECTION") {
            return Some(s).filter(|s| !s.trim().is_empty());
        }
        if !self.system {
            return None;
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
        if std::env::var_os("SSH_CONNECTION").is_some() {
            // This machine's clipboard, not the one of the user's terminal.
            return None;
        }
        // No PRIMARY selection outside X11/Wayland; the clipboard works when the
        // terminal copies on select.
        arboard::Clipboard::new()
            .ok()?
            .get_text()
            .ok()
            .filter(|s| !s.trim().is_empty())
    }
}

impl Default for SelectionSource {
    fn default() -> Self {
        Self::new()
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

/// Find the selection on screen. With a `hint` (the cell where a mouse drag
/// ended) the occurrence touching that row wins; otherwise the one nearest
/// the bottom, which is what the user most likely just read.
pub fn locate(snap: &Snapshot, selection: &str, hint: Option<(usize, usize)>) -> Option<Located> {
    let (screen, pos) = snap.text_with_positions();
    let (norm_screen, map) = normalize(&screen);
    let sel: Vec<char> = selection.chars().collect();
    let (norm_sel, _) = normalize(&sel);
    if norm_sel.is_empty() {
        return None;
    }
    let rows_of = |s: usize, len: usize| (pos[map[s]].0, pos[map[s + len - 1]].0);
    let pick = |starts: Vec<usize>, len: usize| -> Option<(usize, usize)> {
        let best = match hint {
            Some((row, _)) => starts.into_iter().min_by_key(|&s| {
                let (a, b) = rows_of(s, len);
                // Distance from the hint row; ties go to the lower occurrence.
                let d = if row < a {
                    a - row
                } else {
                    row.saturating_sub(b)
                };
                (d, usize::MAX - s)
            }),
            None => starts.into_iter().last(),
        }?;
        Some((best, best + len - 1))
    };
    let found = pick(find_all(&norm_screen, &norm_sel), norm_sel.len()).or_else(|| {
        // Part of a long selection may be off screen: anchor on its visible end or start.
        let n = norm_sel.len().min(40);
        if norm_sel.len() <= n {
            return None;
        }
        let tail = &norm_sel[norm_sel.len() - n..];
        let head = &norm_sel[..n];
        pick(find_all(&norm_screen, tail), n).or_else(|| pick(find_all(&norm_screen, head), n))
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

/// Every place `text` appears on screen, each as the cells it covers: per
/// row, from its first to its last non-blank char (a wide char with its
/// second half).
pub fn occurrences(snap: &Snapshot, text: &str) -> Vec<Vec<(usize, usize)>> {
    let (screen, pos) = snap.text_with_positions();
    let (norm_screen, map) = normalize(&screen);
    let (norm_text, _) = normalize(&text.chars().collect::<Vec<_>>());
    if norm_text.is_empty() {
        return Vec::new();
    }
    find_all(&norm_screen, &norm_text)
        .into_iter()
        .map(|s| {
            let mut cells: Vec<(usize, usize)> = Vec::new();
            for &i in &map[s..s + norm_text.len()] {
                if screen[i].is_whitespace() {
                    continue;
                }
                let (r, c) = pos[i];
                let c = if snap.rows[r][c].flags.contains(Flags::WIDE_CHAR) {
                    c + 1
                } else {
                    c
                };
                // Fill the gaps between words on the same row.
                let from = match cells.last() {
                    Some(&(lr, lc)) if lr == r => lc + 1,
                    _ => pos[i].1,
                };
                cells.extend((from..=c).map(|c| (r, c)));
            }
            cells
        })
        .collect()
}

fn find_all(hay: &[char], needle: &[char]) -> Vec<usize> {
    if needle.len() > hay.len() {
        return Vec::new();
    }
    (0..=hay.len() - needle.len())
        .filter(|&i| hay[i..i + needle.len()] == *needle)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shadow::Shadow;

    fn rows(s: &Snapshot, sel: &str) -> Option<(usize, usize)> {
        locate(s, sel, None).map(|l| (l.first_row, l.last_row))
    }

    #[test]
    fn located_range_is_the_selected_text() {
        let s = snap(b"Tip: Try the Desktop app on Linux\r\n", 40, 3);
        let l = locate(&s, "Desktop app", None).unwrap();
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
    fn mouse_hint_picks_the_occurrence_under_the_pointer() {
        let s = snap(b"use ripgrep here\r\nother\r\nripgrep again\r\n", 20, 5);
        let at = |hint| locate(&s, "ripgrep", hint).map(|l| (l.first_row, l.last_row));
        assert_eq!(at(None), Some((2, 2)));
        assert_eq!(at(Some((0, 10))), Some((0, 0)));
        assert_eq!(at(Some((1, 3))), Some((2, 2)));
    }

    #[test]
    fn soft_wrapped_selection_matches() {
        // 10 columns: "0123456789abc" wraps onto the next row.
        let s = snap(b"0123456789abc\r\n", 10, 4);
        assert_eq!(rows(&s, "789abc"), Some((0, 1)));
    }

    #[test]
    fn cursor_positioned_words_match() {
        // TUIs draw words with explicit cursor moves rather than spaces.
        let s = snap(b"\x1b[1;1HDo\x1b[1;4Hyou\x1b[1;8Htrust", 20, 3);
        assert_eq!(rows(&s, "you trust"), Some((0, 0)));
    }
}
