//! What the explainer is told about the selection.
//!
//! The selected words alone are ambiguous ("Desktop app" - which one?), and a
//! whole conversation is too big and too slow. So the explainer gets layers,
//! each with its own budget, and the selection is always marked *in place*:
//!
//! 1. environment: one line saying this is Codex CLI in some directory (this
//!    alone resolves most interface text such as tips and status lines);
//! 2. the passage the selection sits in, found in the conversation by matching
//!    the selection *with its on-screen neighbours* so the right occurrence is
//!    chosen, or the screen itself when the text isn't part of the conversation;
//! 3. the request that produced that passage, and a one-line-each outline of
//!    the user's earlier requests (the topic of the chat);
//! 4. the earliest other mentions of the same words (where a term was introduced).
//!
//! The user can escalate to the full conversation (a forked thread) when this
//! is not enough; see `explain.rs`.

/// The selection as it appears on screen: the screen text and the selected range.
#[derive(Clone, Debug)]
pub struct ScreenSel {
    pub text: Vec<char>,
    pub start: usize,
    pub end: usize,
}

impl ScreenSel {
    pub fn selected(&self) -> String {
        self.text[self.start..self.end].iter().collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Agent,
    User,
    /// Output of the given command.
    Command(String),
}

#[derive(Clone, Debug)]
pub struct ConvItem {
    pub turn: Option<String>,
    pub source: Source,
    pub text: String,
}

/// A Codex conversation, oldest item first.
#[derive(Clone, Debug, Default)]
pub struct Conversation {
    pub id: String,
    pub title: Option<String>,
    pub items: Vec<ConvItem>,
}

/// Where the selection was found: item index and char range in its text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hit {
    pub item: usize,
    pub start: usize,
    pub end: usize,
}

// Budgets, in characters (roughly 4 per token).
const PASSAGE_SIDE: usize = 1200;
const SCREEN_SIDE: usize = 1500;
const REQUEST_MAX: usize = 1200;
const EARLIER_REQUESTS: usize = 5;
const EARLIER_REQUEST_MAX: usize = 160;
const MENTIONS: usize = 2;
const MENTION_SIDE: usize = 160;
const SEARCH_CAP: usize = 200_000;

pub const OPEN: char = '⟦';
pub const CLOSE: char = '⟧';

/// Characters dropped before comparing screen text with conversation text:
/// markdown markers and the glyphs a TUI draws around text.
fn dropped(c: char) -> bool {
    matches!(
        c,
        '`' | '*'
            | '#'
            | '>'
            | '-'
            | '•'
            | '›'
            | '│'
            | '╭'
            | '╮'
            | '╰'
            | '╯'
            | '─'
            | '┃'
            | '└'
            | '├'
            | '┌'
            | '┐'
            | '┘'
            | '┤'
    )
}

/// Comparable form of `chars` plus, for each kept char, its index in `chars`.
pub fn key_map(chars: &[char]) -> (Vec<char>, Vec<usize>) {
    let mut out = Vec::with_capacity(chars.len());
    let mut map = Vec::with_capacity(chars.len());
    let mut space = false;
    for (i, &c) in chars.iter().enumerate() {
        if dropped(c) {
            continue;
        }
        if c.is_whitespace() {
            if !space && !out.is_empty() {
                out.push(' ');
                map.push(i);
            }
            space = true;
        } else {
            out.push(c);
            map.push(i);
            space = false;
        }
    }
    if out.last() == Some(&' ') {
        out.pop();
        map.pop();
    }
    (out, map)
}

fn rfind(hay: &[char], needle: &[char]) -> Option<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return None;
    }
    (0..=hay.len() - needle.len())
        .rev()
        .find(|&i| hay[i..i + needle.len()] == *needle)
}

fn find_all(hay: &[char], needle: &[char]) -> Vec<usize> {
    if needle.is_empty() || needle.len() > hay.len() {
        return Vec::new();
    }
    (0..=hay.len() - needle.len())
        .filter(|&i| hay[i..i + needle.len()] == *needle)
        .collect()
}

/// Selection bounds in the comparable form of the screen text.
fn screen_key(sel: &ScreenSel) -> Option<(Vec<char>, usize, usize)> {
    let (key, map) = key_map(&sel.text);
    let ns = map.iter().position(|&i| i >= sel.start)?;
    let ne = map.iter().rposition(|&i| i < sel.end)? + 1;
    (ns < ne).then_some((key, ns, ne))
}

