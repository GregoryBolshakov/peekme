//! The agent CLIs peekme knows. What differs between them is reached through
//! here: the command users type, which invocations open an interactive
//! screen, and how the explainer should describe the screen.

use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    Pi,
    Copilot,
    Kiro,
}

impl Agent {
    pub const ALL: [Agent; 5] = [
        Agent::Claude,
        Agent::Codex,
        Agent::Pi,
        Agent::Copilot,
        Agent::Kiro,
    ];

    /// The command users type.
    pub fn command(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
            Agent::Copilot => "copilot",
            Agent::Kiro => "kiro-cli",
            Agent::Pi => "pi",
        }
    }

    /// Product name, for messages.
    pub fn name(self) -> &'static str {
        match self {
            Agent::Claude => "Claude Code",
            Agent::Codex => "Codex",
            Agent::Copilot => "GitHub Copilot CLI",
            Agent::Kiro => "Kiro CLI",
            Agent::Pi => "Pi",
        }
    }

    /// Short name, for "Claude: 3 updates waiting".
    pub fn short(self) -> &'static str {
        match self {
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
            Agent::Copilot => "Copilot",
            Agent::Kiro => "Kiro",
            Agent::Pi => "Pi",
        }
    }

    pub fn install_hint(self) -> &'static str {
        match self {
            Agent::Claude => "npm i -g @anthropic-ai/claude-code",
            Agent::Codex => "npm i -g @openai/codex",
            Agent::Copilot => "npm i -g @github/copilot",
            Agent::Kiro => "curl -fsSL https://cli.kiro.dev/install | bash",
            Agent::Pi => "curl -fsSL https://pi.dev/install.sh | sh",
        }
    }

    /// "Claude Code, Codex, Pi, GitHub Copilot CLI and Kiro CLI", for messages.
    pub fn all_names() -> String {
        list(Agent::ALL.iter().map(|a| a.name().to_string()).collect())
    }

    /// "`claude`, `codex`, `pi`, `copilot` or `kiro-cli`", for messages.
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

    /// The agent's binary on PATH: the first `command()` that is not peekme,
    /// when it is this agent and not another program of the same name.
    pub fn find(self) -> Option<PathBuf> {
        crate::launch::find_real(self.command()).filter(|p| self.is_agent_binary(p))
    }

    /// Whether `path` runs this agent. `pi` is also the name of a Debian
    /// package that prints the digits of pi (a compiled binary); the Pi
    /// agent is a script for Node (npm, its installer, Nix's wrapper).
    pub fn is_agent_binary(self, path: &Path) -> bool {
        if self != Agent::Pi {
            return true;
        }
        let mut head = [0u8; 4];
        let read = std::fs::File::open(path)
            .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut head));
        read.is_ok_and(|()| !is_native_binary(head))
    }

    /// Whether `<command> <args>` opens the agent's interactive screen, where
    /// peeking helps. Everything else runs the agent directly.
    pub fn is_interactive(self, args: &[String]) -> bool {
        match self {
            Agent::Claude => crate::claude::cli::is_interactive(args),
            Agent::Codex => crate::codex::cli::is_interactive(args),
            Agent::Copilot => crate::copilot::cli::is_interactive(args),
            Agent::Kiro => crate::kiro::cli::is_interactive(args),
            Agent::Pi => crate::pi::cli::is_interactive(args),
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
            Agent::Kiro => (
                "Kiro CLI, Amazon's coding agent for the terminal",
                "tool calls and their output, or the Kiro CLI interface itself",
            ),
            Agent::Pi => (
                "Pi, a coding agent for the terminal",
                "tool calls and their output, or the Pi interface itself",
            ),
        };
        format!(
            "<environment>\nThe user is working in {who}, in the directory {cwd}. Text on their \
             screen is the assistant's answers, the user's messages, {what} (tips, status lines, \
             prompts).\n</environment>\n\n"
        )
    }
}

/// ELF (Linux) or Mach-O (macOS, also universal) magic bytes.
fn is_native_binary(head: [u8; 4]) -> bool {
    const MAGIC: [[u8; 4]; 6] = [
        [0x7f, b'E', b'L', b'F'],
        [0xfe, 0xed, 0xfa, 0xce],
        [0xfe, 0xed, 0xfa, 0xcf],
        [0xce, 0xfa, 0xed, 0xfe],
        [0xcf, 0xfa, 0xed, 0xfe],
        [0xca, 0xfe, 0xba, 0xbe],
    ];
    MAGIC.contains(&head)
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
        assert_eq!(
            Agent::from_program("/home/u/.local/bin/kiro-cli"),
            Some(Agent::Kiro)
        );
        assert_eq!(Agent::from_program("/usr/local/bin/pi"), Some(Agent::Pi));
        assert_eq!(Agent::from_program("htop"), None);
        assert_eq!(
            Agent::all_names(),
            "Claude Code, Codex, Pi, GitHub Copilot CLI and Kiro CLI"
        );
        assert_eq!(
            Agent::all_commands("or"),
            "`claude`, `codex`, `pi`, `copilot` or `kiro-cli`"
        );
    }

    /// Pi's npm launcher is a Node script; the Debian `pi` (digits of pi)
    /// is a compiled binary and is left alone.
    #[test]
    fn pi_is_a_script_not_a_binary() {
        let dir = std::env::temp_dir().join(format!("peekme-pi-bin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("pi-script");
        std::fs::write(&script, "#!/usr/bin/env node\nimport './cli.js';\n").unwrap();
        let elf = dir.join("pi-elf");
        std::fs::write(&elf, b"\x7fELF\x02\x01\x01\x00rest").unwrap();
        let macho = dir.join("pi-macho");
        std::fs::write(&macho, [0xcf, 0xfa, 0xed, 0xfe, 7, 0, 0, 1]).unwrap();
        assert!(Agent::Pi.is_agent_binary(&script));
        assert!(!Agent::Pi.is_agent_binary(&elf));
        assert!(!Agent::Pi.is_agent_binary(&macho));
        assert!(!Agent::Pi.is_agent_binary(&dir.join("missing")));
        // Other agents are compiled binaries themselves (Codex, Kiro).
        assert!(Agent::Codex.is_agent_binary(&elf));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
