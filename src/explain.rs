//! What every explainer backend reports back, what it is asked, and which
//! backend explains for which agent.

use std::sync::Arc;

use crate::agent::Agent;
use crate::claude::explain::Pool;
use crate::codex::explain::{self as codex, Slot};
use crate::context::{Nested, ScreenSel};
use crate::copilot::explain as copilot;
use crate::kiro::explain as kiro;

/// Use the request's override, then the current chat model. With neither,
/// leave the choice to the agent's configured default.
pub fn model(agent: Agent, question: bool, chat: Option<&str>) -> Option<String> {
    let (peek, ask) = match agent {
        Agent::Claude => ("PEEKME_CLAUDE_MODEL", "PEEKME_CLAUDE_ASK_MODEL"),
        Agent::Codex => ("PEEKME_MODEL", "PEEKME_CODEX_ASK_MODEL"),
        Agent::Copilot => ("PEEKME_COPILOT_MODEL", "PEEKME_COPILOT_ASK_MODEL"),
        Agent::Kiro => ("PEEKME_KIRO_MODEL", "PEEKME_KIRO_ASK_MODEL"),
    };
    configured_model(
        std::env::var(if question { ask } else { peek })
            .ok()
            .as_deref(),
        chat,
    )
}

fn configured_model(override_model: Option<&str>, chat: Option<&str>) -> Option<String> {
    override_model
        .filter(|m| !m.trim().is_empty())
        .or_else(|| chat.filter(|m| !m.trim().is_empty()))
        .map(|m| m.trim().to_string())
}

/// Streamed progress of one explanation.
#[derive(Debug)]
pub enum Progress {
    /// Model name and where the context came from.
    Started {
        model: String,
        source: &'static str,
    },
    Delta(String),
    Done,
    Failed(String),
    /// The whole chat is much bigger than a normal explanation; ask first.
    TooBig {
        tokens: usize,
        ratio: usize,
    },
}

/// Joins the text of the messages of one answer. A model can write a
/// sentence, think or call a tool, then write the rest as a new message:
/// the messages get an empty line between them instead of running together.
#[derive(Default)]
pub struct Parts {
    /// The message the last text was from.
    last: Option<String>,
    /// Newlines at the end of the text so far.
    tail: usize,
}

impl Parts {
    /// `delta` of message `id`, with a break in front when it starts a new
    /// message after text of another one. None for empty text.
    pub fn text(&mut self, id: &str, delta: &str) -> Option<String> {
        if delta.is_empty() {
            return None;
        }
        let new = self.last.as_deref().is_some_and(|l| l != id);
        self.last = Some(id.to_string());
        let out = if new {
            let lead = delta.len() - delta.trim_start_matches('\n').len();
            let breaks = 2usize.saturating_sub(self.tail + lead);
            format!("{}{delta}", "\n".repeat(breaks))
        } else {
            delta.to_string()
        };
        let trailing = out.len() - out.trim_end_matches('\n').len();
        self.tail = if trailing == out.len() {
            self.tail + trailing
        } else {
            trailing
        };
        Some(out)
    }
}

pub struct Request {
    /// The screen text with the selected range.
    pub screen: ScreenSel,
    pub cwd: String,
    pub max_lines: usize,
    /// Explain with the whole conversation (a forked thread) instead of the
    /// compact context.
    pub deep: bool,
    /// Send the whole conversation even when it is big.
    pub force: bool,
    /// Process id of the agent, for agents whose session is found by it.
    pub agent_pid: Option<u32>,
    /// Words of an earlier answer, when the peek is inside another one.
    pub nested: Option<Nested>,
    /// A question the user typed about the selection (Alt+Q): answered
    /// by the chat's own model, with the whole conversation.
    pub question: Option<String>,
}

/// Above this estimate, "the whole chat" asks before sending (a normal
/// explanation is about 9k tokens with Codex's own overhead).
pub const DEEP_ASK_ABOVE_TOKENS: usize = 40_000;

/// The explainer of a session: each agent explains through its own CLI and
/// login. Nothing is started until the first explanation.
#[derive(Clone)]
pub enum Explainer {
    /// Codex's app-server (its shared one, or our own).
    Codex(Arc<Slot>),
    /// `claude -p`, one process per explanation.
    Claude(Arc<Pool>),
    /// Copilot CLI's headless server, one session per explanation.
    Copilot(Arc<copilot::Slot>),
    /// Kiro CLI's ACP server, one session per explanation.
    Kiro(Arc<kiro::Slot>),
    /// Canned text, no model: `PEEKME_FAKE_EXPLAINER=1`, for end-to-end tests.
    Fake,
}

impl Explainer {
    /// The agent's explainer, or the canned one when `PEEKME_FAKE_EXPLAINER` is set.
    pub fn for_agent(agent: Option<Agent>) -> Self {
        if std::env::var_os("PEEKME_FAKE_EXPLAINER").is_some() {
            return Explainer::Fake;
        }
        match agent {
            Some(a) => Self::new(a),
            None => Self::for_other_program(),
        }
    }

