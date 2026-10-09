//! Peeks inside peeks: a selection in a box opens a box inside that box.
//! Each box scrolls on its own; nested answers are saved under their parent
//! answer and underlined in it like marks on the screen.

use std::ops::Range;

use super::*;
use crate::context::Nested as NestedAsk;
use crate::overlay::{Decor, Level, Pos};

/// A box inside the open box (or inside another nested one).
pub(super) struct Nested {
    /// Id of the request whose answer streams into it.
    pub id: u64,
    /// The words of the parent answer it explains, and where they are in it.
    pub words: String,
    pub range: Range<usize>,
    /// The parent's body line it sits under.
    pub anchor: usize,
    /// Saved answer, once complete.
    pub answer: Option<AnswerId>,
    pub peek: PeekBox,
    /// Rows, borders included.
    pub height: usize,
}

/// Text selected with the mouse in one of the boxes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Drag {
    pub level: usize,
    pub from: Pos,
    pub to: Pos,
    /// The pointer left the press position: a selection, not a click.
    pub moved: bool,
    /// The button is up again.
    pub done: bool,
}

impl Drag {
    fn ordered(&self) -> (Pos, Pos) {
        (self.from.min(self.to), self.from.max(self.to))
    }
}

/// The last click in a box, to tell the second half of a double click.
#[derive(Debug, Clone, Copy)]
pub(super) struct BoxClick {
    at: Instant,
    level: usize,
    pos: Pos,
    /// The click opened a box inside.
    opened: bool,
}

/// A nested answer still streaming after its box closed: saved when complete.
pub(super) struct Orphan {
    parent: AnswerId,
    words: String,
    range: Range<usize>,
    text: String,
    model: String,
}

/// A new level needs this many text columns, and rows.
const MIN_WIDTH: usize = 24;
const MIN_ROWS: usize = 3;

impl App {
    /// The saved answer box `level` shows (0 is the outermost).
    pub(super) fn level_answer(&self, level: usize) -> Option<AnswerId> {
        let open = self.open.as_ref()?;
        match level {
            0 => open.answer,
            k => open.nested.get(k - 1)?.answer,
        }
    }

    fn level_peek(open: &Open, level: usize) -> Option<&PeekBox> {
        match level {
            0 => Some(&open.peek),
            k => open.nested.get(k - 1).map(|n| &n.peek),
        }
    }

    fn level_peek_mut(open: &mut Open, level: usize) -> Option<&mut PeekBox> {
        match level {
            0 => Some(&mut open.peek),
            k => open.nested.get_mut(k - 1).map(|n| &mut n.peek),
        }
    }

    fn level_height(open: &Open, level: usize) -> usize {
        match level {
            0 => open.lay.box_height,
            k => open.nested[k - 1].height,
        }
    }

    /// Bytes that draw the open box with every box inside it: all of its
    /// region when `full` (the box changed size), else only the box rows.
    pub(super) fn box_bytes(&mut self, full: bool) -> String {
        let Some(open) = &self.open else {
            return String::new();
        };
        let decors: Vec<Decor> = (0..=open.nested.len())
            .map(|k| Decor {
                marks: self
                    .level_answer(k)
                    .map(|id| {
                        self.marks
                            .child_ranges(id)
                            .into_iter()
                            .map(|(r, _)| r)
                            .collect()
                    })
                    .unwrap_or_default(),
                hover: open
                    .box_hover
                    .as_ref()
                    .filter(|(l, _)| *l == k)
                    .map(|(_, r)| r.clone()),
                selection: open
                    .drag
                    .filter(|d| d.level == k && d.moved)
                    .map(|d| d.ordered()),
            })
            .collect();
        let anchor = |k: usize| open.nested.get(k).map_or(0, |n| n.anchor);
        let mut levels = vec![Level {
            peek: &open.peek,
            decor: &decors[0],
            height: open.lay.box_height,
            anchor: anchor(0),
        }];
        for (i, n) in open.nested.iter().enumerate() {
            levels.push(Level {
                peek: &n.peek,
                decor: &decors[i + 1],
                height: n.height,
                anchor: anchor(i + 1),
            });
        }
        let (mut frame, parts) = overlay::nest_frame(&open.snap, &open.lay, &levels, full);
        let strip_top = open.strip_top;
        if let Some(top) = strip_top {
            frame.push_str(&strip_cursor(&self.shadow.snapshot(), top));
        }
        if let Some(open) = &mut self.open {
            open.parts = parts;
            open.drawn_at = Instant::now();
            open.dirty = false;
        }
        frame
    }

