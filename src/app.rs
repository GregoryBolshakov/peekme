//! The wrapper: run the child in a PTY, pass its output through untouched,
//! and open peek boxes in place on request.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::panic::AssertUnwindSafe;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use alacritty_terminal::term::cell::Cell;
use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::agent::Agent;
use crate::explain::{self, Explainer, Progress};
use crate::input::{self, Consumed, Kind, Mouse, Token};
use crate::jobctl;
use crate::marks::{self, AnswerId, Marks, Place};
use crate::osc52::Osc52;
use crate::overlay::{self, Layout, PeekBox, Status};
use crate::render::{self, Boundary, SYNC_BEGIN, SYNC_END};
use crate::select::{self, Located, SelectionSource};
use crate::shadow::{Shadow, Snapshot};
use crate::{claude, codex, kiro};

mod nest;

enum Msg {
    Output(Vec<u8>),
    Input(Vec<u8>),
    Resize,
    ChildGone,
    Peek(u64, Progress),
    /// The child suspended itself (Ctrl+Z).
    ChildStopped,
}

/// Why `run` failed. Setup failures happen before the child starts, so the
/// caller can still run the command without peekme.
pub enum Failure {
    Setup(anyhow::Error),
    Runtime(anyhow::Error),
}

struct Open {
    id: u64,
    /// The selection this box explains, for escalating to the full conversation.
    selection: Option<(String, crate::context::ScreenSel)>,
    deep: bool,
    /// Where the selection is, to mark it once the answer is complete.
    mark: Option<(Place, Vec<(usize, usize)>)>,
    /// The saved answer the box shows, once there is one.
    answer: Option<AnswerId>,
    /// A saved answer to the same words that the text being written will
    /// replace. Until it is complete the box shows no saved answer, so the
    /// old one's inner marks are not drawn on (or clicked in) the new text.
    replaces: Option<AnswerId>,
    /// History size when the snapshot was taken (rows of marks count from it).
    history: usize,
    /// Boxes opened inside this one, outermost first.
    nested: Vec<nest::Nested>,
    /// The box's height before boxes opened inside it, to go back to.
    base_height: usize,
    /// Answer lines the model is asked for: what a box of the usual size
    /// shows. The box itself grows to fit the text.
    lines: usize,
    /// Text selected (or being selected) in one of the boxes.
    drag: Option<nest::Drag>,
    /// Words of a nested answer under the pointer: (box, chars of its answer).
    box_hover: Option<(usize, std::ops::Range<usize>)>,
    /// The last click in one of the boxes.
    last_click: Option<nest::BoxClick>,
    /// What each box row shows, from the last time the box was drawn.
    parts: Vec<Vec<overlay::Part>>,
    /// peekme turned mouse reports on for the box (the child had them off).
    own_mouse: bool,
    snap: Snapshot,
    lay: Layout,
    peek: PeekBox,
    /// Child output held back while the box is open, replayed on close.
    held: Vec<u8>,
    /// First row of the child's live area (Codex's composer and status),
    /// kept updating while the box is open.
    strip_top: Option<usize>,
    /// Where the child's input area starts, to see it move. On the full
    /// screen `strip_top` is above it: the blank rows the child keeps between
    /// the chat and its input (its status line shows there) are live too.
    composer: Option<usize>,
    /// What the live rows currently show on the terminal.
    strip_shown: Vec<Vec<Cell>>,
    strip_dirty: bool,
    /// Streamed text is drawn at most every FRAME; `dirty` marks text not shown yet.
    drawn_at: Instant,
    dirty: bool,
    /// When the wheel last went to the child: its output for FOLLOW after
    /// that is shown around the box (the chat scrolls, the box moves with it).
    follow: Option<Instant>,
    /// The screen when the box opened: closing puts the region back to it
    /// before the held output is replayed. `snap` is what the box is drawn
    /// with, newer once the chat scrolled.
    base: Snapshot,
    /// What each region row shows on the terminal, as drawn (None: unknown).
    shown: Vec<Option<String>>,
    /// Chat rows seen while the box is open, drawn, by `scrolled` + row: the
    /// rows next to the box when the chat scrolled them off the screen.
    mem: std::collections::HashMap<isize, String>,
    /// How far the chat scrolled since the box opened, in rows (up is plus).
    scrolled: isize,
    /// The child wrote since the box was last drawn around its screen.
    fresh: bool,
    /// The wheel turns of this run (one way, since the child answered the
    /// turns before): the rows each moves the chat by Codex's own count,
    /// oldest first; their way; and when the last one went.
    run: Vec<isize>,
    run_way: isize,
    sent: Instant,
    /// Updates from the child since the run began that changed nothing or
    /// moved the chat (two a turn), less those still owed to the run before;
    /// and how many there were when the chat last moved.
    answers: isize,
    moved_at: isize,
    /// Rows the chat moved the run's way since it began, and rows of the box
    /// shown in their place at the end of the chat.
    moved: isize,
    revealed: isize,
    /// The child wrote something since the last turn went, and when it last
    /// answered; whether it answered any turn of the run at all.
    heard: bool,
    answered_at: Instant,
    answering: bool,
    /// The start of a synchronized update the child has not ended yet.
    partial: Vec<u8>,
    /// The chat is at its end (1) or its start (-1): turns that way get the
    /// box's rows at once, until the chat moves again.
    end: Option<isize>,
    /// The box is over full-screen Codex's chat from its top row, where
    /// Codex can show its prompt header; whether it shows it now. The chat
    /// rows then start under it: where the header comes or goes, the rows
    /// move one less or more than Codex's place in the chat.
    heads: bool,
    header: bool,
}

/// What an update from the child does, while wheel turns wait for answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answer {
    /// Nothing: Codex's first answer to a turn, or its second when the turn
    /// moved no row (or the chat is at its end).
    Nothing,
    /// The chat's rows moved.
    Scrolled,
    /// Something else changed.
    Other,
}

impl Open {
    /// Turns of the run the child answered, two updates each.
    fn answered(&self) -> usize {
        ((self.answers.max(0) / 2) as usize).min(self.run.len())
    }

    /// The child answered every turn of the run.
    fn done(&self) -> bool {
        self.answers >= 2 * self.run.len() as isize
    }

    /// Rows the turns the child answered move the chat, the run's way: all
    /// of them once it answered the whole run, but one turn less while some
    /// are on their way (Codex can draw a turn's rows a little later).
    fn due(&self) -> isize {
        let n = self.answered();
        let n = if self.done() { n } else { n.saturating_sub(1) };
        self.run[..n].iter().map(|r| r.abs()).sum()
    }

    /// The answered turns moved the chat fewer rows than they should, and
    /// a whole turn's answers since the chat last moved moved nothing: it is
    /// at its end that way. The box's rows come for the rest. (A chat that
    /// keeps moving, fewer rows than counted, scrolls slower than peekme
    /// thinks: no end.) Codex takes several turns read at once together and
    /// answers them with fewer updates: once it went quiet after answering,
    /// all the turns count.
    fn settle_run(&mut self) {
        let all: isize = self.run.iter().map(|r| r.abs()).sum();
        // The chat moved after all, with the box's rows shown for it (a late
        // answer): take them back.
        let over = (self.moved + self.revealed - all).min(self.revealed);
        if over > 0 {
            self.reveal_rows(-over);
            return;
        }
        let quiet = self.heard && self.answered_at.elapsed() >= WHEEL_QUIET;
        let due = if quiet || self.done() {
            all
        } else {
            self.due()
        };
        let short = due - self.moved - self.revealed;
        if short <= 0 || (self.answers - self.moved_at < 2 && !quiet) {
            return;
        }
        self.reveal_rows(short);
        self.end = Some(self.run_way);
    }

    /// Show `rows` more of the box's rows the run's way (fewer: minus).
    fn reveal_rows(&mut self, rows: isize) {
        self.lay = overlay::reveal(&self.lay, self.run_way * rows);
        self.revealed += rows;
        crate::event(
            "reveal",
            serde_json::json!({"rows": self.run_way * rows, "box_top": self.lay.box_top}),
        );
    }
}

/// Mouse reports (button events, SGR form) peekme turns on while a box is open
/// over a child that has them off, so text in the box can be selected.
const MOUSE_ON: &str = "\x1b[?1002h\x1b[?1006h";
const MOUSE_OFF: &str = "\x1b[?1002l";
const SGR_OFF: &str = "\x1b[?1006l";
/// Reports still on their way after peekme turned them off are dropped for
/// this long, so a child that never asked for them gets none.
const STRAY_MOUSE: Duration = Duration::from_secs(1);

/// A wheel turn the child's chat has not moved for in this long, with no
/// answer at all from the child, found its end.
const WHEEL_UNMOVED: Duration = Duration::from_millis(150);
/// Codex took several turns in one read and answered them with fewer
/// updates than two a turn: this long after its last answer it is done.
const WHEEL_QUIET: Duration = Duration::from_millis(100);
/// Codex's synchronized updates: how one begins and ends, and the one that
/// changes nothing, its first answer to every turn of the wheel.
const SYNC_BEGIN_BYTES: &[u8] = b"\x1b[?2026h";
const SYNC_END_BYTES: &[u8] = b"\x1b[?2026l";
const CODEX_EMPTY_UPDATE: &[u8] = b"\x1b[?2026h\x1b[39m\x1b[49m\x1b[0m\x1b[?2026l";

/// How long after a wheel event the child's output is shown under the box.
const FOLLOW: Duration = Duration::from_millis(1500);

/// Redrawing the box for every streamed token would send ~200 KB per explanation.
const FRAME: Duration = Duration::from_millis(33);

/// A mouse selection the child reported with OSC 52 (Claude Code's full screen).
struct AppSelection {
    text: String,
    /// Where the drag ended, one end of the selection.
    cell: (usize, usize),
}

/// OSC 52 counts as a selection only this soon after a mouse release; other
/// clipboard writes (Claude's `/copy`) are not selections.
const SELECTION_AFTER_RELEASE: Duration = Duration::from_secs(2);

/// The snapshot cell shown at screen `cell` while a box laid out as `lay` is
/// open; none inside the box.
fn snap_cell(lay: &Layout, cell: (usize, usize)) -> Option<(usize, usize)> {
    Some((lay.snap_row(cell.0)?, cell.1))
}

/// A mark on screen: its text and the cells it covers.
struct MarkHit {
    text: String,
    cells: Vec<(usize, usize)>,
}

/// A left press on a mark, held back until it is a click (released on the
/// same cell) or a drag (then the child gets it after all).
struct HeldPress {
    bytes: Vec<u8>,
    cell: (usize, usize),
    hit: MarkHit,
}

/// A second press on the same cell this soon after a click that opened a box
/// finishes a double click: the box stays open. Any other press is a click of
/// its own, so closing and opening again is never held back.
const DOUBLE_CLICK: Duration = Duration::from_millis(350);

/// The last click on a mark, to tell the second half of a double click.
struct LastClick {
    at: Instant,
    cell: (usize, usize),
    /// The click opened a box (rather than closing one).
    opened: bool,
}

impl LastClick {
    /// `cell` pressed now finishes the double click this one started.
    fn doubled_by(&self, cell: (usize, usize)) -> bool {
        self.opened && self.cell == cell && self.at.elapsed() < DOUBLE_CLICK
    }
}
/// A press this soon after the window got focus is the click that brought it
/// to the front, not a click on a mark.
const ACTIVATION: Duration = Duration::from_millis(200);

/// A hotkey press waiting for the child to finish a synchronized update.
struct PendingOpen {
    since: Instant,
}

struct PendingHotkey {
    since: Instant,
    option_p: Option<Vec<u8>>,
    /// Alt+Q: the box asks for a question first.
    ask: bool,
}

const HOTKEY_AFTER_DRAG: Duration = Duration::from_secs(1);
const HOTKEY_SELECTION_WAIT: Duration = Duration::from_secs(1);

/// Run `program` in a pseudo-terminal with peekme. `agent` is the agent CLI
/// it is, if any.
pub fn run(
    program: &str,
    args: &[String],
    agent: Option<Agent>,
) -> std::result::Result<i32, Failure> {
    let setup = setup(program, args).map_err(Failure::Setup)?;
    let scroll_speed = matches!(agent, Some(Agent::Codex))
        .then(|| codex::scroll::speed_setting(args))
        .flatten();
    event_loop(setup, agent, scroll_speed).map_err(Failure::Runtime)
}

struct Setup {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    helper_pid: Option<u32>,
    /// The process started in the terminal: the helper, or the command itself.
    pty_pid: Option<u32>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    rx: mpsc::Receiver<Msg>,
    tx: Sender<Msg>,
    cols: u16,
    rows: u16,
    /// Where the cursor was when peekme started (0-based row, col).
    start: (u16, u16),
}

/// Ask the terminal where the cursor is (`CSI 6n`), waiting at most 1 s
/// (a slow SSH link can take a few hundred ms).
/// An agent that draws relative to where it starts (Claude Code's classic
/// screen) never tells us; without this the shadow would be rows off.
fn cursor_position() -> Option<(u16, u16)> {
    let mut out = std::io::stdout();
    out.write_all(b"\x1b[6n").ok()?;
    out.flush().ok()?;
    let deadline = Instant::now() + Duration::from_millis(1000);
    let mut reply = Vec::new();
    // Read the descriptor itself, one byte at a time: Rust's stdin is
    // buffered, and whatever it read ahead would reach the child later.
    while Instant::now() < deadline && !reply.ends_with(b"R") {
        let mut fds = libc::pollfd {
            fd: 0,
            events: libc::POLLIN,
            revents: 0,
        };
        let left = deadline
            .saturating_duration_since(Instant::now())
            .as_millis() as i32;
        if unsafe { libc::poll(&mut fds, 1, left.max(1)) } <= 0 {
            break;
        }
        let mut b = [0u8; 1];
        if unsafe { libc::read(0, b.as_mut_ptr().cast(), 1) } != 1 {
            break;
        }
        reply.push(b[0]);
    }
    let text = String::from_utf8_lossy(&reply);
    let body = text.rsplit("\x1b[").next()?.strip_suffix('R')?;
    let (r, c) = body.split_once(';')?;
    Some((
        r.parse::<u16>().ok()?.saturating_sub(1),
        c.parse::<u16>().ok()?.saturating_sub(1),
    ))
}

fn setup(program: &str, args: &[String]) -> Result<Setup> {
    // Test hook (debug builds only): pretend setup failed, to check fail-open.
    #[cfg(debug_assertions)]
    if std::env::var_os("PEEKME_TEST_FAIL_SETUP").is_some() {
        anyhow::bail!("setup failure requested by PEEKME_TEST_FAIL_SETUP");
    }
    let (cols, rows) = match crossterm::terminal::size() {
        Ok((c, r)) if c > 0 && r > 0 => (c, r),
        _ => (80, 24),
    };
    // Before the child starts, so nothing else answers on stdin.
    let asked = cursor_position();
    crate::event("start", serde_json::json!({"cursor": asked}));
    let start = asked.unwrap_or((0, 0));
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("could not open a pseudo-terminal")?;
    // Run the command under the job-control helper (see jobctl.rs) so that
    // Ctrl+Z works; without our own path, run it directly.
    // Resolved, so the helper shows up as peekme, not as our agent link.
    let exe = std::env::current_exe().and_then(|p| p.canonicalize()).ok();
    let mut cmd = match &exe {
        Some(exe) => {
            let mut c = CommandBuilder::new(exe);
            c.arg(jobctl::HELPER_ARG);
            c.arg(program);
            c
        }
        None => CommandBuilder::new(program),
    };
    cmd.args(args);
    cmd.cwd(std::env::current_dir()?);
    cmd.env(crate::launch::ACTIVE_ENV, "1");
    let child = pair
        .slave
        .spawn_command(cmd)
        .with_context(|| format!("could not start `{program}`"))?;
    let pty_pid = child.process_id();
    let helper_pid = exe.and(pty_pid);
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    let master = pair.master;

    let (tx, rx) = mpsc::channel::<Msg>();

    // Child output.
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut buf = vec![0u8; 65536];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(Msg::Output(buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                }
            }
            let _ = tx.send(Msg::ChildGone);
        });
    }
    // User input.
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let mut stdin = std::io::stdin();
            let mut buf = vec![0u8; 8192];
            while let Ok(n) = stdin.read(&mut buf) {
                if n == 0 || tx.send(Msg::Input(buf[..n].to_vec())).is_err() {
                    break;
                }
            }
        });
    }
    // Window size changes, and the helper reporting that the child stopped.
    {
        let tx = tx.clone();
        let mut signals =
            signal_hook::iterator::Signals::new([signal_hook::consts::SIGWINCH, jobctl::STOPPED])?;
        std::thread::spawn(move || {
            for sig in signals.forever() {
                let msg = if sig == jobctl::STOPPED {
                    Msg::ChildStopped
                } else {
                    Msg::Resize
                };
                if tx.send(msg).is_err() {
                    break;
                }
            }
        });
    }
    Ok(Setup {
        child,
        helper_pid,
        pty_pid,
        master,
        writer,
        rx,
        tx,
        cols,
        rows,
        start,
    })
}

fn event_loop(setup: Setup, agent: Option<Agent>, scroll_speed: Option<f64>) -> Result<i32> {
    let Setup {
        mut child,
        helper_pid,
        pty_pid,
        master,
        mut writer,
        rx,
        tx,
        cols,
        rows,
        start,
    } = setup;
    let mut app = App {
        shadow: {
            let mut s = Shadow::new(cols, rows);
            s.advance(format!("\x1b[{};{}H", start.0 + 1, start.1 + 1).as_bytes());
            s
        },
        known_from: start.0 as usize,
        open: None,
        pending: None,
        next_id: 0,
        consumed: Consumed::default(),
        carry: Vec::new(),
        in_paste: false,
        selection: SelectionSource::new(),
        explainer: Some(Explainer::for_agent(agent)),
        agent,
        pty_pid,
        via_helper: helper_pid.is_some(),
        osc52: Osc52::default(),
        app_selection: None,
        selection_source: "",
        last_release: None,
        mouse_press: None,
        recent_drag: None,
        pending_hotkey: None,
        tmux_baseline: None,
        boundary: Boundary::default(),
        greek_typed: false,
        french_typed: false,
        told_hidden: false,
        can_offer_accessibility: cfg!(target_os = "macos")
            && std::env::var_os("SSH_CONNECTION").is_none()
            && !crate::tmux::inside()
            && std::env::var("TERM_PROGRAM").is_ok_and(|t| t == "Apple_Terminal"),
        offered_accessibility: false,
        accessibility_trusted: crate::macax::trusted,
        accessibility_request: crate::macax::request,
        next_message: None,
        accessibility_offer_open: false,
        in_tmux: crate::tmux::inside(),
        over_ssh: std::env::var_os("SSH_CONNECTION").is_some(),
        mouse_seen: false,
        tx,
        cwd: std::env::current_dir()?.to_string_lossy().into_owned(),
        viewport_top: None,
        degraded: false,
        size: (cols, rows),
        marks: Marks::default(),
        orphans: Default::default(),
        own_mouse_off: None,
        marks_stale: false,
        marks_all: false,
        held_press: None,
        swallow_release: false,
        last_mark_click: None,
        focus_in_at: None,
        clicked: None,
        ask_next: false,
        hover: None,
        pointer_on: false,
        child_pointer: None,
        wheel: codex::scroll::Wheel::new(scroll_speed),
    };
    let mut stdout = std::io::stdout().lock();
    if app.in_tmux {
        crate::tmux::prepare_copy_mode();
    }
    let mut queue: VecDeque<Msg> = VecDeque::new();

    loop {
        let msg = match queue.pop_front() {
            Some(m) => m,
            None => match rx.recv_timeout(Duration::from_millis(40)) {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout) => {
                    guarded(&mut app, &mut stdout, |app, out| {
                        app.on_idle(out, &mut *writer)
                    })?;
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => break,
            },
        };
        match msg {
            Msg::Output(bytes) => guarded(&mut app, &mut stdout, |app, out| {
                app.on_output(&bytes, out, &mut *writer)
            })?,
            Msg::Input(bytes) => {
                if app.degraded {
                    writer.write_all(&bytes)?;
                    writer.flush()?;
                } else {
                    guarded(&mut app, &mut stdout, |app, out| {
                        app.on_input(&bytes, out, &mut *writer)
                    })?
                }
            }
            Msg::Resize => {
                let (c, r) = crossterm::terminal::size().unwrap_or(app.size);
                let _ = master.resize(PtySize {
                    rows: r,
                    cols: c,
                    pixel_width: 0,
                    pixel_height: 0,
                });
                guarded(&mut app, &mut stdout, |app, out| app.on_resize(c, r, out))?;
            }
            Msg::Peek(id, p) => guarded(&mut app, &mut stdout, |app, out| {
                app.on_progress(id, p, out)
            })?,
            Msg::ChildStopped => {
                // The child restores the terminal before it stops; let those
                // bytes reach the screen first.
                let deadline = Instant::now() + Duration::from_millis(60);
                while let Ok(m) =
                    rx.recv_timeout(deadline.saturating_duration_since(Instant::now()))
                {
                    match m {
                        Msg::Output(bytes) => guarded(&mut app, &mut stdout, |app, out| {
                            app.on_output(&bytes, out, &mut *writer)
                        })?,
                        other => queue.push_back(other),
                    }
                }
                guarded(&mut app, &mut stdout, |app, out| app.close(out, false))?;
                stdout.flush()?;
                jobctl::suspend_self();
                // Resumed. The window may have changed size meanwhile.
                queue.push_front(Msg::Resize);
                if let Some(pid) = helper_pid {
                    jobctl::continue_child(pid);
                }
            }
            Msg::ChildGone => break,
        }
    }

    if app.open.is_some() && !app.degraded {
        let _ = std::panic::catch_unwind(AssertUnwindSafe(|| app.close(&mut stdout, true)));
    }
    if app.in_tmux {
        crate::tmux::release_copy_mode();
    }
    if let Some(explainer) = &app.explainer {
        explainer.shutdown();
    }
    let status = child.wait()?;
    Ok(status.exit_code() as i32)
}

/// Run a handler; if the peek code panics, fall back to plain pass-through for
/// the rest of the session instead of taking the child down with it.
fn guarded(
    app: &mut App,
    out: &mut dyn Write,
    f: impl FnOnce(&mut App, &mut dyn Write) -> Result<()>,
) -> Result<()> {
    match std::panic::catch_unwind(AssertUnwindSafe(|| f(app, out))) {
        Ok(r) => r,
        Err(_) => {
            app.degrade(out);
            Ok(())
        }
    }
}

struct App {
    shadow: Shadow,
    open: Option<Open>,
    pending: Option<PendingOpen>,
    next_id: u64,
    consumed: Consumed,
    carry: Vec<u8>,
    in_paste: bool,
    selection: SelectionSource,
    /// Explains through the agent's own CLI, started on the first Alt+P (None in tests).
    explainer: Option<Explainer>,
    agent: Option<Agent>,
    pty_pid: Option<u32>,
    /// The command runs as a child of the job-control helper, not in the terminal directly.
    via_helper: bool,
    osc52: Osc52,
    app_selection: Option<AppSelection>,
    /// Where the last selection lookup found its text (for the event log).
    selection_source: &'static str,
    /// When and where the last mouse button release went to the child.
    last_release: Option<(Instant, (usize, usize))>,
    mouse_press: Option<(usize, usize)>,
    recent_drag: Option<Instant>,
    pending_hotkey: Option<PendingHotkey>,
    /// Inside tmux: tmux's newest paste buffer when the last mouse release went
    /// to the agent. Only a newer buffer is the agent's copy of a selection.
    tmux_baseline: Option<String>,
    /// Where the child's output stopped: a box may only be drawn between sequences.
    boundary: Boundary,
    /// First row peekme knows at startup; rows above it hold output from before
    /// the child started. It moves up as the screen scrolls.
    known_from: usize,
    /// The user typed Greek letters: `π` is a letter for them, never the hotkey.
    greek_typed: bool,
    /// The user typed French accents: `œ` is a letter for them, never the hotkey.
    french_typed: bool,
    /// Told the user once that the terminal keeps the mouse (tmux mouse off, or
    /// a terminal that never reports it), so no selection can be seen.
    told_hidden: bool,
    /// Terminal.app on this Mac: its own selection can be read through
    /// Accessibility once the user allows it, so peekme may offer that.
    can_offer_accessibility: bool,
    /// Offered it in this session (once is enough, whatever the answer).
    offered_accessibility: bool,
    /// Whether macOS allows it already, and asking for it (tests answer for
    /// both: a test must never show the macOS prompt).
    accessibility_trusted: fn() -> bool,
    accessibility_request: fn() -> bool,
    /// The next box shows this message, whatever is selected.
    next_message: Option<&'static str>,
    /// The open box is the offer: Enter asks macOS for the permission.
    accessibility_offer_open: bool,
    /// A mouse report came from the terminal (it does pass the mouse on).
    mouse_seen: bool,
    /// Read from the environment once at start (tests set them).
    in_tmux: bool,
    over_ssh: bool,
    tx: Sender<Msg>,
    cwd: String,
    /// Where the child's live area starts, learned from its scroll regions.
    viewport_top: Option<usize>,
    /// Set after a panic in the peek code: from then on, pass everything through.
    degraded: bool,
    size: (u16, u16),
    /// Selections that got an answer, drawn with a dotted underline.
    marks: Marks,
    /// Nested answers whose box closed before they were complete.
    orphans: std::collections::HashMap<u64, nest::Orphan>,
    /// When peekme last turned off the mouse reports it turned on for a box.
    own_mouse_off: Option<Instant>,
    /// The child wrote something since the marks were last drawn.
    marks_stale: bool,
    /// peekme repainted rows itself (closing a box): draw every mark again.
    marks_all: bool,
    held_press: Option<HeldPress>,
    /// The release of a press that was swallowed (a double click on a mark).
    swallow_release: bool,
    last_mark_click: Option<LastClick>,
    focus_in_at: Option<Instant>,
    /// A mark the user clicked, for the next open instead of the selection.
    clicked: Option<MarkHit>,
    /// The next box asks for a question instead of explaining (Alt+Q).
    ask_next: bool,
    /// Cells of the mark under the pointer, drawn with a solid underline.
    hover: Option<Vec<(usize, usize)>>,
    /// peekme set the pointer shape (OSC 22) to a hand.
    pointer_on: bool,
    /// The child's last pointer shape request, to give it back.
    child_pointer: Option<Vec<u8>>,
    /// Codex's count of wheel rows, kept in step with Codex's own.
    wheel: codex::scroll::Wheel,
}

impl App {
    fn on_output(
        &mut self,
        bytes: &[u8],
        out: &mut dyn Write,
        child: &mut dyn Write,
    ) -> Result<()> {
        if self.degraded {
            out.write_all(bytes)?;
            return Ok(out.flush()?);
        }
        if let Some(i) = find(bytes, b"\x1b]22;") {
            let end = bytes[i..]
                .iter()
                .position(|&b| b == 0x07 || b == b'\\')
                .map_or(bytes.len(), |e| i + e + 1);
            self.child_pointer = Some(bytes[i..end].to_vec());
        }
        if let Some(k) = viewport_top(bytes, self.size.1 as usize) {
            self.viewport_top = Some(k);
        }
        if self.open.is_none() {
            self.pass_through(bytes, out)?;
        } else {
            self.advance_open(bytes);
            self.marks_stale = !self.marks.is_empty();
        }
        if let Some(text) = self.osc52.feed(bytes)
            && let Some((at, cell)) = self.last_release
            && at.elapsed() < SELECTION_AFTER_RELEASE
        {
            self.app_selection = Some(AppSelection { text, cell });
        }
        match &mut self.open {
            None => {
                // The real terminal answers the child's queries; drop the shadow's copies.
                self.shadow.take_replies();
            }
            Some(open) => {
                open.held.extend_from_slice(bytes);
                open.fresh = true;
                // The real terminal won't see these bytes yet, so the shadow answers.
                let replies = self.shadow.take_replies();
                if !replies.is_empty() {
                    child.write_all(&replies)?;
                    child.flush()?;
                }
                match open.strip_top {
                    Some(top) => {
                        open.strip_dirty = true;
                        // Codex started or stopped working: its status moved
                        // where the chat ends, so draw it all again.
                        if self.codex_input_top().is_some()
                            && self
                                .codex_chat_end(&self.shadow.snapshot())
                                .is_some_and(|e| e != top)
                            && let Some(open) = &mut self.open
                        {
                            open.follow = Some(Instant::now());
                        }
                        // The live area grew into the box (an approval prompt, say):
                        // the child needs the room more than we do.
                        if self.viewport_top.is_some_and(|k| k < top) {
                            self.close(out, false)?;
                        }
                    }
                    None => open.peek.waiting_updates += 1,
                }
            }
        }
        if self.following() && self.can_draw() {
            self.follow_feed(out)?;
        }
        self.wheel_past_end(out)?;
        if self.pending.is_some() && self.can_draw() {
            self.pending = None;
            self.open_peek(out)?;
        }
        self.resolve_pending_hotkey(out, child)?;
        Ok(())
    }

