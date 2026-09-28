//! Which `codex` invocations open Codex's interactive screen.

/// Codex options that take a value (from `codex --help`, 0.154.0).
const VALUE_OPTIONS: &[&str] = &[
    "-c",
    "--config",
    "--enable",
    "--disable",
    "--remote",
    "--remote-auth-token-env",
    "-m",
    "--model",
    "--local-provider",
    "-p",
    "--profile",
    "-s",
    "--sandbox",
    "-C",
    "--cd",
    "--add-dir",
    "-a",
    "--ask-for-approval",
];

/// Subcommands that open Codex's own interactive screen, where peeking helps.
/// Everything else (`exec`, `login`, `mcp`, `app-server`, ...) runs directly.
const INTERACTIVE_SUBCOMMANDS: &[&str] = &["resume", "fork"];

/// Every Codex subcommand name (0.154.0), used to tell a subcommand from a prompt.
const SUBCOMMANDS: &[&str] = &[
    "exec",
    "e",
    "review",
    "login",
    "logout",
    "mcp",
    "plugin",
    "app-server",
    "remote-control",
    "completion",
    "update",
    "doctor",
    "sandbox",
    "debug",
    "apply",
    "a",
    "resume",
    "queue",
    "archive",
    "delete",
    "migrate-rollouts",
    "unarchive",
    "fork",
    "cloud",
    "exec-server",
    "features",
    "help",
    "agents",
];

/// True when `codex <args>` starts Codex's interactive screen: no subcommand
/// (maybe a prompt), or `resume` / `fork`. Help and version also run directly.
pub fn is_interactive(args: &[String]) -> bool {
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        match a {
            "-h" | "--help" | "-V" | "--version" => return false,
            "--" => return true,
            "-i" | "--image" => {
                // One or more files follow.
                i += 1;
                while i < args.len() && !args[i].starts_with('-') {
                    i += 1;
                }
                continue;
            }
            _ if VALUE_OPTIONS.contains(&a) => i += 2,
            _ if a.starts_with('-') => i += 1,
            _ => {
                return !SUBCOMMANDS.contains(&a) || INTERACTIVE_SUBCOMMANDS.contains(&a);
            }
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
        assert!(is_interactive(&v(&[
            "-m",
            "gpt-5.6-luna",
            "resume",
            "--last"
        ])));
        assert!(is_interactive(&v(&["--yolo", "fork"])));
        assert!(is_interactive(&v(&[
            "-c",
            "model=\"x\"",
            "-i",
            "a.png",
            "b.png",
            "--search"
        ])));
        // A prompt that happens to be a subcommand's name needs `--`.
        assert!(is_interactive(&v(&["--", "exec"])));
        assert!(!is_interactive(&v(&["exec", "do it"])));
        assert!(!is_interactive(&v(&["-m", "resume", "e", "x"])));
        assert!(!is_interactive(&v(&["login"])));
        assert!(!is_interactive(&v(&["mcp", "list"])));
        assert!(!is_interactive(&v(&["app-server"])));
        assert!(!is_interactive(&v(&["--version"])));
    }
}
