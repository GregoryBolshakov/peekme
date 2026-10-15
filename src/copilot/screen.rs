//! GitHub Copilot CLI's full screen (1.0.98): a tab bar on the top row and a
//! blank row under it, the chat with a scrollbar in the last column, a blank
//! row, a status line, the input box drawn with half blocks (`╻▄▄…`, `┃ text`,
//! `╹▀▀…`) and a line of hints. Copilot 1.0.95 and older drew the input box
//! like Claude Code's: `❯` between two rules.

use std::ops::Range;

use crate::claude;
use crate::shadow::Snapshot;

/// The cell Copilot draws its scrollbar with, track and thumb alike (they
/// differ in colour only).
const BAR: char = '┃';

/// The column of the divider (`│`, down every row) right of Copilot's
/// sidebar, when it is open (the left arrow opens it): the sidebar's rows
/// and the divider are not the chat, which starts a column further right.
pub fn sidebar(snap: &Snapshot) -> Option<usize> {
    let cols = snap.cols;
    (8..cols / 2).find(|&c| {
        !snap.rows.is_empty()
            && snap
                .rows
                .iter()
                .all(|row| row.get(c).is_some_and(|x| x.c == '│'))
    })
}

/// First column of the chat (and of the input box).
fn left(snap: &Snapshot) -> usize {
    sidebar(snap).map_or(0, |c| c + 1)
}

/// First row of the input box: the lowest `╻` followed by a full row of `▄`.
pub fn composer_top(snap: &Snapshot) -> Option<usize> {
    let rows = snap.rows.len();
    let left = left(snap);
    (rows / 3..rows)
        .rev()
        .find(|&r| {
            let row = &snap.rows[r][left.min(snap.rows[r].len())..];
            let blocks = row.iter().filter(|c| c.c == '▄').count();
            row.first().is_some_and(|c| c.c == '╻') && blocks * 5 >= row.len() * 4
        })
        .or_else(|| claude::screen::composer_top(snap))
}

/// The rows Copilot's scrollbar runs down (its chat's rows): the lowest run
/// of rows above `composer` whose last cell is the bar. None while the whole
/// chat fits on the screen (Copilot draws no scrollbar then).
pub fn scrollbar(snap: &Snapshot, composer: usize) -> Option<Range<usize>> {
    let bar = |r: usize| snap.rows[r].last().is_some_and(|c| c.c == BAR);
    let end = (0..composer.min(snap.rows.len())).rev().find(|&r| bar(r))? + 1;
    let start = (0..end).rev().find(|&r| !bar(r)).map_or(0, |r| r + 1);
    // A table's border in the last column is not a scrollbar.
    (end - start >= 3).then_some(start..end)
}

/// `snap` with only the chat's text: Copilot's scrollbar and its sidebar
/// (with the divider) blanked. Copilot copies a selection without them,
/// and neither moves with the chat. A scrollbar's rows are found as in
/// `scrollbar`, above the input box when there is one.
pub fn chat_text(snap: &Snapshot) -> Snapshot {
    let mut out = snap.clone();
    let below = composer_top(snap).unwrap_or(snap.rows.len());
    if let Some(bar) = scrollbar(snap, below) {
        for r in bar {
            if let Some(cell) = out.rows[r].last_mut() {
                *cell = Default::default();
            }
        }
    }
    if let Some(c) = sidebar(snap) {
        for row in &mut out.rows {
            for cell in row.iter_mut().take(c + 1) {
                *cell = Default::default();
            }
        }
    }
    out
}

/// Columns of `snap` that stay put while the chat scrolls: the sidebar
/// with its divider, and the scrollbar (the last column).
pub fn fixed_columns(snap: &Snapshot) -> (usize, bool) {
    let below = composer_top(snap).unwrap_or(snap.rows.len());
    (left(snap), scrollbar(snap, below).is_some())
}

