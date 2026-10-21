//! The colour of peekme's boxes: by default the agent's own accent (Codex's
//! cyan, Claude Code's orange, Copilot's and Kiro's purples, Pi's magenta), or what
//! `PEEKME_COLOR` (all agents) or `PEEKME_<AGENT>_COLOR` (one) says.
//!
//! A value is a hex colour (`#d77757`), a colour of the terminal's palette
//! by name (`cyan`, `bright-magenta`) or a 256-colour index (`173`).
//! Without a terminal that says it shows 24-bit colour (`COLORTERM`), hex
//! colours become the nearest of the 256.

use crate::agent::Agent;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Color {
    /// One of the terminal's 16 palette colours.
    Palette(u8),
    /// An index of the 256-colour palette.
    Index(u8),
    Rgb(u8, u8, u8),
}

const NAMES: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];

/// The agent's own accent.
pub fn default_for(agent: Option<Agent>) -> Color {
    match agent {
        // Codex draws its accents in the palette's cyan.
        Some(Agent::Codex) | None => Color::Palette(6),
        // Claude Code's "claude" orange: its logo, spinner and prompt.
        Some(Agent::Claude) => Color::Rgb(215, 119, 87),
        // Copilot's purple, lightened to read on a dark screen (GitHub's
        // own purple for text there).
        Some(Agent::Copilot) => Color::Rgb(171, 125, 248),
        // Kiro's purple: its logo, agent name and links (2.28).
        Some(Agent::Kiro) => Color::Rgb(193, 154, 255),
        // Pi's accent in its default theme ("system", the terminal's own
        // palette): magenta (1.3).
        Some(Agent::Pi) => Color::Palette(5),
    }
}

/// The environment variable for one agent.
pub fn agent_var(agent: Agent) -> &'static str {
    match agent {
        Agent::Claude => "PEEKME_CLAUDE_COLOR",
        Agent::Codex => "PEEKME_CODEX_COLOR",
        Agent::Copilot => "PEEKME_COPILOT_COLOR",
        Agent::Kiro => "PEEKME_KIRO_COLOR",
        Agent::Pi => "PEEKME_PI_COLOR",
    }
}

pub fn parse(s: &str) -> Option<Color> {
    let s = s.trim().to_ascii_lowercase();
    let hex = s.strip_prefix('#').unwrap_or(&s);
    if hex.len() == 6 && hex.chars().all(|c| c.is_ascii_hexdigit()) {
        let v = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
        return Some(Color::Rgb(v(0)?, v(2)?, v(4)?));
    }
    if let Ok(n) = s.parse::<u8>() {
        return Some(Color::Index(n));
    }
    let (bright, name) = match s
        .strip_prefix("bright-")
        .or_else(|| s.strip_prefix("bright"))
    {
        Some(n) => (true, n.trim_start_matches(['-', '_', ' '])),
        None => (false, s.as_str()),
    };
    let i = NAMES.iter().position(|n| *n == name)? as u8;
    Some(Color::Palette(i + if bright { 8 } else { 0 }))
}

/// The SGR parameters that set `c` as the text colour.
pub fn sgr(c: Color, truecolor: bool) -> String {
    match c {
        Color::Palette(i) if i < 8 => format!("{}", 30 + i),
        Color::Palette(i) => format!("{}", 90 + (i - 8)),
        Color::Index(i) => format!("38;5;{i}"),
        Color::Rgb(r, g, b) if truecolor => format!("38;2;{r};{g};{b}"),
        Color::Rgb(r, g, b) => format!("38;5;{}", nearest_256(r, g, b)),
    }
}

/// The terminal says it shows 24-bit colour.
pub fn truecolor() -> bool {
    std::env::var("COLORTERM").is_ok_and(|v| {
        let v = v.to_ascii_lowercase();
        v == "truecolor" || v == "24bit"
    })
}

