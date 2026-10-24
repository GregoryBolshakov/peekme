//! Explanations through Pi's RPC mode (`pi --mode rpc`, JSON lines on
//! stdin and stdout), on the providers the user signed in to in Pi. The
//! process keeps no session on disk, loads no tools, extensions, skills or
//! context files, and runs in a directory of ours; each explanation starts a
//! new in-memory session, so explanations never show up in `/resume`.
//!
//! A process holds one session at a time, and its events carry no session:
//! each explanation running gets a process of its own (a box inside a box
//! explains while the box it replaced still streams), and one is kept for
//! the next explanation.

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
use crate::explain::{DEEP_ASK_ABOVE_TOKENS, Parts, Progress, Request};

/// Pi's own system prompt is replaced by this one; the instructions for each
/// kind of answer go in the message.
const SYSTEM: &str = "You are \"peek\", an explainer embedded in a terminal. You have no tools. \
     Answer from the text you are given and general knowledge only.";

const EXPLAIN: &str = "The user highlighted a fragment of text (marked ⟦like this⟧) in their Pi \
     session and wants to understand it. Explain what the fragment means where it appears: define \
     the terms, name the specific thing it refers to when the context shows it, say what any code \
     does there, and why it matters for the user's task. No preamble, no headings.";

fn pi_bin() -> String {
    std::env::var("PEEKME_PI_BIN").unwrap_or_else(|_| {
        Agent::Pi
            .find()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "pi".into())
    })
}

/// `provider/id` split; an id alone keeps the provider open.
fn split_model(m: &str) -> (Option<&str>, &str) {
    match m.split_once('/') {
        Some((p, id)) if !p.is_empty() && !id.is_empty() => (Some(p), id),
        _ => (None, m),
    }
}

/// Where the process runs: an empty directory of ours, so Pi finds no
/// project files there.
fn workspace() -> PathBuf {
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    std::env::temp_dir().join(format!("peekme-pi-{uid}"))
}

type Reply = Result<Value, String>;

/// A running `pi --mode rpc`.
pub struct Server {
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    next_id: AtomicU64,
    pending: Arc<Mutex<HashMap<String, Sender<Reply>>>>,
    /// Session events (they carry no id): to the one explanation running.
    events: Arc<Mutex<Option<Sender<Value>>>>,
}

