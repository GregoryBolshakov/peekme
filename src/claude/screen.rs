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
}
