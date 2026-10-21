//! Pi's screen (1.3): the chat, a blank row, then the input area: a
//! full-width rule, the editor's rows, another rule, the directory (it can
//! wrap) and a status row at the bottom. The same full screen and inline.
//! While Pi works its upper rule reads "── ⠋ Working ───". Full screen,
//! scrolled up, Pi draws a scrollbar in the last column of the chat rows and
//! "↓ Jump to latest message · Ctrl+End" over the last chat row; after a
//! drag it shows " Copied! " in reverse video at the right end of the top
//! row.

use std::ops::Range;

use alacritty_terminal::term::cell::Flags;

use crate::codex;
use crate::select::Located;
use crate::shadow::Snapshot;

/// A full-width rule: dashes from the first cell, a short label allowed
/// ("── ⠋ Working ───").
fn is_rule(snap: &Snapshot, r: usize) -> bool {
    let row = &snap.rows[r];
    let dashes = row.iter().filter(|c| c.c == '─').count();
    row.first().is_some_and(|c| c.c == '─') && dashes * 5 >= row.len() * 4
}

/// Editor rows Pi shows at most before it scrolls them (a guess with room:
/// the input area is found from the bottom).
const MAX_EDITOR_ROWS: usize = 16;

/// First row of the input area: the upper of the two lowest rules.
pub fn composer_top(snap: &Snapshot) -> Option<usize> {
    let rows = snap.rows.len();
    let lower = (rows / 3..rows).rev().find(|&r| is_rule(snap, r))?;
    (lower.saturating_sub(MAX_EDITOR_ROWS + 1).max(rows / 3)..lower)
        .rev()
        .find(|&r| is_rule(snap, r))
}

/// The rows between the two rules: what is typed.
fn editor_rows(snap: &Snapshot, composer: usize) -> Range<usize> {
    let rows = snap.rows.len();
    let end = (composer + 1..rows)
        .find(|&r| is_rule(snap, r))
        .unwrap_or(composer + 1);
    composer + 1..end
}

/// Nothing is typed in the input area starting at `composer` (Pi draws its
/// cursor as a reverse-video blank).
pub fn input_empty(snap: &Snapshot, composer: usize) -> bool {
    let r = editor_rows(snap, composer);
    snap.rows_text(r.start, r.end).trim().is_empty()
}

/// The row under the last row of the chat, its input area starting at
/// `composer`: the blank row above the rule does not scroll.
pub fn chat_end(composer: usize) -> usize {
    composer.saturating_sub(1)
}

/// Pi's "↓ Jump to latest message" on row `r`: the full-screen chat is
/// scrolled up from its bottom.
pub fn jump_hint(snap: &Snapshot, r: usize) -> bool {
    r < snap.rows.len() && snap.rows_text(r, r + 1).contains("Jump to latest message")
}

/// The columns of Pi's " ↓ Jump to latest message · Ctrl+End " on row `r`
/// (Pi draws a blank each side, and its key for the jump when it has one),
/// drawn over the chat row there, whose text can go on after it.
pub fn hint_span(snap: &Snapshot, r: usize) -> Option<Range<usize>> {
    let cols = find_in_row(snap, r, "↓ Jump to latest message")?;
    let row = &snap.rows[r];
    let at = |c: usize| row.get(c).map(|x| x.c);
    let mut end = cols.end;
    if at(end) == Some(' ') && at(end + 1) == Some('·') && at(end + 2) == Some(' ') {
        end += 3;
        while at(end).is_some_and(|c| c != ' ') {
            end += 1;
        }
    }
    Some(cols.start.saturating_sub(1)..(end + 1).min(row.len()))
}

/// The column of Pi's scrollbar (`│` track, `┃` thumb in the last column of
/// every chat row), when it shows one.
fn scrollbar(snap: &Snapshot, chat: Range<usize>) -> Option<usize> {
    let col = snap.cols.checked_sub(1)?;
    let bar = |r: usize| {
        snap.rows[r]
            .get(col)
            .is_some_and(|c| c.c == '│' || c.c == '┃')
    };
    (chat.len() >= 3 && chat.clone().all(bar)).then_some(col)
}

/// Pi's scrollbar thumb (`┃`) leaves track (`│`) on both sides within
/// `rows`: the chat is at neither end.
pub fn thumb_inside(snap: &Snapshot, rows: Range<usize>) -> bool {
    let Some(col) = scrollbar(snap, rows.clone()) else {
        return false;
    };
    let thumb: Vec<bool> = rows.map(|r| snap.rows[r][col].c == '┃').collect();
    let (first, last) = (
        thumb.iter().position(|&t| t),
        thumb.iter().rposition(|&t| t),
    );
    matches!((first, last), (Some(f), Some(l)) if f > 0 && l + 1 < thumb.len())
}

/// `snap` with only the chat's text: the scrollbar, the jump hint and the
/// " Copied! " notice blanked (none of them moves with the chat, nor is in
/// what Pi copies). Rows and columns stay where they are.
pub fn chat_text(snap: &Snapshot) -> Snapshot {
    let mut out = snap.clone();
    let rows = snap.rows.len();
    let end = composer_top(snap).map_or(rows, chat_end);
    if let Some(col) = scrollbar(snap, 0..end) {
        for row in &mut out.rows[..end] {
            row[col] = Default::default();
        }
    }
    for r in 0..end {
        let notice = find_in_row(snap, r, " Copied! ");
        for cols in [notice, hint_span(snap, r)].into_iter().flatten() {
            for cell in &mut out.rows[r][cols] {
                *cell = Default::default();
            }
        }
    }
    out
}

