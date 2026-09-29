//! The agent CLIs peekme knows. What differs between them is reached through
//! here: the command users type, which invocations open an interactive
//! screen, and how the explainer should describe the screen.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    Copilot,
}

impl Agent {
    pub const ALL: [Agent; 3] = [Agent::Claude, Agent::Codex, Agent::Copilot];

    /// The command users type.
    pub fn command(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Copilot => "copilot",
        }
    }

    /// Product name, for messages.
    pub fn name(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
            Agent::Copilot => "GitHub Copilot CLI",
        }
    }

    /// Short name, for "Claude: 3 updates waiting".
    pub fn short(self) -> &'static str {
        match self {
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
            Agent::Copilot => "Copilot",
        }
    }

    pub fn install_hint(self) -> &'static str {
        match self {
            Agent::Claude => "npm i -g @anthropic-ai/claude-code",
            Agent::Codex => "npm i -g @openai/codex",
            Agent::Copilot => "npm i -g @github/copilot",
        }
    }

    /// "Claude Code, Codex and GitHub Copilot CLI", for messages.
    pub fn all_names() -> String {
        list(Agent::ALL.iter().map(|a| a.name().to_string()).collect())
    }

    /// "`claude`, `codex` or `copilot`", for messages.
    pub fn all_commands(last: &str) -> String {
        let mut v: Vec<String> = Agent::ALL
            .iter()
            .map(|a| format!("`{}`", a.command()))
            .collect();
        let tail = v.pop().unwrap_or_default();
        format!("{} {last} {tail}", v.join(", "))
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
            Agent::Claude => crate::claude::cli::is_interactive(args),
            Agent::Codex => crate::codex::cli::is_interactive(args),
            Agent::Copilot => crate::copilot::cli::is_interactive(args),
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
            Agent::Copilot => (
                "GitHub Copilot CLI, GitHub's coding agent for the terminal",
                "tool calls and their output, or the Copilot CLI interface itself",
            ),
        };
        format!(
            "<environment>\nThe user is working in {who}, in the directory {cwd}. Text on their \
             screen is the assistant's answers, the user's messages, {what} (tips, status lines, \
             prompts).\n</environment>\n\n"
        )
    }
}

fn list(mut v: Vec<String>) -> String {
    let last = v.pop().unwrap_or_default();
    if v.is_empty() {
        last
    } else {
        format!("{} and {last}", v.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_from_program() {
        assert_eq!(Agent::from_program("codex"), Some(Agent::Codex));
        assert_eq!(Agent::from_program("/usr/bin/claude"), Some(Agent::Claude));
        assert_eq!(Agent::from_program("copilot"), Some(Agent::Copilot));
        assert_eq!(Agent::from_program("htop"), None);
        assert_eq!(
            Agent::all_names(),
            "Claude Code, Codex and GitHub Copilot CLI"
        );
        assert_eq!(Agent::all_commands("or"), "`claude`, `codex` or `copilot`");
    }
}
