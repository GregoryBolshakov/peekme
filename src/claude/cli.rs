//! Which `claude` invocations open Claude Code's interactive screen.

/// Flags that print and exit, or run without the interactive screen.
const BATCH_FLAGS: &[&str] = &[
    "-p",
    "--print",
    "-v",
    "--version",
    "-h",
    "--help",
    "--bg",
    "--background",
];

/// Subcommands (Claude Code 2.1.283). None of them opens the chat screen.
const SUBCOMMANDS: &[&str] = &[
    "agents",
    "attach",
    "auth",
    "auto-mode",
    "doctor",
    "gateway",
    "import",
    "install",
    "logs",
    "mcp",
    "plugin",
    "plugins",
    "project",
    "respawn",
    "rm",
    "setup-token",
    "stop",
    "kill",
    "ultrareview",
    "update",
    "upgrade",
];

/// True when `claude <args>` opens the chat screen: no subcommand (maybe a
/// prompt), `--resume`, `--continue` and the like.
pub fn is_interactive(args: &[String]) -> bool {
    if args.iter().any(|a| BATCH_FLAGS.contains(&a.as_str())) {
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
        assert!(is_interactive(&v(&["fix the build"])));
        assert!(is_interactive(&v(&["--resume"])));
        assert!(is_interactive(&v(&["-c", "--model", "opus"])));
        assert!(!is_interactive(&v(&["-p", "hi"])));
        assert!(!is_interactive(&v(&["--model", "haiku", "--print", "hi"])));
        assert!(!is_interactive(&v(&["mcp", "list"])));
        assert!(!is_interactive(&v(&["--version"])));
    }
}