    fn draw_box(&mut self, full: bool, out: &mut dyn Write) -> Result<()> {
        let frame = self.box_bytes(full);
        out.write_all(frame.as_bytes())?;
        Ok(out.flush()?)
    }

    /// Which box and text position screen `cell` is on, when it is in the box.
    fn box_spot(&self, cell: (usize, usize)) -> Option<(usize, Option<Pos>)> {
        let open = self.open.as_ref()?;
        let row = cell.0.checked_sub(open.lay.box_top)?;
        overlay::spot(open.parts.get(row)?, open.snap.cols, cell.1)
    }

    /// The nested answer whose words are at `pos` of box `level`.
    fn child_at(&self, level: usize, pos: Pos) -> Option<(AnswerId, Range<usize>)> {
        let open = self.open.as_ref()?;
        let peek = Self::level_peek(open, level)?;
        let src = peek.src_at(overlay::text_width(open.snap.cols, level), pos)?;
        self.marks
            .child_ranges(self.level_answer(level)?)
            .into_iter()
            .find(|(r, _)| r.contains(&src))
            .map(|(r, id)| (id, r))
    }

    /// Mouse reports while the box is open. Inside the box: select text, hover
    /// and click nested marks, scroll the box under the pointer. When peekme
    /// turned the reports on itself, the child never gets any. Returns true
    /// when the token is used up here.
    pub(super) fn box_mouse(
        &mut self,
        m: Mouse,
        bytes: &[u8],
        out: &mut dyn Write,
    ) -> Result<bool> {
        let Some(open) = &self.open else {
            return Ok(false);
        };
        let own_mouse = open.own_mouse;
        let dragging = open.drag.is_some_and(|d| !d.done);
        let cell = input::mouse_cell(bytes);
        let button = input::mouse_button(bytes).unwrap_or(u32::MAX);
        let spot = cell.and_then(|c| self.box_spot(c));
        if spot.is_none() && !dragging {
            if own_mouse {
                // The child did not ask for these: none of them reaches it.
                if m == Mouse::Press {
                    self.close(out, false)?;
                }
                return Ok(true);
            }
            if self.open.as_ref().is_some_and(|o| o.box_hover.is_some()) {
                self.open.as_mut().unwrap().box_hover = None;
                self.set_pointer(false, out)?;
                self.draw_box(false, out)?;
            }
            return Ok(false);
        }
        match m {
            Mouse::WheelUp | Mouse::WheelDown => {
                let level = spot.map_or(0, |(l, _)| l);
                self.scroll_level(level, m == Mouse::WheelDown, out)?;
            }
            Mouse::Press => {
                let had = self.open.as_ref().and_then(|o| o.drag);
                let open = self.open.as_mut().unwrap();
                open.drag = match spot {
                    Some((level, Some(pos))) if button == 0 => Some(Drag {
                        level,
                        from: pos,
                        to: pos,
                        moved: false,
                        done: false,
                    }),
                    _ => None,
                };
                if had.is_some_and(|d| d.moved) {
                    self.draw_box(false, out)?;
                }
            }
            Mouse::Motion if button & 3 != 3 => {
                let open = self.open.as_mut().unwrap();
                if let Some(d) = &mut open.drag
                    && !d.done
                    && let Some((level, Some(pos))) = spot
                    && level == d.level
                    && pos != d.to
                {
                    d.to = pos;
                    d.moved |= pos != d.from;
                    self.draw_box(false, out)?;
                }
            }
            Mouse::Motion => {
                let now = match spot {
                    Some((level, Some(pos))) => self.child_at(level, pos).map(|(_, r)| (level, r)),
                    _ => None,
                };
                let open = self.open.as_mut().unwrap();
                if open.box_hover != now {
                    open.box_hover = now;
                    let hand = open.box_hover.is_some();
                    self.set_pointer(hand, out)?;
                    self.draw_box(false, out)?;
                }
            }
            Mouse::Release => {
                let open = self.open.as_mut().unwrap();
                let Some(d) = &mut open.drag else {
                    return Ok(true);
                };
                d.done = true;
                let d = *d;
                if !d.moved {
                    open.drag = None;
                    self.box_click(d.level, d.from, out)?;
                }
            }
        }
        Ok(true)
    }