fn item_key(item: &ConvItem) -> (Vec<char>, Vec<usize>) {
    let chars: Vec<char> = item.text.chars().take(SEARCH_CAP).collect();
    key_map(&chars)
}

/// Find the passage the selection came from. The selection is matched together
/// with its on-screen neighbours first (up to 60 chars each side), so that
/// "Desktop app" in "Try the Desktop app on Linux" finds that sentence and not
/// another mention; only then the bare selection, most recent item first.
pub fn find(conv: &Conversation, sel: &ScreenSel) -> Option<Hit> {
    let (sk, ns, ne) = screen_key(sel)?;
    let keys: Vec<_> = conv.items.iter().map(item_key).collect();
    // Both neighbours first, then one side only (the other side may belong to
    // a different message on screen), then the bare selection.
    let mut tried = Vec::new();
    for (before, after) in [
        (60usize, 60usize),
        (40, 0),
        (0, 40),
        (25, 25),
        (15, 0),
        (0, 15),
        (0, 0),
    ] {
        let a = ns.saturating_sub(before);
        let b = (ne + after).min(sk.len());
        let needle = &sk[a..b];
        let bare = a == ns && b == ne;
        if needle.len() < 3 || (bare && (before, after) != (0, 0)) || tried.contains(&(a, b)) {
            continue;
        }
        tried.push((a, b));
        for (idx, (ik, imap)) in keys.iter().enumerate().rev() {
            if let Some(pos) = rfind(ik, needle) {
                let s = pos + (ns - a);
                let e = s + (ne - ns);
                return Some(Hit {
                    item: idx,
                    start: imap[s],
                    end: imap[e - 1] + 1,
                });
            }
        }
    }
    None
}

/// Up to `side` chars on each side of `start..end`, trimmed to line (or word)
/// boundaries, with the selection marked in place.
fn marked_window(text: &[char], start: usize, end: usize, side: usize) -> String {
    let mut a = start.saturating_sub(side);
    let mut b = (end + side).min(text.len());
    if a > 0 {
        let cut = text[a..start]
            .iter()
            .position(|&c| c == '\n')
            .or_else(|| text[a..start].iter().position(|c| c.is_whitespace()));
        a += cut.map_or(0, |p| p + 1);
    }
    if b < text.len() {
        let cut = text[end..b]
            .iter()
            .rposition(|&c| c == '\n')
            .or_else(|| text[end..b].iter().rposition(|c| c.is_whitespace()));
        if let Some(p) = cut {
            b = end + p;
        }
    }
    let mut s = String::new();
    if a > 0 {
        s.push('…');
    }
    s.extend(&text[a..start]);
    s.push(OPEN);
    s.extend(&text[start..end]);
    s.push(CLOSE);
    s.extend(&text[end..b]);
    if b < text.len() {
        s.push('…');
    }
    s
}

