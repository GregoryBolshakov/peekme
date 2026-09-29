//! The Copilot CLI conversation behind the screen, read from its session log.
//!
//! Sessions live in `<COPILOT_HOME or ~/.copilot>/session-state/<id>/`:
//! `events.jsonl` (the event log) and `workspace.yaml` (its directory). A
//! running session holds `inuse.<pid>.lock` there, with the pid of the real
//! Copilot binary, which may be a child of the `copilot` command peekme
//! started (the npm launcher).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::context::{ConvItem, Conversation, Source};

/// `$COPILOT_HOME`, else `~/.copilot`.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("COPILOT_HOME").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    std::env::var_os("HOME").map(|h| Path::new(&h).join(".copilot"))
}

/// The event log of the session held by one of `pids`, or failing that the
/// most recently written session of `cwd`.
pub fn find(config: &Path, pids: &[u32], cwd: &str) -> Option<PathBuf> {
    let sessions: Vec<PathBuf> = std::fs::read_dir(config.join("session-state"))
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    for dir in &sessions {
        let Ok(files) = std::fs::read_dir(dir) else {
            continue;
        };
        for f in files.flatten() {
            let name = f.file_name().to_string_lossy().into_owned();
            let held_by = name
                .strip_prefix("inuse.")
                .and_then(|n| n.strip_suffix(".lock"))
                .and_then(|n| n.parse::<u32>().ok());
            if held_by.is_some_and(|pid| pids.contains(&pid)) {
                let log = dir.join("events.jsonl");
                // No log yet: nothing was said in this session so far.
                return log.is_file().then_some(log);
            }
        }
    }
    sessions
        .iter()
        .filter(|d| workspace_cwd(d).as_deref() == Some(cwd))
        .filter_map(|d| {
            let log = d.join("events.jsonl");
            Some((log.metadata().ok()?.modified().ok()?, log))
        })
        .max()
        .map(|(_, p)| p)
}

/// `cwd:` from the session's workspace.yaml.
fn workspace_cwd(dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(dir.join("workspace.yaml")).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix("cwd:"))
        .map(|v| v.trim().trim_matches('"').to_string())
}

pub fn load(path: &Path) -> Option<Conversation> {
    let text = std::fs::read_to_string(path).ok()?;
    let id = path.parent()?.file_name()?.to_string_lossy().into_owned();
    Some(parse(&id, &text))
}

/// Typed requests, answers, and tool results labelled with their call.
/// Events of sub-agents are left out, like Claude's sidechains.
pub fn parse(id: &str, jsonl: &str) -> Conversation {
    let mut conv = Conversation {
        id: id.to_string(),
        title: None,
        items: Vec::new(),
    };
    let mut calls: HashMap<String, String> = HashMap::new();
    for line in jsonl.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let data = v.get("data").cloned().unwrap_or(Value::Null);
        let sub_agent = v.get("agentId").is_some_and(|a| !a.is_null())
            || data.get("parentToolCallId").is_some_and(|a| !a.is_null());
        if sub_agent || v.get("ephemeral").and_then(Value::as_bool) == Some(true) {
            continue;
        }
        let text = |k: &str| {
            data.get(k)
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string()
        };
        let turn = data.get("turnId").and_then(Value::as_str).map(String::from);
        match v.get("type").and_then(Value::as_str) {
            Some("session.title_changed") => {
                conv.title = Some(text("title")).filter(|t| !t.is_empty())
            }
            Some("user.message") => push(&mut conv, turn, Source::User, text("content")),
            Some("assistant.message") => {
                for r in data
                    .get("toolRequests")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(id) = r.get("toolCallId").and_then(Value::as_str) {
                        let name = r.get("name").and_then(Value::as_str).unwrap_or("tool");
                        calls.insert(id.to_string(), describe(name, r.get("arguments")));
                    }
                }
                push(&mut conv, turn, Source::Agent, text("content"));
            }
            Some("tool.execution_start") => {
                if let Some(id) = data.get("toolCallId").and_then(Value::as_str) {
                    let name = data
                        .get("toolName")
                        .and_then(Value::as_str)
                        .unwrap_or("tool");
                    calls.insert(id.to_string(), describe(name, data.get("arguments")));
                }
            }
            Some("tool.execution_complete") => {
                let call = data
                    .get("toolCallId")
                    .and_then(Value::as_str)
                    .and_then(|id| calls.get(id))
                    .cloned()
                    .unwrap_or_else(|| "tool".into());
                let result = data.get("result");
                let out = result
                    .and_then(|r| r.get("content"))
                    .or_else(|| result.and_then(|r| r.get("detailedContent")))
                    .and_then(Value::as_str)
                    .unwrap_or("");
                push(&mut conv, turn, Source::Tool(call), out.to_string());
            }
            _ => {}
        }
    }
    conv
}

