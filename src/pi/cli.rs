//! Which `pi` invocations open Pi's interactive screen.

/// Flags that print and exit, or run without the interactive screen.
const BATCH_FLAGS: &[&str] = &[
    "-p",
    "--print",
    "--export",
    "--list-models",
    "-h",
    "--help",
    "-v",
    "--version",
];

/// Commands that manage Pi and exit (Pi 1.3): `pi config` has a screen of
/// its own, not the chat.
const SUBCOMMANDS: &[&str] = &[
    "install",
    "remove",
    "uninstall",
    "update",
    "list",
    "config",
    "auth",
    "mcp",
];

/// Options that take a value as the next argument (Pi 1.3), so the value is
/// not taken for a message or a subcommand.
const WITH_VALUE: &[&str] = &[
    "--provider",
    "--model",
    "--api-key",
    "--system-prompt",
    "--append-system-prompt",
    "--mode",
    "--session",
    "--session-id",
    "--fork",
    "--session-dir",
    "-n",
    "--name",
    "--models",
    "-t",
    "--tools",
    "-xt",
    "--exclude-tools",
    "--thinking",
    "-e",
    "--extension",
    "--skill",
    "--prompt-template",
    "--theme",
    "--use-theme",
    "--tui-mode",
];

/// True when `pi <args>` opens the chat screen: no subcommand, maybe with
/// messages or `@files` for the first prompt, `--continue`, `--resume` and
/// the like. `--mode json` and `--mode rpc` speak JSON on stdout instead.
pub fn is_interactive(args: &[String]) -> bool {
    let mut it = args.iter();
    let mut first = true;
    while let Some(a) = it.next() {
        if a == "--" {
            return true;
        }
        if BATCH_FLAGS.contains(&a.as_str()) {
            return false;
        }
        let mode = if a == "--mode" {
            it.next().map(String::as_str)
        } else {
            a.strip_prefix("--mode=")
        };
        if let Some(m) = mode {
            if m != "text" {
                return false;
            }
        } else if WITH_VALUE.contains(&a.as_str()) {
            it.next();
        } else if !a.starts_with('-') {
            if first && SUBCOMMANDS.contains(&a.as_str()) {
                return false;
            }
            first = false;
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
        assert!(is_interactive(&v(&["fix the build"])));
        assert!(is_interactive(&v(&["@src/main.rs", "explain this"])));
        assert!(is_interactive(&v(&["-c"])));
        assert!(is_interactive(&v(&["--resume"])));
        assert!(is_interactive(&v(&["--session-id", "update"])));
        assert!(is_interactive(&v(&[
            "--model",
            "sonnet:high",
            "list the files"
        ])));
        assert!(is_interactive(&v(&["--tui-mode", "regular"])));
        assert!(is_interactive(&v(&["--mode", "text"])));
        assert!(is_interactive(&v(&["--", "-p is a flag"])));
        assert!(is_interactive(&v(&["review", "install"])));
        assert!(!is_interactive(&v(&["-p", "hi"])));
        assert!(!is_interactive(&v(&["--print", "--model", "x", "hi"])));
        assert!(!is_interactive(&v(&["--mode", "rpc", "--no-session"])));
        assert!(!is_interactive(&v(&["--mode=json", "hi"])));
        assert!(!is_interactive(&v(&["--export", "s.jsonl"])));
        assert!(!is_interactive(&v(&["--list-models", "haiku"])));
        assert!(!is_interactive(&v(&["install", "npm:pkg"])));
        assert!(!is_interactive(&v(&["--offline", "update"])));
        assert!(!is_interactive(&v(&[
            "auth",
            "check",
            "--provider",
            "openai"
        ])));
        assert!(!is_interactive(&v(&["config"])));
        assert!(!is_interactive(&v(&["-v"])));
    }
}