/// The rows of Copilot's chat, its input box starting at `composer`: the
/// scrollbar's rows, or with none, those between the tab bar (and the blank
/// row under it) and the blank row and the status line above the input box.
pub fn chat_rows(snap: &Snapshot, composer: usize) -> Range<usize> {
    if let Some(bar) = scrollbar(snap, composer) {
        return bar;
    }
    let top = if tab_bar(snap) { 2 } else { 0 };
    top..composer.saturating_sub(2).max(top)
}

/// Copilot's tabs ("Current   Sessions   Issues …") on the top row, a blank
/// row under them.
fn tab_bar(snap: &Snapshot) -> bool {
    snap.rows.len() > 2
        && snap.rows_text(0, 1).contains("Sessions")
        && snap.rows_text(1, 2).trim().is_empty()
}

/// Nothing is typed in the input box starting at `composer`: its one text
/// row (`┃` and the text) is blank.
pub fn input_empty(snap: &Snapshot, composer: usize) -> bool {
    let (row, left) = (composer + 1, left(snap));
    let text = |r: usize| -> String { snap.rows[r].iter().skip(left).map(|c| c.c).collect() };
    row + 1 < snap.rows.len()
        && text(row + 1).starts_with('╹')
        && text(row).trim_start_matches(BAR).trim().is_empty()
}

/// The scrollbar's thumb is neither at its top nor at its bottom: the bar
/// shows three runs of colour (track, thumb, track). Copilot's chat is then
/// surely at neither end. A thumb at an end is only a hint: it stays there
/// for a few rows of scrolling in a long chat.
pub fn thumb_inside(snap: &Snapshot, rows: Range<usize>) -> bool {
    let cells: Vec<_> = rows
        .filter_map(|r| snap.rows.get(r)?.last())
        .filter(|c| c.c == BAR)
        .collect();
    let runs = 1 + cells.windows(2).filter(|w| w[0].fg != w[1].fg).count();
    runs >= 3
}

