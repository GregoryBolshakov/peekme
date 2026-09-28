//! The agent CLIs peekme knows. What differs between them is reached through
//! here: the command users type, which invocations open an interactive
//! screen, and how the explainer should describe the screen.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Codex,
    Claude,
}

impl Agent {
    pub const ALL: [Agent; 2] = [Agent::Codex, Agent::Claude];

    /// The command users type.
    pub fn command(self) -> &'static str {
        match self {
            Agent::Codex => "codex",
            Agent::Claude => "claude",
        }
    }

    /// Product name, for messages.
    pub fn name(self) -> &'static str {
        match self {
            Agent::Codex => "Codex",
            Agent::Claude => "Claude Code",
        }
    }

    /// Short name, for "Claude: 3 updates waiting".
    pub fn short(self) -> &'static str {
        match self {
            Agent::Codex => "Codex",
            Agent::Claude => "Claude",
        }
    }

    pub fn install_hint(self) -> &'static str {
        match self {
            Agent::Codex => "npm i -g @openai/codex",
            Agent::Claude => "npm i -g @anthropic-ai/claude-code",
        }
    }

    /// The agent a program path runs, by its file name.
    pub fn from_program(program: &str) -> Option<Agent> {
        let name = Path::new(program).file_name()?.to_str()?;
        Agent::ALL.into_iter().find(|a| a.command() == name)
    }

    /// Whether `<command> <args>` opens the agent's interactive screen, where
    /// peeking helps. Everything else runs the agent directly.
    pub fn is_interactive(self, args: &[String]) -> bool {
        match self {
            Agent::Codex => crate::codex::cli::is_interactive(args),
            Agent::Claude => crate::claude::cli::is_interactive(args),
        }
    }

    /// One paragraph telling the explainer what the screen is.
    pub fn environment(self, cwd: &str) -> String {
        let (who, what) = match self {
            Agent::Codex => (
                "Codex CLI, OpenAI's coding agent for the terminal",
                "command output, or the Codex CLI interface itself",
            ),
            Agent::Claude => (
                "Claude Code, Anthropic's coding agent for the terminal",
                "tool calls and their output, or the Claude Code interface itself",
            ),
        };
        format!(
            "<environment>\nThe user is working in {who}, in the directory {cwd}. Text on their \
             screen is the assistant's answers, the user's messages, {what} (tips, status lines, \
             prompts).\n</environment>\n\n"
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_from_program() {
        assert_eq!(Agent::from_program("codex"), Some(Agent::Codex));
        assert_eq!(Agent::from_program("/usr/bin/claude"), Some(Agent::Claude));
        assert_eq!(Agent::from_program("htop"), None);
    }
}
