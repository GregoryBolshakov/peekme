//! Codex CLI's layout (0.163.0): a port of its `markdown_render.rs` writer,
//! which walks pulldown-cmark's events like this one does. Headings keep
//! their `#`, lists get "•" and right-aligned numbers four columns deeper per
//! level, quotes a green "> ", a rule is "———", code blocks are not wrapped,
//! and tables are columns apart by two spaces under a "━" rule (or one
//! "Header  value" record per row when the columns would be too narrow).
//! Lines wrap with `textwrap`, as Codex's do, keeping URLs whole.
//!
//! Skipped: syntax colours of code with a language, Mermaid diagrams, TeX.

use std::ops::Range;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use unicode_width::UnicodeWidthChar;

use super::*;

/// Codex's colours in its default theme (as it draws them when it cannot
/// read the terminal's own colours).
pub(super) fn colors(truecolor: bool) -> Vec<(Fg, String)> {
    let rgb = |r, g, b| crate::color::sgr(crate::color::Color::Rgb(r, g, b), truecolor);
    vec![
        (Fg::Code, rgb(0xa6, 0xe3, 0xa1)),
        (Fg::Link, rgb(0x63, 0xa8, 0xf8)),
        (Fg::Header, rgb(0xf9, 0xe2, 0xaf)),
        (Fg::Quote, "38;5;2".into()),
        (Fg::Number, "38;5;12".into()),
    ]
}

/// The lines of `text` as Codex lays it out `width` columns wide: links
/// with their address after them (the box shows no hyperlinks).
pub(super) fn lines(text: &str, width: usize) -> Vec<Line> {
    render(text, width, true)
}

/// `show_urls`: false for a terminal where Codex shows a link's text alone.
fn render(text: &str, width: usize, show_urls: bool) -> Vec<Line> {
    let opts = Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TABLES | Options::ENABLE_TASKLISTS;
    let mut at = vec![0; text.len() + 1];
    let mut n = 0;
    for (b, c) in text.char_indices() {
        for k in 0..c.len_utf8() {
            at[b + k] = n;
        }
        n += 1;
    }
    at[text.len()] = n;
    // Consecutive text events as one (DecodedTextMerge).
    let mut events: Vec<(Ev, Range<usize>)> = Vec::new();
    for (ev, r) in Parser::new_ext(text, opts).into_offset_iter() {
        match ev {
            Event::Text(t) => {
                let glyphs = mapped(text, &at, &t, r.clone());
                if let Some((Ev::Text(prev), pr)) = events.last_mut() {
                    prev.extend(glyphs);
                    pr.end = r.end;
                } else {
                    events.push((Ev::Text(glyphs), r));
                }
            }
            Event::Code(t) => events.push((Ev::Code(mapped(text, &at, &t, r.clone())), r)),
            Event::Html(t) => events.push((Ev::Html(mapped(text, &at, &t, r.clone()), false), r)),
            Event::InlineHtml(t) => {
                events.push((Ev::Html(mapped(text, &at, &t, r.clone()), true), r))
            }
            Event::InlineMath(t) | Event::DisplayMath(t) => {
                events.push((Ev::Text(mapped(text, &at, &t, r.clone())), r))
            }
            Event::SoftBreak => events.push((Ev::Soft, r)),
            Event::HardBreak => events.push((Ev::Hard, r)),
            Event::Rule => events.push((Ev::Rule, r)),
            Event::TaskListMarker(done) => events.push((Ev::Task(done), r)),
            Event::FootnoteReference(_) => {}
            Event::Start(tag) => events.push((Ev::Start(tag.into_static()), r)),
            Event::End(tag) => events.push((Ev::End(tag), r)),
        }
    }
    let mut w = Writer::new(text, width, show_urls);
    for (ev, r) in events {
        w.event(ev, r);
    }
    w.flush();
    // A line's own style shows in full but on the answer's first line, where
    // Codex's bullet in front of it leaves only its colour.
    let lines: Vec<Line> = w
        .text
        .into_iter()
        .zip(w.styles)
        .enumerate()
        .map(|(k, (line, style))| {
            if k == 0 {
                line_styled(line, style)
            } else {
                styled(line, style)
            }
        })
        .collect();
    // The chat cell wraps what is still wider (code lines) by words.
    let mut out = Vec::new();
    for line in lines {
        if width_of(&line) > width {
            out.extend(wrap(&line, width, &[], &[]));
        } else {
            out.push(line);
        }
    }
    out
}

/// `t` as glyphs at their places in bytes `r` of `text`.
fn mapped(text: &str, at: &[usize], t: &str, r: Range<usize>) -> Line {
    let r = r.start.min(text.len())..r.end.min(text.len());
    if &text[r.clone()] == t {
        return glyphs(Style::PLAIN, t, Some(at[r.start]));
    }
    let mut out = Line::new();
    let mut pos = r.start;
    for c in t.chars() {
        let found = text
            .get(pos..r.end)
            .and_then(|s| s.find(c))
            .map(|b| pos + b);
        if let Some(p) = found {
            pos = p + c.len_utf8();
        }
        out.push(Glyph {
            c,
            style: Style::PLAIN,
            src: found.map(|p| at[p]),
        });
    }
    out
}

enum Ev {
    Text(Line),
    Code(Line),
    /// Raw HTML, and whether it is inline.
    Html(Line, bool),
    Soft,
    Hard,
    Rule,
    Task(bool),
    Start(Tag<'static>),
    End(TagEnd),
}

/// `patch`: `b` over `a` (its colour if it has one, the flags of both).
fn patch(a: Style, b: Style) -> Style {
    Style {
        fg: if b.fg != Fg::Plain { b.fg } else { a.fg },
        bold: a.bold || b.bold,
        dim: a.dim || b.dim,
        italic: a.italic || b.italic,
        underline: a.underline || b.underline,
        strike: a.strike || b.strike,
    }
}

fn styled(mut line: Line, style: Style) -> Line {
    for g in &mut line {
        g.style = patch(style, g.style);
    }
    line
}

/// A line's own style: only its colour shows, where its text has none.
fn line_styled(mut line: Line, style: Style) -> Line {
    for g in &mut line {
        if g.style.fg == Fg::Plain {
            g.style.fg = style.fg;
        }
    }
    line
}

const H1: Style = Style {
    bold: true,
    underline: true,
    ..Style::PLAIN
};
const H2: Style = Style::BOLD;
const H3: Style = Style {
    bold: true,
    italic: true,
    ..Style::PLAIN
};
const H4: Style = Style {
    italic: true,
    ..Style::PLAIN
};
const CODE: Style = Style::CODE;
const LINK: Style = Style {
    fg: Fg::Link,
    underline: true,
    ..Style::PLAIN
};
const QUOTE: Style = Style {
    fg: Fg::Quote,
    ..Style::PLAIN
};
const NUMBER: Style = Style {
    fg: Fg::Number,
    ..Style::PLAIN
};
const HEADER: Style = Style {
    fg: Fg::Header,
    bold: true,
    ..Style::PLAIN
};
/// Table rules: dim when Codex cannot read the terminal's colours.
const RULE: Style = Style::DIM;

#[derive(Clone)]
struct Indent {
    prefix: Line,
    marker: Option<Line>,
    is_list: bool,
}

struct LinkState {
    dest: String,
    show_dest: bool,
    style_label: bool,
    visible_label: bool,
    /// A link to a file: its path as Codex shows it, and the label collected.
    local: Option<(String, Line)>,
}

#[derive(Clone, Default)]
struct Cell {
    lines: Vec<Line>,
}

impl Cell {
    fn push(&mut self, glyphs: Line) {
        if self.lines.is_empty() {
            self.lines.push(Line::new());
        }
        self.lines.last_mut().unwrap().extend(glyphs);
    }

