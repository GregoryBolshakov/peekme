//! Claude Code's layout (2.1.307): its renderer over `marked` (`h0`, `io`,
//! `Ms`, `$r`, `Wt`, `yo` in its bundle): the same blank lines, list markers
//! and indents, tables drawn with box lines (or as "Header: value" lines when
//! they do not fit), quotes behind a bar, and the same places to wrap. The
//! parse here is pulldown-cmark, and the few places the two parsers differ
//! are noted where they matter.

use pulldown_cmark::Alignment;

use super::*;

/// Claude Code's colours: inline code in its theme's "permission" colour.
pub(super) fn colors(truecolor: bool) -> Vec<(Fg, String)> {
    let theme = crate::claude::theme::theme();
    let code = crate::claude::theme::code_color(theme.as_deref());
    vec![(Fg::Code, crate::color::sgr(code, truecolor))]
}

/// The lines of `text` as Claude Code lays it out `width` columns wide.
pub(super) fn lines(text: &str, width: usize) -> Vec<Line> {
    let nodes = parse(Flavor::Claude, text);
    Lay { text, width }.top(&nodes)
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

fn is_block(n: &Node) -> bool {
    matches!(
        n.block,
        Block::Code { .. } | Block::Quote(_) | Block::Rule | Block::Table { .. } | Block::List(..)
    )
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::tests::{Shot, oracle};
    use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

    fn lines(md: &str, width: usize) -> Vec<Line> {
        lines_as(Flavor::Claude, md, width)
    }

    fn show(md: &str, width: usize) -> Vec<String> {
        crate::markdown::tests::show(Flavor::Claude, md, width)
    }

    /// Each answer of `claude_markdown_cases.md` as Claude Code 2.1.307 drew
    /// it (`claude --resume` of a saved chat, hyperlinks off, 60 and 40
    /// columns). Its code colour is rgb(177,185,249).
    #[test]
    fn answers_look_as_claude_code_draws_them() {
        let colour = |c: &Color| match c {
            Color::Named(NamedColor::Foreground) => Some(Fg::Plain),
            Color::Spec(Rgb {
                r: 177,
                g: 185,
                b: 249,
            }) => Some(Fg::Code),
            _ => None,
        };
        let shots = [
            Shot {
                bytes: include_bytes!("../../tests/fixtures/claude_markdown_60x260.bin"),
                cols: 60,
                rows: 260,
                history: false,
            },
            Shot {
                bytes: include_bytes!("../../tests/fixtures/claude_markdown_40x400.bin"),
                cols: 40,
                rows: 400,
                history: false,
            },
        ];
        oracle(
            |md, w| lines_as(Flavor::Claude, md, w),
            &shots,
            "❯ case ",
            2,
            |cols| cols - 2,
            colour,
            |fg| fg,
        );
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
