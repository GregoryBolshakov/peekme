//! GitHub Copilot CLI's layout (1.0.98): its React renderer over `marked`
//! (`vXr` and the components it picks in `cli-main.js`). Each block is a box
//! with a blank row under it (none inside a tight list, none after the last
//! block of the answer); headings lose their `#`, lists get "•" and "1.",
//! quotes a "│" bar, a rule is 20 "─", and tables are drawn in box lines
//! fitted to the width (`jf`, `HAe`, `zAe`). Colours are its fallback theme
//! (the terminal's own palette).

use pulldown_cmark::Alignment;

use super::*;

/// Copilot's colours: its theme of terminal palette colours.
pub(super) fn colors() -> Vec<(Fg, String)> {
    vec![
        (Fg::Code, "37".into()),
        (Fg::Block, "36".into()),
        (Fg::Link, "36".into()),
        (Fg::Border, "90".into()),
        (Fg::Image, "35".into()),
    ]
}

/// The lines of `text` as Copilot lays it out `width` columns wide, a
/// link's address after its text (the box shows no hyperlinks).
pub(super) fn lines(text: &str, width: usize) -> Vec<Line> {
    render(text, width, true)
}

/// `urls`: false for Copilot's own look, where a link shows its text alone.
fn render(text: &str, width: usize, urls: bool) -> Vec<Line> {
    let nodes = parse_with(Flavor::Copilot, text, urls);
    Lay { text }.blocks(&nodes, width, Ctx::default(), Last::Trim)
}

/// What happens to the blank row under the last block of a run: the
/// answer's last block and its own last ones lose it, a quote's last block
/// loses it.
#[derive(Clone, Copy, PartialEq)]
enum Last {
    Keep,
    Zero,
    Trim,
}

struct Lay<'a> {
    text: &'a str,
}

/// Where a block is: in a list (and whether it is loose), and whether it is
/// an item's own block (`marked` makes an item's text a "text" token, never a
/// paragraph, so it has no blank row under it).
#[derive(Clone, Copy, Default)]
struct Ctx {
    list: Option<bool>,
    item: bool,
}

/// Wrapped the way Copilot's terminal renderer does: `wrap-ansi` (hard, no
/// trim), then a row that starts with a space after a row that does not end
/// with one loses that space.
fn wrap(text: &[Glyph], width: usize) -> Vec<Line> {
    let mut out = Vec::new();
    for line in split(text) {
        let width = width.max(1);
        if width_of(&line) <= width {
            out.push(line);
            continue;
        }
        let mut rows = wrap_ansi(&line, width, true);
        for k in 1..rows.len() {
            let prev_ends_blank = rows[k - 1].last().is_none_or(|g| g.c.is_whitespace());
            let starts_blank = rows[k].first().is_some_and(|g| g.c == ' ');
            if starts_blank && !prev_ends_blank && rows[k].iter().any(|g| g.c != ' ') {
                rows[k].remove(0);
            }
        }
        out.extend(rows);
    }
    out
}

