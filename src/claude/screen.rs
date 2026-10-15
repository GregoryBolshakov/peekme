//! Claude Code's input area: the prompt `❯` between two full-width rules,
//! with its status lines below. The same on the full screen and the classic
//! one, where it sits under the last answer.

use crate::shadow::Snapshot;

fn is_rule(snap: &Snapshot, r: usize) -> bool {
    let row = &snap.rows[r];
    let dashes = row.iter().filter(|c| c.c == '─').count();
    dashes * 5 >= row.len() * 4
}

/// First row of the input area: the rule above the lowest `❯` prompt that
/// has one directly above it (earlier prompts in the transcript have none).
pub fn composer_top(snap: &Snapshot) -> Option<usize> {
    let rows = snap.rows.len();
    (rows / 3..rows).rev().find_map(|r| {
        let text: String = snap.rows[r].iter().take(4).map(|c| c.c).collect();
        (r > 0 && text.trim_start().starts_with('❯') && is_rule(snap, r - 1)).then(|| r - 1)
    })
}

/// The row under the last row of full-screen Claude's chat, its input box
/// starting at `composer`: the row above the box holds Claude's notices
/// ("You've used 96% of your session limit"), blank or not, and does not
/// scroll. While Claude works its spinner line ("✽ Canoodling… (9s ·
/// thinking)") sits above that, a blank row over it, and is not chat either.
pub fn chat_end(snap: &Snapshot, composer: usize) -> usize {
    let notice = composer.saturating_sub(1);
    let busy = (notice.saturating_sub(4)..notice)
        .rev()
        .find(|&r| working(&snap.rows_text(r, r + 1)));
    match busy {
        Some(r) if r > 0 && snap.rows_text(r - 1, r).trim().is_empty() => r - 1,
        Some(r) => r,
        None => notice,
    }
}

/// Claude's spinner line while it works: a spinner glyph, a word ending in
/// "…", and the time so far ("(9s").
fn working(text: &str) -> bool {
    let t = text.trim_start();
    let mut chars = t.chars();
    let glyph = chars.next().is_some_and(|c| "·✢✳✶✻✽*".contains(c));
    let rest = chars.as_str();
    glyph
        && rest.starts_with(' ')
        && rest.contains('…')
        && rest
            .split('(')
            .nth(1)
            .is_some_and(|after| after.starts_with(|c: char| c.is_ascii_digit()))
}

/// Full-screen Claude, scrolled up, pins the prompt of what is on screen on
/// the chat's top row ("❯ the prompt"), over the chat row there. A prompt's
/// own row looks the same, so this only says `row` can be the header.
pub fn may_be_header(snap: &Snapshot, row: usize) -> bool {
    row < snap.rows.len() && snap.rows_text(row, row + 1).starts_with('❯')
}

/// The columns of the hint full-screen Claude draws over the middle of its
/// last chat row while scrolled up ("Jump to bottom (ctrl+End) ↓"), with a
/// column of room each side. Claude leaves it off at the bottom of its chat.
pub fn bottom_hint(snap: &Snapshot, row: usize) -> Option<std::ops::Range<usize>> {
    const HINT: &str = "Jump to bottom";
    let cells = snap.rows.get(row)?;
    let text: String = cells.iter().map(|c| c.c).collect();
    let at = text.find(HINT)?;
    let start = text[..at].chars().count();
    let end = text[at..]
        .find('↓')
        .map_or(start + HINT.len(), |i| text[..at + i].chars().count() + 1);
    Some(start.saturating_sub(1)..(end + 1).min(cells.len()))
}

/// Claude's chat is scrolled to its start: its banner ("Claude Code v…")
/// shows under the first, blank row.
pub fn at_top(snap: &Snapshot, top: usize) -> bool {
    top + 1 < snap.rows.len()
        && snap.rows_text(top, top + 1).trim().is_empty()
        && snap.rows_text(top + 1, top + 2).contains("Claude Code v")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shadow::Shadow;

    fn top(capture: &[u8]) -> Option<usize> {
        let mut s = Shadow::new(120, 40);
        s.advance(capture);
        s.flush_sync();
        composer_top(&s.snapshot())
    }

    #[test]
    fn finds_the_input_box_in_both_renderers() {
        let full = top(include_bytes!(
            "../../tests/fixtures/claude_fullscreen_120x40.bin"
        ));
        let inline = top(include_bytes!(
            "../../tests/fixtures/claude_inline_120x40.bin"
        ));
        // Full screen: the box sits above the two status lines at the bottom.
        assert_eq!(full, Some(35));
        // Classic: right under the last answer, rows of screen left below it.
        assert!(inline.is_some_and(|k| k > 20 && k < 35), "{inline:?}");
        let mut s = Shadow::new(40, 6);
        s.advance(b"\xe2\x9d\xaf earlier prompt\r\nanswer\r\n");
        assert_eq!(composer_top(&s.snapshot()), None);
    }

    /// Scrolled up: the header on top, the hint over the last chat row, the
    /// banner only at the very start.
    #[test]
    fn scrolled_up_chrome() {
        let mut s = Shadow::new(80, 6);
        s.advance(
            "\x1b[1;1H\u{276f} q1 show\x1b[2;1H  - row one\x1b[5;1H  - row four           Jump to bottom (ctrl+End) \u{2193}"
                .as_bytes(),
        );
        let snap = s.snapshot();
        assert!(may_be_header(&snap, 0));
        assert!(!may_be_header(&snap, 1));
        assert_eq!(bottom_hint(&snap, 4), Some(22..51));
        assert_eq!(bottom_hint(&snap, 1), None);
        assert!(!at_top(&snap, 0));
        let mut s = Shadow::new(80, 6);
        s.advance(" \x1b[2;1H \u{2590}\u{259b}   Claude Code v2.1.299".as_bytes());
        assert!(at_top(&s.snapshot(), 0));
    }

    /// Idle, the chat ends above the notice row; working, above the blank row
    /// over Claude's spinner (as raw Claude 2.1.299 drew it).
    #[test]
    fn the_chat_ends_above_the_notices_and_the_spinner() {
        let rule = "\u{2500}".repeat(80);
        let screen = |spinner: bool| {
            let mut s = Shadow::new(80, 40);
            let mut b = String::new();
            for r in 0..33 {
                b.push_str(&format!("\x1b[{};1H     live{r}", r + 1));
            }
            if spinner {
                b.push_str(
                    "\x1b[33;1H\x1b[2K\x1b[34;1H\u{273d} Canoodling\u{2026} (9s \u{b7} thinking)",
                );
            }
            b.push_str(&format!(
                "\x1b[36;1H{rule}\x1b[37;1H\u{276f} \x1b[38;1H{rule}"
            ));
            s.advance(b.as_bytes());
            s.snapshot()
        };
        assert_eq!(composer_top(&screen(false)), Some(35));
        assert_eq!(chat_end(&screen(false), 35), 34);
        assert_eq!(chat_end(&screen(true), 35), 32);
        assert!(!working("  - b1r20 words of row 20 (12 rows)"));
    }
}
