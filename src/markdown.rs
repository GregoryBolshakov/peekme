//! An answer's markdown as box lines, laid out the way Claude Code lays out
//! its own answers (2.1.307): the same blank lines, list markers and indents,
//! tables drawn with box lines (or as "Header: value" lines when they do not
//! fit), quotes behind a bar, and the same places to wrap. Claude's rules are
//! those of its renderer over `marked`; the parse here is pulldown-cmark, and
//! the few places the two parsers differ are noted where they matter.
//!
//! Only the colours are the agent's (`set_look`): inline code takes the
//! agent's code colour.

use std::ops::Range;
use std::sync::OnceLock;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use unicode_width::UnicodeWidthChar;

/// The colour of a glyph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Fg {
    #[default]
    Plain,
    Code,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Style {
    pub fg: Fg,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
}

impl Style {
    pub const PLAIN: Style = Style {
        fg: Fg::Plain,
        bold: false,
        dim: false,
        italic: false,
        underline: false,
        strike: false,
    };
    pub const BOLD: Style = Style {
        bold: true,
        ..Style::PLAIN
    };
    pub const DIM: Style = Style {
        dim: true,
        ..Style::PLAIN
    };
    pub const CODE: Style = Style {
        fg: Fg::Code,
        ..Style::PLAIN
    };
    pub const ERROR: Style = Style {
        fg: Fg::Error,
        ..Style::PLAIN
    };

    /// The escape that sets this style from any other.
    pub fn sgr(self) -> String {
        let mut s = String::from("\x1b[0");
        for (on, p) in [
            (self.bold, ";1"),
            (self.dim, ";2"),
            (self.italic, ";3"),
            (self.underline, ";4"),
            (self.strike, ";9"),
        ] {
            if on {
                s.push_str(p);
            }
        }
        match self.fg {
            Fg::Plain => {}
            Fg::Code => {
                s.push(';');
                s.push_str(&look().code);
            }
            Fg::Error => s.push_str(";31"),
        }
        s.push('m');
        s
    }
}

/// How the agent draws: SGR parameters of its inline code, and whether the
/// terminal shows struck-through text (else "~~" stays around it, as Claude
/// Code does).
pub struct Look {
    pub code: String,
    pub strike: bool,
}

static LOOK: OnceLock<Look> = OnceLock::new();

/// Draw this run's answers with `look`.
pub fn set_look(look: Look) {
    let _ = LOOK.set(look);
}

fn look() -> &'static Look {
    LOOK.get_or_init(|| Look {
        code: "36".into(),
        strike: true,
    })
}

/// Whether the terminal shows SGR 9, by Claude Code's own test (Apple's
/// Terminal and the Linux console do not; others only when known to).
pub fn strike_shown(get: impl Fn(&str) -> Option<String>) -> bool {
    let set = |k: &str| get(k).is_some_and(|v| !v.is_empty());
    let term = get("TERM").unwrap_or_default();
    let program = get("TERM_PROGRAM").unwrap_or_default();
    if set("CLAUDE_CODE_FORCE_STRIKETHROUGH") {
        return true;
    }
    if program == "Apple_Terminal" || term == "linux" {
        return false;
    }
    [
        "iTerm.app",
        "vscode",
        "WezTerm",
        "WarpTerminal",
        "Hyper",
        "Tabby",
        "rio",
        "contour",
        "alacritty",
        "ghostty",
        "mintty",
    ]
    .contains(&program.as_str())
        || term.contains("ghostty")
        || get("TERMINAL_EMULATOR").as_deref() == Some("JetBrains-JediTerm")
        || get("LC_TERMINAL").as_deref() == Some("iTerm2")
        || term.contains("kitty")
        || term.contains("alacritty")
        || term.starts_with("foot")
        || [
            "KITTY_WINDOW_ID",
            "ALACRITTY_LOG",
            "KONSOLE_VERSION",
            "WT_SESSION",
            "ZED_TERM",
        ]
        .iter()
        .any(|k| set(k))
        || get("VTE_VERSION")
            .and_then(|v| v.parse::<u32>().ok())
            .is_some_and(|v| v >= 4400)
}

/// One char of the box text: its style and where it is in the answer (none
/// for what the box adds: indents, markers, borders, "…").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Glyph {
    pub c: char,
    pub style: Style,
    pub src: Option<usize>,
}

pub type Line = Vec<Glyph>;

/// `text` as glyphs, its first char at `src` of the answer when it is from it.
pub fn glyphs(style: Style, text: &str, src: Option<usize>) -> Line {
    text.chars()
        .enumerate()
        .map(|(i, c)| Glyph {
            c,
            style,
            src: src.map(|s| s + i),
        })
        .collect()
}

pub fn width_of(line: &[Glyph]) -> usize {
    line.iter().map(|g| g.c.width().unwrap_or(0)).sum()
}

/// The lines of `text` (markdown), `width` columns wide.
pub fn lines(text: &str, width: usize) -> Vec<Line> {
    let nodes = parse(text);
    let lay = Lay { text, width };
    let mut lines = lay.top(&nodes);
    // A space is in the words on both sides of it, not an answer char of its
    // own: so a mark ends with its last word, not under the space after it.
    for g in lines.iter_mut().flatten().filter(|g| g.c == ' ') {
        g.src = None;
    }
    lines
}

// The parse: blocks of glyphs.

#[derive(Debug)]
enum Block {
    /// `loose`: a paragraph of its own (not the text of a tight list item).
    Para(Line, bool),
    Heading(u8, Line),
    Code {
        text: Line,
        lang: bool,
        indented: bool,
    },
    Quote(Vec<Node>),
    List(Option<u64>, Vec<Item>),
    Table {
        align: Vec<Alignment>,
        head: Vec<Line>,
        rows: Vec<Vec<Line>>,
    },
    Rule,
    Html(Line),
}

#[derive(Debug)]
struct Node {
    block: Block,
    /// Bytes of the answer.
    range: Range<usize>,
}

#[derive(Debug)]
struct Item {
    task: Option<bool>,
    children: Vec<Node>,
}

struct Parse<'a> {
    text: &'a str,
    /// The char index of each byte offset.
    at: Vec<usize>,
    ev: Vec<(Event<'a>, Range<usize>)>,
    i: usize,
    task: Option<bool>,
}

fn parse(text: &str) -> Vec<Node> {
    let mut at = vec![0; text.len() + 1];
    let mut n = 0;
    for (b, c) in text.char_indices() {
        for k in 0..c.len_utf8() {
            at[b + k] = n;
        }
        n += 1;
    }
    at[text.len()] = n;
    // Subscript only so that a single "~" pair is not struck through: Claude
    // Code strikes "~~" only, and "~5 s to ~10 s" is common in answers.
    let opts = Options::ENABLE_TABLES
        | Options::ENABLE_STRIKETHROUGH
        | Options::ENABLE_SUBSCRIPT
        | Options::ENABLE_TASKLISTS;
    let mut p = Parse {
        text,
        at,
        ev: Parser::new_ext(text, opts).into_offset_iter().collect(),
        i: 0,
        task: None,
    };
    let mut nodes = Vec::new();
    while p.i < p.ev.len() {
        nodes.extend(p.blocks());
        // A stray end event: skip it.
        p.i += 1;
    }
    nodes
}

