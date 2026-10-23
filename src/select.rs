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
    /// The system source is a clipboard (no PRIMARY outside Linux): a copy
    /// stays there after its selection is gone, so only a new one counts.
    clipboard_only: bool,
    /// The clipboard text already used or passed over.
    seen: Option<String>,
    /// Also ask the terminal for its selection through Accessibility (a Mac,
    /// not over SSH). Used only once the user allowed it.
    accessibility: bool,
    /// The Accessibility selection already used or passed over: the terminal
    /// keeps it highlighted after the box closes.
    ax_seen: Option<String>,
    /// The last `read` came from Accessibility.
    pub from_accessibility: bool,
    /// The last `read` came from `PEEKME_SELECTION`, not the system.
    pub from_env: bool,
}

impl SelectionSource {
    pub fn new() -> Self {
        let mut s = Self {
            #[cfg(target_os = "linux")]
            clipboard: None,
            system: true,
            fixed: None,
            clipboard_only: cfg!(not(target_os = "linux")),
            seen: None,
            accessibility: cfg!(target_os = "macos")
                && std::env::var_os("SSH_CONNECTION").is_none(),
            ax_seen: None,
            from_accessibility: false,
            from_env: false,
        };
        // Whatever was copied before the agent started is no selection.
        s.settle();
        s
    }

    /// Only `PEEKME_SELECTION`, never the system's selection.
    pub fn without_system() -> Self {
        Self {
            #[cfg(target_os = "linux")]
            clipboard: None,
            system: false,
            fixed: None,
            clipboard_only: false,
            seen: None,
            accessibility: false,
            ax_seen: None,
            from_accessibility: false,
            from_env: false,
        }
    }

