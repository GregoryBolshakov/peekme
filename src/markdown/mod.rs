//! An answer's markdown as box lines, laid out the way the agent of the chat
//! lays out its own answers: the same blank lines, list markers and indents,
//! tables, quotes and places to wrap, in the agent's colours. Each agent's
//! rules are in its own module, taken from its renderer and checked against
//! what it draws (`claude`: Claude Code, `codex`: Codex CLI, `copilot`:
//! GitHub Copilot CLI, `kiro`: Kiro CLI).

use std::ops::Range;
use std::sync::OnceLock;

use pulldown_cmark::{Alignment, CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use unicode_width::UnicodeWidthChar;

mod claude;
mod codex;
mod copilot;
mod kiro;

/// What a glyph's colour stands for; the look of the agent says which colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum Fg {
    #[default]
    Plain,
    Code,
    Error,
    /// A link's text or address.
    Link,
    /// A quote's text or bar.
    Quote,
    /// An ordered list's number.
    Number,
    /// A table's header row.
    Header,
    /// Table borders and rules.
    Border,
    Image,
    /// A code block's text.
    Block,
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
        if let Some(c) = look().color(self.fg) {
            s.push(';');
            s.push_str(c);
        }
        s.push('m');
        s
    }
}

/// Whose rules lay the answer out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Flavor {
    #[default]
    Claude,
    Codex,
    Copilot,
    Kiro,
}

/// How the agent draws: its rules, the SGR parameters of each colour it
/// uses, and whether the terminal shows struck-through text (else "~~" stays
/// around it, as Claude Code does).
pub struct Look {
    pub flavor: Flavor,
    pub colors: Vec<(Fg, String)>,
    pub strike: bool,
}

impl Look {
    fn color(&self, fg: Fg) -> Option<&str> {
        match fg {
            Fg::Plain => None,
            Fg::Error => Some("31"),
            _ => self
                .colors
                .iter()
                .find(|(f, _)| *f == fg)
                .map(|(_, c)| c.as_str()),
        }
    }
}

impl Default for Look {
    fn default() -> Self {
        Look {
            flavor: Flavor::Claude,
            colors: vec![(Fg::Code, "36".into())],
            strike: true,
        }
    }
}

/// The look of `agent`'s answers in this terminal (none: no agent).
pub fn look_for(agent: Option<crate::agent::Agent>) -> Look {
    use crate::agent::Agent;
    let truecolor = crate::color::truecolor();
    let strike = strike_shown(|k| std::env::var(k).ok());
    match agent {
        Some(Agent::Claude) => Look {
            flavor: Flavor::Claude,
            colors: claude::colors(truecolor),
            strike,
        },
        Some(Agent::Codex) => Look {
            flavor: Flavor::Codex,
            colors: codex::colors(truecolor),
            strike,
        },
        Some(Agent::Kiro) => Look {
            flavor: Flavor::Kiro,
            colors: kiro::colors(),
            strike,
        },
        Some(Agent::Copilot) => Look {
            flavor: Flavor::Copilot,
            colors: copilot::colors(),
            strike,
        },
        _ => Look {
            strike,
            ..Look::default()
        },
    }
}

static LOOK: OnceLock<Look> = OnceLock::new();

/// Draw this run's answers with `look`.
pub fn set_look(look: Look) {
    let _ = LOOK.set(look);
}

fn look() -> &'static Look {
    LOOK.get_or_init(Look::default)
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

/// The lines of `text` (markdown), `width` columns wide, as this run's
/// agent lays it out.
pub fn lines(text: &str, width: usize) -> Vec<Line> {
    lines_as(look().flavor, text, width)
}

