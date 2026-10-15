//! How far full-screen Claude Code scrolls its chat for a turn of the wheel.
//!
//! Claude (2.1.299) accelerates the wheel: turns 40 ms apart or more move
//! `base` rows (1, or `CLAUDE_CODE_SCROLL_SPEED`), closer ones a little more
//! each, the first turn the other way none, and a quick reversal starts a
//! faster mode that decays. peekme keeps the same count, so it knows how many
//! rows each turn should move the chat.

use std::time::Instant;

/// Claude's wheel settings, as Claude reads them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Setting {
    pub base: f64,
    /// Turns closer than 40 ms speed up (`wheelScrollAccelerationEnabled`).
    pub accel: bool,
    /// The decay curve Claude uses in xterm.js terminals (VS Code, Cursor),
    /// instead of its native window.
    pub decay: bool,
    /// Terminals that send a flood of reports a turn (Cursor, some VS Code).
    pub flood: bool,
}

/// Claude's count of wheel rows, as Claude keeps it (`GSt` in 2.1.299).
#[derive(Debug, Clone)]
pub struct Wheel {
    set: Setting,
    time: Option<Instant>,
    mult: f64,
    frac: f64,
    dir: isize,
    pending_flip: bool,
    wheel_mode: bool,
    burst: u32,
}

impl Wheel {
    pub fn new(set: Setting) -> Self {
        Wheel {
            set,
            time: None,
            mult: set.base,
            frac: 0.0,
            dir: 0,
            pending_flip: false,
            wheel_mode: false,
            burst: 0,
        }
    }

    /// Rows a turn at `now` moves the chat (down is plus), `way` 1 down or -1 up.
    pub fn turn(&mut self, way: isize, now: Instant) -> isize {
        let d = if way < 0 { -1 } else { 1 };
        let rows = self.rows(d, now);
        d * rows
    }

    fn rows(&mut self, d: isize, now: Instant) -> isize {
        let base = self.set.base;
        // Milliseconds since the last turn; the first one is long ago.
        let gap = self.time.map_or(f64::INFINITY, |t| {
            now.duration_since(t).as_secs_f64() * 1000.0
        });
        let whole = |m: f64| (m.floor() as isize).max(1);
        if self.set.decay {
            let same = d == self.dir;
            self.time = Some(now);
            self.dir = d;
            if same && gap < 5.0 {
                return 1;
            }
            if !self.set.accel {
                return whole(base);
            }
            if !same || gap > 500.0 {
                self.mult = base.max(2.0);
                self.frac = 0.0;
            } else {
                let k = 0.5f64.powf(gap / 150.0);
                let cap: f64 = if gap >= 80.0 { 3.0 } else { 36.0 };
                self.mult = cap.min(1.0 + (self.mult - 1.0) * k + 7.0 * k);
            }
            let m = self.mult + self.frac;
            let rows = m.floor();
            self.frac = m - rows;
            return rows as isize;
        }
        if self.set.flood {
            self.time = Some(now);
            self.dir = d;
            self.mult = if gap > 40.0 { base * 3.0 } else { base };
            return whole(self.mult);
        }
        if self.wheel_mode && gap > 1500.0 {
            (self.wheel_mode, self.burst, self.mult) = (false, 0, base);
        }
        if self.pending_flip {
            self.pending_flip = false;
            if d != self.dir || gap > 200.0 {
                (self.dir, self.time, self.mult) = (d, Some(now), base);
                return whole(self.mult);
            }
            self.wheel_mode = true;
        }
        if d != self.dir && self.dir != 0 {
            (self.pending_flip, self.time) = (true, Some(now));
            return 0;
        }
        (self.dir, self.time) = (d, Some(now));
        if self.wheel_mode {
            if gap < 5.0 {
                self.burst += 1;
                if self.burst >= 5 {
                    (self.wheel_mode, self.burst, self.mult) = (false, 0, base);
                } else {
                    return 1;
                }
            } else {
                self.burst = 0;
            }
        }
        if self.wheel_mode && self.set.accel {
            let k = 0.5f64.powf(gap / 150.0);
            let cap = (15.0 * base.min(1.0)).max(base * 2.0);
            self.mult = cap
                .min(1.0 + (self.mult - 1.0) * k + 15.0 * k)
                .min(self.mult + 3.0);
            return whole(self.mult);
        }
        if gap > 40.0 || !self.set.accel {
            self.mult = base;
        } else {
            let cap = (6.0 * base.min(1.0)).max(base * 2.0);
            self.mult = cap.min(self.mult + 0.3);
        }
        whole(self.mult)
    }
}

