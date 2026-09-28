//! Explanations through `claude -p`: the user's existing Claude Code login, a
//! small model, streamed back into the peek box.
//!
//! Each explanation runs in its own short-lived `claude -p` process, so no
//! explanation sees another. Starting one takes about half a second, so after
//! the first explanation one idle process is kept ready for the next.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};

use super::transcript;
use crate::agent::Agent;
use crate::context::{self, CLOSE, OPEN};
use crate::explain::{DEEP_ASK_ABOVE_TOKENS, Progress, Request};

const SYSTEM: &str = "You are \"peek\", an explainer embedded in a terminal. The user highlighted a fragment \
     of text (marked ⟦like this⟧) in their Claude Code session and wants to understand it. Explain what the \
     fragment means where it appears: define the terms, name the specific thing it refers to when the \
     context shows it, say what any code does there, and why it matters for the user's task. No preamble, \
     no headings. Answer from the provided context and general knowledge only.";

pub fn model() -> String {
    std::env::var("PEEKME_CLAUDE_MODEL")
        .ok()
        .filter(|m| !m.trim().is_empty())
        .unwrap_or_else(|| "haiku".into())
}

fn claude_bin() -> String {
    std::env::var("PEEKME_CLAUDE_BIN").unwrap_or_else(|_| {
        crate::launch::find_real("claude")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "claude".into())
    })
}

/// A started `claude -p` waiting for its one message.
struct Worker {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
}