fn lines_as(flavor: Flavor, text: &str, width: usize) -> Vec<Line> {
    let mut lines = match flavor {
        Flavor::Copilot => copilot::lines(text, width),
        Flavor::Kiro => kiro::lines(text, width),
        Flavor::Codex => codex::lines(text, width),
        Flavor::Claude => claude::lines(text, width),
    };
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
    /// Whose inline rules: links, images, struck text and code differ.
    flavor: Flavor,
    /// Copilot only: a link's address after its text, as the box shows no
    /// hyperlinks (Copilot itself shows the text alone, as a hyperlink).
    urls: bool,
    text: &'a str,
    /// The char index of each byte offset.
    at: Vec<usize>,
    ev: Vec<(Event<'a>, Range<usize>)>,
    i: usize,
    task: Option<bool>,
}

fn parse(flavor: Flavor, text: &str) -> Vec<Node> {
    parse_with(flavor, text, true)
}

fn parse_with(flavor: Flavor, text: &str, urls: bool) -> Vec<Node> {
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
        flavor,
        urls,
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
    /// Kiro's "~sub~", from this glyph on.
    Sub(usize),
}

/// A soft line break in Kiro's text (it shows as a space).
const SOFT: char = '\u{2028}';

/// `text` in subscript digits and signs, when it has only those.
fn subscript(text: &str) -> Option<String> {
    text.chars()
        .map(|c| match c {
            '0'..='9' => char::from_u32(0x2080 + (c as u32 - '0' as u32)),
            '+' => Some('\u{208a}'),
            '-' => Some('\u{208b}'),
            '=' => Some('\u{208c}'),
            '(' => Some('\u{208d}'),
            ')' => Some('\u{208e}'),
            _ => None,
        })
        .collect()
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
                Event::Code(t) => {
                    let code = Style {
                        fg: Fg::Code,
                        ..style
                    };
                    // Copilot pads inline code with a no-break space each side.
                    let pad = self.flavor == Flavor::Copilot;
                    if pad {
                        out.extend(glyphs(code, "\u{a0}", None));
                    }
                    self.push_text(&mut out, &t, r.clone(), code);
                    if pad {
                        out.extend(glyphs(code, "\u{a0}", None));
                    }
                }
                // Kiro turns a soft break into a space (but in quotes): it is
                // marked to tell it from a hard one.
                Event::SoftBreak if self.flavor == Flavor::Kiro => out.push(Glyph {
                    c: SOFT,
                    style: Style::PLAIN,
                    src: None,
                }),
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
                        Tag::Strikethrough if self.flavor != Flavor::Claude || look().strike => (
                            Style {
                                strike: true,
                                ..style
                            },
                            Open::Style,
                        ),
                        // Kiro writes "~2~" as a subscript, else as it is.
                        Tag::Subscript if self.flavor == Flavor::Kiro => {
                            (style, Open::Sub(out.len()))
                        }
                        // Claude Code strikes "~~" only; the others "~" too.
                        Tag::Subscript if self.flavor != Flavor::Claude => (
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
                        Some(Open::Sub(from)) => {
                            let text: String = out[from..].iter().map(|g| g.c).collect();
                            match subscript(&text) {
                                Some(sub) => {
                                    for (g, c) in out[from..].iter_mut().zip(sub.chars()) {
                                        g.c = c;
                                    }
                                }
                                None => {
                                    out.insert(
                                        from,
                                        Glyph {
                                            c: '~',
                                            style,
                                            src: None,
                                        },
                                    );
                                    out.push(Glyph {
                                        c: '~',
                                        style,
                                        src: None,
                                    });
                                }
                            }
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
    /// "text (url)", or the url alone when it is the text. Copilot shows the
    /// text alone, underlined.
    fn end_link(&self, out: &mut Line, from: usize, url: &str, r: Range<usize>) {
        if self.flavor == Flavor::Kiro {
            // The text in the link colour, then " (url)" unless it is the url.
            let text: String = out[from..].iter().map(|g| g.c).collect();
            if from == out.len() {
                out.extend(self.literal(url, r.clone(), Style::PLAIN));
            }
            restyle(&mut out[from..], |s| s.fg = Fg::Link);
            if !text.is_empty() && text != url {
                out.extend(glyphs(Style::PLAIN, " (", None));
                out.extend(self.literal(url, r, Style::PLAIN));
                out.extend(glyphs(Style::PLAIN, ")", None));
            }
            return;
        }
        if self.flavor == Flavor::Copilot {
            let text: String = out[from..].iter().map(|g| g.c).collect();
            if from == out.len() {
                out.extend(self.literal(url, r.clone(), Style::PLAIN));
            }
            restyle(&mut out[from..], |s| {
                s.underline = true;
                s.fg = Fg::Link;
            });
            if self.urls && !text.is_empty() && text != url {
                out.extend(glyphs(Style::PLAIN, " (", None));
                out.extend(self.literal(url, r, Style::PLAIN));
                out.extend(glyphs(Style::PLAIN, ")", None));
            }
            return;
        }
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
        if self.flavor == Flavor::Kiro {
            let alt: String = out[from..].iter().map(|g| g.c).collect();
            out.truncate(from);
            let alt = alt.trim();
            let shown = if alt.is_empty() {
                "[image]".to_string()
            } else {
                format!("[image: {alt}]")
            };
            out.extend(glyphs(Style::PLAIN, &shown, None));
            return;
        }
        if self.flavor == Flavor::Copilot {
            // "Image: alt → url", or "Image: url".
            let alt: String = out[from..].iter().map(|g| g.c).collect();
            out.truncate(from);
            let style = Style {
                fg: Fg::Image,
                ..Style::PLAIN
            };
            let shown = if alt.is_empty() {
                format!("Image: {url}")
            } else {
                format!("Image: {alt} \u{2192} {url}")
            };
            out.extend(glyphs(style, &shown, None));
            return;
        }
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
pub(crate) mod tests {
    use super::*;
    use alacritty_terminal::term::cell::Flags;
    use alacritty_terminal::vte::ansi::Color;

    pub(crate) fn show(flavor: Flavor, md: &str, width: usize) -> Vec<String> {
        lines_as(flavor, md, width)
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

    /// An agent's screen with the answers of `claude_markdown_cases.md`.
    pub(crate) struct Shot {
        pub bytes: &'static [u8],
        pub cols: u16,
        pub rows: u16,
        /// The answers went up into the terminal's history (an inline agent).
        pub history: bool,
    }

    /// Each answer as the agent drew it in `shots` (`case N` prompts found by
    /// `prompt`, the answer `indent` columns in, its text `width(cols)`
    /// wide): the same rows, chars and styles, cell for cell, and nothing
    /// more but blank rows or a rule before the next prompt. Spaces only need
    /// the same underline and strike; `colour` says what each colour of the
    /// agent stands for.
    pub(crate) fn oracle(
        render: impl Fn(&str, usize) -> Vec<Line>,
        shots: &[Shot],
        prompt: &str,
        indent: usize,
        width: impl Fn(usize) -> usize,
        colour: impl Fn(&Color) -> Option<Fg>,
        alias: impl Fn(Fg) -> Fg,
    ) {
        let cases: Vec<&str> = include_str!("../../tests/fixtures/claude_markdown_cases.md")
            .trim_end()
            .split("\n=====\n")
            .collect();
        let mut bad = Vec::new();
        let mut found = std::collections::BTreeSet::new();
        for shot in shots {
            let cols = shot.cols as usize;
            let mut term = crate::shadow::Shadow::new(shot.cols, shot.rows);
            term.advance(shot.bytes);
            term.flush_sync();
            let snap = if shot.history {
                term.snapshot_with_history(100_000)
            } else {
                term.snapshot()
            };
            let rows = &snap.rows;
            let text = |r: usize| -> String {
                rows.get(r)
                    .map(|row| row.iter().map(|c| c.c).collect::<String>())
                    .unwrap_or_default()
                    .trim_end()
                    .to_string()
            };
            let at = |n: usize| {
                let want = format!("{prompt}{n}");
                (0..rows.len()).find(|&r| {
                    let t = text(r);
                    t.trim_start()
                        .strip_prefix(&want)
                        .is_some_and(|rest| !rest.starts_with(|c: char| c.is_ascii_digit()))
                })
            };
            for (n, md) in cases.iter().enumerate() {
                let Some(p) = at(n) else { continue };
                found.insert((cols, n));
                let top = (p + 1..rows.len())
                    .find(|&r| !text(r).is_empty())
                    .unwrap_or(p + 1);
                let mut ours = render(md, width(cols));
                if ours.is_empty() {
                    ours.push(Line::new());
                }
                for (k, line) in ours.iter().enumerate() {
                    let r = top + k;
                    let Some(row) = rows.get(r) else { break };
                    let mut col = indent;
                    let mut cells = Vec::new();
                    for g in line {
                        let Some(cell) = row.get(col) else {
                            cells.push(format!("col {col}: past the row"));
                            break;
                        };
                        let blank = g.c == ' ' || g.c == '\u{a0}';
                        let s = g.style;
                        let theirs = (
                            cell.c,
                            cell.flags.intersects(Flags::ALL_UNDERLINES),
                            cell.flags.contains(Flags::STRIKEOUT),
                            !blank && cell.flags.contains(Flags::BOLD),
                            !blank && cell.flags.contains(Flags::ITALIC),
                            !blank && cell.flags.contains(Flags::DIM),
                            if blank {
                                Some(Fg::Plain)
                            } else {
                                colour(&cell.fg)
                            },
                        );
                        let mine = (
                            g.c,
                            s.underline,
                            s.strike,
                            !blank && s.bold,
                            !blank && s.italic,
                            !blank && s.dim,
                            Some(if blank { Fg::Plain } else { alias(s.fg) }),
                        );
                        if theirs != mine {
                            cells.push(format!("col {col}: theirs {theirs:?}, ours {mine:?}"));
                        }
                        col += g.c.width().unwrap_or(1).max(1);
                    }
                    let rest: String = row.iter().skip(col).map(|c| c.c).collect();
                    if !rest.trim().is_empty() {
                        cells.push(format!("theirs goes on: {rest:?}"));
                    }
                    if !cells.is_empty() {
                        bad.push(format!(
                            "{cols} cols, case {n}, line {k} {:?}: {}",
                            text(r),
                            cells[..cells.len().min(3)].join("; ")
                        ));
                    }
                }
                // Then only blank rows or a rule up to the next prompt.
                let end = at(n + 1).unwrap_or(top + ours.len() + 1);
                for r in top + ours.len()..end {
                    let t = text(r);
                    if !(t.is_empty() || t.chars().all(|c| c == '─')) {
                        bad.push(format!(
                            "{cols} cols, case {n}: {} lines here, theirs goes on with {t:?}",
                            ours.len()
                        ));
                        break;
                    }
                }
            }
        }
        for shot in shots {
            for n in 0..cases.len() {
                if !found.contains(&(shot.cols as usize, n)) {
                    bad.push(format!("{} cols: case {n} on no screen", shot.cols));
                }
            }
        }
        bad.dedup();
        assert!(bad.is_empty(), "{}", bad.join("\n"));
    }
}
