//! Explanations through Codex's own app-server: the user's existing login, an
//! ephemeral thread on a small model, streamed back into the peek box.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::context::{self, ConvItem, Conversation, Hit, ScreenSel, Source};

/// Streamed progress of one explanation.
#[derive(Debug)]
pub enum Progress {
    /// Model name and where the context came from.
    Started {
        model: String,
        source: &'static str,
    },
    Delta(String),
    Done,
    Failed(String),
}

pub struct Request {
    /// The screen text with the selected range.
    pub screen: ScreenSel,
    pub cwd: String,
    pub max_lines: usize,
    /// Explain with the whole conversation (a forked thread) instead of the
    /// compact context.
    pub deep: bool,
}

type Pending = Mutex<HashMap<u64, Sender<Result<Value, String>>>>;

pub struct AppServer {
    stdin: Mutex<ChildStdin>,
    next_id: AtomicU64,
    pending: Arc<Pending>,
    subscribers: Arc<Mutex<HashMap<String, Sender<Value>>>>,
    child: Mutex<Child>,
    model: OnceLock<Option<String>>,
}

impl AppServer {
    pub fn start() -> Result<Arc<Self>> {
        let mut child = Command::new(codex_bin())
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("could not start `codex app-server`")?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let server = Arc::new(Self {
            stdin: Mutex::new(stdin),
            next_id: AtomicU64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            subscribers: Arc::new(Mutex::new(HashMap::new())),
            child: Mutex::new(child),
            model: OnceLock::new(),
        });
        let reader = server.clone();
        std::thread::spawn(move || reader.read_loop(BufReader::new(stdout)));
        server.request(
            "initialize",
            json!({"clientInfo": {"name": "peekme", "version": env!("CARGO_PKG_VERSION")}}),
        )?;
        server.notify("initialized", Value::Null)?;
        Ok(server)
    }

    fn read_loop(&self, stdout: impl BufRead) {
        for line in stdout.lines() {
            let Ok(line) = line else { break };
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            match (msg.get("id"), msg.get("method")) {
                (Some(id), None) => {
                    let Some(id) = id.as_u64() else { continue };
                    if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
                        let res = match msg.get("error") {
                            Some(e) => Err(e
                                .get("message")
                                .and_then(Value::as_str)
                                .unwrap_or("error")
                                .to_string()),
                            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                        };
                        let _ = tx.send(res);
                    }
                }
                (Some(id), Some(_)) => {
                    // A request from the server (e.g. an approval). The explainer
                    // never needs one, so refuse it.
                    let _ = self.send(&json!({"id": id, "error": {"code": -32601, "message": "not supported by peekme"}}));
                }
                (None, Some(_)) => {
                    let thread = msg.pointer("/params/threadId").and_then(Value::as_str);
                    if let Some(t) = thread
                        && let Some(tx) = self.subscribers.lock().unwrap().get(t)
                    {
                        let _ = tx.send(msg);
                    }
                }
                _ => {}
            }
        }
        // Server gone: fail everything that is still waiting.
        for (_, tx) in self.pending.lock().unwrap().drain() {
            let _ = tx.send(Err("codex app-server exited".into()));
        }
        self.subscribers.lock().unwrap().clear();
    }

    fn send(&self, msg: &Value) -> Result<()> {
        let mut stdin = self.stdin.lock().unwrap();
        writeln!(stdin, "{msg}")?;
        stdin.flush()?;
        Ok(())
    }

    fn notify(&self, method: &str, params: Value) -> Result<()> {
        let mut msg = json!({"method": method});
        if !params.is_null() {
            msg["params"] = params;
        }
        self.send(&msg)
    }

    pub fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.send(&json!({"id": id, "method": method, "params": params}))?;
        match rx.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => bail!("{method}: {e}"),
            Err(_) => {
                self.pending.lock().unwrap().remove(&id);
                bail!("{method}: no answer from codex app-server")
            }
        }
    }

    fn subscribe(&self, thread: &str) -> Receiver<Value> {
        let (tx, rx) = mpsc::channel();
        self.subscribers
            .lock()
            .unwrap()
            .insert(thread.to_string(), tx);
        rx
    }

    fn unsubscribe(&self, thread: &str) {
        self.subscribers.lock().unwrap().remove(thread);
    }

    /// The model used for explanations: `PEEKME_MODEL`, else the account's
    /// model described as fast/efficient, else Codex's default.
    fn model(&self) -> Option<String> {
        self.model
            .get_or_init(|| {
                if let Ok(m) = std::env::var("PEEKME_MODEL") {
                    return Some(m);
                }
                let list = self.request("model/list", json!({})).ok()?;
                let models = list.get("data")?.as_array()?;
                let score = |m: &Value| {
                    let id = m
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_lowercase();
                    let desc = m
                        .get("description")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_lowercase();
                    let mut s = 0;
                    for (word, w) in [
                        ("mini", 3),
                        ("nano", 3),
                        ("fast", 2),
                        ("efficient", 2),
                        ("small", 2),
                        ("luna", 1),
                    ] {
                        if id.contains(word) || desc.contains(word) {
                            s += w;
                        }
                    }
                    s
                };
                models
                    .iter()
                    .filter(|m| !m.get("hidden").and_then(Value::as_bool).unwrap_or(false))
                    .max_by_key(|m| score(m))
                    .filter(|m| score(m) > 0)
                    .and_then(|m| m.get("id").and_then(Value::as_str).map(String::from))
            })
            .clone()
    }

    pub fn shutdown(&self) {
        let _ = self.child.lock().unwrap().kill();
    }
}

