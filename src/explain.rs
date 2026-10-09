//! What every explainer backend reports back, what it is asked, and which
//! backend explains for which agent.

use std::sync::Arc;

use crate::agent::Agent;
use crate::claude::explain::Pool;
use crate::codex::explain::{self as codex, Slot};
use crate::context::{Nested, ScreenSel};
use crate::copilot::explain as copilot;
use crate::kiro::explain as kiro;

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
    /// A question the user typed about the selection (Alt+Shift+P): answered
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
