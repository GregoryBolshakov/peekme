//! How far full-screen Codex scrolls its chat for a turn of the wheel.
//!
//! Since 0.162 Codex adds `tui.mouse_scroll_speed` rows (1.0 by default, a
//! fraction allowed) to a running count for each turn, scrolls the whole rows
//! and keeps the fraction, dropped when the wheel turns the other way. 0.161
//! scrolled 3 rows a turn. peekme keeps the same count, so it knows how many
//! rows each turn moves the chat, none included.

/// Codex's count of wheel rows, as Codex keeps it.
#[derive(Debug, Clone)]
pub struct Wheel {
    speed: f64,
    /// The speed came from Codex's settings; otherwise it is learned from how
    /// far single turns move the chat.
    known: bool,
    pending: f64,
    seen: Vec<isize>,
}

impl Wheel {
    pub fn new(setting: Option<f64>) -> Self {
        Wheel {
            speed: setting.unwrap_or(1.0),
            known: setting.is_some(),
            pending: 0.0,
            seen: Vec::new(),
        }
    }

    /// Rows a turn moves the chat (down is plus), `way` 1 down or -1 up, by
    /// Codex's own count (`transcript_view/input.rs`, 0.162.1).
    pub fn turn(&mut self, way: isize) -> isize {
        let direction = if way < 0 { -1.0 } else { 1.0 };
        if self.pending.signum() != direction {
            self.pending = 0.0;
        }
        self.pending += direction * self.speed;
        let rounded = self.pending.round();
        if rounded != 0.0 && (self.pending - rounded).abs() < 1e-9 {
            self.pending = rounded;
        }
        let rows = self.pending as isize;
        self.pending = self.pending.fract();
        rows
    }

    /// Rows per turn.
    pub fn speed(&self) -> f64 {
        self.speed
    }

    /// A single turn moved the chat `rows`: without the setting, the speed is
    /// the middle of the last few such moves.
    pub fn learn(&mut self, rows: isize) {
        if self.known || rows == 0 {
            return;
        }
        self.seen.push(rows.abs());
        if self.seen.len() > 7 {
            self.seen.remove(0);
        }
        let mut s = self.seen.clone();
        s.sort_unstable();
        self.speed = s[s.len() / 2] as f64;
    }
}

/// `tui.mouse_scroll_speed` for a Codex started with `args`: a `-c` override
/// on its command line, else its config file.
pub fn speed_setting(args: &[String]) -> Option<f64> {
    let mut it = args.iter();
    let mut found = None;
    while let Some(a) = it.next() {
        let kv = match a.as_str() {
            "-c" | "--config" => it.next().map(String::as_str),
            _ => a.strip_prefix("--config=").or_else(|| a.strip_prefix("-c")),
        };
        let value = kv
            .and_then(|kv| kv.trim().strip_prefix("tui.mouse_scroll_speed"))
            .and_then(|v| v.trim_start().strip_prefix('='))
            .and_then(number);
        found = value.or(found);
    }
    found.or_else(|| {
        let home = std::env::var_os("CODEX_HOME")
            .map(std::path::PathBuf::from)
            .or_else(|| crate::launch::home().map(|h| h.join(".codex")))?;
        from_config(&std::fs::read_to_string(home.join("config.toml")).ok()?)
    })
}

/// The setting in a Codex `config.toml`: `mouse_scroll_speed` in `[tui]`, or
/// `tui.mouse_scroll_speed` at the top.
pub fn from_config(text: &str) -> Option<f64> {
    let mut table = String::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        if let Some(t) = line.strip_prefix('[') {
            table = t.trim_end_matches(']').trim().to_string();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        if (table == "tui" && key == "mouse_scroll_speed")
            || (table.is_empty() && key == "tui.mouse_scroll_speed")
        {
            return number(value);
        }
    }
    None
}

fn number(s: &str) -> Option<f64> {
    s.trim()
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite() && *v > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rows Codex moved in a run with speed 0.5: 0, -1, 0 up, then down
    /// the count starts again: 0, 1.
    #[test]
    fn the_count_is_codex_own() {
        let mut w = Wheel::new(Some(0.5));
        let rows: Vec<isize> = [-1, -1, -1, 1, 1].iter().map(|&d| w.turn(d)).collect();
        assert_eq!(rows, [0, -1, 0, 0, 1]);
        let mut w = Wheel::new(None);
        assert_eq!((w.turn(1), w.turn(-1)), (1, -1), "1.0 when not set");
        let mut w = Wheel::new(Some(2.5));
        let rows: Vec<isize> = (0..4).map(|_| w.turn(1)).collect();
        assert_eq!(rows, [2, 3, 2, 3]);
    }

    /// Without the setting, single turns teach the speed (3 in Codex 0.161).
    #[test]
    fn the_speed_is_learned() {
        let mut w = Wheel::new(None);
        for _ in 0..3 {
            w.learn(-3);
        }
        assert_eq!(w.turn(1), 3);
        let mut w = Wheel::new(Some(1.0));
        w.learn(3);
        assert_eq!(w.speed(), 1.0, "the setting wins");
    }

    #[test]
    fn the_setting_is_read() {
        let args = |a: &[&str]| a.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(
            speed_setting(&args(&["-c", "tui.mouse_scroll_speed=2.5"])),
            Some(2.5)
        );
        assert_eq!(
            speed_setting(&args(&["--config=tui.mouse_scroll_speed = 0.5"])),
            Some(0.5)
        );
        let toml = "model = \"x\"\n[tui]\n# speed\nmouse_scroll_speed = 1.5 # rows\n[other]\nmouse_scroll_speed = 9\n";
        assert_eq!(from_config(toml), Some(1.5));
        assert_eq!(
            from_config("tui.mouse_scroll_speed = 3\n[tui]\n"),
            Some(3.0)
        );
        assert_eq!(from_config("[tui]\nmouse_scroll_speed = -1\n"), None);
        assert_eq!(from_config("[tuix]\nmouse_scroll_speed = 2\n"), None);
    }
}
