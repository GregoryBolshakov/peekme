//! Explanations through Kiro CLI's Agent Client Protocol server (`kiro-cli
//! acp`, the one Kiro's own screen and editors talk to), on the user's Kiro
//! login. The server runs in a directory of ours with an agent of ours that
//! has no tools; each explanation is its own session, which is closed and
//! deleted afterwards, so explanations never show up in the user's session
//! list.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use super::transcript;
use crate::agent::Agent;
use crate::context::{self, CLOSE, OPEN};
use crate::explain::{DEEP_ASK_ABOVE_TOKENS, Progress, Request};

/// Kiro wraps an agent's own prompt in its system prompt, where the model
/// does not take it as its instructions; so they go first in the message.
const SYSTEM: &str = "You are \"peek\", an explainer embedded in a terminal. The user highlighted a fragment \
     of text (marked ⟦like this⟧) in their Kiro CLI session and wants to understand it. Explain what the \
     fragment means where it appears: define the terms, name the specific thing it refers to when the \
     context shows it, say what any code does there, and why it matters for the user's task. No \
     preamble, no headings. Answer from the provided context and general knowledge only.";

/// Fast, and among the cheapest per credit on Kiro (0.4x of Auto in Kiro CLI 2.28).
const DEFAULT_MODEL: &str = "claude-haiku-4.5";

/// The name of our agent in Kiro.
const AGENT: &str = "peekme";

fn kiro_bin() -> String {
    std::env::var("PEEKME_KIRO_BIN").unwrap_or_else(|_| {
        crate::launch::find_real("kiro-cli")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "kiro-cli".into())
    })
}

/// `PEEKME_KIRO_MODEL`, else a small fast model.
fn model() -> (String, bool) {
    match std::env::var("PEEKME_KIRO_MODEL") {
        Ok(m) if !m.trim().is_empty() => (m.trim().to_string(), true),
        _ => (DEFAULT_MODEL.to_string(), false),
    }
}

/// Where the server runs: Kiro finds our agent in `.kiro/agents` there, and
/// sessions are filed under this directory, not the user's.
fn workspace() -> PathBuf {
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    std::env::temp_dir().join(format!("peekme-kiro-{uid}"))
}

fn write_agent(dir: &std::path::Path, model: &str) -> Result<()> {
    let agents = dir.join(".kiro/agents");
    std::fs::create_dir_all(&agents)?;
    let agent = json!({
        "name": AGENT,
        "description": "peekme's explainer: no tools",
        "prompt": "Explain the highlighted fragment the user asks about.",
        "tools": [],
        "allowedTools": [],
        "resources": [],
        "mcpServers": {},
        "useLegacyMcpJson": false,
        "model": model,
    });
    std::fs::write(
        agents.join(format!("{AGENT}.json")),
        serde_json::to_string_pretty(&agent)?,
    )?;
    Ok(())
}

type Reply = Result<Value, String>;
type Pending = Mutex<HashMap<u64, Sender<Reply>>>;

/// A running `kiro-cli acp`.
pub struct Server {
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    next_id: AtomicU64,
    pending: Arc<Pending>,
    /// Session updates by session id.
    subscribers: Arc<Mutex<HashMap<String, Sender<Value>>>>,
    dir: PathBuf,
}

impl Server {
    fn start() -> Result<Arc<Self>> {
        let dir = workspace();
        write_agent(&dir, &model().0).context("could not write peekme's Kiro agent")?;
        let mut child = Command::new(kiro_bin())
            .args(["acp", "--agent", AGENT])
            .current_dir(&dir)
            .env_remove(crate::launch::ACTIVE_ENV)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("could not start `kiro-cli acp`")?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let server = Arc::new(Self {
            stdin: Mutex::new(stdin),
            child: Mutex::new(child),
            next_id: AtomicU64::new(1),
            pending: Arc::default(),
            subscribers: Arc::default(),
            dir,
        });
        let reader = server.clone();
        std::thread::spawn(move || reader.read_loop(BufReader::new(stdout)));
        server.request(
            "initialize",
            json!({
                "protocolVersion": 1,
                "clientCapabilities": {
                    "fs": {"readTextFile": false, "writeTextFile": false},
                    "terminal": false,
                },
                "clientInfo": {"name": "peekme", "version": env!("CARGO_PKG_VERSION")},
            }),
        )?;
        Ok(server)
    }

