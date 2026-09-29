//! peekme: select text in an agent CLI's output (Claude Code, Codex CLI,
//! GitHub Copilot CLI), press Alt+P, and read an explanation that opens inline
//! next to it.
//!
//! The terminal side (pseudo-terminal, shadow emulator, the box drawn in place)
//! is the same for every agent. What differs per agent lives in its own
//! module: how the selection is read, where the conversation comes from, and
//! which model explains.

pub mod agent;
pub mod app;
pub mod claude;
pub mod codex;
pub mod context;
pub mod copilot;
pub mod explain;
pub mod input;
pub mod install;
pub mod jobctl;
pub mod launch;
pub mod osc52;
pub mod overlay;
pub mod render;
pub mod select;
pub mod shadow;
pub mod tmux;

pub fn log_path() -> std::path::PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".local/state")))
        .unwrap_or_else(std::env::temp_dir);
    base.join("peekme").join("peekme.log")
}

/// Append a line to the log file. Never fails loudly: this runs inside a panic hook.
pub fn log(msg: &str) {
    let path = log_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let _ = std::io::Write::write_all(&mut f, format!("{secs} {msg}\n").as_bytes());
    }
}

/// Append one event to `PEEKME_EVENT_LOG` as a JSON line, if it is set: what
/// peekme decided (a box opened, with which selection; a key was typed or
/// taken as the shortcut). For the end-to-end tests and for bug reports.
pub fn event(kind: &str, fields: serde_json::Value) {
    let Some(path) = std::env::var_os("PEEKME_EVENT_LOG") else {
        return;
    };
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    let mut line = serde_json::json!({"t": ms as u64, "event": kind});
    if let (Some(obj), serde_json::Value::Object(extra)) = (line.as_object_mut(), fields) {
        obj.extend(extra);
    }
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = std::io::Write::write_all(&mut f, format!("{line}\n").as_bytes());
    }
}