impl Lay<'_> {
    /// `nodes` one under the other, each with the blank rows under it.
    fn blocks(&self, nodes: &[Node], width: usize, ctx: Ctx, last: Last) -> Vec<Line> {
        let mut out = Vec::new();
        for (i, n) in nodes.iter().enumerate() {
            let is_last = i + 1 == nodes.len();
            let inner = if is_last && last == Last::Trim {
                Last::Trim
            } else {
                Last::Keep
            };
            let (lines, margin) = self.block(n, width, ctx, inner);
            out.extend(lines);
            if !(is_last && last != Last::Keep) {
                out.extend(std::iter::repeat_n(Line::new(), margin));
            }
        }
        out
    }

    /// A block's lines and the blank rows under it.
    fn block(&self, n: &Node, width: usize, ctx: Ctx, last: Last) -> (Vec<Line>, usize) {
        let gap = usize::from(ctx.list.is_none());
        match &n.block {
            Block::Para(line, _) => {
                let margin = usize::from(!ctx.item && ctx.list != Some(false));
                (wrap(line, width), margin)
            }
            Block::Heading(level, line) => {
                let mut line = line.clone();
                restyle(&mut line, |s| {
                    s.bold = true;
                    s.underline |= *level == 1;
                });
                (wrap(&line, width), gap)
            }
            Block::Code { text, .. } => {
                let mut text = text.clone();
                restyle(&mut text, |s| s.fg = Fg::Block);
                (wrap(&text, width), gap)
            }
            Block::Rule => {
                let style = Style {
                    fg: Fg::Border,
                    ..Style::PLAIN
                };
                (vec![glyphs(style, &"─".repeat(20), None)], 1)
            }
            Block::Html(text) => (wrap(&strip_tags(text), width), 0),
            Block::Quote(children) => {
                let inner_last = if last == Last::Trim {
                    Last::Trim
                } else {
                    Last::Zero
                };
                let inner_ctx = Ctx { item: false, ..ctx };
                let inner = self.blocks(children, width.saturating_sub(2), inner_ctx, inner_last);
                let lines = inner
                    .into_iter()
                    .map(|line| {
                        let mut row = glyphs(Style::PLAIN, "│ ", None);
                        row.extend(line);
                        row
                    })
                    .collect();
                (lines, gap)
            }
            Block::List(start, items) => {
                let loose = self.loose(n, items);
                let mut out = Vec::new();
                for (k, item) in items.iter().enumerate() {
                    let marker = match (item.task, start) {
                        (Some(true), _) => "\u{2713} ".to_string(),
                        (Some(false), _) => "\u{2022} ".to_string(),
                        (None, Some(s)) => format!("{}. ", s + k as u64),
                        (None, None) => "\u{2022} ".to_string(),
                    };
                    let mw = marker.chars().count();
                    let item_last = if k + 1 == items.len() && last == Last::Trim {
                        Last::Trim
                    } else {
                        Last::Keep
                    };
                    let content = self.blocks(
                        &item.children,
                        width.saturating_sub(mw).max(1),
                        Ctx {
                            list: Some(loose),
                            item: true,
                        },
                        item_last,
                    );
                    if content.is_empty() {
                        out.push(glyphs(Style::PLAIN, &marker, None));
                    }
                    for (j, line) in content.into_iter().enumerate() {
                        let mut row = if j == 0 {
                            glyphs(Style::PLAIN, &marker, None)
                        } else {
                            spaces(mw)
                        };
                        row.extend(line);
                        out.push(row);
                    }
                }
                (out, gap)
            }
            Block::Table { align, head, rows } => (table(align, head, rows, width), gap),
        }
    }

    /// `marked` calls a list loose when an item but the last ends in a blank
    /// line, or has one between two of its own blocks.
    fn loose(&self, list: &Node, items: &[Item]) -> bool {
        let starts: Vec<usize> = items
            .iter()
            .filter_map(|i| i.children.first().map(|c| c.range.start))
            .collect();
        let gap = |a: usize, b: usize| {
            let a = self.text[..a.min(self.text.len())].trim_end().len();
            self.text
                .get(a..b)
                .is_some_and(|g| g.matches('\n').count() >= 2)
        };
        let ends: Vec<usize> = items
            .iter()
            .filter_map(|i| i.children.last().map(|c| c.range.end))
            .collect();
        let _ = list;
        ends.iter()
            .zip(starts.iter().skip(1))
            .any(|(&e, &s)| gap(e, s))
            || items.iter().any(|item| self.spread(item))
    }

    fn spread(&self, item: &Item) -> bool {
        item.children.windows(2).any(|w| {
            let end = w[0].range.start
                + self
                    .text
                    .get(w[0].range.clone())
                    .map_or(0, |s| s.trim_end().len());
            self.text
                .get(end..w[1].range.start)
                .is_some_and(|gap| gap.matches('\n').count() >= 2)
        })
    }
}

/// HTML as text, its tags taken out.
fn strip_tags(text: &[Glyph]) -> Line {
    let mut out = Line::new();
    let mut in_tag = false;
    for g in text {
        match g.c {
            '<' => in_tag = true,
            '>' if in_tag => in_tag = false,
            _ if !in_tag => out.push(*g),
            _ => {}
        }
    }
    out
}