    fn plain(&self) -> String {
        let mut s = String::new();
        for (i, l) in self.lines.iter().enumerate() {
            if i > 0 {
                s.push(' ');
            }
            s.extend(l.iter().map(|g| g.c));
        }
        s
    }

    fn width(&self) -> usize {
        self.lines.iter().map(|l| width_of(l)).max().unwrap_or(0)
    }
}

struct Row {
    cells: Vec<Cell>,
    pipes: bool,
}

struct Table {
    align: Vec<Alignment>,
    header: Option<Vec<Cell>>,
    rows: Vec<Row>,
    row: Option<Vec<Cell>>,
    row_pipes: bool,
    cell: Option<Cell>,
    in_header: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum ColKind {
    Narrative,
    TokenHeavy,
    Compact,
}

struct Metrics {
    max_width: usize,
    header_token: usize,
    body_token: usize,
    kind: ColKind,
}

struct Writer<'a> {
    input: &'a str,
    width: usize,
    show_urls: bool,
    text: Vec<Line>,
    /// The style of each line of `text` (patched under its own).
    styles: Vec<Style>,
    inline: Vec<Style>,
    indents: Vec<Indent>,
    lists: Vec<Option<u64>>,
    /// Each open list: it has an item, one of its items takes more than a
    /// row, and the blank rows put between its items (taken out again at its
    /// end unless an item took more than a row).
    uniform: Vec<(bool, bool, Vec<usize>)>,
    item_starts: Vec<usize>,
    link: Option<LinkState>,
    needs_newline: bool,
    pending_marker: bool,
    in_code: bool,
    code_lang: Option<String>,
    code_buffer: Line,
    current: Option<Line>,
    first_indent: Line,
    next_indent: Line,
    line_style: Style,
    current_in_code: bool,
    table: Option<Table>,
}

impl<'a> Writer<'a> {
    fn new(input: &'a str, width: usize, show_urls: bool) -> Self {
        Writer {
            input,
            width,
            show_urls,
            text: Vec::new(),
            styles: Vec::new(),
            inline: Vec::new(),
            indents: Vec::new(),
            lists: Vec::new(),
            uniform: Vec::new(),
            item_starts: Vec::new(),
            link: None,
            needs_newline: false,
            pending_marker: false,
            in_code: false,
            code_lang: None,
            code_buffer: Line::new(),
            current: None,
            first_indent: Line::new(),
            next_indent: Line::new(),
            line_style: Style::PLAIN,
            current_in_code: false,
            table: None,
        }
    }

    fn style(&self) -> Style {
        self.inline.last().copied().unwrap_or_default()
    }

    fn event(&mut self, ev: Ev, range: Range<usize>) {
        match ev {
            Ev::Start(tag) => self.start(tag, range),
            Ev::End(tag) => self.end(tag),
            Ev::Text(t) => self.text(t),
            Ev::Code(t) => self.code(t),
            Ev::Soft => {
                if self.in_cell() {
                    let style = self.style();
                    self.push_to_cell(glyphs(style, " ", None));
                } else if let Some((_, label)) = self.link.as_mut().and_then(|l| l.local.as_mut()) {
                    if label.last().is_some_and(|g| !g.c.is_whitespace()) {
                        label.push(Glyph {
                            c: ' ',
                            style: Style::PLAIN,
                            src: None,
                        });
                    }
                } else {
                    self.push_line(Line::new());
                }
            }
            Ev::Hard => {
                if self.in_cell() {
                    if let Some(cell) = self.table.as_mut().and_then(|t| t.cell.as_mut()) {
                        cell.lines.push(Line::new());
                    }
                } else {
                    self.push_line(Line::new());
                }
            }
            Ev::Rule => {
                self.flush();
                if !self.text.is_empty() {
                    self.push_blank();
                }
                self.push_line(glyphs(Style::PLAIN, "———", None));
                self.needs_newline = true;
            }
            Ev::Html(t, inline) => self.html(t, inline),
            Ev::Task(done) => {
                self.task_marker(done);
                if self.current.is_none() {
                    self.push_line(Line::new());
                }
            }
        }
    }

