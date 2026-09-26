//! Turning shadow cells back into bytes, faithfully: colours are re-emitted in
//! the form the child used (default, indexed, or RGB) so a repaint is invisible.

use std::fmt::Write;

use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::vte::ansi::{Color, NamedColor};

pub const SYNC_BEGIN: &str = "\x1b[?2026h";
pub const SYNC_END: &str = "\x1b[?2026l";

fn color_params(out: &mut String, color: Color, fg: bool) {
    let (base, bright, ext) = if fg { (30, 90, 38) } else { (40, 100, 48) };
    match color {
        Color::Named(n) => {
            let idx = n as usize;
            match idx {
                0..=7 => write!(out, ";{}", base + idx).unwrap(),
                8..=15 => write!(out, ";{}", bright + idx - 8).unwrap(),
                _ => match n {
                    NamedColor::DimBlack
                    | NamedColor::DimRed
                    | NamedColor::DimGreen
                    | NamedColor::DimYellow
                    | NamedColor::DimBlue
                    | NamedColor::DimMagenta
                    | NamedColor::DimCyan
                    | NamedColor::DimWhite => {
                        let i = idx - NamedColor::DimBlack as usize;
                        write!(out, ";{}", base + i).unwrap();
                    }
                    // Foreground/Background/Cursor and friends: the default colour.
                    _ => {}
                },
            }
        }
        Color::Indexed(i) => write!(out, ";{ext};5;{i}").unwrap(),
        Color::Spec(rgb) => write!(out, ";{ext};2;{};{};{}", rgb.r, rgb.g, rgb.b).unwrap(),
    }
}

/// Full SGR sequence (starting from a reset) for a cell's style.
pub fn sgr(cell: &Cell) -> String {
    let mut s = String::from("\x1b[0");
    let f = cell.flags;
    if f.contains(Flags::BOLD) {
        s.push_str(";1");
    }
    if f.contains(Flags::DIM) {
        s.push_str(";2");
    }
    if f.contains(Flags::ITALIC) {
        s.push_str(";3");
    }
    if f.contains(Flags::DOUBLE_UNDERLINE) {
        s.push_str(";21");
    } else if f.contains(Flags::UNDERCURL) {
        s.push_str(";4:3");
    } else if f.contains(Flags::DOTTED_UNDERLINE) {
        s.push_str(";4:4");
    } else if f.contains(Flags::DASHED_UNDERLINE) {
        s.push_str(";4:5");
    } else if f.contains(Flags::UNDERLINE) {
        s.push_str(";4");
    }
    if f.contains(Flags::INVERSE) {
        s.push_str(";7");
    }
    if f.contains(Flags::HIDDEN) {
        s.push_str(";8");
    }
    if f.contains(Flags::STRIKEOUT) {
        s.push_str(";9");
    }
    color_params(&mut s, cell.fg, true);
    color_params(&mut s, cell.bg, false);
    s.push('m');
    s
}

/// Repaint consecutive screen rows starting at `first`. A row that was
/// soft-wrapped is continued into the next one without a cursor move, so the
/// terminal records the wrap again (it matters for its own reflow and for
/// mouse selections spanning the rows).
pub fn rows(out: &mut String, first: usize, rows: &[Vec<Cell>]) {
    let mut continued = false;
    for (i, cells) in rows.iter().enumerate() {
        let is_last = i + 1 == rows.len();
        let wraps = !is_last
            && cells
                .last()
                .is_some_and(|c| c.flags.contains(Flags::WRAPLINE));
        if !continued {
            write!(out, "\x1b[{};1H", first + i + 1).unwrap();
        }
        row_cells(out, cells);
        continued = wraps;
    }
}

/// Bytes that repaint one screen row (0-based `row`) with the given cells.
pub fn row(out: &mut String, row: usize, cells: &[Cell]) {
    write!(out, "\x1b[{};1H", row + 1).unwrap();
    row_cells(out, cells);
}

fn row_cells(out: &mut String, cells: &[Cell]) {
    let mut last = String::new();
    for cell in cells {
        if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
            continue;
        }
        let style = sgr(cell);
        if style != last {
            out.push_str(&style);
            last = style;
        }
        if cell.flags.contains(Flags::LEADING_WIDE_CHAR_SPACER) {
            out.push(' ');
            continue;
        }
        out.push(if cell.c == '\0' { ' ' } else { cell.c });
        if let Some(extra) = cell.zerowidth() {
            out.extend(extra.iter());
        }
    }
    out.push_str("\x1b[0m");
}

/// Put the cursor, its visibility and the pen back where the child left them.
pub fn restore_cursor(out: &mut String, snap: &crate::shadow::Snapshot) {
    out.push_str(&sgr(&snap.template));
    write!(out, "\x1b[{};{}H", snap.cursor.0 + 1, snap.cursor.1 + 1).unwrap();
    out.push_str(if snap.cursor_visible {
        "\x1b[?25h"
    } else {
        "\x1b[?25l"
    });
}

/// Remove the child's terminal queries from bytes that are replayed after the
/// shadow already answered them, so the child never receives two answers.
pub fn strip_answered_queries(buf: &[u8]) -> Vec<u8> {
    const QUERIES: &[&[u8]] = &[
        b"\x1b[6n",
        b"\x1b[?6n",
        b"\x1b[5n",
        b"\x1b[c",
        b"\x1b[0c",
        b"\x1b[>c",
        b"\x1b[>0c",
        b"\x1b[?u",
        b"\x1b]10;?\x1b\\",
        b"\x1b]11;?\x1b\\",
        b"\x1b]10;?\x07",
        b"\x1b]11;?\x07",
    ];
    let mut out = Vec::with_capacity(buf.len());
    let mut i = 0;
    'outer: while i < buf.len() {
        if buf[i] == 0x1b {
            for q in QUERIES {
                if buf[i..].starts_with(q) {
                    i += q.len();
                    continue 'outer;
                }
            }
        }
        out.push(buf[i]);
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_only_queries() {
        let b = b"a\x1b[6nb\x1b]11;?\x1b\\c\x1b[31md";
        assert_eq!(strip_answered_queries(b), b"abc\x1b[31md");
    }
}
