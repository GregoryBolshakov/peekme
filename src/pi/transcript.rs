//! The Pi conversation behind the screen, read from its session file.
//!
//! Sessions live in `<sessions>/--<cwd with / as ->--/<time>_<id>.jsonl`,
//! where `<sessions>` is `PI_CODING_AGENT_SESSION_DIR`, else the agent
//! folder's `sessions` (`PI_CODING_AGENT_DIR`, default `~/.pi/agent`). A file
//! is written once the chat has its first message and appended to after
//! that; nothing in it names the process, so the session of a directory is
//! the one written last. Entries form a tree (`id`, `parentId`): the branch
//! on screen runs from the last entry up to the root.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::context::{ConvItem, Conversation, Source};

/// Pi's agent folder: `$PI_CODING_AGENT_DIR` (a leading `~` is the home),
/// else `~/.pi/agent`.
pub fn agent_dir() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    match std::env::var("PI_CODING_AGENT_DIR") {
        Ok(d) if !d.is_empty() => Some(expand(&d, home.as_deref())),
        _ => home.map(|h| h.join(".pi/agent")),
    }
}

fn expand(p: &str, home: Option<&Path>) -> PathBuf {
    match (p.strip_prefix("~/").or((p == "~").then_some("")), home) {
        (Some(rest), Some(h)) => h.join(rest),
        _ => PathBuf::from(p),
    }
}

/// Where sessions are kept: `--session-dir` in the chat's command line,
/// `$PI_CODING_AGENT_SESSION_DIR`, the `sessionDir` setting of the
/// project or the user, else `<agent folder>/sessions`. Relative paths are
/// from `cwd`.
pub fn sessions_dir(args: &[String], cwd: &Path) -> Option<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let from_args = args
        .iter()
        .position(|a| a == "--session-dir")
        .and_then(|i| args.get(i + 1).cloned())
        .or_else(|| {
            args.iter()
                .find_map(|a| a.strip_prefix("--session-dir=").map(String::from))
        });
    let setting = |file: PathBuf| -> Option<String> {
        let v: Value = serde_json::from_str(&std::fs::read_to_string(file).ok()?).ok()?;
        v.get("sessionDir")?.as_str().map(String::from)
    };
    let agent = agent_dir();
    let chosen = from_args
        .or_else(|| {
            std::env::var("PI_CODING_AGENT_SESSION_DIR")
                .ok()
                .filter(|d| !d.is_empty())
        })
        .or_else(|| setting(cwd.join(".pi/settings.json")))
        .or_else(|| {
            agent
                .as_ref()
                .and_then(|a| setting(a.join("settings.json")))
        });
    match chosen {
        Some(d) => Some(cwd.join(expand(&d, home.as_deref()))),
        None => agent.map(|a| a.join("sessions")),
    }
}

/// The command line of the chat peekme runs, for `chat_sessions_dir`.
static CHAT_ARGS: std::sync::OnceLock<Vec<String>> = std::sync::OnceLock::new();

/// Remember the chat's command line (once, at start).
pub fn set_chat_args(args: &[String]) {
    let _ = CHAT_ARGS.set(args.to_vec());
}

/// `sessions_dir` of the chat peekme runs.
pub fn chat_sessions_dir(cwd: &Path) -> Option<PathBuf> {
    sessions_dir(CHAT_ARGS.get().map_or(&[][..], Vec::as_slice), cwd)
}

/// The folder of `cwd`'s sessions: `/work/my app` is `--work-my app--`.
pub fn folder_name(cwd: &str) -> String {
    let trimmed = cwd.trim_start_matches(['/', '\\']);
    let safe: String = trimmed
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':') {
                '-'
            } else {
                c
            }
        })
        .collect();
    format!("--{safe}--")
}

/// The session of `cwd` written last.
pub fn find(sessions: &Path, cwd: &str) -> Option<PathBuf> {
    let dir = sessions.join(folder_name(cwd));
    std::fs::read_dir(&dir)
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "jsonl"))
        .filter(|p| session_cwd(p).as_deref() == Some(cwd))
        .filter_map(|p| Some((p.metadata().ok()?.modified().ok()?, p)))
        .max()
        .map(|(_, p)| p)
}

/// `cwd` from the session's first line, its header.
fn session_cwd(path: &Path) -> Option<String> {
    use std::io::BufRead;
    let f = std::fs::File::open(path).ok()?;
    let mut first = String::new();
    std::io::BufReader::new(f).read_line(&mut first).ok()?;
    let v: Value = serde_json::from_str(&first).ok()?;
    (v.get("type").and_then(Value::as_str) == Some("session"))
        .then(|| v.get("cwd").and_then(Value::as_str).map(String::from))
        .flatten()
}

