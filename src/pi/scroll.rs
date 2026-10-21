//! How far full-screen Pi scrolls its chat (1.3, `WheelScrollAccelerator`
//! in pi-tui). With its setting `fullscreenWheelScrollLines` at `"auto"`
//! (the default) a wheel event moves one row, or more when events come
//! quickly: up to 6, from how far apart they come on average. Events closer
//! than 5 ms (one notch of some terminals) move one row each and change
//! nothing. Local macOS terminals accelerate the wheel themselves: one row
//! an event there. Alt moves five times as far. PageUp and PageDown move the
//! chat's rows less 4.
//!
//! peekme keeps Pi's count for the turns the user makes, and sends each
//! turn as that many one-row events in one burst (see `App::release_turns_into`),
//! the bursts far enough apart that Pi counts the first event of each as
//! one row too. So every turn moves the chat by a known number of rows.

use std::path::Path;
use std::time::Instant;

/// Events closer than this are one notch: one row each.
const BURST_GAP_MS: f64 = 5.0;
/// A longer pause starts a new gesture.
const GESTURE_GAP_MS: f64 = 200.0;
/// The average gap that moves one row an event.
const REFERENCE_GAP_MS: f64 = 100.0;
const MAX_AUTO_LINES: f64 = 6.0;
const ALT_MULTIPLIER: isize = 5;

/// Pi's wheel count, as Pi keeps it.
#[derive(Debug, Clone)]
pub struct Wheel {
    /// A fixed number of rows an event, from the setting.
    fixed: Option<isize>,
    accelerate: bool,
    last: Option<Instant>,
    last_way: isize,
    average: Option<f64>,
    carry: f64,
}

impl Wheel {
    pub fn new(fixed: Option<isize>, accelerate: bool) -> Self {
        Wheel {
            fixed,
            accelerate,
            last: None,
            last_way: 0,
            average: None,
            carry: 0.0,
        }
    }

    /// Pi's count for the setting of `cwd` and this machine.
    pub fn for_dir(cwd: &Path) -> Self {
        Wheel::new(setting(cwd), accelerates())
    }

    /// Whether Pi moves a fixed number of rows an event (its setting is a
    /// number): its events can go as they come.
    pub fn fixed(&self) -> bool {
        self.fixed.is_some() || !self.accelerate
    }

    /// Rows an event at `now` moves the chat (down is plus), `way` 1 down
    /// or -1 up; `alt` with Alt held.
    pub fn turn(&mut self, way: isize, alt: bool, now: Instant) -> isize {
        let way = if way < 0 { -1 } else { 1 };
        let rows = self.rows(way, now) * if alt { ALT_MULTIPLIER } else { 1 };
        way * rows
    }

    fn rows(&mut self, way: isize, now: Instant) -> isize {
        if let Some(n) = self.fixed {
            return n.max(1);
        }
        if !self.accelerate {
            return 1;
        }
        let gap = self.last.map_or(f64::INFINITY, |t| {
            now.duration_since(t).as_secs_f64() * 1000.0
        });
        let same = way == self.last_way && gap <= GESTURE_GAP_MS;
        self.last = Some(now);
        self.last_way = way;
        if !same {
            self.average = None;
            self.carry = 0.0;
            return 1;
        }
        if gap < BURST_GAP_MS {
            return 1;
        }
        let average = self.average.map_or(gap, |a| (a + gap) / 2.0);
        self.average = Some(average);
        let lines = (REFERENCE_GAP_MS / average).clamp(1.0, MAX_AUTO_LINES) + self.carry;
        let whole = lines.floor();
        self.carry = lines - whole;
        whole as isize
    }
}

/// Whether Pi speeds up quick wheel events here: everywhere but in a local
/// macOS terminal (Pi runs where peekme does, with its environment).
fn accelerates() -> bool {
    let ssh = ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
        .iter()
        .any(|v| std::env::var_os(v).is_some());
    !cfg!(target_os = "macos") || ssh
}

/// `fullscreenWheelScrollLines` as a number (1 to 100): the project's
/// `.pi/settings.json` over the user's; None for `"auto"` or unset.
pub fn setting(cwd: &Path) -> Option<isize> {
    let read = |p: &Path| -> Option<serde_json::Value> {
        let text = std::fs::read_to_string(p).ok()?;
        let v: serde_json::Value = serde_json::from_str(&text).ok()?;
        v.get("fullscreenWheelScrollLines").cloned()
    };
    let user = super::transcript::agent_dir().map(|d| d.join("settings.json"));
    let v = read(&cwd.join(".pi/settings.json")).or_else(|| user.as_deref().and_then(read))?;
    v.as_f64()
        .filter(|n| n.is_finite())
        .map(|n| (n.floor() as isize).clamp(1, 100))
}

/// Rows a PageUp or PageDown moves the chat that shows `rows` rows.
pub fn page_rows(rows: usize) -> isize {
    rows.saturating_sub(4).max(1) as isize
}

/// A wheel report moving one row `way` (the user's report without Alt,
/// at the same cell): what peekme sends Pi, once a row.
pub fn one_row(report: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(report);
    let Some(body) = text.strip_prefix("\x1b[<") else {
        return report.to_vec();
    };
    let Some((button, rest)) = body.split_once(';') else {
        return report.to_vec();
    };
    match button.parse::<u32>() {
        Ok(b) => format!("\x1b[<{};{rest}", b & !8).into_bytes(),
        Err(_) => report.to_vec(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn quick_turns_move_more_rows() {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut w = Wheel::new(None, true);
        // A gesture starts with one row; 50 ms apart is two rows an event.
        assert_eq!(w.turn(-1, false, at(0)), -1);
        assert_eq!(w.turn(-1, false, at(50)), -2);
        assert_eq!(w.turn(-1, false, at(100)), -2);
        // 20 ms apart on average: (50 + 20) / 2 = 35 ms -> 2.86 rows, then
        // (35 + 20) / 2 = 27.5 ms -> 3.64 rows plus the 0.86 carried.
        assert_eq!(w.turn(-1, false, at(120)), -2);
        assert_eq!(w.turn(-1, false, at(140)), -4);
        // One notch of some terminals: one row each.
        assert_eq!(w.turn(-1, false, at(142)), -1);
        // The other way starts a new gesture.
        assert_eq!(w.turn(1, false, at(160)), 1);
        // A pause starts a new gesture; Alt moves five times as far.
        assert_eq!(w.turn(1, true, at(500)), 5);
        // Bursts 150 ms apart, a row each: what peekme sends.
        let mut w = Wheel::new(None, true);
        for (i, ms) in [0, 1, 2, 152, 153, 303].into_iter().enumerate() {
            assert_eq!(w.turn(1, false, at(ms)), 1, "event {i}");
        }
    }

    #[test]
    fn fixed_rows_and_no_acceleration() {
        let now = Instant::now();
        assert_eq!(Wheel::new(Some(3), true).turn(-1, false, now), -3);
        let mut mac = Wheel::new(None, false);
        assert_eq!(mac.turn(1, false, now), 1);
        assert_eq!(mac.turn(1, false, now + Duration::from_millis(10)), 1);
        assert_eq!(page_rows(34), 30);
    }

    #[test]
    fn one_row_reports() {
        assert_eq!(one_row(b"\x1b[<72;40;10M"), b"\x1b[<64;40;10M".to_vec());
        assert_eq!(one_row(b"\x1b[<65;1;2M"), b"\x1b[<65;1;2M".to_vec());
    }
}
