//! The peek box: where it goes, what it looks like, and the exact bytes that
//! open and close it without leaving a trace.

use std::fmt::Write;
use std::ops::Range;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::render::{self, SYNC_BEGIN, SYNC_END};
use crate::shadow::Snapshot;

/// Where the box sits and which screen rows peekme repaints.
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
    layout_within(0, rows, sel_first, sel_last)
}

/// Like `layout`, using only rows `top..rows`: the rows above `top` hold
/// output from before the child started (the shell prompt, say) that peekme
/// has never seen, so it must not paint over them.
pub fn layout_within(top: usize, rows: usize, sel_first: usize, sel_last: usize) -> Layout {
    let want = ((rows - top.min(rows)) * 2 / 5).clamp(5, 14);
    let space_below = rows.saturating_sub(sel_last + 1);
    let space_above = sel_first.saturating_sub(top);
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
            region: (top, sel_first),
            below: false,
        }
    } else {
        // Tiny screen: cover the top rows we know.
        let top = top.min(rows.saturating_sub(1));
        let h = want.min(rows - top);
        Layout {
            box_top: top,
            box_height: h,
            region: (top, top + h),
            below: true,
        }
    }
}

/// The same placement with a smaller box, still touching the selection.
/// The region is unchanged, so rows the box gives back are redrawn in place.
pub fn shrink(lay: &Layout, height: usize) -> Layout {
    let height = height.min(lay.box_height);
    let box_top = if lay.below {
        lay.box_top
    } else {
        lay.box_top + lay.box_height - height
    };
    Layout {
        box_top,
        box_height: height,
        ..*lay
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

/// One char of the box text: its style and where it is in the answer (none
/// for what the box adds: indents, the space between words, "…").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Glyph {
    c: char,
    style: Style,
    src: Option<usize>,
}

type Line = Vec<Glyph>;

/// `text` as glyphs, its first char at `src` of the answer when it is from it.
fn glyphs(style: Style, text: &str, src: Option<usize>) -> Line {
    text.chars()
        .enumerate()
        .map(|(i, c)| Glyph {
            c,
            style,
            src: src.map(|s| s + i),
        })
        .collect()
}

fn width_of(line: &[Glyph]) -> usize {
    line.iter().map(|g| g.c.width().unwrap_or(0)).sum()
}

/// A screen position in a box's text: (body line, column).
pub type Pos = (usize, usize);

/// What a box draws over its text: the words of its nested answers (char
/// ranges of the answer), the one under the pointer, and the selection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Decor {
    pub marks: Vec<Range<usize>>,
    pub hover: Option<Range<usize>>,
    /// From and to, both included, in order.
    pub selection: Option<(Pos, Pos)>,
}

/// One box of a nest, outermost first.
pub struct Level<'a> {
    pub peek: &'a PeekBox,
    pub decor: &'a Decor,
    /// Rows of the box, borders included.
    pub height: usize,
    /// The body line under which the next level's box sits.
    pub anchor: usize,
}

/// A box a screen row crosses, and the body line of it the row shows (none
/// on a border, a blank row or a row of an inner box).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Part {
    pub level: usize,
    pub line: Option<usize>,
}

/// What a cell of a nest is: the box, and the text position when it is on text.
pub fn spot(parts: &[Part], cols: usize, col: usize) -> Option<(usize, Option<Pos>)> {
    parts.iter().rev().find_map(|p| {
        let (left, right) = (2 * p.level, cols.saturating_sub(2 * p.level));
        if !(left..right).contains(&col) {
            return None;
        }
        let text = left + 2..right.saturating_sub(2);
        Some((
            p.level,
            p.line
                .filter(|_| text.contains(&col))
                .map(|l| (l, col - text.start)),
        ))
    })
}

/// Text columns of the box at `level` of a nest on a `cols`-wide screen.
pub fn text_width(cols: usize, level: usize) -> usize {
    cols.saturating_sub(4 * (level + 1))
}

/// One screen row of a nest: its bytes, drawn from the box's left edge.
struct Row {
    text: String,
    parts: Vec<Part>,
}

enum BodyRow {
    Own(usize),
    Inner(Row),
}

