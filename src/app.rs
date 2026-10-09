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
    /// History size when the snapshot was taken (rows of marks count from it).
    history: usize,
    /// Boxes opened inside this one, outermost first.
    nested: Vec<nest::Nested>,
    /// The box's height before boxes opened inside it, to go back to.
    base_height: usize,
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
    /// What the live rows currently show on the terminal.
    strip_shown: Vec<Vec<Cell>>,
    strip_dirty: bool,
    /// Streamed text is drawn at most every FRAME; `dirty` marks text not shown yet.
    drawn_at: Instant,
    dirty: bool,
}

/// Mouse reports (button events, SGR form) peekme turns on while a box is open
/// over a child that has them off, so text in the box can be selected.
const MOUSE_ON: &str = "\x1b[?1002h\x1b[?1006h";
const MOUSE_OFF: &str = "\x1b[?1002l";
const SGR_OFF: &str = "\x1b[?1006l";
/// Reports still on their way after peekme turned them off are dropped for
/// this long, so a child that never asked for them gets none.
const STRAY_MOUSE: Duration = Duration::from_secs(1);

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
    let (r, c) = cell;
    let (top, bottom) = lay.region;
    let in_box = (lay.box_top..lay.box_top + lay.box_height).contains(&r);
    Some(match () {
        _ if in_box => return None,
        _ if !(top..bottom).contains(&r) => (r, c),
        _ if lay.below => (r - lay.box_height, c),
        _ => (r + lay.box_height, c),
    })
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
    /// Alt+Shift+P: the box asks for a question first.
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
    event_loop(setup, agent).map_err(Failure::Runtime)
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