fn inline_end(t: &TagEnd) -> bool {
    matches!(
        t,
        TagEnd::Emphasis
            | TagEnd::Strong
            | TagEnd::Strikethrough
            | TagEnd::Superscript
            | TagEnd::Subscript
            | TagEnd::Link
            | TagEnd::Image
    )
}

fn inline_start(t: &Tag) -> bool {
    matches!(
        t,
        Tag::Emphasis
            | Tag::Strong
            | Tag::Strikethrough
            | Tag::Superscript
            | Tag::Subscript
            | Tag::Link { .. }
            | Tag::Image { .. }
    )
}

enum Open {
    Style,
    /// Text put back around what the terminal cannot show ("~~", "~", "^").
    Literal(&'static str),
    Link(String, usize),
    Image(String, String, usize),
}

impl<'a> Parse<'a> {
    /// Blocks up to the end of their container (left for the caller).
    fn blocks(&mut self) -> Vec<Node> {
        let mut out = Vec::new();
        while let Some((ev, range)) = self.ev.get(self.i) {
            let range = range.clone();
            match ev {
                Event::End(_) => break,
                Event::Rule => {
                    self.i += 1;
                    out.push(Node {
                        block: Block::Rule,
                        range,
                    });
                }
                Event::Start(t) if !inline_start(t) => {
                    if let Some(block) = self.block() {
                        out.push(Node { block, range });
                    }
                }
                _ => {
                    // Text straight in a tight list item.
                    let (from, before) = (range.start, self.i);
                    let (line, to) = self.inlines();
                    if self.i == before {
                        // Nothing it could read: skip the event.
                        self.i += 1;
                        continue;
                    }
                    out.push(Node {
                        block: Block::Para(line, false),
                        range: from..to.max(from),
                    });
                }
            }
        }
        out
    }

    /// The block whose start event is next, through its end event.
    fn block(&mut self) -> Option<Block> {
        let Event::Start(tag) = self.ev[self.i].0.clone() else {
            unreachable!()
        };
        self.i += 1;
        let block = match tag {
            Tag::Paragraph => Some(Block::Para(self.inlines().0, true)),
            Tag::Heading { level, .. } => Some(Block::Heading(level as u8, self.inlines().0)),
            Tag::CodeBlock(kind) => {
                let mut text = Line::new();
                while let Some((Event::Text(t), r)) = self.ev.get(self.i) {
                    self.push_text(&mut text, &t.clone(), r.clone(), Style::PLAIN);
                    self.i += 1;
                }
                if text.last().is_some_and(|g| g.c == '\n') {
                    text.pop();
                }
                let lang = matches!(&kind, CodeBlockKind::Fenced(info) if !info.trim().is_empty());
                Some(Block::Code {
                    text,
                    lang,
                    indented: kind.is_indented(),
                })
            }
            Tag::BlockQuote(_) => Some(Block::Quote(self.blocks())),
            Tag::HtmlBlock => {
                let mut text = Line::new();
                while let Some((Event::Html(t) | Event::Text(t), r)) = self.ev.get(self.i) {
                    self.push_text(&mut text, &t.clone(), r.clone(), Style::PLAIN);
                    self.i += 1;
                }
                Some(Block::Html(text))
            }
            Tag::List(start) => {
                let mut items = Vec::new();
                while let Some((Event::Start(Tag::Item), _)) = self.ev.get(self.i) {
                    self.i += 1;
                    self.task = None;
                    let children = self.blocks();
                    self.i += 1; // the item's end
                    items.push(Item {
                        task: self.task.take(),
                        children,
                    });
                }
                Some(Block::List(start, items))
            }
            Tag::Table(align) => {
                let (mut head, mut rows) = (Vec::new(), Vec::new());
                loop {
                    match self.ev.get(self.i).map(|e| &e.0) {
                        Some(Event::Start(Tag::TableHead)) => {
                            self.i += 1;
                            head = self.cells();
                        }
                        Some(Event::Start(Tag::TableRow)) => {
                            self.i += 1;
                            rows.push(self.cells());
                        }
                        _ => break,
                    }
                }
                for row in &mut rows {
                    row.resize(head.len(), Line::new());
                }
                Some(Block::Table { align, head, rows })
            }
            _ => {
                // Containers the options never turn on: their blocks are dropped.
                self.blocks();
                None
            }
        };
        // The block's end event.
        self.i += 1;
        block
    }

    /// The cells of a table row, through the row's end event.
    fn cells(&mut self) -> Vec<Line> {
        let mut cells = Vec::new();
        while let Some((Event::Start(Tag::TableCell), _)) = self.ev.get(self.i) {
            self.i += 1;
            cells.push(self.inlines().0);
            self.i += 1;
        }
        self.i += 1;
        cells
    }

    /// Inline events up to the first that is not one (left for the caller);
    /// also the byte where the last one ends.
    fn inlines(&mut self) -> (Line, usize) {
        let mut out = Line::new();
        let mut styles = vec![Style::PLAIN];
        let mut open: Vec<Open> = Vec::new();
        let mut end = self.ev.get(self.i).map_or(0, |e| e.1.start);
        while let Some((ev, r)) = self.ev.get(self.i) {
            let r = r.clone();
            let style = *styles.last().unwrap();
            match ev.clone() {
                Event::Text(t)
                | Event::Html(t)
                | Event::InlineHtml(t)
                | Event::InlineMath(t)
                | Event::DisplayMath(t)
                | Event::FootnoteReference(t) => self.push_text(&mut out, &t, r.clone(), style),
                Event::Code(t) => self.push_text(
                    &mut out,
                    &t,
                    r.clone(),
                    Style {
                        fg: Fg::Code,
                        ..style
                    },
                ),
                Event::SoftBreak | Event::HardBreak => out.push(Glyph {
                    c: '\n',
                    style: Style::PLAIN,
                    src: None,
                }),
                Event::TaskListMarker(done) => self.task = Some(done),
                Event::Start(tag) if inline_start(&tag) => {
                    let (next, o) = match tag {
                        Tag::Emphasis => (
                            Style {
                                italic: true,
                                ..style
                            },
                            Open::Style,
                        ),
                        Tag::Strong => (
                            Style {
                                bold: true,
                                ..style
                            },
                            Open::Style,
                        ),
                        Tag::Strikethrough if look().strike => (
                            Style {
                                strike: true,
                                ..style
                            },
                            Open::Style,
                        ),
                        Tag::Strikethrough => (style, Open::Literal("~~")),
                        Tag::Subscript => (style, Open::Literal("~")),
                        Tag::Superscript => (style, Open::Literal("^")),
                        Tag::Link { dest_url, .. } => {
                            (style, Open::Link(dest_url.to_string(), out.len()))
                        }
                        Tag::Image {
                            dest_url, title, ..
                        } => (
                            style,
                            Open::Image(dest_url.to_string(), title.to_string(), out.len()),
                        ),
                        _ => unreachable!(),
                    };
                    if let Open::Literal(s) = o {
                        out.extend(glyphs(style, s, None));
                    }
                    styles.push(next);
                    open.push(o);
                }
                Event::End(tag) if inline_end(&tag) && styles.len() > 1 => {
                    styles.pop();
                    match open.pop() {
                        Some(Open::Literal(s)) => out.extend(glyphs(style, s, None)),
                        Some(Open::Link(url, from)) => {
                            self.end_link(&mut out, from, &url, r.clone())
                        }
                        Some(Open::Image(url, title, from)) => {
                            self.end_image(&mut out, from, &url, &title, r.clone())
                        }
                        _ => {}
                    }
                }
                _ => break,
            }
            end = r.end;
            self.i += 1;
        }
        (out, end)
    }