const BORDER: &str = "\x1b[0;36m";

/// The content of a peek box.
#[derive(Clone)]
pub struct PeekBox {
    pub title: String,
    pub model: String,
    pub text: String,
    pub status: Status,
    pub scroll: usize,
    pub waiting_updates: usize,
    /// Who the waiting updates come from ("Codex", "Claude").
    pub child_name: &'static str,
    /// Offer "Alt+P again" to re-explain with the whole conversation.
    pub deep_available: bool,
    /// The whole chat is big: the next Alt+P confirms sending it.
    pub confirm_deep: bool,
    /// A short notice in the bottom border.
    pub note: Option<&'static str>,
    /// The question being typed (Alt+Shift+P), until Enter sends it.
    pub input: Option<String>,
    /// The question the text answers, shown above it.
    pub question: Option<String>,
    /// Offer Alt+Shift+P to ask a question about the selection.
    pub ask_available: bool,
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
            child_name: "Codex",
            deep_available: false,
            confirm_deep: false,
            note: None,
            input: None,
            question: None,
            ask_available: false,
        }
    }

    pub fn message(text: &str) -> Self {
        Self {
            title: "peekme".into(),
            model: String::new(),
            text: String::new(),
            status: Status::Message(text.into()),
            scroll: 0,
            waiting_updates: 0,
            child_name: "Codex",
            deep_available: false,
            confirm_deep: false,
            note: None,
            input: None,
            question: None,
            ask_available: false,
        }
    }

    fn body_lines(&self, width: usize) -> Vec<Line> {
        if let Some(input) = &self.input {
            let mut l = wrap(
                &glyphs(Style::Dim, "Your question about this text:", None),
                width,
            );
            let mut line = glyphs(Style::Bold, "› ", None);
            line.extend(glyphs(Style::Plain, &sanitize(input), None));
            line.extend(glyphs(Style::Dim, "▏", None));
            l.extend(wrap(&line, width));
            return l;
        }
        let mut l = match &self.question {
            Some(q) => {
                let mut l = wrap(
                    &glyphs(Style::Dim, &format!("› {}", sanitize(q)), None),
                    width,
                );
                l.push(Vec::new());
                l
            }
            None => Vec::new(),
        };
        l.extend(self.answer_lines(width));
        l
    }

    fn answer_lines(&self, width: usize) -> Vec<Line> {
        let text = sanitize(&self.text);
        match &self.status {
            Status::Message(m) => wrap(&glyphs(Style::Plain, &sanitize(m), None), width),
            Status::Thinking if self.text.is_empty() => {
                vec![glyphs(Style::Dim, "thinking…", None)]
            }
            Status::Error(e) => {
                let mut l = markdown_lines(&text, width);
                l.extend(wrap(
                    &glyphs(Style::Error, &format!("error: {}", sanitize(e)), None),
                    width,
                ));
                l
            }
            _ => markdown_lines(&text, width),
        }
    }

    /// Lines of text when it is `width` columns wide.
    pub fn line_count(&self, width: usize) -> usize {
        self.body_lines(width).len()
    }

    /// Rows the box needs to show all of its text (borders included), at least 3.
    pub fn fitted_height(&self, cols: usize) -> usize {
        self.body_lines(cols.saturating_sub(4)).len().max(1) + 2
    }

    /// Max scroll offset for a box of `height` rows on a `cols`-wide screen.
    pub fn max_scroll(&self, cols: usize, height: usize) -> usize {
        let inner = height.saturating_sub(2);
        self.body_lines(cols.saturating_sub(4))
            .len()
            .saturating_sub(inner)
    }

    /// The answer char shown at `pos` when the text is `width` columns wide.
    pub fn src_at(&self, width: usize, pos: Pos) -> Option<usize> {
        let lines = self.body_lines(width);
        let mut col = 0;
        for g in lines.get(pos.0)? {
            let w = g.c.width().unwrap_or(0);
            if pos.1 < col + w.max(1) {
                return g.src;
            }
            col += w;
        }
        None
    }

    /// The word shown at `pos`: where it starts and where its last char is.
    pub fn word_at(&self, width: usize, pos: Pos) -> Option<(Pos, Pos)> {
        let lines = self.body_lines(width);
        let line = lines.get(pos.0)?;
        let mut cols = Vec::with_capacity(line.len());
        let mut col = 0;
        for g in line {
            cols.push(col);
            col += g.c.width().unwrap_or(0);
        }
        let i = cols.iter().rposition(|&c| c <= pos.1)?;
        let word = |g: &Glyph| !g.c.is_whitespace();
        if !word(&line[i]) {
            return None;
        }
        let a = (0..i)
            .rev()
            .take_while(|&k| word(&line[k]))
            .last()
            .unwrap_or(i);
        let b = (i + 1..line.len())
            .take_while(|&k| word(&line[k]))
            .last()
            .unwrap_or(i);
        Some(((pos.0, cols[a]), (pos.0, cols[b])))
    }

    /// The text shown from `from` to `to` (both included), wrapped lines joined
    /// by a space, and the chars of the answer it covers.
    pub fn shown(&self, width: usize, from: Pos, to: Pos) -> (String, Option<Range<usize>>) {
        let lines = self.body_lines(width);
        let mut text = String::new();
        let mut src: Option<Range<usize>> = None;
        for (l, line) in lines.iter().enumerate().take(to.0 + 1).skip(from.0) {
            if l > from.0 && !text.ends_with(' ') {
                text.push(' ');
            }
            let mut col = 0;
            for g in line {
                let at = (l, col);
                col += g.c.width().unwrap_or(0);
                if at < from || at > to || (at.1 == 0 && g.c == ' ' && text.is_empty()) {
                    continue;
                }
                text.push(g.c);
                if let Some(i) = g.src {
                    src = Some(match src {
                        Some(r) => r.start.min(i)..r.end.max(i + 1),
                        None => i..i + 1,
                    });
                }
            }
        }
        (text.trim().to_string(), src)
    }

    /// Bytes for the box rows only.
    pub fn draw(&self, out: &mut String, lay: &Layout, cols: usize) {
        let level = Level {
            peek: self,
            decor: &Decor::default(),
            height: lay.box_height,
            anchor: 0,
        };
        draw_levels(out, lay, cols, &[level]);
    }

    /// The top border: ╭─ peek · "selection" ──── model ─╮
    fn top_border(&self, width: usize) -> String {
        let right = if self.model.is_empty() {
            String::new()
        } else {
            format!(" {} ", sanitize(&self.model))
        };
        let budget = width.saturating_sub(8 + right.width());
        let title = format!(
            " peek · {} ",
            clip(&sanitize(&self.title), budget.saturating_sub(9))
        );
        let fill = width.saturating_sub(3 + title.width() + right.width());
        format!("{BORDER}╭─{title}{}{right}╮", "─".repeat(fill))
    }

    /// The bottom border with hints. Only the innermost box takes keys.
    fn bottom_border(
        &self,
        width: usize,
        body: usize,
        inner_h: usize,
        scroll: usize,
        k: usize,
        innermost: bool,
    ) -> String {
        let mut hints: Vec<String> = self.note.iter().map(|n| n.to_string()).collect();
        if innermost && self.input.is_some() {
            hints.push("Enter ask".into());
            hints.push("Esc cancel".into());
        } else if innermost {
            hints.push("Esc close".to_string());
            if self.deep_available {
                hints.push("Alt+P again: use whole chat".into());
            }
            if self.ask_available {
                hints.push("Alt+Shift+P: ask".into());
            }
            if self.confirm_deep {
                hints.push("Alt+P: send anyway".into());
            }
        }
        if body > inner_h {
            let keys = if innermost { "PgUp/PgDn " } else { "" };
            hints.push(format!("{keys}{}/{body}", (scroll + inner_h).min(body)));
        }
        match &self.status {
            Status::Thinking | Status::Streaming => hints.push("…".into()),
            _ => {}
        }
        if self.waiting_updates > 0 && k == 0 {
            let n = self.waiting_updates;
            let s = if n == 1 { "" } else { "s" };
            hints.push(format!("{}: {n} update{s} waiting", self.child_name));
        }
        let hint = if hints.is_empty() {
            String::new()
        } else {
            clip(&format!(" {} ", hints.join(" · ")), width.saturating_sub(4))
        };
        let fill = width.saturating_sub(3 + hint.width());
        format!("{BORDER}╰{}{hint}─╯\x1b[0m", "─".repeat(fill))
    }
}

