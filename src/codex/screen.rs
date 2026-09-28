//! What Codex draws when it runs full screen: its own mouse selection and its
//! input area.

use crate::context::ScreenSel;
use crate::select::Located;
use crate::shadow::Snapshot;

/// The text Codex itself has selected (its full-screen transcript highlights a
/// mouse selection in reverse video), as a located range on screen.
pub fn selection(snap: &Snapshot) -> Option<Located> {
    use alacritty_terminal::term::cell::Flags;
    let (screen, pos) = snap.text_with_positions();
    let inverse = |i: usize| {
        let (r, c) = pos[i];
        snap.rows[r][c].flags.contains(Flags::INVERSE) && screen[i] != '\n'
    };
    let start = (0..screen.len()).find(|&i| inverse(i))?;
    let end = (0..screen.len()).rfind(|&i| inverse(i))? + 1;
    // One highlighted stretch, possibly over several rows; blanks inside it may
    // be drawn without highlight, so only require the two ends.
    Some(Located {
        first_row: pos[start].0,
        last_row: pos[end - 1].0,
        screen: ScreenSel {
            text: screen,
            start,
            end,
        },
    })
}

/// First row of Codex's input area when it draws full screen: the row whose
/// text starts with the prompt mark `›`, lowest on screen, extended up over
/// rows with the same filled background (the input box padding).
pub fn composer_top(snap: &Snapshot) -> Option<usize> {
    use alacritty_terminal::vte::ansi::{Color, NamedColor};
    let rows = snap.rows.len();
    let prompt_row = (rows / 2..rows).rev().find(|&r| {
        let text: String = snap.rows[r].iter().take(4).map(|c| c.c).collect();
        text.trim_start().starts_with('›')
    })?;
    let bg = snap.rows[prompt_row][0].bg;
    if bg == Color::Named(NamedColor::Background) {
        return Some(prompt_row);
    }
    let mut top = prompt_row;
    while top > rows / 2 && snap.rows[top - 1][0].bg == bg {
        top -= 1;
    }
    Some(top)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shadow::Shadow;

    fn snap(bytes: &[u8], cols: u16, rows: u16) -> Snapshot {
        let mut s = Shadow::new(cols, rows);
        s.advance(bytes);
        s.snapshot()
    }

    #[test]
    fn codex_reverse_video_selection() {
        let s = snap(
            b"line one\r\n\x1b[2;3Hpick \x1b[7mme up\x1b[0m please\r\n",
            30,
            4,
        );
        let l = selection(&s).unwrap();
        assert_eq!(l.screen.selected(), "me up");
        assert_eq!((l.first_row, l.last_row), (1, 1));
        let s = snap(b"nothing selected\r\n", 30, 4);
        assert!(selection(&s).is_none());
    }

    #[test]
    fn composer_top_finds_the_input_band() {
        let s = snap(
            b"history\x1b[6;1H\x1b[48;2;57;57;71m      \x1b[0m\x1b[7;1H\x1b[48;2;57;57;71m\xe2\x80\xba Ask\x1b[0m\x1b[8;1H\x1b[48;2;57;57;71m      \x1b[0m\x1b[9;1Hstatus",
            20,
            10,
        );
        assert_eq!(composer_top(&s), Some(5));
    }
}