/// How many rows (a whole number of turns, 3 rows each, at most `room`) the
/// chat `rows` can move `way` (down 1, up -1) in one frame and still show
/// how far they moved. A frame that moved `k` rows reads the same as one
/// that moved `k ± d` when the rows staying on the screen repeat every `d`
/// rows (a block of rows all alike, say); the rows coming in may repeat too.
/// One turn always tells.
pub fn readable_move(snap: &Snapshot, rows: Range<usize>, way: isize, room: usize) -> usize {
    let mut text: Vec<String> = rows
        .filter(|&r| r < snap.rows.len())
        .map(|r| snap.rows_text(r, r + 1).trim_end().to_string())
        .collect();
    if way < 0 {
        text.reverse();
    }
    let n = text.len();
    let mut r = room.min(n.saturating_sub(4)) / 3 * 3;
    while r > 3 {
        // Moves `r - d` and `r` both: the rows from `r - d` on, staying
        // either way, must not repeat every `d` rows.
        let alike = (3..r)
            .step_by(3)
            .any(|d| (r - d..n - d).all(|j| text[j] == text[j + d]));
        if !alike {
            return r;
        }
        r -= 3;
    }
    3
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shadow::Shadow;

    /// A Copilot 1.0.98 screen, 40 columns by 12 rows, its thumb on chat
    /// rows `thumb` (of 2..8).
    fn screen(thumb: Range<usize>, bar: bool) -> Snapshot {
        let mut s = Shadow::new(40, 12);
        let mut b = String::from("\x1b[1;1H  Current   Sessions   Issues");
        for r in 2..8 {
            b.push_str(&format!("\x1b[{};1H   - row {r}", r + 1));
            if bar {
                let rgb = if thumb.contains(&r) {
                    "240;246;252"
                } else {
                    "145;152;161"
                };
                b.push_str(&format!("\x1b[{};40H\x1b[38;2;{rgb}m\u{2503}\x1b[m", r + 1));
            }
        }
        b.push_str("\x1b[10;1H ~/project [main]");
        b.push_str(&format!("\x1b[11;1H\u{257b}{}", "\u{2584}".repeat(39)));
        b.push_str("\x1b[12;1H\u{2503}");
        s.advance(b.as_bytes());
        s.snapshot()
    }

    #[test]
    fn finds_the_input_box_and_the_chat() {
        let snap = screen(5..8, true);
        assert_eq!(composer_top(&snap), Some(10));
        assert_eq!(scrollbar(&snap, 10), Some(2..8));
        assert_eq!(chat_rows(&snap, 10), 2..8);
        // No scrollbar (the chat fits): under the tab bar, above the gap and
        // the status line.
        let snap = screen(0..0, false);
        assert_eq!(scrollbar(&snap, 10), None);
        assert_eq!(chat_rows(&snap, 10), 2..8);
    }

    /// A screen whose lower rows are all alike: moves of 9 and 12 rows look
    /// the same there; above them three turns still tell.
    /// Copilot 1.0.98 with its sidebar open: a divider down every row, the
    /// chat and the input box right of it.
    #[test]
    fn real_copilot_screen_with_the_sidebar() {
        let mut s = Shadow::new(120, 40);
        s.advance(include_bytes!(
            "../../tests/fixtures/copilot_1098_sidebar_120x40.bin"
        ));
        let snap = s.snapshot();
        assert_eq!(sidebar(&snap), Some(36));
        let k = composer_top(&snap).expect("input box");
        assert_eq!(k, 36);
        assert!(input_empty(&snap, k));
        assert_eq!(chat_rows(&snap, k), 2..34);
        let text = chat_text(&snap);
        assert!(
            text.rows_text(3, 4)
                .trim_start()
                .starts_with("\u{2022} b3r")
        );
        assert_eq!(fixed_columns(&snap), (37, true));
        assert_eq!(sidebar(&text), None);
    }

    #[test]
    fn moves_that_tell_how_far() {
        let screen = |alike_from: usize| {
            let mut s = Shadow::new(40, 32);
            for r in 0..32 {
                let row = if r >= alike_from {
                    "- same row".to_string()
                } else {
                    format!("- row {r}")
                };
                s.advance(format!("\x1b[{};1H{row}", r + 1).as_bytes());
            }
            s.snapshot()
        };
        assert_eq!(readable_move(&screen(32), 0..32, 1, 24), 24);
        assert_eq!(readable_move(&screen(7), 0..32, 1, 24), 9);
        assert_eq!(readable_move(&screen(0), 0..32, 1, 24), 3);
        // Up, the rows at the top stay: they differ.
        assert_eq!(readable_move(&screen(7), 0..32, -1, 24), 24);
    }

    #[test]
    fn the_thumb_inside_the_bar() {
        assert!(thumb_inside(&screen(3..5, true), 2..8));
        assert!(!thumb_inside(&screen(5..8, true), 2..8));
        assert!(!thumb_inside(&screen(2..4, true), 2..8));
    }

    /// The real Copilot 1.0.98 screen: its tab bar, chat, scrollbar and input box.
    #[test]
    fn real_copilot_screen() {
        let mut s = Shadow::new(120, 40);
        s.advance(include_bytes!(
            "../../tests/fixtures/copilot_1098_fullscreen_120x40.bin"
        ));
        let snap = s.snapshot();
        let k = composer_top(&snap).expect("input box");
        assert_eq!(k, 36);
        assert!(input_empty(&snap, k));
        assert_eq!(chat_rows(&snap, k), 2..34);
        assert!(!thumb_inside(&snap, 2..34), "at the bottom");
        // A drag over two rows, as Copilot copies it: without its scrollbar.
        let copied = "b3r17 words of row 17 in block 3\n\u{2022} b3r18";
        assert!(crate::select::locate(&snap, copied, None).is_none());
        let loc = crate::select::locate(&chat_text(&snap), copied, None).expect("found");
        assert_eq!((loc.first_row, loc.last_row), (9, 10));
    }
}