/// Draw a nest of boxes, `levels[0]` filling `lay`. Returns, per box row, the
/// boxes and lines it shows.
pub fn draw_levels(
    out: &mut String,
    lay: &Layout,
    cols: usize,
    levels: &[Level],
) -> Vec<Vec<Part>> {
    if levels.is_empty() {
        return Vec::new();
    }
    let rows = box_rows(levels, 0, cols);
    for (i, row) in rows.iter().enumerate() {
        write!(out, "\x1b[{};1H{}", lay.box_top + 1 + i, row.text).unwrap();
    }
    rows.into_iter().map(|r| r.parts).collect()
}

/// How far the box at `k` can scroll: its own lines plus the inner box.
pub fn scroll_limit(levels: &[Level], k: usize, cols: usize) -> usize {
    let width = cols.saturating_sub(4 * k);
    let lines = levels[k].peek.body_lines(width.saturating_sub(4)).len();
    let inner = levels.get(k + 1).map_or(0, |l| l.height);
    (lines + inner).saturating_sub(levels[k].height.saturating_sub(2))
}

/// The rows of box `k`, `width` columns wide, with the boxes inside it.
fn box_rows(levels: &[Level], k: usize, width: usize) -> Vec<Row> {
    let lv = &levels[k];
    let tw = width.saturating_sub(4);
    let lines = lv.peek.body_lines(tw);
    let mut inner = (k + 1 < levels.len()).then(|| box_rows(levels, k + 1, tw));
    let mut body = Vec::new();
    for i in 0..lines.len() {
        body.push(BodyRow::Own(i));
        if i == lv.anchor.min(lines.len() - 1)
            && let Some(rows) = inner.take()
        {
            body.extend(rows.into_iter().map(BodyRow::Inner));
        }
    }
    if let Some(rows) = inner {
        body.extend(rows.into_iter().map(BodyRow::Inner));
    }
    let inner_h = lv.height.saturating_sub(2);
    let scroll = lv.peek.scroll.min(body.len().saturating_sub(inner_h));
    let edge = |line| vec![Part { level: k, line }];

    let mut rows = vec![Row {
        text: lv.peek.top_border(width),
        parts: edge(None),
    }];
    for i in 0..inner_h {
        rows.push(match body.get(scroll + i) {
            Some(BodyRow::Own(l)) => {
                let mut text = format!("{BORDER}│ ");
                let used = draw_line(&mut text, &lines[*l], *l, lv.decor);
                write!(
                    text,
                    "\x1b[0m{}{BORDER} │",
                    " ".repeat(tw.saturating_sub(used))
                )
                .unwrap();
                Row {
                    text,
                    parts: edge(Some(*l)),
                }
            }
            Some(BodyRow::Inner(row)) => {
                let mut parts = edge(None);
                parts.extend(&row.parts);
                Row {
                    text: format!("{BORDER}│ {}{BORDER} │", row.text),
                    parts,
                }
            }
            None => Row {
                text: format!("{BORDER}│ \x1b[0m{}{BORDER} │", " ".repeat(tw)),
                parts: edge(None),
            },
        });
    }
    rows.push(Row {
        text: lv
            .peek
            .bottom_border(width, body.len(), inner_h, scroll, k, k + 1 == levels.len()),
        parts: edge(None),
    });
    rows
}

