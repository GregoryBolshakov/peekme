//! Inside tmux, Claude Code and Codex do not report a mouse selection with
//! OSC 52: they copy it with `tmux load-buffer -w -`, and tmux forwards it to
//! the terminal's clipboard itself. peekme sits between the agent and tmux, so
//! it never sees that copy; it asks tmux for the newest paste buffer instead.
//! The same buffer holds tmux's own selection when tmux handles the drag.

use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// A buffer older than this is not a selection the user just made.
pub const FRESH: Duration = Duration::from_secs(120);
/// For `π` (Option+P on a Mac), which may also be meant as a letter: when
/// tmux handled the drag, peekme never sees a later click that clears it, so
/// only a copy made just before counts.
pub const FRESH_FOR_OPTION_P: Duration = Duration::from_secs(15);

/// peekme runs inside tmux (and the agent it starts does too).
pub fn inside() -> bool {
    std::env::var_os("TMUX").is_some_and(|v| !v.is_empty())
}

/// Keys that mean Option+P / Alt+P to tmux: Option as Meta, and `π` from a
/// Mac terminal in its default setting.
const OPTION_P_KEYS: [&str; 2] = ["M-p", "π"];
const COPY_TABLES: [&str; 2] = ["copy-mode", "copy-mode-vi"];

/// A drag that tmux handles (`mouse on`, the agent not taking the mouse)
/// puts the pane in copy mode. With `copy-selection` as the drag-end binding
/// it stays there, and every key goes to tmux, not to peekme: Option+P did
/// nothing. So peekme marks its pane and binds Option+P in the copy-mode
/// tables: in a peekme pane it copies what is selected, leaves copy mode and
/// hands peekme Alt+P, which then reads that copy. In other panes the key
/// does nothing, as before. Keys the user bound already are left alone.
pub fn prepare_copy_mode() {
    let Ok(pane) = std::env::var("TMUX_PANE") else {
        return;
    };
    run(&["set-option", "-p", "-t", &pane, "@peekme", "1"]);
    let action = "if-shell -F '#{selection_present}' \
                  'send-keys -X copy-selection-and-cancel' 'send-keys -X cancel' ; \
                  send-keys M-p";
    for table in COPY_TABLES {
        for key in OPTION_P_KEYS {
            let bound = Command::new(tmux_bin())
                .args(["list-keys", "-T", table, key])
                .output()
                .is_ok_and(|o| o.status.success() && !o.stdout.is_empty());
            if !bound {
                run(&[
                    "bind-key",
                    "-T",
                    table,
                    key,
                    "if-shell",
                    "-F",
                    "#{@peekme}",
                    action,
                ]);
            }
        }
    }
}

/// Unmark the pane when peekme ends (the agent may be run without it next).
pub fn release_copy_mode() {
    if let Ok(pane) = std::env::var("TMUX_PANE") {
        run(&["set-option", "-p", "-u", "-t", &pane, "@peekme"]);
    }
}

fn run(args: &[&str]) {
    let _ = Command::new(tmux_bin())
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

fn tmux_bin() -> String {
    std::env::var("PEEKME_TMUX_BIN").unwrap_or_else(|_| "tmux".into())
}

fn unix(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Name of tmux's newest paste buffer, if any. Taken when a mouse release is
/// forwarded to the agent (before the agent sees it): only a buffer newer than
/// this one can be the agent's copy of that selection.
pub fn newest_buffer() -> Option<String> {
    newest(&list_buffers()?, 0).map(|(_, name)| name)
}

/// Text of the newest paste buffer if it is not `baseline` (the newest one at
/// the last mouse release) and at most `max_age` old.
pub fn fresh_buffer(baseline: Option<&str>, max_age: Duration) -> Option<String> {
    let oldest = unix(SystemTime::now().checked_sub(max_age).unwrap_or(UNIX_EPOCH));
    let (_, name) = newest(&list_buffers()?, oldest)?;
    if Some(name.as_str()) == baseline {
        return None; // nothing copied since the last release
    }
    let text = Command::new(tmux_bin())
        .args(["show-buffer", "-b", &name])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&text.stdout).into_owned();
    (!text.trim().is_empty()).then_some(text)
}

/// tmux's `mouse` option for this pane's session. With it off, Codex leaves
/// the mouse to the terminal (it asks tmux at startup), so a drag becomes the
/// terminal's own selection, which peekme cannot read over SSH.
pub fn mouse() -> Option<bool> {
    let mut args = vec!["display-message", "-p"];
    let pane = std::env::var("TMUX_PANE").ok();
    if let Some(p) = &pane {
        args.extend(["-t", p]);
    }
    args.push("#{mouse}");
    let out = Command::new(tmux_bin()).args(&args).output().ok()?;
    match String::from_utf8_lossy(&out.stdout).trim() {
        "1" | "on" => Some(true),
        "0" | "off" => Some(false),
        _ => None,
    }
}

fn list_buffers() -> Option<String> {
    let out = Command::new(tmux_bin())
        .args(["list-buffers", "-F", "#{buffer_created} #{buffer_name}"])
        .output()
        .ok()?;
    Some(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// (created, name) of the newest buffer in `list-buffers` output created at or
/// after `oldest`. tmux lists the newest first, and names new buffers uniquely.
fn newest(list: &str, oldest: u64) -> Option<(u64, String)> {
    list.lines()
        .filter_map(|l| {
            let (created, name) = l.split_once(' ')?;
            Some((created.parse::<u64>().ok()?, name.to_string()))
        })
        .find(|(created, _)| *created >= oldest)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_newest_fresh_buffer() {
        // `list-buffers` prints the newest first.
        let list = "1790000200 buffer5\n1790000150 buffer3\n1790000100 buffer2\n";
        assert_eq!(
            newest(list, 1790000000).map(|b| b.1).as_deref(),
            Some("buffer5")
        );
        assert_eq!(newest(list, 1790000201), None, "all too old");
        assert_eq!(newest("", 0), None);
    }
}
