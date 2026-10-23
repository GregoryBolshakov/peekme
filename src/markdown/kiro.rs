//! Kiro CLI's layout (2.28.0): its renderer in `tui.js` turns `marked`'s
//! tokens into flat segments (`q_`, `Q4`, `Jyt`, `Zyt`), groups them into
//! blocks (`ES`), and draws each block (`L4`, `O4`): a blank row between
//! blocks unless the source had none before a list or inside a quote, list
//! items as "- " and "1. " two columns deeper per level, quotes as a dim
//! "│ " before each line, tables in dim box lines or, when narrow, as
//! "Header: value" lines. Lines are wrapped after all that, so a wrapped row
//! starts at the left edge (`twinki`'s `ann` and `onn`).

use pulldown_cmark::Alignment;

use super::*;

/// Kiro's colours: inline code and links (its "highlight" and "link").
pub(super) fn colors() -> Vec<(Fg, String)> {
    vec![(Fg::Code, "94".into()), (Fg::Link, "34".into())]
}

/// The lines of `text` as Kiro lays it out `width` columns wide.
pub(super) fn lines(text: &str, width: usize) -> Vec<Line> {
    let nodes = parse(Flavor::Kiro, text);
    let lay = Lay { text };
    let segs = lay.seq(&nodes, Ctx::default());
    draw(&group(segs), width)
}

/// Where a segment is.
#[derive(Clone, Copy, Default)]
struct Ctx {
    quote: usize,
    list_depth: usize,
    list_indent: usize,
    compact: bool,
    loose_before: bool,
}

#[derive(Clone)]
enum Kind {
    /// Inline text; `plain` when it is a top-level paragraph (it then runs
    /// into the text around it).
    Text {
        plain: bool,
    },
    /// A heading (0: a paragraph of bold text alone).
    Header(u8),
    Code,
    Item {
        marker: String,
        indent: usize,
        loose: bool,
    },
    Quote,
    Rule,
    Table {
        align: Vec<Alignment>,
        head: Vec<Line>,
        rows: Vec<Vec<Line>>,
    },
}

#[derive(Clone)]
struct Seg {
    kind: Kind,
    text: Line,
    /// Quote bars before each line.
    quote: usize,
    list_indent: usize,
    compact: bool,
}

struct Lay<'a> {
    text: &'a str,
}