/// One body line with the marks and the selection over it; returns its width.
fn draw_line(out: &mut String, line: &[Glyph], l: usize, decor: &Decor) -> usize {
    let within = |r: &Range<usize>, i: usize| -> bool {
        // A gap the box added between two chars of the same words is in them.
        let src = line[i].src.map(|s| (s, s)).or_else(|| {
            let before = line[..i].iter().rev().find_map(|g| g.src)?;
            let after = line[i + 1..].iter().find_map(|g| g.src)?;
            Some((before, after))
        });
        src.is_some_and(|(a, b)| r.contains(&a) && r.contains(&b))
    };
    let mut col = 0;
    let mut pen = String::new();
    for (i, g) in line.iter().enumerate() {
        let hover = decor.hover.as_ref().is_some_and(|r| within(r, i));
        let mark = decor.marks.iter().any(|r| within(r, i));
        let selected = decor
            .selection
            .is_some_and(|(a, b)| a <= (l, col) && (l, col) <= b);
        let mut sgr = g.style.sgr().to_string();
        if hover {
            sgr.push_str("\x1b[4m");
        } else if mark {
            sgr.push_str("\x1b[4:4m");
        }
        if selected {
            sgr.push_str("\x1b[7m");
        }
        if sgr != pen {
            out.push_str(&sgr);
            pen = sgr;
        }
        out.push(g.c);
        col += g.c.width().unwrap_or(0);
    }
    col
}