impl Server {
    fn start() -> Result<Arc<Self>> {
        let dir = workspace();
        std::fs::create_dir_all(&dir).context("could not create peekme's Pi folder")?;
        let mut child = Command::new(pi_bin())
            .args([
                "--mode",
                "rpc",
                "--no-session",
                "--no-tools",
                "--no-extensions",
                "--no-skills",
                "--no-prompt-templates",
                "--no-context-files",
                "--no-mcp",
                "--no-approve",
                "--thinking",
                "off",
                "--system-prompt",
                SYSTEM,
            ])
            .current_dir(&dir)
            .env_remove(crate::launch::ACTIVE_ENV)
            .env("PI_SKIP_VERSION_CHECK", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("could not start `pi --mode rpc`")?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let server = Arc::new(Self {
            stdin: Mutex::new(stdin),
            child: Mutex::new(child),
            next_id: AtomicU64::new(1),
            pending: Arc::default(),
            events: Arc::default(),
        });
        let reader = server.clone();
        std::thread::spawn(move || reader.read_loop(BufReader::new(stdout)));
        Ok(server)
    }

    /// Records are split on `\n` only: JSON strings may hold U+2028.
    fn read_loop(&self, out: impl BufRead) {
        for line in out.split(b'\n') {
            let Ok(line) = line else { break };
            let Ok(msg) = serde_json::from_slice::<Value>(&line) else {
                continue;
            };
            if msg.get("type").and_then(Value::as_str) == Some("response") {
                let Some(id) = msg.get("id").and_then(Value::as_str) else {
                    continue;
                };
                if let Some(tx) = self.pending.lock().unwrap().remove(id) {
                    let ok = msg.get("success").and_then(Value::as_bool) == Some(true);
                    let _ = tx.send(if ok {
                        Ok(msg.get("data").cloned().unwrap_or(Value::Null))
                    } else {
                        Err(msg
                            .get("error")
                            .and_then(Value::as_str)
                            .unwrap_or("error")
                            .to_string())
                    });
                }
            } else if msg.get("type").and_then(Value::as_str) == Some("extension_ui_request") {
                // Nothing of ours asks; answer anything that does with "no".
                if let Some(id) = msg.get("id").and_then(Value::as_str) {
                    let _ = self.send(&json!({"type": "extension_ui_response", "id": id,
                        "cancelled": true}));
                }
            } else if let Some(tx) = self.events.lock().unwrap().as_ref() {
                let _ = tx.send(msg);
            }
        }
        for (_, tx) in self.pending.lock().unwrap().drain() {
            let _ = tx.send(Err("Pi exited".into()));
        }
        self.events.lock().unwrap().take();
    }

    fn send(&self, msg: &Value) -> Result<()> {
        let mut stdin = self.stdin.lock().unwrap();
        writeln!(stdin, "{msg}")?;
        stdin.flush()?;
        Ok(())
    }

    /// Send a command; its response arrives on the returned channel.
    fn start_request(&self, mut cmd: Value) -> Result<Receiver<Reply>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed).to_string();
        cmd["id"] = json!(id);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        self.send(&cmd)?;
        Ok(rx)
    }

    fn request(&self, cmd: Value) -> Result<Value> {
        let what = cmd["type"].as_str().unwrap_or("command").to_string();
        self.start_request(cmd)?
            .recv_timeout(Duration::from_secs(30))
            .map_err(|_| anyhow!("Pi did not answer `{what}`"))?
            .map_err(|e| anyhow!("{e}"))
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

/// The processes of a session: one for each explanation running, started
/// when none waits, and one kept waiting for the next explanation.
#[derive(Default)]
pub struct Slot(Mutex<Servers>);

#[derive(Default)]
struct Servers {
    idle: Vec<Arc<Server>>,
    busy: Vec<Arc<Server>>,
    closed: bool,
}

/// Processes kept waiting once their explanation is done; the others stop.
const SPARE: usize = 1;

impl Slot {
    fn take(&self) -> Result<Lease<'_>> {
        let waiting = loop {
            let Some(s) = self.0.lock().unwrap().idle.pop() else {
                break None;
            };
            if s.is_alive() {
                break Some(s);
            }
            s.shutdown();
        };
        let server = match waiting {
            Some(s) => s,
            None => Server::start()?,
        };
        let mut servers = self.0.lock().unwrap();
        if servers.closed {
            drop(servers);
            server.shutdown();
            bail!("peekme is closing");
        }
        servers.busy.push(server.clone());
        Ok(Lease {
            slot: self,
            server,
            clean: false,
        })
    }

    pub fn shutdown(&self) {
        let gone = {
            let mut servers = self.0.lock().unwrap();
            servers.closed = true;
            let mut gone = std::mem::take(&mut servers.idle);
            gone.append(&mut servers.busy);
            gone
        };
        for s in gone {
            s.shutdown();
        }
    }
}

/// A process explaining; back to the slot when dropped. Only one whose
/// explanation ran to its end is kept: an aborted one may still send events
/// of the old answer.
struct Lease<'a> {
    slot: &'a Slot,
    server: Arc<Server>,
    clean: bool,
}

impl Drop for Lease<'_> {
    fn drop(&mut self) {
        let mut servers = self.slot.0.lock().unwrap();
        servers.busy.retain(|s| !Arc::ptr_eq(s, &self.server));
        let keep =
            self.clean && !servers.closed && servers.idle.len() < SPARE && self.server.is_alive();
        if keep {
            servers.idle.push(self.server.clone());
        } else {
            drop(servers);
            self.server.shutdown();
        }
    }
}

/// Everything the explainer is told, and where it came from; with the
/// compact prompt's size too when the whole conversation was asked for.
pub fn prompt(req: &Request) -> (context::Built, Option<usize>) {
    let conv = conversation(req);
    build(req, conv.as_ref())
}

fn conversation(req: &Request) -> Option<context::Conversation> {
    transcript::chat_sessions_dir(std::path::Path::new(&req.cwd))
        .and_then(|d| transcript::find(&d, &req.cwd))
        .and_then(|p| transcript::load(&p))
}

