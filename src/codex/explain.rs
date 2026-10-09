//! Explanations through Codex's own app-server: the user's existing login, an
//! ephemeral thread on a small model, streamed back into the peek box.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use crate::agent::Agent;
use crate::context::{self, ConvItem, Conversation, Hit, Source};
use crate::explain::{DEEP_ASK_ABOVE_TOKENS, Progress, Request};

type Pending = Mutex<HashMap<u64, Sender<Result<Value, String>>>>;

enum Writer {
    /// Our own `codex app-server` process.
    Stdio(ChildStdin),
    /// Codex's shared background server.
    Shared(super::ws::Sender),
}

pub struct AppServer {
    writer: Mutex<Writer>,
    next_id: AtomicU64,
    pending: Arc<Pending>,
    subscribers: Arc<Mutex<HashMap<String, Sender<Value>>>>,
    child: Mutex<Option<Child>>,
    model: OnceLock<Option<String>>,
    alive: AtomicBool,
}

impl AppServer {
    /// Connect to Codex's shared background server when it runs (no extra
    /// process, no start-up time), otherwise start our own `codex app-server`.
    pub fn start() -> Result<Arc<Self>> {
        if std::env::var_os("PEEKME_OWN_SERVER").is_none()
            && let Some(path) = shared_socket()
            && let Ok(server) = Self::start_shared(&path)
        {
            return Ok(server);
        }
        Self::start_own()
    }

    fn start_shared(path: &std::path::Path) -> Result<Arc<Self>> {
        let (tx, mut rx) = super::ws::connect(path)?;
        let server = Self::new(Writer::Shared(tx), None);
        let reader = server.clone();
        std::thread::spawn(move || reader.read_loop(std::iter::from_fn(move || rx.next_text())));
        server.handshake()?;
        Ok(server)
    }

    fn start_own() -> Result<Arc<Self>> {
        let mut child = Command::new(codex_bin())
            .arg("app-server")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("could not start `codex app-server`")?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let server = Self::new(Writer::Stdio(stdin), Some(child));
        let reader = server.clone();
        std::thread::spawn(move || {
            reader.read_loop(BufReader::new(stdout).lines().map_while(|l| l.ok()))
        });
        server.handshake()?;
        Ok(server)
    }

    fn new(writer: Writer, child: Option<Child>) -> Arc<Self> {
        Arc::new(Self {
            writer: Mutex::new(writer),
            next_id: AtomicU64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            subscribers: Arc::new(Mutex::new(HashMap::new())),
            child: Mutex::new(child),
            model: OnceLock::new(),
            alive: AtomicBool::new(true),
        })
    }

    fn handshake(&self) -> Result<()> {
        self.request(
            "initialize",
            json!({"clientInfo": {"name": "peekme", "version": env!("CARGO_PKG_VERSION")}}),
        )?;
        self.notify("initialized", Value::Null)
    }

