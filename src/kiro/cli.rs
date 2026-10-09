//! Which `kiro-cli` invocations open Kiro CLI's interactive screen.

/// Flags that print and exit, or run without the interactive screen.
const BATCH_FLAGS: &[&str] = &[
    "--no-interactive",
    "--output-format",
    "-l",
    "--list-sessions",
    "--list-models",
    "-d",
    "--delete-session",
    "--sessions",
    "-V",
    "--version",
    "-h",
    "--help",
    "--help-all",
];

/// Options that take a value as the next argument (Kiro CLI 2.28), so the
/// value is not taken for a subcommand.
const WITH_VALUE: &[&str] = &[
    "--agent",
    "--model",
    "--effort",
    "--resume-id",
    "--repo",
    "--trust-tools",
    "-f",
    "--format",
    "--session-source",
];

/// True when `kiro-cli <args>` opens the chat screen: no subcommand or
/// `chat` (maybe with a first question), `--resume` and the like. Every other
/// subcommand (`acp`, `login`, `agent`, `crew`, ...) runs directly.
pub fn is_interactive(args: &[String]) -> bool {
    let batch = |a: &String| {
        BATCH_FLAGS.contains(&a.as_str())
            || a.starts_with("--output-format=")
            || a.starts_with("--delete-session=")
    };
    if args.iter().any(batch) {
        return false;
    }
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if WITH_VALUE.contains(&a.as_str()) {
            it.next();
        } else if !a.starts_with('-') {
            return a == "chat";
        }
    }
    true
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
        assert!(is_interactive(&v(&["chat"])));
        assert!(is_interactive(&v(&["chat", "fix the build"])));
        assert!(is_interactive(&v(&["--resume"])));
        assert!(is_interactive(&v(&["--agent", "acp", "--v3"])));
        assert!(is_interactive(&v(&["chat", "--resume-id", "login"])));
        assert!(is_interactive(&v(&["--tui", "-v"])));
        assert!(!is_interactive(&v(&["chat", "--no-interactive", "hi"])));
        assert!(!is_interactive(&v(&[
            "chat",
            "--output-format=stream-json"
        ])));
        assert!(!is_interactive(&v(&[
            "chat",
            "--list-models",
            "-f",
            "json"
        ])));
        assert!(!is_interactive(&v(&["chat", "-d", "1234"])));
        assert!(!is_interactive(&v(&["acp"])));
        assert!(!is_interactive(&v(&["login"])));
        assert!(!is_interactive(&v(&["--verbose", "mcp", "list"])));
        assert!(!is_interactive(&v(&["-V"])));
    }
}