    fn start(&mut self, tag: Tag<'static>, range: Range<usize>) {
        match tag {
            Tag::Paragraph => {
                if self.in_cell() {
                    return;
                }
                if self.needs_newline {
                    self.push_blank();
                }
                self.push_line(Line::new());
                self.needs_newline = false;
            }
            Tag::Heading { level, .. } => {
                if self.in_cell() {
                    return;
                }
                if self.needs_newline {
                    self.push_line(Line::new());
                    self.needs_newline = false;
                }
                let style = match level as u8 {
                    1 => H1,
                    2 => H2,
                    3 => H3,
                    _ => H4,
                };
                let hashes = format!("{} ", "#".repeat(level as usize));
                self.push_line(glyphs(style, &hashes, None));
                self.push_inline(style);
                self.needs_newline = false;
            }
            Tag::BlockQuote(_) => {
                if self.in_cell() {
                    return;
                }
                if self.needs_newline {
                    self.push_blank();
                    self.needs_newline = false;
                }
                self.indents.push(Indent {
                    prefix: glyphs(Style::PLAIN, "> ", None),
                    marker: None,
                    is_list: false,
                });
            }
            Tag::CodeBlock(kind) => {
                let (indent, lang) = match kind {
                    CodeBlockKind::Fenced(info) => (
                        Line::new(),
                        info.split([',', ' ', '\t'])
                            .next()
                            .filter(|s| !s.is_empty())
                            .map(str::to_string),
                    ),
                    CodeBlockKind::Indented => (spaces(4), None),
                };
                self.flush();
                let first_in_item =
                    self.pending_marker && self.indents.last().is_some_and(|c| c.is_list);
                if !self.text.is_empty() && !first_in_item {
                    self.push_blank();
                }
                self.in_code = true;
                self.code_lang = lang;
                self.code_buffer.clear();
                self.indents.push(Indent {
                    prefix: indent,
                    marker: None,
                    is_list: false,
                });
                self.needs_newline = true;
            }
            Tag::List(start) => {
                if self.pending_marker
                    && self.input[..range.start]
                        .rsplit('\n')
                        .next()
                        .is_some_and(|l| l.trim_matches(['>', ' ', '\t']).is_empty())
                {
                    self.push_line(Line::new());
                }
                if self.lists.is_empty() && self.needs_newline {
                    self.push_line(Line::new());
                }
                self.lists.push(start);
                self.uniform.push((false, false, Vec::new()));
            }
            Tag::Item => self.start_item(),
            Tag::Emphasis => self.push_inline(H4),
            Tag::Strong => self.push_inline(Style::BOLD),
            Tag::Strikethrough => self.push_inline(Style {
                strike: true,
                ..Style::PLAIN
            }),
            Tag::Link { dest_url, .. } => self.push_link(dest_url.to_string()),
            Tag::Table(align) => {
                self.flush();
                if self.needs_newline {
                    self.push_blank();
                    self.needs_newline = false;
                }
                self.table = Some(Table {
                    align,
                    header: None,
                    rows: Vec::new(),
                    row: None,
                    row_pipes: false,
                    cell: None,
                    in_header: false,
                });
            }
            Tag::TableHead => {
                if let Some(t) = self.table.as_mut() {
                    t.in_header = true;
                    t.row = Some(Vec::new());
                }
            }
            Tag::TableRow => {
                let src = self.input.get(range).unwrap_or("").trim();
                let pipes = src.starts_with('|') || src.ends_with('|');
                if let Some(t) = self.table.as_mut() {
                    t.row = Some(Vec::new());
                    t.row_pipes = pipes;
                }
            }
            Tag::TableCell => {
                if let Some(t) = self.table.as_mut() {
                    t.cell = Some(Cell::default());
                }
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph => {
                if !self.in_cell() {
                    self.needs_newline = true;
                    self.pending_marker = false;
                }
            }
            TagEnd::Heading(_) => {
                if !self.in_cell() {
                    self.needs_newline = true;
                    self.inline.pop();
                }
            }
            TagEnd::BlockQuote(_) => {
                self.flush();
                if !self.in_cell() {
                    self.indents.pop();
                    self.needs_newline = true;
                }
            }
            TagEnd::CodeBlock => {
                if self.code_lang.take().is_some() {
                    let code = std::mem::take(&mut self.code_buffer);
                    for line in split(&code) {
                        self.push_line(Line::new());
                        self.push_glyphs(line);
                    }
                    // A trailing newline makes no line of its own.
                    if code.last().is_some_and(|g| g.c == '\n')
                        && self.current.as_ref().is_some_and(|l| l.is_empty())
                    {
                        self.current = None;
                    }
                }
                self.needs_newline = true;
                self.in_code = false;
                self.indents.pop();
            }
            TagEnd::List(_) => {
                self.flush();
                if let Some((_, multiline, separators)) = self.uniform.pop()
                    && !multiline
                    && let Some(&start) = separators.first()
                {
                    let tail = self.text.split_off(start);
                    let tail_styles = self.styles.split_off(start);
                    for (k, (l, st)) in tail.into_iter().zip(tail_styles).enumerate() {
                        if !separators.contains(&(start + k)) {
                            self.text.push(l);
                            self.styles.push(st);
                        }
                    }
                }
                self.lists.pop();
                self.needs_newline = true;
            }
            TagEnd::Item => {
                if self.pending_marker {
                    self.push_line(Line::new());
                }
                self.flush();
                let start = self.item_starts.pop().unwrap_or_default();
                let rows = &self.text[start.min(self.text.len())..];
                let multi = rows.len() > 1 || (rows.len() == 1 && width_of(&rows[0]) > self.width);
                if let Some(u) = self.uniform.last_mut() {
                    u.1 |= multi;
                }
                self.indents.pop();
                self.pending_marker = false;
            }
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {
                self.inline.pop();
            }
            TagEnd::Link => self.pop_link(),
            TagEnd::Table => self.end_table(),
            TagEnd::TableHead => {
                if let Some(t) = self.table.as_mut() {
                    if let Some(c) = t.cell.take() {
                        t.row.get_or_insert_with(Vec::new).push(c);
                    }
                    if let Some(r) = t.row.take() {
                        t.header = Some(r);
                    }
                    t.in_header = false;
                }
            }
            TagEnd::TableRow => {
                if let Some(t) = self.table.as_mut() {
                    if let Some(c) = t.cell.take() {
                        t.row.get_or_insert_with(Vec::new).push(c);
                    }
                    if let Some(cells) = t.row.take() {
                        if t.in_header {
                            t.header = Some(cells);
                        } else {
                            t.rows.push(Row {
                                cells,
                                pipes: t.row_pipes,
                            });
                        }
                    }
                    t.row_pipes = false;
                }
            }
            TagEnd::TableCell => {
                if let Some(t) = self.table.as_mut()
                    && let Some(c) = t.cell.take()
                {
                    t.row.get_or_insert_with(Vec::new).push(c);
                }
            }
            _ => {}
        }
    }

    fn push_inline(&mut self, style: Style) {
        let merged = patch(self.style(), style);
        self.inline.push(merged);
    }

    fn in_cell(&self) -> bool {
        self.table.as_ref().is_some_and(|t| t.cell.is_some())
    }

    fn collecting_local_label(&self) -> bool {
        self.link.as_ref().is_some_and(|l| l.local.is_some())
    }

    /// A web link's text takes the link style.
    fn link_label(&mut self, mut g: Line) -> Line {
        if let Some(link) = self.link.as_mut()
            && web(&link.dest)
        {
            link.visible_label |= g.iter().any(|g| !g.c.is_whitespace());
            for x in &mut g {
                x.style = patch(x.style, LINK);
            }
        }
        g
    }

    fn push_to_cell(&mut self, g: Line) {
        let g = self.link_label(g);
        if let Some(cell) = self.table.as_mut().and_then(|t| t.cell.as_mut()) {
            cell.push(g);
        }
    }

    fn text(&mut self, t: Line) {
        let style = self.style();
        if let Some((_, label)) = self.link.as_mut().and_then(|l| l.local.as_mut()) {
            for (i, line) in split(&t).into_iter().enumerate() {
                if i > 0 && label.last().is_some_and(|g| !g.c.is_whitespace()) {
                    label.push(Glyph {
                        c: ' ',
                        style: Style::PLAIN,
                        src: None,
                    });
                }
                label.extend(styled(line, style));
            }
            return;
        }
        if self.in_cell() {
            for (i, line) in lines_of(&t).into_iter().enumerate() {
                if i > 0
                    && let Some(cell) = self.table.as_mut().and_then(|t| t.cell.as_mut())
                {
                    cell.lines.push(Line::new());
                }
                let line = styled(line, style);
                let line = if self.link.is_none() && !self.in_code {
                    bare_urls(line)
                } else {
                    line
                };
                self.push_to_cell(line);
            }
            return;
        }
        if self.pending_marker {
            self.push_line(Line::new());
        }
        self.pending_marker = false;
        if self.in_code && self.code_lang.is_some() {
            self.code_buffer.extend(t);
            return;
        }
        if self.in_code && !self.needs_newline {
            let has_content = match &self.current {
                Some(l) => !l.is_empty(),
                None => self.text.last().is_some_and(|l| !l.is_empty()),
            };
            if has_content {
                self.push_line(Line::new());
            }
        }
        for (i, line) in lines_of(&t).into_iter().enumerate() {
            if self.needs_newline {
                self.push_line(Line::new());
                self.needs_newline = false;
            }
            if i > 0 {
                self.push_line(Line::new());
            }
            let line = styled(line, style);
            let line = if self.link.is_none() && !self.in_code {
                bare_urls(line)
            } else {
                line
            };
            self.push_glyphs(line);
        }
        self.needs_newline = false;
    }

    fn code(&mut self, t: Line) {
        let code: Line = t.into_iter().map(|g| Glyph { style: CODE, ..g }).collect();
        if let Some((_, label)) = self.link.as_mut().and_then(|l| l.local.as_mut()) {
            label.extend(code);
            return;
        }
        if self.in_cell() {
            self.push_to_cell(code);
            return;
        }
        if self.pending_marker {
            self.push_line(Line::new());
            self.pending_marker = false;
        }
        self.push_glyphs(code);
    }

    fn html(&mut self, t: Line, inline: bool) {
        let style = self.style();
        if self.collecting_local_label() {
            return;
        }
        if self.in_cell() {
            for (i, line) in lines_of(&t).into_iter().enumerate() {
                if i > 0
                    && let Some(cell) = self.table.as_mut().and_then(|t| t.cell.as_mut())
                {
                    cell.lines.push(Line::new());
                }
                self.push_to_cell(styled(line, style));
            }
            if !inline && let Some(cell) = self.table.as_mut().and_then(|t| t.cell.as_mut()) {
                cell.lines.push(Line::new());
            }
            return;
        }
        self.pending_marker = false;
        for (i, line) in lines_of(&t).into_iter().enumerate() {
            if self.needs_newline {
                self.push_line(Line::new());
                self.needs_newline = false;
            }
            if i > 0 {
                self.push_line(Line::new());
            }
            self.push_glyphs(styled(line, style));
        }
        self.needs_newline = !inline;
    }

    fn start_item(&mut self) {
        self.flush();
        if self.uniform.last().is_some_and(|u| u.0) {
            let at = self.text.len();
            self.push_blank();
            if self.text.len() > at
                && let Some(u) = self.uniform.last_mut()
            {
                u.2.push(at);
            }
        }
        if let Some(u) = self.uniform.last_mut() {
            u.0 = true;
        }
        self.item_starts.push(self.text.len());
        self.pending_marker = true;
        let depth = self.lists.len();
        let ordered = self.lists.last().is_some_and(Option::is_some);
        let width = (depth * 4).saturating_sub(3).max(1);
        let marker = match self.lists.last_mut() {
            Some(None) => Some(glyphs(
                Style::PLAIN,
                &format!("{}• ", " ".repeat(width - 1)),
                None,
            )),
            Some(Some(index)) => {
                *index += 1;
                Some(glyphs(NUMBER, &format!("{:width$}. ", *index - 1), None))
            }
            None => None,
        };
        let prefix = if depth == 0 {
            Line::new()
        } else {
            spaces(if ordered { width + 2 } else { width + 1 })
        };
        self.indents.push(Indent {
            prefix,
            marker,
            is_list: true,
        });
        self.needs_newline = false;
    }

    fn task_marker(&mut self, done: bool) {
        let Some(ctx) = self.indents.iter_mut().rev().find(|c| c.is_list) else {
            return;
        };
        if let Some(marker) = ctx.marker.as_mut() {
            let text: String = marker.iter().map(|g| g.c).collect();
            let style = marker.first().map_or(Style::PLAIN, |g| g.style);
            let base = text.strip_suffix("• ").unwrap_or(&text);
            let box_ = if done { "☑" } else { "☐" };
            *marker = glyphs(style, &format!("{base}{box_} "), None);
            ctx.prefix = spaces(width_of(marker));
        }
        if self.current.is_some() {
            self.first_indent = self.prefix(true);
            self.next_indent = self.prefix(false);
        }
    }

    fn push_link(&mut self, dest: String) {
        let style_label = !self.show_urls && web(&dest);
        if style_label {
            self.push_inline(LINK);
        }
        let local = local_path(&dest);
        self.link = Some(LinkState {
            show_dest: !style_label && local.is_none(),
            style_label,
            visible_label: false,
            local: local.map(|p| (p, Line::new())),
            dest,
        });
    }

    fn pop_link(&mut self) {
        let Some(link) = self.link.take() else {
            return;
        };
        if link.style_label {
            self.inline.pop();
        }
        let dest = || glyphs(LINK, &link.dest, None);
        let plain = |s: &str| glyphs(Style::PLAIN, s, None);
        if link.show_dest || (!link.visible_label && web(&link.dest)) {
            if self.in_cell() {
                self.push_to_cell(plain(" ("));
                if let Some(cell) = self.table.as_mut().and_then(|t| t.cell.as_mut()) {
                    cell.push(dest());
                }
                self.push_to_cell(plain(")"));
            } else {
                self.push_glyphs(plain(" ("));
                // Not through `link_label`: the link is over.
                self.push_raw(dest());
                self.push_glyphs(plain(")"));
            }
        } else if let Some((path, label)) = link.local {
            let show_label = label.iter().any(|g| !g.c.is_whitespace());
            let target = glyphs(patch(self.style(), CODE), &path, None);
            let mut parts = Vec::new();
            if show_label {
                parts.push(label);
                parts.push(plain(" ("));
            }
            parts.push(target);
            if show_label {
                parts.push(plain(")"));
            }
            if self.in_cell() {
                for p in parts {
                    self.push_to_cell(p);
                }
            } else {
                if self.pending_marker {
                    self.push_line(Line::new());
                }
                for p in parts {
                    self.push_glyphs(p);
                }
            }
        }
    }

    // Lines.

    fn quote_active(&self) -> bool {
        self.indents
            .iter()
            .any(|c| c.prefix.iter().any(|g| g.c == '>'))
    }

    fn prefix(&self, pending: bool) -> Line {
        let last_marker = if pending {
            self.indents.iter().rposition(|c| c.marker.is_some())
        } else {
            None
        };
        let last_list = self.indents.iter().rposition(|c| c.is_list);
        let mut out = Line::new();
        for (i, ctx) in self.indents.iter().enumerate() {
            if pending {
                if Some(i) == last_marker
                    && let Some(m) = &ctx.marker
                {
                    out.extend(m.iter().copied());
                    continue;
                }
                if ctx.is_list && last_marker.is_some_and(|m| m > i) {
                    continue;
                }
            } else if ctx.is_list && Some(i) != last_list {
                continue;
            }
            out.extend(ctx.prefix.iter().copied());
        }
        out
    }

    fn push_line(&mut self, line: Line) {
        self.flush();
        let style = if self.quote_active() {
            QUOTE
        } else {
            Style::PLAIN
        };
        let was_pending = self.pending_marker;
        self.first_indent = self.prefix(was_pending);
        self.next_indent = self.prefix(false);
        self.line_style = style;
        self.current = Some(line);
        self.current_in_code = self.in_code;
        self.pending_marker = false;
    }

    /// Glyphs onto the current line, a web link's text in the link style.
    fn push_glyphs(&mut self, g: Line) {
        let g = self.link_label(g);
        self.push_raw(g);
    }

    fn push_raw(&mut self, g: Line) {
        if self.current.is_none() {
            self.push_line(Line::new());
        }
        if let Some(line) = self.current.as_mut() {
            line.extend(g);
        }
    }

    fn push_blank(&mut self) {
        self.flush();
        if self.indents.iter().all(|c| c.is_list) {
            self.text.push(Line::new());
            self.styles.push(Style::PLAIN);
        } else {
            self.push_line(Line::new());
            self.flush();
        }
    }

    fn flush(&mut self) {
        let Some(line) = self.current.take() else {
            return;
        };
        let style = self.line_style;
        let first = std::mem::take(&mut self.first_indent);
        let next = std::mem::take(&mut self.next_indent);
        if self.current_in_code {
            let mut row = first;
            row.extend(line);
            self.text.push(row);
            self.styles.push(style);
        } else {
            for row in wrap(&line, self.width, &first, &next) {
                self.text.push(row);
                self.styles.push(style);
            }
        }
        self.current_in_code = false;
    }

    /// A finished line laid out already (a table row): the prefix, no wrap.
    fn push_prewrapped(&mut self, (line, style): (Line, Style), pending: bool) {
        self.flush();
        let mut row = self.prefix(pending);
        row.extend(line);
        self.text.push(row);
        self.styles.push(if self.quote_active() {
            patch(QUOTE, style)
        } else {
            style
        });
    }

    // Tables.

    fn end_table(&mut self) {
        let Some(t) = self.table.take() else {
            return;
        };
        let pending = self.pending_marker;
        let cols = t.align.len();
        let mut lines: Vec<(Line, Style)> = Vec::new();
        let mut spill: Vec<Line> = Vec::new();
        let mut prewrapped = true;
        if cols > 0 {
            let mut rows: Vec<Vec<Cell>> = Vec::new();
            for (k, row) in t.rows.iter().enumerate() {
                if cols > 1 && spillover(row, t.rows.get(k + 1)) {
                    if let Some(c) = row.cells.first() {
                        spill.extend(c.lines.iter().cloned());
                    }
                } else {
                    rows.push(row.cells.clone());
                }
            }
            let mut header = t.header.unwrap_or_else(|| vec![Cell::default(); cols]);
            header.resize(cols, Cell::default());
            for r in &mut rows {
                r.truncate(cols);
                r.resize(cols, Cell::default());
            }
            for c in header.iter_mut().chain(rows.iter_mut().flatten()) {
                if c.lines.is_empty() {
                    c.lines.push(Line::new());
                }
            }
            let metrics = metrics(&header, &rows);
            let prefix_w = width_of(&self.prefix(pending));
            let room = self
                .width
                .saturating_sub(prefix_w + (cols - 1) * 2 + cols * 2);
            let record_room = self.width.saturating_sub(prefix_w);
            match widths(&metrics, room) {
                None if !rows.is_empty() => {
                    lines = plain_rows(records(&header, &rows, &metrics, record_room));
                }
                None => {
                    lines = plain_rows(pipe_rows(&header, &rows, &t.align));
                    prewrapped = false;
                }
                Some(w) if should_records(&rows, &w, &metrics) => {
                    lines = plain_rows(records(&header, &rows, &metrics, record_room));
                }
                Some(w) => {
                    let row_of = |r: &[Cell], style| {
                        table_row(r, &w, &t.align)
                            .into_iter()
                            .map(move |l| (l, style))
                    };
                    lines.extend(row_of(&header, HEADER));
                    lines.push((rule(&w, '━'), Style::PLAIN));
                    for (k, r) in rows.iter().enumerate() {
                        lines.extend(row_of(r, Style::PLAIN));
                        if k + 1 < rows.len() {
                            lines.push((rule(&w, '─'), Style::PLAIN));
                        }
                    }
                }
            }
        }
        let mut pending = pending;
        for line in lines {
            if prewrapped {
                self.push_prewrapped(line, pending);
            } else {
                self.push_line(line.0);
                self.flush();
            }
            pending = false;
        }
        self.pending_marker = false;
        for line in spill {
            self.push_line(line);
            self.flush();
        }
        self.needs_newline = true;
    }
}

/// Lines of a text event (`str::lines`: no line after a last line break).
fn lines_of(t: &[Glyph]) -> Vec<Line> {
    let mut out = split(t);
    if out.len() > 1 && out.last().is_some_and(Vec::is_empty) {
        out.pop();
    }
    out
}

/// An http(s) address with a host.
fn web(dest: &str) -> bool {
    let rest = dest
        .strip_prefix("https://")
        .or_else(|| dest.strip_prefix("http://"));
    rest.is_some_and(|r| {
        let host = r.split(['/', '?', '#']).next().unwrap_or("");
        !host.is_empty() && !dest.chars().any(char::is_control)
    })
}

/// A link to a file: how Codex shows its path (under the current folder
/// relative to it).
fn local_path(dest: &str) -> Option<String> {
    let local = dest.starts_with("file://")
        || dest.starts_with('/')
        || dest.starts_with("~/")
        || dest.starts_with("./")
        || dest.starts_with("../");
    if !local {
        return None;
    }
    let path = dest.strip_prefix("file://").unwrap_or(dest);
    let cwd = std::env::current_dir().ok()?;
    let cwd = cwd.to_string_lossy();
    let shown = path
        .strip_prefix(cwd.as_ref())
        .and_then(|r| r.strip_prefix('/'))
        .filter(|r| !r.is_empty())
        .unwrap_or(path);
    Some(shown.to_string())
}

/// Web addresses in plain text take the link style.
fn bare_urls(mut line: Line) -> Line {
    let mut k = 0;
    while k < line.len() {
        if line[k].c.is_whitespace() {
            k += 1;
            continue;
        }
        let end = (k..line.len())
            .find(|&j| line[j].c.is_whitespace())
            .unwrap_or(line.len());
        let token: String = line[k..end].iter().map(|g| g.c).collect();
        let trimmed = token.trim_end_matches(['.', ',', ';', ':', ')', '!', '?', '\'', '"']);
        if web(trimmed) {
            for g in &mut line[k..k + trimmed.chars().count()] {
                g.style = patch(g.style, LINK);
            }
        }
        k = end;
    }
    line
}

// Wrapping, as Codex's `wrapping.rs` does with `textwrap`.

/// `line` wrapped to `width`, its first row after `first`, the others after
/// `next`; a line with an address in it keeps the address whole.
fn wrap(line: &[Glyph], width: usize, first: &[Glyph], next: &[Glyph]) -> Vec<Line> {
    let flat: String = line.iter().map(|g| g.c).collect();
    // The glyph each byte of `flat` is in.
    let mut owner = Vec::with_capacity(flat.len());
    for (i, g) in line.iter().enumerate() {
        for _ in 0..g.c.len_utf8() {
            owner.push(i);
        }
    }
    let slice = |r: &Range<usize>| -> Line {
        if r.start >= r.end {
            return Line::new();
        }
        let a = owner[r.start];
        let b = owner[r.end - 1];
        line[a..=b].to_vec()
    };
    let w1 = width.saturating_sub(width_of(first)).max(1);
    let w2 = width.saturating_sub(width_of(next)).max(1);
    let (saw_url, saw_other) = url_tokens(&flat);
    let ranges = if saw_url && saw_other {
        mixed_ranges(&flat, w1, w2)
    } else {
        let (sep, splitter, breaks) = if saw_url {
            (
                textwrap::WordSeparator::AsciiSpace,
                textwrap::WordSplitter::NoHyphenation,
                false,
            )
        } else {
            (
                textwrap::WordSeparator::new(),
                textwrap::WordSplitter::HyphenSplitter,
                true,
            )
        };
        let opts = |w: usize| {
            textwrap::Options::new(w)
                .break_words(breaks)
                .word_separator(sep)
                .word_splitter(splitter.clone())
                .wrap_algorithm(textwrap::WrapAlgorithm::FirstFit)
        };
        let firsts = ranges_trim(&flat, &opts(w1));
        let Some(r0) = firsts.first().cloned() else {
            return vec![first.to_vec()];
        };
        let base = r0.end + flat[r0.end..].chars().take_while(|c| *c == ' ').count();
        let mut out = vec![r0];
        if w1 == w2
            && firsts
                .get(1)
                .map_or(base == flat.len(), |r| r.start == base)
        {
            out.extend(firsts.into_iter().skip(1));
        } else {
            out.extend(
                ranges_trim(&flat[base..], &opts(w2))
                    .into_iter()
                    .map(|r| r.start + base..r.end + base),
            );
        }
        out
    };
    let mut rows = Vec::new();
    for (k, r) in ranges.iter().enumerate() {
        if k > 0 && r.is_empty() {
            continue;
        }
        let mut row = if k == 0 {
            first.to_vec()
        } else {
            next.to_vec()
        };
        row.extend(slice(r));
        rows.push(row);
    }
    if rows.is_empty() {
        rows.push(first.to_vec());
    }
    rows
}

/// `textwrap::wrap`'s lines as byte ranges of `text`, without the white
/// space at their ends.
fn ranges_trim(text: &str, opts: &textwrap::Options) -> Vec<Range<usize>> {
    let base = text.as_ptr() as usize;
    let mut cursor = 0;
    let mut out = Vec::new();
    for line in textwrap::wrap(text, opts) {
        let range = match &line {
            std::borrow::Cow::Borrowed(s)
                if (s.as_ptr() as usize) >= base
                    && (s.as_ptr() as usize) + s.len() <= base + text.len() =>
            {
                let a = s.as_ptr() as usize - base;
                a..a + s.len()
            }
            other => {
                // A line textwrap made up (a hyphen added): match it to the text.
                let s: &str = other;
                let mut start = cursor;
                while text[start..].starts_with(' ') && !s.starts_with(' ') {
                    start += 1;
                }
                let mut end = start;
                for c in s.chars() {
                    if text[end..].starts_with(c) {
                        end += c.len_utf8();
                    }
                }
                start..end
            }
        };
        cursor = range.end;
        out.push(range);
    }
    out
}

/// Whether `text` has a URL-like token, and one of other words.
fn url_tokens(text: &str) -> (bool, bool) {
    let (mut url, mut other) = (false, false);
    for raw in text.split_ascii_whitespace() {
        let token = trim_token(raw);
        if url_like(token) {
            url = true;
        } else if !token.is_empty()
            && !decorative(raw, token)
            && token.chars().any(char::is_alphanumeric)
        {
            other = true;
        }
    }
    (url, other)
}

fn trim_token(t: &str) -> &str {
    t.trim_matches([
        '(', ')', '[', ']', '{', '}', '<', '>', ',', '.', ';', ':', '!', '\'', '"',
    ])
}

fn decorative(raw: &str, token: &str) -> bool {
    matches!(
        raw.trim(),
        "-" | "*"
            | "+"
            | "•"
            | "◦"
            | "▪"
            | ">"
            | "|"
            | "│"
            | "┆"
            | "└"
            | "├"
            | "┌"
            | "┐"
            | "┘"
            | "┼"
    ) || (token.chars().all(|c| c.is_ascii_digit())
        && (raw.trim().ends_with('.') || raw.trim().ends_with(')')))
}

fn url_like(token: &str) -> bool {
    if token.is_empty() {
        return false;
    }
    if let Some((scheme, rest)) = token.split_once("://") {
        let mut chars = scheme.chars();
        let scheme_ok = chars.next().is_some_and(|c| c.is_ascii_alphabetic())
            && chars.all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c));
        if !scheme_ok || rest.is_empty() {
            return false;
        }
        let lower = scheme.to_ascii_lowercase();
        if ["http", "https", "ftp", "ftps", "ws", "wss"].contains(&lower.as_str()) {
            let host = rest.split(['/', '?', '#']).next().unwrap_or("");
            return !host.is_empty();
        }
        return true;
    }
    // A bare host with a path, or www.
    let (host_port, trailer) = match token.find(['/', '?', '#']) {
        Some(i) => (&token[..i], true),
        None => (token, false),
    };
    if host_port.is_empty() || (!trailer && !host_port.to_ascii_lowercase().starts_with("www.")) {
        return false;
    }
    let host = match host_port.rsplit_once(':') {
        Some((h, p)) if !h.is_empty() && p.parse::<u16>().is_ok() => h,
        Some((_, p)) if p.chars().all(|c| c.is_ascii_digit()) && !p.is_empty() => return false,
        _ => host_port,
    };
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    let parts: Vec<&str> = host.split('.').collect();
    if parts.len() == 4 && parts.iter().all(|p| p.parse::<u8>().is_ok()) {
        return true;
    }
    let lower = host.to_ascii_lowercase();
    let mut labels: Vec<&str> = lower.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    let tld = labels.pop().unwrap();
    (2..=63).contains(&tld.len())
        && tld.chars().all(|c| c.is_ascii_alphabetic())
        && labels.iter().all(|l| {
            !l.is_empty()
                && l.len() <= 63
                && l.starts_with(|c: char| c.is_ascii_alphanumeric())
                && l.ends_with(|c: char| c.is_ascii_alphanumeric())
                && l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        })
}

