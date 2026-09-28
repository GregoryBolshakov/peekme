//! What every explainer backend reports back, what it is asked, and which
//! backend explains for which agent.

use std::sync::Arc;

use crate::agent::Agent;
use crate::claude::explain::Pool;
use crate::codex::explain::{self as codex, Slot};
use crate::context::ScreenSel;

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
}

impl Explainer {
    pub fn new(agent: Agent) -> Self {
        match agent {
            Agent::Codex => Explainer::Codex(Arc::default()),
            Agent::Claude => Explainer::Claude(Arc::default()),
        }
    }

    /// For a program that is not an agent: whichever agent is installed,
    /// Codex first.
    pub fn for_other_program() -> Self {
        let codex = crate::launch::find_real("codex").is_some();
        let claude = crate::launch::find_real("claude").is_some();
        Self::new(if claude && !codex {
            Agent::Claude
        } else {
            Agent::Codex
        })
    }

    /// Run one explanation, reporting progress on `tx`. Blocks; call from a thread.
    pub fn explain(&self, req: Request, tx: impl Fn(Progress)) {
        match self {
            Explainer::Codex(slot) => match slot.get() {
                Ok(server) => codex::explain(&server, req, tx),
                Err(e) => tx(Progress::Failed(format!("explainer unavailable: {e:#}"))),
            },
            Explainer::Claude(pool) => crate::claude::explain::explain(pool, req, tx),
        }
    }

    pub fn shutdown(&self) {
        match self {
            Explainer::Codex(slot) => slot.shutdown(),
            Explainer::Claude(pool) => pool.shutdown(),
        }
    }
}
