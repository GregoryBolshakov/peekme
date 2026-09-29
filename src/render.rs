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
        self::cells(out, cells);
        continued = wraps;
    }
}

/// Bytes that repaint one screen row (0-based `row`) with the given cells.
pub fn row(out: &mut String, row: usize, cells: &[Cell]) {
    write!(out, "\x1b[{};1H", row + 1).unwrap();
    self::cells(out, cells);
}

/// Bytes for `cells` from the cursor on, ending with the pen reset.
pub fn cells(out: &mut String, cells: &[Cell]) {
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

/// Tracks whether a byte stream stopped between two escape sequences and
/// characters, or inside one. Our own frames may only be written at a
/// boundary: an OSC cut in two by a box frame would print the rest of the
/// window title as text.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Boundary {
    #[default]
    Ground,
    /// Continuation bytes still owed by a UTF-8 character.
    Utf8(u8),
    Esc,
    EscIntermediate,
    Csi,
    /// OSC, DCS, APC, PM or SOS body, ended by BEL or ST.
    Str,
    StrEsc,
}

impl Boundary {
    pub fn feed(&mut self, bytes: &[u8]) {
        for &b in bytes {
            *self = self.step(b);
        }
    }

    pub fn at_boundary(&self) -> bool {
        *self == Boundary::Ground
    }

    fn step(self, b: u8) -> Boundary {
        use Boundary::*;
        match self {
            Str => match b {
                0x07 => Ground,
                0x1b => StrEsc,
                _ => Str,
            },
            StrEsc if b == b'\\' => Ground,
            _ if b == 0x1b => Esc,
            // CAN and SUB cancel any sequence.
            _ if b == 0x18 || b == 0x1a => Ground,
            Ground | StrEsc | Utf8(_) => match b {
                0xc0..=0xdf => Utf8(1),
                0xe0..=0xef => Utf8(2),
                0xf0..=0xf7 => Utf8(3),
                0x80..=0xbf => match self {
                    Utf8(n) if n > 1 => Utf8(n - 1),
                    _ => Ground,
                },
                _ => Ground,
            },
            Esc => match b {
                b'[' => Csi,
                b']' | b'P' | b'_' | b'^' | b'X' => Str,
                0x20..=0x2f => EscIntermediate,
                _ => Ground,
            },
            EscIntermediate => match b {
                0x20..=0x2f => EscIntermediate,
                _ => Ground,
            },
            Csi => match b {
                0x40..=0x7e => Ground,
                _ => Csi,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn boundary_tracks_sequences_and_utf8() {
        let cases: &[(&[u8], bool)] = &[
            (b"plain", true),
            (b"\x1b", false),
            (b"\x1b[38;2;1", false),
            (b"\x1b[38;2;1;2;3m", true),
            (b"\x1b]0;title", false),
            (b"\x1b]0;title\x07", true),
            (b"\x1b]8;;http://x\x1b\\", true),
            (b"\x1b]52;c;aGk\x1b", false),
            (b"\x1b(B", true),
            (b"\x1b(", false),
            (b"\xe2\x97", false),
            (b"\xe2\x97\x90", true),
        ];
        for (bytes, ok) in cases {
            let mut b = Boundary::default();
            b.feed(bytes);
            assert_eq!(b.at_boundary(), *ok, "{:?}", String::from_utf8_lossy(bytes));
        }
    }

    #[test]
    fn strips_only_queries() {
        let b = b"a\x1b[6nb\x1b]11;?\x1b\\c\x1b[31md";
        assert_eq!(strip_answered_queries(b), b"abc\x1b[31md");
    }
}
