//! The Claude Code conversation behind the screen, read from its transcript.
//!
//! A running interactive `claude` registers itself in
//! `<config>/sessions/<pid>.json` with its current `sessionId`, and writes the
//! conversation to `<config>/projects/<cwd, non-alphanumerics as '-'>/<sessionId>.jsonl`.
//! peekme knows the pid of the `claude` it started, so it finds the
//! exact transcript without guessing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::context::{ConvItem, Conversation, Source};

/// `$CLAUDE_CONFIG_DIR`, else `~/.claude`.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    std::env::var_os("HOME").map(|h| Path::new(&h).join(".claude"))
}

/// Claude's name for a project directory: every non-alphanumeric char becomes '-'.
fn project_dir_name(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

/// The transcript of the session run by process `pid`, or failing that the
/// most recently written transcript for `cwd`.
pub fn find(config: &Path, pid: Option<u32>, cwd: &str) -> Option<PathBuf> {
    let projects = config.join("projects");
    let own_dir = projects.join(project_dir_name(cwd));
    if let Some(pid) = pid
        && let Ok(text) =
            std::fs::read_to_string(config.join("sessions").join(format!("{pid}.json")))
        && let Ok(v) = serde_json::from_str::<Value>(&text)
        && let Some(id) = v.get("sessionId").and_then(Value::as_str)
    {
        let file = format!("{id}.jsonl");
        let session_cwd = v.get("cwd").and_then(Value::as_str).unwrap_or(cwd);
        let direct = projects.join(project_dir_name(session_cwd)).join(&file);
        if direct.is_file() {
            return Some(direct);
        }
        // The session may have been started for another directory.
        if let Ok(dirs) = std::fs::read_dir(&projects) {
            for d in dirs.flatten() {
                let p = d.path().join(&file);
                if p.is_file() {
                    return Some(p);
                }
            }
        }
    }
    newest_jsonl(&own_dir)
}

fn newest_jsonl(dir: &Path) -> Option<PathBuf> {
    std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max()
        .map(|(_, p)| p)
}

pub fn load(path: &Path) -> Option<Conversation> {
    let text = std::fs::read_to_string(path).ok()?;
    let id = path.file_stem()?.to_string_lossy().into_owned();
    Some(parse(&id, &text))
}

/// Build the conversation from JSONL records: typed user requests, assistant
/// text, and tool results labelled with the call that produced them.
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
        if v.get("isSidechain").and_then(Value::as_bool) == Some(true)
            || v.get("isMeta").and_then(Value::as_bool) == Some(true)
        {
            continue;
        }
        let turn = v.get("promptId").and_then(Value::as_str).map(String::from);
        match v.get("type").and_then(Value::as_str) {
            Some("ai-title") => {
                if let Some(t) = v.get("aiTitle").and_then(Value::as_str) {
                    conv.title = Some(t.to_string());
                }
            }
            Some("custom-title") => {
                if let Some(t) = v.get("customTitle").and_then(Value::as_str) {
                    conv.title = Some(t.to_string());
                }
            }
            Some("user") => {
                if v.get("isCompactSummary").and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                match v.pointer("/message/content") {
                    Some(Value::String(s)) => push_user(&mut conv, turn, s),
                    Some(Value::Array(blocks)) => {
                        for b in blocks {
                            match b.get("type").and_then(Value::as_str) {
                                Some("text") => {
                                    let t = b.get("text").and_then(Value::as_str).unwrap_or("");
                                    push_user(&mut conv, turn.clone(), t);
                                }
                                Some("tool_result") => {
                                    let call = b
                                        .get("tool_use_id")
                                        .and_then(Value::as_str)
                                        .and_then(|id| calls.get(id))
                                        .cloned()
                                        .unwrap_or_else(|| "tool".into());
                                    let text = content_text(b.get("content"));
                                    if !text.trim().is_empty() {
                                        conv.items.push(ConvItem {
                                            turn: turn.clone(),
                                            source: Source::Tool(call),
                                            text,
                                        });
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some("assistant") => {
                let Some(Value::Array(blocks)) = v.pointer("/message/content") else {
                    continue;
                };
                for b in blocks {
                    match b.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            let t = b.get("text").and_then(Value::as_str).unwrap_or("");
                            if !t.trim().is_empty() {
                                conv.items.push(ConvItem {
                                    turn: turn.clone(),
                                    source: Source::Agent,
                                    text: t.to_string(),
                                });
                            }
                        }
                        Some("tool_use") => {
                            if let Some(id) = b.get("id").and_then(Value::as_str) {
                                calls.insert(id.to_string(), describe_call(b));
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    conv
}

/// A typed request. Slash commands, their output and injected reminders are
/// stored as user records too; they are not requests.
fn push_user(conv: &mut Conversation, turn: Option<String>, text: &str) {
    let t = text.trim();
    let wrapped = [
        "<command-name>",
        "<command-message>",
        "<local-command-",
        "<system-reminder>",
        "<bash-input>",
        "<bash-stdout>",
        "<bash-stderr>",
        "[Request interrupted",
    ];
    if t.is_empty() || wrapped.iter().any(|w| t.starts_with(w)) {
        return;
    }
    conv.items.push(ConvItem {
        turn,
        source: Source::User,
        text: text.to_string(),
    });
}

fn content_text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// `Bash: git status`, `Read: src/main.rs`, or just the tool name.
fn describe_call(block: &Value) -> String {
    let name = block.get("name").and_then(Value::as_str).unwrap_or("tool");
    let input = block.get("input");
    let arg = [
        "command",
        "file_path",
        "notebook_path",
        "pattern",
        "url",
        "query",
        "description",
    ]
    .iter()
    .find_map(|k| input.and_then(|i| i.get(*k)).and_then(Value::as_str));
    match arg {
        Some(a) => format!("{name}: {a}"),
        None => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSONL: &str = r#"{"type":"system","subtype":"local_command","content":"<command-name>/model</command-name>"}
{"type":"user","message":{"role":"user","content":"<command-name>/model</command-name>\n<command-args></command-args>"}}
{"type":"user","promptId":"p1","message":{"role":"user","content":"why is the build slow?"}}
{"type":"attachment","attachment":{"type":"environment"}}
{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"hmm"}]}}
{"type":"assistant","message":{"content":[{"type":"text","text":"Let me check **cargo**."}]}}
{"type":"assistant","message":{"content":[{"type":"tool_use","id":"tu1","name":"Bash","input":{"command":"cargo build --timings","description":"Time the build"}}]}}
{"type":"user","promptId":"p1","message":{"role":"user","content":[{"tool_use_id":"tu1","type":"tool_result","content":"Finished in 93s"}]}}
{"type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"subagent chatter"}]}}
{"type":"user","isMeta":true,"message":{"role":"user","content":"meta"}}
{"type":"assistant","message":{"content":[{"type":"text","text":"The `syn` crate dominates."}]}}
{"type":"ai-title","aiTitle":"Slow build"}
not json
"#;

    #[test]
    fn parses_claude_jsonl() {
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
                (Source::Agent, "Let me check **cargo**."),
                (
                    Source::Tool("Bash: cargo build --timings".into()),
                    "Finished in 93s"
                ),
                (Source::Agent, "The `syn` crate dominates."),
            ]
        );
        assert_eq!(c.items[0].turn.as_deref(), Some("p1"));
    }

    #[test]
    fn finds_the_transcript_from_the_pid() {
        let root =
            std::env::temp_dir().join(format!("peekme-transcript-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let proj = root.join("projects").join("-work-my-app");
        std::fs::create_dir_all(&proj).unwrap();
        std::fs::create_dir_all(root.join("sessions")).unwrap();
        std::fs::write(proj.join("aaa.jsonl"), "").unwrap();
        std::fs::write(proj.join("bbb.jsonl"), "").unwrap();
        std::fs::write(
            root.join("sessions/4242.json"),
            r#"{"pid":4242,"sessionId":"aaa","cwd":"/work/my.app"}"#,
        )
        .unwrap();
        assert_eq!(project_dir_name("/work/my.app"), "-work-my-app");
        assert_eq!(
            find(&root, Some(4242), "/work/my.app"),
            Some(proj.join("aaa.jsonl"))
        );
        // Unknown pid: the newest transcript of the directory.
        assert!(find(&root, Some(1), "/work/my.app").is_some());
        assert_eq!(find(&root, None, "/elsewhere"), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
