//! The wrapper: run the child in a PTY, pass its output through untouched,
//! and open peek boxes in place on request.

use std::collections::VecDeque;
use std::io::{Read, Write};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use alacritty_terminal::term::cell::Cell;
use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::explain::{self, Progress};
use crate::input::{self, Consumed, Kind, Mouse, Token};
use crate::jobctl;
use crate::overlay::{self, Layout, PeekBox, Status};
use crate::render::{self, SYNC_BEGIN, SYNC_END};
use crate::select::{self, SelectionSource};
use crate::shadow::{Shadow, Snapshot};

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
}

/// A hotkey press waiting for the child to finish a synchronized update.
struct PendingOpen {
    since: Instant,
}

pub fn run(program: &str, args: &[String]) -> std::result::Result<i32, Failure> {
    let setup = setup(program, args).map_err(Failure::Setup)?;
    event_loop(setup).map_err(Failure::Runtime)
}

struct Setup {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    helper_pid: Option<u32>,
    master: Box<dyn portable_pty::MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    rx: mpsc::Receiver<Msg>,
    tx: Sender<Msg>,
    cols: u16,
    rows: u16,
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
    let exe = std::env::current_exe().ok();
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
    let helper_pid = exe.and(child.process_id());
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
        master,
        writer,
        rx,
        tx,
        cols,
        rows,
    })
}