    /// Pass the child's bytes to the terminal first: whatever happens below,
    /// the user sees the child. When a synchronized frame of the child ends,
    /// the marks it moved or wrote over are drawn into that frame, just before
    /// its end, so the terminal shows the text and its underline at once. A
    /// child that keeps redrawing (a spinner) would otherwise hold the marks
    /// back, and text that moved would show the old line until a quiet tick.
    fn pass_through(&mut self, bytes: &[u8], out: &mut dyn Write) -> Result<()> {
        let mut rest = bytes;
        while !self.marks.is_empty()
            && self.pending.is_none()
            && let Some(i) = find(rest, SYNC_END.as_bytes())
        {
            let end = i + SYNC_END.len();
            out.write_all(&rest[..i])?;
            self.shadow.advance(&rest[..end]);
            self.boundary.feed(&rest[..end]);
            self.marks_stale = true;
            let frame = if self.can_draw() {
                // The frame must end even when drawing the marks fails.
                std::panic::catch_unwind(AssertUnwindSafe(|| self.marks_bytes(out)))
            } else {
                Ok(Ok(String::new()))
            };
            let s = match frame {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    out.write_all(&rest[i..])?;
                    return Err(e);
                }
                Err(panic) => {
                    let _ = out.write_all(&rest[i..]);
                    std::panic::resume_unwind(panic);
                }
            };
            out.write_all(s.as_bytes())?;
            out.write_all(SYNC_END.as_bytes())?;
            rest = &rest[end..];
        }
        out.write_all(rest)?;
        out.flush()?;
        self.shadow.advance(rest);
        self.boundary.feed(rest);
        if !rest.is_empty() {
            self.marks_stale = !self.marks.is_empty();
        }
        Ok(())
    }

    fn on_idle(&mut self, out: &mut dyn Write, child: &mut dyn Write) -> Result<()> {
        self.resolve_pending_hotkey(out, child)?;
        self.tick(out)
    }

    /// First screen row whose content the shadow really knows.
    fn known_top(&self) -> usize {
        if self.shadow.alt_screen() {
            return 0;
        }
        self.known_from.saturating_sub(self.shadow.history_size())
    }

    /// The terminal is between the child's frames and escape sequences.
    fn can_draw(&self) -> bool {
        !self.shadow.in_sync_update() && self.boundary.at_boundary()
    }

    /// The child's input area on `snap`, per agent.
    fn composer_top(&self, snap: &Snapshot) -> Option<usize> {
        match self.agent {
            // Copilot draws its input box like Claude Code: `❯` between two rules.
            Some(Agent::Claude | Agent::Copilot) => claude::screen::composer_top(snap),
            Some(Agent::Kiro) => kiro::screen::composer_top(snap),
            _ => codex::screen::composer_top(snap),
        }
    }

    /// First row of the child's live area: from its scroll regions when it draws
    /// inline, from the position of its input box when it draws full screen.
    fn live_top(&self, snap: &Snapshot) -> Option<usize> {
        if self.shadow.alt_screen() {
            self.composer_top(snap)
        } else {
            self.viewport_top.or_else(|| self.composer_top(snap))
        }
    }

    /// Redraw the child's live rows under the box from the shadow, only the
    /// rows that changed, and put the cursor where the child has it.
    /// If the input area grew into the box (an approval question, say), the
    /// child needs the room more than we do. Claude replaces its input box
    /// with the question, so there any change of the box counts.
    fn strip_moved(&self, now: &Snapshot, top: usize) -> bool {
        match self.agent {
            Some(Agent::Claude | Agent::Copilot) => claude::screen::composer_top(now) != Some(top),
            Some(Agent::Kiro) => kiro::screen::composer_top(now) != Some(top),
            _ => {
                self.shadow.alt_screen()
                    && codex::screen::composer_top(now).is_some_and(|k| k < top)
            }
        }
    }

    /// Where full-screen Codex's chat ends on `snap`, with the box open: its
    /// status above the input area changes height while it works.
    fn codex_chat_end(&self, snap: &Snapshot) -> Option<usize> {
        let open = self.open.as_ref()?;
        let k = self.codex_input_top()?;
        Some(codex::screen::chat_end(snap, k)).filter(|&end| end > open.lay.region.0)
    }

    /// Full-screen Codex's input area under the open box, whose chat ends
    /// where the region does.
    fn codex_input_top(&self) -> Option<usize> {
        let codex = !matches!(
            self.agent,
            Some(Agent::Claude | Agent::Copilot | Agent::Kiro)
        );
        let open = self.open.as_ref()?;
        (codex && self.shadow.alt_screen() && open.strip_top == Some(open.lay.region.1))
            .then_some(open.composer)
            .flatten()
    }

    /// The wheel went to the child a moment ago and it has written since.
    fn following(&self) -> bool {
        self.open
            .as_ref()
            .is_some_and(|o| o.fresh && o.follow.is_some_and(|t| t.elapsed() < FOLLOW))
    }

    /// Show the child's new screen around the open box. Its output stays
    /// held (closing replays it); the box keeps its place next to the
    /// selection, which moved by as many rows as the chat scrolled, and only
    /// the rows that changed are written.
    fn follow_feed(&mut self, out: &mut dyn Write) -> Result<()> {
        let now = self.shadow.snapshot();
        if let Some(top) = self.open.as_ref().and_then(|o| o.composer)
            && self.strip_moved(&now, top)
        {
            return self.close(out, false);
        }
        let (history, alt) = (self.shadow.history_size(), self.shadow.alt_screen());
        let end = self.codex_chat_end(&now);
        let Some(open) = &mut self.open else {
            return Ok(());
        };
        let (lo, was) = open.lay.region;
        // Codex's status grew or shrank: the chat ends elsewhere, and what is
        // under it is drawn as Codex has it.
        if let Some(end) = end.filter(|&e| e != was) {
            // A box that was whole stays whole, pushing its selection.
            let (top, h) = (open.lay.box_top, open.lay.box_height as isize);
            if top >= lo as isize && top + h <= was as isize && top + h > end as isize {
                open.lay.box_top = (end as isize - h).max(lo as isize);
            }
            open.lay.region.1 = end;
            open.strip_top = Some(end);
            open.strip_shown.clear();
            open.shown.resize(end - lo, None);
        }
        let hi = open.lay.region.1;
        // Rows that are chat on both screens tell how far it moved.
        let both = hi.min(was).min(now.rows.len()).min(open.snap.rows.len());
        let found = match (&open.selection, &mut open.mark) {
            (Some((sel, _)), Some((place, cells))) if place.alt() == alt => place
                .find(&now, &select::Text::of(&now), sel, history)
                .inspect(|f| *cells = f.clone()),
            _ => None,
        };
        // Rows match rows: how far the chat moved (where rows repeat, as far
        // as the wheel turns sent say if that fits). The selection's own row
        // when nothing matches (it jumped further than a screen).
        let (way, so_far) = (open.run_way, open.moved);
        let expect: Vec<isize> = (open.run.iter())
            .scan(0, |sum, r| {
                *sum += r.abs();
                Some(*sum - so_far)
            })
            .filter(|&rows| rows > 0)
            .map(|rows| way * rows)
            .collect();
        // Codex's top row may be its prompt header: only the rows under it
        // are surely chat.
        let first = lo + usize::from(open.heads);
        let moved =
            overlay::scroll_delta_near(&open.snap, &now, first..both, &expect).or_else(|| {
                let f = found.as_ref()?;
                let home = if open.lay.below {
                    f[f.len() - 1].0 as isize + 1
                } else {
                    f[0].0 as isize
                };
                Some(open.lay.home - home)
            });
        let k = moved.unwrap_or(0);
        // Codex's place in the chat moved one row more than its rows where
        // its prompt header came (it covers a chat row), one less where it went.
        let header = open.heads
            && match moved {
                Some(k) => codex_header(open, &now, k, &expect),
                None => codex::screen::may_be_header(&now, lo),
            };
        let place = k + isize::from(header) - isize::from(open.header);
        open.header = header;
        if k != 0 {
            open.scrolled += k;
            open.lay = overlay::scroll_by(&open.lay, k);
        }
        if place != 0 || k != 0 {
            open.end = None;
        }
        if place.signum() == open.run_way {
            open.moved += place.abs();
        }
        open.settle_run();
        if open.run.len() == 1 && open.done() && open.revealed == 0 && open.moved > 0 {
            self.wheel.learn(open.run_way * open.moved);
        }
        for r in lo + usize::from(open.header)..hi {
            // A row of the chat keeps its text while it scrolls: a blank one
            // over a remembered one is something that stays put, not chat.
            let key = r as isize + open.scrolled;
            if !now.rows_text(r, r + 1).trim().is_empty() || !open.mem.contains_key(&key) {
                open.mem.insert(key, render::row_text(&now.rows[r]));
            }
        }
        crate::event(
            "follow",
            serde_json::json!({"moved": moved, "header": open.header, "box_top": open.lay.box_top}),
        );
        open.history = history;
        open.snap = now;
        open.fresh = false;
        open.strip_dirty = open.strip_top.is_some();
        let frame = self.box_bytes();
        out.write_all(frame.as_bytes())?;
        self.refresh_strip(out)?;
        out.flush()?;
        Ok(())
    }

    /// The wheel turned and a child that does not answer it (Codex does,
    /// see `answer_turns`) did not move its chat: it is at its end (or its
    /// start). The box made the chat longer than the child knows, by the
    /// rows it pushed past the bottom (or the top), so peekme scrolls those
    /// into view itself, a wheel step at a time.
    fn wheel_past_end(&mut self, out: &mut dyn Write) -> Result<()> {
        let can_draw = self.can_draw();
        let Some(open) = &mut self.open else {
            return Ok(());
        };
        if open.run.is_empty() || !can_draw {
            return Ok(());
        }
        let before = open.lay;
        if open.answering {
            // Codex answered and may have gone quiet: see `Open::settle_run`.
            open.settle_run();
        } else if open.sent.elapsed() >= WHEEL_UNMOVED && !open.done() {
            // No answer at all: a child that does not answer the wheel.
            let all: isize = open.run.iter().map(|r| r.abs()).sum();
            open.answers = 2 * open.run.len() as isize;
            let short = all - open.moved - open.revealed;
            if short > 0 {
                open.reveal_rows(short);
                open.end = Some(open.run_way);
            }
        }
        if open.lay == before {
            return Ok(());
        }
        let frame = self.box_bytes();
        out.write_all(frame.as_bytes())?;
        out.flush()?;
        Ok(())
    }

    /// PageUp or PageDown for a child that scrolls its chat with the wheel
    /// (full-screen Codex), with the box showing all its text: as many turns
    /// of the wheel as a page of the chat has rows, so the chat scrolls and
    /// the box follows it as it does the wheel. None for other children.
    fn page_as_wheel(&mut self, down: bool) -> Option<(Vec<u8>, isize)> {
        if !self.shadow.mouse_mode() || !self.shadow.sgr_mouse() {
            return None;
        }
        let open = self.open.as_mut()?;
        open.strip_top?;
        let (lo, hi) = open.lay.region;
        let page = (hi - lo).saturating_sub(1) as f64;
        let turns = ((page / self.wheel.speed()).round() as isize).max(1);
        // Over the middle of the chat: Codex scrolls for the wheel over its
        // chat rows only, and its top row may be its prompt header.
        let report = format!(
            "\x1b[<{};1;{}M",
            if down { 65 } else { 64 },
            (lo + hi) / 2 + 1
        );
        let bytes = report.repeat(turns as usize).into_bytes();
        Some((bytes, if down { turns } else { -turns }))
    }

    /// Wheel turns on their way to the child (down is plus). At the end of
    /// the chat already, the box's rows come at once, as many as the turns
    /// would move the chat; otherwise they wait for it to move, or to show
    /// its end.
    fn wheel_turns(&mut self, turns: isize, out: &mut dyn Write) -> Result<()> {
        let can_draw = self.can_draw();
        let way = turns.signum();
        let rows: Vec<isize> = (0..turns.abs()).map(|_| self.wheel.turn(way)).collect();
        crate::event("wheel", serde_json::json!({"rows": rows}));
        let Some(open) = &mut self.open else {
            return Ok(());
        };
        open.follow = Some(Instant::now());
        if open.run_way != way || open.done() {
            // A new run. What the child still owes the turns before does not
            // answer these.
            let owed = (2 * open.run.len() as isize - open.answers).max(0);
            open.run.clear();
            (open.run_way, open.answers, open.moved, open.revealed) = (way, -owed, 0, 0);
            (open.moved_at, open.answering) = (-owed, false);
            open.heard = false;
        }
        // Known to be at the end that way: the box's rows come at once. The
        // turns still go to the child, which answers them too.
        if open.end == Some(way) && can_draw {
            let all: isize = rows.iter().map(|r| r.abs()).sum();
            open.revealed += all;
            let lay = overlay::reveal(&open.lay, way * all);
            if lay != open.lay {
                open.lay = lay;
                let frame = self.box_bytes();
                out.write_all(frame.as_bytes())?;
                out.flush()?;
            }
        } else if open.end.is_some_and(|e| e != way) {
            open.end = None;
        }
        let Some(open) = &mut self.open else {
            return Ok(());
        };
        open.run.extend(rows);
        (open.sent, open.heard) = (Instant::now(), false);
        Ok(())
    }

    /// The child's output with the box open. While wheel turns wait for an
    /// answer it goes in one synchronized update at a time, each told apart
    /// by what it does to the chat. Codex answers every turn with two
    /// updates: one that changes nothing, then the scroll, or a second one
    /// that changes nothing when the turn moves no row. It sometimes draws
    /// two turns' rows in one; updates that change other things (its status,
    /// the selection's highlight) are no answers. Counting the answers tells
    /// how many turns Codex took, and `Open::settle_run` compares the rows.
    fn advance_open(&mut self, bytes: &[u8]) {
        let Some(open) = &mut self.open else {
            return;
        };
        if open.done() {
            open.partial.clear();
            self.shadow.advance(bytes);
            return;
        }
        let (lo, hi) = open.lay.region;
        let mut rest = bytes;
        while !rest.is_empty() {
            let cut = find(rest, SYNC_END_BYTES).map(|i| i + SYNC_END_BYTES.len());
            let (seg, tail) = rest.split_at(cut.unwrap_or(rest.len()));
            let before = cut.map(|_| self.shadow.rows_text(lo, hi));
            self.shadow.advance(seg);
            let Some(open) = &mut self.open else {
                return;
            };
            open.partial.extend_from_slice(seg);
            if let Some(before) = before {
                let start = rfind(&open.partial, SYNC_BEGIN_BYTES).unwrap_or(0);
                let answer = if open.partial[start..] == *CODEX_EMPTY_UPDATE {
                    Answer::Nothing
                } else if self.shadow.rows_text(lo, hi) != before {
                    Answer::Scrolled
                } else {
                    Answer::Other
                };
                crate::event(
                    "update",
                    serde_json::json!({"answer": format!("{answer:?}"), "len": open.partial.len() - start}),
                );
                open.partial.clear();
                open.heard = true;
                if answer != Answer::Other {
                    open.answers += 1;
                    (open.answered_at, open.answering) = (Instant::now(), true);
                }
                if answer == Answer::Scrolled {
                    open.moved_at = open.answers;
                }
            }
            rest = tail;
        }
        if let Some(open) = &mut self.open
            && open.partial.len() > 1 << 20
        {
            open.partial.clear();
        }
    }

    /// The wheel over an agent that leaves the mouse to the terminal: peekme
    /// stops taking the mouse, so the terminal scrolls its own history with
    /// the next turns of the wheel. The box stays as it is.
    fn lend_mouse(&mut self, out: &mut dyn Write) -> Result<()> {
        let Some(open) = &mut self.open else {
            return Ok(());
        };
        if !open.own_mouse {
            return Ok(());
        }
        open.own_mouse = false;
        let mut s = String::from(MOUSE_OFF);
        if !self.shadow.sgr_mouse() {
            s.push_str(SGR_OFF);
        }
        self.own_mouse_off = Some(Instant::now());
        crate::event("lend_mouse", serde_json::json!({}));
        out.write_all(s.as_bytes())?;
        out.flush()?;
        Ok(())
    }

    fn refresh_strip(&mut self, out: &mut dyn Write) -> Result<()> {
        let Some(open) = &mut self.open else {
            return Ok(());
        };
        let Some(top) = open.strip_top else {
            return Ok(());
        };
        let now = self.shadow.snapshot();
        if open.composer.is_some_and(|k| self.strip_moved(&now, k)) {
            return self.close(out, false);
        }
        let Some(open) = &mut self.open else {
            return Ok(());
        };
        let mut bytes = String::from(SYNC_BEGIN);
        for r in top..now.rows.len() {
            let i = r - top;
            let changed = open
                .strip_shown
                .get(i)
                .is_none_or(|shown| !same_cells(shown, &now.rows[r]));
            if changed {
                render::row(&mut bytes, r, &now.rows[r]);
            }
        }
        open.strip_shown = now.rows[top..].to_vec();
        open.strip_dirty = false;
        bytes.push_str(&strip_cursor(&now, top));
        bytes.push_str(SYNC_END);
        out.write_all(bytes.as_bytes())?;
        out.flush()?;
        Ok(())
    }

    /// Stop the peek feature for this session after a panic in it, putting the
    /// screen back as well as possible.
    fn degrade(&mut self, out: &mut dyn Write) {
        crate::log("peek code panicked; passing the child through for the rest of the session");
        self.degraded = true;
        self.pending = None;
        if let Some(open) = self.open.take() {
            let repaint = std::panic::catch_unwind(AssertUnwindSafe(|| {
                let mut s = String::from(SYNC_BEGIN);
                if let Some(top) = open.strip_top {
                    render::rows(&mut s, top, &open.base.rows[top..]);
                }
                s.push_str(&overlay::close_frame(&open.base, &open.lay));
                s
            }))
            .unwrap_or_default();
            let _ = out.write_all(repaint.as_bytes());
            let _ = out.write_all(&open.held);
            let _ = out.write_all(SYNC_END.as_bytes());
            let _ = out.flush();
        }
    }

    fn tick(&mut self, out: &mut dyn Write) -> Result<()> {
        if self.degraded {
            return Ok(());
        }
        self.shadow.check_sync_timeout();
        if self.following() && self.can_draw() {
            self.follow_feed(out)?;
        }
        self.wheel_past_end(out)?;
        if self.open.as_ref().is_some_and(|o| o.strip_dirty) {
            self.refresh_strip(out)?;
        }
        if let Some(p) = &self.pending
            && (self.can_draw() || p.since.elapsed() > Duration::from_millis(200))
        {
            self.pending = None;
            self.open_peek(out)?;
        }
        if self.open.as_ref().is_some_and(|o| o.dirty) {
            self.redraw_box(out)?;
        }
        // Ticks come when the child has been quiet for a moment, so the marks
        // aren't drawn into the middle of its frame.
        if (self.marks_stale || self.marks_all)
            && self.open.is_none()
            && self.pending.is_none()
            && self.can_draw()
        {
            self.draw_marks(out)?;
        }
        Ok(())
    }

    /// Draw the underline again under marks whose rows the child wrote over.
    fn draw_marks(&mut self, out: &mut dyn Write) -> Result<()> {
        let s = self.marks_bytes(out)?;
        if s.is_empty() {
            return Ok(out.flush()?);
        }
        let mut frame = String::from(SYNC_BEGIN);
        frame.push_str(&s);
        frame.push_str(SYNC_END);
        out.write_all(frame.as_bytes())?;
        out.flush()?;
        Ok(())
    }

    /// Bytes that draw the underline again where the marks need it and take it
    /// off where they left, the cursor and pen put back after; empty when
    /// nothing changed. The pointer shape goes to `out` straight away.
    fn marks_bytes(&mut self, out: &mut dyn Write) -> Result<String> {
        let damaged = self.shadow.take_damage();
        let all = std::mem::take(&mut self.marks_all);
        self.marks_stale = false;
        let snap = self.shadow.snapshot();
        let placed =
            self.marks
                .visible(&snap, self.shadow.history_size(), self.shadow.alt_screen());
        let known = self.known_top();
        let mut s = String::new();
        let stale: Vec<_> = self
            .marks
            .stale(&placed)
            .into_iter()
            .filter(|&(r, c)| r >= known && r < snap.rows.len() && c < snap.cols)
            .collect();
        marks::restore(&mut s, &snap, &stale);
        if !stale.is_empty() {
            self.hover = None;
        }
        for v in placed {
            let touched = all
                || v.moved
                || damaged
                    .as_ref()
                    .is_none_or(|rows| v.cells.iter().any(|(r, _)| rows.contains(r)));
            if touched && v.cells.iter().all(|&(r, _)| r >= known) {
                marks::draw(
                    &mut s,
                    &snap,
                    &v.cells,
                    self.hover.as_ref() == Some(&v.cells),
                );
                self.marks.drawn(v.index, &v.cells);
            }
        }
        self.set_pointer(self.hover.is_some(), out)?;
        if s.is_empty() {
            return Ok(s);
        }
        crate::event(
            "marks",
            serde_json::json!({"all": all, "damaged": damaged, "bytes": s.len()}),
        );
        render::restore_cursor(&mut s, &snap);
        Ok(s)
    }

    fn on_input(&mut self, bytes: &[u8], out: &mut dyn Write, child: &mut dyn Write) -> Result<()> {
        let mut pasting = self.in_paste;
        let tokens = input::tokenize(bytes, &mut self.carry, &mut self.in_paste);
        let mut forward: Vec<u8> = Vec::new();
        for mut t in tokens {
            match t.bytes.as_slice() {
                b"\x1b[200~" => pasting = true,
                b"\x1b[201~" => pasting = false,
                _ => {}
            }
            // While a question is typed, every key is part of it.
            if self.asking() && self.ask_key(&t, pasting, out)? {
                continue;
            }
            if t.kind == Kind::Key && !pasting && types_greek(&t.bytes) {
                self.greek_typed = true;
            }
            if t.kind == Kind::Key && !pasting && types_french(&t.bytes) {
                self.french_typed = true;
            }
            if matches!(t.kind, Kind::OptionP | Kind::OptionQ | Kind::OptionShiftP) {
                let ask = t.kind != Kind::OptionP;
                // `œ` is a letter for whoever types French, `π` and `∏` for Greek.
                let letter = if t.kind == Kind::OptionQ {
                    self.french_typed
                } else {
                    self.greek_typed
                };
                let mut hotkey = !letter && self.option_p_is_hotkey();
                if !hotkey && !letter && self.hotkey_follows_drag() {
                    self.flush_forward(&mut forward, child)?;
                    crate::event(
                        "option_p_wait",
                        serde_json::json!({"mouse": self.shadow.mouse_mode()}),
                    );
                    self.pending_hotkey = Some(PendingHotkey {
                        since: Instant::now(),
                        option_p: Some(t.bytes),
                        ask,
                    });
                    continue;
                }
                if !hotkey
                    && !letter
                    && !self.told_hidden
                    && (self.selection_hidden()
                        || (cfg!(target_os = "macos") && self.mouse_withheld()))
                {
                    // Option+P can never find a selection here: say why, once,
                    // instead of typing π without a word.
                    hotkey = true;
                    self.told_hidden = true;
                }
                if !hotkey && !letter && self.offers_accessibility() {
                    // Terminal.app keeps the drag to itself: offer, once, to
                    // let peekme read it instead of typing π without a word.
                    hotkey = true;
                    self.offered_accessibility = true;
                    self.next_message = Some(ACCESSIBILITY_OFFER);
                }
                crate::event(
                    "option_p",
                    serde_json::json!({
                        "hotkey": hotkey,
                        "source": self.selection_source,
                        "mouse": self.shadow.mouse_mode(),
                    }),
                );
                t.kind = match (hotkey, ask) {
                    (true, false) => Kind::Hotkey,
                    (true, true) => Kind::AskHotkey,
                    (false, _) => Kind::Key,
                };
                if !hotkey {
                    self.selection.settle();
                }
            }
            self.sniff_colors(&t);
            if matches!(t.bytes.as_slice(), b"\x1b[I" | b"\x1b[O") {
                self.consumed.clear();
                self.focus_in_at = (t.bytes == b"\x1b[I").then(Instant::now);
            }
            // Repeats and releases of keys we consumed never reach the child.
            if t.kind != Kind::Passive && self.consumed.swallow(&t) {
                continue;
            }
            if let Kind::Mouse(_) = t.kind {
                self.mouse_seen = true;
            }
            if let Kind::Mouse(m) = t.kind
                && self.open.is_some()
                && self.box_mouse(m, &t.bytes, out)?
            {
                continue;
            }
            if matches!(t.kind, Kind::Mouse(_))
                && self.open.is_none()
                && !self.shadow.mouse_mode()
                && self
                    .own_mouse_off
                    .is_some_and(|t| t.elapsed() < STRAY_MOUSE)
            {
                continue;
            }
            // The child has the mouse off, so this is a click tmux handed
            // over (see `tmux::prepare_copy_mode`): it opens a mark it hit and
            // never reaches the child, which did not ask for it.
            if let Kind::Mouse(m) = t.kind
                && self.in_tmux
                && self.open.is_none()
                && !self.shadow.mouse_mode()
            {
                self.mark_mouse(m, &t.bytes, &mut forward, out, child)?;
                continue;
            }
            if let Kind::Mouse(m) = t.kind
                && self.mark_mouse(m, &t.bytes, &mut forward, out, child)?
            {
                continue;
            }
            match (t.kind, self.open.is_some()) {
                (Kind::Hotkey | Kind::AskHotkey, _) => {
                    let ask = t.kind == Kind::AskHotkey;
                    self.consumed.consume(&t);
                    self.clicked = None;
                    if self.open.is_none() && self.hotkey_follows_drag() && !self.has_selection() {
                        self.flush_forward(&mut forward, child)?;
                        self.pending_hotkey = Some(PendingHotkey {
                            since: Instant::now(),
                            option_p: None,
                            ask,
                        });
                        continue;
                    }
                    if self.open.is_some() {
                        self.flush_forward(&mut forward, child)?;
                        // Text selected in a box: a box inside it.
                        if self.box_selection().is_some() {
                            self.open_nested(ask, out)?;
                            continue;
                        }
                        // Boxes inside: Alt+P with nothing selected in them does
                        // nothing, Alt+Q asks about the innermost box's words.
                        if self.open.as_ref().is_some_and(|o| !o.nested.is_empty()) {
                            if ask {
                                self.ask_in_nested(out)?;
                            }
                            continue;
                        }
                        // Alt+Q in a box: a question about its selection.
                        if ask && self.ask_in_box(out)? {
                            continue;
                        }
                        // Alt+P again on the same selection: explain with the whole conversation.
                        if self.escalate(out)? {
                            continue;
                        }
                        self.close(out, false)?;
                    }
                    self.ask_next = ask;
                    self.request_open(out)?;
                }
                (Kind::Key, true) if self.accessibility_offer_open && t.bytes == b"\r" => {
                    self.consumed.consume(&t);
                    self.flush_forward(&mut forward, child)?;
                    self.close(out, false)?;
                    let allowed = (self.accessibility_request)();
                    crate::event(
                        "accessibility_request",
                        serde_json::json!({"trusted": allowed}),
                    );
                    self.next_message = Some(if allowed {
                        ACCESSIBILITY_ALLOWED
                    } else {
                        ACCESSIBILITY_ASKED
                    });
                    self.request_open(out)?;
                }
                (Kind::Esc, true) => {
                    self.consumed.consume(&t);
                    if !self.close_innermost(out)? {
                        self.close(out, false)?;
                    }
                }
                (Kind::PageUp | Kind::PageDown, true) => {
                    self.consumed.consume(&t);
                    let down = t.kind == Kind::PageDown;
                    let depth = self.open.as_ref().map_or(0, |o| o.nested.len());
                    // A box with more text than rows scrolls; otherwise the chat.
                    let wheel = if self.can_scroll(depth, down) {
                        None
                    } else {
                        self.page_as_wheel(down)
                    };
                    match wheel {
                        Some((wheel, turns)) => {
                            self.wheel_turns(turns, out)?;
                            forward.extend_from_slice(&wheel);
                        }
                        None => self.scroll(down, out)?,
                    }
                }
                (Kind::ChatJump, true) => {
                    // Far from the selection: the box closes, Codex jumps.
                    self.flush_forward(&mut forward, child)?;
                    self.close(out, false)?;
                    forward.extend_from_slice(&t.bytes);
                }
                (Kind::Passive | Kind::Release, _) => forward.extend_from_slice(&t.bytes),
                // With the box open: moving the mouse (or the wheel to the
                // side: nothing scrolls that way) does nothing, the wheel
                // scrolls the chat (a box with more text than rows took it
                // already), a click elsewhere closes it (and still goes to the
                // child, e.g. to start a new selection).
                (Kind::Mouse(m), true) => match m {
                    Mouse::Motion | Mouse::WheelSide => {}
                    Mouse::WheelUp | Mouse::WheelDown => {
                        if self.shadow.mouse_mode() {
                            // The child scrolls; its new screen shows under the box.
                            let turn = if m == Mouse::WheelDown { 1 } else { -1 };
                            self.wheel_turns(turn, out)?;
                            forward.extend_from_slice(&t.bytes);
                        } else {
                            self.lend_mouse(out)?;
                        }
                    }
                    Mouse::Press => {
                        self.flush_forward(&mut forward, child)?;
                        self.close(out, false)?;
                        self.note_mouse(m, &t.bytes);
                        forward.extend_from_slice(&t.bytes);
                    }
                    Mouse::Release => {
                        self.note_mouse(m, &t.bytes);
                        forward.extend_from_slice(&t.bytes);
                    }
                },
                (Kind::Mouse(m), false) => {
                    self.note_mouse(m, &t.bytes);
                    // Codex counts these too.
                    if matches!(m, Mouse::WheelUp | Mouse::WheelDown) && self.shadow.mouse_mode() {
                        self.wheel.turn(if m == Mouse::WheelDown { 1 } else { -1 });
                    }
                    forward.extend_from_slice(&t.bytes);
                }
                (_, true) => {
                    let live = self.open.as_ref().is_some_and(|o| o.strip_top.is_some());
                    // With the child's input line live under the box, typing goes
                    // to it and the box stays. Enter sends the message and the
                    // conversation moves on, so the box closes. Without the live
                    // line any key closes the box.
                    if !live || t.bytes == b"\r" {
                        self.flush_forward(&mut forward, child)?;
                        self.close(out, false)?;
                    }
                    forward.extend_from_slice(&t.bytes);
                }
                (_, false) => forward.extend_from_slice(&t.bytes),
            }
        }
        self.flush_forward(&mut forward, child)
    }

    /// Clicks and hover on marks. A plain left press on a mark is held back:
    /// released on the same cell it opens the saved answer and the child never
    /// sees it, a drag sends it on so the child's selection starts there.
    /// Returns true when the token is used up here.
    fn mark_mouse(
        &mut self,
        m: Mouse,
        bytes: &[u8],
        forward: &mut Vec<u8>,
        out: &mut dyn Write,
        child: &mut dyn Write,
    ) -> Result<bool> {
        let cell = input::mouse_cell(bytes);
        let button = input::mouse_button(bytes).unwrap_or(u32::MAX);
        match m {
            Mouse::Press => {
                if let Some(h) = self.held_press.take() {
                    self.forward_press(&h.bytes, forward, out, child)?;
                }
                self.swallow_release = false;
                let Some(cell) = cell else {
                    return Ok(false);
                };
                // Modifiers belong to the terminal, tmux or the agent (links).
                if button != 0 || self.focus_in_at.is_some_and(|t| t.elapsed() < ACTIVATION) {
                    return Ok(false);
                }
                let Some(hit) = self.mark_hit(cell) else {
                    return Ok(false);
                };
                // Only one press finishes a double click, and the next starts afresh.
                if self
                    .last_mark_click
                    .take()
                    .is_some_and(|c| c.doubled_by(cell))
                {
                    self.swallow_release = true;
                    return Ok(true);
                }
                self.held_press = Some(HeldPress {
                    bytes: bytes.to_vec(),
                    cell,
                    hit,
                });
                Ok(true)
            }
            Mouse::Release => {
                if std::mem::take(&mut self.swallow_release) {
                    return Ok(true);
                }
                let Some(h) = self.held_press.take() else {
                    return Ok(false);
                };
                if cell == Some(h.cell) {
                    self.flush_forward(forward, child)?;
                    self.click_mark(h.hit, h.cell, out)?;
                    return Ok(true);
                }
                self.forward_press(&h.bytes, forward, out, child)?;
                Ok(false)
            }
            Mouse::Motion => {
                let held_button = button & 3 != 3;
                if held_button && let Some(h) = self.held_press.take() {
                    self.forward_press(&h.bytes, forward, out, child)?;
                } else if !held_button && self.held_press.is_none() && self.open.is_none() {
                    self.hover(cell, out)?;
                }
                Ok(false)
            }
            _ => Ok(false),
        }
    }

    /// A press the child gets after all, as it would have without peekme.
    fn forward_press(
        &mut self,
        bytes: &[u8],
        forward: &mut Vec<u8>,
        out: &mut dyn Write,
        child: &mut dyn Write,
    ) -> Result<()> {
        if self.open.is_some() {
            self.flush_forward(forward, child)?;
            self.close(out, false)?;
        }
        // A click tmux handed over is nobody else's.
        if !self.in_tmux || self.shadow.mouse_mode() {
            self.note_mouse(Mouse::Press, bytes);
            forward.extend_from_slice(bytes);
        }
        Ok(())
    }

    /// The mark at screen `cell`, if a click there is peekme's: the child
    /// takes the mouse, and the cell is not a link (links are the agent's).
    /// With a box open, only the mark of that box counts (a click closes it).
    fn mark_hit(&mut self, cell: (usize, usize)) -> Option<MarkHit> {
        // A child with the mouse off gets no clicks, but inside tmux peekme
        // gets the plain ones (see `tmux::prepare_copy_mode`).
        let handed_over = self.in_tmux && self.open.is_none();
        if self.marks.is_empty() || !(self.shadow.mouse_mode() || handed_over) {
            return None;
        }
        let known = self.known_top();
        let hit =
            |snap: &Snapshot, marks: &mut Marks, cell: (usize, usize), history: usize, alt| {
                if snap.rows.get(cell.0)?.get(cell.1)?.hyperlink().is_some() {
                    return None;
                }
                marks
                    .visible(snap, history, alt)
                    .into_iter()
                    .find(|v| v.cells.contains(&cell) && v.cells.iter().all(|&(r, _)| r >= known))
                    .map(|v| MarkHit {
                        text: v.text,
                        cells: v.cells,
                    })
            };
        let alt = self.shadow.alt_screen();
        if let Some(open) = &self.open {
            // The screen shows the snapshot with the box in it and rows moved
            // away from it: find the snapshot cell under the pointer. A click
            // on the box's own mark closes it, on another one switches to it.
            let cell = snap_cell(&open.lay, cell)?;
            return hit(&open.snap, &mut self.marks, cell, open.history, alt);
        }
        let snap = self.shadow.snapshot();
        hit(
            &snap,
            &mut self.marks,
            cell,
            self.shadow.history_size(),
            alt,
        )
    }

    /// A click on a mark: show its saved answer, or close it if it is open.
    fn click_mark(
        &mut self,
        hit: MarkHit,
        cell: (usize, usize),
        out: &mut dyn Write,
    ) -> Result<()> {
        let own = self
            .open
            .as_ref()
            .and_then(|o| o.selection.as_ref())
            .is_some_and(|(s, _)| *s == hit.text);
        crate::event(
            "mark_click",
            serde_json::json!({"selection": hit.text, "close": own}),
        );
        self.last_mark_click = Some(LastClick {
            at: Instant::now(),
            cell,
            opened: !own,
        });
        self.close(out, false)?;
        if own {
            return Ok(());
        }
        self.set_pointer(false, out)?;
        self.hover = None;
        self.clicked = Some(hit);
        self.request_open(out)
    }

    /// The pointer moved: a mark under it gets a solid underline and a hand
    /// pointer (OSC 22, where the terminal has it), like a link.
    fn hover(&mut self, cell: Option<(usize, usize)>, out: &mut dyn Write) -> Result<()> {
        if self.marks.is_empty() && self.hover.is_none() {
            return Ok(());
        }
        let now = cell.and_then(|c| self.mark_hit(c)).map(|h| h.cells);
        if now == self.hover {
            return Ok(());
        }
        let old = std::mem::replace(&mut self.hover, now);
        if !self.can_draw() {
            // Mid-frame: the next quiet tick draws every mark with this hover.
            self.marks_all = true;
            return Ok(());
        }
        let snap = self.shadow.snapshot();
        let mut frame = String::from(SYNC_BEGIN);
        if let Some(cells) = &old {
            marks::draw(&mut frame, &snap, cells, false);
        }
        if let Some(cells) = &self.hover {
            marks::draw(&mut frame, &snap, cells, true);
        }
        render::restore_cursor(&mut frame, &snap);
        frame.push_str(SYNC_END);
        out.write_all(frame.as_bytes())?;
        self.set_pointer(self.hover.is_some(), out)?;
        out.flush()?;
        Ok(())
    }

    /// The hand pointer over a mark; off it, the child's own shape again.
    fn set_pointer(&mut self, hand: bool, out: &mut dyn Write) -> Result<()> {
        if hand == self.pointer_on {
            return Ok(());
        }
        self.pointer_on = hand;
        if hand {
            out.write_all(b"\x1b]22;pointer\x1b\\")?;
        } else {
            match &self.child_pointer {
                Some(seq) => out.write_all(seq)?,
                // Empty: the terminal's own default.
                None => out.write_all(b"\x1b]22;\x1b\\")?,
            }
        }
        Ok(())
    }

    fn flush_forward(&mut self, forward: &mut Vec<u8>, child: &mut dyn Write) -> Result<()> {
        if !forward.is_empty() {
            child.write_all(forward)?;
            child.flush()?;
            forward.clear();
        }
        Ok(())
    }

    /// Remember the terminal's colours from its answers to the child's OSC 10/11 queries.
    fn sniff_colors(&mut self, t: &Token) {
        let Ok(s) = std::str::from_utf8(&t.bytes) else {
            return;
        };
        for (prefix, fg) in [("\x1b]10;rgb:", true), ("\x1b]11;rgb:", false)] {
            if let Some(rest) = s.strip_prefix(prefix) {
                let hex: Vec<u8> = rest
                    .trim_end_matches(['\x07', '\\', '\x1b'])
                    .split('/')
                    .filter_map(|h| {
                        u16::from_str_radix(h, 16)
                            .ok()
                            .map(|v| if h.len() > 2 { (v >> 8) as u8 } else { v as u8 })
                    })
                    .collect();
                if let [r, g, b] = hex[..] {
                    let rgb = alacritty_terminal::vte::ansi::Rgb { r, g, b };
                    if fg {
                        self.shadow.fg = Some(rgb);
                    } else {
                        self.shadow.bg = Some(rgb);
                    }
                }
            }
        }
    }

    /// Whether `π` (Option+P on a Mac) means "explain", or `œ` (Option+Q)
    /// "ask": when a box is open or a selection is on screen. Otherwise it is
    /// typed, so nobody loses the letter (and never once the user types Greek,
    /// or French for `œ`). Nothing else can tell a Mac keyboard apart: over
    /// SSH and in tmux the environment says nothing.
    fn option_p_is_hotkey(&mut self) -> bool {
        if self.open.is_some() {
            return true;
        }
        self.has_selection()
    }

    /// Whether π with nothing selected should offer Accessibility: in
    /// Terminal.app, with the drag the terminal's own (the agent leaves it the
    /// mouse), not allowed yet and not offered before.
    fn offers_accessibility(&self) -> bool {
        self.can_offer_accessibility
            && !self.offered_accessibility
            && self.open.is_none()
            && !self.shadow.mouse_mode()
            && !(self.accessibility_trusted)()
    }

    fn has_selection(&mut self) -> bool {
        let snap = self.shadow.snapshot();
        self.selection_within(&snap, crate::tmux::FRESH_FOR_OPTION_P)
            .1
            .is_some()
    }

    fn hotkey_follows_drag(&self) -> bool {
        self.recent_drag
            .is_some_and(|at| at.elapsed() < HOTKEY_AFTER_DRAG)
    }

    fn resolve_pending_hotkey(&mut self, out: &mut dyn Write, child: &mut dyn Write) -> Result<()> {
        let Some(pending) = &self.pending_hotkey else {
            return Ok(());
        };
        let timed_out = pending.since.elapsed() >= HOTKEY_SELECTION_WAIT;
        if self.has_selection() {
            let pending = self.pending_hotkey.take().unwrap();
            if pending.option_p.is_some() {
                crate::event(
                    "option_p",
                    serde_json::json!({
                        "hotkey": true,
                        "delayed": true,
                        "source": self.selection_source,
                        "mouse": self.shadow.mouse_mode(),
                    }),
                );
            }
            self.clicked = None;
            self.ask_next = pending.ask;
            self.request_open(out)?;
        } else if timed_out {
            let pending = self.pending_hotkey.take().unwrap();
            if let Some(bytes) = pending.option_p {
                child.write_all(&bytes)?;
                child.flush()?;
                self.selection.settle();
                crate::event(
                    "option_p",
                    serde_json::json!({
                        "hotkey": false,
                        "delayed": true,
                        "source": self.selection_source,
                        "mouse": self.shadow.mouse_mode(),
                    }),
                );
            } else {
                self.ask_next = pending.ask;
                self.request_open(out)?;
            }
        }
        Ok(())
    }

    /// A press starts a new selection (or is a click that clears the child's);
    /// the release is where a drag ended.
    fn note_mouse(&mut self, m: Mouse, bytes: &[u8]) {
        match m {
            Mouse::Press => {
                self.app_selection = None;
                self.recent_drag = None;
                self.mouse_press = (input::mouse_button(bytes) == Some(0))
                    .then(|| input::mouse_cell(bytes))
                    .flatten();
            }
            Mouse::Release => {
                if let Some(cell) = input::mouse_cell(bytes) {
                    let now = Instant::now();
                    self.recent_drag = self
                        .mouse_press
                        .take()
                        .filter(|&start| start != cell)
                        .map(|_| now);
                    self.last_release = Some((now, cell));
                    // Before the agent gets the release, so it cannot copy first.
                    if self.in_tmux {
                        self.tmux_baseline = crate::tmux::newest_buffer();
                    }
                }
            }
            _ => {}
        }
    }

    /// The selection to explain and where it is on `snap`. A selection the
    /// child draws itself wins: Claude's (reported with OSC 52, looked for
    /// near where the drag ended) or Codex's (reverse video). Otherwise the
    /// terminal's mouse selection (X11 PRIMARY), or `PEEKME_SELECTION`.
    fn current_selection(&mut self, snap: &Snapshot) -> (Option<String>, Option<Located>) {
        self.selection_within(snap, crate::tmux::FRESH)
    }

    /// Like `current_selection`, taking a tmux buffer only if at most `tmux_age` old.
    fn selection_within(
        &mut self,
        snap: &Snapshot,
        tmux_age: std::time::Duration,
    ) -> (Option<String>, Option<Located>) {
        let (text, located) = self.any_selection(snap, tmux_age);
        // Blank cells selected (a drag over an empty line) is no selection.
        match text {
            Some(t) if !t.trim().is_empty() => (Some(t), located),
            _ => (None, None),
        }
    }

    fn any_selection(
        &mut self,
        snap: &Snapshot,
        tmux_age: std::time::Duration,
    ) -> (Option<String>, Option<Located>) {
        self.selection_source = "none";
        // Codex copies the markdown source of an answer (`- **Desktop app**`),
        // which is not on screen as such. A long copy can still be found by
        // its first words, but then it is not what the screen shows: Codex's
        // highlight below is the selection.
        let highlight = matches!(self.agent, Some(Agent::Codex) | None)
            && codex::screen::selection(snap).is_some();
        let shown = |loc: &Located, text: &str| {
            !highlight
                || select::normalize(&loc.screen.text[loc.screen.start..loc.screen.end]).0
                    == select::normalize(&text.chars().collect::<Vec<_>>()).0
        };
        let mut unlocated = None;
        if let Some(s) = &self.app_selection {
            if let Some(loc) = select::locate(snap, &s.text, Some(s.cell))
                && shown(&loc, &s.text)
            {
                self.selection_source = "osc52";
                return (Some(s.text.clone()), Some(loc));
            }
            unlocated = Some(s.text.clone());
        }
        // Inside tmux, Claude Code and Codex copy a selection into a tmux
        // buffer instead of reporting it with OSC 52.
        if self.in_tmux
            && let Some(text) = crate::tmux::fresh_buffer(self.tmux_baseline.as_deref(), tmux_age)
        {
            let hint = self.last_release.map(|(_, cell)| cell);
            if let Some(loc) = select::locate(snap, &text, hint)
                && shown(&loc, &text)
            {
                self.selection_source = "tmux";
                return (Some(text), Some(loc));
            }
        }
        // Reverse video means "selected" in Codex only; other TUIs use it for
        // tabs and menus.
        if matches!(self.agent, Some(Agent::Codex) | None)
            && let Some(loc) = codex::screen::selection(snap)
        {
            self.selection_source = "reverse";
            return (Some(loc.screen.selected()), Some(loc));
        }
        if let Some(text) = unlocated {
            self.selection_source = "osc52";
            return (Some(text), None);
        }
        let s = self.selection.read();
        if s.is_some() {
            self.selection_source = if self.selection.from_accessibility {
                "accessibility"
            } else {
                "system"
            };
        }
        let l = s.as_deref().and_then(|s| select::locate(snap, s, None));
        (s, l)
    }

    /// The agent inside tmux leaves the mouse to the terminal, and tmux does
    /// too (its `mouse` option is off, the default). Codex does that on
    /// purpose. A drag is then the terminal's own selection: over SSH it lives
    /// on the user's machine, and on a Mac it is not even in the clipboard.
    /// peekme can never see it, so Option+P can never work there.
    fn selection_hidden(&self) -> bool {
        let remote = self.over_ssh || cfg!(target_os = "macos");
        self.in_tmux && remote && !self.shadow.mouse_mode() && crate::tmux::mouse() == Some(false)
    }

    /// The agent asked for the mouse, but not one mouse report came from the
    /// terminal: it keeps the mouse to itself (Terminal with View > Allow
    /// Mouse Reporting off, or every drag made with Option held in iTerm2).
    /// A drag is then the terminal's own selection, which peekme can't read.
    fn mouse_withheld(&self) -> bool {
        !self.in_tmux && !self.over_ssh && self.shadow.mouse_mode() && !self.mouse_seen
    }

    /// The agent's own process: with the job-control helper, the helper's child.
    fn agent_pid(&self) -> Option<u32> {
        let pid = self.pty_pid?;
        if !self.via_helper {
            return Some(pid);
        }
        crate::launch::children(pid).first().copied()
    }

    fn request_open(&mut self, out: &mut dyn Write) -> Result<()> {
        if !self.can_draw() {
            // Never cut into the child's half-finished frame or escape sequence.
            self.pending = Some(PendingOpen {
                since: Instant::now(),
            });
            return Ok(());
        }
        self.open_peek(out)
    }

    fn open_peek(&mut self, out: &mut dyn Write) -> Result<()> {
        // If the child's frame never finished, show what a terminal without
        // synchronized output already shows: everything received so far.
        if self.shadow.in_sync_update() {
            self.shadow.flush_sync();
        }
        let snap = self.shadow.snapshot();
        let rows = snap.rows.len();
        // Test hook (debug builds only): panic after the box is drawn, to check
        // that the session degrades to pass-through instead of dying.
        #[cfg(debug_assertions)]
        let panic_after_open = std::env::var_os("PEEKME_TEST_PANIC").is_some();
        self.next_id += 1;
        let id = self.next_id;
        let ask = std::mem::take(&mut self.ask_next);

        let forced = self.next_message.take();
        let (selection, located) = match self.clicked.take() {
            _ if forced.is_some() => (None, None),
            Some(hit) => {
                self.selection_source = "mark";
                let loc = select::at_cells(&snap, &hit.cells);
                (Some(hit.text), loc)
            }
            None => self.current_selection(&snap),
        };
        let hidden = selection.is_none() && self.selection_hidden();
        let withheld = selection.is_none() && !hidden && self.mouse_withheld();
        let mut remembered = None;
        let mut strip = None;
        let mut composer = None;
        let mut mark = None;
        let mut saved = None;
        let mut replace = None;
        let sel_first = located.as_ref().map(|l| l.first_row);
        let (mut peek, lay) = match (&selection, located) {
            (Some(sel), Some(loc)) => {
                // Keep the child's live area (input line, status) out of the box's
                // way when we know where it starts and the selection is above it.
                composer = self
                    .live_top(&snap)
                    .filter(|&k| loc.last_row < k && k < rows && k >= rows / 3);
                // Full-screen Codex keeps rows between its chat and its input
                // area: its status line ("Copy sent to terminal", "Working")
                // and gaps. They are not chat: they stay put while the chat
                // scrolls, so they stay live with the input area.
                let codex = !matches!(
                    self.agent,
                    Some(Agent::Claude | Agent::Copilot | Agent::Kiro)
                );
                strip = composer.map(|k| {
                    if codex && self.shadow.alt_screen() {
                        codex::screen::chat_end(&snap, k).max(loc.last_row + 1)
                    } else {
                        k
                    }
                });
                let lay = overlay::layout_within(
                    self.known_top(),
                    strip.unwrap_or(rows),
                    loc.first_row,
                    loc.last_row,
                );
                let mut peek = PeekBox::new(sel);
                let first = snap.text_with_positions().1[loc.screen.start];
                mark = Place::new(
                    &snap,
                    sel,
                    first,
                    self.shadow.history_size(),
                    self.shadow.alt_screen(),
                );
                if ask {
                    // Nothing is sent before the question is typed. A saved
                    // answer for these words gets the new one.
                    peek.input = Some(String::new());
                    replace = self.marks.find(sel);
                } else if let Some(id) = self.marks.find(sel)
                    && let Some(a) = self.marks.answer(id)
                {
                    // Asked before: show that answer, no new call, and mark this copy too.
                    saved = Some(id);
                    let (answer, model) = (a.answer.clone(), a.model.clone());
                    peek.question = a.question.clone();
                    if let Some((place, _)) = &mark {
                        self.marks.add(id, place.clone());
                    }
                    peek.text = answer;
                    peek.model = format!("{model} · saved");
                    peek.status = Status::Done;
                    peek.deep_available = true;
                } else {
                    let max_lines = lay.box_height.saturating_sub(2).clamp(3, 12);
                    if let Some(e) = self.start_explain(
                        id,
                        loc.screen.clone(),
                        max_lines,
                        false,
                        false,
                        None,
                        None,
                    ) {
                        peek.status = Status::Error(e);
                    }
                }
                remembered = Some((sel.clone(), loc.screen));
                (peek, lay)
            }
            (None, _) => {
                let msg = if let Some(m) = forced {
                    m.to_string()
                } else if hidden {
                    tmux_mouse_message(self.agent)
                } else if withheld {
                    withheld_mouse_message(self.agent)
                } else if self.over_ssh
                    && self.agent == Some(Agent::Kiro)
                    && !self.shadow.mouse_mode()
                {
                    "Kiro's inline screen leaves the mouse to your terminal, and over SSH the \
                     terminal's selection stays on your own computer, so peekme can't see it. \
                     Type /fullscreen in Kiro, then select and press Alt+P."
                        .to_string()
                } else if self.over_ssh {
                    // A terminal's own selection (a drag with Option held, over
                    // an agent that takes the mouse) never leaves the user's
                    // computer: no terminal hands it to a program over SSH.
                    "Select some text with the mouse first, then press Alt+P. Text selected \
                     with Option held stays in your terminal on your own computer, so peekme \
                     can't see it: drag without Option."
                        .to_string()
                } else {
                    "Select some text with the mouse first, then press Alt+P.".to_string()
                };
                {
                    let peek = PeekBox::message(&msg);
                    let lay = message_layout(&snap, self.known_top(), &peek);
                    (peek, lay)
                }
            }
            (Some(_), None) => {
                let msg = "The selected text isn't on screen (only visible text can be peeked in this version).";
                {
                    let peek = PeekBox::message(msg);
                    let lay = message_layout(&snap, self.known_top(), &peek);
                    (peek, lay)
                }
            }
        };

        peek.child_name = self.agent.map_or("Output", Agent::short);
        crate::event(
            "open",
            serde_json::json!({
                "selection": selection,
                "found": remembered.is_some(),
                "source": self.selection_source,
                "hidden": hidden,
                "withheld": withheld,
                "saved": saved.is_some(),
                "ask": ask,
                "mark": mark.as_ref().map(|(_, c)| c.len()),
                "alt": self.shadow.alt_screen(),
                "box_top": lay.box_top,
                "live_input": strip,
                "message": forced,
            }),
        );
        let lines = lay.box_height.saturating_sub(2);
        // The box starts as small as its text and grows with it (see `fit_top`).
        let lay = overlay::resize(&lay, peek.fitted_height(snap.cols).max(3));
        let strip_shown = strip.map_or_else(Vec::new, |top| snap.rows[top..].to_vec());
        // Full-screen Codex's top row may be its prompt header, not chat (a
        // guess until the chat moves; never with the selection on it).
        let codex = !matches!(
            self.agent,
            Some(Agent::Claude | Agent::Copilot | Agent::Kiro)
        );
        let lo = lay.region.0;
        let heads = codex && self.shadow.alt_screen() && strip.is_some() && lo == 0;
        let header =
            heads && codex::screen::may_be_header(&snap, lo) && sel_first.is_some_and(|r| r > lo);
        // Selecting text in the box needs the mouse reports.
        let own_mouse = !self.shadow.mouse_mode();
        self.accessibility_offer_open = forced == Some(ACCESSIBILITY_OFFER);
        self.open = Some(Open {
            id,
            selection: remembered,
            deep: false,
            mark,
            answer: saved,
            replaces: replace,
            history: self.shadow.history_size(),
            nested: Vec::new(),
            base_height: 0,
            lines,
            drag: None,
            box_hover: None,
            last_click: None,
            parts: Vec::new(),
            own_mouse,
            base: snap.clone(),
            shown: vec![None; lay.region.1 - lay.region.0],
            mem: (lo + usize::from(header)..lay.region.1.min(snap.rows.len()))
                .map(|r| (r as isize, render::row_text(&snap.rows[r])))
                .collect(),
            scrolled: 0,
            fresh: false,
            run: Vec::new(),
            run_way: 0,
            sent: Instant::now(),
            answers: 0,
            moved_at: 0,
            moved: 0,
            revealed: 0,
            heard: false,
            answered_at: Instant::now(),
            answering: false,
            partial: Vec::new(),
            end: None,
            heads,
            header,
            snap,
            lay,
            peek,
            held: Vec::new(),
            strip_top: strip,
            composer,
            strip_shown,
            strip_dirty: false,
            drawn_at: Instant::now(),
            dirty: false,
            follow: None,
        });
        let mut frame = self.box_bytes();
        if own_mouse {
            frame.push_str(MOUSE_ON);
        }
        out.write_all(frame.as_bytes())?;
        out.flush()?;
        #[cfg(debug_assertions)]
        if panic_after_open {
            panic!("panic requested by PEEKME_TEST_PANIC");
        }
        Ok(())
    }

    /// Start an explanation in the background; returns an error message if the
    /// explainer isn't available.
    #[allow(clippy::too_many_arguments)]
    fn start_explain(
        &self,
        id: u64,
        screen: crate::context::ScreenSel,
        max_lines: usize,
        deep: bool,
        force: bool,
        nested: Option<crate::context::Nested>,
        question: Option<String>,
    ) -> Option<String> {
        let Some(explainer) = self.explainer.clone() else {
            return Some("explainer disabled".into());
        };
        let req = explain::Request {
            screen,
            cwd: self.cwd.clone(),
            max_lines,
            deep,
            force,
            agent_pid: self.agent_pid(),
            nested,
            question,
        };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let send = |p| {
                let _ = tx.send(Msg::Peek(id, p));
            };
            let run = std::panic::catch_unwind(AssertUnwindSafe(|| explainer.explain(req, send)));
            if run.is_err() {
                let _ = tx.send(Msg::Peek(id, Progress::Failed("internal error".into())));
            }
        });
        None
    }

    /// If the open box explains the current selection, re-explain it with the
    /// whole conversation in the same box. Returns false when that doesn't apply.
    fn escalate(&mut self, out: &mut dyn Write) -> Result<bool> {
        let snap = self.shadow.snapshot();
        let (current, _) = self.current_selection(&snap);
        let Some(open) = &self.open else {
            return Ok(false);
        };
        let Some((sel, screen)) = &open.selection else {
            return Ok(false);
        };
        // Allowed once from the normal explanation, and once more to confirm
        // sending a big chat.
        let force = open.peek.confirm_deep;
        if (open.deep && !force) || current.as_deref() != Some(sel.as_str()) {
            return Ok(false);
        }
        let screen = screen.clone();
        let max_lines = open.lines.clamp(3, 12);
        crate::event("escalate", serde_json::json!({"selection": sel}));
        self.next_id += 1;
        let id = self.next_id;
        let err = self.start_explain(id, screen, max_lines, true, force, None, None);
        let open = self.open.as_mut().unwrap();
        open.id = id;
        open.deep = true;
        open.replaces = open.answer.take().or(open.replaces);
        open.peek.text.clear();
        open.peek.scroll = 0;
        open.peek.deep_available = false;
        open.peek.confirm_deep = false;
        open.peek.status = match err {
            Some(e) => Status::Error(e),
            None => Status::Thinking,
        };
        let frame = self.box_bytes();
        out.write_all(frame.as_bytes())?;
        out.flush()?;
        Ok(true)
    }

    /// A question is being typed in the innermost box.
    fn asking(&self) -> bool {
        self.open
            .as_ref()
            .is_some_and(|o| o.nested.last().map_or(&o.peek, |n| &n.peek).input.is_some())
    }

    /// A key while the question is typed: text goes into it, Backspace,
    /// Ctrl+U and Ctrl+W edit it, Enter sends it. Esc, the mouse and the
    /// terminal's replies are handled as usual (returns false).
    fn ask_key(&mut self, t: &Token, pasting: bool, out: &mut dyn Write) -> Result<bool> {
        if matches!(
            t.kind,
            Kind::Esc
                | Kind::Mouse(_)
                | Kind::Passive
                | Kind::Release
                | Kind::PageUp
                | Kind::PageDown
        ) {
            return Ok(false);
        }
        self.consumed.consume(t);
        let Some(input) = self.open.as_mut().and_then(|o| {
            o.nested
                .last_mut()
                .map_or(&mut o.peek, |n| &mut n.peek)
                .input
                .as_mut()
        }) else {
            return Ok(false);
        };
        let mut submit = false;
        match t.bytes.as_slice() {
            b"\x1b[200~" | b"\x1b[201~" => {}
            _ if pasting => {
                let text = String::from_utf8_lossy(&t.bytes);
                input.extend(text.chars().map(|c| if c.is_control() { ' ' } else { c }));
            }
            bytes if bytes.first() == Some(&0x1b) => {
                // Keys in the kitty or modifyOtherKeys form, once the child asked for them.
                if let Some(k) = input::parse_key(bytes)
                    && k.id.0 == b'u'
                    && k.event != 3
                    && k.mods & !1 == 0
                {
                    match k.id.1 {
                        13 => submit = true,
                        8 | 127 => {
                            input.pop();
                        }
                        c => {
                            if let Some(ch) = char::from_u32(c).filter(|ch| !ch.is_control()) {
                                if k.mods & 1 == 1 {
                                    input.extend(ch.to_uppercase());
                                } else {
                                    input.push(ch);
                                }
                            }
                        }
                    }
                }
            }
            // Typed text, which can hold Enter and editing keys too when the
            // terminal sends several keys in one read.
            bytes => {
                for c in String::from_utf8_lossy(bytes).chars() {
                    match c {
                        '\r' | '\n' => {
                            submit = true;
                            break;
                        }
                        '\x7f' | '\x08' => {
                            input.pop();
                        }
                        // Ctrl+U clears, Ctrl+W takes the last word.
                        '\x15' => input.clear(),
                        '\x17' => {
                            let keep = input.trim_end().rfind(' ').map_or(0, |i| i + 1);
                            input.truncate(keep);
                        }
                        c if c.is_control() => {}
                        c => input.push(c),
                    }
                }
            }
        }
        if submit {
            self.submit_question(out)?;
        } else {
            self.redraw_box(out)?;
        }
        Ok(true)
    }

    /// Enter: answer the typed question in the box, with the chat's model.
    fn submit_question(&mut self, out: &mut dyn Write) -> Result<()> {
        let Some(open) = &self.open else {
            return Ok(());
        };
        if !open.nested.is_empty() {
            return self.submit_nested_question(out);
        }
        let question = open.peek.input.as_deref().unwrap_or("").trim().to_string();
        let Some((sel, screen)) = open.selection.clone() else {
            return Ok(());
        };
        if question.is_empty() {
            return Ok(());
        }
        let max_lines = open.lines.max(3);
        crate::event(
            "ask",
            serde_json::json!({"selection": sel, "question": question}),
        );
        self.next_id += 1;
        let id = self.next_id;
        let err = self.start_explain(
            id,
            screen,
            max_lines,
            false,
            true,
            None,
            Some(question.clone()),
        );
        let open = self.open.as_mut().unwrap();
        open.id = id;
        open.deep = true;
        open.replaces = open.answer.take().or(open.replaces);
        open.peek.input = None;
        open.peek.question = Some(question);
        open.peek.text.clear();
        open.peek.scroll = 0;
        open.peek.deep_available = false;
        open.peek.ask_available = false;
        open.peek.confirm_deep = false;
        open.peek.status = match err {
            Some(e) => Status::Error(e),
            None => Status::Thinking,
        };
        self.redraw_box(out)
    }

    /// Alt+Q with a box open: ask about the words that box explains,
    /// in a new box of full size (the old one may have shrunk to its answer).
    /// Returns false when the box explains no selection.
    fn ask_in_box(&mut self, out: &mut dyn Write) -> Result<bool> {
        let Some(open) = &self.open else {
            return Ok(false);
        };
        let (Some((sel, _)), Some((_, cells))) = (&open.selection, &open.mark) else {
            return Ok(false);
        };
        self.clicked = Some(MarkHit {
            text: sel.clone(),
            cells: cells.clone(),
        });
        self.close(out, false)?;
        self.ask_next = true;
        self.request_open(out)?;
        Ok(true)
    }

    /// Close the box: repaint the region from the snapshot, then replay held
    /// output so the terminal reaches exactly the child's current state.
    fn close(&mut self, out: &mut dyn Write, skip_repaint: bool) -> Result<()> {
        self.close_below(0);
        self.accessibility_offer_open = false;
        let Some(open) = self.open.take() else {
            return Ok(());
        };
        self.selection.settle();
        crate::event("close", serde_json::json!({"held_bytes": open.held.len()}));
        let mut bytes = Vec::new();
        bytes.extend_from_slice(SYNC_BEGIN.as_bytes());
        let in_place = !skip_repaint && self.can_close_in_place(&open);
        if in_place {
            bytes.extend_from_slice(&self.close_in_place(&open));
        } else if !skip_repaint {
            // The live rows show newer content than the snapshot; put them back
            // too, so replaying the held output starts from the state it expects.
            let mut s = String::new();
            if let Some(top) = open.strip_top {
                render::rows(&mut s, top, &open.base.rows[top..]);
            }
            s.push_str(&overlay::close_frame(&open.base, &open.lay));
            bytes.extend_from_slice(s.as_bytes());
        }
        if open.own_mouse {
            bytes.extend_from_slice(MOUSE_OFF.as_bytes());
            if !self.shadow.sgr_mouse() {
                bytes.extend_from_slice(SGR_OFF.as_bytes());
            }
            self.own_mouse_off = Some(Instant::now());
        }
        self.set_pointer(false, &mut bytes)?;
        if !in_place {
            let held = render::strip_answered_queries(&open.held);
            self.boundary.feed(&held);
            bytes.extend_from_slice(&held);
        }
        // The repaint took the underlines off the rows the box covered or
        // moved: put them back in the same frame, so they never blink.
        self.marks_all = true;
        if !skip_repaint && !self.marks.is_empty() && self.can_draw() {
            let marks = self.marks_bytes(&mut bytes)?;
            bytes.extend_from_slice(marks.as_bytes());
        }
        bytes.extend_from_slice(SYNC_END.as_bytes());
        out.write_all(&bytes)?;
        out.flush()?;
        Ok(())
    }

    /// After the chat scrolled under a full-screen agent, the terminal shows
    /// its newer screen around the box: closing can draw the rest of that
    /// screen in place, instead of the old screen and then every held frame
    /// (which a terminal without synchronized output shows as flicker). Only
    /// when the held output stays on the alternate screen and ends between
    /// two sequences, so the terminal ends in exactly the child's state.
    fn can_close_in_place(&self, open: &Open) -> bool {
        let switches = [&b"\x1b[?1049"[..], b"\x1b[?1047", b"\x1b[?47", b"\x1bc"];
        let mut end = Boundary::default();
        end.feed(&open.held);
        open.follow.is_some()
            && self.shadow.alt_screen()
            && !self.shadow.in_sync_update()
            && end.at_boundary()
            && !switches
                .iter()
                .any(|s| open.held.windows(s.len()).any(|w| w == *s))
    }

    /// The held output's mode changes, then every row that differs from the
    /// child's screen now, written in place.
    fn close_in_place(&mut self, open: &Open) -> Vec<u8> {
        let modes = render::modes_only(&render::strip_answered_queries(&open.held));
        self.boundary.feed(&modes);
        let now = self.shadow.snapshot();
        let (lo, hi) = open.lay.region;
        let mut s = String::new();
        for (r, row) in now.rows.iter().enumerate() {
            let want = render::row_text(row);
            let has = if (lo..hi).contains(&r) {
                open.shown.get(r - lo).cloned().flatten()
            } else if let Some(top) = open.strip_top.filter(|&t| r >= t) {
                open.strip_shown.get(r - top).map(|c| render::row_text(c))
            } else {
                open.base.rows.get(r).map(|c| render::row_text(c))
            };
            if has.as_ref() != Some(&want) {
                s.push_str(&format!("\x1b[{};1H{want}", r + 1));
            }
        }
        render::restore_cursor(&mut s, &now);
        let mut bytes = modes;
        bytes.extend_from_slice(s.as_bytes());
        bytes
    }

    /// PgUp/PgDn: the innermost box scrolls.
    fn scroll(&mut self, down: bool, out: &mut dyn Write) -> Result<()> {
        let depth = self.open.as_ref().map_or(0, |o| o.nested.len());
        self.scroll_level(depth, down, out)
    }

    fn on_progress(&mut self, id: u64, p: Progress, out: &mut dyn Write) -> Result<()> {
        let Some(open) = &mut self.open else {
            return Ok(());
        };
        if open.id != id {
            return self.nested_progress(id, p, out);
        }
        match p {
            Progress::Started { model, source } => {
                open.peek.model = format!("{model} · {source}");
            }
            Progress::Delta(d) => {
                open.peek.text.push_str(&d);
                open.peek.status = Status::Streaming;
            }
            Progress::Done => {
                open.peek.status = Status::Done;
                open.peek.deep_available = !open.deep && open.peek.question.is_none();
                open.peek.ask_available = open.selection.is_some();
                if let (Some((sel, _)), Some((place, cells))) = (&open.selection, &open.mark)
                    && !open.peek.text.trim().is_empty()
                {
                    let answer = overlay::sanitize(&open.peek.text);
                    let id = match open.answer.or(open.replaces.take()) {
                        // Explained again with the whole chat.
                        Some(id) => {
                            self.marks.replace(id, &answer, &open.peek.model);
                            id
                        }
                        None => self.marks.save(None, sel, &answer, &open.peek.model),
                    };
                    open.answer = Some(id);
                    // A typed question is saved with its answer, so a click
                    // on the words shows both.
                    self.marks.set_question(id, open.peek.question.clone());
                    self.marks.add(id, place.clone());
                    crate::event(
                        "mark",
                        serde_json::json!({"selection": sel, "cells": cells.len()}),
                    );
                    // Underline it right away if the box leaves its rows alone.
                    if cells.iter().all(|&(r, _)| open.lay.snap_row(r) == Some(r)) {
                        let mut s = String::from(SYNC_BEGIN);
                        marks::draw(&mut s, &open.snap, cells, false);
                        render::restore_cursor(&mut s, &open.snap);
                        s.push_str("\x1b[?25l");
                        if let Some(top) = open.strip_top {
                            s.push_str(&strip_cursor(&self.shadow.snapshot(), top));
                        }
                        s.push_str(SYNC_END);
                        out.write_all(s.as_bytes())?;
                    }
                }
            }
            Progress::Failed(e) => open.peek.status = Status::Error(e),
            Progress::TooBig { tokens, ratio } => {
                open.peek.status = Status::Message(format!(
                    "This chat is about {}k tokens, about {ratio} times a normal peek. \
                     Press Alt+P again to send it anyway, or Esc to close.",
                    tokens / 1000
                ));
                open.peek.confirm_deep = true;
            }
        }
        if open.peek.status == Status::Streaming && open.drawn_at.elapsed() < FRAME {
            open.dirty = true;
            return Ok(());
        }
        self.redraw_box(out)
    }

    fn redraw_box(&mut self, out: &mut dyn Write) -> Result<()> {
        if self.open.is_none() {
            return Ok(());
        }
        let frame = self.box_bytes();
        out.write_all(frame.as_bytes())?;
        out.flush()?;
        Ok(())
    }

    fn on_resize(&mut self, cols: u16, rows: u16, out: &mut dyn Write) -> Result<()> {
        // The terminal has already reflowed the box; don't paint old-width rows
        // back. Codex reprints its whole transcript after a resize, and Claude
        // redraws its screen.
        if self.open.is_some() {
            self.close(out, true)?;
        }
        self.pending = None;
        self.marks_all = true;
        self.hover = None;
        self.held_press = None;
        self.viewport_top = None;
        self.size = (cols, rows);
        self.shadow.resize(cols, rows);
        Ok(())
    }
}

