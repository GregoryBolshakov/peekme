//! Dotted underlines under text that already got an explanation, so the user
//! sees what they asked about before. Selecting it again shows the saved answer.
//!
//! The terminal keeps the underline until the child writes over those cells,
//! so after the child's output settles peekme draws it again where needed.
//!
//! Saved answers are a tree: a peek opened on words inside an answer is that
//! answer's child. The marks on screen point to the top answers.

use std::collections::BTreeMap;
use std::fmt::Write;
use std::ops::Range;

use alacritty_terminal::term::cell::Flags;

use crate::render;
use crate::select;
use crate::shadow::Snapshot;

/// Screen cells, (row, column).
type Cells = Vec<(usize, usize)>;

/// Oldest marks are dropped past this many.
const MAX: usize = 100;
/// Oldest top answers, with everything under them, are dropped past this
/// many answers in all.
const MAX_ANSWERS: usize = 300;

/// Chars of text kept on each side of a mark, to tell its copy of the words
/// from others.
const AROUND: usize = 48;
/// With the words on screen more than once, the text around a copy must
/// agree in at least this many chars for it to be the mark's copy.
const MIN_AGREE: usize = 4;
/// On the normal screen a mark normally stays at its row. After a resize the
/// rows rewrap: then a copy whose surroundings agree this much is the mark.
const REFLOW_AGREE: usize = 24;

/// Which saved answer. Newer answers have bigger ids.
pub type AnswerId = u64;

/// A saved explanation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Answer {
    /// The words it explains.
    pub text: String,
    pub answer: String,
    pub model: String,
    /// The answer whose words these are; none for words on the screen.
    pub parent: Option<AnswerId>,
    /// Answers to words of this one, oldest first.
    pub children: Vec<Child>,
}

/// A nested answer and the places in its parent's text it was asked about.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Child {
    pub id: AnswerId,
    /// Char ranges of the parent's `answer`.
    pub ranges: Vec<Range<usize>>,
}

/// A mark on screen.
struct Mark {
    answer: AnswerId,
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
    answers: BTreeMap<AnswerId, Answer>,
    next: AnswerId,
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

    /// Save an answer to `text`, words on the screen when `parent` is none,
    /// else words of that answer. Returns its id.
    pub fn save(
        &mut self,
        parent: Option<AnswerId>,
        text: &str,
        answer: &str,
        model: &str,
    ) -> AnswerId {
        self.next += 1;
        let id = self.next;
        self.answers.insert(
            id,
            Answer {
                text: text.into(),
                answer: answer.into(),
                model: model.into(),
                parent,
                children: Vec::new(),
            },
        );
        if let Some(p) = parent.and_then(|p| self.answers.get_mut(&p)) {
            p.children.push(Child {
                id,
                ranges: Vec::new(),
            });
        }
        while self.answers.len() > MAX_ANSWERS {
            let Some(oldest) = self
                .answers
                .iter()
                .find(|(_, a)| a.parent.is_none())
                .map(|(&id, _)| id)
            else {
                break;
            };
            self.remove(oldest);
        }
        id
    }

    /// Drop an answer, its children and the marks of all of them.
    fn remove(&mut self, id: AnswerId) {
        let Some(a) = self.answers.remove(&id) else {
            return;
        };
        if let Some(p) = a.parent.and_then(|p| self.answers.get_mut(&p)) {
            p.children.retain(|c| c.id != id);
        }
        self.list.retain(|m| m.answer != id);
        for c in a.children {
            self.remove(c.id);
        }
    }

    /// A new answer to the same words, from more context: what was asked
    /// about the old text no longer fits it, so its children go.
    pub fn replace(&mut self, id: AnswerId, answer: &str, model: &str) {
        let Some(a) = self.answers.get_mut(&id) else {
            return;
        };
        a.answer = answer.into();
        a.model = model.into();
        for c in std::mem::take(&mut a.children) {
            self.remove(c.id);
        }
    }

    pub fn answer(&self, id: AnswerId) -> Option<&Answer> {
        self.answers.get(&id)
    }

    /// Mark the words of answer `id` at `place` on the screen.
    pub fn add(&mut self, id: AnswerId, place: Place) {
        self.list
            .retain(|m| (m.place.alt, m.place.anchor) != (place.alt, place.anchor));
        self.list.push(Mark {
            answer: id,
            place,
            drawn: None,
        });
        if self.list.len() > MAX {
            self.list.remove(0);
        }
    }

    /// Answer `child` was asked about the chars `range` of its parent too.
    pub fn add_range(&mut self, child: AnswerId, range: Range<usize>) {
        let Some(parent) = self.answers.get(&child).and_then(|a| a.parent) else {
            return;
        };
        if let Some(c) = self
            .answers
            .get_mut(&parent)
            .and_then(|p| p.children.iter_mut().find(|c| c.id == child))
            && !c.ranges.contains(&range)
        {
            c.ranges.push(range);
        }
    }