/// A table in box lines (`jf`): columns at their widest when they fit,
/// else shared out to fill the width; cells wrapped by words, a word wider
/// than its column broken. Cells are plain text unless they hold a link.
/// A table without rows draws nothing.
fn table(align: &[Alignment], head: &[Line], rows: &[Vec<Line>], width: usize) -> Vec<Line> {
    const MIN: usize = 3;
    let n = head.len();
    if n == 0 || rows.is_empty() {
        return Vec::new();
    }
    // Inline code is bare text in a cell.
    let plain = |c: &Line| -> Line {
        let link = c.iter().any(|g| g.style.fg == Fg::Link);
        c.iter()
            .filter(|g| !(g.c == '\u{a0}' && g.src.is_none()))
            .map(|g| Glyph {
                style: if link && g.style.fg != Fg::Code {
                    g.style
                } else {
                    Style::PLAIN
                },
                ..*g
            })
            .collect()
    };
    let head: Vec<Line> = head.iter().map(plain).collect();
    let rows: Vec<Vec<Line>> = rows.iter().map(|r| r.iter().map(plain).collect()).collect();
    // HAe: the widths, borders included, for a table `width - 1` wide.
    let total = (width.saturating_sub(1)).max(20);
    let total = total.max(n * MIN + n + 1);
    let room = (total as isize - (3 * n as isize + 1)).max(n as isize) as usize;
    let mut content = vec![0usize; n];
    let mut word = vec![0usize; n];
    for r in std::iter::once(&head).chain(rows.iter()) {
        for (c, cell) in r.iter().enumerate().take(n) {
            content[c] = content[c].max(width_of(cell));
            let w = cell
                .split(|g| g.c.is_whitespace())
                .map(width_of)
                .max()
                .unwrap_or(0);
            word[c] = word[c].max(w);
        }
    }
    let sum: usize = content.iter().sum();
    let mut widths: Vec<usize> = if sum <= room {
        content.iter().map(|w| w + 2).collect()
    } else {
        let mut u: Vec<usize> = (0..n)
            .map(|c| ((room * content[c]) / sum).max(word[c]) + 2)
            .collect();
        if u.iter().sum::<usize>() + n + 1 > total {
            u = squeeze(&u, &word, total, n);
        }
        // b$r: the widest column takes up the difference.
        let used = u.iter().sum::<usize>() + n + 1;
        if used != total {
            let mut widest = 0;
            for c in 1..n {
                if u[c] > u[widest] {
                    widest = c;
                }
            }
            u[widest] = (u[widest] as isize + total as isize - used as isize).max(0) as usize;
        }
        u
    };
    for w in &mut widths {
        *w = (*w).max(MIN);
    }
    let border = Style {
        fg: Fg::Border,
        ..Style::PLAIN
    };
    let rule = |l: &str, m: &str, r: &str| {
        let parts: Vec<String> = widths.iter().map(|w| "─".repeat(*w)).collect();
        glyphs(border, &format!("{l}{}{r}", parts.join(m)), None)
    };
    let row_lines = |r: &[Line], header: bool| -> Vec<Line> {
        let cells: Vec<Vec<Line>> = (0..n)
            .map(|c| {
                let inner = widths[c].saturating_sub(2).max(1);
                let a = align.get(c).copied().unwrap_or(Alignment::None);
                let a = match a {
                    Alignment::Center | Alignment::Right => a,
                    _ => Alignment::Left,
                };
                let empty = Line::new();
                let cell = r.get(c).unwrap_or(&empty);
                split(cell)
                    .iter()
                    .flat_map(|l| wrap_cell_words(l, inner))
                    .map(|l| {
                        let mut l = aligned(&l, inner, a);
                        if header {
                            restyle(&mut l, |s| s.bold = true);
                        }
                        l
                    })
                    .collect()
            })
            .collect();
        let h = cells.iter().map(Vec::len).max().unwrap_or(1).max(1);
        (0..h)
            .map(|i| {
                let mut line = glyphs(border, "│", None);
                for (c, cell) in cells.iter().enumerate() {
                    if c > 0 {
                        line.extend(glyphs(border, "│", None));
                    }
                    line.extend(spaces(1));
                    let inner = widths[c].saturating_sub(2).max(1);
                    line.extend(cell.get(i).cloned().unwrap_or_else(|| spaces(inner)));
                    line.extend(spaces(1));
                }
                line.extend(glyphs(border, "│", None));
                line
            })
            .collect()
    };
    let mut out = vec![rule("┌", "┬", "┐")];
    out.extend(row_lines(&head, true));
    out.push(rule("├", "┼", "┤"));
    for (k, r) in rows.iter().enumerate() {
        if k > 0 {
            out.push(rule("├", "┼", "┤"));
        }
        out.extend(row_lines(r, false));
    }
    out.push(rule("└", "┴", "┘"));
    out
}