    /// A link as Claude Code shows it where the terminal takes no hyperlinks:
    /// "text (url)", or the url alone when it is the text.
    fn end_link(&self, out: &mut Line, from: usize, url: &str, r: Range<usize>) {
        let shown_url = url.strip_prefix("mailto:").unwrap_or(url);
        let text: String = out[from..].iter().map(|g| g.c).collect();
        let same = text.is_empty()
            || text == shown_url
            || url == format!("http://{text}")
            || url == format!("https://{text}");
        let url_glyphs = self.literal(shown_url, r, Style::PLAIN);
        if same {
            out.truncate(from);
            out.extend(url_glyphs);
        } else {
            out.extend(glyphs(Style::PLAIN, " (", None));
            out.extend(url_glyphs);
            out.extend(glyphs(Style::PLAIN, ")", None));
        }
    }

    fn end_image(&self, out: &mut Line, from: usize, url: &str, title: &str, r: Range<usize>) {
        let url_glyphs = self.literal(url, r, Style::PLAIN);
        if from == out.len() && title.is_empty() {
            out.extend(url_glyphs);
            return;
        }
        out.extend(glyphs(
            Style::PLAIN,
            if from == out.len() { "(" } else { " (" },
            None,
        ));
        out.extend(url_glyphs);
        if !title.is_empty() {
            out.extend(glyphs(Style::PLAIN, &format!(" \"{title}\""), None));
        }
        out.extend(glyphs(Style::PLAIN, ")", None));
    }

    /// `s` as glyphs at its place within `r` of the answer, when it is there.
    fn literal(&self, s: &str, r: Range<usize>, style: Style) -> Line {
        let src = self.text[r.clone()].find(s).map(|b| self.at[r.start + b]);
        glyphs(style, s, src)
    }

    /// `t`, the text of an event over bytes `r` of the answer: each char at
    /// its place there (escapes and entities make the two differ).
    fn push_text(&self, out: &mut Line, t: &str, r: Range<usize>, style: Style) {
        let r = r.start.min(self.text.len())..r.end.min(self.text.len());
        if &self.text[r.clone()] == t {
            out.extend(glyphs(style, t, Some(self.at[r.start])));
            return;
        }
        let mut pos = r.start;
        for c in t.chars() {
            let found = self
                .text
                .get(pos..r.end)
                .and_then(|s| s.find(c))
                .map(|b| pos + b);
            if let Some(p) = found {
                pos = p + c.len_utf8();
            }
            out.push(Glyph {
                c,
                style,
                src: found.map(|p| self.at[p]),
            });
        }
    }
}

// The layout.

fn nl() -> Glyph {
    Glyph {
        c: '\n',
        style: Style::PLAIN,
        src: None,
    }
}

fn spaces(n: usize) -> Line {
    glyphs(Style::PLAIN, &" ".repeat(n), None)
}

fn restyle(line: &mut [Glyph], f: impl Fn(&mut Style)) {
    for g in line.iter_mut().filter(|g| g.c != '\n') {
        f(&mut g.style);
    }
}

/// Text split at its line breaks.
fn split(text: &[Glyph]) -> Vec<Line> {
    text.split(|g| g.c == '\n').map(<[Glyph]>::to_vec).collect()
}

/// Without line breaks in front or white space at the end.
fn trimmed(text: &[Glyph]) -> &[Glyph] {
    let a = text.iter().take_while(|g| g.c == '\n').count();
    let b = text.len()
        - text[a..]
            .iter()
            .rev()
            .take_while(|g| g.c.is_whitespace())
            .count();
    &text[a..b.max(a)]
}

/// Ends in a blank line.
fn blank_at_end(text: &[Glyph]) -> bool {
    if text.last().is_none_or(|g| g.c != '\n') {
        return false;
    }
    for g in text[..text.len() - 1].iter().rev() {
        if g.c == '\n' {
            return true;
        }
        if !g.c.is_whitespace() {
            return false;
        }
    }
    false
}

/// A blank line between blocks: `marked` keeps it as a "space" token.
enum Tok<'n> {
    Space,
    Node(&'n Node),
}

/// The marker of item `n` of a list `depth` deep: "-", or a number, a letter
/// (second level) or a roman number (third).
fn marker(depth: usize, ordered: Option<(u64, u64, u64)>) -> String {
    let Some((n, first, last)) = ordered else {
        return "-".into();
    };
    let s = match depth + 1 {
        2 if first >= 1 => letters(n),
        3 if first >= 1 && last <= 3999 => roman(n),
        _ => n.to_string(),
    };
    format!("{s}.")
}

fn letters(mut n: u64) -> String {
    let mut s = Vec::new();
    while n > 0 {
        n -= 1;
        s.push(b'a' + (n % 26) as u8);
        n /= 26;
    }
    s.reverse();
    String::from_utf8(s).unwrap()
}

fn roman(mut n: u64) -> String {
    const R: [(u64, &str); 13] = [
        (1000, "m"),
        (900, "cm"),
        (500, "d"),
        (400, "cd"),
        (100, "c"),
        (90, "xc"),
        (50, "l"),
        (40, "xl"),
        (10, "x"),
        (9, "ix"),
        (5, "v"),
        (4, "iv"),
        (1, "i"),
    ];
    let mut s = String::new();
    for (v, r) in R {
        while n >= v {
            s.push_str(r);
            n -= v;
        }
    }
    s
}

fn is_block(n: &Node) -> bool {
    matches!(
        n.block,
        Block::Code { .. } | Block::Quote(_) | Block::Rule | Block::Table { .. } | Block::List(..)
    )
}

/// Glyphs padded to `w` columns as aligned in a table cell.
fn aligned(text: &[Glyph], w: usize, align: Alignment) -> Line {
    let gap = w.saturating_sub(width_of(text));
    let (l, r) = match align {
        Alignment::Center => (gap / 2, gap - gap / 2),
        Alignment::Right => (gap, 0),
        _ => (0, gap),
    };
    let mut out = spaces(l);
    out.extend_from_slice(text);
    out.extend(spaces(r));
    out
}

struct Lay<'a> {
    text: &'a str,
    width: usize,
}