/// `mixed_url_wrap_ranges`: prose by words, an address never split, a
/// word longer than a row split by chars.
fn mixed_ranges(text: &str, first_w: usize, next_w: usize) -> Vec<Range<usize>> {
    struct Word {
        range: Range<usize>,
        url: bool,
    }
    let wid = |r: &Range<usize>| {
        text[r.clone()]
            .chars()
            .map(|c| c.width().unwrap_or(0))
            .sum::<usize>()
    };
    let lead = text.chars().take_while(|c| *c == ' ').count();
    let mut words = Vec::new();
    let mut i = 0;
    let bytes = text.as_bytes();
    while i < text.len() {
        if bytes[i] == b' ' {
            i += 1;
            continue;
        }
        let start = i;
        while i < text.len() && bytes[i] != b' ' {
            i += 1;
        }
        words.push(Word {
            url: url_like(trim_token(&text[start..i])),
            range: start..i,
        });
    }
    let split_word = |w: &Word, limit: usize| -> Vec<Word> {
        if w.url || wid(&w.range) <= limit {
            return vec![Word {
                range: w.range.clone(),
                url: w.url,
            }];
        }
        let mut pieces = Vec::new();
        let (mut start, mut width) = (w.range.start, 0);
        for (off, c) in text[w.range.clone()].char_indices() {
            let cw = c.width().unwrap_or(0);
            if width > 0 && width + cw > limit.max(1) {
                let end = w.range.start + off;
                pieces.push(Word {
                    range: start..end,
                    url: false,
                });
                start = end;
                width = 0;
            }
            width += cw;
        }
        if start < w.range.end {
            pieces.push(Word {
                range: start..w.range.end,
                url: false,
            });
        }
        pieces
    };
    let mut lines = Vec::new();
    let mut line_start: Option<usize> = None;
    let (mut line_end, mut line_width) = (0, 0);
    let mut limit = first_w.max(1);
    for word in words {
        let mut pending = split_word(&word, limit);
        let mut idx = 0;
        while idx < pending.len() {
            let empty_prefix = if line_start.is_none() && lines.is_empty() {
                lead
            } else {
                0
            };
            let piece_limit = limit.saturating_sub(empty_prefix).max(1);
            let mut indivisible = false;
            if line_start.is_none() && !pending[idx].url && wid(&pending[idx].range) > piece_limit {
                let split = split_word(&pending[idx], piece_limit);
                if split.len() > 1 {
                    pending.splice(idx..=idx, split);
                    continue;
                }
                indivisible = true;
            }
            let piece = &pending[idx];
            let pw = wid(&piece.range);
            let gap = line_start.map_or(0, |_| piece.range.start - line_end);
            let fits = if line_start.is_none() {
                piece.url || indivisible || empty_prefix + pw <= limit || empty_prefix >= limit
            } else {
                line_width + gap + pw <= limit
            };
            if fits {
                if line_start.is_none() {
                    let first_line = lines.is_empty();
                    line_start = Some(if first_line { 0 } else { piece.range.start });
                    line_width = if first_line { lead + pw } else { pw };
                } else {
                    line_width += gap + pw;
                }
                line_end = piece.range.end;
                idx += 1;
                continue;
            }
            if let Some(s) = line_start.take() {
                lines.push(s..line_end);
            }
            line_end = 0;
            line_width = 0;
            limit = next_w.max(1);
        }
    }
    if let Some(s) = line_start {
        lines.push(s..line_end);
    }
    lines
}

