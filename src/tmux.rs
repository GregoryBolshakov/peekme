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

fn tmux_bin() -> String {
    std::env::var("PEEKME_TMUX_BIN").unwrap_or_else(|_| "tmux".into())
}

fn unix(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Text of the newest paste buffer created at or after `since` (the last
/// mouse release) and no older than `max_age`.
pub fn fresh_buffer(since: Option<SystemTime>, max_age: Duration) -> Option<String> {
    let now = SystemTime::now();
    let oldest = unix(now.checked_sub(max_age).unwrap_or(UNIX_EPOCH))
        .max(since.map_or(0, |s| unix(s).saturating_sub(1)));
    let out = Command::new(tmux_bin())
        .args(["list-buffers", "-F", "#{buffer_created} #{buffer_name}"])
        .output()
        .ok()?;
    let name = newest(&String::from_utf8_lossy(&out.stdout), oldest)?;
    let text = Command::new(tmux_bin())
        .args(["show-buffer", "-b", &name])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&text.stdout).into_owned();
    (!text.trim().is_empty()).then_some(text)
}

/// The newest buffer in `list-buffers` output created at or after `oldest`.
fn newest(list: &str, oldest: u64) -> Option<String> {
    list.lines()
        .filter_map(|l| {
            let (created, name) = l.split_once(' ')?;
            Some((created.parse::<u64>().ok()?, name.to_string()))
        })
        .filter(|(created, _)| *created >= oldest)
        .max_by_key(|(created, _)| *created)
        .map(|(_, name)| name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_newest_fresh_buffer() {
        let list = "1790000100 buffer2\n1790000200 buffer5\n1790000150 buffer3\n";
        assert_eq!(newest(list, 1790000000).as_deref(), Some("buffer5"));
        assert_eq!(newest(list, 1790000201), None, "all older than the release");
        assert_eq!(newest("", 0), None);
    }
}