    /// False once the connection or the process is gone.
    pub fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    fn read_loop(&self, messages: impl Iterator<Item = String>) {
        for line in messages {
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
        self.alive.store(false, Ordering::Relaxed);
        for (_, tx) in self.pending.lock().unwrap().drain() {
            let _ = tx.send(Err("codex app-server exited".into()));
        }
        self.subscribers.lock().unwrap().clear();
    }

    fn send(&self, msg: &Value) -> Result<()> {
        match &mut *self.writer.lock().unwrap() {
            Writer::Stdio(stdin) => {
                writeln!(stdin, "{msg}")?;
                stdin.flush()?;
            }
            Writer::Shared(ws) => ws.send_text(&msg.to_string())?,
        }
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

    /// Stop our own server process, or leave the shared one (it is Codex's).
    pub fn shutdown(&self) {
        if let Some(child) = self.child.lock().unwrap().as_mut() {
            let _ = child.kill();
        }
        if let Writer::Shared(ws) = &*self.writer.lock().unwrap() {
            ws.close();
        }
    }
}

/// Rough size of what a fork sends: the conversation text plus Codex's own
/// instructions and tools (about 8k tokens), and how many times a normal
/// explanation that is. `None` when it is small enough to just send.
fn too_big_for_deep(conv: &Conversation, prompt: &str) -> Option<(usize, usize)> {
    const OVERHEAD: usize = 8_000;
    let chars: usize = conv.items.iter().map(|i| i.text.chars().count()).sum();
    let tokens = chars / 4 + OVERHEAD;
    let normal = prompt.chars().count() / 4 + OVERHEAD;
    (tokens > DEEP_ASK_ABOVE_TOKENS).then(|| (tokens, tokens / normal))
}

/// Codex's shared app-server socket, if it exists.
fn shared_socket() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("CODEX_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| crate::launch::home().map(|h| h.join(".codex")))?;
    let path = home
        .join("app-server-control")
        .join("app-server-control.sock");
    path.exists().then_some(path)
}

/// The explainer connection shared by all peeks of a session, (re)started on
/// demand: connecting is lazy, so Codex has usually started its shared server
/// by the first Alt+P, and a lost connection is replaced on the next one.
#[derive(Default)]
pub struct Slot(Mutex<Option<Arc<AppServer>>>);

impl Slot {
    pub fn get(&self) -> Result<Arc<AppServer>> {
        let mut guard = self.0.lock().unwrap();
        if let Some(s) = guard.as_ref().filter(|s| s.is_alive()) {
            return Ok(s.clone());
        }
        if let Some(old) = guard.take() {
            old.shutdown();
        }
        let server = AppServer::start()?;
        *guard = Some(server.clone());
        Ok(server)
    }

    pub fn shutdown(&self) {
        if let Some(s) = self.0.lock().unwrap().take() {
            s.shutdown();
        }
    }
}

fn codex_bin() -> String {
    std::env::var("PEEKME_CODEX_BIN").unwrap_or_else(|_| {
        crate::launch::find_real("codex")
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
    let mut built = context::build(
        Agent::Codex,
        &req.cwd,
        conv.as_ref(),
        hit,
        &req.screen,
        req.nested.as_ref(),
    );
    let asking = req.question.is_some();
    // A question runs on the chat's own model (without a thread: Codex's
    // configured default).
    let model = match &req.question {
        Some(_) => std::env::var("PEEKME_CODEX_ASK_MODEL")
            .ok()
            .filter(|m| !m.trim().is_empty())
            .or_else(|| conv.as_ref().and_then(|c| c.model.clone())),
        None => server.model(),
    };
    if let Some(q) = &req.question {
        // The fork carries the conversation; the prompt only needs the passage.
        built = context::build_question(
            Agent::Codex,
            &req.cwd,
            conv.as_ref(),
            hit,
            &req.screen,
            req.nested.as_ref(),
            false,
            q,
        );
    }
    let developer = if asking {
        context::ask_system(Agent::Codex)
    } else {
        format!(
            "You are \"peek\", an explainer embedded in a terminal. The user highlighted a fragment of text \
         (marked {open}like this{close}) and wants to understand it. Explain what the fragment means where it \
         appears: define the terms, name the specific thing it refers to when the context shows it, say what \
         any code does there, and why it matters for the user's task. At most {lines} short lines. No \
         preamble, no headings. Do not use tools, do not run commands, do not read files: answer from the \
         provided context and general knowledge only.",
            open = context::OPEN,
            close = context::CLOSE,
            lines = req.max_lines
        )
    };

    if req.deep
        && !asking
        && !req.force
        && let Some(conv) = &conv
        && let Some((tokens, ratio)) = too_big_for_deep(conv, &built.prompt)
    {
        tx(Progress::TooBig { tokens, ratio });
        return Ok(());
    }
    let deep_thread = conv.as_ref().filter(|_| req.deep || asking);
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
            // A question may be about anything in the chat: the whole thread.
            if let Some(turn) = hit
                .and_then(|h| conv.items[h.item].turn.clone())
                .filter(|_| !asking)
            {
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
            if asking {
                json!({"threadId": thread, "input": [{"type": "text", "text": built.prompt}]})
            } else {
                json!({"threadId": thread, "input": [{"type": "text", "text": built.prompt}], "effort": "low"})
            },
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
    Some(Conversation {
        id,
        title,
        model: thread_model(thread),
        items,
    })
}

/// The model a thread runs on: the server says so for a loaded thread,
/// otherwise its rollout file does (`session_meta`, then each `turn_context`
/// after a `/model`).
fn thread_model(thread: &Value) -> Option<String> {
    if let Some(m) = thread.get("model").and_then(Value::as_str) {
        return Some(m.to_string());
    }
    let text = std::fs::read_to_string(thread.get("path")?.as_str()?).ok()?;
    rollout_model(&text)
}

fn rollout_model(rollout: &str) -> Option<String> {
    rollout
        .lines()
        .rev()
        .filter(|l| l.contains("\"model\""))
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| {
            matches!(
                v.get("type").and_then(Value::as_str),
                Some("session_meta" | "turn_context")
            )
        })
        .filter_map(|v| {
            v.pointer("/payload/model")
                .and_then(Value::as_str)
                .map(String::from)
        })
        .next()
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
    fn deep_mode_asks_only_for_big_chats() {
        let item = |n: usize| ConvItem {
            turn: None,
            source: Source::Agent,
            text: "x".repeat(n),
        };
        let small = Conversation {
            items: vec![item(20_000)],
            ..Default::default()
        };
        assert_eq!(too_big_for_deep(&small, &"p".repeat(4_000)), None);
        let big = Conversation {
            items: vec![item(400_000), item(100_000)],
            ..Default::default()
        };
        let (tokens, ratio) = too_big_for_deep(&big, &"p".repeat(4_000)).unwrap();
        assert_eq!(tokens, 133_000);
        assert_eq!(ratio, 14);
    }

    #[test]
    fn the_thread_model_is_the_last_one_in_the_rollout() {
        let rollout = r#"{"type":"session_meta","payload":{"id":"t","model":"gpt-6.1-sol"}}
{"type":"response_item","payload":{"type":"message","content":"the \"model\" said"}}
{"type":"turn_context","payload":{"model":"gpt-6-luna","effort":"high"}}
"#;
        assert_eq!(rollout_model(rollout).as_deref(), Some("gpt-6-luna"));
        assert_eq!(rollout_model("{}"), None);
        assert_eq!(
            thread_model(&json!({"model": "gpt-6.1-sol", "path": "/nonexistent"})).as_deref(),
            Some("gpt-6.1-sol")
        );
    }

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