    /// The text selected in one of the boxes, when there is one.
    pub(super) fn box_selection(&self) -> Option<Drag> {
        self.open.as_ref()?.drag.filter(|d| d.done && d.moved)
    }

    /// A click (no drag) at `pos` of box `level`. On a nested answer's words it
    /// opens or closes that answer, except as the second half of a double
    /// click that opened it. A double click elsewhere selects the word.
    fn box_click(&mut self, level: usize, pos: Pos, out: &mut dyn Write) -> Result<()> {
        let open = self.open.as_mut().unwrap();
        let prev = open.last_click.take();
        let double =
            prev.filter(|c| c.level == level && c.pos == pos && c.at.elapsed() < DOUBLE_CLICK);
        let mut click = BoxClick {
            at: Instant::now(),
            level,
            pos,
            opened: false,
        };
        if let Some((child, range)) = self.child_at(level, pos) {
            if double.is_some_and(|c| c.opened) {
                return Ok(());
            }
            click.opened = self.click_child(level, child, range, pos.0, out)?;
        } else if double.is_some() {
            let open = self.open.as_ref().unwrap();
            let width = overlay::text_width(open.snap.cols, level);
            let word = Self::level_peek(open, level).and_then(|p| p.word_at(width, pos));
            if let Some((from, to)) = word {
                self.open.as_mut().unwrap().drag = Some(Drag {
                    level,
                    from,
                    to,
                    moved: true,
                    done: true,
                });
                self.draw_box(false, out)?;
            }
            return Ok(());
        }
        if let Some(open) = &mut self.open {
            open.last_click = Some(click);
        }
        Ok(())
    }

    /// A click on words of box `level` that have a nested answer: open it in a
    /// box under them, or close it when it is open already.
    fn click_child(
        &mut self,
        level: usize,
        child: AnswerId,
        range: Range<usize>,
        line: usize,
        out: &mut dyn Write,
    ) -> Result<bool> {
        let open = self.open.as_ref().unwrap();
        let shown = open
            .nested
            .get(level)
            .is_some_and(|n| n.answer == Some(child));
        crate::event(
            "nested_click",
            serde_json::json!({"level": level, "close": shown}),
        );
        self.close_below(level);
        if shown {
            self.fit();
            self.draw_box(true, out)?;
            return Ok(false);
        }
        let Some(a) = self.marks.answer(child) else {
            return Ok(false);
        };
        let words = a.text.clone();
        self.push_level(level, words, range, line, Some(child), false, out)?;
        Ok(true)
    }

    /// Open a box inside box `level` for the text selected in it; with `ask`
    /// (Alt+Shift+P) it waits for a typed question.
    pub(super) fn open_nested(&mut self, ask: bool, out: &mut dyn Write) -> Result<()> {
        let Some(d) = self.box_selection() else {
            return Ok(());
        };
        let open = self.open.as_ref().unwrap();
        let width = overlay::text_width(open.snap.cols, d.level);
        let (from, to) = d.ordered();
        let (words, range) = match Self::level_peek(open, d.level).map(|p| p.shown(width, from, to))
        {
            Some((w, Some(r))) if !w.is_empty() => (w, r),
            _ => return Ok(()),
        };
        let Some(parent) = self.level_answer(d.level) else {
            // The answer is not complete yet: nothing to ask inside of.
            return Ok(());
        };
        let saved = self.marks.find_child(parent, &words);
        if let Some(id) = saved {
            self.marks.add_range(id, range.clone());
        }
        self.close_below(d.level);
        self.open.as_mut().unwrap().drag = None;
        let saved = saved.filter(|_| !ask);
        self.push_level(d.level, words, range, to.0, saved, ask, out)
    }

    /// Alt+Shift+P with boxes inside and nothing selected: ask about the
    /// words the innermost box explains, in its place.
    pub(super) fn ask_in_nested(&mut self, out: &mut dyn Write) -> Result<()> {
        let Some(n) = self.open.as_ref().and_then(|o| o.nested.last()) else {
            return Ok(());
        };
        let (level, words, range, line) = (
            self.open.as_ref().unwrap().nested.len() - 1,
            n.words.clone(),
            n.range.clone(),
            n.anchor,
        );
        self.close_below(level);
        self.push_level(level, words, range, line, None, true, out)
    }