    fn read_loop(&self, out: impl BufRead) {
        for line in out.lines() {
            let Ok(line) = line else { break };
            let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            match (msg.get("id"), msg.get("method")) {
                (Some(id), None) => {
                    let Some(id) = id.as_u64() else { continue };
                    if let Some(tx) = self.pending.lock().unwrap().remove(&id) {
                        let _ = tx.send(match msg.get("error") {
                            Some(e) => Err(error_text(e)),
                            None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                        });
                    }
                }
                (Some(id), Some(method)) => {
                    // A request from the agent. Ours has no tools, so a permission
                    // question means something went wrong: refuse it.
                    let reply = if method == "session/request_permission" {
                        json!({"jsonrpc": "2.0", "id": id,
                            "result": {"outcome": {"outcome": "cancelled"}}})
                    } else {
                        json!({"jsonrpc": "2.0", "id": id,
                            "error": {"code": -32601, "message": "not supported by peekme"}})
                    };
                    let _ = self.send(&reply);
                }
                (None, Some(_)) => {
                    let sid = msg.pointer("/params/sessionId").and_then(Value::as_str);
                    if let Some(sid) = sid
                        && let Some(tx) = self.subscribers.lock().unwrap().get(sid)
                    {
                        let _ = tx.send(msg);
                    }
                }
                _ => {}
            }
        }
        for (_, tx) in self.pending.lock().unwrap().drain() {
            let _ = tx.send(Err("the Kiro server exited".into()));
        }
        self.subscribers.lock().unwrap().clear();
    }

    fn send(&self, msg: &Value) -> Result<()> {
        let mut stdin = self.stdin.lock().unwrap();
        writeln!(stdin, "{msg}")?;
        stdin.flush()?;
        Ok(())
    }