fn same_cells(a: &[Cell], b: &[Cell]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.c == y.c && x.fg == y.fg && x.bg == y.bg && x.flags == y.flags)
}

/// Cursor for the live rows: where the child has it, if it is visible and in
/// those rows; hidden otherwise (it would land inside the box).
fn strip_cursor(now: &Snapshot, top: usize) -> String {
    let (r, c) = now.cursor;
    if now.cursor_visible && r >= top {
        format!("\x1b[{};{}H\x1b[?25h", r + 1, c + 1)
    } else {
        "\x1b[?25l".into()
    }
}

/// The child's live area starts below the scroll region it uses to push
/// history up: Codex writes `CSI 1;K r`, then scrolls rows 1..K. Returns K as a
/// 0-based row (the first live row) for the last such region in `bytes`.
fn viewport_top(bytes: &[u8], rows: usize) -> Option<usize> {
    let mut found = None;
    let mut i = 0;
    while let Some(pos) = bytes[i..].windows(4).position(|w| w == b"\x1b[1;") {
        let start = i + pos + 4;
        let digits = bytes[start..]
            .iter()
            .take_while(|b| b.is_ascii_digit())
            .count();
        if digits > 0
            && bytes.get(start + digits) == Some(&b'r')
            && let Ok(k) = std::str::from_utf8(&bytes[start..start + digits])
                .unwrap_or("")
                .parse::<usize>()
            && k > 0
            && k < rows
        {
            found = Some(k);
        }
        i = start;
    }
    found
}