impl Worker {
    fn spawn() -> Result<Self> {
        let mut cmd = Command::new(claude_bin());
        cmd.args([
            "-p",
            "--model",
            &model(),
            "--no-session-persistence",
            "--tools",
            "",
            "--setting-sources",
            "",
            "--strict-mcp-config",
            "--disable-slash-commands",
            "--system-prompt",
            SYSTEM,
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
        ]);
        // Set when peekme itself runs inside Claude Code; they would make
        // the explainer believe it is a nested session. CLAUDE_CONFIG_DIR stays,
        // so the explainer uses the same login.
        for (k, _) in std::env::vars_os() {
            let k = k.to_string_lossy();
            if k == "CLAUDECODE" || k.starts_with("CLAUDE_CODE_") || k == "CLAUDE_PID" {
                cmd.env_remove(&*k);
            }
        }
        cmd.env("MAX_THINKING_TOKENS", "0")
            .current_dir(std::env::temp_dir())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut child = cmd
            .spawn()
            .with_context(|| format!("could not start `{}`", claude_bin()))?;
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            lines,
        })
    }

    fn alive(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Hands out workers; keeps one spare ready once explanations are in use.
#[derive(Default)]
pub struct Pool {
    spare: Mutex<Option<Worker>>,
    closed: Mutex<bool>,
}

impl Pool {
    fn take(&self) -> Result<Worker> {
        if let Some(mut w) = self.spare.lock().unwrap().take()
            && w.alive()
        {
            return Ok(w);
        }
        Worker::spawn()
    }

    /// Start a spare in the background if there is none.
    pub fn refill(self: &Arc<Self>) {
        if *self.closed.lock().unwrap() || self.spare.lock().unwrap().is_some() {
            return;
        }
        let pool = self.clone();
        std::thread::spawn(move || {
            if let Ok(w) = Worker::spawn() {
                let mut spare = pool.spare.lock().unwrap();
                if spare.is_none() && !*pool.closed.lock().unwrap() {
                    *spare = Some(w);
                }
            }
        });
    }

    pub fn shutdown(&self) {
        *self.closed.lock().unwrap() = true;
        self.spare.lock().unwrap().take();
    }
}

pub fn explain(pool: &Arc<Pool>, req: Request, tx: impl Fn(Progress)) {
    if let Err(e) = run(pool, req, &tx) {
        tx(Progress::Failed(format!("{e:#}")));
    }
    pool.refill();
}

/// Everything the explainer is told, and where it came from; with the
/// compact prompt's size too when the whole conversation was asked for.
pub fn prompt(req: &Request) -> (context::Built, Option<usize>) {
    let conv = transcript::config_dir()
        .and_then(|c| transcript::find(&c, req.agent_pid, &req.cwd))
        .and_then(|p| transcript::load(&p));
    let hit = conv.as_ref().and_then(|c| context::find(c, &req.screen));
    let compact = context::build(Agent::Claude, &req.cwd, conv.as_ref(), hit, &req.screen);
    let (mut built, compact_len) = match (&conv, req.deep) {
        (Some(c), true) => (
            context::build_deep(Agent::Claude, &req.cwd, c, hit, &req.screen),
            Some(compact.prompt.chars().count()),
        ),
        _ => (compact, None),
    };
    built.prompt.push_str(&format!(
        "Answer in at most {} short lines; keep the {OPEN}{CLOSE} marks out of the answer.\n",
        req.max_lines
    ));
    (built, compact_len)
}

/// Rough token count of a prompt plus `claude -p`'s own overhead (~400 tokens
/// with no tools and our short system prompt).
fn tokens(chars: usize) -> usize {
    chars / 4 + 400
}

fn run(pool: &Arc<Pool>, req: Request, tx: &impl Fn(Progress)) -> Result<()> {
    let (built, compact_len) = prompt(&req);
    if let Some(compact) = compact_len
        && !req.force
    {
        let (deep, normal) = (tokens(built.prompt.chars().count()), tokens(compact));
        if deep > DEEP_ASK_ABOVE_TOKENS {
            tx(Progress::TooBig {
                tokens: deep,
                ratio: deep / normal,
            });
            return Ok(());
        }
    }
    if let Some(path) = debug_prompt_path() {
        let _ = std::fs::write(path, &built.prompt);
    }
    let mut w = pool.take()?;
    let msg = json!({"type": "user", "message": {"role": "user", "content": built.prompt}});
    writeln!(w.stdin, "{msg}").context("the explainer process is not running")?;
    w.stdin.flush()?;

    let mut started = false;
    let mut model_name = model();
    loop {
        let line = w
            .lines
            .recv_timeout(Duration::from_secs(90))
            .map_err(|e| match e {
                mpsc::RecvTimeoutError::Timeout => anyhow!("the explanation timed out"),
                mpsc::RecvTimeoutError::Disconnected => {
                    anyhow!("`claude -p` exited without answering; check that `claude -p hi` works")
                }
            })?;
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("system") if v.get("subtype").and_then(Value::as_str) == Some("init") => {
                if let Some(m) = v.get("model").and_then(Value::as_str) {
                    model_name = short_model(m);
                }
            }
            Some("stream_event") => {
                let delta = v.pointer("/event/delta");
                if delta.and_then(|d| d.get("type")).and_then(Value::as_str) == Some("text_delta")
                    && let Some(t) = delta.and_then(|d| d.get("text")).and_then(Value::as_str)
                {
                    if !started {
                        started = true;
                        tx(Progress::Started {
                            model: model_name.clone(),
                            source: built.label,
                        });
                    }
                    tx(Progress::Delta(t.to_string()));
                }
            }
            Some("result") => {
                if v.get("is_error").and_then(Value::as_bool) == Some(true) {
                    let m = v
                        .get("result")
                        .and_then(Value::as_str)
                        .or_else(|| v.get("subtype").and_then(Value::as_str))
                        .unwrap_or("the model returned an error");
                    bail!("{m}");
                }
                if !started {
                    // No streamed deltas (older CLI): use the final text.
                    tx(Progress::Started {
                        model: model_name.clone(),
                        source: built.label,
                    });
                    if let Some(t) = v.get("result").and_then(Value::as_str) {
                        tx(Progress::Delta(t.to_string()));
                    }
                }
                tx(Progress::Done);
                return Ok(());
            }
            _ => {}
        }
    }
}

/// `claude-haiku-4-5-20251001` -> `claude-haiku-4-5`.
fn short_model(m: &str) -> String {
    match m.rsplit_once('-') {
        Some((head, date)) if date.len() == 8 && date.bytes().all(|b| b.is_ascii_digit()) => {
            head.to_string()
        }
        _ => m.to_string(),
    }
}

fn debug_prompt_path() -> Option<PathBuf> {
    std::env::var_os("PEEKME_DEBUG_PROMPT")
        .map(|_| std::env::temp_dir().join("peekme-last-prompt.txt"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_model_names() {
        assert_eq!(short_model("claude-haiku-4-5-20251001"), "claude-haiku-4-5");
        assert_eq!(short_model("claude-sonnet-5"), "claude-sonnet-5");
    }
}