/// Claude's wheel settings for a `claude` started with `args` in this
/// environment: the speed from `CLAUDE_CODE_SCROLL_SPEED`, acceleration from
/// `wheelScrollAccelerationEnabled` (on unless a settings file or `--settings`
/// turns it off), and the xterm.js modes from the terminal Claude can see.
pub fn setting(args: &[String]) -> Setting {
    let env = |k: &str| std::env::var(k).ok();
    let vscode = env("TERM_PROGRAM").as_deref() == Some("vscode");
    let flood = env("CURSOR_TRACE_ID").is_some()
        || env("VSCODE_GIT_ASKPASS_MAIN").is_some_and(|v| v.contains("cursor"))
        || (vscode
            && env("TERM_PROGRAM_VERSION")
                .and_then(|v| version(&v))
                .is_some_and(|v| (1_092_000..1_105_000).contains(&v)));
    let jedi = env("TERMINAL_EMULATOR").is_some_and(|v| v.contains("JediTerm"));
    let wt = env("WT_SESSION").is_some();
    let xterm_js = vscode;
    let decay = !flood && (xterm_js || wt);
    let mut base = if jedi {
        2.0
    } else if !flood && (xterm_js || wt) {
        3.0
    } else {
        1.0
    };
    if !jedi
        && let Some(v) = env("CLAUDE_CODE_SCROLL_SPEED").and_then(|v| v.trim().parse::<f64>().ok())
        && v > 0.0
    {
        base = v.min(20.0);
    }
    Setting {
        base,
        accel: accel(args),
        decay,
        flood,
    }
}

fn version(v: &str) -> Option<u64> {
    let mut it = v.split('.').map(|p| p.parse::<u64>().ok());
    Some(it.next()?? * 1_000_000 + it.next()?? * 1000 + it.next().flatten().unwrap_or(0))
}

/// `wheelScrollAccelerationEnabled` from `--settings` (JSON or a file) and
/// the user and project settings files; on unless one sets it false.
fn accel(args: &[String]) -> bool {
    let mut texts: Vec<String> = Vec::new();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let v = match a.strip_prefix("--settings=") {
            Some(v) => Some(v.to_string()),
            None if a == "--settings" => it.next().cloned(),
            None => None,
        };
        if let Some(v) = v {
            if v.trim_start().starts_with('{') {
                texts.push(v);
            } else if let Ok(t) = std::fs::read_to_string(&v) {
                texts.push(t);
            }
        }
    }
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .or_else(|| crate::launch::home().map(|h| h.join(".claude")));
    let mut files: Vec<std::path::PathBuf> =
        dir.into_iter().map(|d| d.join("settings.json")).collect();
    files.push(".claude/settings.json".into());
    files.push(".claude/settings.local.json".into());
    texts.extend(files.iter().filter_map(|f| std::fs::read_to_string(f).ok()));
    !texts.iter().any(|t| {
        serde_json::from_str::<serde_json::Value>(t)
            .ok()
            .and_then(|v| v.get("wheelScrollAccelerationEnabled")?.as_bool())
            == Some(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn native() -> Wheel {
        Wheel::new(Setting {
            base: 1.0,
            accel: true,
            decay: false,
            flood: false,
        })
    }

    /// The rows raw Claude 2.1.299 moved: 40 turns 60 ms apart, a row each;
    /// 10 turns 8 ms apart, 19 rows; five reports in one write the other
    /// way, 0 then 1 each.
    #[test]
    fn the_count_is_claude_own() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut w = native();
        let slow: Vec<isize> = (0..40).map(|i| w.turn(-1, at(60 * i))).collect();
        assert!(slow.iter().all(|&r| r == -1), "{slow:?}");
        let mut w = native();
        let fast: Vec<isize> = (0..10).map(|i| w.turn(-1, at(5000 + 8 * i))).collect();
        assert_eq!(fast, [-1, -1, -1, -1, -2, -2, -2, -3, -3, -3]);
        let back: Vec<isize> = (0..5).map(|_| w.turn(1, at(5400))).collect();
        assert_eq!(back, [0, 1, 1, 1, 1]);
    }

    /// A slow turn after a reversal moves `base` rows again; with
    /// acceleration off every turn does.
    #[test]
    fn reversal_and_no_acceleration() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut w = native();
        assert_eq!(
            (w.turn(1, at(0)), w.turn(-1, at(300)), w.turn(-1, at(600))),
            (1, 0, -1)
        );
        let mut w = Wheel::new(Setting {
            base: 2.0,
            accel: false,
            decay: false,
            flood: false,
        });
        let rows: Vec<isize> = (0..6).map(|i| w.turn(1, at(3 * i))).collect();
        assert_eq!(rows, [2, 2, 2, 2, 2, 2]);
    }
}