    /// Enter in the innermost box: answer its typed question, with the boxes
    /// around it as context.
    pub(super) fn submit_nested_question(&mut self, out: &mut dyn Write) -> Result<()> {
        let Some(open) = &self.open else {
            return Ok(());
        };
        let level = open.nested.len() - 1;
        let n = &open.nested[level];
        let question = n.peek.input.as_deref().unwrap_or("").trim().to_string();
        if question.is_empty() {
            return Ok(());
        }
        let (range, height) = (n.range.clone(), n.height);
        let screen = open.selection.as_ref().map(|(_, s)| s.clone());
        crate::event(
            "nested_ask",
            serde_json::json!({"level": level + 1, "words": n.words, "question": question}),
        );
        self.next_id += 1;
        let id = self.next_id;
        let err = match (self.nested_request(level, &range), screen) {
            (Some(nested), Some(screen)) => self.start_explain(
                id,
                screen,
                height.saturating_sub(2).max(3),
                false,
                true,
                Some(nested),
                Some(question.clone()),
            ),
            _ => Some("nothing to ask about".into()),
        };
        let n = self.open.as_mut().unwrap().nested.last_mut().unwrap();
        n.id = id;
        n.peek.input = None;
        n.peek.question = Some(question);
        n.peek.text.clear();
        n.peek.scroll = 0;
        n.peek.status = match err {
            Some(e) => Status::Error(e),
            None => Status::Thinking,
        };
        self.fit();
        self.draw_box(true, out)
    }

    /// Put a new box inside box `level`, under its body line `line`: the saved
    /// answer `saved`, a box waiting for a question (`ask`), or a new
    /// explanation of `words`.
    #[allow(clippy::too_many_arguments)]
    fn push_level(
        &mut self,
        level: usize,
        words: String,
        range: Range<usize>,
        line: usize,
        saved: Option<AnswerId>,
        ask: bool,
        out: &mut dyn Write,
    ) -> Result<()> {
        let open = self.open.as_mut().unwrap();
        let cols = open.snap.cols;
        if open.nested.is_empty() {
            open.base_height = open.lay.box_height;
        }
        let width = overlay::text_width(cols, level + 1);
        let max_h = self.caps().get(level + 1).copied().unwrap_or(0);
        let open = self.open.as_mut().unwrap();
        if width < MIN_WIDTH || max_h < MIN_ROWS {
            crate::event("nested_no_room", serde_json::json!({"level": level + 1}));
            if let Some(p) = Self::level_peek_mut(open, level) {
                p.note = Some("no room for another box here");
            }
            return self.draw_box(true, out);
        }
        let mut peek = PeekBox::new(&words);
        peek.child_name = open.peek.child_name;
        let mut id = 0;
        let height = max_h;
        match saved.and_then(|a| self.marks.answer(a)) {
            _ if ask => peek.input = Some(String::new()),
            Some(a) => {
                peek.text = a.answer.clone();
                peek.model = format!("{} · saved", a.model);
                peek.status = Status::Done;
            }
            None => {
                self.next_id += 1;
                id = self.next_id;
                let req = self.nested_request(level, &range);
                match req {
                    Some(nested) => {
                        let screen = self
                            .open
                            .as_ref()
                            .and_then(|o| o.selection.as_ref())
                            .map(|(_, s)| s.clone());
                        let err = screen.and_then(|screen| {
                            self.start_explain(
                                id,
                                screen,
                                height.saturating_sub(2).max(3),
                                false,
                                false,
                                Some(nested),
                                None,
                            )
                        });
                        if let Some(e) = err {
                            peek.status = Status::Error(e);
                        }
                    }
                    None => peek.status = Status::Error("nothing to explain in".into()),
                }
            }
        }
        crate::event(
            "nested_open",
            serde_json::json!({"level": level + 1, "words": words, "saved": saved.is_some(), "ask": ask}),
        );
        let open = self.open.as_mut().unwrap();
        open.nested.push(Nested {
            id,
            words,
            range,
            anchor: line,
            answer: saved,
            peek,
            height,
        });
        self.fit();
        // Scroll the parent so the line and the new box under it show.
        let open = self.open.as_mut().unwrap();
        let height = open.nested[level].height;
        let inner = Self::level_height(open, level).saturating_sub(2);
        if let Some(p) = Self::level_peek_mut(open, level) {
            p.note = None;
            if line < p.scroll {
                p.scroll = line;
            } else if line + 1 + height > p.scroll + inner {
                p.scroll = (line + 1 + height).saturating_sub(inner);
            }
        }
        self.draw_box(true, out)
    }

