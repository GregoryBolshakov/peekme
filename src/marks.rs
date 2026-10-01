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

/// Screen cells, (row, column).
type Cells = Vec<(usize, usize)>;

/// Oldest marks are dropped past this many.
const MAX: usize = 100;

/// Chars of text kept on each side of a mark, to tell its copy of the words
/// from others.
const AROUND: usize = 48;
/// With the words on screen more than once, the text around a copy must
/// agree in at least this many chars for it to be the mark's copy.
const MIN_AGREE: usize = 4;
/// On the normal screen a mark normally stays at its row. After a resize the
/// rows rewrap: then a copy whose surroundings agree this much is the mark.
const REFLOW_AGREE: usize = 24;

pub struct Mark {
    pub text: String,
    pub answer: String,
    pub model: String,
    place: Place,
    /// The cells the terminal last got its underline on from us.
    drawn: Option<Vec<(usize, usize)>>,
}

/// Where a mark is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    /// On the alternate (full-screen) buffer or the normal one.
    alt: bool,
    /// First cell, the row counted from the top of history, so it stays put
    /// while the normal screen scrolls.
    anchor: (usize, usize),
    /// The text just before and after it, whitespace runs as one space.
    before: Vec<char>,
    after: Vec<char>,
    /// Its rows as they were drawn: the whole row text and the mark's columns
    /// in it. When only some of them are on screen, those still get the line.
    rows: Vec<Row>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Row {
    text: String,
    from: usize,
    to: usize,
}

/// A mark on screen.
#[derive(Debug, PartialEq, Eq)]
pub struct Visible {
    pub index: usize,
    pub text: String,
    pub cells: Vec<(usize, usize)>,
    /// The terminal may not have its line here: drawn elsewhere or not yet.
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
        let copy = select::copies(snap, text, AROUND)
            .into_iter()
            .find(|c| c.cells.first() == Some(&first))?;
        let place = Place {
            alt,
            anchor: (history + first.0, first.1),
            before: copy.before,
            after: copy.after,
            rows: rows_of(snap, &copy.cells),
        };
        Some((place, copy.cells))
    }

    /// How many chars of the text around `copy` agree with the mark's.
    fn agree(&self, copy: &select::Copy) -> usize {
        let back = self
            .before
            .iter()
            .rev()
            .zip(copy.before.iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let ahead = self
            .after
            .iter()
            .zip(&copy.after)
            .take_while(|(a, b)| a == b)
            .count();
        back + ahead
    }

    /// The mark's rows that are on `snap`, when not all of them are: the
    /// longest run of them, nearest to the old place among equals.
    fn partly(&self, snap: &Snapshot) -> Option<Vec<(usize, usize)>> {
        let n = self.rows.len();
        if n < 2 {
            return None;
        }
        let line = |r: usize| snap.rows_text(r, r + 1);
        let screen: Vec<String> = (0..snap.rows.len()).map(line).collect();
        // Rows of the run, distance from the old place, cells.
        let mut best: Option<(usize, usize, Cells)> = None;
        for (r, text) in screen.iter().enumerate() {
            for k in 0..n {
                if *text != self.rows[k].text || self.rows[k].text.trim().is_empty() {
                    continue;
                }
                // Start of a run: the row before it is not the mark's row before.
                if k > 0 && r > 0 && screen[r - 1] == self.rows[k - 1].text {
                    continue;
                }
                let mut j = k;
                while j + 1 < n
                    && r + (j + 1 - k) < screen.len()
                    && screen[r + j + 1 - k] == self.rows[j + 1].text
                {
                    j += 1;
                }
                if k == 0 && j == n - 1 {
                    continue;
                }
                let cells: Vec<_> = (k..=j)
                    .flat_map(|i| {
                        let row = r + i - k;
                        (self.rows[i].from..=self.rows[i].to).map(move |c| (row, c))
                    })
                    .collect();
                let len = j - k + 1;
                // Where its first row would be, above the screen when cut at the top.
                let dist = (r as isize - k as isize - self.anchor.0 as isize).unsigned_abs();
                if best
                    .as_ref()
                    .is_none_or(|(l, d, _)| len > *l || (len == *l && dist < *d))
                {
                    best = Some((len, dist, cells));
                }
            }
        }
        best.map(|(_, _, cells)| cells)
    }
}

