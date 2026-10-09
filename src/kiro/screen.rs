//! Kiro CLI's input area: a full-width rule, its status lines (agent, model,
//! directory), then the prompt `›` at the left edge and a hint line. The same
//! inline and full screen. The user's earlier messages are drawn with `›` too,
//! but indented.

use crate::shadow::Snapshot;

fn is_rule(snap: &Snapshot, r: usize) -> bool {
    let row = &snap.rows[r];
    let dashes = row.iter().filter(|c| c.c == '─').count();
    dashes * 5 >= row.len() * 4
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
}
