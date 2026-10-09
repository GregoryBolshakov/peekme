//! Explanations through GitHub Copilot CLI's headless server, the same
//! JSON-RPC the Copilot SDK uses (`copilot --headless --stdio`), on the user's
//! Copilot login. Each explanation is its own session with no tools and a
//! short system prompt; it is deleted afterwards, so explanations never show
//! up in the user's session list.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use super::transcript;
use crate::agent::Agent;
use crate::context::{self, CLOSE, OPEN};
use crate::explain::{DEEP_ASK_ABOVE_TOKENS, Progress, Request};

const SYSTEM: &str = "You are \"peek\", an explainer embedded in a terminal. The user highlighted a fragment \
     of text (marked ⟦like this⟧) in their GitHub Copilot CLI session and wants to understand it. Explain \
     what the fragment means where it appears: define the terms, name the specific thing it refers to when \
     the context shows it, say what any code does there, and why it matters for the user's task. No \
     preamble, no headings. Answer from the provided context and general knowledge only.";

fn copilot_bin() -> String {
    std::env::var("PEEKME_COPILOT_BIN").unwrap_or_else(|_| {
        crate::launch::find_real("copilot")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "copilot".into())
    })
}

/// `PEEKME_COPILOT_MODEL`, else Copilot's own choice (Auto).
fn model() -> Option<String> {
    std::env::var("PEEKME_COPILOT_MODEL")
        .ok()
        .filter(|m| !m.trim().is_empty())
}

type Pending = Mutex<HashMap<u64, Sender<Result<Value, String>>>>;

/// A running `copilot --headless --stdio`.
pub struct Server {
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    next_id: AtomicU64,
    pending: Arc<Pending>,
    /// Session events by session id.
    subscribers: Arc<Mutex<HashMap<String, Sender<Value>>>>,
}

impl Server {
    fn start() -> Result<Arc<Self>> {
        let mut child = Command::new(copilot_bin())
            .args([
                "--headless",
                "--stdio",
                "--no-auto-update",
                "--log-level",
                "none",
            ])
            .current_dir(std::env::temp_dir())
            .env_remove(crate::launch::ACTIVE_ENV)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("could not start `copilot --headless`")?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let server = Arc::new(Self {
            stdin: Mutex::new(stdin),
            child: Mutex::new(child),
            next_id: AtomicU64::new(1),
            pending: Arc::default(),
            subscribers: Arc::default(),
        });
        let reader = server.clone();
        std::thread::spawn(move || reader.read_loop(BufReader::new(stdout)));
        server.request("ping", json!({"message": "peekme"}))?;
        Ok(server)
    }

    fn read_loop(&self, mut out: impl BufRead) {
        while let Some(msg) = read_frame(&mut out) {
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
                    // A request from the server (a permission, a question to the
                    // user). The explainer has no tools and never needs one.
                    let _ = self.send(&json!({"jsonrpc": "2.0", "id": id,
                        "error": {"code": -32601, "message": "not supported by peekme"}}));
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
            let _ = tx.send(Err("the Copilot server exited".into()));
        }
        self.subscribers.lock().unwrap().clear();
    }

    fn send(&self, msg: &Value) -> Result<()> {
        let body = msg.to_string();
        let mut stdin = self.stdin.lock().unwrap();
        write!(stdin, "Content-Length: {}\r\n\r\n{body}", body.len())?;
        stdin.flush()?;
        Ok(())
    }