// Tables, as Codex's `markdown_render.rs` and `table_key_value.rs` do them.

fn spillover(row: &Row, next: Option<&Row>) -> bool {
    let only_first = |cells: &[Cell]| -> Option<String> {
        let first = cells.first()?.plain();
        if first.trim().is_empty() || cells[1..].iter().any(|c| !c.plain().trim().is_empty()) {
            return None;
        }
        Some(first)
    };
    let Some(first) = only_first(&row.cells) else {
        return false;
    };
    if row.cells.len() == 1 && !row.pipes {
        return true;
    }
    let html = |t: &str| {
        let b = t.as_bytes();
        (0..b.len()).any(|i| {
            if b[i] != b'<' {
                return false;
            }
            let mut s = i + 1;
            if s < b.len() && (b[s] == b'/' || b[s] == b'!') {
                s += 1;
            }
            b.get(s).is_some_and(u8::is_ascii_alphabetic) && b[s + 1..].contains(&b'>')
        })
    };
    if html(&first) {
        return true;
    }
    if first.trim_end().ends_with(':') {
        if next
            .and_then(|r| only_first(&r.cells))
            .is_some_and(|t| html(&t))
        {
            return true;
        }
        let label = first.trim().trim_end_matches(':').trim();
        if next.is_none()
            && label
                .split_whitespace()
                .any(|w| w.eq_ignore_ascii_case("html"))
        {
            return true;
        }
    }
    false
}

