//! The wrapper: run the child in a PTY, pass its output through untouched,
//! and open peek boxes in place on request.

use std::io::{Read, Write};
use std::sync::Arc;
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, PtySize, native_pty_system};

use crate::explain::{self, AppServer, Progress};
use crate::input::{self, Consumed, Kind, Token};
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
    AppServer(Result<Arc<AppServer>, String>),
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
}

/// A hotkey press waiting for the child to finish a synchronized update.
struct PendingOpen {
    since: Instant,
}

pub fn run(program: &str, args: &[String]) -> Result<i32> {
    let (cols, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let pty = native_pty_system();
    let pair = pty
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("could not open a pseudo-terminal")?;
    let mut cmd = CommandBuilder::new(program);
    cmd.args(args);
    cmd.cwd(std::env::current_dir()?);
    let mut child = pair
        .slave
        .spawn_command(cmd)
        .with_context(|| format!("could not start `{program}`"))?;
    drop(pair.slave);
    let mut reader = pair.master.try_clone_reader()?;
    let mut writer = pair.master.take_writer()?;
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
    // Window size changes.
    {
        let tx = tx.clone();
        let mut signals = signal_hook::iterator::Signals::new([signal_hook::consts::SIGWINCH])?;
        std::thread::spawn(move || {
            for _ in signals.forever() {
                if tx.send(Msg::Resize).is_err() {
                    break;
                }
            }
        });
    }
    // Explainer backend, started in the background so the child starts instantly.
    {
        let tx = tx.clone();
        std::thread::spawn(move || {
            let r = AppServer::start().map_err(|e| format!("{e:#}"));
            let _ = tx.send(Msg::AppServer(r));
        });
    }

    let mut app = App {
        shadow: Shadow::new(cols, rows),
        open: None,
        pending: None,
        next_id: 0,
        consumed: Consumed::default(),
        carry: Vec::new(),
        in_paste: false,
        selection: SelectionSource::new(),
        server: None,
        server_error: None,
        tx: tx.clone(),
        cwd: std::env::current_dir()?.to_string_lossy().into_owned(),
    };
    let mut stdout = std::io::stdout().lock();

    loop {
        let msg = match rx.recv_timeout(Duration::from_millis(40)) {
            Ok(m) => m,
            Err(RecvTimeoutError::Timeout) => {
                app.tick(&mut stdout, &mut *writer)?;
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => break,
        };
        match msg {
            Msg::Output(bytes) => app.on_output(&bytes, &mut stdout, &mut *writer)?,
            Msg::Input(bytes) => app.on_input(&bytes, &mut stdout, &mut *writer)?,
            Msg::Resize => {
                let (c, r) = crossterm::terminal::size().unwrap_or((cols, rows));
                let _ = master.resize(PtySize {
                    rows: r,
                    cols: c,
                    pixel_width: 0,
                    pixel_height: 0,
                });
                app.on_resize(c, r, &mut stdout)?;
            }
            Msg::Peek(id, p) => app.on_progress(id, p, &mut stdout)?,
            Msg::AppServer(r) => match r {
                Ok(s) => app.server = Some(s),
                Err(e) => app.server_error = Some(e),
            },
            Msg::ChildGone => break,
        }
    }

    if app.open.is_some() {
        app.close(&mut stdout, true)?;
    }
    if let Some(s) = &app.server {
        s.shutdown();
    }
    let status = child.wait()?;
    Ok(status.exit_code() as i32)
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
    server: Option<Arc<AppServer>>,
    server_error: Option<String>,
    tx: Sender<Msg>,
    cwd: String,
}

impl App {
    fn on_output(
        &mut self,
        bytes: &[u8],
        out: &mut impl Write,
        child: &mut dyn Write,
    ) -> Result<()> {
        self.shadow.advance(bytes);
        match &mut self.open {
            None => {
                out.write_all(bytes)?;
                out.flush()?;
                // The real terminal answers the child's queries; drop the shadow's copies.
                self.shadow.take_replies();
            }
            Some(open) => {
                open.held.extend_from_slice(bytes);
                open.peek.waiting_updates += 1;
                // The real terminal won't see these bytes yet, so the shadow answers.
                let replies = self.shadow.take_replies();
                if !replies.is_empty() {
                    child.write_all(&replies)?;
                    child.flush()?;
                }
            }
        }
        if self.pending.is_some() && !self.shadow.in_sync_update() {
            self.pending = None;
            self.open_peek(out)?;
        }
        Ok(())
    }

    fn tick(&mut self, out: &mut impl Write, _child: &mut dyn Write) -> Result<()> {
        self.shadow.check_sync_timeout();
        if let Some(p) = &self.pending
            && (!self.shadow.in_sync_update() || p.since.elapsed() > Duration::from_millis(200))
        {
            self.pending = None;
            self.open_peek(out)?;
        }
        Ok(())
    }

    fn on_input(
        &mut self,
        bytes: &[u8],
        out: &mut impl Write,
        child: &mut dyn Write,
    ) -> Result<()> {
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
                (_, true) => {
                    // Any other key closes the box and goes to the child as usual.
                    self.close(out, false)?;
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

    fn request_open(&mut self, out: &mut impl Write) -> Result<()> {
        if self.shadow.in_sync_update() {
            // Never cut into the child's half-finished frame.
            self.pending = Some(PendingOpen {
                since: Instant::now(),
            });
            return Ok(());
        }
        self.open_peek(out)
    }

    fn open_peek(&mut self, out: &mut impl Write) -> Result<()> {
        // If the child's frame never finished, show what a terminal without
        // synchronized output already shows: everything received so far.
        if self.shadow.in_sync_update() {
            self.shadow.flush_sync();
        }
        let snap = self.shadow.snapshot();
        let rows = snap.rows.len();
        self.next_id += 1;
        let id = self.next_id;

        let selection = self.selection.read();
        let located = selection.as_deref().and_then(|s| select::locate(&snap, s));
        let mut remembered = None;
        let (peek, lay) = match (&selection, located) {
            (Some(sel), Some(loc)) => {
                let lay = overlay::layout(rows, loc.first_row, loc.last_row);
                let mut peek = PeekBox::new(sel);
                let max_lines = lay.box_height.saturating_sub(2).clamp(3, 12);
                if let Some(e) = self.start_explain(id, loc.screen.clone(), max_lines, false) {
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

        let frame = overlay::open_frame(&snap, &lay, &peek);
        out.write_all(frame.as_bytes())?;
        out.flush()?;
        self.open = Some(Open {
            id,
            selection: remembered,
            deep: false,
            snap,
            lay,
            peek,
            held: Vec::new(),
        });
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
    ) -> Option<String> {
        match (&self.server, &self.server_error) {
            (Some(server), _) => {
                let req = explain::Request {
                    screen,
                    cwd: self.cwd.clone(),
                    max_lines,
                    deep,
                };
                let server = server.clone();
                let tx = self.tx.clone();
                std::thread::spawn(move || {
                    explain::explain(&server, req, |p| {
                        let _ = tx.send(Msg::Peek(id, p));
                    })
                });
                None
            }
            (None, Some(e)) => Some(format!("explainer unavailable: {e}")),
            (None, None) => Some("explainer is still starting, try again".into()),
        }
    }

    /// If the open box explains the current selection, re-explain it with the
    /// whole conversation in the same box. Returns false when that doesn't apply.
    fn escalate(&mut self, out: &mut impl Write) -> Result<bool> {
        let current = self.selection.read();
        let Some(open) = &self.open else {
            return Ok(false);
        };
        let Some((sel, screen)) = &open.selection else {
            return Ok(false);
        };
        if open.deep || current.as_deref() != Some(sel.as_str()) {
            return Ok(false);
        }
        let screen = screen.clone();
        let max_lines = open.lay.box_height.saturating_sub(2).clamp(3, 12);
        self.next_id += 1;
        let id = self.next_id;
        let err = self.start_explain(id, screen, max_lines, true);
        let open = self.open.as_mut().unwrap();
        open.id = id;
        open.deep = true;
        open.peek.text.clear();
        open.peek.scroll = 0;
        open.peek.deep_available = false;
        open.peek.status = match err {
            Some(e) => Status::Error(e),
            None => Status::Thinking,
        };
        out.write_all(overlay::box_frame(&open.snap, &open.lay, &open.peek).as_bytes())?;
        out.flush()?;
        Ok(true)
    }

    /// Close the box: repaint the region from the snapshot, then replay held
    /// output so the terminal reaches exactly the child's current state.
    fn close(&mut self, out: &mut impl Write, skip_repaint: bool) -> Result<()> {
        let Some(open) = self.open.take() else {
            return Ok(());
        };
        let mut bytes = Vec::new();
        bytes.extend_from_slice(SYNC_BEGIN.as_bytes());
        if !skip_repaint {
            bytes.extend_from_slice(overlay::close_frame(&open.snap, &open.lay).as_bytes());
        }
        bytes.extend_from_slice(&render::strip_answered_queries(&open.held));
        bytes.extend_from_slice(SYNC_END.as_bytes());
        out.write_all(&bytes)?;
        out.flush()?;
        Ok(())
    }

    fn scroll(&mut self, down: bool, out: &mut impl Write) -> Result<()> {
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
        out.write_all(overlay::box_frame(&open.snap, &open.lay, &open.peek).as_bytes())?;
        out.flush()?;
        Ok(())
    }

    fn on_progress(&mut self, id: u64, p: Progress, out: &mut impl Write) -> Result<()> {
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
            }
            Progress::Failed(e) => open.peek.status = Status::Error(e),
        }
        out.write_all(overlay::box_frame(&open.snap, &open.lay, &open.peek).as_bytes())?;
        out.flush()?;
        Ok(())
    }

    fn on_resize(&mut self, cols: u16, rows: u16, out: &mut impl Write) -> Result<()> {
        // The terminal has already reflowed the box; don't paint old-width rows
        // back. Codex reprints its whole transcript after a resize anyway.
        if self.open.is_some() {
            self.close(out, true)?;
        }
        self.pending = None;
        self.shadow.resize(cols, rows);
        Ok(())
    }
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
}