    fn request(&self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        rx.recv_timeout(Duration::from_secs(60))
            .map_err(|_| anyhow!("Copilot did not answer `{method}`"))?
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

    fn is_alive(&self) -> bool {
        matches!(self.child.lock().unwrap().try_wait(), Ok(None))
    }

    fn shutdown(&self) {
        let mut child = self.child.lock().unwrap();
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// One JSON-RPC message with `Content-Length` framing, or None at the end.
fn read_frame(out: &mut impl BufRead) -> Option<Value> {
    loop {
        let mut len = None;
        loop {
            let mut line = String::new();
            if out.read_line(&mut line).ok()? == 0 {
                return None;
            }
            let line = line.trim();
            if line.is_empty() {
                break;
            }
            if let Some((k, v)) = line.split_once(':')
                && k.eq_ignore_ascii_case("content-length")
            {
                len = v.trim().parse::<usize>().ok();
            }
        }
        let Some(len) = len else { continue };
        let mut body = vec![0u8; len];
        out.read_exact(&mut body).ok()?;
        if let Ok(v) = serde_json::from_slice(&body) {
            return Some(v);
        }
    }
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
    prompt_and_model(req).0
}

/// The prompt, and for a typed question the model to answer it:
/// `PEEKME_COPILOT_ASK_MODEL`, else the chat's own model (None: Auto).
fn prompt_and_model(req: &Request) -> ((context::Built, Option<usize>), Option<String>) {
    let pids = req
        .agent_pid
        .map(crate::launch::process_tree)
        .unwrap_or_default();
    let conv = transcript::config_dir()
        .and_then(|c| transcript::find(&c, &pids, &req.cwd))
        .and_then(|p| transcript::load(&p));
    let hit = conv.as_ref().and_then(|c| context::find(c, &req.screen));
    if let Some(q) = &req.question {
        let mut built = match &conv {
            Some(c) if req.nested.is_none() => {
                context::build_deep(Agent::Copilot, &req.cwd, c, hit, &req.screen)
            }
            _ => context::build(
                Agent::Copilot,
                &req.cwd,
                conv.as_ref(),
                hit,
                &req.screen,
                req.nested.as_ref(),
            ),
        };
        context::with_question(&mut built, &req.screen, q);
        built.prompt.push_str(&format!(
            "Keep the {OPEN}{CLOSE} marks out of the answer.\n"
        ));
        let model = std::env::var("PEEKME_COPILOT_ASK_MODEL")
            .ok()
            .filter(|m| !m.trim().is_empty())
            .or_else(|| conv.and_then(|c| c.model))
            .filter(|m| m != "auto");
        return ((built, None), model);
    }
    let compact = context::build(
        Agent::Copilot,
        &req.cwd,
        conv.as_ref(),
        hit,
        &req.screen,
        req.nested.as_ref(),
    );
    let (mut built, compact_len) = match (&conv, req.deep) {
        (Some(c), true) => (
            context::build_deep(Agent::Copilot, &req.cwd, c, hit, &req.screen),
            Some(compact.prompt.chars().count()),
        ),
        _ => (compact, None),
    };
    built.prompt.push_str(&format!(
        "Answer in at most {} short lines; keep the {OPEN}{CLOSE} marks out of the answer.\n",
        req.max_lines
    ));
    ((built, compact_len), None)
}

pub fn explain(slot: &Slot, req: Request, tx: impl Fn(Progress)) {
    if let Err(e) = run(slot, req, &tx) {
        tx(Progress::Failed(format!("{e:#}")));
    }
}

fn run(slot: &Slot, req: Request, tx: &impl Fn(Progress)) -> Result<()> {
    let ((built, compact_len), ask_model) = prompt_and_model(&req);
    let asking = req.question.is_some();
    let model = || if asking { ask_model.clone() } else { model() };
    if let Some(compact) = compact_len
        && !req.force
    {
        // Rough tokens: text plus a small fixed overhead (no tools, short system prompt).
        let (deep, normal) = (built.prompt.chars().count() / 4 + 300, compact / 4 + 300);
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
    let auth = server.request("auth.getStatus", json!({}))?;
    if auth.get("isAuthenticated").and_then(Value::as_bool) != Some(true) {
        bail!("Copilot CLI is not signed in. Run `copilot login`.");
    }

    let sid = uuid_v4();
    let events = server.subscribe(&sid);
    let result = (|| -> Result<()> {
        let system = if asking {
            context::ask_system(Agent::Copilot)
        } else {
            SYSTEM.to_string()
        };
        let mut params = json!({
            "sessionId": sid,
            "clientName": "peekme",
            "availableTools": [],
            "streaming": true,
            "systemMessage": {"mode": "replace", "content": system},
        });
        if !asking {
            params["reasoningEffort"] = json!("none");
        }
        if let Some(m) = model() {
            params["model"] = json!(m);
        }
        server.request("session.create", params)?;
        server.request(
            "session.send",
            json!({"sessionId": sid, "prompt": built.prompt}),
        )?;
        let mut model_name = model().unwrap_or_else(|| "Copilot".into());
        let mut started = false;
        loop {
            let msg = events
                .recv_timeout(Duration::from_secs(90))
                .map_err(|_| anyhow!("the explanation timed out"))?;
            let event = msg.pointer("/params/event").cloned().unwrap_or(Value::Null);
            let data = event.get("data").cloned().unwrap_or(Value::Null);
            match event.get("type").and_then(Value::as_str) {
                Some("session.auto_mode_resolved") => {
                    if let Some(m) = data.get("chosenModel").and_then(Value::as_str) {
                        model_name = m.to_string();
                    }
                }
                Some("assistant.message_delta") => {
                    if let Some(d) = data.get("deltaContent").and_then(Value::as_str) {
                        if !started {
                            started = true;
                            tx(Progress::Started {
                                model: model_name.clone(),
                                source: built.label,
                            });
                        }
                        tx(Progress::Delta(d.to_string()));
                    }
                }
                Some("assistant.message") if !started => {
                    // Not streamed: the whole answer at once.
                    if let Some(t) = data.get("content").and_then(Value::as_str) {
                        started = true;
                        tx(Progress::Started {
                            model: model_name.clone(),
                            source: built.label,
                        });
                        tx(Progress::Delta(t.to_string()));
                    }
                }
                Some("session.error") => {
                    let m = data
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("Copilot returned an error");
                    bail!("{m}");
                }
                Some("session.idle") => {
                    tx(Progress::Done);
                    return Ok(());
                }
                _ => {}
            }
        }
    })();
    server.unsubscribe(&sid);
    // Always remove the session, also after an error or a timeout.
    let _ = server.request("session.delete", json!({"sessionId": sid}));
    result
}

/// A random (version 4) UUID, for the session id.
fn uuid_v4() -> String {
    let mut b = [0u8; 16];
    if let Ok(mut f) = std::fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut b);
    }
    if b == [0u8; 16] {
        let t = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        b = (t ^ (u128::from(std::process::id()) << 64)).to_le_bytes();
    }
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &h[0..8],
        &h[8..12],
        &h[12..16],
        &h[16..20],
        &h[20..32]
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_and_uuids() {
        let bytes = b"Content-Length: 14\r\n\r\n{\"id\":1,\"a\":2}Content-Type: x\r\ncontent-length: 2\r\n\r\n{}";
        let mut r = BufReader::new(&bytes[..]);
        assert_eq!(read_frame(&mut r), Some(json!({"id": 1, "a": 2})));
        assert_eq!(read_frame(&mut r), Some(json!({})));
        assert_eq!(read_frame(&mut r), None);
        let u = uuid_v4();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
        assert_ne!(uuid_v4(), u);
    }
}