fn one_line(s: &str, max: usize) -> String {
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= max {
        return flat;
    }
    let mut out: String = flat.chars().take(max).collect();
    out.push('…');
    out
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.trim().to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

fn source_label(src: &Source) -> String {
    match src {
        Source::Agent => "the assistant's answer".into(),
        Source::User => "the user's own message".into(),
        Source::Command(cmd) => format!("the output of the command `{}`", one_line(cmd, 80)),
    }
}

/// The request (user message) that led to item `idx`.
fn request_for(conv: &Conversation, idx: usize) -> Option<usize> {
    let turn = conv.items[idx].turn.as_ref();
    conv.items[..=idx]
        .iter()
        .rposition(|it| it.source == Source::User && (turn.is_none() || it.turn.as_ref() == turn))
        .or_else(|| {
            conv.items[..=idx]
                .iter()
                .rposition(|it| it.source == Source::User)
        })
}

/// Earliest mentions of the selected words outside the passage itself.
fn other_mentions(conv: &Conversation, sel_key: &[char], exclude: Option<usize>) -> Vec<String> {
    if sel_key.len() < 4 {
        return Vec::new();
    }
    let mut found = Vec::new();
    let mut total = 0;
    for (idx, item) in conv.items.iter().enumerate() {
        if Some(idx) == exclude || matches!(item.source, Source::Command(_)) {
            continue;
        }
        let chars: Vec<char> = item.text.chars().take(SEARCH_CAP).collect();
        let (ik, imap) = key_map(&chars);
        let hits = find_all(&ik, sel_key);
        total += hits.len();
        if let Some(&pos) = hits.first()
            && found.len() < MENTIONS
        {
            let (s, e) = (imap[pos], imap[pos + sel_key.len() - 1] + 1);
            found.push(format!(
                "in {}: {}",
                source_label(&item.source),
                one_line(
                    &marked_window(&chars, s, e, MENTION_SIDE),
                    2 * MENTION_SIDE + 40
                )
            ));
        }
    }
    // A word used everywhere is not a term being introduced.
    if total > 20 { Vec::new() } else { found }
}

pub struct Built {
    pub prompt: String,
    /// Short description of where the context came from, for the box header.
    pub label: &'static str,
}

/// Assemble the explainer's input.
pub fn build(cwd: &str, conv: Option<&Conversation>, hit: Option<Hit>, sel: &ScreenSel) -> Built {
    let selected = one_line(&sel.selected(), 300);
    let mut p = String::new();
    p.push_str(
        "<environment>\nThe user is working in Codex CLI, OpenAI's coding agent for the terminal, ",
    );
    p.push_str(&format!("in the directory {cwd}. Text on their screen is the assistant's answers, the user's messages, "));
    p.push_str("command output, or the Codex CLI interface itself (tips, status lines, prompts).\n</environment>\n\n");

    if let Some(conv) = conv {
        let current = hit.and_then(|h| request_for(conv, h.item));
        let upto = current
            .or_else(|| hit.map(|h| h.item))
            .unwrap_or(conv.items.len());
        let earlier: Vec<&ConvItem> = conv.items[..upto]
            .iter()
            .filter(|it| it.source == Source::User)
            .collect();
        let start = earlier.len().saturating_sub(EARLIER_REQUESTS);
        match &conv.title {
            Some(t) => p.push_str(&format!("<conversation title=\"{}\">\n", one_line(t, 100))),
            None => p.push_str("<conversation>\n"),
        }
        if !earlier.is_empty() {
            p.push_str("Earlier requests from the user, oldest first:\n");
            if start > 0 {
                p.push_str(&format!("- ({start} earlier requests omitted)\n"));
            }
            for it in &earlier[start..] {
                p.push_str(&format!("- {}\n", one_line(&it.text, EARLIER_REQUEST_MAX)));
            }
        }
        if let Some(c) = current {
            p.push_str(&format!(
                "The request this passage answers:\n{}\n",
                truncate(&conv.items[c].text, REQUEST_MAX)
            ));
        }
        p.push_str("</conversation>\n\n");
    }

    let label = match (conv, hit) {
        (Some(conv), Some(h)) => {
            let item = &conv.items[h.item];
            let chars: Vec<char> = item.text.chars().collect();
            p.push_str(&format!(
                "<passage source=\"{}\">\n",
                source_label(&item.source)
            ));
            p.push_str(&marked_window(&chars, h.start, h.end, PASSAGE_SIDE));
            p.push_str("\n</passage>\n");
            "conversation"
        }
        _ => {
            p.push_str("<passage source=\"the terminal screen; this text was not found in the conversation, so it is probably Codex CLI interface text or tool output\">\n");
            p.push_str(&marked_window(&sel.text, sel.start, sel.end, SCREEN_SIDE));
            p.push_str("\n</passage>\n");
            "screen"
        }
    };

    if let (Some(conv), Some((sk, ns, ne))) = (conv, screen_key(sel)) {
        let mentions = other_mentions(conv, &sk[ns..ne], hit.map(|h| h.item));
        if !mentions.is_empty() {
            p.push_str("\n<earlier_mentions>\n");
            for m in mentions {
                p.push_str(&format!("- {m}\n"));
            }
            p.push_str("</earlier_mentions>\n");
        }
    }

    p.push_str(&format!(
        "\nExplain {OPEN}{selected}{CLOSE} as it is used in the passage. If the passage or the conversation shows \
         which specific thing it refers to (a product, file, function, command, setting, concept), name it \
         concretely. Do not summarize the passage. If the context does not settle what it refers to, give the \
         most likely reading and say that it is a guess.\n"
    ));
    Built { prompt: p, label }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn screen(text: &str, selected: &str) -> ScreenSel {
        let chars: Vec<char> = text.chars().collect();
        let sel: Vec<char> = selected.chars().collect();
        let start = (0..=chars.len() - sel.len())
            .rev()
            .find(|&i| chars[i..i + sel.len()] == *sel)
            .unwrap();
        ScreenSel {
            text: chars,
            start,
            end: start + sel.len(),
        }
    }

    fn item(turn: &str, source: Source, text: &str) -> ConvItem {
        ConvItem {
            turn: Some(turn.into()),
            source,
            text: text.into(),
        }
    }

    #[test]
    fn neighbours_pick_the_right_occurrence() {
        let conv = Conversation {
            id: "t".into(),
            title: None,
            items: vec![
                item("1", Source::User, "compare the apps"),
                item("1", Source::Agent, "The Desktop app on Linux is new."),
                item("1", Source::Agent, "The Desktop app for Mac is older."),
            ],
        };
        let s = screen(
            "│ The Desktop app on Linux is new. │\n│ The Desktop app for Mac is older. │",
            "Desktop app",
        );
        // The *last* on-screen occurrence is the Mac one.
        assert_eq!(find(&conv, &s).unwrap().item, 2);
        let s = screen("• The Desktop app on Linux is new.", "Desktop app");
        let hit = find(&conv, &s).unwrap();
        assert_eq!(hit.item, 1);
        let text: String = conv.items[1]
            .text
            .chars()
            .skip(hit.start)
            .take(hit.end - hit.start)
            .collect();
        assert_eq!(text, "Desktop app");
    }

    #[test]
    fn markdown_and_wrapping_do_not_break_matching() {
        let conv = Conversation {
            id: "t".into(),
            title: None,
            items: vec![item(
                "1",
                Source::Agent,
                "Run **`peekme --help`** to see the\noptions, then `alias` it.",
            )],
        };
        let s = screen(
            "  Run peekme --help to see the\n  options, then alias it.",
            "peekme --help",
        );
        let hit = find(&conv, &s).unwrap();
        let text: String = conv.items[0]
            .text
            .chars()
            .skip(hit.start)
            .take(hit.end - hit.start)
            .collect();
        assert_eq!(text, "peekme --help");
    }

    #[test]
    fn interface_text_falls_back_to_the_screen_with_environment() {
        let tip = "  Tip: Try the Desktop app on Linux: install it from https://learn.chatgpt.com/docs/linux/linux-app and run 'chatgpt'.";
        let s = screen(tip, "Desktop app");
        let conv = Conversation {
            id: "t".into(),
            title: Some("Fix the build".into()),
            items: vec![item("1", Source::User, "fix the build")],
        };
        assert_eq!(find(&conv, &s), None);
        let b = build("/home/u/proj", Some(&conv), None, &s);
        assert_eq!(b.label, "screen");
        assert!(b.prompt.contains("Codex CLI, OpenAI's coding agent"));
        assert!(
            b.prompt.contains(
                "Try the ⟦Desktop app⟧ on Linux: install it from https://learn.chatgpt.com"
            )
        );
        assert!(b.prompt.contains("run 'chatgpt'"));
        assert!(b.prompt.contains("fix the build"));
    }

    #[test]
    fn prompt_has_request_outline_and_mentions_within_budget() {
        let long_answer = format!(
            "{} The shadow emulator keeps a copy of the screen. {}",
            "filler ".repeat(2000),
            "more ".repeat(2000)
        );
        let conv = Conversation {
            id: "t".into(),
            title: Some("peekme design".into()),
            items: vec![
                item("1", Source::User, "what is a shadow emulator?"),
                item(
                    "1",
                    Source::Agent,
                    "A shadow emulator is an in-memory terminal that mirrors the real one.",
                ),
                item(
                    "2",
                    Source::User,
                    &"explain the whole design please ".repeat(100),
                ),
                item("2", Source::Agent, &long_answer),
            ],
        };
        let s = screen(
            "The shadow emulator keeps a copy of the screen.",
            "shadow emulator",
        );
        let hit = find(&conv, &s).unwrap();
        assert_eq!(hit.item, 3);
        let b = build("/p", Some(&conv), Some(hit), &s);
        assert_eq!(b.label, "conversation");
        assert!(b.prompt.contains("The ⟦shadow emulator⟧ keeps a copy"));
        assert!(b.prompt.contains("- what is a shadow emulator?"));
        assert!(
            b.prompt.contains(
                "in the assistant's answer: A ⟦shadow emulator⟧ is an in-memory terminal"
            )
        );
        assert!(b.prompt.contains("The request this passage answers"));
        assert!(
            b.prompt.chars().count() < 7000,
            "prompt too long: {}",
            b.prompt.chars().count()
        );
    }
}
