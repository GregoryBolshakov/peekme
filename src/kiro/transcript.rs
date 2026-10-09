//! The Kiro CLI conversation behind the screen, read from its session log.
//!
//! Sessions live in `<KIRO_HOME or ~/.kiro>/sessions/cli/`, three files each:
//! `<id>.json` (metadata, with the directory), `<id>.jsonl` (the event log)
//! and, while a session is open, `<id>.lock` with the pid of the
//! `kiro-cli-chat acp` process that holds it, a grandchild of the `kiro-cli`
//! command peekme started. Every start opens a new session, so the newest
//! session of a directory is often an empty one.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::context::{ConvItem, Conversation, Source};

/// `$KIRO_HOME`, else `~/.kiro`.
pub fn config_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("KIRO_HOME").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    std::env::var_os("HOME").map(|h| Path::new(&h).join(".kiro"))
}

/// The event log of the session held by one of `pids`, or failing that the
/// most recently written non-empty log of `cwd`.
pub fn find(config: &Path, pids: &[u32], cwd: &str) -> Option<PathBuf> {
    let dir = config.join("sessions/cli");
    let files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .collect();
    for lock in files
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "lock"))
    {
        let held_by = std::fs::read_to_string(lock)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| v.get("pid").and_then(Value::as_u64));
        if held_by.is_some_and(|pid| pids.contains(&(pid as u32))) {
            let log = lock.with_extension("jsonl");
            return log.is_file().then_some(log);
        }
    }
    files
        .iter()
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .filter(|p| session_cwd(&p.with_extension("json")).as_deref() == Some(cwd))
        .filter_map(|p| {
            let m = p.metadata().ok().filter(|m| m.len() > 0)?;
            Some((m.modified().ok()?, p.clone()))
        })
        .max()
        .map(|(_, p)| p)
}

/// `cwd` from the session's metadata.
fn session_cwd(meta: &Path) -> Option<String> {
    let text = std::fs::read_to_string(meta).ok()?;
    let v: Value = serde_json::from_str(&text).ok()?;
    v.get("cwd").and_then(Value::as_str).map(String::from)
}

pub fn load(path: &Path) -> Option<Conversation> {
    let text = std::fs::read_to_string(path).ok()?;
    let id = path.file_stem()?.to_string_lossy().into_owned();
    let mut conv = parse(&id, &text);
    conv.title = std::fs::read_to_string(path.with_extension("json"))
        .ok()
        .and_then(|t| serde_json::from_str::<Value>(&t).ok())
        .and_then(|v| v.get("title").and_then(Value::as_str).map(String::from))
        .filter(|t| !t.is_empty());
    Some(conv)
}

/// Typed requests, answers, and tool results labelled with their call. Each
/// prompt starts a turn; thinking is left out (Kiro stores it redacted).
pub fn parse(id: &str, jsonl: &str) -> Conversation {
    let mut conv = Conversation {
        id: id.to_string(),
        title: None,
        items: Vec::new(),
    };
    let mut calls: HashMap<String, String> = HashMap::new();
    let mut turn: Option<String> = None;
    for line in jsonl.lines() {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let data = v.get("data").cloned().unwrap_or(Value::Null);
        let parts = data
            .get("content")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        match v.get("kind").and_then(Value::as_str) {
            Some("Prompt") => {
                turn = data
                    .get("message_id")
                    .and_then(Value::as_str)
                    .map(String::from);
                push(&mut conv, &turn, Source::User, texts(&parts));
            }
            Some("AssistantMessage") => {
                for p in &parts {
                    if p.get("kind").and_then(Value::as_str) != Some("toolUse") {
                        continue;
                    }
                    let d = p.get("data");
                    if let Some(id) = d.and_then(|d| d.get("toolUseId")).and_then(Value::as_str) {
                        let name = d
                            .and_then(|d| d.get("name"))
                            .and_then(Value::as_str)
                            .unwrap_or("tool");
                        calls.insert(
                            id.to_string(),
                            describe(name, d.and_then(|d| d.get("input"))),
                        );
                    }
                }
                push(&mut conv, &turn, Source::Agent, texts(&parts));
            }
            Some("ToolResults") => {
                for p in &parts {
                    let Some(d) = p.get("data") else { continue };
                    let call = d
                        .get("toolUseId")
                        .and_then(Value::as_str)
                        .and_then(|id| calls.get(id))
                        .cloned()
                        .unwrap_or_else(|| "tool".into());
                    let out = texts(
                        d.get("content")
                            .and_then(Value::as_array)
                            .map_or(&[][..], Vec::as_slice),
                    );
                    push(&mut conv, &turn, Source::Tool(call), out);
                }
            }
            _ => {}
        }
    }
    conv
}