fn build(req: &Request, conv: Option<&context::Conversation>) -> (context::Built, Option<usize>) {
    let hit = conv.and_then(|c| context::find(c, &req.screen));
    if let Some(q) = &req.question {
        let mut built = context::build_question(
            Agent::Pi,
            &req.cwd,
            conv,
            hit,
            &req.screen,
            req.nested.as_ref(),
            true,
            q,
        );
        built.prompt = format!("{}\n\n{}", context::ask_system(Agent::Pi), built.prompt);
        built.prompt.push_str(&format!(
            "Keep the {OPEN}{CLOSE} marks out of the answer.\n"
        ));
        return (built, None);
    }
    let compact = context::build(
        Agent::Pi,
        &req.cwd,
        conv,
        hit,
        &req.screen,
        req.nested.as_ref(),
    );
    let (mut built, compact_len) = match (conv, req.deep) {
        (Some(c), true) => (
            context::build_deep(Agent::Pi, &req.cwd, c, hit, &req.screen),
            Some(compact.prompt.chars().count()),
        ),
        _ => (compact, None),
    };
    built.prompt = format!("{EXPLAIN}\n\n{}", built.prompt);
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
    let conv = conversation(&req);
    let (built, compact_len) = build(&req, conv.as_ref());
    if let Some(compact) = compact_len
        && !req.force
    {
        // Rough tokens: the text plus a little for Pi's request.
        let (deep, normal) = (built.prompt.chars().count() / 4 + 500, compact / 4 + 500);
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
    let mut lease = slot.take()?;
    let chat = conv.as_ref().and_then(|c| c.model.as_deref());
    let model = crate::explain::model(Agent::Pi, req.question.is_some(), chat);
    let done = ask(&lease.server, &built, model.as_deref(), tx);
    lease.clean = done.is_ok();
    done
}

/// One answer in a new in-memory session.
fn ask(
    server: &Server,
    built: &context::Built,
    model: Option<&str>,
    tx: &impl Fn(Progress),
) -> Result<()> {
    // One explanation per process: events carry no session (see `Slot`).
    let (etx, events) = mpsc::channel();
    *server.events.lock().unwrap() = Some(etx);
    let result = (|| -> Result<()> {
        server.request(json!({"type": "new_session"}))?;
        let mut name = String::from("Pi");
        if let Some(m) = model {
            let (provider, id) = split_model(m);
            let mut cmd = json!({"type": "set_model", "modelId": id});
            if let Some(p) = provider {
                cmd["provider"] = json!(p);
            }
            let data = server
                .request(cmd)
                .map_err(|e| anyhow!("{e}. Set PEEKME_PI_MODEL to provider/model"))?;
            name = data
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or(id)
                .to_string();
        } else if let Ok(state) = server.request(json!({"type": "get_state"})) {
            match state.pointer("/model/id").and_then(Value::as_str) {
                Some(id) => name = id.to_string(),
                None => bail!("Pi has no model to explain with. Run /login in Pi"),
            }
        }
        // `set_model` restores the model's saved thinking level.
        let _ = server.request(json!({"type": "set_thinking_level", "level": "off"}));
        let reply = server.start_request(json!({"type": "prompt", "message": built.prompt}))?;
        match reply.recv_timeout(Duration::from_secs(30)) {
            Ok(Ok(_)) => {}
            Ok(Err(e)) => bail!("{e}"),
            Err(_) => bail!("Pi did not take the question"),
        }
        let deadline = Instant::now() + Duration::from_secs(90);
        let (mut parts, mut started, mut error) = (Parts::default(), false, None::<String>);
        let mut message = 0usize;
        loop {
            let msg = match events.recv_timeout(Duration::from_millis(100)) {
                Ok(m) => m,
                Err(RecvTimeoutError::Disconnected) => bail!("Pi exited"),
                Err(RecvTimeoutError::Timeout) => {
                    if Instant::now() > deadline {
                        let _ = server.send(&json!({"type": "abort"}));
                        bail!("the explanation timed out");
                    }
                    continue;
                }
            };
            match msg.get("type").and_then(Value::as_str) {
                // Pi retries a failed request by itself, for up to a minute:
                // an explanation says what went wrong at once instead. (Its
                // `set_auto_retry` would turn retries off in the user's own
                // settings.json too.)
                Some("auto_retry_start") => {
                    let _ = server.send(&json!({"type": "abort_retry"}));
                }
                Some("message_start") => message += 1,
                Some("message_update") => {
                    let ev = msg.get("assistantMessageEvent").unwrap_or(&Value::Null);
                    if ev.get("type").and_then(Value::as_str) != Some("text_delta") {
                        continue;
                    }
                    let Some(delta) = ev.get("delta").and_then(Value::as_str) else {
                        continue;
                    };
                    if !started {
                        started = true;
                        tx(Progress::Started {
                            model: name.clone(),
                            source: built.label,
                        });
                    }
                    if let Some(t) = parts.text(&message.to_string(), delta) {
                        tx(Progress::Delta(t));
                    }
                }
                Some("message_end") => {
                    let m = msg.get("message").unwrap_or(&Value::Null);
                    if m.get("role").and_then(Value::as_str) == Some("assistant")
                        && m.get("stopReason").and_then(Value::as_str) == Some("error")
                    {
                        error = Some(
                            m.get("errorMessage")
                                .and_then(Value::as_str)
                                .unwrap_or("the model failed")
                                .to_string(),
                        );
                    }
                }
                Some("agent_settled") => {
                    if let Some(e) = error {
                        bail!("{}", with_login_hint(e));
                    }
                    if !started {
                        bail!("Pi answered with no text");
                    }
                    tx(Progress::Done);
                    return Ok(());
                }
                _ => {}
            }
        }
    })();
    server.events.lock().unwrap().take();
    result
}

/// Point at the fix when the provider says the login is missing or expired.
fn with_login_hint(e: String) -> String {
    let lower = e.to_lowercase();
    if [
        "login",
        "log in",
        "sign in",
        "api key",
        "not authenticated",
        "unauthorized",
        "401",
        "expired",
    ]
    .iter()
    .any(|w| lower.contains(w))
    {
        format!("{e} Run /login in Pi.")
    } else {
        e
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `pi --mode rpc` as far as explanations use it. Like Pi, a process
    /// takes one prompt at a time; the answer names its process. A prompt
    /// with "fail-retry" fails as Pi's retryable errors do: Pi waits 20 s
    /// before trying again, unless told `abort_retry`.
    const FAKE_PI: &str = r#"#!/usr/bin/env python3
import json, os, sys, threading, time
lock, busy, stop = threading.Lock(), threading.Event(), threading.Event()
def out(m):
    with lock:
        sys.stdout.write(json.dumps(m) + "\n")
        sys.stdout.flush()
def answer(text):
    out({"type": "message_start"})
    if "fail-retry" in text:
        out({"type": "message_end", "message": {"role": "assistant", "stopReason": "error",
                                                "errorMessage": "503 overloaded"}})
        out({"type": "auto_retry_start", "attempt": 1, "maxAttempts": 3, "delayMs": 20000})
        stop.wait(20)
    else:
        for w in ("answer ", "from ", str(os.getpid())):
            time.sleep(0.2)
            out({"type": "message_update",
                 "assistantMessageEvent": {"type": "text_delta", "delta": w}})
        out({"type": "message_end", "message": {"role": "assistant", "stopReason": "stop"}})
    busy.clear()
    out({"type": "agent_settled"})
for line in sys.stdin:
    cmd = json.loads(line)
    with open(os.environ["FAKE_PI_LOG"], "a") as f:
        f.write(f"{os.getpid()} {cmd['type']}\n")
    ok = {"type": "response", "id": cmd.get("id"), "command": cmd["type"], "success": True}
    if cmd["type"] == "abort_retry":
        stop.set()
    elif cmd["type"] == "get_state":
        out(dict(ok, data={"model": {"id": "fake"}}))
    elif cmd["type"] == "prompt" and busy.is_set():
        out(dict(ok, success=False, error="Agent is already processing"))
    elif cmd["type"] == "prompt":
        busy.set()
        out(ok)
        threading.Thread(target=answer, args=(cmd["message"],)).start()
    else:
        out(ok)
"#;

    fn request(selected: &str) -> Request {
        let text: Vec<char> = format!("some words, {selected}, more words")
            .chars()
            .collect();
        let start = 12;
        Request {
            screen: context::ScreenSel {
                end: start + selected.chars().count(),
                text,
                start,
            },
            cwd: "/".into(),
            max_lines: 5,
            deep: false,
            force: false,
            agent_pid: None,
            nested: None,
            question: None,
            terminal: false,
        }
    }

    /// The answer's text, or the error.
    fn explain_now(slot: &Slot, selected: &str) -> Result<String, String> {
        let progress = Mutex::new(Vec::new());
        explain(slot, request(selected), |p| {
            progress.lock().unwrap().push(p)
        });
        let mut text = String::new();
        for p in progress.into_inner().unwrap() {
            match p {
                Progress::Delta(d) => text.push_str(&d),
                Progress::Done => return Ok(text),
                Progress::Failed(e) => return Err(e),
                _ => {}
            }
        }
        Err(format!("no end after {text:?}"))
    }

    fn pid_of(answer: &str) -> i32 {
        answer.rsplit(' ').next().unwrap().parse().unwrap()
    }

    /// A box inside a box explains while the box it replaced still streams:
    /// each explanation gets a process (Pi's has one session), one stays for
    /// the next. A retryable error ends the explanation at once, without
    /// `set_auto_retry`, which Pi writes into the user's settings.json.
    #[test]
    fn explanations_at_once_get_a_process_each() {
        // The fake Pi is set in a subprocess, never in the test runner's
        // environment, with an agent folder of its own (no transcripts).
        if std::env::var_os("PEEKME_TEST_PI_POOL").is_none() {
            use std::os::unix::fs::PermissionsExt;
            let dir = std::env::temp_dir().join(format!("peekme-pi-pool-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let bin = dir.join("pi");
            std::fs::write(&bin, FAKE_PI).unwrap();
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o700)).unwrap();
            let output = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "pi::explain::tests::explanations_at_once_get_a_process_each",
                    "--nocapture",
                ])
                .env("PEEKME_TEST_PI_POOL", "1")
                .env("PEEKME_PI_BIN", &bin)
                .env("FAKE_PI_LOG", dir.join("log"))
                .env("PI_CODING_AGENT_DIR", dir.join("agent"))
                .env_remove("PEEKME_PI_MODEL")
                .env_remove("PEEKME_DEBUG_PROMPT")
                .output()
                .unwrap();
            std::fs::remove_dir_all(&dir).unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }
        let slot = Slot::default();
        let (a, b) = std::thread::scope(|s| {
            let a = s.spawn(|| explain_now(&slot, "outer"));
            let b = s.spawn(|| explain_now(&slot, "inner"));
            (a.join().unwrap(), b.join().unwrap())
        });
        let (a, b) = (a.unwrap(), b.unwrap());
        assert!(a.starts_with("answer from ") && b.starts_with("answer from "));
        assert_ne!(pid_of(&a), pid_of(&b), "one process each");
        assert_eq!(slot.0.lock().unwrap().idle.len(), SPARE);
        let next = explain_now(&slot, "next").unwrap();
        assert!([pid_of(&a), pid_of(&b)].contains(&pid_of(&next)));

        let t = Instant::now();
        let failed = explain_now(&slot, "fail-retry").unwrap_err();
        assert!(failed.contains("503 overloaded"), "{failed}");
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "Pi's retry was waited for"
        );
        let log = std::fs::read_to_string(std::env::var("FAKE_PI_LOG").unwrap()).unwrap();
        assert!(log.contains(" abort_retry\n"), "{log}");
        assert!(!log.contains("set_auto_retry"), "{log}");

        let kept = slot.0.lock().unwrap().idle.clone();
        slot.shutdown();
        assert!(kept.iter().all(|s| !s.is_alive()));
        assert!(explain_now(&slot, "after").is_err());
    }

    #[test]
    fn model_names() {
        assert_eq!(
            split_model("anthropic/claude-haiku-4-5"),
            (Some("anthropic"), "claude-haiku-4-5")
        );
        assert_eq!(split_model("openrouter/x/y"), (Some("openrouter"), "x/y"));
        assert_eq!(split_model("haiku"), (None, "haiku"));
        assert!(with_login_hint("401 Unauthorized".into()).ends_with("Run /login in Pi."));
        assert_eq!(
            with_login_hint("Connection error.".into()),
            "Connection error."
        );
    }
}
