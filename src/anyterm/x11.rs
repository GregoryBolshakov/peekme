//! X11: move the pointer with XTEST (the nudge), and watch who owns PRIMARY
//! with XFixes, so a selection made after the last key press is told from an
//! old one that is still in PRIMARY.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xfixes::{self, ConnectionExt as _, SelectionEventMask};
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, MOTION_NOTIFY_EVENT};
use x11rb::protocol::xtest::ConnectionExt as _;

/// The first move: one pixel, enough for VTE, which reports any motion.
const FIRST_STEP: i16 = 1;
/// Terminals that report motion only into another cell (xterm) need a move
/// wider than a cell; the row stays the same, which is what peekme needs.
const SECOND_STEP: i16 = 24;
/// How long the pointer stays moved before it goes back.
const HOLD: Duration = Duration::from_millis(20);
/// How long the first move gets to bring a report.
const FIRST_WAIT: Duration = Duration::from_millis(80);

/// Move the pointer a pixel sideways and back; if no report came, a bigger
/// step. Errors are ignored: no report just means the box opens as before.
pub fn nudge(answered: Arc<AtomicBool>) {
    let _ = try_nudge(&answered);
}

fn try_nudge(answered: &AtomicBool) -> Result<(), Box<dyn std::error::Error>> {
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    let width = conn.setup().roots[screen].width_in_pixels as i16;
    let p = conn.query_pointer(root)?.reply()?;
    let (x, y) = (p.root_x, p.root_y);
    let hop = |dx: i16| -> Result<(), Box<dyn std::error::Error>> {
        // Toward the middle of the screen, so the move never hits an edge.
        let to = if x + dx < width { x + dx } else { x - dx };
        conn.xtest_fake_input(MOTION_NOTIFY_EVENT, 0, x11rb::CURRENT_TIME, root, to, y, 0)?;
        conn.flush()?;
        std::thread::sleep(HOLD);
        conn.xtest_fake_input(MOTION_NOTIFY_EVENT, 0, x11rb::CURRENT_TIME, root, x, y, 0)?;
        conn.flush()?;
        Ok(())
    };
    hop(FIRST_STEP)?;
    let until = Instant::now() + FIRST_WAIT;
    while Instant::now() < until {
        if answered.load(Ordering::Relaxed) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    if !answered.load(Ordering::Relaxed) {
        hop(SECOND_STEP)?;
    }
    Ok(())
}

fn changed_at() -> &'static Mutex<Option<Instant>> {
    static AT: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    AT.get_or_init(|| Mutex::new(None))
}

/// When PRIMARY last got an owner (someone selected text), since the watch
/// started. None before the first change, or with no watch.
pub fn primary_changed_at() -> Option<Instant> {
    *changed_at().lock().unwrap()
}

/// Start the PRIMARY watch on its own thread (once per process).
pub fn watch_primary() {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    std::thread::spawn(|| {
        if let Err(e) = run_watch() {
            crate::log(&format!("anyterm: no X11 selection watch: {e}"));
        }
    });
}

fn run_watch() -> Result<(), Box<dyn std::error::Error>> {
    let (conn, screen) = x11rb::connect(None)?;
    let root = conn.setup().roots[screen].root;
    // XFixes must be told which version we speak before any other request.
    conn.xfixes_query_version(5, 0)?.reply()?;
    conn.xfixes_select_selection_input(
        root,
        AtomEnum::PRIMARY.into(),
        SelectionEventMask::SET_SELECTION_OWNER,
    )?;
    conn.flush()?;
    loop {
        if let Event::XfixesSelectionNotify(xfixes::SelectionNotifyEvent { .. }) =
            conn.wait_for_event()?
        {
            *changed_at().lock().unwrap() = Some(Instant::now());
        }
    }
}
