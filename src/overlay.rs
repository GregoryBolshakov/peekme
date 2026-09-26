//! The peek box: where it goes, what it looks like, and the exact bytes that
//! open and close it without leaving a trace.

use std::fmt::Write;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::render::{self, SYNC_BEGIN, SYNC_END};
use crate::shadow::Snapshot;

/// Where the box sits and which screen rows codex-peek repaints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// First and last screen rows of the box.
    pub box_top: usize,
    pub box_height: usize,
    /// Rows redrawn while the box is open (and restored on close).
    pub region: (usize, usize),
    /// Box below the selection (rows after it shift down) or above it (rows before shift up).
    pub below: bool,
}

pub fn layout(rows: usize, sel_first: usize, sel_last: usize) -> Layout {
    let want = (rows * 2 / 5).clamp(5, 14);
    let space_below = rows.saturating_sub(sel_last + 1);
    let space_above = sel_first;
    if space_below >= want || (space_below >= 4 && space_below >= space_above) {
        let h = want.min(space_below);
        Layout {
            box_top: sel_last + 1,
            box_height: h,
            region: (sel_last + 1, rows),
            below: true,
        }
    } else if space_above >= 4 {
        let h = want.min(space_above);
        Layout {
            box_top: sel_first - h,
            box_height: h,
            region: (0, sel_first),
            below: false,
        }
    } else {
        // Tiny screen: cover the top rows.
        let h = want.min(rows);
        Layout {
            box_top: 0,
            box_height: h,
            region: (0, h),
            below: true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Style {
    Plain,
    Bold,
    Code,
    Dim,
    Error,
}

impl Style {
    fn sgr(self) -> &'static str {
        match self {
            Style::Plain => "\x1b[0m",
            Style::Bold => "\x1b[0;1m",
            Style::Code => "\x1b[0;36m",
            Style::Dim => "\x1b[0;2m",
            Style::Error => "\x1b[0;31m",
        }
    }
}

type Line = Vec<(Style, String)>;

/// The content of a peek box.
pub struct PeekBox {
    pub title: String,
    pub model: String,
    pub text: String,
    pub status: Status,
    pub scroll: usize,
    pub waiting_updates: usize,
    /// Offer "Alt+P again" to re-explain with the whole conversation.
    pub deep_available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Thinking,
    Streaming,
    Done,
    Error(String),
    Message(String),
}

impl PeekBox {
    pub fn new(selection: &str) -> Self {
        let flat: String = selection.split_whitespace().collect::<Vec<_>>().join(" ");
        Self {
            title: flat,
            model: String::new(),
            text: String::new(),
            status: Status::Thinking,
            scroll: 0,
            waiting_updates: 0,
            deep_available: false,
        }
    }

    pub fn message(text: &str) -> Self {
        Self {
            title: "codex-peek".into(),
            model: String::new(),
            text: String::new(),
            status: Status::Message(text.into()),
            scroll: 0,
            waiting_updates: 0,
            deep_available: false,
        }
    }

    fn body_lines(&self, width: usize) -> Vec<Line> {
        match &self.status {
            Status::Message(m) => wrap(&[(Style::Plain, m.clone())], width),
            Status::Thinking if self.text.is_empty() => {
                vec![vec![(Style::Dim, "thinking…".into())]]
            }
            Status::Error(e) => {
                let mut l = markdown_lines(&self.text, width);
                l.extend(wrap(&[(Style::Error, format!("error: {e}"))], width));
                l
            }
            _ => markdown_lines(&self.text, width),
        }
    }

    /// Max scroll offset for a box of `height` rows on a `cols`-wide screen.
    pub fn max_scroll(&self, cols: usize, height: usize) -> usize {
        let inner = height.saturating_sub(2);
        self.body_lines(cols.saturating_sub(4))
            .len()
            .saturating_sub(inner)
    }

    /// Bytes for the box rows only.
    pub fn draw(&self, out: &mut String, lay: &Layout, cols: usize) {
        let inner_w = cols.saturating_sub(4);
        let inner_h = lay.box_height.saturating_sub(2);
        let lines = self.body_lines(inner_w);
        let scroll = self.scroll.min(lines.len().saturating_sub(inner_h));
        let border = "\x1b[0;36m";

        // Top border: ╭─ peek · "selection" ──── model ─╮
        let right = if self.model.is_empty() {
            String::new()
        } else {
            format!(" {} ", self.model)
        };
        let budget = cols.saturating_sub(8 + right.width());
        let title = format!(" peek · {} ", clip(&self.title, budget.saturating_sub(9)));
        let fill = cols.saturating_sub(3 + title.width() + right.width());
        write!(
            out,
            "\x1b[{};1H{border}╭─{title}{}{right}╮",
            lay.box_top + 1,
            "─".repeat(fill)
        )
        .unwrap();

        for i in 0..inner_h {
            write!(out, "\x1b[{};1H{border}│ ", lay.box_top + 2 + i).unwrap();
            let mut used = 0;
            if let Some(line) = lines.get(scroll + i) {
                for (style, text) in line {
                    out.push_str(style.sgr());
                    out.push_str(text);
                    used += text.width();
                }
            }
            write!(
                out,
                "\x1b[0m{}{border} │",
                " ".repeat(inner_w.saturating_sub(used))
            )
            .unwrap();
        }

        // Bottom border with hints.
        let mut hints = vec!["Esc close".to_string()];
        if self.deep_available {
            hints.push("Alt+P again: use whole chat".into());
        }
        if lines.len() > inner_h {
            hints.push(format!(
                "PgUp/PgDn {}/{}",
                (scroll + inner_h).min(lines.len()),
                lines.len()
            ));
        }
        match &self.status {
            Status::Thinking | Status::Streaming => hints.push("…".into()),
            _ => {}
        }
        if self.waiting_updates > 0 {
            hints.push(format!("Codex: {} updates waiting", self.waiting_updates));
        }
        let hint = clip(&format!(" {} ", hints.join(" · ")), cols.saturating_sub(4));
        let fill = cols.saturating_sub(3 + hint.width());
        write!(
            out,
            "\x1b[{};1H{border}╰{}{hint}─╯\x1b[0m",
            lay.box_top + lay.box_height,
            "─".repeat(fill)
        )
        .unwrap();
    }
}

/// The frame that shows the box: the region repainted with the box inserted and
/// the surrounding rows shifted away from it.
pub fn open_frame(snap: &Snapshot, lay: &Layout, peek: &PeekBox) -> String {
    let mut out = String::from(SYNC_BEGIN);
    out.push_str("\x1b[?25l");
    let (top, bottom) = lay.region;
    for r in top..bottom {
        let in_box = r >= lay.box_top && r < lay.box_top + lay.box_height;
        if in_box {
            continue;
        }
        let src = if lay.below {
            r.checked_sub(lay.box_height)
        } else {
            Some(r + lay.box_height)
        };
        match src.filter(|&s| s < snap.rows.len()) {
            Some(s) => render::row(&mut out, r, &snap.rows[s]),
            None => write!(out, "\x1b[{};1H\x1b[0m\x1b[2K", r + 1).unwrap(),
        }
    }
    peek.draw(&mut out, lay, snap.cols);
    render::restore_cursor(&mut out, snap);
    out.push_str(SYNC_END);
    out
}

/// A frame that only refreshes the box (while text streams in).
pub fn box_frame(snap: &Snapshot, lay: &Layout, peek: &PeekBox) -> String {
    let mut out = String::from(SYNC_BEGIN);
    out.push_str("\x1b[?25l");
    peek.draw(&mut out, lay, snap.cols);
    render::restore_cursor(&mut out, snap);
    out.push_str(SYNC_END);
    out
}

/// Repaint the region exactly as it was when the box opened.
pub fn close_frame(snap: &Snapshot, lay: &Layout) -> String {
    let mut out = String::new();
    let (top, bottom) = (lay.region.0, lay.region.1.min(snap.rows.len()));
    render::rows(&mut out, top, &snap.rows[top..bottom]);
    render::restore_cursor(&mut out, snap);
    out
}

fn clip(s: &str, max: usize) -> String {
    if s.width() <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw + 1 > max {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.push('…');
    out
}

/// Minimal markdown: **bold**, `code`, fenced code blocks, headings as bold.
fn markdown_lines(text: &str, width: usize) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut in_fence = false;
    for raw in text.split('\n') {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            lines.push(vec![(Style::Code, clip(raw, width))]);
            continue;
        }
        if raw.trim().is_empty() {
            lines.push(Vec::new());
            continue;
        }
        let (heading, body) = match trimmed.strip_prefix('#') {
            Some(_) => (true, trimmed.trim_start_matches('#').trim_start()),
            None => (false, raw),
        };
        let mut spans = inline_spans(body);
        if heading {
            for s in &mut spans {
                s.0 = Style::Bold;
            }
        }
        lines.extend(wrap(&spans, width));
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

fn inline_spans(s: &str) -> Vec<(Style, String)> {
    let mut spans = Vec::new();
    let mut cur = String::new();
    let mut bold = false;
    let mut code = false;
    let mut chars = s.chars().peekable();
    let style = |bold: bool, code: bool| {
        if code {
            Style::Code
        } else if bold {
            Style::Bold
        } else {
            Style::Plain
        }
    };
    while let Some(c) = chars.next() {
        if c == '`' {
            spans.push((style(bold, code), std::mem::take(&mut cur)));
            code = !code;
        } else if c == '*' && !code && chars.peek() == Some(&'*') {
            chars.next();
            spans.push((style(bold, code), std::mem::take(&mut cur)));
            bold = !bold;
        } else {
            cur.push(c);
        }
    }
    spans.push((style(bold, code), cur));
    spans.retain(|(_, t)| !t.is_empty());
    spans
}

/// Word-wrap styled spans to `width` columns, keeping each word's style.
fn wrap(spans: &[(Style, String)], width: usize) -> Vec<Line> {
    let width = width.max(8);
    let mut lines = vec![Vec::new()];
    let mut col = 0;
    let indent = spans
        .first()
        .map(|(_, t)| {
            let lead = t.len() - t.trim_start().len();
            let bullet = ["- ", "* ", "• "]
                .iter()
                .any(|b| t.trim_start().starts_with(b));
            lead + if bullet { 2 } else { 0 }
        })
        .unwrap_or(0)
        .min(width / 2);

    for (style, text) in spans {
        let pieces = text.split(' ');
        let mut first = true;
        for word in pieces {
            let piece = if first {
                word.to_string()
            } else {
                format!(" {word}")
            };
            first = false;
            let w = piece.width();
            if col + w > width && col > indent {
                lines.push(vec![(Style::Plain, " ".repeat(indent))]);
                col = indent;
                let word = piece.trim_start().to_string();
                col += word.width();
                push(lines.last_mut().unwrap(), *style, word);
            } else if w > width {
                // A single word longer than the line: hard-break it.
                let mut chunk = String::new();
                for ch in piece.chars() {
                    let cw = ch.width().unwrap_or(0);
                    if col + cw > width {
                        push(
                            lines.last_mut().unwrap(),
                            *style,
                            std::mem::take(&mut chunk),
                        );
                        lines.push(Vec::new());
                        col = 0;
                    }
                    chunk.push(ch);
                    col += cw;
                }
                push(lines.last_mut().unwrap(), *style, chunk);
            } else {
                col += w;
                push(lines.last_mut().unwrap(), *style, piece);
            }
        }
    }
    lines
}

fn push(line: &mut Line, style: Style, text: String) {
    if text.is_empty() {
        return;
    }
    match line.last_mut() {
        Some((s, t)) if *s == style => t.push_str(&text),
        _ => line.push((style, text)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_prefers_below_then_above() {
        let l = layout(40, 5, 6);
        assert!(l.below);
        assert_eq!((l.box_top, l.region), (7, (7, 40)));
        let l = layout(40, 37, 38);
        assert!(!l.below);
        assert_eq!(l.box_top + l.box_height, 37);
        assert_eq!(l.region, (0, 37));
    }

    #[test]
    fn wraps_and_styles() {
        let lines = markdown_lines(
            "An **orphaned process group** uses `kill(0, SIGTSTP)` here.",
            20,
        );
        assert!(lines.len() > 1);
        assert!(
            lines
                .iter()
                .flatten()
                .any(|(s, t)| *s == Style::Bold && t.contains("orphaned"))
        );
        assert!(
            lines
                .iter()
                .flatten()
                .any(|(s, t)| *s == Style::Code && t.contains("kill(0,"))
        );
        for l in &lines {
            let w: usize = l.iter().map(|(_, t)| t.width()).sum();
            assert!(w <= 20, "line too wide: {l:?}");
        }
    }
}