/// What the last part drawn at the top level was.
#[derive(Clone, Copy, PartialEq)]
enum Last {
    None,
    Prose,
    List,
    Block,
}

struct Top {
    lines: Vec<Line>,
    last: Last,
    /// The prose before ended in a blank line.
    gap: bool,
}

impl Top {
    fn put(&mut self, margin: bool, lines: Vec<Line>, last: Last) {
        if margin {
            self.lines.push(Line::new());
        }
        self.lines.extend(lines);
        self.last = last;
        self.gap = false;
    }

    /// The blank line before a list or a block.
    fn margin(&self, block: bool) -> bool {
        self.last != Last::None && (block || self.last == Last::Block || self.gap)
    }
}

impl Lay<'_> {
    /// `nodes` with the blank lines between them that `marked` keeps: after
    /// a heading, a table or an indented code block it never does.
    fn spaced<'n>(&self, nodes: &'n [Node]) -> Vec<Tok<'n>> {
        let mut out = Vec::new();
        for (k, n) in nodes.iter().enumerate() {
            if let Some(p) = k.checked_sub(1).map(|k| &nodes[k]) {
                let eats = matches!(
                    p.block,
                    Block::Heading(..) | Block::Table { .. } | Block::Code { indented: true, .. }
                );
                let end = p.range.start
                    + self
                        .text
                        .get(p.range.clone())
                        .map_or(0, |s| s.trim_end().len());
                let gap = self.text.get(end..n.range.start).unwrap_or("");
                if !eats && gap.matches('\n').count() >= 2 {
                    out.push(Tok::Space);
                }
            }
            out.push(Tok::Node(n));
        }
        out
    }

    /// The text Claude Code makes of a block before laying it out (`h0`):
    /// what runs into one stretch of prose, and what lists, quotes and tables
    /// inside lists and quotes become.
    fn prose(&self, tok: &Tok, depth: usize, task: Option<bool>) -> Line {
        let Tok::Node(n) = tok else {
            return vec![nl()];
        };
        let mut out = match &n.block {
            Block::Para(line, _) => {
                let mut out = match task {
                    Some(done) => glyphs(Style::PLAIN, if done { "[x] " } else { "[ ] " }, None),
                    None => Line::new(),
                };
                out.extend_from_slice(line);
                out
            }
            Block::Heading(level, line) => {
                let mut out = line.clone();
                restyle(&mut out, |s| {
                    s.bold = true;
                    if *level == 1 {
                        s.italic = true;
                        s.underline = true;
                    }
                });
                out.push(nl());
                out
            }
            Block::Code {
                text,
                lang,
                indented,
            } => {
                let mut out = text.clone();
                if !lang && !indented {
                    restyle(&mut out, |s| s.fg = Fg::Code);
                }
                out
            }
            Block::Rule => glyphs(Style::PLAIN, "---", None),
            Block::Html(text) => return text.clone(),
            Block::Quote(children) => return self.quote_text(children),
            Block::List(start, items) => return self.list_text(*start, items, depth, 0),
            Block::Table { align, head, rows } => return table_text(align, head, rows),
        };
        out.push(nl());
        out
    }

    /// A quote inside a list or a quote: a dim bar in front of each line that
    /// has text, one more for each quote it is in.
    fn quote_text(&self, children: &[Node]) -> Line {
        let mut text = Line::new();
        for tok in self.spaced(children) {
            text.extend(self.prose(&tok, 0, None));
        }
        let mut out = Line::new();
        for (k, mut line) in split(&text).into_iter().enumerate() {
            if k > 0 {
                out.push(nl());
            }
            if line.iter().any(|g| !g.c.is_whitespace()) {
                out.push(Glyph {
                    c: '▎',
                    style: Style::DIM,
                    src: None,
                });
                out.extend(spaces(1));
                restyle(&mut line, |s| s.italic = true);
            }
            out.extend(line);
        }
        if out.last().is_some_and(|g| g.c != '\n') {
            out.push(nl());
        }
        out
    }

    /// A list inside a quote, as text: markers, and continuation lines
    /// indented under the text.
    fn list_text(&self, start: Option<u64>, items: &[Item], depth: usize, indent: usize) -> Line {
        let first = start.unwrap_or(1);
        let last = first + items.len().saturating_sub(1) as u64;
        let mut out = Line::new();
        for (u, item) in items.iter().enumerate() {
            let f = marker(depth, start.map(|_| (first + u as u64, first, last)));
            let h = indent + f.chars().count() + 1;
            let inner = h.min(32);
            let toks = self.spaced(&item.children);
            let toks: Vec<&Tok> = toks
                .iter()
                .skip_while(|t| matches!(t, Tok::Space))
                .collect();
            let starts_block = toks
                .first()
                .is_some_and(|t| matches!(t, Tok::Node(n) if is_block(n)));
            let mut bare = !starts_block;
            let mut body = Line::new();
            for (k, tok) in toks.iter().enumerate() {
                let o = match tok {
                    Tok::Node(Node {
                        block: Block::List(s, inner_items),
                        ..
                    }) => self.list_text(*s, inner_items, depth + 1, inner),
                    _ => self.prose(tok, depth + 1, task_of(item, k, tok)),
                };
                let block = matches!(tok, Tok::Space) || matches!(tok, Tok::Node(n) if is_block(n));
                if block {
                    if matches!(
                        tok,
                        Tok::Node(Node {
                            block: Block::Code { .. },
                            ..
                        })
                    ) && o.len() == 1
                    {
                        continue;
                    }
                    let lead = if matches!(
                        tok,
                        Tok::Node(Node {
                            block: Block::Quote(_) | Block::Rule | Block::Table { .. },
                            ..
                        })
                    ) {
                        o.iter().take_while(|g| g.c == '\n').count()
                    } else {
                        0
                    };
                    body.extend_from_slice(&o[lead..]);
                    continue;
                }
                let first_prefix = if bare {
                    let mut p = spaces(indent);
                    p.extend(glyphs(Style::PLAIN, &format!("{f} "), None));
                    p
                } else {
                    spaces(h)
                };
                bare = false;
                for (v, line) in split(&o).into_iter().enumerate() {
                    if v > 0 {
                        body.push(nl());
                        if line.is_empty() {
                            continue;
                        }
                        body.extend(spaces(h));
                    } else {
                        body.extend(first_prefix.iter().copied());
                    }
                    body.extend(line);
                }
            }
            if bare || starts_block {
                out.extend(spaces(indent));
                out.extend(glyphs(Style::PLAIN, &f, None));
                out.push(nl());
            }
            out.extend(body);
        }
        out
    }

    fn top(&self, nodes: &[Node]) -> Vec<Line> {
        let mut top = Top {
            lines: Vec::new(),
            last: Last::None,
            gap: false,
        };
        let mut prose = Line::new();
        for tok in self.spaced(nodes) {
            match tok {
                Tok::Node(Node {
                    block: Block::Table { align, head, rows },
                    ..
                }) => {
                    self.flush(&mut top, &mut prose);
                    let m = top.margin(true);
                    top.put(m, self.table(align, head, rows), Last::Block);
                }
                Tok::Node(Node {
                    block: Block::List(start, items),
                    ..
                }) => {
                    self.flush(&mut top, &mut prose);
                    let m = top.margin(false);
                    let lines = self.list(*start, items, 0, 0, m);
                    top.put(false, lines, Last::List);
                }
                Tok::Node(Node {
                    block: Block::Quote(children),
                    ..
                }) => {
                    self.flush(&mut top, &mut prose);
                    let m = top.margin(true);
                    top.put(m, self.quote(children), Last::Block);
                }
                _ => prose.extend(self.prose(&tok, 0, None)),
            }
        }
        self.flush(&mut top, &mut prose);
        top.lines
    }

    /// Lay out the prose gathered so far.
    fn flush(&self, top: &mut Top, prose: &mut Line) {
        if prose.is_empty() {
            return;
        }
        let text = std::mem::take(prose);
        let lead = text[0].c == '\n';
        let gap = blank_at_end(&text);
        let body = trimmed(&text);
        if body.is_empty() {
            top.gap = true;
            return;
        }
        let margin = match top.last {
            Last::None => false,
            Last::List => lead,
            _ => true,
        };
        top.put(margin, wrap_text(body, self.width), Last::Prose);
        top.gap = gap;
    }

    /// A quote: a dim bar on each row, its text in italics.
    fn quote(&self, children: &[Node]) -> Vec<Line> {
        let mut text = Line::new();
        for tok in self.spaced(children) {
            text.extend(match tok {
                Tok::Node(Node {
                    block: Block::Quote(inner),
                    ..
                }) => self.quote_text(inner),
                _ => self.prose(&tok, 0, None),
            });
        }
        let mut body = trimmed(&text).to_vec();
        restyle(&mut body, |s| s.italic = true);
        wrap_text(&body, self.width.saturating_sub(2))
            .into_iter()
            .map(|line| {
                let mut row = vec![Glyph {
                    c: '▎',
                    style: Style::DIM,
                    src: None,
                }];
                row.extend(spaces(1));
                row.extend(line);
                row
            })
            .collect()
    }

    /// A list: each item a marker cell and its text wrapped beside it; nested
    /// lists indented by their parent's marker.
    fn list(
        &self,
        start: Option<u64>,
        items: &[Item],
        indent: usize,
        depth: usize,
        blank_before: bool,
    ) -> Vec<Line> {
        enum Piece<'n> {
            Inline(Line),
            Block(Line, bool),
            List(Option<u64>, &'n [Item]),
        }
        let first = start.unwrap_or(1);
        let last = first + items.len().saturating_sub(1) as u64;
        let room = self.width as isize - 12;
        let mut out = Vec::new();
        for (u, item) in items.iter().enumerate() {
            let mark = marker(depth, start.map(|_| (first + u as u64, first, last)));
            let k = mark.chars().count() + 1;
            let nested = (indent + k).min(32);
            let row_indent = (indent as isize).min(room - 2).max(0) as usize;
            let cell = (k as isize).min(room - row_indent as isize).max(2) as usize;
            let mut pieces: Vec<Piece> = Vec::new();
            for (n, tok) in self.spaced(&item.children).iter().enumerate() {
                match tok {
                    Tok::Node(Node {
                        block: Block::List(s, inner),
                        ..
                    }) => pieces.push(Piece::List(*s, inner)),
                    Tok::Node(node) if is_block(node) => pieces.push(Piece::Block(
                        self.prose(tok, depth + 1, None),
                        matches!(node.block, Block::Code { .. }),
                    )),
                    _ => {
                        let text = self.prose(tok, depth + 1, task_of(item, n, tok));
                        match pieces.last_mut() {
                            Some(Piece::Inline(t)) => t.extend(text),
                            _ => pieces.push(Piece::Inline(text)),
                        }
                    }
                }
            }
            if !matches!(pieces.first(), Some(Piece::Inline(_))) {
                pieces.insert(0, Piece::Inline(Line::new()));
            }
            let mut blank = if u == 0 {
                blank_before
            } else {
                ends_loose(&items[u - 1])
            };
            for (n, piece) in pieces.into_iter().enumerate() {
                match piece {
                    Piece::Inline(text) => {
                        let lead = text.first().is_some_and(|g| g.c == '\n');
                        let gap = blank_at_end(&text);
                        let body = trimmed(&text);
                        if body.is_empty() && n > 0 {
                            blank |= text.iter().any(|g| g.c == '\n');
                            continue;
                        }
                        if blank || (n > 0 && lead) {
                            out.push(Line::new());
                        }
                        let tw = self.width.saturating_sub(row_indent + cell).max(1);
                        for (j, line) in wrap_text(body, tw).into_iter().enumerate() {
                            let mut row = spaces(row_indent);
                            if n == 0 && j == 0 {
                                let mut m = glyphs(Style::PLAIN, &mark, None);
                                m.extend(spaces(cell.saturating_sub(mark.chars().count())));
                                row.extend(m);
                            } else {
                                row.extend(spaces(cell));
                            }
                            row.extend(line);
                            out.push(row);
                        }
                        blank = gap;
                    }
                    Piece::Block(text, code) => {
                        if blank {
                            out.push(Line::new());
                        }
                        // Claude Code draws these at the list's left edge.
                        let body: &[Glyph] = if code {
                            text.strip_suffix(&[nl()]).unwrap_or(&text)
                        } else {
                            trimmed(&text)
                        };
                        out.extend(wrap_text(body, self.width));
                        blank = false;
                    }
                    Piece::List(s, inner) => {
                        out.extend(self.list(s, inner, nested, depth + 1, blank));
                        blank = false;
                    }
                }
            }
        }
        out
    }

    /// A table in box lines, its columns fitted to the width, or as
    /// "Header: value" lines when the cells would wrap more than 4 lines.
    fn table(&self, align: &[Alignment], head: &[Line], rows: &[Vec<Line>]) -> Vec<Line> {
        const MIN: usize = 3;
        let cols = head.len();
        if cols == 0 {
            return Vec::new();
        }
        // Claude Code fits tables to the terminal, 2 columns wider than its text.
        let term = self.width + 2;
        let all = || std::iter::once(head).chain(rows.iter().map(Vec::as_slice));
        let words = |c: &Line| {
            split_words(c)
                .iter()
                .map(|w| width_of(w))
                .max()
                .unwrap_or(0)
                .max(MIN)
        };
        let least: Vec<usize> = (0..cols)
            .map(|c| all().map(|r| words(&r[c])).max().unwrap_or(MIN))
            .collect();
        let ideal: Vec<usize> = (0..cols)
            .map(|c| all().map(|r| width_of(&r[c]).max(MIN)).max().unwrap_or(MIN))
            .collect();
        let room =
            (term as isize - (1 + 3 * cols as isize) - 4).max((cols * MIN) as isize) as usize;
        let (least_sum, ideal_sum) = (least.iter().sum::<usize>(), ideal.iter().sum::<usize>());
        let mut hard = false;
        let widths: Vec<usize> = if ideal_sum <= room {
            ideal.clone()
        } else if least_sum <= room {
            let extra = (room - least_sum) as f64;
            let over: Vec<usize> = (0..cols).map(|c| ideal[c] - least[c]).collect();
            let total = over.iter().sum::<usize>() as f64;
            (0..cols)
                .map(|c| {
                    if total == 0.0 {
                        least[c]
                    } else {
                        least[c] + (over[c] as f64 / total * extra).floor() as usize
                    }
                })
                .collect()
        } else {
            hard = true;
            let scale = room as f64 / least_sum as f64;
            least
                .iter()
                .map(|&w| ((w as f64 * scale).floor() as usize).max(MIN))
                .collect()
        };
        let cell = |c: &Line, w: usize| wrap_cell(c, w, hard);
        let tallest = all()
            .flat_map(|r| (0..cols).map(move |c| (r, c)))
            .map(|(r, c)| cell(&r[c], widths[c]).len())
            .max()
            .unwrap_or(1);
        if tallest > 4 {
            return self.table_vertical(head, rows, term);
        }
        let row_lines = |r: &[Line], header: bool| -> Vec<Line> {
            let wrapped: Vec<Vec<Line>> = (0..cols).map(|c| cell(&r[c], widths[c])).collect();
            let h = wrapped.iter().map(Vec::len).max().unwrap_or(1).max(1);
            (0..h)
                .map(|i| {
                    let mut line = glyphs(Style::PLAIN, "│", None);
                    for (c, w) in wrapped.iter().enumerate() {
                        let top = (h - w.len()) / 2;
                        let text = i
                            .checked_sub(top)
                            .and_then(|k| w.get(k))
                            .cloned()
                            .unwrap_or_default();
                        let a = if header {
                            Alignment::Center
                        } else {
                            align.get(c).copied().unwrap_or(Alignment::None)
                        };
                        line.extend(spaces(1));
                        line.extend(aligned(&text, widths[c], a));
                        line.extend(glyphs(Style::PLAIN, " │", None));
                    }
                    line
                })
                .collect()
        };
        let rule = |l: &str, m: &str, r: &str| {
            let mut s = l.to_string();
            for (c, w) in widths.iter().enumerate() {
                s.push_str(&"─".repeat(w + 2));
                s.push_str(if c + 1 < cols { m } else { r });
            }
            glyphs(Style::PLAIN, &s, None)
        };
        let mut out = vec![rule("┌", "┬", "┐")];
        out.extend(row_lines(head, true));
        out.push(rule("├", "┼", "┤"));
        for (k, r) in rows.iter().enumerate() {
            out.extend(row_lines(r, false));
            if k + 1 < rows.len() {
                out.push(rule("├", "┼", "┤"));
            }
        }
        out.push(rule("└", "┴", "┘"));
        if out.iter().any(|l| width_of(l) > term.saturating_sub(4)) {
            return self.table_vertical(head, rows, term);
        }
        out
    }

    /// Each row as "Header: value" lines, rows apart by a rule.
    fn table_vertical(&self, head: &[Line], rows: &[Vec<Line>], term: usize) -> Vec<Line> {
        let (labels, rows): (Vec<Line>, Vec<Vec<Line>>) = if rows.is_empty() {
            (Vec::new(), vec![head.to_vec()])
        } else {
            (
                head.iter().map(|h| trim_glyphs(h).to_vec()).collect(),
                rows.to_vec(),
            )
        };
        let rule = glyphs(
            Style::PLAIN,
            &"─".repeat(term.saturating_sub(1).min(40)),
            None,
        );
        let mut out: Vec<Line> = Vec::new();
        for row in &rows {
            let mut lines = Vec::new();
            for (c, value) in row.iter().enumerate() {
                let label = labels.get(c).cloned().unwrap_or_default();
                let value = collapse(value);
                if label.is_empty() && value.is_empty() {
                    continue;
                }
                let first_w = if label.is_empty() {
                    term as isize - 1
                } else {
                    term as isize - width_of(&label) as isize - 3
                };
                let parts = wrap_cell(&value, first_w.max(10) as usize, false);
                let mut parts = if parts.len() <= 1 {
                    parts
                } else {
                    let mut rest = Line::new();
                    for (k, p) in parts[1..].iter().enumerate() {
                        if k > 0 {
                            rest.extend(spaces(1));
                        }
                        rest.extend_from_slice(trim_glyphs(p));
                    }
                    let mut v = vec![parts[0].clone()];
                    v.extend(wrap_cell(&rest, term.saturating_sub(3), false));
                    v
                }
                .into_iter();
                let head_line = parts.next().unwrap_or_default();
                let mut line = Line::new();
                if !label.is_empty() {
                    let mut l = label;
                    l.extend(glyphs(Style::PLAIN, ":", None));
                    restyle(&mut l, |s| s.bold = true);
                    line.extend(l);
                    line.extend(spaces(1));
                }
                line.extend(head_line);
                lines.push(line);
                lines.extend(parts.filter(|p| p.iter().any(|g| !g.c.is_whitespace())));
            }
            if lines.is_empty() {
                continue;
            }
            if !out.is_empty() {
                out.push(rule.clone());
            }
            out.extend(lines);
        }
        out.iter().flat_map(|l| wrap_prose(l, self.width)).collect()
    }
}