    /// The most rows each level may take, outermost first, one more than
    /// there are levels: the outermost box up to about two thirds of the screen
    /// (never less than it had), each inner one 3 rows less than its parent
    /// (its borders and the line it explains).
    fn caps(&self) -> Vec<usize> {
        let Some(open) = &self.open else {
            return Vec::new();
        };
        let region = open.lay.region.1 - open.lay.region.0;
        let rows = self.size.1 as usize;
        let mut caps = vec![open.base_height.max(rows * 2 / 3).min(region)];
        for _ in 0..=open.nested.len() {
            let last = *caps.last().unwrap();
            caps.push(last.saturating_sub(3));
        }
        caps
    }

    /// Size every box to its text and the box inside it, within the caps. A
    /// box whose answer is still coming keeps its cap, so it does not jump on
    /// every line. With no boxes inside, the outer box goes back to its height.
    pub(super) fn fit(&mut self) {
        let caps = self.caps();
        let Some(open) = &mut self.open else {
            return;
        };
        if open.nested.is_empty() {
            if open.base_height > 0 {
                open.lay = overlay::resize(&open.lay, open.base_height);
                open.base_height = 0;
            }
            return;
        }
        let cols = open.snap.cols;
        let mut inner = 0;
        for k in (1..=open.nested.len()).rev() {
            let n = &mut open.nested[k - 1];
            let want = if matches!(n.peek.status, Status::Thinking | Status::Streaming) {
                caps[k]
            } else {
                n.peek.line_count(overlay::text_width(cols, k)) + 2 + inner
            };
            n.height = want.min(caps[k]).max(MIN_ROWS);
            inner = n.height;
        }
        let own = open.peek.line_count(overlay::text_width(cols, 0));
        let h0 = (own + 2 + inner).min(caps[0]).max(MIN_ROWS);
        open.lay = overlay::resize(&open.lay, h0);
    }

    /// The trail of answers down to box `level` and the words picked in it.
    pub(super) fn nested_request(&self, level: usize, range: &Range<usize>) -> Option<NestedAsk> {
        let mut trail = Vec::new();
        for k in 0..=level {
            let a = self.marks.answer(self.level_answer(k)?)?;
            trail.push((a.text.clone(), a.answer.clone()));
        }
        Some(NestedAsk {
            trail,
            pick: range.clone(),
        })
    }

    /// Close the boxes inside box `level`. Answers still streaming into them
    /// are saved when they complete.
    pub(super) fn close_below(&mut self, level: usize) {
        let Some(open) = &mut self.open else {
            return;
        };
        if open.nested.len() <= level {
            return;
        }
        let gone: Vec<Nested> = open.nested.drain(level..).collect();
        open.box_hover = None;
        if open.drag.is_some_and(|d| d.level > level) {
            open.drag = None;
        }
        for (k, n) in gone.into_iter().enumerate() {
            let streaming = matches!(n.peek.status, Status::Thinking | Status::Streaming);
            let parent = if k == 0 {
                self.level_answer(level)
            } else {
                None
            };
            if let (true, Some(parent)) = (streaming && n.id != 0, parent) {
                self.orphans.insert(
                    n.id,
                    Orphan {
                        parent,
                        words: n.words,
                        range: n.range,
                        // A typed question is saved with its answer.
                        text: match &n.peek.question {
                            Some(q) => format!("› {}\n\n{}", overlay::sanitize(q), n.peek.text),
                            None => n.peek.text,
                        },
                        model: n.peek.model,
                    },
                );
            }
        }
    }