/// Each row `cells` cover: its whole text and the columns covered.
fn rows_of(snap: &Snapshot, cells: &[(usize, usize)]) -> Vec<Row> {
    let mut rows: Vec<Row> = Vec::new();
    let mut last = None;
    for &(r, c) in cells {
        if last == Some(r) {
            if let Some(row) = rows.last_mut() {
                row.to = c;
            }
        } else {
            rows.push(Row {
                text: snap.rows_text(r, r + 1),
                from: c,
                to: c,
            });
            last = Some(r);
        }
    }
    rows
}

impl Marks {
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    pub fn add(&mut self, text: &str, answer: &str, model: &str, place: Place) {
        self.list
            .retain(|m| (m.place.alt, m.place.anchor) != (place.alt, place.anchor));
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

    /// The terminal now has the underline of mark `index` on `cells`.
    pub fn drawn(&mut self, index: usize, cells: &[(usize, usize)]) {
        if let Some(m) = self.list.get_mut(index) {
            m.drawn = Some(cells.to_vec());
        }
    }

    /// Cells that got our underline before and must lose it: those of marks
    /// that moved or left the screen (`now` is what `visible` found), except
    /// cells a mark has now. The child does not repaint them: in its own view
    /// they never changed.
    pub fn stale(&mut self, now: &[Visible]) -> Vec<(usize, usize)> {
        let keep: std::collections::HashSet<_> =
            now.iter().flat_map(|v| v.cells.iter().copied()).collect();
        let mut out = Vec::new();
        for (index, m) in self.list.iter_mut().enumerate() {
            let current = now.iter().find(|v| v.index == index).map(|v| &v.cells);
            if m.drawn.as_ref() != current
                && let Some(old) = m.drawn.take()
            {
                out.extend(old.into_iter().filter(|c| !keep.contains(c)));
            }
        }
        out.sort_unstable();
        out.dedup();
        out
    }

    /// The newest mark with this text, wherever it is.
    pub fn find(&self, text: &str) -> Option<&Mark> {
        let key = flat(text);
        self.list.iter().rev().find(|m| flat(&m.text) == key)
    }

    /// Every mark visible on `snap`: its text and its cells.
    ///
    /// On the normal screen a mark stays at its row (after a resize, where
    /// rows rewrap, at the copy whose surrounding text agrees). Full-screen
    /// agents scroll their own content, so there the copy whose surrounding
    /// text agrees most wins, the nearest among equals; with several copies
    /// and none that agrees, none. When the words are not all on screen, the
    /// rows of the mark that are still there count.
    pub fn visible(&mut self, snap: &Snapshot, history: usize, alt: bool) -> Vec<Visible> {
        let mut found = Vec::new();
        for (index, m) in self.list.iter_mut().enumerate() {
            if m.place.alt != alt {
                continue;
            }
            let (ar, ac) = m.place.anchor;
            let at = |c: &[(usize, usize)]| c.first().map(|&(r, c)| (history + r, c));
            let copies = select::copies(snap, &m.text, AROUND);
            let only = copies.len() == 1;
            let best = copies
                .into_iter()
                .map(|c| (m.place.agree(&c), c))
                .filter(|(agree, c)| {
                    if alt {
                        only || *agree >= MIN_AGREE
                    } else {
                        at(&c.cells) == Some(m.place.anchor) || *agree >= REFLOW_AGREE
                    }
                })
                .min_by_key(|(agree, c)| {
                    let (r, col) = at(&c.cells).unwrap_or_default();
                    (
                        at(&c.cells) != Some(m.place.anchor),
                        std::cmp::Reverse(*agree),
                        r.abs_diff(ar),
                        col.abs_diff(ac),
                    )
                });
            let cells = match best {
                Some((_, copy)) => {
                    m.place.anchor = at(&copy.cells).unwrap_or(m.place.anchor);
                    m.place.rows = rows_of(snap, &copy.cells);
                    m.place.before = copy.before;
                    m.place.after = copy.after;
                    copy.cells
                }
                None if alt => match m.place.partly(snap) {
                    Some(cells) => cells,
                    None => continue,
                },
                None => continue,
            };
            found.push(Visible {
                index,
                text: m.text.clone(),
                moved: m.drawn.as_ref() != Some(&cells),
                cells,
            });
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
    paint(out, snap, cells, Some(style));
}

/// Bytes that redraw `cells` as the child has them, without our line.
pub fn restore(out: &mut String, snap: &Snapshot, cells: &[(usize, usize)]) {
    paint(out, snap, cells, None);
}

fn paint(out: &mut String, snap: &Snapshot, cells: &[(usize, usize)], style: Option<Flags>) {
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
                if let Some(style) = style {
                    cell.flags.remove(Flags::ALL_UNDERLINES);
                    cell.flags.insert(style);
                }
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

    fn alt(cols: u16, rows: u16, lines: &[&str]) -> Snapshot {
        let mut b = b"\x1b[?1049h".to_vec();
        for (i, l) in lines.iter().enumerate() {
            b.extend(format!("\x1b[{};1H{l}", i + 1).bytes());
        }
        let mut s = Shadow::new(cols, rows);
        s.advance(&b);
        s.snapshot()
    }

    #[test]
    fn rows_of_a_mark_still_on_screen_keep_their_line() {
        let text = "the quick brown fox jumps over";
        let page = [
            "intro",
            "a line with the quick brown",
            "fox jumps over the dog",
            "outro",
        ];
        let snap = alt(30, 6, &page);
        let (place, cells) = Place::new(&snap, text, (1, 12), 0, true).unwrap();
        assert_eq!(cells.last(), Some(&(2, 13)));
        let mut marks = Marks::default();
        marks.add(text, "x", "m", place);

        // Scrolled up by two rows: only the second row of the mark is left.
        let snap = alt(30, 6, &page[2..]);
        let v = marks.visible(&snap, 0, true);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].cells, (0..=13).map(|c| (0, c)).collect::<Vec<_>>());
        // Scrolled down: only the first row, now at the bottom.
        let snap = alt(30, 6, &["", "", "", "", "", "a line with the quick brown"]);
        let v = marks.visible(&snap, 0, true);
        assert_eq!(v[0].cells, (12..=26).map(|c| (5, c)).collect::<Vec<_>>());
        // Back in full.
        let snap = alt(30, 6, &page);
        assert_eq!(marks.visible(&snap, 0, true)[0].cells, cells);
    }

    #[test]
    fn a_mark_is_found_again_after_its_rows_rewrap() {
        let text = "pseudo-terminal";
        let wide = [
            "> what is a pseudo-terminal",
            "- the pseudo-terminal layer: tmux gives each pane one",
        ];
        let snap = alt(60, 6, &wide);
        let (place, _) = Place::new(&snap, text, (1, 6), 0, true).unwrap();
        let mut marks = Marks::default();
        marks.add(text, "x", "m", place);
        // Narrower: the answer's line wraps, the question stays on row 0.
        let narrow = [
            "> what is a pseudo-terminal",
            "- the",
            "  pseudo-terminal layer: tmux",
            "  gives each pane one",
        ];
        let v = marks.visible(&alt(30, 6, &narrow), 0, true);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].cells[0], (2, 2), "the answer's copy");

        // The normal screen: the row number changes when lines rewrap.
        let mut s = Shadow::new(60, 4);
        s.advance(b"first\r\n- the pseudo-terminal layer: tmux gives each pane one");
        let (place, _) = Place::new(&s.snapshot(), text, (1, 6), s.history_size(), false).unwrap();
        let mut marks = Marks::default();
        marks.add(text, "x", "m", place);
        let mut s = Shadow::new(30, 4);
        s.advance(b"first\r\n- the\r\n  pseudo-terminal layer: tmux\r\n  gives each pane one");
        let v = marks.visible(&s.snapshot(), s.history_size(), false);
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].cells[0], (2, 2));
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
        marks.drawn(v[0].index, &v[0].cells);
        assert!(!marks.visible(&snap, 0, true)[0].moved);

        // The row changed and two copies are on screen: show none, not the wrong one.
        let mut s = screen(7);
        s.advance(b"\x1b[8;1H\x1b[2K- a pseudo-terminal, rewritten");
        assert!(marks.visible(&s.snapshot(), 0, true).is_empty());
    }
}