/// The task box goes before the text of a tight item, first in it.
fn task_of(item: &Item, k: usize, tok: &Tok) -> Option<bool> {
    let tight = matches!(
        tok,
        Tok::Node(Node {
            block: Block::Para(_, false),
            ..
        })
    );
    item.task.filter(|_| k == 0 && tight)
}

/// The item ends in a heading or a table (then a blank line follows it).
fn ends_loose(item: &Item) -> bool {
    match item.children.last().map(|n| &n.block) {
        Some(Block::Heading(..) | Block::Table { .. }) => true,
        Some(Block::List(_, items)) => items.last().is_some_and(ends_loose),
        _ => false,
    }
}

/// A table inside a list or a quote: markdown pipes, columns padded.
fn table_text(align: &[Alignment], head: &[Line], rows: &[Vec<Line>]) -> Line {
    let widths: Vec<usize> = (0..head.len())
        .map(|c| {
            std::iter::once(&head[c])
                .chain(rows.iter().map(|r| &r[c]))
                .map(|x| width_of(x))
                .max()
                .unwrap_or(0)
                .max(3)
        })
        .collect();
    let row = |r: &[Line]| {
        let mut line = glyphs(Style::PLAIN, "| ", None);
        for (c, w) in widths.iter().enumerate() {
            line.extend(aligned(
                &r[c],
                *w,
                align.get(c).copied().unwrap_or(Alignment::None),
            ));
            line.extend(glyphs(Style::PLAIN, " | ", None));
        }
        while line.last().is_some_and(|g| g.c == ' ') {
            line.pop();
        }
        line.push(nl());
        line
    };
    let mut out = row(head);
    let mut rule = "|".to_string();
    for w in &widths {
        rule.push_str(&"-".repeat(w + 2));
        rule.push('|');
    }
    out.extend(glyphs(Style::PLAIN, &rule, None));
    out.push(nl());
    for r in rows {
        out.extend(row(r));
    }
    out.push(nl());
    out
}

