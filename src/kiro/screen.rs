//! Kiro CLI's input area: a full-width rule, its status lines (agent, model,
//! directory), then the prompt `›` at the left edge and a hint line. The same
//! inline and full screen. The user's earlier messages are drawn with `›` too,
//! but indented.

use crate::shadow::Snapshot;

/// A full-width rule. Full screen, scrolled up, Kiro centres its "2 more
/// lines (click or End) ↓" on the rule above its input area: a rule still,
/// dashes at both ends.
fn is_rule(snap: &Snapshot, r: usize) -> bool {
    let row = &snap.rows[r];
    let dashes = row.iter().filter(|c| c.c == '─').count();
    let ends = row.first().is_some_and(|c| c.c == '─') && row.last().is_some_and(|c| c.c == '─');
    dashes * 5 >= row.len() * 4 || (ends && jump_hint(snap, r))
}

/// Kiro's "N more lines (click or End) ↓" (or "N new messages", "Jump to
/// bottom") on row `r`: the full-screen chat is scrolled up from its bottom.
pub fn jump_hint(snap: &Snapshot, r: usize) -> bool {
    r < snap.rows.len() && snap.rows_text(r, r + 1).contains("(click or End)")
}

/// Status lines between the rule and the prompt: agent and model, then the
/// directory, which can wrap.
const MAX_STATUS_ROWS: usize = 6;

/// First row of the input area: the rule above the lowest `›` prompt that
/// starts its row.
pub fn composer_top(snap: &Snapshot) -> Option<usize> {
    let rows = snap.rows.len();
    let prompt = (rows / 3..rows).rev().find(|&r| snap.rows[r][0].c == '›')?;
    (prompt.saturating_sub(MAX_STATUS_ROWS + 1)..prompt)
        .rev()
        .find(|&r| is_rule(snap, r))
}

/// Nothing is typed in the input area starting at `composer`: its prompt
/// row (`›` at the left edge) shows Kiro's placeholder, or nothing.
pub fn input_empty(snap: &Snapshot, composer: usize) -> bool {
    let rows = snap.rows.len();
    (composer..rows)
        .find(|&r| snap.rows[r].first().is_some_and(|c| c.c == '›'))
        .is_some_and(|r| {
            let text = snap.rows_text(r, r + 1);
            let typed = text.trim_start_matches('›').trim();
            typed.is_empty() || typed.starts_with("ask a question or describe a task")
        })
}

/// The row under the last row of full-screen Kiro's chat, its input area
/// starting at `composer`: the row above Kiro's rule holds its notices
/// ("• Copied "…"" after a selection, "• Entered fullscreen …"), blank or
/// not, and does not scroll.
pub fn chat_end(composer: usize) -> usize {
    composer.saturating_sub(1)
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
    fn finds_the_input_area_inline_and_full_screen() {
        let inline = top(include_bytes!(
            "../../tests/fixtures/kiro_inline_120x40.bin"
        ));
        let full = top(include_bytes!(
            "../../tests/fixtures/kiro_fullscreen_120x40.bin"
        ));
        assert_eq!(inline, Some(30));
        assert_eq!(full, Some(31));
        // Earlier messages (`  › List the files ...`) are not the prompt, and
        // a screen without Kiro's input area has none.
        assert_eq!(top(b"  \xe2\x80\xba hello\r\n"), None);
    }

    /// Scrolled up, the hint on the rule; the chat ends above a blank row.
    #[test]
    fn the_rule_with_its_hint() {
        let mut s = Shadow::new(60, 12);
        let rule = |hint: &str| {
            let side = (60 - hint.chars().count()) / 2;
            format!(
                "{}{hint}{}",
                "\u{2500}".repeat(side),
                "\u{2500}".repeat(60 - side - hint.chars().count())
            )
        };
        let hint = rule(" 2 more lines (click or End) \u{2193} ");
        s.advance(
            format!(
                "\x1b[1;1H  - row\x1b[8;1H{hint}\x1b[9;1Hkiro_default \u{b7} auto\x1b[11;1H\u{203a}  ask a question or describe a task \u{21b5}"
            )
            .as_bytes(),
        );
        let snap = s.snapshot();
        assert_eq!(composer_top(&snap), Some(7));
        assert!(jump_hint(&snap, 7));
        assert_eq!(chat_end(7), 6);
        assert!(input_empty(&snap, 7));
    }
}