fn tokens_width(t: &str) -> usize {
    t.split_whitespace().map(str_width).max().unwrap_or(0)
}

fn str_width(t: &str) -> usize {
    t.chars().map(|c| c.width().unwrap_or(0)).sum()
}

fn metrics(header: &[Cell], rows: &[Vec<Cell>]) -> Vec<Metrics> {
    (0..header.len())
        .map(|c| {
            let head_plain = header[c].plain();
            let mut max_width = header[c].width();
            let (mut body_token, mut tokens, mut long, mut words, mut cells, mut total) =
                (0usize, 0usize, 0usize, 0usize, 0usize, 0usize);
            for row in rows {
                let cell = &row[c];
                max_width = max_width.max(cell.width());
                let plain = cell.plain();
                let mut count = 0;
                for t in plain.split_whitespace() {
                    let w = str_width(t);
                    body_token = body_token.max(w);
                    long += usize::from(w >= 20);
                    count += 1;
                }
                if count > 0 {
                    tokens += count;
                    words += count;
                    cells += 1;
                    total += str_width(&plain);
                }
            }
            let avg_words = if cells == 0 {
                head_plain.split_whitespace().count() as f64
            } else {
                words as f64 / cells as f64
            };
            let avg_width = if cells == 0 {
                str_width(&head_plain) as f64
            } else {
                total as f64 / cells as f64
            };
            let kind = if long > 0 && long >= tokens.saturating_sub(long) {
                ColKind::TokenHeavy
            } else if avg_words >= 4.0 || avg_width >= 28.0 {
                ColKind::Narrative
            } else {
                ColKind::Compact
            };
            Metrics {
                max_width,
                header_token: tokens_width(&head_plain),
                body_token,
                kind,
            }
        })
        .collect()
}