    /// Like a Mac's clipboard, holding `text`: it counts until `settle`.
    #[cfg(test)]
    pub fn fixed_clipboard(text: &str) -> Self {
        Self {
            fixed: Some(text.to_string()),
            clipboard_only: true,
            ..Self::without_system()
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
        self.from_env = false;
        if self.fixed.is_none()
            && let Ok(s) = std::env::var("PEEKME_SELECTION")
        {
            self.from_env = true;
            return Some(s).filter(|s| !s.trim().is_empty());
        }
        self.from_accessibility = false;
        let text = self.read_raw();
        if !(self.clipboard_only && text.is_some() && text == self.seen) && text.is_some() {
            return text;
        }
        let ax = self.read_accessibility();
        if ax.is_some() && ax != self.ax_seen {
            self.from_accessibility = true;
            return ax;
        }
        None
    }

    fn read_accessibility(&self) -> Option<String> {
        if !self.accessibility || !crate::macax::trusted() {
            return None;
        }
        let probe = crate::macax::probe();
        crate::event("accessibility", probe.fields());
        probe.text.filter(|t| !t.trim().is_empty())
    }

    /// The selection was used, or a key was typed with it there: from now on
    /// a clipboard counts only once something new is copied.
    pub fn settle(&mut self) {
        if self.clipboard_only {
            self.seen = self.read_raw();
        }
        if self.accessibility {
            self.ax_seen = self.read_accessibility();
        }
    }

    fn read_raw(&mut self) -> Option<String> {
        if let Some(s) = &self.fixed {
            return Some(s.clone());
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
        // GNOME on Wayland gives X11 programs no PRIMARY; its helper does.
        if crate::anyterm::gnome::session()
            && let Some(text) = crate::anyterm::gnome::primary()
        {
            return Some(text);
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

/// Blank for matching a selection with the screen: whitespace, and the
/// bullet Claude Code draws before an answer, which its copies leave out.
fn blank(c: char) -> bool {
    c.is_whitespace() || matches!(c, '\u{a0}' | '●' | '⏺')
}

/// Collapse blank runs (see `blank`) to one space; returns the text and, for
/// each output char, the index of the input char it came from.
pub fn normalize(chars: &[char]) -> (Vec<char>, Vec<usize>) {
    let mut out = Vec::with_capacity(chars.len());
    let mut map = Vec::with_capacity(chars.len());
    let mut in_space = false;
    for (i, &c) in chars.iter().enumerate() {
        if blank(c) {
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
    /// The selected text before and after the part on screen, when not all
    /// of it is there: scrolled away during the drag, or copied otherwise
    /// than drawn.
    pub unseen: (String, String),
}

impl Located {
    /// `screen` with the unseen parts of the selection put in place, so that
    /// the explainer gets all of what the user selected.
    pub fn whole(&self) -> crate::context::ScreenSel {
        let s = &self.screen;
        let (before, after) = &self.unseen;
        if before.is_empty() && after.is_empty() {
            return s.clone();
        }
        let mut text = s.text[..s.start].to_vec();
        text.extend(before.chars());
        text.extend(&s.text[s.start..s.end]);
        text.extend(after.chars());
        let end = text.len();
        text.extend(&s.text[s.end..]);
        crate::context::ScreenSel {
            text,
            start: s.start,
            end,
        }
    }
}

/// Find the selection on screen. With a `hint` (the cell where a mouse drag
/// ended) the occurrence touching that row wins; otherwise the one nearest
/// the bottom, which is what the user most likely just read.
pub fn locate(snap: &Snapshot, selection: &str, hint: Option<(usize, usize)>) -> Option<Located> {
    let (screen, pos) = snap.text_with_positions();
    let (norm_screen, map) = normalize(&screen);
    let sel: Vec<char> = selection.chars().collect();
    let (norm_sel, sel_map) = normalize(&sel);
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
    let len = norm_sel.len();
    // The found range on screen and the part of the selection it is.
    let (found, part) = match pick(find_all(&norm_screen, &norm_sel), len) {
        Some(f) => (f, 0..len),
        None => {
            // Part of a long selection may be off screen: anchor on its
            // visible end or start, then take as much more as agrees.
            let n = len.min(40);
            if len <= n {
                return None;
            }
            if let Some((s, e)) = pick(find_all(&norm_screen, &norm_sel[len - n..]), n) {
                let more = (1..=(len - n).min(s))
                    .take_while(|&k| norm_sel[len - n - k] == norm_screen[s - k])
                    .count();
                ((s - more, e), len - n - more..len)
            } else {
                let (s, e) = pick(find_all(&norm_screen, &norm_sel[..n]), n)?;
                let more = (1..=(len - n).min(norm_screen.len() - 1 - e))
                    .take_while(|&k| norm_sel[n - 1 + k] == norm_screen[e + k])
                    .count();
                ((s, e + more), 0..n + more)
            }
        }
    };
    let (start, last) = (map[found.0], map[found.1]);
    let unseen = (
        sel[..sel_map[part.start]].iter().collect::<String>(),
        sel[sel_map[part.end - 1] + 1..].iter().collect::<String>(),
    );
    Some(Located {
        first_row: pos[start].0,
        last_row: pos[last].0,
        screen: crate::context::ScreenSel {
            text: screen,
            start,
            end: last + 1,
        },
        unseen,
    })
}

/// Every place `text` appears on screen, each as the cells it covers: per
/// row, from its first to its last non-blank char (a wide char with its
/// second half).
pub fn occurrences(snap: &Snapshot, text: &str) -> Vec<Vec<(usize, usize)>> {
    copies(snap, text, 0).into_iter().map(|c| c.cells).collect()
}

/// One place `text` appears on screen, with up to `around` chars of the text
/// before and after it. Whitespace runs count as one space, so the context
/// does not depend on where lines wrap.
pub struct Copy {
    pub cells: Vec<(usize, usize)>,
    pub before: Vec<char>,
    pub after: Vec<char>,
}

pub fn copies(snap: &Snapshot, text: &str, around: usize) -> Vec<Copy> {
    Text::of(snap).copies(snap, text, around)
}

/// The screen's text, read once for several searches.
pub struct Text {
    screen: Vec<char>,
    pos: Vec<(usize, usize)>,
    norm: Vec<char>,
    map: Vec<usize>,
}

impl Text {
    pub fn of(snap: &Snapshot) -> Text {
        let (screen, pos) = snap.text_with_positions();
        let (norm, map) = normalize(&screen);
        Text {
            screen,
            pos,
            norm,
            map,
        }
    }

    /// [`copies`] on the screen this text was read from.
    pub fn copies(&self, snap: &Snapshot, text: &str, around: usize) -> Vec<Copy> {
        let Text {
            screen,
            pos,
            norm: norm_screen,
            map,
        } = self;
        let (norm_text, _) = normalize(&text.chars().collect::<Vec<_>>());
        if norm_text.is_empty() {
            return Vec::new();
        }
        find_all(norm_screen, &norm_text)
            .into_iter()
            .map(|s| {
                let end = s + norm_text.len();
                let mut cells: Vec<(usize, usize)> = Vec::new();
                for &i in &map[s..end] {
                    if blank(screen[i]) {
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
                Copy {
                    cells,
                    before: norm_screen[s.saturating_sub(around)..s].to_vec(),
                    after: norm_screen[end..(end + around).min(norm_screen.len())].to_vec(),
                }
            })
            .collect()
    }
}

/// `cells` (one occurrence, as `occurrences` gives it) as a located range.
pub fn at_cells(snap: &Snapshot, cells: &[(usize, usize)]) -> Option<Located> {
    let (screen, pos) = snap.text_with_positions();
    let (&first, &last) = (cells.first()?, cells.last()?);
    let start = pos.iter().position(|&p| p == first)?;
    // The last cell may be the second half of a wide char.
    let end = pos
        .iter()
        .rposition(|&p| p == last || p == (last.0, last.1.wrapping_sub(1)))?
        + 1;
    Some(Located {
        first_row: first.0,
        last_row: last.0,
        screen: crate::context::ScreenSel {
            text: screen,
            start,
            end,
        },
        unseen: Default::default(),
    })
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
    fn a_selection_longer_than_the_screen_is_all_of_its_rows_there() {
        let lines: Vec<String> = (0..9).map(|i| format!("row {i} of the diff")).collect();
        let s = snap(lines.join("\r\n").as_bytes(), 30, 4);
        // Scrolled away above the screen during the drag.
        let l = locate(&s, &lines[2..].join("\n"), None).unwrap();
        assert_eq!((l.first_row, l.last_row), (0, 3));
        assert_eq!(l.screen.selected(), lines[5..].join("\n"));
        assert_eq!(l.whole().selected(), lines[2..].join("\n"));
        // And below it: the copy runs on past the last row.
        let more = format!("{}\nrow 9 of the diff\nrow 10", lines[6..].join("\n"));
        let l = locate(&s, &more, None).unwrap();
        assert_eq!((l.first_row, l.last_row), (1, 3));
        assert_eq!(l.whole().selected(), more);
        assert_eq!(
            l.unseen,
            (String::new(), "\nrow 9 of the diff\nrow 10".to_string())
        );
    }

    #[test]
    fn claude_copies_leave_out_the_answer_bullet() {
        let s = snap(
            "❯ q0 show it\r\n\r\n● - first row\r\n  - second row".as_bytes(),
            30,
            5,
        );
        let copied = "❯ q0 show it\n\n- first row\n- second row";
        let l = locate(&s, copied, None).unwrap();
        assert_eq!((l.first_row, l.last_row, l.unseen.0.as_str()), (0, 3, ""));
        let cells = occurrences(&s, copied).remove(0);
        assert!(!cells.contains(&(2, 0)), "the bullet gets no underline");
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