    /// Send a request; its answer arrives on the returned channel.
    fn start_request(&self, method: &str, params: Value) -> Result<Receiver<Reply>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        Ok(rx)
    }

    fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.start_request(method, params)?
            .recv_timeout(Duration::from_secs(60))
            .map_err(|_| anyhow!("Kiro did not answer `{method}`"))?
            .map_err(|e| anyhow!("{e}"))
    }

    fn subscribe(&self, sid: &str) -> Receiver<Value> {
        let (tx, rx) = mpsc::channel();
        self.subscribers.lock().unwrap().insert(sid.to_string(), tx);
        rx
    }

    fn unsubscribe(&self, sid: &str) {
        self.subscribers.lock().unwrap().remove(sid);
    }

    /// Close the session, then delete it with Kiro's own command (in the
    /// background: it takes a second or two). Closing first matters: an open
    /// session is written again after the delete.
    fn discard(&self, sid: &str) {
        let _ = self.request("_kiro.dev/session/terminate", json!({"sessionId": sid}));
        let (bin, dir, sid) = (kiro_bin(), self.dir.clone(), sid.to_string());
        std::thread::spawn(move || {
            let _ = Command::new(bin)
                .args(["chat", "--delete-session", &sid])
                .current_dir(dir)
                .env_remove(crate::launch::ACTIVE_ENV)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        });
    }

    fn is_alive(&self) -> bool {
        matches!(self.child.lock().unwrap().try_wait(), Ok(None))
    }

    fn shutdown(&self) {
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Kiro puts the useful part of an error in `data`.
fn error_text(e: &Value) -> String {
    e.get("data")
        .and_then(Value::as_str)
        .or_else(|| e.get("message").and_then(Value::as_str))
        .unwrap_or("error")
        .to_string()
}

/// The server shared by all peeks of a session, started on the first one and
/// started again if it went away.
#[derive(Default)]
pub struct Slot(Mutex<Option<Arc<Server>>>);

impl Slot {
    fn get(&self) -> Result<Arc<Server>> {
        let mut guard = self.0.lock().unwrap();
        if let Some(s) = guard.as_ref().filter(|s| s.is_alive()) {
            return Ok(s.clone());
        }
        if let Some(old) = guard.take() {
            old.shutdown();
        }
        let server = Server::start()?;
        *guard = Some(server.clone());
        Ok(server)
    }

    pub fn shutdown(&self) {
        if let Some(s) = self.0.lock().unwrap().take() {
            s.shutdown();
        }
    }
}

/// Everything the explainer is told, and where it came from; with the
/// compact prompt's size too when the whole conversation was asked for.
pub fn prompt(req: &Request) -> (context::Built, Option<usize>) {
    let pids = req
        .agent_pid
        .map(crate::launch::process_tree)
        .unwrap_or_default();
    let conv = transcript::config_dir()
        .and_then(|c| transcript::find(&c, &pids, &req.cwd))
        .and_then(|p| transcript::load(&p));
    let hit = conv.as_ref().and_then(|c| context::find(c, &req.screen));
    let compact = context::build(
        Agent::Kiro,
        &req.cwd,
        conv.as_ref(),
        hit,
        &req.screen,
        req.nested.as_ref(),
    );
    let (mut built, compact_len) = match (&conv, req.deep) {
        (Some(c), true) => (
            context::build_deep(Agent::Kiro, &req.cwd, c, hit, &req.screen),
            Some(compact.prompt.chars().count()),
        ),
        _ => (compact, None),
    };
    built.prompt = format!("{SYSTEM}\n\n{}", built.prompt);
    built.prompt.push_str(&format!(
        "Answer in at most {} short lines; keep the {OPEN}{CLOSE} marks out of the answer.\n",
        req.max_lines
    ));
    (built, compact_len)
}

pub fn explain(slot: &Slot, req: Request, tx: impl Fn(Progress)) {
    if let Err(e) = run(slot, req, &tx) {
        tx(Progress::Failed(format!("{e:#}")));
    }
}

fn run(slot: &Slot, req: Request, tx: &impl Fn(Progress)) -> Result<()> {
    let (built, compact_len) = prompt(&req);
    if let Some(compact) = compact_len
        && !req.force
    {
        // Rough tokens: text plus Kiro's own system prompt (about 4k without tools).
        let (deep, normal) = (built.prompt.chars().count() / 4 + 4000, compact / 4 + 4000);
        if deep > DEEP_ASK_ABOVE_TOKENS {
            tx(Progress::TooBig {
                tokens: deep,
                ratio: deep / normal,
            });
            return Ok(());
        }
    }
    if std::env::var_os("PEEKME_DEBUG_PROMPT").is_some() {
        let _ = std::fs::write(
            std::env::temp_dir().join("peekme-last-prompt.txt"),
            &built.prompt,
        );
    }
    let server = slot.get()?;
    match ask(&server, &built, None, tx) {
        // The default model is not on every plan: fall back to Kiro's Auto.
        Err(e) if !model().1 && !e.started && e.message.contains("is not available") => {
            ask(&server, &built, Some("auto"), tx).map_err(|e| anyhow!(e.message))
        }
        r => r.map_err(|e| anyhow!(e.message)),
    }
}

struct Failure {
    message: String,
    /// Some of the answer was shown already.
    started: bool,
}

/// One explanation in a new session, closed and deleted afterwards whatever happens.
fn ask(
    server: &Server,
    built: &context::Built,
    model_override: Option<&str>,
    tx: &impl Fn(Progress),
) -> Result<(), Failure> {
    let fail = |message: String, started: bool| Failure { message, started };
    let created = server
        .request(
            "session/new",
            json!({"cwd": server.dir.to_string_lossy(), "mcpServers": []}),
        )
        .map_err(|e| fail(format!("{e:#}"), false))?;
    let Some(sid) = created.get("sessionId").and_then(Value::as_str) else {
        return Err(fail("Kiro did not open a session".into(), false));
    };
    let events = server.subscribe(sid);
    let mut started = false;
    let result = (|| -> Result<()> {
        let mut model_name = created
            .pointer("/models/currentModelId")
            .and_then(Value::as_str)
            .unwrap_or("Kiro")
            .to_string();
        if let Some(m) = model_override {
            server.request("session/set_model", json!({"sessionId": sid, "modelId": m}))?;
            model_name = m.to_string();
        }
        let reply = server.start_request(
            "session/prompt",
            json!({"sessionId": sid, "prompt": [{"type": "text", "text": built.prompt}]}),
        )?;
        let deadline = Instant::now() + Duration::from_secs(90);
        let on_event = |msg: Value, started: &mut bool| {
            let update = msg
                .pointer("/params/update")
                .cloned()
                .unwrap_or(Value::Null);
            if update.get("sessionUpdate").and_then(Value::as_str) != Some("agent_message_chunk") {
                return;
            }
            if let Some(t) = update.pointer("/content/text").and_then(Value::as_str) {
                if !*started {
                    *started = true;
                    tx(Progress::Started {
                        model: model_name.clone(),
                        source: built.label,
                    });
                }
                tx(Progress::Delta(t.to_string()));
            }
        };
        loop {
            match events.recv_timeout(Duration::from_millis(50)) {
                Ok(msg) => {
                    on_event(msg, &mut started);
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => bail!("the Kiro server exited"),
                Err(RecvTimeoutError::Timeout) => {}
            }
            match reply.try_recv() {
                Ok(Ok(r)) => {
                    // Updates sent before the answer are all queued by now.
                    while let Ok(msg) = events.try_recv() {
                        on_event(msg, &mut started);
                    }
                    if r.get("stopReason").and_then(Value::as_str) == Some("refusal") {
                        bail!("Kiro declined to answer this one");
                    }
                    tx(Progress::Done);
                    return Ok(());
                }
                Ok(Err(e)) => bail!("{}", with_login_hint(e)),
                Err(mpsc::TryRecvError::Disconnected) => bail!("the Kiro server exited"),
                Err(mpsc::TryRecvError::Empty) => {}
            }
            if Instant::now() > deadline {
                bail!("the explanation timed out");
            }
        }
    })();
    server.unsubscribe(sid);
    server.discard(sid);
    result.map_err(|e| fail(format!("{e:#}"), started))
}

/// Point at the fix when Kiro says the login is missing or expired.
fn with_login_hint(e: String) -> String {
    let lower = e.to_lowercase();
    if [
        "login",
        "log in",
        "sign in",
        "not authenticated",
        "unauthorized",
        "expired",
    ]
    .iter()
    .any(|w| lower.contains(w))
    {
        format!("{e} Run `kiro-cli login`.")
    } else {
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_name_the_fix() {
        assert_eq!(
            error_text(
                &json!({"code": -32603, "message": "Internal error", "data": "The model 'x' is not available."})
            ),
            "The model 'x' is not available."
        );
        assert_eq!(
            error_text(&json!({"message": "Method not found"})),
            "Method not found"
        );
        assert!(
            with_login_hint("Your session has expired.".into()).ends_with("Run `kiro-cli login`.")
        );
        assert_eq!(with_login_hint("quota".into()), "quota");
    }
}