impl Lay<'_> {
    /// Blank lines in the source between two nodes.
    fn gap(&self, a: &Node, b: &Node) -> usize {
        let end = a.range.start
            + self
                .text
                .get(a.range.clone())
                .map_or(0, |s| s.trim_end().len());
        self.text
            .get(end..b.range.start)
            .map_or(0, |g| g.matches('\n').count())
    }

    /// `Q4`: blocks one after the other.
    fn seq(&self, nodes: &[Node], ctx: Ctx) -> Vec<Seg> {
        let mut out: Vec<Seg> = Vec::new();
        for (k, n) in nodes.iter().enumerate() {
            let newlines = k.checked_sub(1).map_or(0, |p| self.gap(&nodes[p], n));
            let blank = newlines >= 2;
            let is_list = matches!(n.block, Block::List(..));
            let is_quote = matches!(n.block, Block::Quote(_));
            let c = Ctx {
                loose_before: blank && is_list,
                compact: if out.is_empty() {
                    ctx.compact
                } else {
                    !blank && (ctx.quote > 0 || ctx.list_indent > 0 || is_quote || is_list)
                },
                ..ctx
            };
            let mut segs = self.node(n, c);
            if blank
                && ctx.quote > 0
                && out.last().is_some_and(|s| matches!(s.kind, Kind::Quote))
                && segs.first().is_some_and(|s| matches!(s.kind, Kind::Quote))
            {
                let first = &mut segs[0];
                first.text.insert(0, nl());
                first.compact = true;
            }
            if blank
                && !out.is_empty()
                && ctx.quote == 0
                && ctx.list_depth == 0
                && ctx.list_indent == 0
                && out.last().is_some_and(plain)
                && segs.first().is_some_and(plain)
            {
                out.push(Seg {
                    kind: Kind::Text { plain: true },
                    text: vec![nl(); newlines],
                    quote: 0,
                    list_indent: 0,
                    compact: false,
                });
            }
            out.extend(segs);
        }
        out
    }

    /// `Jyt`: one block's segments.
    fn node(&self, n: &Node, ctx: Ctx) -> Vec<Seg> {
        let seg = |kind: Kind, text: Line| Seg {
            kind,
            text,
            quote: ctx.quote,
            list_indent: ctx.list_indent,
            compact: ctx.compact,
        };
        match &n.block {
            Block::Para(line, _) => {
                if ctx.quote > 0 {
                    // Inside a quote a soft break stays a line break.
                    let text = line
                        .iter()
                        .map(|g| if g.c == SOFT { nl() } else { *g })
                        .collect();
                    return vec![seg(Kind::Quote, text)];
                }
                let text = soft_spaces(line);
                if ctx.list_indent > 0 {
                    return vec![seg(Kind::Text { plain: false }, text)];
                }
                if self.bold_alone(n, line) {
                    return vec![seg(Kind::Header(0), text)];
                }
                vec![Seg {
                    compact: false,
                    ..seg(Kind::Text { plain: true }, text)
                }]
            }
            Block::Heading(level, line) => vec![seg(Kind::Header(*level), soft_spaces(line))],
            // Its `marked` keeps an indented block's last line break.
            Block::Code { text, indented, .. } => {
                let mut text = text.clone();
                if *indented {
                    text.push(nl());
                }
                vec![seg(Kind::Code, text)]
            }
            Block::Rule => vec![seg(Kind::Rule, Line::new())],
            Block::Html(text) => vec![seg(Kind::Text { plain: false }, text.clone())],
            Block::Quote(children) => {
                let inner = Ctx {
                    quote: ctx.quote + 1,
                    ..ctx
                };
                let segs = self.seq(children, inner);
                if segs.is_empty() {
                    return vec![Seg {
                        quote: ctx.quote + 1,
                        ..seg(Kind::Quote, Line::new())
                    }];
                }
                segs
            }
            Block::List(start, items) => self.list(*start, items, ctx),
            Block::Table { align, head, rows } => vec![seg(
                Kind::Table {
                    align: align.clone(),
                    head: head.iter().map(|c| soft_spaces(c)).collect(),
                    rows: rows
                        .iter()
                        .map(|r| r.iter().map(|c| soft_spaces(c)).collect())
                        .collect(),
                },
                Line::new(),
            )],
        }
    }

    /// A top-level paragraph that is bold text alone shows as a heading.
    fn bold_alone(&self, n: &Node, line: &[Glyph]) -> bool {
        let src = self.text.get(n.range.clone()).unwrap_or("").trim();
        src.starts_with("**")
            && src.ends_with("**")
            && line.iter().all(|g| g.style.bold || g.c.is_whitespace())
    }

    /// `Zyt`: each item a segment for its first text, then its other blocks
    /// (nested lists a level deeper, others indented under the item).
    fn list(&self, start: Option<u64>, items: &[Item], ctx: Ctx) -> Vec<Seg> {
        let mut out = Vec::new();
        let mut number = start.unwrap_or(1);
        for (k, item) in items.iter().enumerate() {
            if k > 0 {
                number += 1;
            }
            let loose = if k == 0 {
                ctx.loose_before
            } else {
                match (items[k - 1].children.last(), item.children.first()) {
                    (Some(a), Some(b)) => self.gap(a, b) >= 2,
                    _ => false,
                }
            };
            let marker = match start {
                Some(_) => format!("{number}. "),
                None => "- ".to_string(),
            };
            let item_ctx = Ctx {
                compact: if out.is_empty() { ctx.compact } else { false },
                ..ctx
            };
            let mut prefix = match item.task {
                Some(true) => glyphs(Style::PLAIN, "[x] ", None),
                Some(false) => glyphs(Style::PLAIN, "[ ] ", None),
                None => Line::new(),
            };
            let item_seg = |text: Line| Seg {
                kind: Kind::Item {
                    marker: marker.clone(),
                    indent: ctx.list_depth,
                    loose,
                },
                text,
                quote: ctx.quote,
                list_indent: ctx.list_indent,
                compact: item_ctx.compact,
            };
            let mut children = item.children.iter().peekable();
            match children.peek().map(|c| &c.block) {
                Some(Block::Para(line, _)) => {
                    prefix.extend(soft_spaces(line));
                    out.push(item_seg(prefix));
                    children.next();
                }
                _ => out.push(item_seg(prefix)),
            }
            for c in children {
                match &c.block {
                    Block::List(s, inner) => out.extend(self.list(
                        *s,
                        inner,
                        Ctx {
                            list_depth: ctx.list_depth + 1,
                            loose_before: false,
                            compact: true,
                            ..ctx
                        },
                    )),
                    _ => out.extend(self.node(
                        c,
                        Ctx {
                            list_indent: ctx.list_depth + 1,
                            compact: false,
                            ..ctx
                        },
                    )),
                }
            }
        }
        out
    }
}