/// Why peekme sees no selection inside tmux with the mouse left to the terminal.
fn tmux_mouse_message(agent: Option<Agent>) -> String {
    if agent == Some(Agent::Kiro) {
        // Kiro inline never asks for the mouse, so no restart is needed.
        return "Kiro leaves the mouse to the terminal on its inline screen, and tmux has the \
                mouse off, so peekme cannot see what you select. Turn it on with `tmux set -g \
                mouse on` (and add `set -g mouse on` to ~/.tmux.conf), or type /fullscreen in Kiro."
            .into();
    }
    let who = agent.map_or("The program", Agent::short);
    format!(
        "{who} leaves the mouse to the terminal here, because tmux has the mouse off. So peekme \
         cannot see what you select. Turn it on with `tmux set -g mouse on` (and add \
         `set -g mouse on` to ~/.tmux.conf), then restart {}.",
        agent.map_or("it", Agent::command)
    )
}

const ACCESSIBILITY_OFFER: &str = "Terminal keeps what you select to itself, so peekme can't see it. \
     macOS can let peekme read it: press Enter, then turn on Terminal in System Settings > \
     Privacy & Security > Accessibility. Esc: not now.";

const ACCESSIBILITY_ASKED: &str = "Once Terminal is on in that list, select text and press Option+P \
     again. If π still appears, quit Terminal and open it again.";

const ACCESSIBILITY_ALLOWED: &str =
    "Terminal can already use Accessibility. Select text and press Option+P.";

/// Why peekme sees no selection when the terminal never reports the mouse.
fn withheld_mouse_message(agent: Option<Agent>) -> String {
    let who = agent.map_or("the program", Agent::short);
    format!(
        "Your terminal does not send the mouse to {who}, so what you select is the terminal's \
         own selection and peekme can't see it. In Terminal, turn on View > Allow Mouse \
         Reporting (Cmd+R). In iTerm2, drag without Option held."
    )
}