/// Columns of `word` on row `r`. " Copied! " only in reverse video.
fn find_in_row(snap: &Snapshot, r: usize, word: &str) -> Option<Range<usize>> {
    let row = &snap.rows[r];
    let w: Vec<char> = word.chars().collect();
    (0..row.len().checked_sub(w.len())? + 1).find_map(|c| {
        let same = row[c..c + w.len()]
            .iter()
            .zip(&w)
            .all(|(cell, ch)| cell.c == *ch);
        let marked = !word.starts_with(' ')
            || row[c..c + w.len()]
                .iter()
                .all(|cell| cell.flags.contains(Flags::INVERSE));
        (same && marked).then_some(c..c + w.len())
    })
}

/// Pi's own selection: its reverse-video highlight in the chat rows (the
/// notice and the input area's drawn cursor are not in them).
pub fn selection(snap: &Snapshot) -> Option<Located> {
    let mut chat = chat_text(snap);
    let end = composer_top(snap).map_or(chat.rows.len(), chat_end);
    for row in &mut chat.rows[end..] {
        for cell in row.iter_mut() {
            *cell = Default::default();
        }
    }
    let loc = codex::screen::selection(&chat)?;
    // The same cells in the text as it is on the screen.
    let (_, cells) = chat.text_with_positions();
    let (first, last) = (cells[loc.screen.start], cells[loc.screen.end - 1]);
    let (text, pos) = snap.text_with_positions();
    let start = pos.iter().position(|&p| p == first)?;
    let end = pos.iter().position(|&p| p == last)? + 1;
    Some(Located {
        screen: crate::context::ScreenSel { text, start, end },
        ..loc
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shadow::Shadow;

    fn snap(capture: &[u8], cols: u16, rows: u16) -> Snapshot {
        let mut s = Shadow::new(cols, rows);
        s.advance(capture);
        s.flush_sync();
        s.snapshot()
    }

    #[test]
    fn finds_the_input_area_full_screen_and_inline() {
        let full = snap(
            include_bytes!("../../tests/fixtures/pi_fullscreen_120x40.bin"),
            120,
            40,
        );
        assert_eq!(composer_top(&full), Some(35));
        assert_eq!(chat_end(35), 34);
        assert!(input_empty(&full, 35));
        let inline = snap(
            include_bytes!("../../tests/fixtures/pi_regular_120x40.bin"),
            120,
            40,
        );
        let k = composer_top(&inline).expect("input area inline");
        assert!(inline.rows_text(k + 1, k + 2).trim().is_empty());
        // A screen without Pi's input area has none.
        assert_eq!(composer_top(&snap(b"hello\r\n", 40, 10)), None);
    }

    /// Working: a label on the upper rule; text typed in the editor.
    #[test]
    fn the_rule_while_working_and_typed_text() {
        let rule = "\u{2500}".repeat(60);
        let working = format!(
            "\u{2500}\u{2500} \u{280b} Working {}",
            "\u{2500}".repeat(48)
        );
        let screen = format!(
            "\x1b[1;1H r0 row\x1b[8;1H{working}\x1b[9;1Hfix it\x1b[10;1H{rule}\x1b[11;1H/work\x1b[12;1H0.1%/128k"
        );
        let s = snap(screen.as_bytes(), 60, 12);
        assert_eq!(composer_top(&s), Some(7));
        assert!(!input_empty(&s, 7));
    }

    /// The scrollbar, the jump hint and " Copied! " are not chat text; the
    /// reverse-video drag is the selection.
    #[test]
    fn chat_text_and_selection() {
        let rule = "\u{2500}".repeat(40);
        let mut screen = String::new();
        for r in 0..6 {
            screen.push_str(&format!("\x1b[{};1H r{r} row of the answer", r + 1));
            screen.push_str(&format!("\x1b[{};40H\u{2502}", r + 1));
        }
        screen.push_str("\x1b[1;30H\x1b[7m Copied! \x1b[0m");
        screen.push_str("\x1b[3;5H\x1b[7mrow of\x1b[27m");
        screen.push_str("\x1b[6;5H \u{2193} Jump to latest message \u{b7} End ");
        screen.push_str(&format!(
            "\x1b[8;1H{rule}\x1b[10;1H{rule}\x1b[11;1H/w\x1b[12;1Hs"
        ));
        let s = snap(screen.as_bytes(), 40, 12);
        assert_eq!(composer_top(&s), Some(7));
        assert!(jump_hint(&s, 5));
        let text = chat_text(&s);
        assert_eq!(text.rows_text(0, 1).trim_end(), " r0 row of the answer");
        // The hint is drawn over the row from column 5 on, its text after it.
        assert_eq!(text.rows_text(5, 6).trim_end(), " r5");
        assert_eq!(hint_span(&s, 5), Some(4..36));
        let sel = selection(&s).expect("selection");
        assert_eq!(sel.first_row, 2);
        assert_eq!(sel.screen.selected(), "row of");
    }
}