/// Soft breaks as spaces (outside quotes).
fn soft_spaces(line: &[Glyph]) -> Line {
    line.iter()
        .map(|g| {
            if g.c == SOFT {
                Glyph { c: ' ', ..*g }
            } else {
                *g
            }
        })
        .collect()
}

/// A top-level paragraph's text, which runs into the text next to it.
fn plain(s: &Seg) -> bool {
    matches!(s.kind, Kind::Text { plain: true })
}

/// `ES`: top-level paragraphs and the blank lines between them become one
/// block of text.
fn group(segs: Vec<Seg>) -> Vec<Seg> {
    let mut out: Vec<Seg> = Vec::new();
    for s in segs {
        if plain(&s)
            && let Some(last) = out.last_mut()
            && plain(last)
        {
            last.text.extend(s.text);
            continue;
        }
        out.push(s);
    }
    out
}

/// `M4`: a blank row before a block, but not when the source had none
/// before it in a list or quote, nor between items of one level (unless
/// loose) or going deeper.
fn margin(prev: &Seg, cur: &Seg) -> bool {
    if cur.compact {
        return false;
    }
    if let (
        Kind::Item { indent: a, .. },
        Kind::Item {
            indent: b, loose, ..
        },
    ) = (&prev.kind, &cur.kind)
    {
        return *loose || b < a;
    }
    true
}

/// `L4`: each block's lines, prefixed by its quote bars and list indent,
/// then wrapped.
fn draw(blocks: &[Seg], width: usize) -> Vec<Line> {
    let mut out = Vec::new();
    for (k, b) in blocks.iter().enumerate() {
        if k > 0 && margin(&blocks[k - 1], b) {
            out.push(Line::new());
        }
        let quote = match b.kind {
            Kind::Quote if b.quote == 0 => 1,
            _ => b.quote,
        };
        let body: Vec<Line> = match &b.kind {
            Kind::Text { .. } => split(&b.text),
            Kind::Code => split(&b.text),
            Kind::Header(level) => {
                let mut line = b.text.clone();
                restyle(&mut line, |s| match level {
                    1 => {
                        s.bold = true;
                        s.underline = true;
                    }
                    0 | 2 => s.bold = true,
                    3 => {
                        s.bold = true;
                        s.italic = true;
                    }
                    4 => s.underline = true,
                    5 => s.italic = true,
                    _ => s.dim = true,
                });
                split(&line)
            }
            Kind::Item { marker, indent, .. } => {
                let lead = "  ".repeat(*indent);
                split(&b.text)
                    .into_iter()
                    .enumerate()
                    .map(|(j, line)| {
                        let mut row = if j == 0 {
                            glyphs(Style::PLAIN, &format!("{lead}{marker}"), None)
                        } else {
                            spaces(lead.len() + marker.chars().count())
                        };
                        row.extend(line);
                        row
                    })
                    .collect()
            }
            Kind::Quote => {
                let mut line = b.text.clone();
                restyle(&mut line, |s| s.italic = true);
                split(&line)
            }
            Kind::Rule => vec![glyphs(Style::PLAIN, &"─".repeat(width), None)],
            Kind::Table { align, head, rows } => {
                let room = width.saturating_sub(quote * 2 + b.list_indent * 2).max(1);
                table(align, head, rows, room)
            }
        };
        let mut prefix = Line::new();
        for _ in 0..quote {
            prefix.extend(glyphs(Style::DIM, "│ ", None));
        }
        prefix.extend(spaces(2 * b.list_indent));
        for line in body {
            let mut row = prefix.clone();
            row.extend(line);
            if matches!(b.kind, Kind::Rule) {
                row.truncate(width);
                out.push(row);
            } else {
                out.extend(wrap(&row, width));
            }
        }
    }
    out
}