pub fn load(path: &Path) -> Option<Conversation> {
    let text = std::fs::read_to_string(path).ok()?;
    let stem = path.file_stem()?.to_string_lossy();
    // `<time>_<id>`
    let id = stem.split_once('_').map_or(&*stem, |(_, id)| id);
    Some(parse(id, &text))
}

/// The branch on screen: requests, answers, tool results labelled with
/// their call, and `!` commands with their output. Each request starts a
/// turn; thinking is left out. `model` is the chat's `provider/id`.
pub fn parse(id: &str, jsonl: &str) -> Conversation {
    let entries: Vec<Value> = jsonl
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v.get("type").and_then(Value::as_str) != Some("session"))
        .collect();
    let by_id: HashMap<&str, &Value> = entries
        .iter()
        .filter_map(|e| Some((e.get("id")?.as_str()?, e)))
        .collect();
    let mut branch = Vec::new();
    let mut next = entries.last();
    while let Some(e) = next {
        if branch.len() > entries.len() {
            break; // a cycle: not a tree
        }
        branch.push(e);
        next = e
            .get("parentId")
            .and_then(Value::as_str)
            .and_then(|p| by_id.get(p).copied());
    }
    branch.reverse();

    let mut conv = Conversation {
        id: id.to_string(),
        title: None,
        model: None,
        items: Vec::new(),
    };
    let mut calls: HashMap<String, String> = HashMap::new();
    let mut turn: Option<String> = None;
    for e in branch {
        match e.get("type").and_then(Value::as_str) {
            Some("model_change") => {
                let provider = e.get("provider").and_then(Value::as_str);
                let model = e.get("modelId").and_then(Value::as_str);
                if let (Some(p), Some(m)) = (provider, model) {
                    conv.model = Some(format!("{p}/{m}"));
                }
            }
            Some("session_info") => {
                conv.title = e
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|n| !n.is_empty())
                    .map(String::from);
            }
            Some("message") => {
                let m = e.get("message").unwrap_or(&Value::Null);
                match m.get("role").and_then(Value::as_str) {
                    Some("user") => {
                        turn = e.get("id").and_then(Value::as_str).map(String::from);
                        push(&mut conv, &turn, Source::User, texts(m.get("content")));
                    }
                    Some("assistant") => {
                        let parts = m.get("content").and_then(Value::as_array);
                        for p in parts.into_iter().flatten() {
                            if p.get("type").and_then(Value::as_str) != Some("toolCall") {
                                continue;
                            }
                            if let Some(id) = p.get("id").and_then(Value::as_str) {
                                let name = p.get("name").and_then(Value::as_str).unwrap_or("tool");
                                calls.insert(id.to_string(), describe(name, p.get("arguments")));
                            }
                        }
                        if conv.model.is_none()
                            && let (Some(p), Some(id)) = (
                                m.get("provider").and_then(Value::as_str),
                                m.get("model").and_then(Value::as_str),
                            )
                        {
                            conv.model = Some(format!("{p}/{id}"));
                        }
                        push(&mut conv, &turn, Source::Agent, texts(m.get("content")));
                    }
                    Some("toolResult") => {
                        let call = m
                            .get("toolCallId")
                            .and_then(Value::as_str)
                            .and_then(|id| calls.get(id))
                            .cloned()
                            .or_else(|| m.get("toolName").and_then(Value::as_str).map(String::from))
                            .unwrap_or_else(|| "tool".into());
                        push(
                            &mut conv,
                            &turn,
                            Source::Tool(call),
                            texts(m.get("content")),
                        );
                    }
                    Some("bashExecution") => {
                        let cmd = m.get("command").and_then(Value::as_str).unwrap_or_default();
                        let out = m.get("output").and_then(Value::as_str).unwrap_or_default();
                        push(
                            &mut conv,
                            &turn,
                            Source::Command(cmd.to_string()),
                            out.to_string(),
                        );
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
    conv
}

/// A message's text: a plain string, or its text parts joined.
fn texts(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|p| p.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
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

/// `bash: cargo test`, `read: src/main.rs`, or just the tool name.
fn describe(name: &str, args: Option<&Value>) -> String {
    let arg = ["command", "path", "file_path", "pattern", "url", "query"]
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

    /// Shapes from a real Pi 1.3.0 session (fake model), texts shortened;
    /// plus a branch: `old` was answered, then the user went back to `u1`'s
    /// answer and asked `u2` instead.
    const JSONL: &str = r#"{"type":"session","version":3,"id":"s1","timestamp":"2026-10-21T02:54:03.551Z","cwd":"/work/app"}
{"type":"model_change","id":"m1","parentId":null,"timestamp":"t","provider":"fake","modelId":"fake-model"}
{"type":"thinking_level_change","id":"k1","parentId":"m1","timestamp":"t","thinkingLevel":"off"}
{"type":"message","id":"sy","parentId":"k1","timestamp":"t","message":{"role":"system","content":"","sections":{"preamble":"You are an expert coding assistant"}}}
{"type":"message","id":"u1","parentId":"sy","timestamp":"t","message":{"role":"user","content":[{"type":"text","text":"run a tool please"}],"timestamp":1}}
{"type":"message","id":"a1","parentId":"u1","timestamp":"t","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"},{"type":"toolCall","id":"call_1","name":"bash","arguments":{"command":"ls"}}],"provider":"fake","model":"fake-model"}}
{"type":"message","id":"r1","parentId":"a1","timestamp":"t","message":{"role":"toolResult","toolCallId":"call_1","toolName":"bash","content":[{"type":"text","text":"main.rs"}],"isError":false}}
{"type":"message","id":"a2","parentId":"r1","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"**main.rs** is the entry point."}],"provider":"fake","model":"fake-model"}}
{"type":"message","id":"old","parentId":"a2","timestamp":"t","message":{"role":"user","content":"a question left behind"}}
{"type":"message","id":"oa","parentId":"old","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"an answer left behind"}]}}
{"type":"message","id":"u2","parentId":"a2","timestamp":"t","message":{"role":"user","content":[{"type":"text","text":"and now?"}]}}
{"type":"message","id":"b1","parentId":"u2","timestamp":"t","message":{"role":"bashExecution","command":"git status","output":"clean","exitCode":0}}
{"type":"session_info","id":"n1","parentId":"b1","timestamp":"t","name":"Fix it"}
not json
"#;

    #[test]
    fn parses_the_branch_on_screen() {
        let c = parse("s1", JSONL);
        let got: Vec<(Source, &str, Option<&str>)> = c
            .items
            .iter()
            .map(|i| (i.source.clone(), i.text.as_str(), i.turn.as_deref()))
            .collect();
        assert_eq!(
            got,
            vec![
                (Source::User, "run a tool please", Some("u1")),
                (Source::Tool("bash: ls".into()), "main.rs", Some("u1")),
                (Source::Agent, "**main.rs** is the entry point.", Some("u1")),
                (Source::User, "and now?", Some("u2")),
                (Source::Command("git status".into()), "clean", Some("u2")),
            ]
        );
        assert_eq!(c.model.as_deref(), Some("fake/fake-model"));
        assert_eq!(c.title.as_deref(), Some("Fix it"));
    }

    #[test]
    fn folder_of_a_directory() {
        assert_eq!(
            folder_name("/tmp/user-1000/-home-u/p0/proj"),
            "--tmp-user-1000--home-u-p0-proj--"
        );
        assert_eq!(folder_name("C:\\work\\app"), "--C--work-app--");
    }

    #[test]
    fn finds_the_session_written_last() {
        let root = std::env::temp_dir().join(format!("peekme-pi-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join(folder_name("/work/app"));
        std::fs::create_dir_all(&dir).unwrap();
        let header = |cwd: &str| {
            format!("{{\"type\":\"session\",\"version\":3,\"id\":\"x\",\"cwd\":\"{cwd}\"}}\n")
        };
        let a = dir.join("2026-10-21T01-00-00-000Z_aaa.jsonl");
        let b = dir.join("2026-10-21T02-00-00-000Z_bbb.jsonl");
        std::fs::write(&b, header("/work/app")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        // Older by name, written last: a resumed session.
        std::fs::write(&a, header("/work/app")).unwrap();
        assert_eq!(find(&root, "/work/app"), Some(a.clone()));
        // A file of another directory that maps to the same folder is not ours.
        std::fs::write(&a, header("/work-app")).unwrap();
        assert_eq!(find(&root, "/work/app"), Some(b));
        assert_eq!(find(&root, "/elsewhere"), None);
        assert_eq!(load(&a).unwrap().id, "aaa");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_session_dir_from_the_command_line() {
        let cwd = Path::new("/work/app");
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            sessions_dir(&args(&["--session-dir", "s"]), cwd),
            Some(PathBuf::from("/work/app/s"))
        );
        assert_eq!(
            sessions_dir(&args(&["--session-dir=/abs"]), cwd),
            Some(PathBuf::from("/abs"))
        );
        assert_eq!(expand("~/x", Some(Path::new("/h"))), PathBuf::from("/h/x"));
    }
}