    /// Where answer `id` underlines the words of its children: each range with
    /// the child it opens.
    pub fn child_ranges(&self, id: AnswerId) -> Vec<(Range<usize>, AnswerId)> {
        self.answers.get(&id).map_or_else(Vec::new, |a| {
            a.children
                .iter()
                .flat_map(|c| c.ranges.iter().map(|r| (r.clone(), c.id)))
                .collect()
        })
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

    /// The newest answer to these words on the screen, wherever they are.
    pub fn find(&self, text: &str) -> Option<AnswerId> {
        self.find_under(None, text)
    }

    /// The newest answer to these words of answer `parent`.
    pub fn find_child(&self, parent: AnswerId, text: &str) -> Option<AnswerId> {
        self.find_under(Some(parent), text)
    }

    fn find_under(&self, parent: Option<AnswerId>, text: &str) -> Option<AnswerId> {
        let key = flat(text);
        self.answers
            .iter()
            .rev()
            .find(|(_, a)| a.parent == parent && flat(&a.text) == key)
            .map(|(&id, _)| id)
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
        let screen = select::Text::of(snap);
        for (index, m) in self.list.iter_mut().enumerate() {
            let Some(text) = self.answers.get(&m.answer).map(|a| &a.text) else {
                continue;
            };
            if m.place.alt != alt {
                continue;
            }
            let (ar, ac) = m.place.anchor;
            let at = |c: &[(usize, usize)]| c.first().map(|&(r, c)| (history + r, c));
            let copies = screen.copies(snap, text, AROUND);
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
                text: text.clone(),
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

    fn mark(marks: &mut Marks, text: &str, answer: &str, model: &str, place: Place) -> AnswerId {
        let id = marks.save(None, text, answer, model);
        marks.add(id, place);
        id
    }

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
        mark(&mut marks, "ripgrep", "a search tool", "haiku", place);
        let v = marks.visible(&snap, s.history_size(), false);
        assert_eq!((v[0].text.as_str(), &v[0].cells), ("ripgrep", &cells));

        // Two lines scroll off: the mark is gone, not moved to the other ripgrep.
        s.advance(b"\r\nfour\r\nfive\r\nsix");
        let snap = s.snapshot();
        assert_eq!(snap.rows_text(0, 1), "ripgrep three\n");
        assert!(marks.visible(&snap, s.history_size(), false).is_empty());
        let id = marks.find("ripgrep").unwrap();
        assert_eq!(marks.answer(id).unwrap().answer, "a search tool");
        assert!(marks.find("rip").is_none());
    }

    #[test]
    fn full_screen_marks_follow_the_nearest_copy() {
        let s = shadow(b"\x1b[?1049h\x1b[2;1Hsee the  PTY  here");
        let snap = s.snapshot();
        let (place, _) = Place::new(&snap, "the PTY", (1, 4), 0, true).unwrap();
        let mut marks = Marks::default();
        mark(&mut marks, "the PTY", "x", "m", place);
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
        mark(&mut marks, text, "x", "m", place);

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
        mark(&mut marks, text, "x", "m", place);
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
        mark(&mut marks, text, "x", "m", place);
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
        mark(&mut marks, "pseudo-terminal", "x", "m", place);
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

    #[test]
    fn nested_answers_are_found_under_their_parent_only() {
        let mut marks = Marks::default();
        let top = marks.save(None, "the PTY", "a pseudo-terminal: job control works", "m");
        let job = marks.save(Some(top), "job control", "Ctrl+Z, fg, bg", "m");
        marks.add_range(job, 19..30);
        marks.add_range(job, 19..30);
        assert_eq!(marks.find("the  PTY"), Some(top));
        // Nested words are not words on the screen, and the other way round.
        assert_eq!(marks.find("job control"), None);
        assert_eq!(marks.find_child(top, "job\ncontrol"), Some(job));
        assert_eq!(marks.find_child(job, "job control"), None);
        assert_eq!(marks.child_ranges(top), vec![(19..30, job)]);

        // Two levels down, then the top answer is explained again: what was
        // asked about its old text goes, all the way down.
        let fg = marks.save(Some(job), "fg", "foreground", "m");
        marks.replace(top, "new text", "deep");
        assert_eq!(marks.answer(top).unwrap().answer, "new text");
        assert!(marks.child_ranges(top).is_empty());
        assert!(marks.answer(job).is_none() && marks.answer(fg).is_none());
    }

    #[test]
    fn the_oldest_tree_goes_first_with_its_marks() {
        let s = shadow(b"one ripgrep");
        let snap = s.snapshot();
        let mut marks = Marks::default();
        let (place, _) = Place::new(&snap, "ripgrep", (0, 4), 0, false).unwrap();
        let first = mark(&mut marks, "ripgrep", "a", "m", place);
        let child = marks.save(Some(first), "a", "b", "m");
        for i in 0..MAX_ANSWERS - 2 {
            marks.save(None, &format!("w{i}"), "x", "m");
        }
        assert!(marks.answer(child).is_some());
        assert_eq!(marks.visible(&snap, 0, false).len(), 1);
        marks.save(None, "one more", "x", "m");
        assert!(marks.answer(first).is_none() && marks.answer(child).is_none());
        assert!(marks.is_empty());
        assert_eq!(marks.answers.len(), MAX_ANSWERS - 1);
    }
}