fn widths(metrics: &[Metrics], room: usize) -> Option<Vec<usize>> {
    const MIN: usize = 3;
    let mut widths: Vec<usize> = metrics.iter().map(|m| m.max_width.max(MIN)).collect();
    if room < metrics.len() * MIN {
        return None;
    }
    let mut floors: Vec<usize> = metrics
        .iter()
        .map(|m| {
            let target = match m.kind {
                ColKind::Narrative | ColKind::TokenHeavy => 16,
                ColKind::Compact => m.header_token.max(m.body_token.min(16)),
            };
            target.max(MIN).min(m.max_width)
        })
        .collect();
    let floor_total: usize = floors.iter().sum();
    if floor_total > room {
        let mins = vec![MIN; floors.len()];
        shrink(&mut floors, &mins, metrics, floor_total - room);
    }
    let total: usize = widths.iter().sum();
    if total > room && shrink(&mut widths, &floors, metrics, total - room) > 0 {
        return None;
    }
    Some(widths)
}

fn shrink(widths: &mut [usize], floors: &[usize], metrics: &[Metrics], mut amount: usize) -> usize {
    for kind in [ColKind::TokenHeavy, ColKind::Narrative, ColKind::Compact] {
        let of_kind = |i: usize| metrics[i].kind == kind;
        let slack = |w: &[usize], i: usize| w[i].saturating_sub(floors[i]);
        let total: usize = (0..widths.len())
            .filter(|&i| of_kind(i))
            .map(|i| slack(widths, i))
            .sum();
        let remove = amount.min(total);
        if remove == 0 {
            continue;
        }
        let (mut low, mut high) = (
            0,
            (0..widths.len())
                .filter(|&i| of_kind(i))
                .map(|i| slack(widths, i))
                .max()
                .unwrap_or(0),
        );
        while low < high {
            let cap = low + (high - low) / 2;
            let removed: usize = (0..widths.len())
                .filter(|&i| of_kind(i))
                .map(|i| slack(widths, i).saturating_sub(cap))
                .sum();
            if removed > remove {
                low = cap + 1;
            } else {
                high = cap;
            }
        }
        let cap = low;
        let mut removed = 0;
        for i in 0..widths.len() {
            if of_kind(i) {
                let r = slack(widths, i).saturating_sub(cap);
                widths[i] -= r;
                removed += r;
            }
        }
        let mut rest = remove - removed;
        for i in 0..widths.len() {
            if rest == 0 {
                break;
            }
            if of_kind(i) && slack(widths, i) == cap {
                widths[i] -= 1;
                rest -= 1;
            }
        }
        amount -= remove;
        if amount == 0 {
            break;
        }
    }
    amount
}

fn wrap_cell(cell: &Cell, width: usize) -> Vec<Line> {
    let mut out = Vec::new();
    for l in &cell.lines {
        out.extend(wrap(l, width.max(1), &[], &[]));
    }
    if out.is_empty() {
        out.push(Line::new());
    }
    out
}

fn rule(widths: &[usize], c: char) -> Line {
    let parts: Vec<String> = widths.iter().map(|w| c.to_string().repeat(w + 2)).collect();
    glyphs(RULE, &parts.join("  "), None)
}

