//! The shadow terminal: an in-memory emulator fed every byte the child writes,
//! so codex-peek always knows exactly what the user's screen shows.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::{Cell, Flags};
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Processor, Rgb, StdSyncHandler};

/// Replies the emulator wants to send back to the child (answers to queries).
#[derive(Clone, Default)]
struct Replies(Arc<Mutex<Vec<Reply>>>);

enum Reply {
    Text(String),
    Color(usize, Arc<dyn Fn(Rgb) -> String + Sync + Send + 'static>),
}

impl EventListener for Replies {
    fn send_event(&self, event: Event) {
        let reply = match event {
            Event::PtyWrite(s) => Reply::Text(s),
            Event::ColorRequest(i, f) => Reply::Color(i, f),
            _ => return,
        };
        if let Ok(mut v) = self.0.lock() {
            v.push(reply);
        }
    }
}

pub struct Shadow {
    term: Term<Replies>,
    parser: Processor<StdSyncHandler>,
    replies: Replies,
    /// Foreground/background as reported by the real terminal (sniffed from its replies).
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
}

/// A frozen copy of the visible screen.
#[derive(Clone)]
pub struct Snapshot {
    pub rows: Vec<Vec<Cell>>,
    pub cols: usize,
    pub cursor: (usize, usize),
    pub cursor_visible: bool,
    pub template: Cell,
}

impl Shadow {
    pub fn new(cols: u16, rows: u16) -> Self {
        let replies = Replies::default();
        let config = Config {
            scrolling_history: 10_000,
            kitty_keyboard: true,
            ..Config::default()
        };
        let size = TermSize::new(cols as usize, rows as usize);
        Self {
            term: Term::new(config, &size, replies.clone()),
            parser: Processor::new(),
            replies,
            fg: None,
            bg: None,
        }
    }

    pub fn advance(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
    }

    pub fn resize(&mut self, cols: u16, rows: u16) {
        self.term
            .resize(TermSize::new(cols as usize, rows as usize));
    }

    /// True while the child is inside a synchronized update (mode 2026) that
    /// has not been applied to the shadow yet.
    pub fn in_sync_update(&self) -> bool {
        self.parser.sync_timeout().sync_timeout().is_some()
    }

    /// Apply a synchronized update whose timeout expired.
    pub fn check_sync_timeout(&mut self) {
        if let Some(deadline) = self.parser.sync_timeout().sync_timeout()
            && Instant::now() >= deadline
        {
            self.parser.stop_sync(&mut self.term);
        }
    }

    /// Apply any pending synchronized update now.
    pub fn flush_sync(&mut self) {
        self.parser.stop_sync(&mut self.term);
    }

    /// Answers the emulator produced for the child's queries since the last call.
    pub fn take_replies(&mut self) -> Vec<u8> {
        let pending = std::mem::take(&mut *self.replies.0.lock().unwrap());
        let mut out = Vec::new();
        for r in pending {
            match r {
                Reply::Text(s) => out.extend_from_slice(s.as_bytes()),
                Reply::Color(i, f) => {
                    let rgb = match i {
                        256 => self.fg.unwrap_or(Rgb {
                            r: 0xff,
                            g: 0xff,
                            b: 0xff,
                        }),
                        _ => self.bg.unwrap_or(Rgb { r: 0, g: 0, b: 0 }),
                    };
                    out.extend_from_slice(f(rgb).as_bytes());
                }
            }
        }
        out
    }

    pub fn rows(&self) -> usize {
        self.term.screen_lines()
    }

    pub fn cols(&self) -> usize {
        self.term.columns()
    }

    pub fn snapshot(&self) -> Snapshot {
        let grid = self.term.grid();
        let rows = (0..self.rows())
            .map(|r| {
                let row = &grid[Line(r as i32)];
                (0..self.cols()).map(|c| row[Column(c)].clone()).collect()
            })
            .collect();
        let cursor = grid.cursor.point;
        Snapshot {
            rows,
            cols: self.cols(),
            cursor: (cursor.line.0.max(0) as usize, cursor.column.0),
            cursor_visible: self.term.mode().contains(TermMode::SHOW_CURSOR),
            template: grid.cursor.template.clone(),
        }
    }
}

impl Snapshot {
    /// Plain text of the screen with a map from each char to its (row, col).
    /// Soft-wrapped rows are joined without a newline, like terminal selections.
    pub fn text_with_positions(&self) -> (Vec<char>, Vec<(usize, usize)>) {
        let mut chars = Vec::new();
        let mut pos = Vec::new();
        for (r, row) in self.rows.iter().enumerate() {
            let mut line: Vec<(char, usize)> = Vec::new();
            for (c, cell) in row.iter().enumerate() {
                if cell
                    .flags
                    .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER)
                {
                    continue;
                }
                line.push((cell.c, c));
            }
            let wrapped = row
                .last()
                .is_some_and(|c| c.flags.contains(Flags::WRAPLINE));
            if !wrapped {
                while line.last().is_some_and(|(ch, _)| *ch == ' ') {
                    line.pop();
                }
            }
            for (ch, c) in line {
                chars.push(ch);
                pos.push((r, c));
            }
            if !wrapped {
                chars.push('\n');
                pos.push((r, self.cols.saturating_sub(1)));
            }
        }
        (chars, pos)
    }

    /// Text of rows `from..to`, trailing blanks removed.
    #[allow(dead_code)] // used by examples/vtdump.rs
    pub fn rows_text(&self, from: usize, to: usize) -> String {
        let mut s = String::new();
        for row in &self.rows[from.min(self.rows.len())..to.min(self.rows.len())] {
            let line: String = row
                .iter()
                .filter(|c| !c.flags.contains(Flags::WIDE_CHAR_SPACER))
                .map(|c| c.c)
                .collect();
            s.push_str(line.trim_end());
            s.push('\n');
        }
        s
    }
}