fn trim_glyphs(text: &[Glyph]) -> &[Glyph] {
    let a = text.iter().take_while(|g| g.c.is_whitespace()).count();
    let b = text.len()
        - text[a..]
            .iter()
            .rev()
            .take_while(|g| g.c.is_whitespace())
            .count();
    &text[a..b.max(a)]
}

/// Runs of white space as one space, none at the ends.
fn collapse(text: &[Glyph]) -> Line {
    let mut out = Line::new();
    for g in trim_glyphs(text) {
        if g.c.is_whitespace() {
            if out.last().is_some_and(|p: &Glyph| p.c != ' ') {
                out.push(Glyph { c: ' ', ..*g });
            }
        } else {
            out.push(*g);
        }
    }
    out
}

fn split_words(text: &[Glyph]) -> Vec<&[Glyph]> {
    text.split(|g| g.c.is_whitespace())
        .filter(|w| !w.is_empty())
        .collect()
}

/// Lines of prose, each wrapped to `width`.
fn wrap_text(text: &[Glyph], width: usize) -> Vec<Line> {
    split(text)
        .iter()
        .flat_map(|l| wrap_prose(l, width))
        .collect()
}

/// A line wrapped the way Claude Code's terminal renderer (Ink) does:
/// `wrap-ansi` with `hard: true, trim: false`, and no white space starting
/// a continuation row (or ending a row it broke). So a line keeps its indent, and a word longer than a
/// row breaks where that takes fewer rows.
fn wrap_prose(line: &[Glyph], width: usize) -> Vec<Line> {
    let width = width.max(1);
    if width_of(line) <= width {
        return vec![line.to_vec()];
    }
    let mut rows = wrap_ansi(line, width, true);
    let last = rows.len() - 1;
    for (k, r) in rows.iter_mut().enumerate() {
        if k > 0 {
            let lead = r.iter().take_while(|g| g.c == ' ').count();
            r.drain(..lead);
        }
        // The space at a break shows as nothing: no need to keep it.
        while k < last && r.last().is_some_and(|g| g.c == ' ') {
            r.pop();
        }
    }
    rows
}