/// The box colour for this run, `get` reading the environment; with a
/// value that is not a colour, the default and what to log about it.
pub fn for_agent(
    agent: Option<Agent>,
    get: impl Fn(&str) -> Option<String>,
) -> (Color, Option<String>) {
    let var = agent.map(agent_var);
    let set = var
        .and_then(|v| get(v).map(|x| (v, x)))
        .or_else(|| get("PEEKME_COLOR").map(|x| ("PEEKME_COLOR", x)))
        .filter(|(_, x)| !x.trim().is_empty());
    match set {
        Some((var, value)) => match parse(&value) {
            Some(c) => (c, None),
            None => (
                default_for(agent),
                Some(format!(
                    "{var}={value:?} is not a colour, using the default"
                )),
            ),
        },
        None => (default_for(agent), None),
    }
}

/// The xterm 256-colour index nearest to an RGB colour: the 6x6x6 cube or
/// the grey ramp.
fn nearest_256(r: u8, g: u8, b: u8) -> u8 {
    const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
    let step = |v: u8| {
        (0..6)
            .min_by_key(|&i| (LEVELS[i] as i32 - v as i32).abs())
            .unwrap_or(0)
    };
    let (ri, gi, bi) = (step(r), step(g), step(b));
    let cube = (LEVELS[ri], LEVELS[gi], LEVELS[bi]);
    let avg = (r as u32 + g as u32 + b as u32) / 3;
    let gi_ramp = ((avg.saturating_sub(8)) / 10).min(23) as u8;
    let grey = 8 + 10 * gi_ramp;
    let dist = |(x, y, z): (u8, u8, u8)| {
        let d = |a: u8, b: u8| (a as i32 - b as i32).pow(2);
        d(x, r) + d(y, g) + d(z, b)
    };
    if dist((grey, grey, grey)) < dist(cube) {
        232 + gi_ramp
    } else {
        16 + 36 * ri as u8 + 6 * gi as u8 + bi as u8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values() {
        assert_eq!(parse("#D77757"), Some(Color::Rgb(215, 119, 87)));
        assert_eq!(parse("d77757"), Some(Color::Rgb(215, 119, 87)));
        assert_eq!(parse("cyan"), Some(Color::Palette(6)));
        assert_eq!(parse("Bright-Magenta"), Some(Color::Palette(13)));
        assert_eq!(parse("brightblue"), Some(Color::Palette(12)));
        assert_eq!(parse("173"), Some(Color::Index(173)));
        assert_eq!(parse("orangeish"), None);
        assert_eq!(parse("#12345"), None);
    }

    #[test]
    fn escapes() {
        assert_eq!(sgr(Color::Palette(6), true), "36");
        assert_eq!(sgr(Color::Palette(13), false), "95");
        assert_eq!(sgr(Color::Index(173), false), "38;5;173");
        assert_eq!(sgr(Color::Rgb(215, 119, 87), true), "38;2;215;119;87");
        // Claude's orange is 173 (#d7875f) of the 256.
        assert_eq!(sgr(Color::Rgb(215, 119, 87), false), "38;5;173");
        assert_eq!(nearest_256(128, 128, 128), 244);
        assert_eq!(nearest_256(255, 128, 255), 213);
    }

    #[test]
    fn the_agents_variable_wins_over_the_common_one() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |k: &str| {
                pairs
                    .iter()
                    .find(|(n, _)| *n == k)
                    .map(|(_, v)| v.to_string())
            }
        };
        let both = env(&[("PEEKME_COLOR", "green"), ("PEEKME_KIRO_COLOR", "#102030")]);
        assert_eq!(for_agent(Some(Agent::Kiro), both).0, Color::Rgb(16, 32, 48));
        assert_eq!(for_agent(Some(Agent::Codex), both).0, Color::Palette(2));
        assert_eq!(
            for_agent(Some(Agent::Claude), env(&[])),
            (Color::Rgb(215, 119, 87), None)
        );
        // Not a colour: the agent's own, and a line for the log.
        let (c, warn) = for_agent(Some(Agent::Codex), env(&[("PEEKME_COLOR", "nope")]));
        assert_eq!(c, Color::Palette(6));
        assert!(warn.is_some_and(|w| w.starts_with("PEEKME_COLOR=\"nope\"")));
    }
}
