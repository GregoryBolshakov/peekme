//! Which agent explains a selection in a plain terminal: the one the user
//! named in `PEEKME_EXPLAINER`, else the first installed one that is logged in,
//! else the first installed one. Asked at the first explanation, never at
//! start: a login check runs the agent's CLI (Kiro's takes most of a second).

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::agent::Agent;

/// A login check that has not answered in this long counts as "not logged in".
const CHECK_TIMEOUT: Duration = Duration::from_secs(5);

pub fn agent() -> Agent {
    if let Some(a) = std::env::var("PEEKME_EXPLAINER")
        .ok()
        .and_then(|v| from_name(&v))
    {
        return a;
    }
    let installed: Vec<Agent> = Agent::ALL
        .into_iter()
        .filter(|a| crate::launch::find_real(a.command()).is_some())
        .collect();
    let chosen = installed
        .iter()
        .copied()
        .find(|&a| logged_in(a))
        .or_else(|| installed.first().copied())
        .unwrap_or(Agent::Claude);
    crate::event(
        "explainer_pick",
        serde_json::json!({"installed": installed.len(), "agent": chosen.command()}),
    );
    chosen
}

/// `PEEKME_EXPLAINER`'s value: an agent's command or short name.
pub fn from_name(name: &str) -> Option<Agent> {
    let name = name.trim().to_ascii_lowercase();
    Agent::ALL
        .into_iter()
        .find(|a| name == a.command() || name == a.short().to_ascii_lowercase())
        .or_else(|| (name == "kiro").then_some(Agent::Kiro))
}

fn logged_in(agent: Agent) -> bool {
    let Some(bin) = crate::launch::find_real(agent.command()) else {
        return false;
    };
    match agent {
        Agent::Claude => run(&bin, &["auth", "status"]).is_some_and(|out| claude_logged_in(&out)),
        Agent::Codex => run(&bin, &["login", "status"]).is_some(),
        Agent::Kiro => run(&bin, &["whoami"]).is_some(),
        Agent::Copilot => {
            ["COPILOT_GITHUB_TOKEN", "GH_TOKEN", "GITHUB_TOKEN"]
                .iter()
                .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty()))
                || crate::copilot::transcript::config_dir().is_some_and(|d| copilot_logged_in(&d))
        }
    }
}

/// `claude auth status` prints JSON with `"loggedIn": true`.
fn claude_logged_in(out: &str) -> bool {
    serde_json::from_str::<serde_json::Value>(out)
        .ok()
        .and_then(|v| v.get("loggedIn").and_then(serde_json::Value::as_bool))
        .unwrap_or(false)
}

/// Copilot keeps its logins in `config.json` (JSON after `//` comment lines).
fn copilot_logged_in(dir: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(dir.join("config.json")) else {
        return false;
    };
    copilot_config_has_login(&text)
}

fn copilot_config_has_login(text: &str) -> bool {
    let json: String = text
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n");
    serde_json::from_str::<serde_json::Value>(&json)
        .ok()
        .and_then(|v| v.get("loggedInUsers")?.as_array().map(|a| !a.is_empty()))
        .unwrap_or(false)
}

/// Stdout of `bin args` if it exits 0 within the timeout.
fn run(bin: &Path, args: &[&str]) -> Option<String> {
    let mut child = Command::new(bin)
        .args(args)
        .env_remove(crate::launch::ACTIVE_ENV)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let until = Instant::now() + CHECK_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) | Err(_) => return None,
            Ok(None) if Instant::now() >= until => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let mut out = String::new();
    std::io::Read::read_to_string(child.stdout.as_mut()?, &mut out).ok()?;
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explainer_names() {
        assert_eq!(from_name("claude"), Some(Agent::Claude));
        assert_eq!(from_name(" Codex "), Some(Agent::Codex));
        assert_eq!(from_name("kiro-cli"), Some(Agent::Kiro));
        assert_eq!(from_name("kiro"), Some(Agent::Kiro));
        assert_eq!(from_name("copilot"), Some(Agent::Copilot));
        assert_eq!(from_name("gpt"), None);
    }

    #[test]
    fn claude_status_json() {
        assert!(claude_logged_in(
            r#"{"loggedIn": true, "authMethod": "claude.ai"}"#
        ));
        assert!(!claude_logged_in(r#"{"loggedIn": false}"#));
        assert!(!claude_logged_in("Not logged in"));
    }

    #[test]
    fn copilot_config_with_comment_lines() {
        let with = "// User settings belong in settings.json.\n// This file is managed automatically.\n\
                    {\n  \"loggedInUsers\": [\n    {\"host\": \"https://github.com\", \"login\": \"u\"}\n  ]\n}\n";
        assert!(copilot_config_has_login(with));
        assert!(!copilot_config_has_login("{\"loggedInUsers\": []}"));
        assert!(!copilot_config_has_login("// only a comment\n{}"));
    }
}