/// A table cell's rows (`wrap-ansi` with `trim: false`, no empty rows).
fn wrap_cell(text: &[Glyph], width: usize, hard: bool) -> Vec<Line> {
    let text = {
        let b = text.len()
            - text
                .iter()
                .rev()
                .take_while(|g| g.c.is_whitespace())
                .count();
        &text[..b]
    };
    if width == 0 {
        return vec![text.to_vec()];
    }
    let rows: Vec<Line> = split(text)
        .iter()
        .flat_map(|l| wrap_ansi(l, width, hard))
        .filter(|r| !r.is_empty())
        .collect();
    if rows.is_empty() {
        vec![Line::new()]
    } else {
        rows
    }
}

/// `wrap-ansi`'s word wrap of one line (`wordWrap: true, trim: false`).
fn wrap_ansi(line: &[Glyph], cols: usize, hard: bool) -> Vec<Line> {
    let mut space = Glyph {
        c: ' ',
        style: Style::PLAIN,
        src: None,
    };
    let mut rows: Vec<Line> = vec![Line::new()];
    // Each word with the space before it (styled as the text around it).
    let mut words: Vec<(Glyph, &[Glyph])> = Vec::new();
    let mut from = 0;
    for (k, g) in line.iter().enumerate() {
        if g.c == ' ' {
            words.push((space, &line[from..k]));
            space = *g;
            from = k + 1;
        }
    }
    words.push((space, &line[from..]));
    for (index, (space, word)) in words.into_iter().enumerate() {
        let mut len = width_of(rows.last().unwrap());
        if index != 0 {
            if len >= cols {
                rows.push(Line::new());
                len = 0;
            }
            rows.last_mut().unwrap().push(Glyph { src: None, ..space });
            len += 1;
        }
        let wl = width_of(word);
        if hard && wl > cols {
            let remaining = cols as isize - len as isize;
            let this_line = 1 + (wl as isize - remaining - 1).div_euclid(cols as isize);
            let next_line = ((wl - 1) / cols) as isize;
            if next_line < this_line {
                rows.push(Line::new());
            }
            wrap_word(&mut rows, word, cols);
            continue;
        }
        if len + wl > cols && len > 0 && wl > 0 {
            rows.push(Line::new());
        }
        rows.last_mut().unwrap().extend_from_slice(word);
    }
    rows
}