/// Wrapped the way Kiro's `twinki` does: plain ASCII text by `onn` (a word
/// longer than the row on rows of its own), other text by `ann` (a long
/// word's last piece goes on with the next words); spaces at a break go.
fn wrap(line: &[Glyph], width: usize) -> Vec<Line> {
    let width = width.max(1);
    if width_of(line) <= width {
        return vec![line.to_vec()];
    }
    let ascii = line
        .iter()
        .all(|g| g.style == Style::PLAIN && (' '..='~').contains(&g.c));
    let rows = if ascii {
        wrap_plain(line, width)
    } else {
        wrap_styled(line, width)
    };
    rows.into_iter()
        .map(|mut r| {
            while r.last().is_some_and(|g| g.c.is_whitespace()) {
                r.pop();
            }
            r
        })
        .collect()
}

/// `onn`.
fn wrap_plain(line: &[Glyph], width: usize) -> Vec<Line> {
    let space = Glyph {
        c: ' ',
        style: Style::PLAIN,
        src: None,
    };
    let mut rows = Vec::new();
    let mut cur: Line = Line::new();
    for word in line.split(|g| g.c == ' ') {
        if word.len() > width {
            if !cur.is_empty() {
                rows.push(std::mem::take(&mut cur));
            }
            rows.extend(word.chunks(width).map(<[Glyph]>::to_vec));
        } else if cur.len() + word.len() < width {
            if !cur.is_empty() {
                cur.push(space);
            }
            cur.extend_from_slice(word);
        } else {
            if !cur.is_empty() {
                rows.push(std::mem::take(&mut cur));
            }
            cur = word.to_vec();
        }
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    if rows.is_empty() {
        rows.push(Line::new());
    }
    rows
}

/// `ann`.
fn wrap_styled(line: &[Glyph], width: usize) -> Vec<Line> {
    // Runs of spaces and of anything else.
    let mut runs: Vec<(&[Glyph], bool)> = Vec::new();
    let mut from = 0;
    for k in 1..=line.len() {
        if k == line.len() || (line[k].c == ' ') != (line[from].c == ' ') {
            // Spaces right after a change of style carry its escape, and
            // `ann` then takes them for a word.
            let styled = from > 0 && line[from - 1].style != line[from].style;
            let blank = line[from].c == ' ' && !styled;
            runs.push((&line[from..k], blank));
            from = k;
        }
    }
    let mut rows: Vec<Line> = Vec::new();
    let mut cur: Line = Line::new();
    let mut col = 0;
    for (run, blank) in runs {
        let w = width_of(run);
        if w > width && !blank {
            if !cur.is_empty() {
                rows.push(std::mem::take(&mut cur));
            }
            let mut piece = Line::new();
            let mut pw = 0;
            for g in run {
                let cw = g.c.width().unwrap_or(0);
                if pw + cw > width {
                    rows.push(std::mem::take(&mut piece));
                    pw = 0;
                }
                piece.push(*g);
                pw += cw;
            }
            cur = piece;
            col = pw;
            continue;
        }
        if col + w > width && col > 0 {
            rows.push(std::mem::take(&mut cur));
            if blank {
                col = 0;
            } else {
                cur = run.to_vec();
                col = w;
            }
        } else {
            cur.extend_from_slice(run);
            col += w;
        }
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    if rows.is_empty() {
        rows.push(Line::new());
    }
    rows
}

/// `O4`: a table in dim box lines, columns at their widest when they fit,
/// else shrunk; when even 8 columns each would not fit, "Header: value"
/// lines, a blank line between rows.
fn table(align: &[Alignment], head: &[Line], rows: &[Vec<Line>], width: usize) -> Vec<Line> {
    let n = head.len();
    if n == 0 {
        return Vec::new();
    }
    let mut widths: Vec<usize> = (0..n)
        .map(|c| {
            std::iter::once(&head[c])
                .chain(rows.iter().filter_map(|r| r.get(c)))
                .map(|x| width_of(x))
                .max()
                .unwrap_or(0)
                .max(3)
        })
        .collect();
    let overhead = 3 * n + 1;
    if widths.iter().map(|w| (*w).min(8)).sum::<usize>() + overhead + 4 > width {
        return stacked(head, rows);
    }
    let room = width as isize - overhead as isize - 4;
    if widths.iter().sum::<usize>() as isize > room && room > 0 {
        let room = room as usize;
        let each = room / n;
        let mut left = room as isize;
        let fixed: Vec<bool> = widths.iter().map(|w| *w <= each).collect();
        for c in 0..n {
            if fixed[c] {
                left -= widths[c] as isize;
            }
        }
        let free = fixed.iter().filter(|f| !**f).count();
        let sum: usize = (0..n).filter(|c| !fixed[*c]).map(|c| widths[c]).sum();
        if sum > 0 && free > 0 {
            let left = left.max(0) as f64;
            let floor = 3.max(10.min((left as usize) / free));
            for c in 0..n {
                if !fixed[c] {
                    widths[c] = floor.max((widths[c] as f64 / sum as f64 * left).floor() as usize);
                }
            }
        }
    }
    let dim = Style::DIM;
    let rule = |l: &str, m: &str, r: &str| {
        let parts: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
        glyphs(dim, &format!("{l}{}{r}", parts.join(m)), None)
    };
    let row_lines = |r: &[Line], header: bool| -> Vec<Line> {
        let cells: Vec<Vec<Line>> = (0..n)
            .map(|c| {
                let mut cell = r.get(c).cloned().unwrap_or_default();
                if header {
                    restyle(&mut cell, |s| s.bold = true);
                }
                split(&cell)
                    .iter()
                    .flat_map(|l| wrap_cell_words(l, widths[c]))
                    .collect()
            })
            .collect();
        let h = cells.iter().map(Vec::len).max().unwrap_or(1).max(1);
        (0..h)
            .map(|i| {
                let mut line = glyphs(dim, "│", None);
                line.extend(spaces(1));
                for (c, cell) in cells.iter().enumerate() {
                    if c > 0 {
                        line.extend(spaces(1));
                        line.extend(glyphs(dim, "│", None));
                        line.extend(spaces(1));
                    }
                    let a = align.get(c).copied().unwrap_or(Alignment::None);
                    let text = cell.get(i).cloned().unwrap_or_default();
                    line.extend(aligned(&text, widths[c], a));
                }
                line.extend(spaces(1));
                line.extend(glyphs(dim, "│", None));
                line
            })
            .collect()
    };
    let mut out = vec![rule("┌", "┬", "┐")];
    out.extend(row_lines(head, true));
    if !rows.is_empty() {
        out.push(rule("├", "┼", "┤"));
        for (k, r) in rows.iter().enumerate() {
            out.extend(row_lines(r, false));
            if k + 1 < rows.len() {
                out.push(rule("├", "┼", "┤"));
            }
        }
    }
    out.push(rule("└", "┴", "┘"));
    out
}

/// `s_t`: each row as "Header: value" lines.
fn stacked(head: &[Line], rows: &[Vec<Line>]) -> Vec<Line> {
    let bold = |l: &Line| {
        let mut l = l.clone();
        restyle(&mut l, |s| s.bold = true);
        l
    };
    if rows.is_empty() {
        return head.iter().filter(|h| !h.is_empty()).map(bold).collect();
    }
    let mut out: Vec<Line> = Vec::new();
    for row in rows {
        if row.iter().all(|c| c.is_empty()) {
            continue;
        }
        let mut lines = Vec::new();
        for (c, value) in row.iter().enumerate() {
            if value.is_empty() {
                continue;
            }
            let label = bold(&head.get(c).cloned().unwrap_or_default());
            let lw = width_of(&label);
            for (j, part) in split(value).into_iter().enumerate() {
                let mut line = if j == 0 {
                    let mut l = label.clone();
                    l.extend(glyphs(Style::PLAIN, ":", None));
                    l.push(Glyph {
                        c: ' ',
                        style: Style::PLAIN,
                        src: None,
                    });
                    l
                } else {
                    spaces(lw + 2)
                };
                line.extend(part);
                lines.push(line);
            }
        }
        if !out.is_empty() {
            out.push(Line::new());
        }
        out.extend(lines);
    }
    out
}

/// `l_t`: a cell's line by words; a word longer than the column is cut and
/// its last piece goes on with the next words.
fn wrap_cell_words(line: &[Glyph], width: usize) -> Vec<Line> {
    if width_of(line) <= width {
        return vec![line.to_vec()];
    }
    let space = Glyph {
        c: ' ',
        style: Style::PLAIN,
        src: None,
    };
    let mut rows = Vec::new();
    let mut cur: Line = Line::new();
    for word in line.split(|g| g.c == ' ') {
        if width_of(word) > width {
            if !cur.is_empty() {
                rows.push(std::mem::take(&mut cur));
            }
            let mut piece = Line::new();
            let mut pw = 0;
            for g in word {
                let cw = g.c.width().unwrap_or(0);
                if !piece.is_empty() && pw + cw > width {
                    rows.push(std::mem::take(&mut piece));
                    pw = 0;
                }
                piece.push(*g);
                pw += cw;
            }
            cur = piece;
            continue;
        }
        let mut joined = cur.clone();
        if !joined.is_empty() {
            joined.push(space);
        }
        joined.extend_from_slice(word);
        if width_of(&joined) <= width {
            cur = joined;
        } else {
            if !cur.is_empty() {
                rows.push(std::mem::take(&mut cur));
            }
            cur = word.to_vec();
        }
    }
    if !cur.is_empty() {
        rows.push(cur);
    }
    if rows.is_empty() {
        rows.push(Line::new());
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::tests::{Shot, oracle};
    use alacritty_terminal::vte::ansi::{Color, NamedColor};

    /// Each answer of `claude_markdown_cases.md` as Kiro CLI 2.28.0 drew it
    /// (`kiro-cli chat --resume-id` of two saved chats, full screen, 60 and
    /// 40 columns).
    #[test]
    fn answers_look_as_kiro_draws_them() {
        let colour = |c: &Color| match c {
            Color::Named(NamedColor::Foreground) => Some(Fg::Plain),
            Color::Named(NamedColor::BrightBlue) => Some(Fg::Code),
            Color::Named(NamedColor::Blue) => Some(Fg::Link),
            _ => None,
        };
        let shot = |bytes: &'static [u8], cols, rows| Shot {
            bytes,
            cols,
            rows,
            history: false,
        };
        let shots = [
            shot(
                include_bytes!("../../tests/fixtures/kiro_markdown_60x300_a.bin"),
                60,
                300,
            ),
            shot(
                include_bytes!("../../tests/fixtures/kiro_markdown_60x300_b.bin"),
                60,
                300,
            ),
            shot(
                include_bytes!("../../tests/fixtures/kiro_markdown_40x400_a.bin"),
                40,
                400,
            ),
            shot(
                include_bytes!("../../tests/fixtures/kiro_markdown_40x400_b.bin"),
                40,
                400,
            ),
        ];
        oracle(
            |md, w| lines_as(Flavor::Kiro, md, w),
            &shots,
            "› case ",
            2,
            |cols| cols - 2,
            colour,
            |fg| fg,
        );
    }
}