fn event_loop(setup: Setup, agent: Option<Agent>) -> Result<i32> {
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
        told_hidden: false,
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
    /// Told the user once that the terminal keeps the mouse (tmux mouse off, or
    /// a terminal that never reports it), so no selection can be seen.
    told_hidden: bool,
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
    /// The next box asks for a question instead of explaining (Alt+Shift+P).
    ask_next: bool,
    /// Cells of the mark under the pointer, drawn with a solid underline.
    hover: Option<Vec<(usize, usize)>>,
    /// peekme set the pointer shape (OSC 22) to a hand.
    pointer_on: bool,
    /// The child's last pointer shape request, to give it back.
    child_pointer: Option<Vec<u8>>,
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
            self.shadow.advance(bytes);
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
                // The real terminal won't see these bytes yet, so the shadow answers.
                let replies = self.shadow.take_replies();
                if !replies.is_empty() {
                    child.write_all(&replies)?;
                    child.flush()?;
                }
                match open.strip_top {
                    Some(top) => {
                        open.strip_dirty = true;
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
    fn refresh_strip(&mut self, out: &mut dyn Write) -> Result<()> {
        let Some(open) = &mut self.open else {
            return Ok(());
        };
        let Some(top) = open.strip_top else {
            return Ok(());
        };
        let now = self.shadow.snapshot();
        // If the input area grew into the box (an approval question, say), the
        // child needs the room more than we do. Claude replaces its input box
        // with the question, so there any change of the box counts.
        let moved = match self.agent {
            Some(Agent::Claude | Agent::Copilot) => claude::screen::composer_top(&now) != Some(top),
            Some(Agent::Kiro) => kiro::screen::composer_top(&now) != Some(top),
            _ => {
                self.shadow.alt_screen()
                    && codex::screen::composer_top(&now).is_some_and(|k| k < top)
            }
        };
        if moved {
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
                    render::rows(&mut s, top, &open.snap.rows[top..]);
                }
                s.push_str(&overlay::close_frame(&open.snap, &open.lay));
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
            if matches!(t.kind, Kind::OptionP | Kind::OptionShiftP) {
                let ask = t.kind == Kind::OptionShiftP;
                let mut hotkey = self.option_p_is_hotkey();
                if !hotkey && !self.greek_typed && self.hotkey_follows_drag() {
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
                    && !self.greek_typed
                    && !self.told_hidden
                    && (self.selection_hidden()
                        || (cfg!(target_os = "macos") && self.mouse_withheld()))
                {
                    // Option+P can never find a selection here: say why, once,
                    // instead of typing π without a word.
                    hotkey = true;
                    self.told_hidden = true;
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
                        // nothing, Alt+Shift+P asks about the innermost box's words.
                        if self.open.as_ref().is_some_and(|o| !o.nested.is_empty()) {
                            if ask {
                                self.ask_in_nested(out)?;
                            }
                            continue;
                        }
                        // Alt+Shift+P in a box: a question about its selection.
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
                (Kind::Esc, true) => {
                    self.consumed.consume(&t);
                    if !self.close_innermost(out)? {
                        self.close(out, false)?;
                    }
                }
                (Kind::PageUp | Kind::PageDown, true) => {
                    self.consumed.consume(&t);
                    self.scroll(t.kind == Kind::PageDown, out)?;
                }
                (Kind::Passive | Kind::Release, _) => forward.extend_from_slice(&t.bytes),
                // With the box open: moving the mouse does nothing, the wheel
                // scrolls the box, a click elsewhere closes it (and still goes
                // to the child, e.g. to start a new selection).
                (Kind::Mouse(m), true) => match m {
                    Mouse::Motion => {}
                    Mouse::WheelUp | Mouse::WheelDown => self.scroll(m == Mouse::WheelDown, out)?,
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
        self.note_mouse(Mouse::Press, bytes);
        forward.extend_from_slice(bytes);
        Ok(())
    }

    /// The mark at screen `cell`, if a click there is peekme's: the child
    /// takes the mouse, and the cell is not a link (links are the agent's).
    /// With a box open, only the mark of that box counts (a click closes it).
    fn mark_hit(&mut self, cell: (usize, usize)) -> Option<MarkHit> {
        if self.marks.is_empty() || !self.shadow.mouse_mode() {
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

    /// Whether `π` (Option+P on a Mac) means "explain": when a box is open or
    /// a selection is on screen. Otherwise it is typed, so nobody loses the
    /// letter, and never once the user types Greek. Nothing else can tell a
    /// Mac keyboard apart: over SSH and in tmux the environment says nothing.
    fn option_p_is_hotkey(&mut self) -> bool {
        if self.greek_typed {
            return false;
        }
        if self.open.is_some() {
            return true;
        }
        self.has_selection()
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
            self.selection_source = "system";
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

        let (selection, located) = match self.clicked.take() {
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
        let mut mark = None;
        let mut saved = None;
        let mut replace = None;
        let (mut peek, lay) = match (&selection, located) {
            (Some(sel), Some(loc)) => {
                // Keep the child's live area (input line, status) out of the box's
                // way when we know where it starts and the selection is above it.
                strip = self
                    .live_top(&snap)
                    .filter(|&k| loc.last_row < k && k < rows && k >= rows / 3);
                let mut lay = overlay::layout_within(
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
                    if let Some((place, _)) = &mark {
                        self.marks.add(id, place.clone());
                    }
                    peek.text = answer;
                    peek.model = format!("{model} · saved");
                    peek.status = Status::Done;
                    peek.deep_available = true;
                    lay = overlay::shrink(&lay, peek.fitted_height(snap.cols).max(3));
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
                let msg = if hidden {
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
            }),
        );
        let strip_shown = strip.map_or_else(Vec::new, |top| snap.rows[top..].to_vec());
        // Selecting text in the box needs the mouse reports.
        let own_mouse = !self.shadow.mouse_mode();
        self.open = Some(Open {
            id,
            selection: remembered,
            deep: false,
            mark,
            answer: saved.or(replace),
            history: self.shadow.history_size(),
            nested: Vec::new(),
            base_height: 0,
            drag: None,
            box_hover: None,
            last_click: None,
            parts: Vec::new(),
            own_mouse,
            snap,
            lay,
            peek,
            held: Vec::new(),
            strip_top: strip,
            strip_shown,
            strip_dirty: false,
            drawn_at: Instant::now(),
            dirty: false,
        });
        let mut frame = self.box_bytes(true);
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
        let max_lines = open.lay.box_height.saturating_sub(2).clamp(3, 12);
        crate::event("escalate", serde_json::json!({"selection": sel}));
        self.next_id += 1;
        let id = self.next_id;
        let err = self.start_explain(id, screen, max_lines, true, force, None, None);
        let open = self.open.as_mut().unwrap();
        open.id = id;
        open.deep = true;
        open.peek.text.clear();
        open.peek.scroll = 0;
        open.peek.deep_available = false;
        open.peek.confirm_deep = false;
        open.peek.status = match err {
            Some(e) => Status::Error(e),
            None => Status::Thinking,
        };
        let frame = self.box_bytes(false);
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
            b"\r" | b"\n" => submit = true,
            b"\x7f" | b"\x08" => {
                input.pop();
            }
            // Ctrl+U clears, Ctrl+W takes the last word.
            b"\x15" => input.clear(),
            b"\x17" => {
                let keep = input.trim_end().rfind(' ').map_or(0, |i| i + 1);
                input.truncate(keep);
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
            bytes => {
                let text = String::from_utf8_lossy(bytes);
                input.extend(text.chars().filter(|c| !c.is_control()));
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
        let max_lines = open.lay.box_height.saturating_sub(2).max(3);
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

    /// Alt+Shift+P with a box open: ask about the words that box explains,
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
        let Some(open) = self.open.take() else {
            return Ok(());
        };
        self.selection.settle();
        crate::event("close", serde_json::json!({"held_bytes": open.held.len()}));
        let mut bytes = Vec::new();
        bytes.extend_from_slice(SYNC_BEGIN.as_bytes());
        if !skip_repaint {
            // The live rows show newer content than the snapshot; put them back
            // too, so replaying the held output starts from the state it expects.
            let mut s = String::new();
            if let Some(top) = open.strip_top {
                render::rows(&mut s, top, &open.snap.rows[top..]);
            }
            s.push_str(&overlay::close_frame(&open.snap, &open.lay));
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
        let held = render::strip_answered_queries(&open.held);
        self.boundary.feed(&held);
        bytes.extend_from_slice(&held);
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
                    // A typed question is saved with its answer, so a click
                    // on the words shows both.
                    let answer = match &open.peek.question {
                        Some(q) => format!(
                            "› {}\n\n{}",
                            overlay::sanitize(q),
                            overlay::sanitize(&open.peek.text)
                        ),
                        None => overlay::sanitize(&open.peek.text),
                    };
                    let id = match open.answer {
                        // Explained again with the whole chat.
                        Some(id) => {
                            self.marks.replace(id, &answer, &open.peek.model);
                            id
                        }
                        None => self.marks.save(None, sel, &answer, &open.peek.model),
                    };
                    open.answer = Some(id);
                    self.marks.add(id, place.clone());
                    crate::event(
                        "mark",
                        serde_json::json!({"selection": sel, "cells": cells.len()}),
                    );
                    // Underline it right away if the box leaves its rows alone.
                    let (top, bottom) = open.lay.region;
                    let box_rows = open.lay.box_top..open.lay.box_top + open.lay.box_height;
                    if cells
                        .iter()
                        .all(|&(r, _)| !(top..bottom).contains(&r) && !box_rows.contains(&r))
                    {
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
                // Give back the rows the text doesn't need, once, now that it is complete.
                let fitted = open.peek.fitted_height(open.snap.cols).max(3);
                if fitted < open.lay.box_height {
                    // Same region, smaller box: one frame redraws every row of it.
                    open.lay = overlay::shrink(&open.lay, fitted);
                    let frame = self.box_bytes(true);
                    out.write_all(frame.as_bytes())?;
                    out.flush()?;
                    return Ok(());
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
        let frame = self.box_bytes(false);
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
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn types_greek(bytes: &[u8]) -> bool {
    String::from_utf8_lossy(bytes).chars().any(|c| {
        matches!(c, '\u{0370}'..='\u{03ff}' | '\u{1f00}'..='\u{1fff}') && c != input::MAC_OPTION_P
    })
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
        box_top: top,
        box_height: h,
        region: (top, top + h),
        below: true,
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
            let small = overlay::shrink(&lay, 4);
            real.advance(overlay::open_frame(&snap, &small, &peek).as_bytes());
            real.advance(overlay::close_frame(&snap, &small).as_bytes());
            assert!(
                same_screen(&real.snapshot(), &snap),
                "not restored after shrinking"
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
        let top = app.open.as_ref().unwrap().lay.box_top;
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
        let top = app.open.as_ref().unwrap().lay.box_top;
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
        let top = app.open.as_ref().unwrap().lay.box_top;
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
        let top = app.open.as_ref().unwrap().lay.box_top;
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
            told_hidden: false,
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
        assert!(open.lay.below && open.lay.box_top > 0);
        let above = open.lay.box_top;

        // Mouse motion is ignored, the wheel scrolls the box, nothing reaches Codex.
        app.on_input(b"\x1b[<35;40;3M\x1b[<65;40;3M", &mut out, &mut child)
            .unwrap();
        assert!(child.is_empty() && app.open.is_some());
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
        assert_eq!(open.lay.box_top, 10, "box right under the selected row");
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
        assert_eq!(open.lay.box_top, 22, "right under the selected row");
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
        assert_eq!(open.lay.box_top + open.lay.box_height, 26);
        assert_eq!(open.peek.child_name, "Kiro");
        assert_eq!(open.strip_top, Some(31), "Kiro's input area stays live");

        app.on_input(b"\x1b", &mut out, &mut child).unwrap();
        real.advance(&std::mem::take(&mut out));
        real.flush_sync();
        assert!(same_screen(&real.snapshot(), &app.shadow.snapshot()));
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
            open.lay.box_top + open.lay.box_height,
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

    /// Alt+Shift+P: the box asks for a question first; Enter sends it with
    /// the selection, and the answer is saved with the question.
    #[test]
    fn alt_shift_p_asks_a_typed_question() {
        let mut app = test_app(40, 24);
        let (tx, rx) = mpsc::channel();
        app.tx = tx;
        app.explainer = Some(Explainer::Fake);
        app.selection = SelectionSource::fixed("true colour");
        let (mut out, mut child) = (Vec::new(), Vec::new());
        app.on_output(SCREEN, &mut out, &mut child).unwrap();
        app.on_input(b"\x1bP", &mut out, &mut child).unwrap();
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
        assert!(
            saved.is_some_and(|a| a.answer.starts_with("› why so π bright\n\n")),
            "the question is saved with the answer"
        );
        assert!(child.is_empty());
    }

    /// Alt+Shift+P in a box that explained something: a new box asks about
    /// the same words; Esc while typing closes it and nothing is sent.
    #[test]
    fn alt_shift_p_in_a_box_asks_about_its_words() {
        let mut app = app_with_an_answer("short", false);
        let (mut out, mut child) = (Vec::new(), Vec::new());
        app.on_input(b"\x1bP", &mut out, &mut child).unwrap();
        let open = app.open.as_ref().expect("a box");
        assert_eq!(open.peek.input.as_deref(), Some(""));
        assert_eq!(open.peek.title, "true colour");
        assert!(
            open.lay.box_height > 3,
            "full size again, not the shrunk box"
        );
        app.on_input(b"x", &mut out, &mut child).unwrap();
        app.on_input(b"\x1b", &mut out, &mut child).unwrap();
        assert!(app.open.is_none());
        assert!(child.is_empty());
    }

    /// Alt+Shift+P on words in a box: a box inside it waits for a question;
    /// it is sent with the answers around it, and saved with the question.
    /// Alt+Shift+P with nothing selected asks again about the same words.
    #[test]
    fn alt_shift_p_on_words_in_a_box_asks_about_them() {
        let answer = "A pseudo-terminal lets job control work inside tmux panes.";
        let mut app = app_with_an_answer(answer, true);
        let (tx, rx) = mpsc::channel();
        app.tx = tx;
        app.explainer = Some(Explainer::Fake);
        let (mut child, mut out) = (Vec::new(), Vec::new());
        let top = app.open.as_ref().unwrap().lay.box_top;
        let at = |col: usize| (top + 1, 2 + col);
        let mut drag = mouse(0, at(23), true);
        drag.extend(mouse(32, at(33), true));
        drag.extend(mouse(0, at(33), false));
        app.on_input(&drag, &mut out, &mut child).unwrap();
        app.on_input(b"\x1bP", &mut out, &mut child).unwrap();
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
        assert!(saved.answer.starts_with("› how does fg work?\n\n"));

        // Nothing selected now: Alt+Shift+P asks about the same words again.
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
}