/// y$r: columns at most their longest word keep their width, the others
/// share what is left; failing that every column gets its share.
fn squeeze(u: &[usize], word: &[usize], total: usize, n: usize) -> Vec<usize> {
    let room = total as isize - n as isize - 1;
    let floor: Vec<usize> = (0..u.len()).map(|c| (word[c] + 2).max(3)).collect();
    let (mut fixed, mut free) = (0isize, 0isize);
    for c in 0..u.len() {
        if u[c] <= floor[c] {
            fixed += u[c] as isize;
        } else {
            free += u[c] as isize;
        }
    }
    let left = room - fixed;
    if left > 0 && free > 0 {
        return (0..u.len())
            .map(|c| {
                if u[c] <= floor[c] {
                    u[c]
                } else {
                    floor[c].max(((left * u[c] as isize) / free).max(0) as usize)
                }
            })
            .collect();
    }
    let sum: isize = u.iter().map(|&w| w as isize).sum();
    let mut out: Vec<usize> = u
        .iter()
        .map(|&w| 3.max(((room * w as isize) / sum).max(0) as usize))
        .collect();
    if (n * 3) as isize > room {
        let each = 3.max((room / n as isize).max(0) as usize);
        out = vec![each; u.len()];
    }
    out
}

/// A cell's line in rows of `width` (C$r): words that fit go on, white space
/// at the start of a row is dropped, a word that does not fit starts a new
/// row, and one wider than the row is broken there char by char.
fn wrap_cell_words(line: &[Glyph], width: usize) -> Vec<Line> {
    let mut rows: Vec<Line> = vec![Line::new()];
    let mut col = 0;
    let push_row = |rows: &mut Vec<Line>, col: &mut usize| {
        rows.push(Line::new());
        *col = 0;
    };
    // Runs of white space and of anything else.
    let mut runs: Vec<&[Glyph]> = Vec::new();
    let mut from = 0;
    for k in 1..=line.len() {
        if k == line.len() || line[k].c.is_whitespace() != line[from].c.is_whitespace() {
            if from < k {
                runs.push(&line[from..k]);
            }
            from = k;
        }
    }
    for run in runs {
        let w = width_of(run);
        if run[0].c.is_whitespace() {
            let nbsp = run.iter().any(|g| g.c == '\u{a0}');
            if (col > 0 || nbsp) && col + w <= width {
                rows.last_mut().unwrap().extend_from_slice(run);
                col += w;
            } else if col > 0 {
                push_row(&mut rows, &mut col);
            }
            continue;
        }
        if col + w <= width {
            rows.last_mut().unwrap().extend_from_slice(run);
            col += w;
        } else if col == 0 || w > width {
            let mut first = col > 0;
            for g in run {
                let cw = g.c.width().unwrap_or(0);
                if (first || col + cw > width) && col > 0 {
                    push_row(&mut rows, &mut col);
                }
                first = false;
                rows.last_mut().unwrap().push(*g);
                col += cw;
            }
        } else {
            push_row(&mut rows, &mut col);
            rows.last_mut().unwrap().extend_from_slice(run);
            col += w;
        }
    }
    if rows.len() > 1 && rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markdown::tests::{Shot, oracle};
    use alacritty_terminal::vte::ansi::{Color, NamedColor};

    /// Each answer of `claude_markdown_cases.md` as Copilot CLI 1.0.98 drew
    /// it (`copilot --resume` of a saved chat, 60 and 40 columns, its answer
    /// 3 columns in and 2 short of the right edge).
    #[test]
    fn answers_look_as_copilot_draws_them() {
        let colour = |c: &Color| match c {
            Color::Named(NamedColor::Foreground) => Some(Fg::Plain),
            Color::Named(NamedColor::White) => Some(Fg::Code),
            Color::Named(NamedColor::Cyan) => Some(Fg::Link),
            Color::Named(NamedColor::BrightBlack) => Some(Fg::Border),
            _ => None,
        };
        let shots = [
            Shot {
                bytes: include_bytes!("../../tests/fixtures/copilot_markdown_60x300.bin"),
                cols: 60,
                rows: 300,
                history: true,
            },
            Shot {
                bytes: include_bytes!("../../tests/fixtures/copilot_markdown_40x400.bin"),
                cols: 40,
                rows: 400,
                history: true,
            },
        ];
        // Cyan is both its links and its code blocks.
        let same = |fg: Fg| if fg == Fg::Block { Fg::Link } else { fg };
        oracle(
            |md, w| render(md, w, false),
            &shots,
            "❯ case ",
            3,
            |cols| cols - 5,
            colour,
            same,
        );
    }
}