fn event_loop(setup: Setup) -> Result<i32> {
    let Setup {
        mut child,
        helper_pid,
        master,
        mut writer,
        rx,
        tx,
        cols,
        rows,
    } = setup;
    let mut app = App {
        shadow: Shadow::new(cols, rows),
        open: None,
        pending: None,
        next_id: 0,
        consumed: Consumed::default(),
        carry: Vec::new(),
        in_paste: false,
        selection: SelectionSource::new(),
        explainer: Some(Arc::new(explain::Slot::default())),
        tx,
        cwd: std::env::current_dir()?.to_string_lossy().into_owned(),
        viewport_top: None,
        degraded: false,
        size: (cols, rows),
    };
    let mut stdout = std::io::stdout().lock();
    let mut queue: VecDeque<Msg> = VecDeque::new();

    loop {
        let msg = match queue.pop_front() {
            Some(m) => m,
            None => match rx.recv_timeout(Duration::from_millis(40)) {
                Ok(m) => m,
                Err(RecvTimeoutError::Timeout) => {
                    guarded(&mut app, &mut stdout, |app, out| app.tick(out))?;
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
    if let Some(slot) = &app.explainer {
        slot.shutdown();
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
    /// The explainer connection, made on the first Alt+P (None in tests).
    explainer: Option<Arc<explain::Slot>>,
    tx: Sender<Msg>,
    cwd: String,
    /// Where the child's live area starts, learned from its scroll regions.
    viewport_top: Option<usize>,
    /// Set after a panic in the peek code: from then on, pass everything through.
    degraded: bool,
    size: (u16, u16),
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
        if self.open.is_none() {
            // Pass through first: whatever happens below, the user sees the child.
            out.write_all(bytes)?;
            out.flush()?;
        }
        if let Some(k) = viewport_top(bytes, self.size.1 as usize) {
            self.viewport_top = Some(k);
        }
        self.shadow.advance(bytes);
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
        if self.pending.is_some() && !self.shadow.in_sync_update() {
            self.pending = None;
            self.open_peek(out)?;
        }
        Ok(())
    }

    /// First row of the child's live area: from its scroll regions when it draws
    /// inline, from the position of its input box when it draws full screen.
    fn live_top(&self, snap: &Snapshot) -> Option<usize> {
        if self.shadow.alt_screen() {
            select::composer_top(snap)
        } else {
            self.viewport_top.or_else(|| select::composer_top(snap))
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
        // Full screen: if the input area grew into the box (an approval
        // question, say), the child needs the room more than we do.
        if self.shadow.alt_screen() && select::composer_top(&now).is_some_and(|k| k < top) {
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
            && (!self.shadow.in_sync_update() || p.since.elapsed() > Duration::from_millis(200))
        {
            self.pending = None;
            self.open_peek(out)?;
        }
        Ok(())
    }

    fn on_input(&mut self, bytes: &[u8], out: &mut dyn Write, child: &mut dyn Write) -> Result<()> {
        let tokens = input::tokenize(bytes, &mut self.carry, &mut self.in_paste);
        let mut forward: Vec<u8> = Vec::new();
        for t in tokens {
            self.sniff_colors(&t);
            if matches!(t.bytes.as_slice(), b"\x1b[I" | b"\x1b[O") {
                self.consumed.clear();
            }
            // Repeats and releases of keys we consumed never reach the child.
            if t.kind != Kind::Passive && self.consumed.swallow(&t) {
                continue;
            }
            match (t.kind, self.open.is_some()) {
                (Kind::Hotkey, _) => {
                    self.consumed.consume(&t);
                    if self.open.is_some() {
                        self.flush_forward(&mut forward, child)?;
                        // Alt+P again on the same selection: explain with the whole conversation.
                        if self.escalate(out)? {
                            continue;
                        }
                        self.close(out, false)?;
                    }
                    self.request_open(out)?;
                }
                (Kind::Esc, true) => {
                    self.consumed.consume(&t);
                    self.close(out, false)?;
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
                        forward.extend_from_slice(&t.bytes);
                    }
                    Mouse::Release => forward.extend_from_slice(&t.bytes),
                },
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

    fn request_open(&mut self, out: &mut dyn Write) -> Result<()> {
        if self.shadow.in_sync_update() {
            // Never cut into the child's half-finished frame.
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

        // Codex's own selection (full-screen mode highlights it in reverse video)
        // wins; otherwise the terminal's mouse selection (X11 PRIMARY).
        let (selection, located) = match select::codex_selection(&snap) {
            Some(loc) => (Some(loc.screen.selected()), Some(loc)),
            None => {
                let s = self.selection.read();
                let l = s.as_deref().and_then(|s| select::locate(&snap, s));
                (s, l)
            }
        };
        let mut remembered = None;
        let mut strip = None;
        let (peek, lay) = match (&selection, located) {
            (Some(sel), Some(loc)) => {
                // Keep the child's live area (input line, status) out of the box's
                // way when we know where it starts and the selection is above it.
                strip = self
                    .live_top(&snap)
                    .filter(|&k| loc.last_row < k && k < rows && k >= rows / 3);
                let lay = overlay::layout(strip.unwrap_or(rows), loc.first_row, loc.last_row);
                let mut peek = PeekBox::new(sel);
                let max_lines = lay.box_height.saturating_sub(2).clamp(3, 12);
                if let Some(e) = self.start_explain(id, loc.screen.clone(), max_lines, false, false)
                {
                    peek.status = Status::Error(e);
                }
                remembered = Some((sel.clone(), loc.screen));
                (peek, lay)
            }
            (None, _) => {
                let msg = "Select some text with the mouse first, then press Alt+P.";
                (PeekBox::message(msg), message_layout(&snap))
            }
            (Some(_), None) => {
                let msg = "The selected text isn't on screen (only visible text can be peeked in this version).";
                (PeekBox::message(msg), message_layout(&snap))
            }
        };

        let mut frame = overlay::open_frame(&snap, &lay, &peek);
        if let Some(top) = strip {
            frame.push_str(&strip_cursor(&snap, top));
        }
        out.write_all(frame.as_bytes())?;
        out.flush()?;
        let strip_shown = strip.map_or_else(Vec::new, |top| snap.rows[top..].to_vec());
        self.open = Some(Open {
            id,
            selection: remembered,
            deep: false,
            snap,
            lay,
            peek,
            held: Vec::new(),
            strip_top: strip,
            strip_shown,
            strip_dirty: false,
        });
        #[cfg(debug_assertions)]
        if panic_after_open {
            panic!("panic requested by PEEKME_TEST_PANIC");
        }
        Ok(())
    }

    /// Start an explanation in the background; returns an error message if the
    /// explainer isn't available.
    fn start_explain(
        &self,
        id: u64,
        screen: crate::context::ScreenSel,
        max_lines: usize,
        deep: bool,
        force: bool,
    ) -> Option<String> {
        let Some(slot) = self.explainer.clone() else {
            return Some("explainer disabled".into());
        };
        let req = explain::Request {
            screen,
            cwd: self.cwd.clone(),
            max_lines,
            deep,
            force,
        };
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let send = |p| {
                let _ = tx.send(Msg::Peek(id, p));
            };
            let run = std::panic::catch_unwind(AssertUnwindSafe(|| match slot.get() {
                Ok(server) => explain::explain(&server, req, send),
                Err(e) => send(Progress::Failed(format!("explainer unavailable: {e:#}"))),
            }));
            if run.is_err() {
                let _ = tx.send(Msg::Peek(id, Progress::Failed("internal error".into())));
            }
        });
        None
    }

    /// If the open box explains the current selection, re-explain it with the
    /// whole conversation in the same box. Returns false when that doesn't apply.
    fn escalate(&mut self, out: &mut dyn Write) -> Result<bool> {
        let current = self.selection.read();
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
        self.next_id += 1;
        let id = self.next_id;
        let err = self.start_explain(id, screen, max_lines, true, force);
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
        let mut frame = overlay::box_frame(&open.snap, &open.lay, &open.peek);
        if let Some(top) = open.strip_top {
            frame.push_str(&strip_cursor(&self.shadow.snapshot(), top));
        }
        out.write_all(frame.as_bytes())?;
        out.flush()?;
        Ok(true)
    }

    /// Close the box: repaint the region from the snapshot, then replay held
    /// output so the terminal reaches exactly the child's current state.
    fn close(&mut self, out: &mut dyn Write, skip_repaint: bool) -> Result<()> {
        let Some(open) = self.open.take() else {
            return Ok(());
        };
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
        bytes.extend_from_slice(&render::strip_answered_queries(&open.held));
        bytes.extend_from_slice(SYNC_END.as_bytes());
        out.write_all(&bytes)?;
        out.flush()?;
        Ok(())
    }

    fn scroll(&mut self, down: bool, out: &mut dyn Write) -> Result<()> {
        let Some(open) = &mut self.open else {
            return Ok(());
        };
        let step = open.lay.box_height.saturating_sub(3).max(1);
        let max = open.peek.max_scroll(open.snap.cols, open.lay.box_height);
        open.peek.scroll = if down {
            (open.peek.scroll + step).min(max)
        } else {
            open.peek.scroll.saturating_sub(step)
        };
        let mut frame = overlay::box_frame(&open.snap, &open.lay, &open.peek);
        if let Some(top) = open.strip_top {
            frame.push_str(&strip_cursor(&self.shadow.snapshot(), top));
        }
        out.write_all(frame.as_bytes())?;
        out.flush()?;
        Ok(())
    }

    fn on_progress(&mut self, id: u64, p: Progress, out: &mut dyn Write) -> Result<()> {
        let Some(open) = &mut self.open else {
            return Ok(());
        };
        if open.id != id {
            return Ok(());
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
                open.peek.deep_available = !open.deep;
                // Give back the rows the text doesn't need, once, now that it is complete.
                let fitted = open.peek.fitted_height(open.snap.cols).max(3);
                if fitted < open.lay.box_height {
                    // Same region, smaller box: one frame redraws every row of it.
                    open.lay = overlay::shrink(&open.lay, fitted);
                    let mut frame = overlay::open_frame(&open.snap, &open.lay, &open.peek);
                    if let Some(top) = open.strip_top {
                        frame.push_str(&strip_cursor(&self.shadow.snapshot(), top));
                    }
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
        let mut frame = overlay::box_frame(&open.snap, &open.lay, &open.peek);
        if let Some(top) = open.strip_top {
            frame.push_str(&strip_cursor(&self.shadow.snapshot(), top));
        }
        out.write_all(frame.as_bytes())?;
        out.flush()?;
        Ok(())
    }

    fn on_resize(&mut self, cols: u16, rows: u16, out: &mut dyn Write) -> Result<()> {
        // The terminal has already reflowed the box; don't paint old-width rows
        // back. Codex reprints its whole transcript after a resize anyway.
        if self.open.is_some() {
            self.close(out, true)?;
        }
        self.pending = None;
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

/// Where to show a one-line message: just above the cursor's row.
fn message_layout(snap: &Snapshot) -> Layout {
    let rows = snap.rows.len();
    let h = 3.min(rows);
    let top = snap.cursor.0.saturating_sub(h);
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
        let (tx, _rx) = mpsc::channel();
        App {
            shadow: Shadow::new(cols, rows),
            open: None,
            pending: None,
            next_id: 0,
            consumed: Consumed::default(),
            carry: Vec::new(),
            in_paste: false,
            selection: SelectionSource::new(),
            explainer: None,
            tx,
            cwd: "/tmp".into(),
            viewport_top: None,
            degraded: false,
            size: (cols, rows),
        }
    }

    /// Drive the real App code over real Codex output: open a box with the live
    /// strip, let Codex draw and type under it, then close with Enter. The
    /// emulated terminal must end up identical to the child's own screen.
    #[test]
    fn live_strip_round_trip() {
        // SAFETY: only this test reads the variable.
        unsafe { std::env::set_var("PEEKME_SELECTION", "Collaboration mode") };
        let mut app = test_app(120, 40);
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

    #[test]
    fn fullscreen_codex_selection_and_live_area() {
        let mut app = test_app(120, 40);
        let mut real = Shadow::new(120, 40);
        let mut child: Vec<u8> = Vec::new();
        let mut out: Vec<u8> = Vec::new();

        app.on_output(CODEX_FULLSCREEN, &mut out, &mut child)
            .unwrap();
        app.shadow.flush_sync();
        real.advance(&std::mem::take(&mut out));
        real.flush_sync();
        assert!(app.shadow.alt_screen());

        app.open_peek(&mut out).unwrap();
        real.advance(&std::mem::take(&mut out));
        let open = app.open.as_ref().unwrap();
        assert_eq!(open.peek.title, "83, 6, 9", "Codex's own selection is used");
        let top = open.strip_top.expect("input area found on the full screen");
        assert!(top > 11 && top < 40);

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
        // A click closes the box and still reaches Codex.
        app.on_input(b"\x1b[<0;5;5M", &mut out, &mut child).unwrap();
        assert!(app.open.is_none());
        assert!(child.ends_with(b"\x1b[<0;5;5M"));
        real.advance(&std::mem::take(&mut out));
        real.flush_sync();
        assert!(same_screen(&real.snapshot(), &app.shadow.snapshot()));
    }
}