fn push(conv: &mut Conversation, turn: Option<String>, source: Source, text: String) {
    if !text.trim().is_empty() {
        conv.items.push(ConvItem { turn, source, text });
    }
}

/// `bash: cargo test`, `view: src/main.rs`, or just the tool name.
fn describe(name: &str, args: Option<&Value>) -> String {
    let arg = [
        "command",
        "path",
        "file_path",
        "pattern",
        "url",
        "query",
        "description",
    ]
    .iter()
    .find_map(|k| args.and_then(|a| a.get(*k)).and_then(Value::as_str));
    match arg {
        Some(a) => format!("{name}: {a}"),
        None => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shapes from a real Copilot CLI 1.0.89 session, texts shortened.
    const JSONL: &str = r#"{"type":"session.start","data":{"sessionId":"x","producer":"copilot-agent"},"id":"1","timestamp":"t","parentId":null}
{"type":"user.message","data":{"content":"why is the build slow?","transformedContent":"<current_datetime>...","turnId":"0"},"id":"2"}
{"type":"assistant.turn_start","data":{"turnId":"0"},"id":"3"}
{"type":"assistant.message","data":{"messageId":"m1","content":"Let me time it.","toolRequests":[{"toolCallId":"c1","name":"bash","arguments":{"command":"cargo build --timings"}}],"turnId":"0"},"id":"4"}
{"type":"tool.execution_start","data":{"toolCallId":"c1","toolName":"bash","arguments":{"command":"cargo build --timings"}},"id":"5"}
{"type":"tool.execution_complete","data":{"toolCallId":"c1","success":true,"result":{"content":"Finished in 93s"}},"id":"6"}
{"type":"assistant.message","agentId":"sub","data":{"messageId":"m2","content":"sub-agent chatter"},"id":"7"}
{"type":"assistant.message","data":{"messageId":"m3","content":"The `syn` crate dominates.","turnId":"0"},"id":"8"}
{"type":"session.title_changed","data":{"title":"Slow build"},"id":"9"}
not json
"#;

    #[test]
    fn parses_copilot_events() {
        let c = parse("s1", JSONL);
        assert_eq!(c.title.as_deref(), Some("Slow build"));
        let got: Vec<(Source, &str)> = c
            .items
            .iter()
            .map(|i| (i.source.clone(), i.text.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                (Source::User, "why is the build slow?"),
                (Source::Agent, "Let me time it."),
                (
                    Source::Tool("bash: cargo build --timings".into()),
                    "Finished in 93s"
                ),
                (Source::Agent, "The `syn` crate dominates."),
            ]
        );
    }

    #[test]
    fn finds_the_session_held_by_the_process() {
        let root = std::env::temp_dir().join(format!("peekme-copilot-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (a, b) = (
            root.join("session-state/aaa"),
            root.join("session-state/bbb"),
        );
        for d in [&a, &b] {
            std::fs::create_dir_all(d).unwrap();
            std::fs::write(d.join("events.jsonl"), "").unwrap();
            std::fs::write(d.join("workspace.yaml"), "id: x\ncwd: /work/app\n").unwrap();
        }
        std::fs::write(a.join("inuse.4242.lock"), "4242").unwrap();
        assert_eq!(
            find(&root, &[1, 4242], "/work/app"),
            Some(a.join("events.jsonl"))
        );
        // Not held by us: the newest session of the directory.
        assert!(find(&root, &[7], "/work/app").is_some());
        assert_eq!(find(&root, &[7], "/elsewhere"), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