/// The text parts of a message, joined.
fn texts(parts: &[Value]) -> String {
    parts
        .iter()
        .filter(|p| p.get("kind").and_then(Value::as_str) == Some("text"))
        .filter_map(|p| p.get("data").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("\n")
}

fn push(conv: &mut Conversation, turn: &Option<String>, source: Source, text: String) {
    if !text.trim().is_empty() {
        conv.items.push(ConvItem {
            turn: turn.clone(),
            source,
            text,
        });
    }
}

/// `shell: cargo test`, `read: src/main.rs`, or just the tool name.
fn describe(name: &str, input: Option<&Value>) -> String {
    let arg = ["command", "path", "file_path", "pattern", "url", "query"]
        .iter()
        .find_map(|k| input.and_then(|a| a.get(*k)).and_then(Value::as_str))
        .or_else(|| {
            input
                .and_then(|a| a.pointer("/operations/0/path"))
                .and_then(Value::as_str)
        });
    match arg {
        Some(a) => format!("{name}: {a}"),
        None => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shapes from a real Kiro CLI 2.28.0 session, texts and ids shortened.
    const JSONL: &str = r#"{"version":"v1","kind":"Prompt","data":{"message_id":"p1","content":[{"kind":"text","data":"List the files here, then say what each one is for."}],"meta":{"timestamp":1791564802}}}
{"version":"v1","kind":"AssistantMessage","data":{"message_id":"a1","content":[{"kind":"thinking","data":{"text":"","signature":null,"redactedContent":[46,75],"modelId":"auto"}},{"kind":"text","data":""},{"kind":"toolUse","data":{"toolUseId":"t1","name":"read","input":{"__tool_use_purpose":"List files","operations":[{"mode":"Directory","path":"/work/demo","depth":2}]}}}]}}
{"version":"v1","kind":"ToolResults","data":{"message_id":"r1","content":[{"kind":"toolResult","data":{"toolUseId":"t1","content":[{"kind":"text","data":"-rw-rw-r-- 1 1000 1000 26 Oct 09 16:52 /work/demo/README.md"}],"status":"success"}}],"results":{}}}
{"version":"v1","kind":"AssistantMessage","data":{"message_id":"a2","content":[{"kind":"text","data":"- `README.md` — a tiny demo crate."}]}}
not json
"#;

    #[test]
    fn parses_kiro_events() {
        let c = parse("s1", JSONL);
        let got: Vec<(Source, &str)> = c
            .items
            .iter()
            .map(|i| (i.source.clone(), i.text.as_str()))
            .collect();
        assert_eq!(
            got,
            vec![
                (
                    Source::User,
                    "List the files here, then say what each one is for."
                ),
                (
                    Source::Tool("read: /work/demo".into()),
                    "-rw-rw-r-- 1 1000 1000 26 Oct 09 16:52 /work/demo/README.md"
                ),
                (Source::Agent, "- `README.md` — a tiny demo crate."),
            ]
        );
        assert!(c.items.iter().all(|i| i.turn.as_deref() == Some("p1")));
    }

    #[test]
    fn finds_the_session_held_by_the_process() {
        let root = std::env::temp_dir().join(format!("peekme-kiro-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("sessions/cli");
        std::fs::create_dir_all(&dir).unwrap();
        for (id, log) in [("aaa", "{}\n"), ("bbb", "{}\n"), ("ccc", "")] {
            std::fs::write(dir.join(format!("{id}.jsonl")), log).unwrap();
            std::fs::write(
                dir.join(format!("{id}.json")),
                r#"{"session_id":"x","cwd":"/work/app"}"#,
            )
            .unwrap();
        }
        std::fs::write(dir.join("aaa.lock"), r#"{"pid":4242,"started_at":"t"}"#).unwrap();
        assert_eq!(
            find(&root, &[1, 4242], "/work/app"),
            Some(dir.join("aaa.jsonl"))
        );
        // Not held by us: the newest session of the directory that has a log.
        let other = find(&root, &[7], "/work/app");
        assert!(other.is_some() && other != Some(dir.join("ccc.jsonl")));
        assert_eq!(find(&root, &[7], "/elsewhere"), None);
        let _ = std::fs::remove_dir_all(&root);
    }
}
