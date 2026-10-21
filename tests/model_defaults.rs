//! Model settings are isolated in child processes: tests never change the
//! process environment or call a model.

use peekme::{agent::Agent, explain};
use std::process::Command;

const SETTINGS: [(Agent, &str, &str); 5] = [
    (
        Agent::Claude,
        "PEEKME_CLAUDE_MODEL",
        "PEEKME_CLAUDE_ASK_MODEL",
    ),
    (Agent::Codex, "PEEKME_MODEL", "PEEKME_CODEX_ASK_MODEL"),
    (
        Agent::Copilot,
        "PEEKME_COPILOT_MODEL",
        "PEEKME_COPILOT_ASK_MODEL",
    ),
    (Agent::Kiro, "PEEKME_KIRO_MODEL", "PEEKME_KIRO_ASK_MODEL"),
    (Agent::Pi, "PEEKME_PI_MODEL", "PEEKME_PI_ASK_MODEL"),
];

#[test]
fn each_agent_follows_the_chat_and_has_independent_overrides() {
    for case in ["default", "peek", "ask", "both", "empty"] {
        let mut child = Command::new(std::env::current_exe().unwrap());
        child.args(["--exact", "model_selection_child", "--nocapture"]);
        child.env("PEEKME_TEST_MODEL_CASE", case);
        for (agent, peek, ask) in SETTINGS {
            child.env_remove(peek).env_remove(ask);
            if matches!(case, "peek" | "both") {
                child.env(peek, format!("{}-peek", agent.command()));
            }
            if matches!(case, "ask" | "both") {
                child.env(ask, format!("{}-ask", agent.command()));
            }
            if case == "empty" {
                child.env(peek, " ").env(ask, "");
            }
        }
        let output = child.output().unwrap();
        assert!(
            output.status.success(),
            "{case}: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn model_selection_child() {
    let Ok(case) = std::env::var("PEEKME_TEST_MODEL_CASE") else {
        return;
    };
    for (agent, _, _) in SETTINGS {
        for question in [false, true] {
            let overridden =
                case == "both" || (case == "peek" && !question) || (case == "ask" && question);
            let expected_override = format!(
                "{}-{}",
                agent.command(),
                if question { "ask" } else { "peek" }
            );
            for chat in [None, Some("first-model"), Some("switched-model")] {
                let expected = if overridden {
                    Some(expected_override.as_str())
                } else {
                    chat
                };
                assert_eq!(
                    explain::model(agent, question, chat).as_deref(),
                    expected,
                    "{case}, {agent:?}, question={question}"
                );
            }
        }
    }
}