/// The frame that shows the box: the region repainted with the box inserted and
/// the surrounding rows shifted away from it.
pub fn open_frame(snap: &Snapshot, lay: &Layout, peek: &PeekBox) -> String {
    let level = Level {
        peek,
        decor: &Decor::default(),
        height: lay.box_height,
        anchor: 0,
    };
    nest_frame(snap, lay, &[level], true).0
}

/// A frame with a nest of boxes, `levels[0]` filling `lay`: all of the region
/// when `full` (it opened or changed size), else only the box rows. Also
/// returns what each box row shows.
pub fn nest_frame(
    snap: &Snapshot,
    lay: &Layout,
    levels: &[Level],
    full: bool,
) -> (String, Vec<Vec<Part>>) {
    let mut out = String::from(SYNC_BEGIN);
    out.push_str("\x1b[?25l");
    if full {
        shift_rows(&mut out, snap, lay);
    }
    let parts = draw_levels(&mut out, lay, snap.cols, levels);
    hide_cursor_at_rest(&mut out, snap);
    out.push_str(SYNC_END);
    (out, parts)
}

/// The region's rows outside the box, shifted away from it.
fn shift_rows(out: &mut String, snap: &Snapshot, lay: &Layout) {
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
            Some(s) => render::row(out, r, &snap.rows[s]),
            None => write!(out, "\x1b[{};1H\x1b[0m\x1b[2K", r + 1).unwrap(),
        }
    }
}

/// While the box is open the cursor stays hidden: its row may now be inside the
/// box. The pen and position are still put back for the child's next write.
fn hide_cursor_at_rest(out: &mut String, snap: &Snapshot) {
    render::restore_cursor(out, snap);
    out.push_str("\x1b[?25l");
}

/// A frame that only refreshes the box (while text streams in).
pub fn box_frame(snap: &Snapshot, lay: &Layout, peek: &PeekBox) -> String {
    let level = Level {
        peek,
        decor: &Decor::default(),
        height: lay.box_height,
        anchor: 0,
    };
    nest_frame(snap, lay, &[level], false).0
}

/// The same placement with a box of `height` rows, as far as the region has
/// room. The region is unchanged, so one full frame redraws all of it.
pub fn resize(lay: &Layout, height: usize) -> Layout {
    let height = height.min(lay.region.1 - lay.region.0);
    let box_top = if lay.below {
        lay.box_top
    } else {
        lay.region.1 - height
    };
    Layout {
        box_top,
        box_height: height,
        ..*lay
    }
}

/// Repaint the region exactly as it was when the box opened.
pub fn close_frame(snap: &Snapshot, lay: &Layout) -> String {
    let mut out = String::new();
    let (top, bottom) = (lay.region.0, lay.region.1.min(snap.rows.len()));
    render::rows(&mut out, top, &snap.rows[top..bottom]);
    render::restore_cursor(&mut out, snap);
    out
}