fn codex_bin() -> String {
    std::env::var("PEEKME_CODEX_BIN").unwrap_or_else(|_| {
        crate::launch::find_real_codex()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "codex".into())
    })
}

/// Run one explanation, reporting progress on `tx`. Blocks; call from a thread.
pub fn explain(server: &AppServer, req: Request, tx: impl Fn(Progress)) {
    if let Err(e) = run(server, req, &tx) {
        tx(Progress::Failed(e.to_string()));
    }
}

fn run(server: &AppServer, req: Request, tx: &impl Fn(Progress)) -> Result<()> {
    let (conv, hit) = find_conversation(server, &req);
    let built = context::build(&req.cwd, conv.as_ref(), hit, &req.screen);
    let model = server.model();
    let developer = format!(
        "You are \"peek\", an explainer embedded in a terminal. The user highlighted a fragment of text \
         (marked {open}like this{close}) and wants to understand it. Explain what the fragment means where it \
         appears: define the terms, name the specific thing it refers to when the context shows it, say what \
         any code does there, and why it matters for the user's task. At most {lines} short lines. No \
         preamble, no headings. Do not use tools, do not run commands, do not read files: answer from the \
         provided context and general knowledge only.",
        open = context::OPEN,
        close = context::CLOSE,
        lines = req.max_lines
    );

    let deep_thread = conv.as_ref().filter(|_| req.deep);
    let (method, mut params, source) = match deep_thread {
        Some(conv) => {
            let mut p = json!({
                "threadId": conv.id,
                "ephemeral": true,
                // Required for ephemeral forks; it only trims the response.
                "excludeTurns": true,
                "sandbox": "read-only",
                "approvalPolicy": "never",
                "developerInstructions": developer,
            });
            if let Some(turn) = hit.and_then(|h| conv.items[h.item].turn.clone()) {
                p["lastTurnId"] = json!(turn);
            }
            ("thread/fork", p, "full conversation")
        }
        None => (
            "thread/start",
            json!({
                "ephemeral": true,
                "cwd": req.cwd,
                "sandbox": "read-only",
                "approvalPolicy": "never",
                "developerInstructions": developer,
                "baseInstructions": "You explain short text selections concisely for software developers.",
            }),
            if built.label == "conversation" {
                "conversation"
            } else {
                "screen"
            },
        ),
    };
    if let Some(m) = &model {
        params["model"] = json!(m);
    }
    let started = server.request(method, params)?;
    let thread = started
        .pointer("/thread/id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("{method} returned no thread id"))?
        .to_string();
    let model_name = started
        .get("model")
        .and_then(Value::as_str)
        .map(String::from)
        .or(model)
        .unwrap_or_default();
    if std::env::var_os("PEEKME_DEBUG_PROMPT").is_some() {
        let _ = std::fs::write(
            std::env::temp_dir().join("peekme-last-prompt.txt"),
            &built.prompt,
        );
    }
    tx(Progress::Started {
        model: model_name,
        source,
    });

    let events = server.subscribe(&thread);
    let result = (|| -> Result<()> {
        server.request(
            "turn/start",
            json!({"threadId": thread, "input": [{"type": "text", "text": built.prompt}], "effort": "low"}),
        )?;
        loop {
            let msg = events
                .recv_timeout(Duration::from_secs(90))
                .map_err(|_| anyhow!("the explanation timed out"))?;
            let method = msg
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match method {
                "item/agentMessage/delta" => {
                    if let Some(d) = msg.pointer("/params/delta").and_then(Value::as_str) {
                        tx(Progress::Delta(d.to_string()));
                    }
                }
                "error" => {
                    if !msg
                        .pointer("/params/willRetry")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                    {
                        let m = msg
                            .pointer("/params/error/message")
                            .and_then(Value::as_str)
                            .unwrap_or("model error");
                        bail!("{m}");
                    }
                }
                "turn/completed" => {
                    let status = msg
                        .pointer("/params/turn/status")
                        .and_then(Value::as_str)
                        .unwrap_or("completed");
                    if status == "failed" {
                        let m = msg
                            .pointer("/params/turn/error/message")
                            .and_then(Value::as_str)
                            .unwrap_or("turn failed");
                        bail!("{m}");
                    }
                    tx(Progress::Done);
                    return Ok(());
                }
                _ => {}
            }
        }
    })();
    server.unsubscribe(&thread);
    result
}

/// The Codex conversation running in this directory, and where the selection
/// is in it. Tries the most recent threads; the first one containing the
/// selection wins, otherwise the most recent is still used for its outline.
fn find_conversation(server: &AppServer, req: &Request) -> (Option<Conversation>, Option<Hit>) {
    let Ok(list) = server.request(
        "thread/list",
        json!({"cwd": req.cwd, "limit": 3, "sortKey": "recency_at", "sortDirection": "desc"}),
    ) else {
        return (None, None);
    };
    let threads = list
        .get("data")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut first = None;
    for thread in &threads {
        let Some(conv) = fetch_conversation(server, thread) else {
            continue;
        };
        if let Some(hit) = context::find(&conv, &req.screen) {
            return (Some(conv), Some(hit));
        }
        first.get_or_insert(conv);
    }
    (first, None)
}

const MAX_ITEMS: usize = 5000;

fn fetch_conversation(server: &AppServer, thread: &Value) -> Option<Conversation> {
    let id = thread.get("id")?.as_str()?.to_string();
    let title = ["name", "preview"]
        .iter()
        .find_map(|k| {
            thread
                .get(*k)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .map(String::from);
    let mut items = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let mut params = json!({"threadId": id, "limit": 500, "sortDirection": "desc"});
        if let Some(c) = &cursor {
            params["cursor"] = json!(c);
        }
        let page = server.request("thread/items/list", params).ok()?;
        for entry in page.get("data")?.as_array()? {
            if let Some(item) = conv_item(entry) {
                items.push(item);
            }
        }
        cursor = page
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(String::from);
        if cursor.is_none() || items.len() >= MAX_ITEMS {
            break;
        }
    }
    items.reverse();
    Some(Conversation { id, title, items })
}

fn conv_item(entry: &Value) -> Option<ConvItem> {
    let item = entry.get("item")?;
    let turn = entry
        .get("turnId")
        .and_then(Value::as_str)
        .map(String::from);
    let (source, text) = match item.get("type")?.as_str()? {
        "agentMessage" => (Source::Agent, item.get("text")?.as_str()?.to_string()),
        "userMessage" => {
            let parts = item.get("content")?.as_array()?;
            let text = parts
                .iter()
                .filter_map(|p| p.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n");
            (Source::User, text)
        }
        "commandExecution" => {
            let cmd = item
                .get("command")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let out = item
                .get("aggregatedOutput")
                .and_then(Value::as_str)?
                .to_string();
            (Source::Command(cmd), out)
        }
        _ => return None,
    };
    (!text.trim().is_empty()).then_some(ConvItem { turn, source, text })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conv_items_from_app_server_json() {
        let agent =
            json!({"turnId": "t1", "item": {"type": "agentMessage", "id": "a", "text": "hello"}});
        let user = json!({"turnId": "t1", "item": {"type": "userMessage", "id": "u", "content": [{"type": "text", "text": "hi"}]}});
        let cmd = json!({"turnId": "t1", "item": {"type": "commandExecution", "id": "c", "command": "ls", "aggregatedOutput": "a.txt"}});
        let other = json!({"turnId": "t1", "item": {"type": "reasoning", "id": "r"}});
        assert_eq!(conv_item(&agent).unwrap().source, Source::Agent);
        assert_eq!(conv_item(&user).unwrap().text, "hi");
        assert_eq!(
            conv_item(&cmd).unwrap().source,
            Source::Command("ls".into())
        );
        assert!(conv_item(&other).is_none());
    }
}