    pub fn new(agent: Agent) -> Self {
        match agent {
            Agent::Codex => Explainer::Codex(Arc::default()),
            Agent::Claude => Explainer::Claude(Arc::default()),
            Agent::Copilot => Explainer::Copilot(Arc::default()),
            Agent::Kiro => Explainer::Kiro(Arc::default()),
        }
    }

    /// For a program that is not an agent: the first agent that is installed.
    pub fn for_other_program() -> Self {
        let installed = Agent::ALL
            .into_iter()
            .find(|a| crate::launch::find_real(a.command()).is_some());
        Self::new(installed.unwrap_or(Agent::Claude))
    }

    /// Run one explanation, reporting progress on `tx`. Blocks; call from a thread.
    pub fn explain(&self, req: Request, tx: impl Fn(Progress)) {
        match self {
            Explainer::Codex(slot) => match slot.get() {
                Ok(server) => codex::explain(&server, req, tx),
                Err(e) => tx(Progress::Failed(format!("explainer unavailable: {e:#}"))),
            },
            Explainer::Claude(pool) => crate::claude::explain::explain(pool, req, tx),
            Explainer::Copilot(slot) => copilot::explain(slot, req, tx),
            Explainer::Kiro(slot) => kiro::explain(slot, req, tx),
            Explainer::Fake => {
                let sel = match &req.nested {
                    Some(n) => {
                        let parent = n.trail.last().map_or("", |(w, _)| w.as_str());
                        format!("{}⟧ in ⟦{parent}", n.words())
                    }
                    None => req.screen.selected(),
                };
                if let Some(q) = &req.question {
                    tx(Progress::Started {
                        model: "test-ask".into(),
                        source: "whole conversation",
                    });
                    for part in [
                        "Test answer ",
                        &format!("to ⟦{q}⟧ "),
                        &format!("about ⟦{sel}⟧."),
                    ] {
                        tx(Progress::Delta(part.to_string()));
                    }
                    tx(Progress::Done);
                    return;
                }
                tx(Progress::Started {
                    model: "test".into(),
                    source: if req.deep {
                        "whole conversation"
                    } else {
                        "test"
                    },
                });
                for part in ["Test explanation ", "of ", &format!("⟦{sel}⟧.")] {
                    tx(Progress::Delta(part.to_string()));
                }
                // A taller answer for end-to-end runs, line by line as slowly
                // as `PEEKME_FAKE_DELAY_MS` says. `@file` reads "lines [delay]"
                // from a file at each request.
                let mut more = std::env::var("PEEKME_FAKE_LINES").ok();
                let mut delay = std::env::var("PEEKME_FAKE_DELAY_MS").ok();
                if let Some(path) = more
                    .clone()
                    .and_then(|m| m.strip_prefix('@').map(String::from))
                {
                    let text = std::fs::read_to_string(path).unwrap_or_default();
                    let mut words = text.split_whitespace().map(String::from);
                    (more, delay) = (words.next(), words.next().or(delay));
                }
                let delay = delay.and_then(|n| n.parse().ok()).unwrap_or(0);
                for k in 1..=more.and_then(|n| n.parse::<usize>().ok()).unwrap_or(0) {
                    std::thread::sleep(std::time::Duration::from_millis(delay));
                    tx(Progress::Delta(format!(
                        "\n\n- Line {k} of the test answer."
                    )));
                }
                tx(Progress::Done);
            }
        }
    }

    pub fn shutdown(&self) {
        match self {
            Explainer::Codex(slot) => slot.shutdown(),
            Explainer::Claude(pool) => pool.shutdown(),
            Explainer::Copilot(slot) => slot.shutdown(),
            Explainer::Kiro(slot) => slot.shutdown(),
            Explainer::Fake => {}
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn model_overrides_win_and_empty_settings_follow_the_chat() {
        use super::configured_model;
        assert_eq!(
            configured_model(Some(" custom "), Some("chat")).as_deref(),
            Some("custom")
        );
        for setting in [None, Some(""), Some("  ")] {
            assert_eq!(
                configured_model(setting, Some("first")).as_deref(),
                Some("first")
            );
            assert_eq!(
                configured_model(setting, Some("second")).as_deref(),
                Some("second")
            );
            assert_eq!(configured_model(setting, None), None);
        }
        assert_eq!(
            configured_model(Some("custom"), None).as_deref(),
            Some("custom")
        );
        assert_eq!(configured_model(None, Some(" ")), None);
    }

    use super::*;

    #[test]
    fn messages_of_one_answer_get_an_empty_line_between_them() {
        let mut p = Parts::default();
        let mut all = String::new();
        for (id, d) in [
            ("a", "First"),
            ("a", " sentence."),
            ("b", "Then"),
            ("b", " more."),
        ] {
            all.push_str(&p.text(id, d).unwrap());
        }
        assert_eq!(all, "First sentence.\n\nThen more.");
        // Newlines already there count.
        let mut p = Parts::default();
        let a = p.text("a", "One.\n").unwrap();
        let b = p.text("b", "\nTwo.").unwrap();
        assert_eq!(a + &b, "One.\n\nTwo.");
        assert_eq!(p.text("b", ""), None);
    }
}