fn plain_rows(lines: Vec<Line>) -> Vec<(Line, Style)> {
    lines.into_iter().map(|l| (l, Style::PLAIN)).collect()
}

fn table_row(row: &[Cell], widths: &[usize], align: &[Alignment]) -> Vec<Line> {
    let cells: Vec<Vec<Line>> = row
        .iter()
        .zip(widths)
        .map(|(c, w)| wrap_cell(c, *w))
        .collect();
    let h = cells.iter().map(Vec::len).max().unwrap_or(1);
    (0..h)
        .map(|i| {
            let last = cells
                .iter()
                .rposition(|l| l.get(i).is_some_and(|x| width_of(x) > 0));
            let Some(last) = last else {
                return cells
                    .iter()
                    .find_map(|c| c.get(i))
                    .cloned()
                    .unwrap_or_default();
            };
            let mut out = Line::new();
            for (c, w) in widths.iter().enumerate().take(last + 1) {
                out.extend(spaces(1));
                let line = cells[c].get(i).cloned().unwrap_or_default();
                let rest = w.saturating_sub(width_of(&line));
                let (l, r) = match align.get(c).copied().unwrap_or(Alignment::None) {
                    Alignment::Center => (rest / 2, rest - rest / 2),
                    Alignment::Right => (rest, 0),
                    _ => (0, rest),
                };
                out.extend(spaces(l));
                out.extend(line);
                if c != last {
                    out.extend(spaces(r + 1 + 2));
                }
            }
            out
        })
        .collect()
}

/// `should_render_records`: enough rows whose values the columns break.
fn should_records(rows: &[Vec<Cell>], widths: &[usize], metrics: &[Metrics]) -> bool {
    if rows.is_empty() {
        return false;
    }
    let starved = |row: &[Cell]| {
        let wide: Vec<(ColKind, usize, usize)> = row
            .iter()
            .zip(widths)
            .zip(metrics)
            .filter(|(_, m)| m.kind != ColKind::Compact)
            .map(|((c, w), m)| (m.kind, *w, wrap_cell(c, *w).len()))
            .collect();
        wide.iter().filter(|(_, _, h)| *h >= 4).count() >= 2
            || wide
                .iter()
                .any(|(k, w, h)| *k == ColKind::Narrative && *w < 12 && *h >= 7)
    };
    let hit = rows
        .iter()
        .filter(|row| {
            let broken = row.iter().zip(widths).zip(metrics).any(|((c, w), m)| {
                let long = c.plain().split_whitespace().any(|t| str_width(t) > *w);
                match m.kind {
                    ColKind::Compact => long,
                    ColKind::TokenHeavy => *w < 12 && long,
                    ColKind::Narrative => false,
                }
            });
            broken || starved(row)
        })
        .count();
    let threshold = if rows.len() == 1 {
        1
    } else {
        2.max(rows.len().div_ceil(3))
    };
    hit >= threshold
}

/// `render_records`: each row as "Header  value" lines, a rule between rows.
fn records(header: &[Cell], rows: &[Vec<Cell>], metrics: &[Metrics], room: usize) -> Vec<Line> {
    let label_w = header
        .iter()
        .map(|h| str_width(&h.plain()))
        .max()
        .unwrap_or(0);
    let min_value = if metrics.iter().any(|m| m.kind != ColKind::Compact) {
        24
    } else {
        12
    };
    let aligned = 1 + label_w + 2 + min_value <= room;
    let mut out = Vec::new();
    for (k, row) in rows.iter().enumerate() {
        for (h, v) in header.iter().zip(row) {
            let label = glyphs(HEADER, &h.plain(), None);
            if aligned {
                let indent = 1 + label_w + 2;
                let vw = room.saturating_sub(indent).max(3);
                for (j, line) in wrap_cell(v, vw).into_iter().enumerate() {
                    let mut row = if j == 0 {
                        let mut r = spaces(1);
                        r.extend(label.iter().copied());
                        r.extend(spaces(label_w.saturating_sub(width_of(&label)) + 2));
                        r
                    } else {
                        spaces(indent)
                    };
                    row.extend(line);
                    out.push(row);
                }
            } else {
                for l in wrap(&label, room.saturating_sub(1).max(1), &[], &[]) {
                    let mut r = spaces(1);
                    r.extend(l);
                    out.push(r);
                }
                for line in wrap_cell(v, room.saturating_sub(2).max(1)) {
                    let mut r = spaces(2);
                    r.extend(line);
                    out.push(r);
                }
            }
        }
        if k + 1 < rows.len() {
            out.push(glyphs(RULE, &"─".repeat(room), None));
        }
    }
    out
}

/// A table without rows that does not fit: its markdown pipes.
fn pipe_rows(header: &[Cell], rows: &[Vec<Cell>], align: &[Alignment]) -> Vec<Line> {
    let row = |cells: &[Cell]| {
        let mut out = glyphs(Style::PLAIN, "|", None);
        for c in cells {
            out.extend(spaces(1));
            for (i, l) in c.lines.iter().enumerate() {
                if i > 0 {
                    out.extend(spaces(1));
                }
                for g in l {
                    if g.c == '|' {
                        out.push(Glyph { c: '\\', ..*g });
                    }
                    out.push(*g);
                }
            }
            out.extend(glyphs(Style::PLAIN, " |", None));
        }
        out
    };
    let mut out = vec![row(header)];
    let mut rule = String::from("|");
    for a in align {
        rule.push_str(match a {
            Alignment::Left => ":---",
            Alignment::Center => ":---:",
            Alignment::Right => "---:",
            Alignment::None => "---",
        });
        rule.push('|');
    }
    out.push(glyphs(Style::PLAIN, &rule, None));
    out.extend(rows.iter().map(|r| row(r)));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::tests::{Shot, oracle};
    use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

    /// Each answer of `claude_markdown_cases.md` as Codex CLI 0.163.0 drew
    /// it (`codex resume` of a saved chat, inline, 60 and 40 columns, in a
    /// terminal that shows a link's text alone and does not tell Codex its
    /// colours).
    #[test]
    fn answers_look_as_codex_draws_them() {
        let colour = |c: &Color| match c {
            Color::Named(NamedColor::Foreground) => Some(Fg::Plain),
            Color::Spec(Rgb {
                r: 0xa6,
                g: 0xe3,
                b: 0xa1,
            }) => Some(Fg::Code),
            Color::Spec(Rgb {
                r: 0x63,
                g: 0xa8,
                b: 0xf8,
            }) => Some(Fg::Link),
            Color::Spec(Rgb {
                r: 0xf9,
                g: 0xe2,
                b: 0xaf,
            }) => Some(Fg::Header),
            Color::Indexed(2) => Some(Fg::Quote),
            Color::Indexed(12) => Some(Fg::Number),
            _ => None,
        };
        let shot = |bytes: &'static [u8], cols, rows| Shot {
            bytes,
            cols,
            rows,
            history: true,
        };
        let shots = [
            shot(
                include_bytes!("../../tests/fixtures/codex_markdown_60x300.bin"),
                60,
                300,
            ),
            shot(
                include_bytes!("../../tests/fixtures/codex_markdown_40x400.bin"),
                40,
                400,
            ),
        ];
        oracle(
            |md, w| render(md, w, false),
            &shots,
            "› case ",
            2,
            |cols| cols - 2,
            colour,
            |fg| fg,
        );
    }
}