fn wrap_word(rows: &mut Vec<Line>, word: &[Glyph], cols: usize) {
    let mut visible = width_of(rows.last().unwrap());
    for (i, g) in word.iter().enumerate() {
        let w = g.c.width().unwrap_or(0);
        if visible + w <= cols {
            rows.last_mut().unwrap().push(*g);
        } else {
            rows.push(vec![*g]);
            visible = 0;
        }
        visible += w;
        if visible == cols && i + 1 < word.len() {
            rows.push(Line::new());
            visible = 0;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(lines: &[Line]) -> Vec<String> {
        lines
            .iter()
            .map(|l| {
                l.iter()
                    .map(|g| g.c)
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect()
    }

    fn show(md: &str, width: usize) -> Vec<String> {
        text(&lines(md, width))
    }

    /// Each answer of `claude_markdown_cases.md` as Claude Code 2.1.307 drew
    /// it (`claude --resume` of a saved chat, hyperlinks off, 60 and 40
    /// columns): the same rows, chars and styles, cell for cell. Spaces only
    /// need the same underline and strike; Claude's code colour is
    /// rgb(177,185,249).
    #[test]
    fn answers_look_as_claude_code_draws_them() {
        use alacritty_terminal::term::cell::Flags;
        use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};
        let cases: Vec<&str> = include_str!("../tests/fixtures/claude_markdown_cases.md")
            .trim_end()
            .split("\n=====\n")
            .collect();
        let shots: [(&[u8], u16, u16); 2] = [
            (
                include_bytes!("../tests/fixtures/claude_markdown_60x260.bin"),
                60,
                260,
            ),
            (
                include_bytes!("../tests/fixtures/claude_markdown_40x400.bin"),
                40,
                400,
            ),
        ];
        let mut bad = Vec::new();
        for (bytes, cols, rows) in shots {
            let mut term = crate::shadow::Shadow::new(cols, rows);
            term.advance(bytes);
            term.flush_sync();
            let snap = term.snapshot();
            let row_text = |r: usize| snap.rows_text(r, r + 1).trim_end().to_string();
            for (n, md) in cases.iter().enumerate() {
                let prompt = (0..snap.rows.len())
                    .find(|&r| row_text(r) == format!("❯ case {n}"))
                    .unwrap_or_else(|| panic!("case {n} not on the {cols}-column screen"));
                let top = prompt + 2;
                assert!(row_text(top).starts_with('●'));
                let ours = lines(md, cols as usize - 2);
                for (k, line) in ours.iter().enumerate() {
                    let r = top + k;
                    let mut col = 2;
                    let mut cells = Vec::new();
                    for g in line {
                        let cell = &snap.rows[r][col];
                        let blank = g.c == ' ';
                        let fg = match cell.fg {
                            Color::Named(NamedColor::Foreground) => Fg::Plain,
                            Color::Spec(Rgb {
                                r: 177,
                                g: 185,
                                b: 249,
                            }) => Fg::Code,
                            _ => Fg::Error,
                        };
                        let s = g.style;
                        let theirs = (
                            cell.c,
                            cell.flags.intersects(Flags::ALL_UNDERLINES),
                            cell.flags.contains(Flags::STRIKEOUT),
                            !blank && cell.flags.contains(Flags::BOLD),
                            !blank && cell.flags.contains(Flags::ITALIC),
                            !blank && cell.flags.contains(Flags::DIM),
                            if blank { Fg::Plain } else { fg },
                        );
                        let mine = (
                            g.c,
                            s.underline,
                            s.strike,
                            !blank && s.bold,
                            !blank && s.italic,
                            !blank && s.dim,
                            if blank { Fg::Plain } else { s.fg },
                        );
                        if theirs != mine {
                            cells.push(format!("col {col}: claude {theirs:?}, peekme {mine:?}"));
                        }
                        col += g.c.width().unwrap_or(1).max(1);
                    }
                    let rest: String = snap.rows[r][col..].iter().map(|c| c.c).collect();
                    if !rest.trim().is_empty() {
                        cells.push(format!("claude has more: {rest:?}"));
                    }
                    if !cells.is_empty() {
                        bad.push(format!(
                            "{cols} cols, case {n}, line {k} {:?}: {}",
                            row_text(r),
                            cells[..cells.len().min(3)].join("; ")
                        ));
                    }
                }
                // Then the blank row before the next prompt (or the end).
                let after = top + ours.len();
                if !row_text(after).is_empty()
                    || (n + 1 < cases.len() && row_text(after + 1) != format!("❯ case {}", n + 1))
                {
                    bad.push(format!(
                        "{cols} cols, case {n}: {} lines here, Claude goes on with {:?}",
                        ours.len(),
                        row_text(after)
                    ));
                }
            }
        }
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }

    #[test]
    fn tables_get_box_lines() {
        let md = "| Left | Center | Right |\n|:-----|:------:|------:|\n| `a` | **b** | 1 |\n| longer cell text | c | 22 |";
        assert_eq!(
            show(md, 98),
            [
                "┌──────────────────┬────────┬───────┐",
                "│       Left       │ Center │ Right │",
                "├──────────────────┼────────┼───────┤",
                "│ a                │   b    │     1 │",
                "├──────────────────┼────────┼───────┤",
                "│ longer cell text │   c    │    22 │",
                "└──────────────────┴────────┴───────┘",
            ]
        );
        // Cells of styled text keep their style.
        let l = lines(md, 98);
        assert_eq!(l[3][2].style, Style::CODE);
        assert!(l[3][23].style.bold);
    }

    #[test]
    fn a_narrow_table_turns_into_lines() {
        let md = "| A | B | C | D | E |\n|---|---|---|---|---|\n| alpha beta gamma | delta epsilon zeta | eta theta iota | kappa lambda mu | nu xi omicron pi |";
        assert_eq!(
            show(md, 34),
            [
                "A: alpha beta gamma",
                "B: delta epsilon zeta",
                "C: eta theta iota",
                "D: kappa lambda mu",
                "E: nu xi omicron pi",
            ]
        );
    }

    #[test]
    fn lists_and_spacing_follow_claude_code() {
        let md = "Intro paragraph right before a list:\n- item a that is long enough to wrap around the narrow terminal width for sure yes\n- item b\nText right after the list.\n## Heading glued to text\nBody under heading.\n> quote that is long enough to wrap around the narrow terminal width for sure yes indeed\n> - list in quote\n\n1. one\n2. two\n   - sub bullet\n     1. deep numbered\n     2. deep numbered two\n10. ten";
        assert_eq!(
            show(md, 58),
            [
                "Intro paragraph right before a list:",
                "- item a that is long enough to wrap around the narrow",
                "  terminal width for sure yes",
                "- item b",
                "  Text right after the list.",
                "Heading glued to text",
                "",
                "Body under heading.",
                "",
                "▎ quote that is long enough to wrap around the narrow",
                "▎ terminal width for sure yes indeed",
                "▎ - list in quote",
                "",
                "1. one",
                "2. two",
                "   - sub bullet",
                "     i. deep numbered",
                "     ii. deep numbered two",
                "3. ten",
            ]
        );
    }

    #[test]
    fn long_words_break_where_it_takes_fewer_rows() {
        // As Claude Code 2.1.307 drew it at 60 and 36 columns.
        let md = "Word12345entity and a very-long-unbreakable-word-that-is-longer-than-the-width-of-the-terminal-for-sure.";
        assert_eq!(
            show(md, 58),
            [
                "Word12345entity and a very-long-unbreakable-word-that-is-l",
                "onger-than-the-width-of-the-terminal-for-sure.",
            ]
        );
        assert_eq!(
            show(md, 34),
            [
                "Word12345entity and a",
                "very-long-unbreakable-word-that-is",
                "-longer-than-the-width-of-the-term",
                "inal-for-sure.",
            ]
        );
    }

    #[test]
    fn inline_styles() {
        let md = "**b** *i* ***bi*** ~~s~~ `c` [t](https://e.com) <https://a.com> ~5 s";
        let l = &lines(md, 80)[0];
        let s: String = l.iter().map(|g| g.c).collect();
        assert_eq!(s, "b i bi s c t (https://e.com) https://a.com ~5 s");
        let at = |c: char| l.iter().find(|g| g.c == c).unwrap().style;
        assert!(at('b').bold && !at('b').italic);
        assert!(at('i').italic && !at('i').bold);
        assert!(at('s').strike);
        assert_eq!(at('c'), Style::CODE);
    }

    #[test]
    fn source_positions() {
        let md = "A **bold** \\*x and `code`";
        let l = &lines(md, 80)[0];
        let chars: Vec<char> = md.chars().collect();
        for g in l {
            if let Some(i) = g.src {
                assert_eq!(chars[i], g.c, "{l:?}");
            }
        }
        assert_eq!(l.iter().find(|g| g.c == '*').unwrap().src, Some(12));
    }
}
