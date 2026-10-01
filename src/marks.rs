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
    /// Where the terminal last got its underline from us.
    drawn: Option<(usize, usize)>,
}

/// Where a mark is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    /// On the alternate (full-screen) buffer or the normal one.
    alt: bool,
    /// First cell, the row counted from the top of history, so it stays put
    /// while the normal screen scrolls.
    anchor: (usize, usize),
    /// The rows around it, to tell it from other copies of the same words
    /// when a full-screen agent scrolls or expands something above it.
    context: Context,
}

/// The text of a mark's own rows and of the rows just above and below.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Context {
    above: String,
    own: String,
    below: String,
}

impl Context {
    fn of(snap: &Snapshot, cells: &[(usize, usize)]) -> Context {
        let (first, last) = (cells[0].0, cells[cells.len() - 1].0);
        Context {
            above: first
                .checked_sub(1)
                .map_or_else(String::new, |r| snap.rows_text(r, r + 1)),
            own: snap.rows_text(first, last + 1),
            below: snap.rows_text(last + 1, last + 2),
        }
    }

    /// How well `cells` on `snap` fit: None when its own rows differ.
    fn fit(&self, snap: &Snapshot, cells: &[(usize, usize)]) -> Option<usize> {
        let here = Context::of(snap, cells);
        (here.own == self.own)
            .then(|| usize::from(here.above == self.above) + usize::from(here.below == self.below))
    }
}

/// A mark on screen.
#[derive(Debug, PartialEq, Eq)]
pub struct Visible {
    pub index: usize,
    pub text: String,
    pub cells: Vec<(usize, usize)>,
    /// It is not where it was last drawn.
    pub moved: bool,
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
            context: Context::of(snap, &cells),
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
            drawn: None,
        });
        if self.list.len() > MAX {
            self.list.remove(0);
        }
    }

    /// The terminal now has the underline of mark `index` where it is.
    pub fn drawn(&mut self, index: usize) {
        if let Some(m) = self.list.get_mut(index) {
            m.drawn = Some(m.place.anchor);
        }
    }

    /// The newest mark with this text, wherever it is.
    pub fn find(&self, text: &str) -> Option<&Mark> {
        let key = flat(text);
        self.list.iter().rev().find(|m| flat(&m.text) == key)
    }

    /// Every mark visible on `snap`: its text, its cells, and whether it is
    /// not where it was last drawn (then the terminal may not have its line).
    /// On the normal screen a mark only counts at its own place. Full-screen
    /// agents scroll their own content, so there the copy whose rows look like
    /// the mark's rows wins, the nearest one among equals. When several
    /// copies are on screen and none fits, the mark is not shown rather than
    /// put on the wrong copy.
    pub fn visible(&mut self, snap: &Snapshot, history: usize, alt: bool) -> Vec<Visible> {
        let mut found = Vec::new();
        for (index, m) in self.list.iter_mut().enumerate() {
            if m.place.alt != alt {
                continue;
            }
            let (ar, ac) = m.place.anchor;
            let at = |c: &Vec<(usize, usize)>| c.first().map(|&(r, c)| (history + r, c));
            let copies: Vec<_> = select::occurrences(snap, &m.text)
                .into_iter()
                .filter(|c| alt || at(c) == Some(m.place.anchor))
                .collect();
            let only = copies.len() == 1;
            let best = copies
                .into_iter()
                .filter_map(|c| {
                    let fit = m.place.context.fit(snap, &c);
                    (fit.is_some() || only).then_some((fit, c))
                })
                .min_by_key(|(fit, c)| {
                    let (r, col) = at(c).unwrap_or_default();
                    (std::cmp::Reverse(*fit), r.abs_diff(ar), col.abs_diff(ac))
                });
            if let Some((_, cells)) = best {
                m.place.anchor = at(&cells).unwrap_or(m.place.anchor);
                m.place.context = Context::of(snap, &cells);
                found.push(Visible {
                    index,
                    text: m.text.clone(),
                    cells,
                    moved: m.drawn != Some(m.place.anchor),
                });
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
        let v = marks.visible(&snap, s.history_size(), false);
        assert_eq!((v[0].text.as_str(), &v[0].cells), ("ripgrep", &cells));

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
            .map(|v| v.cells)
            .collect();
        assert_eq!(cells, vec![(4..12).map(|c| (0, c)).collect::<Vec<_>>()]);
        assert!(marks.visible(&snap, 0, false).is_empty());

        let mut out = String::new();
        draw(&mut out, &snap, &cells[0], false);
        assert!(out.starts_with("\x1b[1;5H"));
        assert!(out.contains(";4:4"));
        assert!(out.contains("the  PTY"));
    }

    #[test]
    fn a_full_screen_mark_keeps_to_its_copy_when_another_is_closer() {
        // The words are in the question (top row, which the agent pins) and
        // in the answer, which is marked.
        let screen = |answer_row: usize| {
            let mut b = b"\x1b[?1049h\x1b[1;1H> what is a pseudo-terminal".to_vec();
            b.extend(format!("\x1b[{};1Hanswer line", answer_row).bytes());
            b.extend(format!("\x1b[{};1H- the pseudo-terminal layer", answer_row + 1).bytes());
            b.extend(format!("\x1b[{};1Hmore text", answer_row + 2).bytes());
            let mut s = Shadow::new(40, 12);
            s.advance(&b);
            s
        };
        let s = screen(3);
        let snap = s.snapshot();
        let (place, _) = Place::new(&snap, "pseudo-terminal", (3, 6), 0, true).unwrap();
        let mut marks = Marks::default();
        marks.add("pseudo-terminal", "x", "m", place);
        // Scrolled: the answer is 3 rows lower, the question copy is now nearer
        // to the old place.
        let snap = screen(7).snapshot();
        let v = marks.visible(&snap, 0, true);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].cells[0], (7, 6), "still the answer's copy");
        assert!(v[0].moved);
        marks.drawn(v[0].index);
        assert!(!marks.visible(&snap, 0, true)[0].moved);

        // The row changed and two copies are on screen: show none, not the wrong one.
        let mut s = screen(7);
        s.advance(b"\x1b[8;1H\x1b[2K- a pseudo-terminal, rewritten");
        assert!(marks.visible(&s.snapshot(), 0, true).is_empty());
    }
}