    /// Esc: close the innermost box. Returns false when there is none inside.
    pub(super) fn close_innermost(&mut self, out: &mut dyn Write) -> Result<bool> {
        let depth = self.open.as_ref().map_or(0, |o| o.nested.len());
        if depth == 0 {
            return Ok(false);
        }
        self.close_below(depth - 1);
        self.fit();
        self.draw_box(true, out)?;
        Ok(true)
    }

    /// Scroll box `level` by about a page.
    pub(super) fn scroll_level(
        &mut self,
        level: usize,
        down: bool,
        out: &mut dyn Write,
    ) -> Result<()> {
        let Some(open) = &mut self.open else {
            return Ok(());
        };
        let cols = open.snap.cols;
        let max = {
            let decor = Decor::default();
            let mut levels = vec![Level {
                peek: &open.peek,
                decor: &decor,
                height: open.lay.box_height,
                anchor: 0,
            }];
            for n in &open.nested {
                levels.push(Level {
                    peek: &n.peek,
                    decor: &decor,
                    height: n.height,
                    anchor: 0,
                });
            }
            overlay::scroll_limit(&levels, level.min(levels.len() - 1), cols)
        };
        let step = Self::level_height(open, level).saturating_sub(3).max(1);
        let Some(peek) = Self::level_peek_mut(open, level) else {
            return Ok(());
        };
        peek.scroll = if down {
            (peek.scroll + step).min(max)
        } else {
            peek.scroll.saturating_sub(step)
        };
        // The selection is kept by text position; it scrolls with the text.
        self.draw_box(false, out)
    }

    /// Progress of a request that is not the outermost box's.
    pub(super) fn nested_progress(
        &mut self,
        id: u64,
        p: Progress,
        out: &mut dyn Write,
    ) -> Result<()> {
        let level = self
            .open
            .as_ref()
            .and_then(|o| o.nested.iter().position(|n| n.id == id && id != 0));
        let Some(i) = level else {
            return self.orphan_progress(id, p);
        };
        let parent = self.level_answer(i);
        let open = self.open.as_mut().unwrap();
        let n = &mut open.nested[i];
        let mut full = false;
        match p {
            Progress::Started { model, source } => n.peek.model = format!("{model} · {source}"),
            Progress::Delta(d) => {
                n.peek.text.push_str(&d);
                n.peek.status = Status::Streaming;
            }
            Progress::Done => {
                n.peek.status = Status::Done;
                n.peek.ask_available = true;
                if let Some(parent) = parent
                    && !n.peek.text.trim().is_empty()
                {
                    // A typed question is saved with its answer.
                    let text = match &n.peek.question {
                        Some(q) => format!(
                            "› {}\n\n{}",
                            overlay::sanitize(q),
                            overlay::sanitize(&n.peek.text)
                        ),
                        None => overlay::sanitize(&n.peek.text),
                    };
                    let id = self
                        .marks
                        .save(Some(parent), &n.words, &text, &n.peek.model);
                    self.marks.add_range(id, n.range.clone());
                    n.answer = Some(id);
                    crate::event("nested_mark", serde_json::json!({"words": n.words}));
                }
                full = true;
            }
            Progress::Failed(e) => n.peek.status = Status::Error(e),
            // Only asked of the outermost box.
            Progress::TooBig { .. } => {}
        }
        if !full && n.peek.status == Status::Streaming && open.drawn_at.elapsed() < FRAME {
            open.dirty = true;
            return Ok(());
        }
        if full {
            self.fit();
        }
        self.draw_box(full, out)
    }

    /// An answer whose box was closed while it streamed: kept, and saved when
    /// complete, so its words get a mark.
    fn orphan_progress(&mut self, id: u64, p: Progress) -> Result<()> {
        let Some(o) = self.orphans.get_mut(&id) else {
            return Ok(());
        };
        match p {
            Progress::Started { model, source } => o.model = format!("{model} · {source}"),
            Progress::Delta(d) => o.text.push_str(&d),
            Progress::Done => {
                let o = self.orphans.remove(&id).unwrap();
                if !o.text.trim().is_empty() && self.marks.answer(o.parent).is_some() {
                    let text = overlay::sanitize(&o.text);
                    let child = self.marks.save(Some(o.parent), &o.words, &text, &o.model);
                    self.marks.add_range(child, o.range);
                }
            }
            Progress::Failed(_) | Progress::TooBig { .. } => {
                self.orphans.remove(&id);
            }
        }
        Ok(())
    }
}