/// Typed text with Greek letters other than `π`.
/// Whether full-screen Codex's top chat row is its prompt header on `now`,
/// the chat rows having moved `k` (down is plus) since the box's screen.
/// The header and a prompt's own first row look the same; the chat row that
/// would be on top tells them apart: the one under it before (scrolling
/// down), or one seen before (up). Rows never seen, scrolling up: the header
/// stays the header, and a new one comes when the chat moved a row more than
/// Codex's turns say (`expect`, as in `follow_feed`) but not when it moved
/// just as far (the prompt's own row).
fn codex_header(open: &Open, now: &Snapshot, k: isize, expect: &[isize]) -> bool {
    let lo = open.lay.region.0;
    if !codex::screen::may_be_header(now, lo) {
        return false;
    }
    let old = &open.snap;
    let text = |s: &Snapshot, r: usize| render::row_text(&s.rows[r]);
    let top = text(now, lo);
    if k == 0 {
        return (top == text(old, lo)) == open.header;
    }
    let hi = open.lay.region.1.min(old.rows.len());
    let below = usize::try_from(k)
        .ok()
        .map(|k| lo + k)
        .filter(|&r| r < hi)
        .map(|r| text(old, r));
    match below.or_else(|| open.mem.get(&(lo as isize + open.scrolled + k)).cloned()) {
        Some(chat) => chat != top,
        None => {
            open.header
                || top == text(old, lo)
                || !(expect.contains(&k) && !expect.contains(&(k + 1)))
        }
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn rfind(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).rposition(|w| w == needle)
}

fn types_greek(bytes: &[u8]) -> bool {
    String::from_utf8_lossy(bytes).chars().any(|c| {
        matches!(c, '\u{0370}'..='\u{03ff}' | '\u{1f00}'..='\u{1fff}') && c != input::MAC_OPTION_P
    })
}

/// Typed text with the accents French writes words with `œ` among.
fn types_french(bytes: &[u8]) -> bool {
    String::from_utf8_lossy(bytes)
        .chars()
        .any(|c| "éèêëàâçîïôûùÿÉÈÊÀÇ".contains(c))
}

/// Where to show a one-line message: just above the cursor's row.
fn message_layout(snap: &Snapshot, known_top: usize, peek: &PeekBox) -> Layout {
    let rows = snap.rows.len();
    // Tall enough for the whole message, up to half the screen.
    let h = peek
        .fitted_height(snap.cols)
        .clamp(3, (rows / 2).max(3))
        .min(rows);
    let top = snap.cursor.0.saturating_sub(h).max(known_top).min(rows - h);
    Layout {
        box_top: top as isize,
        box_height: h,
        region: (top, top + h),
        below: true,
        home: top as isize,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shadow::Shadow;
    use alacritty_terminal::term::cell::Flags;

    /// Feed `frame` bytes into a second emulator standing in for the real
    /// terminal and compare its visible cells to the shadow's.
    fn same_screen(a: &Snapshot, b: &Snapshot) -> bool {
        a.rows.len() == b.rows.len()
            && a.rows.iter().zip(&b.rows).all(|(ra, rb)| {
                ra.iter()
                    .zip(rb)
                    .all(|(x, y)| x.c == y.c && x.fg == y.fg && x.bg == y.bg && x.flags == y.flags)
            })
            && a.cursor == b.cursor
    }

    const SCREEN: &[u8] = b"\x1b[1;1H\x1b[1mBold title\x1b[0m\r\n\
        \x1b[38;5;6mindexed colour\x1b[0m and \x1b[38;2;10;200;30mtrue colour\x1b[0m\r\n\
        \x1b[48;2;30;30;30m composer row with background              \x1b[0m\r\n\
        plain line \x1b[2mdim\x1b[0m \x1b[3mitalic\x1b[0m \x1b[4munderline\x1b[0m\r\n\
        wide: \xe4\xbd\xa0\xe5\xa5\xbd end\r\n\
        last line\x1b[3;5H";

    #[test]
    fn open_then_close_restores_the_screen_exactly() {
        for (first, last) in [(1, 1), (4, 5), (9, 9)] {
            let mut shadow = Shadow::new(40, 12);
            let mut real = Shadow::new(40, 12);
            shadow.advance(SCREEN);
            real.advance(SCREEN);
            let snap = shadow.snapshot();
            let lay = overlay::layout(12, first, last);
            let mut peek = PeekBox::new("indexed colour");
            peek.text =
                "An **explanation** with `code` that is long enough to wrap onto more lines."
                    .into();
            real.advance(overlay::open_frame(&snap, &lay, &peek).as_bytes());
            real.advance(overlay::box_frame(&snap, &lay, &peek).as_bytes());
            let small = overlay::resize(&lay, 4);
            real.advance(overlay::open_frame(&snap, &small, &peek).as_bytes());
            real.advance(overlay::close_frame(&snap, &small).as_bytes());
            assert!(
                same_screen(&real.snapshot(), &snap),
                "not restored after shrinking"
            );
            // Taller than its side: the selection moved away, and back on close.
            let tall = overlay::resize(&lay, 11);
            real.advance(overlay::open_frame(&snap, &tall, &peek).as_bytes());
            real.advance(overlay::close_frame(&snap, &tall).as_bytes());
            assert!(
                same_screen(&real.snapshot(), &snap),
                "not restored after a tall box"
            );
            real.advance(overlay::open_frame(&snap, &lay, &peek).as_bytes());
            assert!(
                !same_screen(&real.snapshot(), &snap),
                "the box should be visible"
            );
            real.advance(overlay::close_frame(&snap, &lay).as_bytes());
            assert!(
                same_screen(&real.snapshot(), &snap),
                "screen not restored for selection {first}..{last}"
            );
        }
    }

    /// The chars of `row` drawn with a dotted underline.
    fn dotted(s: &Snapshot, row: usize) -> String {
        s.rows[row]
            .iter()
            .filter(|c| c.flags.contains(Flags::DOTTED_UNDERLINE))
            .map(|c| c.c)
            .collect()
    }

    #[test]
    fn answered_selection_gets_a_dotted_underline_that_survives_redraws() {
        let mut app = test_app(40, 12);
        app.selection = SelectionSource::fixed("true colour");
        let mut real = Shadow::new(40, 12);
        let mut step = |app: &mut App, f: &dyn Fn(&mut App, &mut Vec<u8>)| {
            let mut bytes = Vec::new();
            f(app, &mut bytes);
            real.advance(&bytes);
            real.snapshot()
        };
        step(&mut app, &|app, out| {
            app.on_output(SCREEN, out, &mut Vec::new()).unwrap()
        });
        step(&mut app, &|app, out| app.open_peek(out).unwrap());
        let id = app.open.as_ref().unwrap().id;
        let answer = "Colour given as 24-bit RGB.";
        let shown = step(&mut app, &|app, out| {
            app.on_progress(id, Progress::Delta(answer.into()), out)
                .unwrap();
            app.on_progress(id, Progress::Done, out).unwrap();
        });
        // Underlined as soon as the answer is complete, box still open.
        assert_eq!(dotted(&shown, 1), "true colour");
        let shown = step(&mut app, &|app, out| {
            app.close(out, false).unwrap();
            app.tick(out).unwrap();
        });
        assert_eq!(dotted(&shown, 1), "true colour");
        assert_eq!(shown.cursor, app.shadow.snapshot().cursor);

        // The child draws the row again: the underline comes back after it settles.
        let redraw = b"\x1b[2;1H\x1b[38;5;6mindexed colour\x1b[0m and true colour\x1b[3;5H";
        let shown = step(&mut app, &|app, out| {
            app.on_output(redraw, out, &mut Vec::new()).unwrap()
        });
        assert_eq!(dotted(&shown, 1), "");
        let shown = step(&mut app, &|app, out| app.tick(out).unwrap());
        assert_eq!(dotted(&shown, 1), "true colour");
        // Other rows are not touched again.
        let quiet = step(&mut app, &|app, out| {
            app.on_output(b"\x1b[6;1Hlast line", out, &mut Vec::new())
                .unwrap();
            app.tick(out).unwrap();
            assert!(!String::from_utf8_lossy(out).contains("4:4"));
        });
        assert_eq!(dotted(&quiet, 1), "true colour");

        // Selecting it again shows the saved answer, with no new call.
        step(&mut app, &|app, out| app.open_peek(out).unwrap());
        let open = app.open.as_ref().unwrap();
        assert_eq!(open.peek.text, answer);
        assert_eq!(open.peek.status, Status::Done);
    }

    /// The box opens at 3 rows while it thinks, grows with every line that
    /// streams in, never past its region, and keeps that height when done.
    #[test]
    fn the_box_grows_with_its_text() {
        let mut app = test_app(40, 30);
        app.selection = SelectionSource::fixed("true colour");
        let mut out = Vec::new();
        app.on_output(SCREEN, &mut out, &mut Vec::new()).unwrap();
        app.open_peek(&mut out).unwrap();
        let open = app.open.as_ref().unwrap();
        let (id, region) = (open.id, open.lay.region);
        assert_eq!(open.lay.box_height, 3, "one line: thinking…");
        assert!(
            open.lines > 3,
            "the model is still asked for a usual answer"
        );
        let mut heights = Vec::new();
        for k in 0..40 {
            app.on_progress(id, Progress::Delta(format!("line {k}\n\n")), &mut out)
                .unwrap();
            // Frames are at most every FRAME apart: the tick draws what is left.
            app.open.as_mut().unwrap().drawn_at -= FRAME;
            app.tick(&mut out).unwrap();
            heights.push(app.open.as_ref().unwrap().lay.box_height);
        }
        assert_eq!(heights[0], 3);
        assert_eq!(heights[1], 5, "two paragraphs and the blank line between");
        assert!(heights.windows(2).all(|w| w[0] <= w[1]), "{heights:?}");
        assert_eq!(
            *heights.last().unwrap(),
            region.1 - region.0,
            "all of its region"
        );
        app.on_progress(id, Progress::Done, &mut out).unwrap();
        assert_eq!(
            app.open.as_ref().unwrap().lay.box_height,
            region.1 - region.0
        );
    }

    /// `SCREEN` with the mouse taken by the child and "true colour" (row 2,
    /// columns 20-30 in mouse terms) answered once.
    fn app_with_a_mark() -> App {
        let mut app = test_app(40, 12);
        app.selection = SelectionSource::fixed("true colour");
        let mut out = Vec::new();
        app.on_output(SCREEN, &mut out, &mut Vec::new()).unwrap();
        app.on_output(
            b"\x1b[?1000h\x1b[?1003h\x1b[?1006h",
            &mut out,
            &mut Vec::new(),
        )
        .unwrap();
        app.open_peek(&mut out).unwrap();
        let id = app.open.as_ref().unwrap().id;
        app.on_progress(id, Progress::Delta("24-bit RGB.".into()), &mut out)
            .unwrap();
        app.on_progress(id, Progress::Done, &mut out).unwrap();
        app.close(&mut out, false).unwrap();
        app.selection = SelectionSource::without_system();
        app
    }

    #[test]
    fn a_mark_moves_with_its_text_in_the_same_frame() {
        // Full screen, like Codex: a block above the mark unfolds and pushes it down.
        let page = |gap: usize| {
            let mut b = b"\x1b[?2026h\x1b[2J\x1b[1;1Htitle".to_vec();
            for r in 0..gap {
                b.extend(format!("\x1b[{};1Hdetail {r}", r + 2).bytes());
            }
            b.extend(format!("\x1b[{};1Hsee the true colour here\x1b[1;1H", gap + 2).bytes());
            b.extend(SYNC_END.bytes());
            b
        };
        let mut app = test_app(40, 12);
        let mut real = Shadow::new(40, 12);
        let mut out = b"\x1b[?1049h".to_vec();
        app.on_output(&out.clone(), &mut Vec::new(), &mut Vec::new())
            .unwrap();
        app.selection = SelectionSource::fixed("true colour");
        app.on_output(&page(0), &mut out, &mut Vec::new()).unwrap();
        app.open_peek(&mut out).unwrap();
        let id = app.open.as_ref().unwrap().id;
        app.on_progress(id, Progress::Delta("24-bit RGB.".into()), &mut out)
            .unwrap();
        app.on_progress(id, Progress::Done, &mut out).unwrap();
        app.close(&mut out, false).unwrap();
        app.tick(&mut out).unwrap();
        real.advance(&out);
        assert_eq!(dotted(&real.snapshot(), 1), "true colour");

        // Unfolded: no tick in between, the line is already at its new row.
        let mut out = Vec::new();
        app.on_output(&page(2), &mut out, &mut Vec::new()).unwrap();
        assert!(out.ends_with(SYNC_END.as_bytes()));
        real.advance(&out);
        let shown = real.snapshot();
        assert_eq!(dotted(&shown, 1), "");
        assert_eq!(dotted(&shown, 3), "true colour");
        assert_eq!(shown.cursor, (0, 0));
        // Folded again: back up, and the rows it left lose the line.
        let mut out = Vec::new();
        app.on_output(&page(0), &mut out, &mut Vec::new()).unwrap();
        real.advance(&out);
        let shown = real.snapshot();
        assert_eq!(dotted(&shown, 1), "true colour");
        assert_eq!(dotted(&shown, 3), "");
    }

    /// The agent takes the mouse; a box on "true colour" is open with `answer`.
    fn app_with_an_answer(answer: &str, mouse: bool) -> App {
        let mut app = test_app(40, 24);
        app.selection = SelectionSource::fixed("true colour");
        let mut out = Vec::new();
        app.on_output(SCREEN, &mut out, &mut Vec::new()).unwrap();
        if mouse {
            app.on_output(
                b"\x1b[?1000h\x1b[?1003h\x1b[?1006h",
                &mut out,
                &mut Vec::new(),
            )
            .unwrap();
        }
        app.open_peek(&mut out).unwrap();
        let id = app.open.as_ref().unwrap().id;
        app.on_progress(id, Progress::Delta(answer.into()), &mut out)
            .unwrap();
        app.on_progress(id, Progress::Done, &mut out).unwrap();
        app.selection = SelectionSource::without_system();
        app
    }

    /// SGR mouse report at 0-based screen `cell`: `b` button bits, press or release.
    fn mouse(b: u32, cell: (usize, usize), press: bool) -> Vec<u8> {
        let end = if press { 'M' } else { 'm' };
        format!("\x1b[<{b};{};{}{end}", cell.1 + 1, cell.0 + 1).into_bytes()
    }

    #[test]
    fn words_in_a_box_open_a_box_inside_it() {
        let answer = "A pseudo-terminal lets job control work inside tmux panes.";
        let mut app = app_with_an_answer(answer, true);
        let mut real = Shadow::new(40, 24);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        let top = app.open.as_ref().unwrap().lay.box_top as usize;
        // Body line 0 is the row under the top border; its text starts at column 2.
        // "A pseudo-terminal lets " is 23 chars: drag over "job control".
        let at = |col: usize| (top + 1, 2 + col);
        let mut drag = mouse(0, at(23), true);
        drag.extend(mouse(32, at(30), true));
        drag.extend(mouse(32, at(33), true));
        drag.extend(mouse(0, at(33), false));
        app.on_input(&drag, &mut out, &mut child).unwrap();
        assert!(
            app.open.is_some(),
            "a press in the box selects, it does not close"
        );
        real.advance(&out);
        let row: String = real.snapshot().rows[top + 1]
            .iter()
            .filter(|c| c.flags.contains(Flags::INVERSE))
            .map(|c| c.c)
            .collect();
        assert_eq!(row, "job control");

        out.clear();
        let base = app.open.as_ref().unwrap().lay.box_height;
        app.on_input(b"\x1bp", &mut out, &mut child).unwrap();
        let open = app.open.as_ref().unwrap();
        assert_eq!(open.nested.len(), 1);
        let n = &open.nested[0];
        assert_eq!(
            (n.words.as_str(), n.range.clone(), n.anchor),
            ("job control", 23..34, 0)
        );
        let id = n.id;
        app.on_progress(id, Progress::Delta("Ctrl+Z, fg and bg.".into()), &mut out)
            .unwrap();
        app.on_progress(id, Progress::Done, &mut out).unwrap();
        real.advance(&out);
        let snap = real.snapshot();
        assert!(
            snap.rows_text(top + 2, top + 3)
                .starts_with("│ ╭─ peek · job control")
        );
        assert!(
            snap.rows_text(top + 3, top + 4)
                .starts_with("│ │ Ctrl+Z, fg and bg.")
        );
        // Each box fits its text: the outer one its 2 lines and the inner box.
        let open = app.open.as_ref().unwrap();
        assert_eq!(open.nested[0].height, 3);
        assert_eq!(open.lay.box_height, 2 + 2 + 3);
        let root = app.open.as_ref().unwrap().answer.unwrap();
        let child_id = app.open.as_ref().unwrap().nested[0].answer.expect("saved");
        assert_eq!(app.marks.child_ranges(root), vec![(23..34, child_id)]);
        assert!(child.is_empty(), "the agent got no mouse report and no key");

        // Esc closes the inner box; its words keep a dotted line.
        out.clear();
        app.on_input(b"\x1b", &mut out, &mut child).unwrap();
        app.on_idle(&mut out, &mut child).unwrap();
        let open = app.open.as_ref().expect("only the inner box closed");
        assert!(open.nested.is_empty());
        assert_eq!(open.lay.box_height, base, "back to its height");
        real.advance(&out);
        let dotted: String = real.snapshot().rows[top + 1]
            .iter()
            .filter(|c| c.flags.contains(Flags::DOTTED_UNDERLINE))
            .map(|c| c.c)
            .collect();
        assert_eq!(dotted, "job control");

        // A click on them shows the saved answer, no new call; again closes it.
        let click = |c| [mouse(0, at(c), true), mouse(0, at(c), false)].concat();
        app.on_input(&click(25), &mut out, &mut child).unwrap();
        let n = &app.open.as_ref().unwrap().nested[0];
        assert_eq!((n.id, n.peek.text.as_str()), (0, "Ctrl+Z, fg and bg."));
        // (The same cell at once would be a double click, which keeps it open.)
        app.on_input(&click(27), &mut out, &mut child).unwrap();
        assert!(app.open.as_ref().unwrap().nested.is_empty());
        assert!(child.is_empty());
    }

    #[test]
    fn a_box_inside_a_box_inside_a_box() {
        let answer = "A pseudo-terminal lets job control work inside tmux panes.";
        let mut app = app_with_an_answer(answer, true);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        let top = app.open.as_ref().unwrap().lay.box_top as usize;
        let select = |app: &mut App, row: usize, from: usize, to: usize| {
            let drag = [
                mouse(0, (row, from), true),
                mouse(32, (row, to), true),
                mouse(0, (row, to), false),
            ]
            .concat();
            let (mut child, mut out) = (Vec::new(), Vec::new());
            app.on_input(&drag, &mut out, &mut child).unwrap();
            app.on_input(b"\x1bp", &mut out, &mut child).unwrap();
            assert!(child.is_empty());
        };
        select(&mut app, top + 1, 2 + 23, 2 + 33);
        let id = app.open.as_ref().unwrap().nested[0].id;
        app.on_progress(id, Progress::Delta("Ctrl+Z, fg and bg.".into()), &mut out)
            .unwrap();
        app.on_progress(id, Progress::Done, &mut out).unwrap();
        // The inner box's text starts at column 4, under its top border.
        select(&mut app, top + 3, 4 + 8, 4 + 9);
        let open = app.open.as_ref().unwrap();
        assert_eq!(open.nested.len(), 2);
        assert_eq!(open.nested[1].words, "fg");
        let ask = app.nested_request(1, &(8..10)).unwrap();
        assert_eq!(ask.words(), "fg");
        assert_eq!(
            ask.trail
                .iter()
                .map(|(w, _)| w.as_str())
                .collect::<Vec<_>>(),
            ["true colour", "job control"]
        );
        // Esc twice: back to the outer box, which is still open.
        app.on_input(b"\x1b", &mut out, &mut child).unwrap();
        app.on_idle(&mut out, &mut child).unwrap();
        app.on_input(b"\x1b", &mut out, &mut child).unwrap();
        app.on_idle(&mut out, &mut child).unwrap();
        assert!(app.open.as_ref().is_some_and(|o| o.nested.is_empty()));
        app.on_input(b"\x1b", &mut out, &mut child).unwrap();
        app.on_idle(&mut out, &mut child).unwrap();
        assert!(app.open.is_none());
    }

    #[test]
    fn the_wheel_scrolls_the_box_under_the_pointer() {
        let answer: String = (1..=30)
            .map(|i| format!("line {i} with job control\n"))
            .collect();
        let mut app = app_with_an_answer(&answer, true);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        let top = app.open.as_ref().unwrap().lay.box_top as usize;
        let at = |col: usize| (top + 1, 2 + col);
        let drag = [
            mouse(0, at(17), true),
            mouse(32, at(27), true),
            mouse(0, at(27), false),
        ]
        .concat();
        app.on_input(&drag, &mut out, &mut child).unwrap();
        app.on_input(b"\x1bp", &mut out, &mut child).unwrap();
        let id = app.open.as_ref().unwrap().nested[0].id;
        let inner: String = (1..=20).map(|i| format!("inner {i}\n")).collect();
        app.on_progress(id, Progress::Delta(inner), &mut out)
            .unwrap();
        app.on_progress(id, Progress::Done, &mut out).unwrap();
        // The inner box starts under body line 0: its first text row is top + 3.
        app.on_input(&mouse(65, (top + 3, 6), true), &mut out, &mut child)
            .unwrap();
        let open = app.open.as_ref().unwrap();
        assert!(open.nested[0].peek.scroll > 0 && open.peek.scroll == 0);
        // On the outer box's own border, the outer box scrolls.
        app.on_input(&mouse(65, (top + 3, 1), true), &mut out, &mut child)
            .unwrap();
        assert!(app.open.as_ref().unwrap().peek.scroll > 0);
        // PgDn goes to the innermost box.
        let before = app.open.as_ref().unwrap().nested[0].peek.scroll;
        app.on_input(b"\x1b[5~", &mut out, &mut child).unwrap();
        assert!(app.open.as_ref().unwrap().nested[0].peek.scroll < before);
        assert!(child.is_empty());
    }

    #[test]
    fn a_box_over_an_agent_without_mouse_reports_turns_them_on_and_off() {
        let mut app = app_with_an_answer("An answer.", false);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        let open = app.open.as_ref().unwrap();
        assert!(open.own_mouse);
        // Press far below the box: it closes, and the agent gets nothing.
        app.on_input(&mouse(0, (23, 0), true), &mut out, &mut child)
            .unwrap();
        assert!(app.open.is_none());
        assert!(String::from_utf8_lossy(&out).contains("\x1b[?1002l\x1b[?1006l"));
        app.on_input(&mouse(0, (23, 0), false), &mut out, &mut child)
            .unwrap();
        assert!(child.is_empty(), "{:?}", String::from_utf8_lossy(&child));
    }

    #[test]
    fn a_click_on_a_mark_opens_its_answer_and_again_closes_it() {
        let mut app = app_with_a_mark();
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.on_input(b"\x1b[<0;24;2M\x1b[<0;24;2m", &mut out, &mut child)
            .unwrap();
        let open = app.open.as_ref().expect("the click opened the box");
        assert_eq!(open.peek.text, "24-bit RGB.");
        assert!(child.is_empty(), "the child never saw the click");

        // A double click is one open: its second press is dropped.
        app.on_input(b"\x1b[<0;24;2M\x1b[<0;24;2m", &mut out, &mut child)
            .unwrap();
        assert!(app.open.is_some() && child.is_empty());

        // Later, a click on the same mark closes the box.
        app.last_mark_click = None;
        app.on_input(b"\x1b[<0;22;2M\x1b[<0;22;2m", &mut out, &mut child)
            .unwrap();
        assert!(app.open.is_none());
        assert!(child.is_empty());
    }

    #[test]
    fn marks_under_a_closing_box_are_back_in_the_same_frame() {
        let mut app = test_app(40, 12);
        let mut real = Shadow::new(40, 12);
        let mut out = Vec::new();
        app.on_output(SCREEN, &mut out, &mut Vec::new()).unwrap();
        let explain = |app: &mut App, sel: &str, out: &mut Vec<u8>| {
            app.selection = SelectionSource::fixed(sel);
            app.open_peek(out).unwrap();
            let id = app.open.as_ref().unwrap().id;
            app.on_progress(id, Progress::Delta("x".into()), out)
                .unwrap();
            app.on_progress(id, Progress::Done, out).unwrap();
            app.close(out, false).unwrap();
        };
        explain(&mut app, "true colour", &mut out);
        app.tick(&mut out).unwrap();
        // A box on the title, row 0, covers row 1 with "true colour" on it.
        explain(&mut app, "Bold title", &mut out);
        real.advance(&out);
        assert_eq!(dotted(&real.snapshot(), 1), "true colour", "no tick needed");
    }

    #[test]
    fn quick_clicks_on_a_mark_close_and_open_again() {
        let mut app = app_with_a_mark();
        let (mut child, mut out) = (Vec::new(), Vec::new());
        let click = |col: usize| [mouse(0, (1, col), true), mouse(0, (1, col), false)].concat();
        // Open, then at once a click on another cell of the mark closes it,
        // and the next one opens it again: none of them is held back.
        app.on_input(&click(23), &mut out, &mut child).unwrap();
        assert!(app.open.is_some());
        app.on_input(&click(21), &mut out, &mut child).unwrap();
        assert!(app.open.is_none(), "a quick click closes");
        app.on_input(&click(21), &mut out, &mut child).unwrap();
        assert!(app.open.is_some(), "and the next one opens again");
        // A double click (same cell, right after it opened) leaves it open;
        // a third click is a click of its own and closes.
        app.on_input(&click(21), &mut out, &mut child).unwrap();
        assert!(app.open.is_some(), "the double click's second press");
        app.on_input(&click(21), &mut out, &mut child).unwrap();
        assert!(app.open.is_none(), "no chain of held-back presses");
        assert!(child.is_empty());
    }

    #[test]
    fn a_click_on_another_mark_switches_to_its_answer() {
        let mut app = app_with_a_mark();
        let mut out = Vec::new();
        app.selection = SelectionSource::fixed("indexed colour");
        app.open_peek(&mut out).unwrap();
        let id = app.open.as_ref().unwrap().id;
        app.on_progress(id, Progress::Delta("256 colours.".into()), &mut out)
            .unwrap();
        app.on_progress(id, Progress::Done, &mut out).unwrap();
        app.selection = SelectionSource::without_system();
        // The box on "indexed colour" is open below row 1; "true colour" is
        // on row 1 too, outside the box.
        assert!(app.open.as_ref().unwrap().lay.below);
        let mut child = Vec::new();
        let click = [mouse(0, (1, 24), true), mouse(0, (1, 24), false)].concat();
        app.on_input(&click, &mut out, &mut child).unwrap();
        let open = app.open.as_ref().expect("one click switched boxes");
        assert_eq!(open.peek.text, "24-bit RGB.");
        assert!(child.is_empty());
    }

    #[test]
    fn double_clicks_in_a_box() {
        let answer = "A pseudo-terminal lets job control work inside tmux panes.";
        let mut app = app_with_an_answer(answer, true);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        let top = app.open.as_ref().unwrap().lay.box_top as usize;
        let click = |col: usize| {
            [
                mouse(0, (top + 1, 2 + col), true),
                mouse(0, (top + 1, 2 + col), false),
            ]
            .concat()
        };
        // A double click on a word selects it; Alt+P explains it.
        app.on_input(&[click(25), click(25)].concat(), &mut out, &mut child)
            .unwrap();
        let d = app.box_selection().expect("the word is selected");
        assert_eq!((d.from, d.to), ((0, 23), (0, 25)));
        app.on_input(b"\x1bp", &mut out, &mut child).unwrap();
        let open = app.open.as_ref().unwrap();
        assert_eq!(open.nested[0].words, "job");
        let id = open.nested[0].id;
        app.on_progress(id, Progress::Delta("A task.".into()), &mut out)
            .unwrap();
        app.on_progress(id, Progress::Done, &mut out).unwrap();
        app.on_input(b"\x1b", &mut out, &mut child).unwrap();
        // A double click on its marked words opens the saved answer and keeps
        // it open; a later click closes it.
        app.on_input(&[click(24), click(24)].concat(), &mut out, &mut child)
            .unwrap();
        assert_eq!(app.open.as_ref().unwrap().nested.len(), 1);
        app.on_input(&click(24), &mut out, &mut child).unwrap();
        assert!(app.open.as_ref().unwrap().nested.is_empty());
        assert!(child.is_empty());
    }

    #[test]
    fn mark_clicks_leave_drags_modifiers_links_and_window_focus_to_the_child() {
        let mut app = app_with_a_mark();
        let (mut child, mut out) = (Vec::new(), Vec::new());
        // A drag that starts on the mark: the child gets the press after all.
        app.on_input(b"\x1b[<0;24;2M", &mut out, &mut child)
            .unwrap();
        assert!(child.is_empty(), "held until it is a click or a drag");
        app.on_input(b"\x1b[<32;27;2M\x1b[<0;27;2m", &mut out, &mut child)
            .unwrap();
        assert_eq!(child, b"\x1b[<0;24;2M\x1b[<32;27;2M\x1b[<0;27;2m");
        assert!(app.open.is_none());
        child.clear();

        // Ctrl+click is the agent's (links).
        app.on_input(b"\x1b[<16;24;2M\x1b[<16;24;2m", &mut out, &mut child)
            .unwrap();
        assert!(app.open.is_none());
        assert_eq!(child, b"\x1b[<16;24;2M\x1b[<16;24;2m");
        child.clear();

        // The click that brings the window to the front.
        app.on_input(b"\x1b[I\x1b[<0;24;2M\x1b[<0;24;2m", &mut out, &mut child)
            .unwrap();
        assert!(app.open.is_none());
        assert_eq!(child, b"\x1b[I\x1b[<0;24;2M\x1b[<0;24;2m");
        child.clear();
        app.focus_in_at = None;

        // The agent made those words a link: a click is the agent's.
        app.on_output(
            b"\x1b]8;;https://example.com\x1b\\\x1b[2;20Htrue colour\x1b]8;;\x1b\\",
            &mut out,
            &mut Vec::new(),
        )
        .unwrap();
        app.on_input(b"\x1b[<0;24;2M\x1b[<0;24;2m", &mut out, &mut child)
            .unwrap();
        assert!(app.open.is_none());
        assert_eq!(child, b"\x1b[<0;24;2M\x1b[<0;24;2m");
    }

    /// Inside tmux, a child with the mouse off (Kiro inline, Claude classic)
    /// gets no clicks, but tmux hands peekme the plain ones: a click on a mark
    /// opens its saved answer, any other is dropped, and the child gets neither.
    #[test]
    fn marks_open_on_clicks_tmux_hands_over() {
        let mut app = app_with_a_mark();
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.on_output(b"\x1b[?1003l\x1b[?1000l\x1b[?1006l", &mut out, &mut child)
            .unwrap();
        app.in_tmux = true;
        app.on_input(b"\x1b[<0;5;6M\x1b[<0;5;6m", &mut out, &mut child)
            .unwrap();
        assert!(app.open.is_none());
        app.on_input(b"\x1b[<0;24;2M\x1b[<0;24;2m", &mut out, &mut child)
            .unwrap();
        assert_eq!(
            app.open.as_ref().expect("saved answer").peek.text,
            "24-bit RGB."
        );
        assert!(child.is_empty(), "{child:?}");
    }

    #[test]
    fn hovering_a_mark_underlines_it_solid_with_a_hand_pointer() {
        let mut app = app_with_a_mark();
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.on_input(b"\x1b[<35;24;2M", &mut out, &mut child)
            .unwrap();
        let shown = String::from_utf8_lossy(&out).to_string();
        assert!(shown.contains("\x1b]22;pointer\x1b\\"), "{shown:?}");
        assert!(!shown.contains("4:4"), "solid, not dotted");
        assert_eq!(child, b"\x1b[<35;24;2M", "the child still gets the motion");
        out.clear();
        app.on_input(b"\x1b[<35;5;6M", &mut out, &mut child)
            .unwrap();
        let shown = String::from_utf8_lossy(&out).to_string();
        assert!(shown.contains("4:4"), "dotted again");
        assert!(shown.contains("\x1b]22;\x1b\\"), "pointer given back");
    }

    #[test]
    fn output_held_while_open_is_replayed_correctly() {
        // Scroll-region inserts (how Codex adds history) arrive while the box is open.
        let later: &[u8] = b"\x1b[1;4r\x1b[2S\x1b[r\x1b[3;1Hnew history line\x1b[8;1H\x1b[48;2;30;30;30mcomposer\x1b[0m";
        let mut shadow = Shadow::new(40, 12);
        let mut real = Shadow::new(40, 12);
        shadow.advance(SCREEN);
        real.advance(SCREEN);
        let snap = shadow.snapshot();
        let lay = overlay::layout(12, 1, 1);
        let peek = PeekBox::new("x");
        real.advance(overlay::open_frame(&snap, &lay, &peek).as_bytes());
        shadow.advance(later); // what the child wrote meanwhile
        real.advance(overlay::close_frame(&snap, &lay).as_bytes());
        real.advance(later); // replay
        assert!(same_screen(&real.snapshot(), &shadow.snapshot()));
    }

    /// Real Codex output (startup, `/status`, an unknown command) captured at 120x40.
    const CODEX: &[u8] = include_bytes!("../tests/fixtures/codex_session_120x40.bin");

    #[test]
    fn round_trip_over_real_codex_output() {
        let mut base = Shadow::new(120, 40);
        base.advance(CODEX);
        base.flush_sync();
        let snap = base.snapshot();
        for first in (0..40).step_by(3) {
            let last = (first + 1).min(39);
            let mut real = Shadow::new(120, 40);
            real.advance(CODEX);
            real.flush_sync();
            let lay = overlay::layout(40, first, last);
            let mut peek = PeekBox::new("status");
            peek.text = "Streaming **explanation** text. ".repeat(12);
            real.advance(overlay::open_frame(&snap, &lay, &peek).as_bytes());
            real.advance(overlay::close_frame(&snap, &lay).as_bytes());
            let got = real.snapshot();
            if !same_screen(&got, &snap) {
                for (r, (ra, rb)) in got.rows.iter().zip(&snap.rows).enumerate() {
                    if let Some(c) = (0..ra.len()).find(|&c| {
                        let (x, y) = (&ra[c], &rb[c]);
                        x.c != y.c || x.fg != y.fg || x.bg != y.bg || x.flags != y.flags
                    }) {
                        eprintln!(
                            "row {r} col {c}: {:?} {:?} {:?} {:?} | want {:?} {:?} {:?} {:?}",
                            ra[c].c,
                            ra[c].fg,
                            ra[c].bg,
                            ra[c].flags,
                            rb[c].c,
                            rb[c].fg,
                            rb[c].bg,
                            rb[c].flags
                        );
                    }
                }
                eprintln!(
                    "cursor {:?} want {:?}; layout {:?}",
                    got.cursor, snap.cursor, lay
                );
            }
            assert!(
                same_screen(&got, &snap),
                "not restored for selection at row {first}"
            );
        }
    }

    #[test]
    fn real_codex_output_held_mid_stream() {
        // Open the box part-way through the capture; the rest arrives while it is open.
        for cut in [CODEX.len() / 3, CODEX.len() / 2, CODEX.len() * 4 / 5] {
            let mut shadow = Shadow::new(120, 40);
            let mut real = Shadow::new(120, 40);
            shadow.advance(&CODEX[..cut]);
            real.advance(&CODEX[..cut]);
            if shadow.in_sync_update() {
                continue; // peekme waits for the frame to finish before opening
            }
            let snap = shadow.snapshot();
            let lay = overlay::layout(40, 10, 11);
            real.advance(overlay::open_frame(&snap, &lay, &PeekBox::new("x")).as_bytes());
            shadow.advance(&CODEX[cut..]);
            real.advance(overlay::close_frame(&snap, &lay).as_bytes());
            real.advance(&render::strip_answered_queries(&CODEX[cut..]));
            shadow.flush_sync();
            real.flush_sync();
            assert!(
                same_screen(&real.snapshot(), &shadow.snapshot()),
                "diverged when cut at {cut}"
            );
        }
    }

    #[test]
    fn viewport_top_from_scroll_regions() {
        assert_eq!(viewport_top(b"x\x1b[1;24r\x1b[24S\x1b[r", 40), Some(24));
        assert_eq!(viewport_top(b"\x1b[1;24r..\x1b[1;16r", 40), Some(16));
        assert_eq!(viewport_top(b"\x1b[1;40r", 40), None);
        assert_eq!(viewport_top(b"\x1b[1;5H\x1b[r", 40), None);
    }

    fn test_app(cols: u16, rows: u16) -> App {
        test_app_for(Agent::Codex, cols, rows)
    }

    fn test_app_for(agent: Agent, cols: u16, rows: u16) -> App {
        let (tx, _rx) = mpsc::channel();
        App {
            agent: Some(agent),
            pty_pid: None,
            via_helper: false,
            osc52: Osc52::default(),
            app_selection: None,
            selection_source: "",
            last_release: None,
            mouse_press: None,
            recent_drag: None,
            pending_hotkey: None,
            tmux_baseline: None,
            known_from: 0,
            boundary: Boundary::default(),
            greek_typed: false,
            french_typed: false,
            told_hidden: false,
            can_offer_accessibility: false,
            offered_accessibility: false,
            accessibility_trusted: || false,
            accessibility_request: || false,
            next_message: None,
            accessibility_offer_open: false,
            in_tmux: false,
            over_ssh: false,
            mouse_seen: false,
            shadow: Shadow::new(cols, rows),
            open: None,
            pending: None,
            next_id: 0,
            consumed: Consumed::default(),
            carry: Vec::new(),
            in_paste: false,
            selection: SelectionSource::without_system(),
            explainer: None,
            tx,
            cwd: "/tmp".into(),
            viewport_top: None,
            degraded: false,
            size: (cols, rows),
            marks: Marks::default(),
            orphans: Default::default(),
            own_mouse_off: None,
            marks_stale: false,
            marks_all: false,
            held_press: None,
            swallow_release: false,
            last_mark_click: None,
            focus_in_at: None,
            clicked: None,
            ask_next: false,
            hover: None,
            pointer_on: false,
            child_pointer: None,
            // The fixtures are Codex 0.161: 3 rows a turn of the wheel.
            wheel: codex::scroll::Wheel::new(Some(3.0)),
        }
    }

    /// Drive the real App code over real Codex output: open a box with the live
    /// strip, let Codex draw and type under it, then close with Enter. The
    /// emulated terminal must end up identical to the child's own screen.
    #[test]
    fn live_strip_round_trip() {
        let mut app = test_app(120, 40);
        app.selection = SelectionSource::fixed("Collaboration mode");
        let mut real = Shadow::new(120, 40);
        let mut child: Vec<u8> = Vec::new();
        let mut out: Vec<u8> = Vec::new();

        app.on_output(CODEX, &mut out, &mut child).unwrap();
        real.advance(&std::mem::take(&mut out));
        let k = app.viewport_top.expect("viewport found in Codex output");

        app.open_peek(&mut out).unwrap();
        real.advance(&std::mem::take(&mut out));
        assert_eq!(app.open.as_ref().unwrap().strip_top, Some(k));

        // Codex echoes typing in its input line and pushes a history line up.
        let later = format!(
            "\x1b[?2026h\x1b[{};3Hhello\x1b[?2026l\x1b[1;{k}r\x1b[1S\x1b[r\x1b[{k};1Hpushed up",
            k + 2
        );
        app.on_input(b"hello", &mut out, &mut child).unwrap();
        assert!(app.open.is_some(), "typing must not close the box");
        assert_eq!(child, b"hello");
        app.on_output(later.as_bytes(), &mut out, &mut child)
            .unwrap();
        app.shadow.flush_sync();
        app.tick(&mut out).unwrap();
        real.advance(&std::mem::take(&mut out));
        let now = app.shadow.snapshot();
        let shown = real.snapshot();
        for r in k..40 {
            assert!(
                same_cells(&shown.rows[r], &now.rows[r]),
                "live row {r} not updated"
            );
        }

        app.on_input(b"\r", &mut out, &mut child).unwrap();
        assert!(app.open.is_none(), "Enter closes the box");
        real.advance(&std::mem::take(&mut out));
        real.flush_sync();
        assert!(same_screen(&real.snapshot(), &app.shadow.snapshot()));
    }

    /// Codex 0.157 draws full screen, captures the mouse and highlights its own
    /// selection in reverse video. Capture: a resumed session with "83, 6, 9"
    /// selected by a mouse drag.
    const CODEX_FULLSCREEN: &[u8] =
        include_bytes!("../tests/fixtures/codex_0157_fullscreen_selection_120x40.bin");

    /// Codex 0.161, the startup screen with "OpenAI Codex" in its header
    /// selected by a mouse drag. No session, no personal data on screen.
    const CODEX_0161_FULLSCREEN: &[u8] =
        include_bytes!("../tests/fixtures/codex_0161_fullscreen_selection_120x40.bin");

    #[test]
    fn fullscreen_codex_selection_and_live_area() {
        codex_selection_and_live_area(CODEX_FULLSCREEN, "83, 6, 9");
    }

    #[test]
    fn fullscreen_codex_0161_selection_and_live_area() {
        codex_selection_and_live_area(CODEX_0161_FULLSCREEN, "OpenAI Codex");
    }

    /// The wheel over an agent that leaves the mouse alone: peekme gives
    /// the mouse back, so the terminal scrolls its own history; the box stays,
    /// and reports still on their way never reach the agent.
    #[test]
    fn the_wheel_gives_the_mouse_back_to_the_terminal() {
        let mut app = test_app(40, 12);
        app.selection = SelectionSource::fixed("true colour");
        let (mut out, mut child) = (Vec::new(), Vec::new());
        app.on_output(SCREEN, &mut out, &mut child).unwrap();
        app.open_peek(&mut out).unwrap();
        assert!(app.open.as_ref().unwrap().own_mouse);
        out.clear();
        app.on_input(b"\x1b[<65;5;10M", &mut out, &mut child)
            .unwrap();
        assert!(String::from_utf8_lossy(&out).contains(MOUSE_OFF));
        let open = app.open.as_ref().expect("still open");
        assert!(!open.own_mouse);
        app.on_input(
            b"\x1b[<65;5;10M\x1b[<0;5;10M\x1b[<0;5;10m",
            &mut out,
            &mut child,
        )
        .unwrap();
        assert!(child.is_empty());
        assert!(app.open.is_some());
    }

    /// The wheel with a box open: Codex scrolls its chat, and the box follows
    /// the selection to its new row in the same frame, still open.
    #[test]
    fn the_box_follows_its_selection_when_codex_scrolls() {
        let mut app = test_app(120, 40);
        let mut real = Shadow::new(120, 40);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.on_output(CODEX_0161_FULLSCREEN, &mut out, &mut child)
            .unwrap();
        app.shadow.flush_sync();
        real.advance(&std::mem::take(&mut out));
        real.flush_sync();
        app.open_peek(&mut out).unwrap();
        real.advance(&std::mem::take(&mut out));
        let open = app.open.as_ref().unwrap();
        let (top, above) = (open.strip_top.unwrap(), (open.lay.box_top as usize));
        let sel_row = above - 1;
        let sel_text = app.shadow.snapshot().rows_text(sel_row, sel_row + 1);

        app.on_input(b"\x1b[<65;40;30M", &mut out, &mut child)
            .unwrap();
        assert_eq!(child, b"\x1b[<65;40;30M", "the wheel reaches Codex");
        // Codex shows older chat: its rows above the input area move down two.
        let scroll = format!("\x1b[?2026h\x1b[1;{top}r\x1b[2T\x1b[r\x1b[?2026l");
        app.on_output(scroll.as_bytes(), &mut out, &mut child)
            .unwrap();
        app.shadow.flush_sync();
        app.tick(&mut out).unwrap();
        // peekme draws the result itself, row by row in place: Codex's own
        // scroll never reaches the terminal, and no row is blanked first.
        let frame = String::from_utf8_lossy(&out).to_string();
        assert!(
            !frame.contains("\x1b[2T") && !frame.contains("\x1b[2K"),
            "{frame:?}"
        );
        real.advance(&std::mem::take(&mut out));
        real.flush_sync();
        let open = app.open.as_ref().expect("still open");
        assert_eq!(
            (open.lay.box_top as usize),
            above + 2,
            "moved with its selection"
        );
        let shown = real.snapshot();
        assert_eq!(shown.rows_text(sel_row + 2, sel_row + 3), sel_text);
        let border = shown.rows_text(above + 2, above + 3);
        assert!(
            border.starts_with("╭─ peek"),
            "the box right under it: {border}"
        );
        // Closing leaves exactly Codex's screen, drawn in place too.
        app.close(&mut out, false).unwrap();
        assert!(!String::from_utf8_lossy(&out).contains("\x1b[2T"));
        real.advance(&std::mem::take(&mut out));
        let now = app.shadow.snapshot();
        let shown = real.snapshot();
        for r in 0..40 {
            assert_eq!(
                shown.rows_text(r, r + 1),
                now.rows_text(r, r + 1),
                "row {r}"
            );
        }
    }

    /// At the end of the chat Codex does not move for the wheel: peekme
    /// scrolls the rows the box pushed past the bottom into view itself.
    #[test]
    fn the_wheel_reaches_the_end_of_the_chat_past_the_box() {
        let mut app = test_app(120, 40);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.on_output(CODEX_0161_FULLSCREEN, &mut out, &mut child)
            .unwrap();
        let chat: String = (0..34)
            .map(|r| format!("\x1b[{};1H\x1b[0m\x1b[2K  chat line {r}", r + 1))
            .collect();
        app.on_output(format!("\x1b7{chat}\x1b8").as_bytes(), &mut out, &mut child)
            .unwrap();
        app.shadow.flush_sync();
        app.selection = SelectionSource::fixed("chat line 5");
        app.open_peek(&mut out).unwrap();
        let open = app.open.as_ref().unwrap();
        let (id, home) = (open.id, open.lay.home);
        let answer: String = (1..=5).map(|k| format!("Line {k}.\n\n")).collect();
        app.on_progress(id, Progress::Delta(answer), &mut out)
            .unwrap();
        app.on_progress(id, Progress::Done, &mut out).unwrap();
        let open = app.open.as_ref().unwrap();
        assert!(open.lay.below && open.lay.box_top == home, "{:?}", open.lay);
        let h = open.lay.box_height as isize;
        // Down at the end: Codex sends nothing back.
        for turn in 1..=5 {
            app.on_input(b"\x1b[<65;60;5M", &mut Vec::new(), &mut child)
                .unwrap();
            // The first turn waits; once the end is known the rest do not.
            let open = app.open.as_mut().unwrap();
            if !open.done() {
                open.sent -= WHEEL_UNMOVED;
            }
            app.tick(&mut out).unwrap();
            let lay = app.open.as_ref().unwrap().lay;
            assert_eq!(lay.box_top, home - (3 * turn).min(h), "turn {turn}");
        }
        let lay = app.open.as_ref().unwrap().lay;
        assert_eq!(
            lay.src_row(33),
            Some(33),
            "the chat's last row at its place"
        );
    }

    /// A box over the middle of Codex's chat, with an answer of 8 lines;
    /// returns the app, the box's place and the first row under the chat.
    fn box_over_codex_chat() -> (App, isize, isize, usize) {
        let mut app = test_app(120, 40);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.on_output(CODEX_0161_FULLSCREEN, &mut out, &mut child)
            .unwrap();
        let chat: String = (0..34)
            .map(|r| format!("\x1b[{};1H\x1b[0m\x1b[2K  chat line {r}", r + 1))
            .collect();
        app.on_output(format!("\x1b7{chat}\x1b8").as_bytes(), &mut out, &mut child)
            .unwrap();
        app.shadow.flush_sync();
        app.selection = SelectionSource::fixed("chat line 5");
        app.open_peek(&mut out).unwrap();
        let id = app.open.as_ref().unwrap().id;
        let answer: String = (1..=8).map(|k| format!("Line {k}.\n\n")).collect();
        app.on_progress(id, Progress::Delta(answer), &mut out)
            .unwrap();
        app.on_progress(id, Progress::Done, &mut out).unwrap();
        let open = app.open.as_ref().unwrap();
        let (home, top, end) = (open.lay.home, open.lay.box_top, open.strip_top.unwrap());
        (app, home, top, end)
    }

    /// Codex scrolls a moment after its first, empty answer to a turn. Late
    /// as that may come (a slower machine), the turn is not the end of the
    /// chat: the box moves with the chat, 3 rows a turn, no more.
    #[test]
    fn a_late_scroll_is_not_the_end_of_the_chat() {
        let (mut app, home, top, end) = box_over_codex_chat();
        let (mut child, mut out) = (Vec::new(), Vec::new());
        let nothing = b"\x1b[?2026h\x1b[39m\x1b[49m\x1b[0m\x1b[?2026l";
        let up3 = format!("\x1b[?2026h\x1b[1;{end}r\x1b[3S\x1b[r\x1b[?2026l");
        for turn in 1..=3 {
            app.on_input(b"\x1b[<65;60;5M", &mut out, &mut child)
                .unwrap();
            app.on_output(nothing, &mut out, &mut child).unwrap();
            std::thread::sleep(Duration::from_millis(60));
            app.tick(&mut out).unwrap();
            app.on_output(up3.as_bytes(), &mut out, &mut child).unwrap();
            app.tick(&mut out).unwrap();
            let lay = app.open.as_ref().unwrap().lay;
            assert_eq!(
                (lay.home, lay.box_top),
                (home - 3 * turn, top - 3 * turn),
                "turn {turn}"
            );
        }
    }

    /// Mid-chat, a turn that moves the chat fewer rows than usual (another
    /// step size) is not the end either: the box moves only as the chat does.
    #[test]
    fn a_short_turn_in_the_middle_is_not_the_end() {
        let (mut app, home, top, end) = box_over_codex_chat();
        let (mut child, mut out) = (Vec::new(), Vec::new());
        let nothing = b"\x1b[?2026h\x1b[39m\x1b[49m\x1b[0m\x1b[?2026l";
        let up1 = format!("\x1b[?2026h\x1b[1;{end}r\x1b[1S\x1b[r\x1b[?2026l");
        for turn in 1..=4 {
            app.on_input(b"\x1b[<65;60;5M", &mut out, &mut child)
                .unwrap();
            app.on_output(nothing, &mut out, &mut child).unwrap();
            app.on_output(up1.as_bytes(), &mut out, &mut child).unwrap();
            app.tick(&mut out).unwrap();
            let lay = app.open.as_ref().unwrap().lay;
            assert_eq!(
                (lay.home, lay.box_top),
                (home - turn, top - turn),
                "turn {turn}"
            );
        }
    }

    /// Codex 0.162 pins the prompt of the answer on the chat's top row: a
    /// turn that brings the header moves no chat row (the header covers the
    /// row that would have come), one that takes it away moves two. Neither
    /// is the end of the chat: the box moves just as the rows do, and the
    /// header is never taken for a chat row.
    #[test]
    fn codex_prompt_header_coming_and_going_is_not_the_end() {
        let (mut app, home, top, end) = box_over_codex_chat();
        app.wheel = codex::scroll::Wheel::new(Some(1.0));
        let (mut child, mut out) = (Vec::new(), Vec::new());
        let nothing = b"\x1b[?2026h\x1b[39m\x1b[49m\x1b[0m\x1b[?2026l";
        let header = "\x1b[?2026h\x1b[1;1H\x1b[0m\x1b[2K› q1 the prompt\x1b[?2026l".to_string();
        let under = format!("\x1b[?2026h\x1b[2;{end}r\x1b[1S\x1b[r\x1b[?2026l");
        let gone = format!(
            "\x1b[?2026h\x1b[1;{end}r\x1b[2S\x1b[r\x1b[1;1H\x1b[0m\x1b[2K  chat line 3\x1b[?2026l"
        );
        let mut turn = |answer: &str| {
            app.on_input(b"\x1b[<65;60;5M", &mut out, &mut child)
                .unwrap();
            app.on_output(nothing, &mut out, &mut child).unwrap();
            app.on_output(answer.as_bytes(), &mut out, &mut child)
                .unwrap();
            std::thread::sleep(WHEEL_QUIET + Duration::from_millis(30));
            app.tick(&mut out).unwrap();
            let open = app.open.as_ref().unwrap();
            (open.lay.home, open.lay.box_top, open.header)
        };
        assert_eq!(turn(&header), (home, top, true), "the header came");
        assert_eq!(turn(&under), (home - 1, top - 1, true), "a row under it");
        assert_eq!(turn(&gone), (home - 3, top - 3, false), "the header went");
        let open = app.open.as_ref().unwrap();
        assert!(
            !open.mem.values().any(|r| r.contains("q1 the prompt")),
            "the header is not a chat row"
        );
        assert_eq!(open.revealed, 0, "no box rows for an end");
    }

    /// Codex 0.162 moves its chat `tui.mouse_scroll_speed` rows a turn,
    /// counting fractions: at 0.5 every other turn moves nothing, and Codex
    /// answers that turn as it does at the end of the chat. peekme keeps the
    /// same count: such a turn is not the end. At the end, each turn shows
    /// as many of the box's rows as it would have moved the chat.
    #[test]
    fn a_slow_wheel_turn_that_moves_nothing_is_not_the_end() {
        for speed in [0.5, 1.0] {
            let (mut app, home, top, end) = box_over_codex_chat();
            app.wheel = codex::scroll::Wheel::new(Some(speed));
            let (mut child, mut out) = (Vec::new(), Vec::new());
            let nothing = b"\x1b[?2026h\x1b[39m\x1b[49m\x1b[0m\x1b[?2026l";
            let up1 = format!("\x1b[?2026h\x1b[1;{end}r\x1b[1S\x1b[r\x1b[?2026l");
            let mut model = codex::scroll::Wheel::new(Some(speed));
            let mut moved = 0;
            for turn in 1..=6 {
                app.on_input(b"\x1b[<65;60;5M", &mut out, &mut child)
                    .unwrap();
                app.on_output(nothing, &mut out, &mut child).unwrap();
                let rows = model.turn(1);
                let answer = if rows == 1 { up1.as_bytes() } else { nothing };
                app.on_output(answer, &mut out, &mut child).unwrap();
                moved += rows;
                let lay = app.open.as_ref().unwrap().lay;
                assert_eq!(
                    (lay.home, lay.box_top),
                    (home - moved, top - moved),
                    "speed {speed}, turn {turn}: only as Codex moved"
                );
            }
            // The end: Codex moves no more; the box's rows come, at the same pace.
            let lift = |app: &App| {
                let lay = app.open.as_ref().unwrap().lay;
                lay.home - lay.box_top
            };
            let start = lift(&app);
            let mut want = start;
            for turn in 1..=4 {
                app.on_input(b"\x1b[<65;60;5M", &mut out, &mut child)
                    .unwrap();
                app.on_output(nothing, &mut out, &mut child).unwrap();
                app.on_output(nothing, &mut out, &mut child).unwrap();
                want += model.turn(1);
                assert_eq!(lift(&app), want, "speed {speed}, end turn {turn}");
            }
            assert!(lift(&app) > start, "speed {speed}: the box's rows came");
        }
    }

    /// Codex answers each turn in order with an update that changes nothing
    /// and then the scroll. Other updates (its status, a redraw) between
    /// turns do not count, so a quick run of turns mid-chat moves the box
    /// with the chat only, however the updates come in.
    #[test]
    fn codex_answers_are_read_in_order() {
        let (mut app, home, top, end) = box_over_codex_chat();
        app.wheel = codex::scroll::Wheel::new(Some(1.0));
        let (mut child, mut out) = (Vec::new(), Vec::new());
        let nothing = "\x1b[?2026h\x1b[39m\x1b[49m\x1b[0m\x1b[?2026l";
        let up1 = format!("\x1b[?2026h\x1b[1;{end}r\x1b[1S\x1b[r\x1b[?2026l");
        let status = format!(
            "\x1b[?2026h\x1b7\x1b[{};1HCopy sent\x1b8\x1b[?2026l",
            end + 1
        );
        app.on_input(&b"\x1b[<65;60;5M".repeat(4), &mut out, &mut child)
            .unwrap();
        // All four turns' answers in one read, a status update first, the
        // last one cut in two.
        let all = format!("{status}{nothing}{up1}{nothing}{up1}{nothing}{up1}{nothing}{up1}");
        let (a, b) = all.split_at(all.len() - 9);
        app.on_output(a.as_bytes(), &mut out, &mut child).unwrap();
        app.on_output(b.as_bytes(), &mut out, &mut child).unwrap();
        let open = app.open.as_ref().unwrap();
        assert_eq!((open.lay.home, open.lay.box_top), (home - 4, top - 4));
        assert!(open.done() && open.end.is_none());
        // A slow wheel (0.5): turns that move nothing get two updates that
        // change nothing, with the selection's highlight drawn again in
        // between; that is not the end of the chat.
        app.wheel = codex::scroll::Wheel::new(Some(0.5));
        let mut model = codex::scroll::Wheel::new(Some(0.5));
        let highlight = "\x1b[?2026h\x1b7\x1b[12;3H\x1b[7mchat\x1b[0m\x1b8\x1b[?2026l";
        let mut moved = 4;
        for _ in 0..6 {
            app.on_input(b"\x1b[<65;60;5M", &mut out, &mut child)
                .unwrap();
            let second = if model.turn(1) == 1 {
                up1.as_str()
            } else {
                nothing
            };
            moved += isize::from(second != nothing);
            let answer = format!("{nothing}{highlight}{second}");
            app.on_output(answer.as_bytes(), &mut out, &mut child)
                .unwrap();
            let open = app.open.as_ref().unwrap();
            assert_eq!(
                (open.lay.home, open.lay.box_top),
                (home - moved, top - moved)
            );
            assert!(open.end.is_none(), "not the end");
        }
    }

    /// Codex sometimes draws two turns' rows in one update and then answers
    /// the second turn with two updates that change nothing, as seen with a
    /// slow wheel (0.5, rows 0 1 0 1). Read turn by turn that looks like the
    /// end of the chat; counted as a whole the rows add up, and the box only
    /// moves as the chat does.
    #[test]
    fn two_turns_drawn_at_once_are_not_the_end() {
        let (mut app, home, top, end) = box_over_codex_chat();
        app.wheel = codex::scroll::Wheel::new(Some(0.5));
        let (mut child, mut out) = (Vec::new(), Vec::new());
        let n = "\x1b[?2026h\x1b[39m\x1b[49m\x1b[0m\x1b[?2026l";
        let up1 = format!("\x1b[?2026h\x1b[1;{end}r\x1b[1S\x1b[r\x1b[?2026l");
        let turn = b"\x1b[<65;60;5M";
        // Turns 1 and 2: 0 rows, then 1 row.
        app.on_input(&turn.repeat(2), &mut out, &mut child).unwrap();
        app.on_output(format!("{n}{n}{n}{up1}").as_bytes(), &mut out, &mut child)
            .unwrap();
        // Turns 3 and 4 (0 and 1): the row comes with turn 3's answer.
        app.on_input(&turn.repeat(2), &mut out, &mut child).unwrap();
        app.on_output(format!("{n}{up1}").as_bytes(), &mut out, &mut child)
            .unwrap();
        app.on_output(format!("{n}{n}").as_bytes(), &mut out, &mut child)
            .unwrap();
        let open = app.open.as_ref().unwrap();
        assert_eq!((open.lay.home, open.lay.box_top), (home - 2, top - 2));
        assert!(open.end.is_none(), "not the end");
    }

    /// Turns read at once Codex takes together, answering them with fewer
    /// updates than two a turn. At the end of the chat: once Codex went quiet
    /// after answering, the box's rows come for all of them.
    #[test]
    fn turns_taken_together_at_the_end() {
        let (mut app, home, top, _) = box_over_codex_chat();
        app.wheel = codex::scroll::Wheel::new(Some(0.5));
        let (mut child, mut out) = (Vec::new(), Vec::new());
        let n = "\x1b[?2026h\x1b[39m\x1b[49m\x1b[0m\x1b[?2026l";
        app.on_input(&b"\x1b[<65;60;5M".repeat(5), &mut out, &mut child)
            .unwrap();
        app.on_output(n.repeat(6).as_bytes(), &mut out, &mut child)
            .unwrap();
        app.tick(&mut out).unwrap();
        // Counted, three turns moved nothing: the row of the second one
        // (the third may still come).
        let lay = app.open.as_ref().unwrap().lay;
        assert_eq!(lay.box_top, top - 1, "by count");
        app.open.as_mut().unwrap().answered_at -= WHEEL_QUIET;
        app.tick(&mut out).unwrap();
        let open = app.open.as_ref().unwrap();
        // 0 1 0 1 0 rows at 0.5 a turn.
        assert_eq!((open.lay.home, open.lay.box_top), (home, top - 2));
        assert_eq!(open.end, Some(1));
    }

    /// Codex answers a wheel turn at the end of its chat with frames that
    /// change nothing: peekme shows the hidden rows soon after, without the
    /// long wait, also for the turns of a quick burst that Codex moved for
    /// only in part.
    #[test]
    fn codex_answering_the_wheel_at_its_end_shows_the_rows_at_once() {
        let mut app = test_app(120, 40);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.on_output(CODEX_0161_FULLSCREEN, &mut out, &mut child)
            .unwrap();
        let chat: String = (0..34)
            .map(|r| format!("\x1b[{};1H\x1b[0m\x1b[2K  chat line {r}", r + 1))
            .collect();
        app.on_output(format!("\x1b7{chat}\x1b8").as_bytes(), &mut out, &mut child)
            .unwrap();
        app.shadow.flush_sync();
        app.selection = SelectionSource::fixed("chat line 5");
        app.open_peek(&mut out).unwrap();
        let open = app.open.as_ref().unwrap();
        let (id, home, top) = (open.id, open.lay.home, open.strip_top.unwrap());
        let answer: String = (1..=8).map(|k| format!("Line {k}.\n\n")).collect();
        app.on_progress(id, Progress::Delta(answer), &mut out)
            .unwrap();
        app.on_progress(id, Progress::Done, &mut out).unwrap();
        let h = app.open.as_ref().unwrap().lay.box_height as isize;
        assert!(h >= 9, "{h}");
        // Codex answers every turn with a frame that changes nothing, then
        // a second one: the same again at the end of its chat.
        let nothing = b"\x1b[?2026h\x1b[39m\x1b[49m\x1b[0m\x1b[?2026l";
        app.on_input(b"\x1b[<65;60;5M", &mut out, &mut child)
            .unwrap();
        app.on_output(nothing, &mut out, &mut child).unwrap();
        assert_eq!(app.open.as_ref().unwrap().lay.box_top, home, "not yet");
        app.on_output(nothing, &mut out, &mut child).unwrap();
        let open = app.open.as_ref().unwrap();
        assert_eq!(open.lay.box_top, home - 3, "one turn, on the second answer");
        assert!(open.done());
        // Known to be at the end: the next turn shows the rows at once.
        app.on_input(b"\x1b[<65;60;5M", &mut out, &mut child)
            .unwrap();
        assert_eq!(app.open.as_ref().unwrap().lay.box_top, home - 6, "at once");
        app.on_output(nothing, &mut out, &mut child).unwrap();
        app.on_output(nothing, &mut out, &mut child).unwrap();
        assert_eq!(
            app.open.as_ref().unwrap().lay.box_top,
            home - 6,
            "only once"
        );
        // Back up a turn: Codex scrolls again, the end is behind.
        let down = format!("\x1b[?2026h\x1b[1;{top}r\x1b[3T\x1b[r\x1b[?2026l");
        app.on_input(b"\x1b[<64;60;5M", &mut out, &mut child)
            .unwrap();
        app.on_output(nothing, &mut out, &mut child).unwrap();
        app.on_output(down.as_bytes(), &mut out, &mut child)
            .unwrap();
        let open = app.open.as_ref().unwrap();
        assert_eq!((open.lay.home, open.lay.box_top), (home + 3, home - 3));
        let up = format!("\x1b[?2026h\x1b[1;{top}r\x1b[3S\x1b[r\x1b[?2026l");
        app.on_input(b"\x1b[<65;60;5M", &mut out, &mut child)
            .unwrap();
        app.on_output(nothing, &mut out, &mut child).unwrap();
        app.on_output(up.as_bytes(), &mut out, &mut child).unwrap();
        let (home, lift) = (home, 6);
        assert_eq!(app.open.as_ref().unwrap().lay.box_top, home - lift);
        // Two turns at once: Codex moves for the first (a row was still
        // below), then has nothing left for the second.
        app.on_input(b"\x1b[<65;60;5M\x1b[<65;60;5M", &mut out, &mut child)
            .unwrap();
        app.on_output(nothing, &mut out, &mut child).unwrap();
        let up = format!("\x1b[?2026h\x1b[1;{top}r\x1b[1S\x1b[r\x1b[?2026l");
        app.on_output(up.as_bytes(), &mut out, &mut child).unwrap();
        let open = app.open.as_ref().unwrap();
        assert_eq!(open.lay.home, home - 1, "the selection moved a row");
        assert_eq!(open.lay.box_top, home - 1 - lift, "and only that, for now");
        assert!(!open.done(), "the second turn is on its way");
        app.on_output(nothing, &mut out, &mut child).unwrap();
        app.on_output(nothing, &mut out, &mut child).unwrap();
        let open = app.open.as_ref().unwrap();
        // The end showed: the box's rows make up the two rows the first turn
        // was short, and the second turn.
        assert_eq!(
            open.lay.box_top,
            home - 1 - lift - 2 - 3,
            "the second turn too"
        );
        assert!(open.done());
    }

    /// Codex starts working while the box is open: its status line, taller
    /// than the idle one, is not chat. The chat ends above it and the status
    /// shows as Codex draws it, also once the chat scrolls.
    #[test]
    fn codex_working_ends_the_chat_above_its_status() {
        let mut app = test_app(120, 40);
        let mut real = Shadow::new(120, 40);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.on_output(CODEX_0161_FULLSCREEN, &mut out, &mut child)
            .unwrap();
        let chat: String = (0..34)
            .map(|r| format!("\x1b[{};1H\x1b[0m\x1b[2K  chat line {r}", r + 1))
            .collect();
        app.on_output(format!("\x1b7{chat}\x1b8").as_bytes(), &mut out, &mut child)
            .unwrap();
        app.shadow.flush_sync();
        real.advance(&std::mem::take(&mut out));
        app.selection = SelectionSource::fixed("chat line 10");
        app.open_peek(&mut out).unwrap();
        real.advance(&std::mem::take(&mut out));
        assert_eq!(app.open.as_ref().unwrap().lay.region.1, 34);
        // Working: the chat moves up three rows to make room for the status.
        let busy: String = (0..31)
            .map(|r| format!("\x1b[{};1H\x1b[0m\x1b[2K  chat line {}", r + 1, r + 3))
            .collect::<String>()
            + "\x1b[32;1H\x1b[2K\x1b[33;1H\x1b[2K• Working (1s • esc to interrupt)\x1b[34;1H\x1b[2K";
        app.on_output(
            format!("\x1b[?2026h\x1b7{busy}\x1b8\x1b[?2026l").as_bytes(),
            &mut out,
            &mut child,
        )
        .unwrap();
        app.tick(&mut out).unwrap();
        real.advance(&std::mem::take(&mut out));
        let open = app.open.as_ref().unwrap();
        assert_eq!(open.lay.region.1, 31, "the chat ends above the status");
        assert_eq!(open.strip_top, Some(31));
        let shown = real.snapshot();
        assert!(
            shown.rows_text(32, 33).contains("Working"),
            "{:?}",
            shown.rows_text(28, 36)
        );
        let sel = (0..31).find(|&r| shown.rows_text(r, r + 1).contains("chat line 10"));
        let border = sel.map(|r| shown.rows_text(r + 1, r + 2));
        assert!(
            border.as_ref().is_some_and(|b| b.starts_with("╭")),
            "the box under its line: {border:?}"
        );
    }

    /// PageDown with the box showing all its text pages Codex's chat: as
    /// turns of the wheel, so the box follows as it does the wheel. Ctrl+End
    /// takes the chat far from the selection: the box closes first.
    #[test]
    fn paging_keys_scroll_the_chat_under_the_box() {
        let mut app = test_app(120, 40);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.on_output(CODEX_0161_FULLSCREEN, &mut out, &mut child)
            .unwrap();
        app.shadow.flush_sync();
        app.open_peek(&mut out).unwrap();
        let open = app.open.as_ref().unwrap();
        let (lo, hi) = open.lay.region;
        child.clear();
        app.on_input(b"\x1b[6~", &mut out, &mut child).unwrap();
        let turns = (hi - lo - 1 + 1) / 3;
        // Over the middle of the chat, not its top row (Codex's prompt
        // header, which does not scroll for the wheel).
        assert_eq!(
            child,
            format!("\x1b[<65;1;{}M", (lo + hi) / 2 + 1)
                .repeat(turns)
                .into_bytes(),
            "a page of wheel turns"
        );
        let open = app.open.as_ref().expect("still open");
        assert_eq!(open.run.len(), turns);
        child.clear();
        app.on_input(b"\x1b[1;5F", &mut out, &mut child).unwrap();
        assert!(app.open.is_none(), "Ctrl+End closes the box");
        assert_eq!(child, b"\x1b[1;5F");
    }

    /// A trackpad sends the wheel to the side along with up and down. Codex
    /// does not scroll for it, and that is not the end of the chat: the box
    /// stays with its text.
    #[test]
    fn the_wheel_to_the_side_does_not_move_the_box() {
        let mut app = test_app(120, 40);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.on_output(CODEX_0161_FULLSCREEN, &mut out, &mut child)
            .unwrap();
        let chat: String = (0..34)
            .map(|r| format!("\x1b[{};1H\x1b[0m\x1b[2K  chat line {r}", r + 1))
            .collect();
        app.on_output(format!("\x1b7{chat}\x1b8").as_bytes(), &mut out, &mut child)
            .unwrap();
        app.shadow.flush_sync();
        app.selection = SelectionSource::fixed("chat line 5");
        app.open_peek(&mut out).unwrap();
        let id = app.open.as_ref().unwrap().id;
        let answer: String = (1..=8).map(|k| format!("Line {k}.\n\n")).collect();
        app.on_progress(id, Progress::Delta(answer), &mut out)
            .unwrap();
        app.on_progress(id, Progress::Done, &mut out).unwrap();
        let lay = app.open.as_ref().unwrap().lay;
        child.clear();
        app.on_input(
            b"\x1b[<67;60;5M\x1b[<67;60;5M\x1b[<66;60;5M\x1b[<67;60;5M",
            &mut out,
            &mut child,
        )
        .unwrap();
        let nothing = b"\x1b[?2026h\x1b[39m\x1b[49m\x1b[0m\x1b[?2026l";
        app.on_output(nothing, &mut out, &mut child).unwrap();
        let open = app.open.as_mut().unwrap();
        assert!(open.done());
        app.tick(&mut out).unwrap();
        assert_eq!(app.open.as_ref().unwrap().lay, lay);
    }

    /// A tall box above a selection near the bottom pushes the chat up; as
    /// the chat scrolls down under it, every chat row stays the chat's own:
    /// no blank band where the box was, and Codex's status line (which stays
    /// put) never shows up among the chat rows.
    #[test]
    fn scrolling_leaves_no_dead_zone_where_the_box_was() {
        let mut app = test_app(120, 40);
        let mut real = Shadow::new(120, 40);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.on_output(CODEX_0161_FULLSCREEN, &mut out, &mut child)
            .unwrap();
        let chat: String = (0..34)
            .map(|r| format!("\x1b[{};1H\x1b[0m\x1b[2K  chat line {r}", r + 1))
            .collect();
        let status = "\x1b[35;1H\x1b[2K  Copy sent to terminal";
        app.on_output(
            format!("\x1b7{chat}{status}\x1b8").as_bytes(),
            &mut out,
            &mut child,
        )
        .unwrap();
        app.shadow.flush_sync();
        real.advance(&std::mem::take(&mut out));
        real.flush_sync();
        app.selection = SelectionSource::fixed("chat line 30");
        app.open_peek(&mut out).unwrap();
        let open = app.open.as_ref().unwrap();
        let (id, top) = (open.id, open.strip_top.unwrap());
        assert_eq!(top, 34, "Codex's status rows are not chat");
        let answer: String = (1..=12).map(|k| format!("Line {k}.\n\n")).collect();
        app.on_progress(id, Progress::Delta(answer), &mut out)
            .unwrap();
        app.on_progress(id, Progress::Done, &mut out).unwrap();
        real.advance(&std::mem::take(&mut out));
        assert!(!app.open.as_ref().unwrap().lay.below, "above its selection");
        let mut older = 0;
        for step in 0..14 {
            app.on_input(b"\x1b[<64;60;5M", &mut Vec::new(), &mut Vec::new())
                .unwrap();
            older += 3;
            let lines: String = (0..3)
                .map(|i| format!("\x1b[{};1H  chat line -{}", i + 1, older - i))
                .collect();
            // Codex scrolls its chat rows only; the status line stays put.
            let bytes = format!("\x1b[?2026h\x1b[1;34r\x1b[3T\x1b[r{lines}\x1b[?2026l");
            app.on_output(bytes.as_bytes(), &mut out, &mut Vec::new())
                .unwrap();
            app.shadow.flush_sync();
            app.tick(&mut out).unwrap();
            real.advance(&std::mem::take(&mut out));
            real.flush_sync();
            let shown = real.snapshot();
            let mut seen: Vec<i64> = Vec::new();
            for r in 0..34 {
                let t = shown.rows_text(r, r + 1);
                let t = t.trim();
                if t.starts_with(['│', '╭', '╰']) {
                    continue;
                }
                assert!(
                    !t.contains("Copy sent"),
                    "step {step} row {r}: status in the chat"
                );
                let n = t.strip_prefix("chat line ").and_then(|n| n.parse().ok());
                let n = n.unwrap_or_else(|| panic!("step {step} row {r}: {t:?} is not a chat row"));
                seen.push(n);
            }
            let mut want: Vec<i64> = seen.clone();
            want.sort();
            want.dedup();
            assert_eq!(seen, want, "step {step}: in order, none twice");
            // The box hides some rows while it is on the screen, no more.
            let gap = seen.windows(2).filter(|w| w[1] != w[0] + 1).count();
            assert!(gap <= 1, "step {step}: {seen:?}");
        }
        app.close(&mut out, false).unwrap();
        real.advance(&std::mem::take(&mut out));
        let (now, shown) = (app.shadow.snapshot(), real.snapshot());
        for r in 0..40 {
            assert_eq!(
                shown.rows_text(r, r + 1),
                now.rows_text(r, r + 1),
                "row {r}"
            );
        }
    }

    /// The chat scrolls the selection off the top: the box goes with it,
    /// cut at the edge of the screen, and comes back whole when the chat
    /// scrolls back. Each step writes only the rows that changed.
    #[test]
    fn the_box_scrolls_off_the_screen_with_its_selection() {
        let mut app = test_app(120, 40);
        let mut real = Shadow::new(120, 40);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.on_output(CODEX_0161_FULLSCREEN, &mut out, &mut child)
            .unwrap();
        // A chat under the banner, as in a session that has been going a while.
        let chat: String = (4..34)
            .map(|r| format!("\x1b[{};3Hchat line {r}", r + 1))
            .collect();
        app.on_output(format!("\x1b7{chat}\x1b8").as_bytes(), &mut out, &mut child)
            .unwrap();
        app.shadow.flush_sync();
        real.advance(&std::mem::take(&mut out));
        real.flush_sync();
        app.open_peek(&mut out).unwrap();
        real.advance(&std::mem::take(&mut out));
        let open = app.open.as_ref().unwrap();
        let (top, above) = (open.strip_top.unwrap(), open.lay.box_top);
        let mut step = |app: &mut App, scroll: &str| {
            app.on_input(b"\x1b[<65;40;30M", &mut Vec::new(), &mut Vec::new())
                .unwrap();
            let bytes = format!("\x1b[?2026h\x1b[1;{top}r{scroll}\x1b[r\x1b[?2026l");
            let mut out = Vec::new();
            app.on_output(bytes.as_bytes(), &mut out, &mut Vec::new())
                .unwrap();
            app.shadow.flush_sync();
            app.tick(&mut out).unwrap();
            real.advance(&out);
            real.flush_sync();
            (
                real.snapshot(),
                String::from_utf8_lossy(&out).matches(";1H").count(),
            )
        };
        // Up 3: the selection row and the box top border are above the screen.
        let (shown, _) = step(&mut app, "\x1b[3S");
        let open = app.open.as_ref().unwrap();
        assert_eq!(open.lay.box_top, above - 3);
        let body = shown.rows_text(0, 1);
        assert!(body.starts_with("│ "), "the box, cut at the top: {body}");
        // Down 3 again: whole, back where it was.
        let (shown, rows) = step(&mut app, "\x1b[3T");
        assert_eq!(app.open.as_ref().unwrap().lay.box_top, above);
        let border = shown.rows_text(above as usize, above as usize + 1);
        assert!(border.starts_with("╭─ peek"), "{border}");
        // Each chat row at most once (and the cursor put back).
        assert!(rows <= top + 1, "{rows} rows written");
        // The rows the chat scrolled off came back from memory, not blank.
        let first = shown.rows_text(0, 1);
        assert_eq!(first, app.shadow.snapshot().rows_text(0, 1));
        app.close(&mut out, false).unwrap();
        real.advance(&std::mem::take(&mut out));
        let (now, shown) = (app.shadow.snapshot(), real.snapshot());
        for r in 0..40 {
            assert_eq!(
                shown.rows_text(r, r + 1),
                now.rows_text(r, r + 1),
                "row {r}"
            );
        }
    }

    fn codex_selection_and_live_area(capture: &[u8], selected: &str) {
        let mut app = test_app(120, 40);
        let mut real = Shadow::new(120, 40);
        let mut child: Vec<u8> = Vec::new();
        let mut out: Vec<u8> = Vec::new();

        app.on_output(capture, &mut out, &mut child).unwrap();
        app.shadow.flush_sync();
        real.advance(&std::mem::take(&mut out));
        real.flush_sync();
        assert!(app.shadow.alt_screen());

        app.open_peek(&mut out).unwrap();
        real.advance(&std::mem::take(&mut out));
        let open = app.open.as_ref().unwrap();
        assert_eq!(open.peek.title, selected, "Codex's own selection is used");
        let top = open.strip_top.expect("input area found on the full screen");
        assert!(top > 11 && top < 40);
        // The row right above the box (1-based), outside it.
        assert!(open.lay.below && (open.lay.box_top as usize) > 0);
        let above = open.lay.box_top as usize;

        // Mouse motion is ignored; the wheel goes to Codex, which scrolls its
        // chat, and the box stays.
        app.on_input(b"\x1b[<35;40;3M\x1b[<65;40;3M", &mut out, &mut child)
            .unwrap();
        assert_eq!(child, b"\x1b[<65;40;3M");
        assert!(app.open.is_some());
        child.clear();
        // Typing goes to Codex; its input line updates live under the box.
        app.on_input(b"hi", &mut out, &mut child).unwrap();
        assert_eq!(child, b"hi");
        let echo = format!("\x1b[?2026h\x1b[{};5Hhi\x1b[?2026l", top + 2);
        app.on_output(echo.as_bytes(), &mut out, &mut child)
            .unwrap();
        app.shadow.flush_sync();
        app.tick(&mut out).unwrap();
        real.advance(&std::mem::take(&mut out));
        real.flush_sync();
        let now = app.shadow.snapshot();
        let shown = real.snapshot();
        for r in top..40 {
            assert!(
                same_cells(&shown.rows[r], &now.rows[r]),
                "live row {r} not updated"
            );
        }
        // A click outside the box closes it and still reaches Codex.
        let click = format!("\x1b[<0;5;{above}M");
        app.on_input(click.as_bytes(), &mut out, &mut child)
            .unwrap();
        assert!(app.open.is_none());
        assert!(child.ends_with(click.as_bytes()));
        real.advance(&std::mem::take(&mut out));
        real.flush_sync();
        assert!(same_screen(&real.snapshot(), &app.shadow.snapshot()));
    }

    /// Real Claude Code 2.1.283 sessions at 120x40 (startup and one answer),
    /// one per renderer. Personal data replaced with same-length text.
    const CLAUDE: [&[u8]; 2] = [
        include_bytes!("../tests/fixtures/claude_fullscreen_120x40.bin"),
        include_bytes!("../tests/fixtures/claude_inline_120x40.bin"),
    ];

    #[test]
    fn round_trip_over_real_claude_output() {
        for capture in CLAUDE {
            let mut base = Shadow::new(120, 40);
            base.advance(capture);
            base.flush_sync();
            let snap = base.snapshot();
            for first in (0..40).step_by(3) {
                let last = (first + 1).min(39);
                let mut real = Shadow::new(120, 40);
                real.advance(capture);
                real.flush_sync();
                let lay = overlay::layout(40, first, last);
                let mut peek = PeekBox::new("rebase");
                peek.text = "Streaming **explanation** text. ".repeat(12);
                real.advance(overlay::open_frame(&snap, &lay, &peek).as_bytes());
                real.advance(overlay::close_frame(&snap, &lay).as_bytes());
                assert!(
                    same_screen(&real.snapshot(), &snap),
                    "not restored for selection at row {first}"
                );
            }
        }
    }

    #[test]
    fn real_claude_output_held_mid_stream() {
        // Claude writes nearly everything inside synchronized frames, and
        // peekme opens between them: cut the captures after frame ends.
        let mut checked = 0;
        for capture in CLAUDE {
            let ends: Vec<usize> = capture
                .windows(SYNC_END.len())
                .enumerate()
                .filter(|(_, w)| *w == SYNC_END.as_bytes())
                .map(|(i, _)| i + SYNC_END.len())
                .collect();
            for k in 1..=10 {
                let cut = ends[ends.len() * k / 11];
                let mut shadow = Shadow::new(120, 40);
                let mut real = Shadow::new(120, 40);
                shadow.advance(&capture[..cut]);
                real.advance(&capture[..cut]);
                let mut boundary = Boundary::default();
                boundary.feed(&capture[..cut]);
                if shadow.in_sync_update() || !boundary.at_boundary() {
                    continue;
                }
                checked += 1;
                let snap = shadow.snapshot();
                let lay = overlay::layout(40, 10, 11);
                real.advance(overlay::open_frame(&snap, &lay, &PeekBox::new("x")).as_bytes());
                shadow.advance(&capture[cut..]);
                real.advance(overlay::close_frame(&snap, &lay).as_bytes());
                real.advance(&render::strip_answered_queries(&capture[cut..]));
                shadow.flush_sync();
                real.flush_sync();
                assert!(
                    same_screen(&real.snapshot(), &shadow.snapshot()),
                    "diverged when cut at {cut}"
                );
            }
        }
        assert!(
            checked >= 12,
            "too few cut points between frames: {checked}"
        );
    }

    /// Claude Code full screen: a mouse drag goes to Claude, which answers the
    /// release with OSC 52; Alt+P explains that text, located at the drag's row.
    #[test]
    fn claude_fullscreen_selection_and_live_area() {
        let mut app = test_app_for(Agent::Claude, 120, 40);
        let mut real = Shadow::new(120, 40);
        let mut child: Vec<u8> = Vec::new();
        let mut out: Vec<u8> = Vec::new();
        app.on_output(CLAUDE[0], &mut out, &mut child).unwrap();
        app.shadow.flush_sync();
        real.advance(&std::mem::take(&mut out));
        real.flush_sync();

        // Row 10 (1-based) holds the answer's first line; drag over part of it.
        app.on_input(
            b"\x1b[<0;5;10M\x1b[<32;12;10M\x1b[<0;12;10m",
            &mut out,
            &mut child,
        )
        .unwrap();
        assert!(
            child.ends_with(b"\x1b[<0;12;10m"),
            "the drag reaches Claude"
        );
        // "merge c" as Claude copies it.
        app.on_output(b"\x1b]52;c;bWVyZ2UgYw==\x07", &mut out, &mut child)
            .unwrap();
        real.advance(&std::mem::take(&mut out));

        app.open_peek(&mut out).unwrap();
        real.advance(&std::mem::take(&mut out));
        let open = app.open.as_ref().unwrap();
        assert_eq!(open.peek.title, "merge c");
        assert_eq!(
            (open.lay.box_top as usize),
            10,
            "box right under the selected row"
        );
        assert_eq!(open.peek.child_name, "Claude");
        let top = open.strip_top.expect("Claude's input box stays live");
        assert_eq!(top, 35);

        // Typing goes to Claude and shows in its input line under the box.
        app.on_input(b"hi", &mut out, &mut child).unwrap();
        assert!(child.ends_with(b"hi") && app.open.is_some());
        let echo = format!("\x1b[?2026h\x1b[{};3Hhi\x1b[?2026l", top + 2);
        app.on_output(echo.as_bytes(), &mut out, &mut child)
            .unwrap();
        app.tick(&mut out).unwrap();
        real.advance(&std::mem::take(&mut out));
        let (now, shown) = (app.shadow.snapshot(), real.snapshot());
        for r in top..40 {
            assert!(
                same_cells(&shown.rows[r], &now.rows[r]),
                "live row {r} not updated"
            );
        }

        // Esc closes; the terminal ends where Claude's own screen is.
        app.on_input(b"\x1b", &mut out, &mut child).unwrap();
        assert!(app.open.is_none());
        real.advance(&std::mem::take(&mut out));
        real.flush_sync();
        assert!(same_screen(&real.snapshot(), &app.shadow.snapshot()));

        // A click (no drag, no OSC 52) forgets the selection.
        app.on_input(b"\x1b[<0;5;5M\x1b[<0;5;5m", &mut out, &mut child)
            .unwrap();
        assert!(app.app_selection.is_none());
    }

    /// Alt+P while the child's output stopped inside an escape sequence (here
    /// its window title) waits until the sequence is complete.
    #[test]
    fn no_box_inside_an_escape_sequence() {
        let mut app = test_app_for(Agent::Claude, 120, 40);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.on_output(CLAUDE[0], &mut out, &mut child).unwrap();
        app.app_selection = Some(AppSelection {
            text: "merge".into(),
            cell: (9, 5),
        });
        app.on_output(b"\x1b]0;\xe2\x97\x90 Git reb", &mut out, &mut child)
            .unwrap();
        app.request_open(&mut out).unwrap();
        assert!(app.open.is_none() && app.pending.is_some());
        app.on_output(b"ase vs merge\x07", &mut out, &mut child)
            .unwrap();
        assert!(app.open.is_some(), "opens once the title is complete");
    }

    /// Option+P on a Mac arrives as `π`, also over SSH and in tmux, where
    /// nothing says it came from a Mac. It explains a selection and is typed
    /// otherwise.
    #[test]
    fn option_p_explains_a_selection_and_types_otherwise() {
        let mut app = test_app_for(Agent::Claude, 120, 40);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.on_output(CLAUDE[0], &mut out, &mut child).unwrap();
        app.shadow.flush_sync();
        // The terminal sends mouse reports (on a Mac, the first π would say
        // so when it never had).
        app.mouse_seen = true;

        // Nothing selected: π is typed.
        app.selection = SelectionSource::without_system();
        app.on_input("π".as_bytes(), &mut out, &mut child).unwrap();
        assert!(app.open.is_none(), "no box without a selection");
        assert_eq!(child, "π".as_bytes());
        child.clear();

        // A drag Claude reported: π explains it and never reaches Claude.
        app.on_input(b"\x1b[<0;5;10M\x1b[<0;12;10m", &mut out, &mut child)
            .unwrap();
        app.on_output(b"\x1b]52;c;bWVyZ2UgYw==\x07", &mut out, &mut child)
            .unwrap();
        child.clear();
        app.on_input("π".as_bytes(), &mut out, &mut child).unwrap();
        assert!(app.open.is_some(), "π opened the box");
        assert!(child.is_empty(), "π did not reach Claude");
        app.on_input(b"\x1b", &mut out, &mut child).unwrap();

        // Someone typing Greek keeps the letter, selection or not.
        app.on_input("αβ".as_bytes(), &mut out, &mut child).unwrap();
        child.clear();
        app.on_input("π".as_bytes(), &mut out, &mut child).unwrap();
        assert!(app.open.is_none());
        assert_eq!(child, "π".as_bytes());
    }

    /// Option+Q on a Mac arrives as `œ`: like `π`, it asks about a selection
    /// and is typed otherwise, and always for someone who types French.
    #[test]
    fn option_q_asks_about_a_selection_and_types_otherwise() {
        let mut app = test_app_for(Agent::Claude, 120, 40);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.on_output(CLAUDE[0], &mut out, &mut child).unwrap();
        app.shadow.flush_sync();
        app.mouse_seen = true;

        app.selection = SelectionSource::without_system();
        app.on_input("œ".as_bytes(), &mut out, &mut child).unwrap();
        assert!(app.open.is_none(), "no box without a selection");
        assert_eq!(child, "œ".as_bytes());
        child.clear();

        app.on_input(b"\x1b[<0;5;10M\x1b[<0;12;10m", &mut out, &mut child)
            .unwrap();
        app.on_output(b"\x1b]52;c;bWVyZ2UgYw==\x07", &mut out, &mut child)
            .unwrap();
        child.clear();
        app.on_input("œ".as_bytes(), &mut out, &mut child).unwrap();
        let open = app.open.as_ref().expect("œ opened the box");
        assert_eq!(open.peek.input.as_deref(), Some(""), "waits for a question");
        assert!(child.is_empty(), "œ did not reach Claude");
        app.on_input(b"\x1b", &mut out, &mut child).unwrap();

        app.on_input("café".as_bytes(), &mut out, &mut child)
            .unwrap();
        child.clear();
        app.on_input("œ".as_bytes(), &mut out, &mut child).unwrap();
        assert!(app.open.is_none());
        assert_eq!(child, "œ".as_bytes());
    }

    #[test]
    fn option_p_waits_for_codex_selection_repaint() {
        let mut app = test_app(40, 8);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.on_output(b"\x1b[2;1Hpick me", &mut out, &mut child)
            .unwrap();
        let input = b"\x1b[<0;1;2M\x1b[<0;8;2m\xcf\x80";
        app.on_input(input, &mut out, &mut child).unwrap();
        assert_eq!(child, &input[..input.len() - 2]);
        app.on_output(b"\x1b[2;1H\x1b[7mpick me\x1b[0m", &mut out, &mut child)
            .unwrap();
        assert_eq!(
            app.open.as_ref().unwrap().selection.as_ref().unwrap().0,
            "pick me"
        );
        assert_eq!(child, &input[..input.len() - 2]);
    }

    #[test]
    fn option_p_after_drag_is_forwarded_without_a_selection() {
        let mut app = test_app(40, 8);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        let input = b"\x1b[<0;1;2M\x1b[<0;8;2m\xcf\x80";
        app.on_input(input, &mut out, &mut child).unwrap();
        app.pending_hotkey.as_mut().unwrap().since = Instant::now() - HOTKEY_SELECTION_WAIT;
        app.on_idle(&mut out, &mut child).unwrap();
        assert!(app.open.is_none());
        assert_eq!(child, input);
    }

    /// Codex asked for the mouse, but the terminal never sent a mouse report
    /// (Terminal with Allow Mouse Reporting off): the drag is the terminal's
    /// own selection. The box says why instead of "select some text".
    #[test]
    fn says_when_the_terminal_keeps_the_mouse() {
        let mut app = test_app_for(Agent::Codex, 100, 20);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.selection = SelectionSource::without_system();
        app.on_output(
            b"\x1b[?1000h\x1b[?1002h\x1b[?1006hsome answer\r\n",
            &mut out,
            &mut child,
        )
        .unwrap();
        app.shadow.flush_sync();
        out.clear();
        app.on_input(b"\x1bp", &mut out, &mut child).unwrap();
        assert!(app.open.is_some());
        assert!(String::from_utf8_lossy(&out).contains("Reporting"));
        app.on_input(b"\x1b", &mut out, &mut child).unwrap();

        // Once a mouse report came, the terminal does pass the mouse on.
        app.on_input(b"\x1b[<0;3;1M\x1b[<0;3;1m", &mut out, &mut child)
            .unwrap();
        out.clear();
        app.on_input(b"\x1bp", &mut out, &mut child).unwrap();
        assert!(app.open.is_some());
        assert!(!String::from_utf8_lossy(&out).contains("Reporting"));
    }

    /// On a Mac with no tmux Codex copies to the pasteboard, which peekme reads
    /// as the system selection. Once used, that copy is no selection any more:
    /// π with nothing highlighted is typed, until something new is copied.
    #[test]
    fn option_p_types_once_the_mac_clipboard_copy_was_used() {
        let mut app = test_app_for(Agent::Codex, 60, 10);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.selection = SelectionSource::fixed_clipboard("Desktop app");
        app.on_output(
            b"\x1b[3;3H\xe2\x80\xa2 \x1b[7mDesktop app\x1b[0m: install it\r\n",
            &mut out,
            &mut child,
        )
        .unwrap();
        app.shadow.flush_sync();
        app.on_input("π".as_bytes(), &mut out, &mut child).unwrap();
        assert!(app.open.is_some(), "π opened the box");
        assert_eq!(app.selection_source, "reverse");
        app.on_input(b"\x1b", &mut out, &mut child).unwrap();
        assert!(app.open.is_none());

        // A click cleared Codex's highlight; the copy is still in the clipboard.
        app.on_output(b"\x1b[3;5HDesktop app", &mut out, &mut child)
            .unwrap();
        app.shadow.flush_sync();
        child.clear();
        app.on_input("π".as_bytes(), &mut out, &mut child).unwrap();
        assert!(app.open.is_none(), "an old copy is no selection");
        assert_eq!(child, "π".as_bytes());

        // Something new copied (the terminal's own copy on select) counts.
        app.selection = SelectionSource::fixed_clipboard("install it");
        child.clear();
        app.on_input("π".as_bytes(), &mut out, &mut child).unwrap();
        assert!(app.open.is_some(), "a new copy is a selection");
        assert_eq!(app.selection_source, "system");
    }

    /// Codex 0.159 copies a selection in an answer as markdown source, which
    /// is not on screen. Its reverse-video highlight is then the selection, so
    /// π still explains instead of being typed.
    #[test]
    fn option_p_uses_codex_highlight_when_the_copy_is_markdown() {
        let mut app = test_app_for(Agent::Codex, 60, 10);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.selection = SelectionSource::without_system();
        app.on_output(
            b"\x1b[3;3H\xe2\x80\xa2 \x1b[7mDesktop app\x1b[0m: install it with apt\r\n",
            &mut out,
            &mut child,
        )
        .unwrap();
        app.shadow.flush_sync();
        app.on_input(b"\x1b[<0;5;3M\x1b[<0;15;3m", &mut out, &mut child)
            .unwrap();
        // "- **Desktop app**"
        app.on_output(
            b"\x1b]52;c;LSAqKkRlc2t0b3AgYXBwKio=\x07",
            &mut out,
            &mut child,
        )
        .unwrap();
        child.clear();
        app.on_input("π".as_bytes(), &mut out, &mut child).unwrap();
        assert!(app.open.is_some(), "π opened the box");
        assert!(child.is_empty(), "π did not reach Codex");
        assert_eq!(app.selection_source, "reverse");

        // A long copy found on screen by its first words only, markdown after
        // them: still Codex's highlight, the text the user sees.
        let mut app = test_app_for(Agent::Codex, 70, 10);
        app.selection = SelectionSource::without_system();
        app.on_output(
            b"\x1b[3;1H\x1b[7m3. Step number 3 of the plan, with some filler words.\x1b[0m\r\n\
              \x1b[7m\xe2\x80\xa2 Desktop app\x1b[0m: install it\r\n",
            &mut out,
            &mut child,
        )
        .unwrap();
        app.shadow.flush_sync();
        app.on_input(b"\x1b[<0;1;3M\x1b[<0;13;4m", &mut out, &mut child)
            .unwrap();
        // "3. Step number 3 of the plan, with some filler words.\n- **Desktop app**"
        let osc = b"\x1b]52;c;My4gU3RlcCBudW1iZXIgMyBvZiB0aGUgcGxhbiwgd2l0aCBzb21lIGZpbGxlciB3b3Jkcy4KLSAqKkRlc2t0b3AgYXBwKio=\x07";
        app.on_output(osc, &mut out, &mut child).unwrap();
        child.clear();
        app.on_input("π".as_bytes(), &mut out, &mut child).unwrap();
        assert!(app.open.is_some());
        assert_eq!(app.selection_source, "reverse");
        let sel = &app.open.as_ref().unwrap().selection.as_ref().unwrap().0;
        assert!(sel.ends_with("• Desktop app"), "{sel:?}");
    }

    /// Real GitHub Copilot CLI 1.0.89 at 120x40: startup, one question and its
    /// answer. It draws full screen without synchronized frames.
    const COPILOT: &[u8] = include_bytes!("../tests/fixtures/copilot_fullscreen_120x40.bin");

    #[test]
    fn round_trip_over_real_copilot_output() {
        let mut base = Shadow::new(120, 40);
        base.advance(COPILOT);
        let snap = base.snapshot();
        for first in (0..40).step_by(3) {
            let mut real = Shadow::new(120, 40);
            real.advance(COPILOT);
            let lay = overlay::layout(40, first, (first + 1).min(39));
            let mut peek = PeekBox::new("rebase");
            peek.text = "Streaming **explanation** text. ".repeat(12);
            real.advance(overlay::open_frame(&snap, &lay, &peek).as_bytes());
            real.advance(overlay::close_frame(&snap, &lay).as_bytes());
            assert!(same_screen(&real.snapshot(), &snap), "row {first}");
        }
        // Output that arrives while the box is open, cut wherever the stream
        // is between escape sequences (no frames to wait for here).
        let mut checked = 0;
        for k in 1..=12 {
            let cut = COPILOT.len() * k / 13;
            let mut boundary = Boundary::default();
            boundary.feed(&COPILOT[..cut]);
            if !boundary.at_boundary() {
                continue;
            }
            checked += 1;
            let (mut shadow, mut real) = (Shadow::new(120, 40), Shadow::new(120, 40));
            shadow.advance(&COPILOT[..cut]);
            real.advance(&COPILOT[..cut]);
            let snap = shadow.snapshot();
            let lay = overlay::layout(40, 10, 11);
            real.advance(overlay::open_frame(&snap, &lay, &PeekBox::new("x")).as_bytes());
            shadow.advance(&COPILOT[cut..]);
            real.advance(overlay::close_frame(&snap, &lay).as_bytes());
            real.advance(&render::strip_answered_queries(&COPILOT[cut..]));
            assert!(
                same_screen(&real.snapshot(), &shadow.snapshot()),
                "cut {cut}"
            );
        }
        assert!(checked >= 4, "cut points at a boundary: {checked}");
    }

    /// Copilot reports a drag with OSC 52 `p!;..;`; Alt+P explains it at the
    /// drag's row, with Copilot's input box live under the box.
    #[test]
    fn copilot_selection_and_live_area() {
        let mut app = test_app_for(Agent::Copilot, 120, 40);
        let mut real = Shadow::new(120, 40);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.on_output(COPILOT, &mut out, &mut child).unwrap();
        real.advance(&std::mem::take(&mut out));

        // Row 22 (1-based) holds "git merge combines two branches".
        app.on_input(b"\x1b[<0;5;22M\x1b[<0;30;22m", &mut out, &mut child)
            .unwrap();
        // "merge  combines two" as Copilot copies it.
        app.on_output(
            b"\x1b]52;p!;bWVyZ2UgIGNvbWJpbmVzIHR3bw==;\x07",
            &mut out,
            &mut child,
        )
        .unwrap();
        real.advance(&std::mem::take(&mut out));
        app.on_input(b"\x1bp", &mut out, &mut child).unwrap();
        real.advance(&std::mem::take(&mut out));
        let open = app.open.as_ref().expect("box open");
        assert_eq!(open.peek.title, "merge combines two");
        assert_eq!(
            (open.lay.box_top as usize),
            22,
            "right under the selected row"
        );
        assert_eq!(open.peek.child_name, "Copilot");
        let top = open.strip_top.expect("Copilot's input box stays live");
        assert_eq!(top, 36);

        app.on_input(b"\x1b", &mut out, &mut child).unwrap();
        real.advance(&std::mem::take(&mut out));
        assert!(same_screen(&real.snapshot(), &app.shadow.snapshot()));
        // Copilot's reverse-video tabs are not a selection.
        app.app_selection = None;
        assert!(app.current_selection(&app.shadow.snapshot()).0.is_none());
    }

    /// Real Kiro CLI 2.28.0 at 120x40: startup, one question with two tool
    /// calls and the answer; drawn inline, then the same after `/fullscreen`.
    const KIRO: [&[u8]; 2] = [
        include_bytes!("../tests/fixtures/kiro_inline_120x40.bin"),
        include_bytes!("../tests/fixtures/kiro_fullscreen_120x40.bin"),
    ];

    #[test]
    fn round_trip_over_real_kiro_output() {
        for capture in KIRO {
            let mut base = Shadow::new(120, 40);
            base.advance(capture);
            base.flush_sync();
            let snap = base.snapshot();
            for first in (0..40).step_by(3) {
                let mut real = Shadow::new(120, 40);
                real.advance(capture);
                real.flush_sync();
                let lay = overlay::layout(40, first, (first + 1).min(39));
                let mut peek = PeekBox::new("manifest");
                peek.text = "Streaming **explanation** text. ".repeat(12);
                real.advance(overlay::open_frame(&snap, &lay, &peek).as_bytes());
                real.advance(overlay::close_frame(&snap, &lay).as_bytes());
                assert!(same_screen(&real.snapshot(), &snap), "row {first}");
            }
        }
    }

    /// Kiro full screen reports a drag with OSC 52 on release; Alt+P explains
    /// it at the drag's row, with Kiro's input area live under the box.
    #[test]
    fn kiro_fullscreen_selection_and_live_area() {
        let mut app = test_app_for(Agent::Kiro, 120, 40);
        let mut real = Shadow::new(120, 40);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.on_output(KIRO[1], &mut out, &mut child).unwrap();
        app.shadow.flush_sync();
        real.advance(&std::mem::take(&mut out));
        real.flush_sync();

        // Row 27 (1-based) holds "Cargo.toml — Rust package manifest".
        app.on_input(
            b"\x1b[<0;19;27M\x1b[<32;40;27M\x1b[<0;40;27m",
            &mut out,
            &mut child,
        )
        .unwrap();
        assert!(child.ends_with(b"\x1b[<0;40;27m"), "the drag reaches Kiro");
        // "Rust package manifest" as Kiro copies it.
        app.on_output(
            b"\x1b]52;c;UnVzdCBwYWNrYWdlIG1hbmlmZXN0\x07",
            &mut out,
            &mut child,
        )
        .unwrap();
        real.advance(&std::mem::take(&mut out));
        app.on_input(b"\x1bp", &mut out, &mut child).unwrap();
        real.advance(&std::mem::take(&mut out));
        let open = app.open.as_ref().expect("box open");
        assert_eq!(open.peek.title, "Rust package manifest");
        // Four rows down to the input area are too few: the box goes above row 27.
        assert!(!open.lay.below);
        assert_eq!((open.lay.box_top as usize) + open.lay.box_height, 26);
        assert_eq!(open.peek.child_name, "Kiro");
        assert_eq!(open.strip_top, Some(31), "Kiro's input area stays live");

        app.on_input(b"\x1b", &mut out, &mut child).unwrap();
        real.advance(&std::mem::take(&mut out));
        real.flush_sync();
        assert!(same_screen(&real.snapshot(), &app.shadow.snapshot()));
    }

    /// Terminal.app keeps Kiro inline's selection to itself. The first π with
    /// nothing peekme can read offers Accessibility; Enter asks macOS, and
    /// after that (or after Esc) π is typed again. The screen comes back.
    #[test]
    fn kiro_inline_in_mac_terminal_offers_accessibility_once() {
        for answer in [&b"\r"[..], b"\x1b"] {
            let mut app = test_app_for(Agent::Kiro, 120, 40);
            app.can_offer_accessibility = true;
            let mut real = Shadow::new(120, 40);
            let (mut child, mut out) = (Vec::new(), Vec::new());
            app.on_output(KIRO[0], &mut out, &mut child).unwrap();
            app.shadow.flush_sync();
            real.advance(&std::mem::take(&mut out));
            real.flush_sync();

            app.on_input("π".as_bytes(), &mut out, &mut child).unwrap();
            real.advance(&std::mem::take(&mut out));
            let open = app.open.as_ref().expect("the offer");
            assert_eq!(
                open.peek.status,
                Status::Message(ACCESSIBILITY_OFFER.into())
            );
            assert!(child.is_empty(), "π did not reach Kiro");

            app.on_input(answer, &mut out, &mut child).unwrap();
            real.advance(&std::mem::take(&mut out));
            assert!(child.is_empty(), "the answer did not reach Kiro");
            if answer == b"\r" {
                let open = app.open.as_ref().expect("what to do next");
                assert_eq!(
                    open.peek.status,
                    Status::Message(ACCESSIBILITY_ASKED.into())
                );
                app.on_input(b"\x1b", &mut out, &mut child).unwrap();
                real.advance(&std::mem::take(&mut out));
            }
            assert!(app.open.is_none());
            real.flush_sync();
            assert!(same_screen(&real.snapshot(), &app.shadow.snapshot()));

            app.on_input("π".as_bytes(), &mut out, &mut child).unwrap();
            assert!(app.open.is_none(), "offered once");
            assert_eq!(child, "π".as_bytes());
        }
    }

    /// Not offered outside Terminal.app, nor when the agent takes the mouse.
    #[test]
    fn accessibility_is_not_offered_when_it_would_not_help() {
        let mut app = test_app_for(Agent::Kiro, 120, 40);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.on_output(KIRO[0], &mut out, &mut child).unwrap();
        app.on_input("π".as_bytes(), &mut out, &mut child).unwrap();
        assert!(app.open.is_none());
        assert_eq!(child, "π".as_bytes());

        let mut app = test_app_for(Agent::Kiro, 120, 40);
        app.can_offer_accessibility = true;
        app.mouse_seen = true;
        child.clear();
        app.on_output(KIRO[1], &mut out, &mut child).unwrap();
        app.on_input("π".as_bytes(), &mut out, &mut child).unwrap();
        assert!(app.open.is_none());
        assert_eq!(child, "π".as_bytes());
    }

    /// Kiro inline leaves the mouse to the terminal: Alt+P explains the
    /// terminal's own selection.
    #[test]
    fn kiro_inline_explains_the_terminal_selection() {
        let mut app = test_app_for(Agent::Kiro, 120, 40);
        app.selection = SelectionSource::fixed("main.rs — the program entry point");
        let mut real = Shadow::new(120, 40);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.on_output(KIRO[0], &mut out, &mut child).unwrap();
        app.shadow.flush_sync();
        real.advance(&std::mem::take(&mut out));
        real.flush_sync();
        app.on_input(b"\x1bp", &mut out, &mut child).unwrap();
        real.advance(&std::mem::take(&mut out));
        let open = app.open.as_ref().expect("box open");
        assert_eq!(app.selection_source, "system");
        assert_eq!(open.peek.title, "main.rs — the program entry point");
        assert_eq!(
            (open.lay.box_top as usize) + open.lay.box_height,
            26,
            "right above row 27"
        );
        assert!(child.is_empty(), "Alt+P does not reach Kiro");

        app.on_input(b"\x1b", &mut out, &mut child).unwrap();
        real.advance(&std::mem::take(&mut out));
        real.flush_sync();
        assert!(same_screen(&real.snapshot(), &app.shadow.snapshot()));
    }

    /// Over SSH, Kiro's inline screen leaves the selection on the user's own
    /// computer: Alt+P with nothing seen points to `/fullscreen`.
    #[test]
    fn kiro_inline_over_ssh_suggests_fullscreen() {
        let mut app = test_app_for(Agent::Kiro, 120, 40);
        app.over_ssh = true;
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.on_output(KIRO[0], &mut out, &mut child).unwrap();
        app.on_input(b"\x1bp", &mut out, &mut child).unwrap();
        let open = app.open.as_ref().expect("a message box");
        match &open.peek.status {
            Status::Message(m) => assert!(m.contains("/fullscreen"), "{m}"),
            other => panic!("not a message: {other:?}"),
        }
    }

    /// Alt+Q: the box asks for a question first; Enter sends it with
    /// the selection, and the answer is saved with the question.
    #[test]
    fn alt_q_asks_a_typed_question() {
        let mut app = test_app(40, 24);
        let (tx, rx) = mpsc::channel();
        app.tx = tx;
        app.explainer = Some(Explainer::Fake);
        app.selection = SelectionSource::fixed("true colour");
        let (mut out, mut child) = (Vec::new(), Vec::new());
        app.on_output(SCREEN, &mut out, &mut child).unwrap();
        app.on_input(b"\x1bq", &mut out, &mut child).unwrap();
        let open = app.open.as_ref().expect("a box");
        assert_eq!(open.peek.input.as_deref(), Some(""), "waits for a question");
        assert!(rx.try_recv().is_err(), "nothing sent before the question");
        // Typing (π too), Backspace, a paste, Ctrl+W: all in the box, none to the child.
        app.on_input(b"why soo", &mut out, &mut child).unwrap();
        app.on_input(b"\x7f", &mut out, &mut child).unwrap();
        app.on_input(b" \xcf\x80 xx", &mut out, &mut child).unwrap();
        app.on_input(b"\x17\x1b[200~bright\r\n\x1b[201~", &mut out, &mut child)
            .unwrap();
        assert!(child.is_empty(), "keys reached the child: {child:?}");
        let typed = app.open.as_ref().unwrap().peek.input.clone();
        assert_eq!(typed.as_deref(), Some("why so π bright  "));
        app.on_input(b"\r", &mut out, &mut child).unwrap();
        let open = app.open.as_ref().unwrap();
        assert_eq!(open.peek.input, None);
        assert_eq!(open.peek.question.as_deref(), Some("why so π bright"));
        loop {
            if let Msg::Peek(id, p) = rx.recv_timeout(Duration::from_secs(5)).expect("progress") {
                let done = matches!(p, Progress::Done);
                app.on_progress(id, p, &mut out).unwrap();
                if done {
                    break;
                }
            }
        }
        let open = app.open.as_ref().unwrap();
        assert_eq!(
            open.peek.text,
            "Test answer to ⟦why so π bright⟧ about ⟦true colour⟧."
        );
        let saved = app
            .marks
            .find("true colour")
            .and_then(|id| app.marks.answer(id));
        let saved = saved.expect("saved");
        assert_eq!(
            saved.question.as_deref(),
            Some("why so π bright"),
            "saved with its question"
        );
        assert_eq!(
            saved.answer,
            "Test answer to ⟦why so π bright⟧ about ⟦true colour⟧."
        );
        assert!(child.is_empty());
    }

    /// Alt+Q in a box that explained something: a new box asks about
    /// the same words; Esc while typing closes it and nothing is sent.
    #[test]
    fn alt_q_in_a_box_asks_about_its_words() {
        let mut app = app_with_an_answer("short", false);
        let (mut out, mut child) = (Vec::new(), Vec::new());
        app.on_input(b"\x1bq", &mut out, &mut child).unwrap();
        let open = app.open.as_ref().expect("a box");
        assert_eq!(open.peek.input.as_deref(), Some(""));
        assert_eq!(open.peek.title, "true colour");
        assert_eq!(open.lay.box_height, 4, "as tall as the question line");
        app.on_input(b"x", &mut out, &mut child).unwrap();
        app.on_input(b"\x1b", &mut out, &mut child).unwrap();
        assert!(app.open.is_none());
        assert!(child.is_empty());
    }

    /// Alt+Q on words in a box: a box inside it waits for a question;
    /// it is sent with the answers around it, and saved with the question.
    /// Alt+Q with nothing selected asks again about the same words.
    #[test]
    fn alt_q_on_words_in_a_box_asks_about_them() {
        let answer = "A pseudo-terminal lets job control work inside tmux panes.";
        let mut app = app_with_an_answer(answer, true);
        let (tx, rx) = mpsc::channel();
        app.tx = tx;
        app.explainer = Some(Explainer::Fake);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        let top = app.open.as_ref().unwrap().lay.box_top as usize;
        let at = |col: usize| (top + 1, 2 + col);
        let mut drag = mouse(0, at(23), true);
        drag.extend(mouse(32, at(33), true));
        drag.extend(mouse(0, at(33), false));
        app.on_input(&drag, &mut out, &mut child).unwrap();
        app.on_input(b"\x1bq", &mut out, &mut child).unwrap();
        let open = app.open.as_ref().unwrap();
        assert_eq!(open.nested.len(), 1);
        let n = &open.nested[0];
        assert_eq!(n.words, "job control");
        assert_eq!(n.peek.input.as_deref(), Some(""), "waits for a question");
        assert!(rx.try_recv().is_err(), "nothing sent before the question");

        app.on_input(b"how does fg work?", &mut out, &mut child)
            .unwrap();
        app.on_input(b"\r", &mut out, &mut child).unwrap();
        let wait_done = |app: &mut App, out: &mut Vec<u8>| loop {
            if let Msg::Peek(id, p) = rx.recv_timeout(Duration::from_secs(5)).expect("progress") {
                let done = matches!(p, Progress::Done);
                app.on_progress(id, p, out).unwrap();
                if done {
                    break;
                }
            }
        };
        wait_done(&mut app, &mut out);
        let n = &app.open.as_ref().unwrap().nested[0];
        assert_eq!(n.peek.question.as_deref(), Some("how does fg work?"));
        assert!(
            n.peek
                .text
                .starts_with("Test answer to ⟦how does fg work?⟧ about ⟦job control⟧"),
            "{}",
            n.peek.text
        );
        let saved = n.answer.and_then(|id| app.marks.answer(id)).expect("saved");
        assert_eq!(saved.question.as_deref(), Some("how does fg work?"));
        assert!(saved.answer.starts_with("Test answer to"));

        // Nothing selected now: Alt+Shift+P (the old ask key, still there)
        // asks about the same words again.
        app.on_input(b"\x1bP", &mut out, &mut child).unwrap();
        let open = app.open.as_ref().unwrap();
        assert_eq!(open.nested.len(), 1);
        assert_eq!(open.nested[0].words, "job control");
        assert_eq!(open.nested[0].peek.input.as_deref(), Some(""));
        // Esc closes only that box.
        app.on_input(b"\x1b", &mut out, &mut child).unwrap();
        assert!(app.open.as_ref().is_some_and(|o| o.nested.is_empty()));
        assert!(child.is_empty(), "the agent got no key");
    }

    /// Words of an answer that were peeked inside keep a dotted line in the
    /// box, but only while the box shows that answer: a new answer to the
    /// same selection (Alt+P again for the whole chat, or a typed question)
    /// must not show the old answer's lines while it streams.
    #[test]
    fn a_new_answer_does_not_show_the_old_answers_inner_marks() {
        let answer = "A pseudo-terminal lets job control work inside tmux panes.";
        for again in [&b"\x1bp"[..], b"\x1bq"] {
            let mut app = app_with_an_answer(answer, true);
            let (mut child, mut out) = (Vec::new(), Vec::new());
            let top = app.open.as_ref().unwrap().lay.box_top as usize;
            let at = |col: usize| (top + 1, 2 + col);
            // A peek inside the box on "job control", answered.
            let mut drag = mouse(0, at(23), true);
            drag.extend(mouse(32, at(33), true));
            drag.extend(mouse(0, at(33), false));
            app.on_input(&drag, &mut out, &mut child).unwrap();
            app.on_input(b"\x1bp", &mut out, &mut child).unwrap();
            let id = app.open.as_ref().unwrap().nested[0].id;
            app.on_progress(id, Progress::Delta("Ctrl+Z, fg and bg.".into()), &mut out)
                .unwrap();
            app.on_progress(id, Progress::Done, &mut out).unwrap();
            // Close both boxes, select the same text again, open its saved answer.
            app.on_input(b"\x1b", &mut out, &mut child).unwrap();
            app.on_input(b"\x1b", &mut out, &mut child).unwrap();
            assert!(app.open.is_none());
            app.selection = SelectionSource::fixed("true colour");
            app.on_input(b"\x1bp", &mut out, &mut child).unwrap();
            let mut real = Shadow::new(40, 24);
            real.advance(SCREEN);
            real.advance(&std::mem::take(&mut out));
            let top = app.open.as_ref().unwrap().lay.box_top as usize;
            assert_eq!(
                dotted(&real.snapshot(), top + 1),
                "job control",
                "saved answer, its marks"
            );
            // A new answer: the whole chat (Alt+P again) or a typed question.
            app.on_input(again, &mut out, &mut child).unwrap();
            if again == b"\x1bq" {
                app.on_input(b"why?\r", &mut out, &mut child).unwrap();
            }
            let id = app.open.as_ref().unwrap().id;
            app.on_progress(
                id,
                Progress::Delta("Something else entirely, not about jobs at all.".into()),
                &mut out,
            )
            .unwrap();
            app.on_idle(&mut out, &mut child).unwrap();
            real.advance(&std::mem::take(&mut out));
            let snap = real.snapshot();
            let top = app.open.as_ref().unwrap().lay.box_top as usize;
            let h = app.open.as_ref().unwrap().lay.box_height;
            for r in top..top + h {
                assert_eq!(
                    dotted(&snap, r),
                    "",
                    "old inner mark on row {r} ({:?})",
                    again
                );
            }
        }
    }

    /// An answer to a typed question shows the question above it, also when
    /// opened again from its mark, and a peek inside it keeps its line on
    /// the right words (the question is not part of the answer's text).
    #[test]
    fn a_saved_question_answer_keeps_its_inner_marks_in_place() {
        let mut app = test_app(40, 24);
        app.selection = SelectionSource::fixed("true colour");
        let (mut child, mut out) = (Vec::new(), Vec::new());
        app.on_output(SCREEN, &mut out, &mut child).unwrap();
        app.on_output(b"\x1b[?1000h\x1b[?1003h\x1b[?1006h", &mut out, &mut child)
            .unwrap();
        app.on_input(b"\x1bq", &mut out, &mut child).unwrap();
        app.on_input(b"why?\r", &mut out, &mut child).unwrap();
        let id = app.open.as_ref().unwrap().id;
        let answer = "A pseudo-terminal lets job control work inside tmux panes.";
        app.on_progress(id, Progress::Delta(answer.into()), &mut out)
            .unwrap();
        app.on_progress(id, Progress::Done, &mut out).unwrap();
        // The answer's first line is under the question line and a blank one.
        let top = app.open.as_ref().unwrap().lay.box_top as usize;
        let at = |col: usize| (top + 3, 2 + col);
        let mut drag = mouse(0, at(23), true);
        drag.extend(mouse(32, at(33), true));
        drag.extend(mouse(0, at(33), false));
        app.on_input(&drag, &mut out, &mut child).unwrap();
        app.on_input(b"\x1bp", &mut out, &mut child).unwrap();
        let n = &app.open.as_ref().unwrap().nested[0];
        assert_eq!(n.words, "job control");
        let id = n.id;
        app.on_progress(id, Progress::Delta("Ctrl+Z.".into()), &mut out)
            .unwrap();
        app.on_progress(id, Progress::Done, &mut out).unwrap();
        app.on_input(b"\x1b\x1b", &mut out, &mut child).unwrap();
        app.on_input(b"\x1b", &mut out, &mut child).unwrap();
        assert!(app.open.is_none());
        // Open the saved answer again.
        app.selection = SelectionSource::fixed("true colour");
        out.clear();
        app.on_input(b"\x1bp", &mut out, &mut child).unwrap();
        let open = app.open.as_ref().unwrap();
        assert_eq!(open.peek.question.as_deref(), Some("why?"));
        assert_eq!(open.peek.text, answer);
        let mut real = Shadow::new(40, 24);
        real.advance(SCREEN);
        real.advance(&out);
        let top = open.lay.box_top as usize;
        assert_eq!(dotted(&real.snapshot(), top + 3), "job control");
    }
}
