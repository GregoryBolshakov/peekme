//! Dotted underlines under text that already got an explanation, so the user
//! sees what they asked about before. Selecting it again shows the saved answer.
//!
//! The terminal keeps the underline until the child writes over those cells,
//! so after the child's output settles peekme draws it again where needed.

use std::fmt::Write;

use alacritty_terminal::term::cell::Flags;

use crate::render;
use crate::select;
use crate::shadow::Snapshot;

/// Oldest marks are dropped past this many.
const MAX: usize = 100;

pub struct Mark {
    pub text: String,
    pub answer: String,
    pub model: String,
    place: Place,
}

/// Where a mark is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    /// On the alternate (full-screen) buffer or the normal one.
    alt: bool,
    /// First cell, the row counted from the top of history, so it stays put
    /// while the normal screen scrolls.
    anchor: (usize, usize),
}

#[derive(Default)]
pub struct Marks {
    list: Vec<Mark>,
}

impl Place {
    /// The place of the occurrence of `text` that starts at screen cell `first`.
    pub fn new(
        snap: &Snapshot,
        text: &str,
        first: (usize, usize),
        history: usize,
        alt: bool,
    ) -> Option<(Place, Vec<(usize, usize)>)> {
        let cells = select::occurrences(snap, text)
            .into_iter()
            .find(|c| c.first() == Some(&first))?;
        let place = Place {
            alt,
            anchor: (history + first.0, first.1),
        };
        Some((place, cells))
    }
}

impl Marks {
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn add(&mut self, text: &str, answer: &str, model: &str, place: Place) {
        self.list.retain(|m| m.place != place);
        self.list.push(Mark {
            text: text.into(),
            answer: answer.into(),
            model: model.into(),
            place,
        });
        if self.list.len() > MAX {
            self.list.remove(0);
        }
    }

    /// The newest mark with this text, wherever it is.
    pub fn find(&self, text: &str) -> Option<&Mark> {
        let key = flat(text);
        self.list.iter().rev().find(|m| flat(&m.text) == key)
    }

    /// Text and cells of every mark visible on `snap`. On the normal screen a mark
    /// only counts at its own place. Full-screen agents scroll their own
    /// content, so there the occurrence nearest to the old place wins and
    /// becomes the new place.
    pub fn visible(
        &mut self,
        snap: &Snapshot,
        history: usize,
        alt: bool,
    ) -> Vec<(String, Vec<(usize, usize)>)> {
        let mut found = Vec::new();
        for m in self.list.iter_mut().filter(|m| m.place.alt == alt) {
            let (ar, ac) = m.place.anchor;
            let at = |c: &Vec<(usize, usize)>| c.first().map(|&(r, c)| (history + r, c));
            let best = select::occurrences(snap, &m.text)
                .into_iter()
                .filter(|c| alt || at(c) == Some(m.place.anchor))
                .min_by_key(|c| at(c).map(|(r, c)| (r.abs_diff(ar), c.abs_diff(ac))));
            if let Some(cells) = best {
                m.place.anchor = at(&cells).unwrap_or(m.place.anchor);
                found.push((m.text.clone(), cells));
            }
        }
        found
    }
}

/// Whitespace runs as one space, like the search on screen.
fn flat(s: &str) -> Vec<char> {
    select::normalize(&s.chars().collect::<Vec<_>>()).0
}

/// Bytes that redraw `cells` of `snap` with a dotted underline, or a solid one
/// while the pointer is over the mark. The caller puts the cursor and the pen
/// back.
pub fn draw(out: &mut String, snap: &Snapshot, cells: &[(usize, usize)], hover: bool) {
    let style = if hover {
        Flags::UNDERLINE
    } else {
        Flags::DOTTED_UNDERLINE
    };
    let mut i = 0;
    while i < cells.len() {
        let (r, c0) = cells[i];
        let mut c1 = c0;
        while i + 1 < cells.len() && cells[i + 1] == (r, c1 + 1) {
            i += 1;
            c1 += 1;
        }
        i += 1;
        let Some(row) = snap.rows.get(r) else {
            continue;
        };
        let styled: Vec<_> = row[c0..=c1.min(row.len() - 1)]
            .iter()
            .map(|cell| {
                let mut cell = cell.clone();
                cell.flags.remove(Flags::ALL_UNDERLINES);
                cell.flags.insert(style);
                cell
            })
            .collect();
        write!(out, "\x1b[{};{}H", r + 1, c0 + 1).unwrap();
        render::cells(out, &styled);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shadow::Shadow;

    fn shadow(bytes: &[u8]) -> Shadow {
        let mut s = Shadow::new(20, 4);
        s.advance(bytes);
        s
    }

    #[test]
    fn a_mark_stays_with_its_text_while_the_screen_scrolls() {
        let mut s = shadow(b"one ripgrep\r\ntwo\r\nripgrep three");
        let snap = s.snapshot();
        let (place, cells) = Place::new(&snap, "ripgrep", (0, 4), s.history_size(), false).unwrap();
        assert_eq!(cells, (4..11).map(|c| (0, c)).collect::<Vec<_>>());
        let mut marks = Marks::default();
        marks.add("ripgrep", "a search tool", "haiku", place);
        assert_eq!(
            marks.visible(&snap, s.history_size(), false),
            vec![("ripgrep".to_string(), cells)]
        );

        // Two lines scroll off: the mark is gone, not moved to the other ripgrep.
        s.advance(b"\r\nfour\r\nfive\r\nsix");
        let snap = s.snapshot();
        assert_eq!(snap.rows_text(0, 1), "ripgrep three\n");
        assert!(marks.visible(&snap, s.history_size(), false).is_empty());
        assert_eq!(marks.find("ripgrep").unwrap().answer, "a search tool");
        assert!(marks.find("rip").is_none());
    }

    #[test]
    fn full_screen_marks_follow_the_nearest_copy() {
        let s = shadow(b"\x1b[?1049h\x1b[2;1Hsee the  PTY  here");
        let snap = s.snapshot();
        let (place, _) = Place::new(&snap, "the PTY", (1, 4), 0, true).unwrap();
        let mut marks = Marks::default();
        marks.add("the PTY", "x", "m", place);
        // The agent scrolled its own view by one row.
        let s = shadow(b"\x1b[?1049h\x1b[1;1Hsee the  PTY  here");
        let snap = s.snapshot();
        let cells: Vec<_> = marks
            .visible(&snap, 0, true)
            .into_iter()
            .map(|(_, c)| c)
            .collect();
        assert_eq!(cells, vec![(4..12).map(|c| (0, c)).collect::<Vec<_>>()]);
        assert!(marks.visible(&snap, 0, false).is_empty());

        let mut out = String::new();
        draw(&mut out, &snap, &cells[0], false);
        assert!(out.starts_with("\x1b[1;5H"));
        assert!(out.contains(";4:4"));
        assert!(out.contains("the  PTY"));
    }
}