/// Text from the model, the selection or an error message is untrusted: it
/// could carry terminal control sequences (an OSC 52 clipboard write, say) or
/// bidi overrides that reorder what is shown. Keep newlines, turn tabs into
/// spaces, and drop every other control character.
pub fn sanitize(s: &str) -> String {
    s.chars()
        .filter_map(|c| match c {
            '\n' => Some('\n'),
            '\t' => Some(' '),
            c if c.is_control() => None, // C0, DEL and C1 (U+0080-U+009F)
            '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' => None,
            c => Some(c),
        })
        .collect()
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
    let mut at = 0;
    for raw in text.split('\n') {
        let start = at;
        let len = raw.chars().count();
        at += len + 1;
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            lines.push(clip_line(glyphs(Style::Code, raw, Some(start)), width));
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
        // `body` ends where `raw` does.
        let mut line = inline_glyphs(body, start + len - body.chars().count());
        if heading {
            for g in &mut line {
                g.style = Style::Bold;
            }
        }
        lines.extend(wrap(&line, width));
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

/// `line` cut to `max` columns, with "…" when cut.
fn clip_line(line: Line, max: usize) -> Line {
    if width_of(&line) <= max {
        return line;
    }
    let mut out = Vec::new();
    let mut w = 0;
    for g in &line {
        let cw = g.c.width().unwrap_or(0);
        if w + cw + 1 > max {
            break;
        }
        out.push(*g);
        w += cw;
    }
    let style = line[0].style;
    out.push(Glyph {
        c: '…',
        style,
        src: None,
    });
    out
}

/// `s` without its markdown markers, styled; its first char is char `at` of
/// the answer.
fn inline_glyphs(s: &str, at: usize) -> Line {
    let chars: Vec<char> = s.chars().collect();
    let mut out = Vec::new();
    let mut bold = false;
    let mut code = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c == '`' {
            code = !code;
            i += 1;
            continue;
        }
        if c == '*' && !code && chars.get(i + 1) == Some(&'*') {
            bold = !bold;
            i += 2;
            continue;
        }
        let style = if code {
            Style::Code
        } else if bold {
            Style::Bold
        } else {
            Style::Plain
        };
        out.push(Glyph {
            c,
            style,
            src: Some(at + i),
        });
        i += 1;
    }
    out
}

/// Word-wrap a line to `width` columns, keeping each word's style.
fn wrap(line: &[Glyph], width: usize) -> Vec<Line> {
    let width = width.max(8);
    let lead = line.iter().take_while(|g| g.c.is_whitespace()).count();
    let start: String = line[lead..].iter().take(2).map(|g| g.c).collect();
    let bullet = ["- ", "* ", "• "].contains(&start.as_str());
    let indent = (lead + if bullet { 2 } else { 0 }).min(width / 2);
    let space = Glyph {
        c: ' ',
        style: Style::Plain,
        src: None,
    };
    let pad = |n: usize| vec![space; n];

    // Words are split on spaces only, so a word may span several styles
    // ("`main`," is one word): punctuation never starts a line on its own.
    let mut words: Vec<Line> = vec![Vec::new()];
    let mut lead_spaces = 0;
    for g in line {
        if g.c == ' ' {
            if words.last().is_some_and(|w| !w.is_empty()) {
                words.push(Vec::new());
            } else if words.len() == 1 {
                lead_spaces += 1;
            }
        } else {
            words.last_mut().unwrap().push(*g);
        }
    }

    let mut lines: Vec<Line> = vec![Vec::new()];
    let mut col = 0;
    if lead_spaces > 0 {
        col = lead_spaces.min(width / 2);
        lines[0] = pad(col);
    }
    for word in words.into_iter().filter(|w| !w.is_empty()) {
        let w = width_of(&word);
        let sep = usize::from(
            col > 0 && !(lead_spaces > 0 && col == lead_spaces.min(width / 2) && lines.len() == 1),
        );
        if col + sep + w > width && col > indent {
            lines.push(pad(indent));
            col = indent;
        } else if sep == 1 {
            lines.last_mut().unwrap().push(space);
            col += 1;
        }
        if col + w <= width {
            col += w;
            lines.last_mut().unwrap().extend(word);
            continue;
        }
        // A single word longer than the line: hard-break it.
        for g in word {
            let cw = g.c.width().unwrap_or(0);
            if col + cw > width {
                lines.push(Vec::new());
                col = 0;
            }
            lines.last_mut().unwrap().push(g);
            col += cw;
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(line: &[Glyph]) -> String {
        line.iter().map(|g| g.c).collect()
    }

    /// The screen a frame draws, as text rows.
    fn screen(frame: &str, cols: u16, rows: u16) -> Vec<String> {
        let mut s = crate::shadow::Shadow::new(cols, rows);
        s.advance(frame.as_bytes());
        let snap = s.snapshot();
        (0..rows as usize)
            .map(|r| snap.rows_text(r, r + 1).trim_end().to_string())
            .collect()
    }

    #[test]
    fn layout_keeps_out_of_rows_it_never_saw() {
        // The child started on row 20; rows above hold the user's shell output.
        let l = layout_within(20, 40, 35, 36);
        assert!(!l.below);
        assert!(l.region.0 >= 20 && l.box_top >= 20, "{l:?}");
        let l = layout_within(20, 40, 22, 38);
        assert!(l.box_top >= 20 && l.region.0 >= 20, "{l:?}");
    }

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
    fn untrusted_text_cannot_reach_the_terminal_as_controls() {
        let evil = "ok \x1b]52;c;aGVsbG8=\x07 \u{9b}31m \x1b[2J\r\x08 \u{202e}txt\tend";
        assert_eq!(sanitize(evil), "ok ]52;c;aGVsbG8= 31m [2J txt end");
        let mut peek = PeekBox::new(evil);
        peek.text = evil.into();
        peek.model = evil.into();
        peek.status = Status::Error(evil.into());
        let mut out = String::new();
        let lay = layout(20, 2, 2);
        peek.draw(&mut out, &lay, 60);
        // Our own frame only uses CSI cursor moves and SGR; no OSC, no C1, no erase.
        assert!(!out.contains("\x1b]") && !out.contains('\u{9b}') && !out.contains("\x1b[2J"));
        assert!(!out.contains('\u{202e}') && !out.contains('\x07') && !out.contains('\x08'));
    }

    #[test]
    fn punctuation_stays_with_its_word() {
        let text = "one straight sequence on top of the current `main`, without merge commits";
        for width in 20..60 {
            for line in markdown_lines(text, width) {
                let s = plain(&line);
                assert!(
                    !s.trim_start().starts_with(','),
                    "line starts with a comma at width {width}: {s:?}"
                );
                assert!(s.width() <= width);
            }
        }
    }

    #[test]
    fn wraps_and_styles() {
        let lines = markdown_lines(
            "An **orphaned process group** uses `kill(0, SIGTSTP)` here.",
            20,
        );
        assert!(lines.len() > 1);
        let styled = |style| -> String {
            lines
                .iter()
                .flatten()
                .filter(|g| g.style == style)
                .map(|g| g.c)
                .collect()
        };
        assert!(styled(Style::Bold).contains("orphaned"));
        assert!(styled(Style::Code).contains("kill(0,"));
        for l in &lines {
            assert!(width_of(l) <= 20, "line too wide: {l:?}");
        }
    }

    #[test]
    fn box_text_knows_where_it_is_in_the_answer() {
        let mut peek = PeekBox::new("x");
        peek.status = Status::Done;
        peek.text = "# Head\nAn **orphaned process group** has `no` parent.".into();
        let lines = peek.body_lines(16);
        assert_eq!(plain(&lines[0]), "Head");
        assert_eq!(plain(&lines[1]), "An orphaned");
        // "orphaned" starts at char 12 of the answer: "# Head\nAn **" is 12 chars.
        assert_eq!(peek.src_at(16, (1, 3)), Some(12));
        assert_eq!(peek.src_at(16, (1, 2)), None, "the space the box put there");
        let (text, src) = peek.shown(16, (1, 3), (2, 6));
        assert_eq!(text, "orphaned process");
        let answer: Vec<char> = peek.text.chars().collect();
        let src = src.unwrap();
        assert_eq!(answer[src].iter().collect::<String>(), "orphaned process");
        // Wide chars take two columns.
        peek.text = "你好 world".into();
        assert_eq!(peek.src_at(20, (0, 3)), Some(1));
        assert_eq!(peek.src_at(20, (0, 5)), Some(3));
    }

    #[test]
    fn boxes_nest_and_each_scrolls() {
        let cols = 40;
        let mut outer = PeekBox::new("the PTY layer");
        outer.status = Status::Done;
        outer.text = (1..=8)
            .map(|i| format!("outer {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let mut inner = PeekBox::new("job control");
        inner.status = Status::Done;
        inner.text = (1..=5)
            .map(|i| format!("inner {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let decor = Decor::default();
        let lay = Layout {
            box_top: 0,
            box_height: 10,
            region: (0, 10),
            below: true,
        };
        let draw = |outer: &PeekBox, inner: &PeekBox| {
            let levels = [
                Level {
                    peek: outer,
                    decor: &decor,
                    height: 10,
                    anchor: 1,
                },
                Level {
                    peek: inner,
                    decor: &decor,
                    height: 4,
                    anchor: 0,
                },
            ];
            let mut out = String::new();
            let parts = draw_levels(&mut out, &lay, cols, &levels);
            (
                screen(&out, cols as u16, 10),
                parts,
                scroll_limit(&levels, 0, cols),
                scroll_limit(&levels, 1, cols),
            )
        };
        let (rows, parts, outer_max, inner_max) = draw(&outer, &inner);
        assert!(rows[0].starts_with("╭─ peek · the PTY layer"));
        assert_eq!(rows[2], "│ outer 2                              │");
        assert!(rows[3].starts_with("│ ╭─ peek · job control"), "{rows:#?}");
        assert_eq!(rows[4], "│ │ inner 1                          │ │");
        assert!(rows[6].starts_with("│ ╰") && rows[6].contains("PgUp/PgDn 2/5"));
        assert_eq!(rows[7], "│ outer 3                              │");
        assert!(rows[9].ends_with("8/12 ─╯"), "{}", rows[9]);
        assert_eq!((outer_max, inner_max), (4, 3));

        // What a click hits.
        assert_eq!(spot(&parts[2], cols, 4), Some((0, Some((1, 2)))));
        assert_eq!(spot(&parts[4], cols, 4), Some((1, Some((0, 0)))));
        assert_eq!(
            spot(&parts[4], cols, 1),
            Some((0, None)),
            "the outer border"
        );
        assert_eq!(
            spot(&parts[4], cols, 2),
            Some((1, None)),
            "the inner border"
        );

        // The inner box scrolls on its own; the outer one moves it with its line.
        let mut inner2 = PeekBox {
            scroll: 3,
            ..inner.clone()
        };
        let (rows, ..) = draw(&outer, &inner2);
        assert_eq!(rows[4], "│ │ inner 4                          │ │");
        let outer2 = PeekBox {
            scroll: 3,
            ..outer.clone()
        };
        inner2.scroll = 0;
        let (rows, parts, ..) = draw(&outer2, &inner2);
        // Outer rows 0-2 (two lines, the inner box's top) are scrolled away.
        assert_eq!(
            rows[1], "│ │ inner 1                          │ │",
            "{rows:#?}"
        );
        assert_eq!(spot(&parts[1], cols, 4), Some((1, Some((0, 0)))));
        assert_eq!(rows[4], "│ outer 3                              │");
    }

    #[test]
    fn marks_and_the_selection_are_drawn_over_the_text() {
        let mut peek = PeekBox::new("x");
        peek.status = Status::Done;
        peek.text = "see the job control here".into();
        let decor = Decor {
            marks: std::iter::once(8..19).collect(),
            hover: None,
            selection: Some(((0, 0), (0, 2))),
        };
        let lay = layout(10, 0, 0);
        let mut out = String::new();
        draw_levels(
            &mut out,
            &lay,
            40,
            &[Level {
                peek: &peek,
                decor: &decor,
                height: 3,
                anchor: 0,
            }],
        );
        let mut s = crate::shadow::Shadow::new(40, 10);
        s.advance(out.as_bytes());
        let snap = s.snapshot();
        let row = &snap.rows[lay.box_top + 1];
        let flagged = |f: Flags| -> String {
            row.iter()
                .filter(|c| c.flags.contains(f))
                .map(|c| c.c)
                .collect()
        };
        use alacritty_terminal::term::cell::Flags;
        assert_eq!(flagged(Flags::DOTTED_UNDERLINE), "job control");
        assert_eq!(flagged(Flags::INVERSE), "see");
    }
}
