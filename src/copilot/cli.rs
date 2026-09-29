//! Which `copilot` invocations open Copilot CLI's interactive screen.

/// Flags that run without the interactive screen: prompt mode, servers,
/// version and help.
const BATCH_FLAGS: &[&str] = &[
    "-p",
    "--prompt",
    "--acp",
    "--server",
    "--headless",
    "-v",
    "--version",
    "-h",
    "--help",
];

/// Subcommands (Copilot CLI 1.0.89). None of them opens the chat screen.
const SUBCOMMANDS: &[&str] = &[
    "app",
    "login",
    "help",
    "init",
    "update",
    "version",
    "workflow",
    "workflows",
    "sessions",
    "memories",
    "plugin",
    "mcp",
    "skill",
    "instruction",
    "lsp",
    "completion",
];

/// True when `copilot <args>` opens the chat screen: no subcommand, `-i`,
/// `--resume`, `--continue` and the like.
pub fn is_interactive(args: &[String]) -> bool {
    let batch_flag = |a: &String| {
        BATCH_FLAGS.contains(&a.as_str()) || a.starts_with("--prompt=") || a.starts_with("-p=")
    };
    if args.iter().any(batch_flag) {
        return false;
    }
    !args
        .first()
        .is_some_and(|a| SUBCOMMANDS.contains(&a.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn interactive_or_not() {
        assert!(is_interactive(&v(&[])));
        assert!(is_interactive(&v(&["-i", "fix the build"])));
        assert!(is_interactive(&v(&["--resume"])));
        assert!(is_interactive(&v(&["--continue", "--model", "auto"])));
        assert!(!is_interactive(&v(&["-p", "hi", "--allow-all-tools"])));
        assert!(!is_interactive(&v(&["--prompt=hi"])));
        assert!(!is_interactive(&v(&["--headless", "--stdio"])));
        assert!(!is_interactive(&v(&["--acp"])));
        assert!(!is_interactive(&v(&["login"])));
        assert!(!is_interactive(&v(&["mcp", "list"])));
    }
}
