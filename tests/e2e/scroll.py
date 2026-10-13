#!/usr/bin/env python3
"""Scrolling Codex's chat with a peek box open: real Codex, full screen, Linux,
no tmux. No model calls (the fake explainer writes the box).

Every frame peekme draws while a box is open is checked against the one right
picture: Codex's chat T with the box (H rows) put in after the selection
(before it, for a box above), and the screen a window of that:

    V = T[:I] + box + T[I:]        chat rows on screen == V[off:off + R]

Each turn of the wheel moves `off` by as many rows as it moves Codex's chat
(Codex's own count, kept here too: `tui.mouse_scroll_speed` rows a turn since
0.162, fractions carried), stopping at either end of V. Nothing else moves it, except the box growing while
whole on the screen and running into its edge (it then pushes the
selection). T is read from Codex itself before the box opens: scrolled to
the top, then a page at a time to the end.

    python3 tests/e2e/scroll.py [--only NAME] [--repeat N]
"""
import json
import math
import os
import re
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import matrix as m  # noqa: E402
import real_agents as ra  # noqa: E402

VTFRAMES = os.path.join(m.ROOT, "target/release/examples/vtframes")
# Another build to test (an older one, to see a check fail on it).
m.PEEKME = os.environ.get("PEEKME_BIN", m.PEEKME)


def rust_round(x):
    return math.copysign(math.floor(abs(x) + 0.5), x)


class Wheel:
    """Codex's count of wheel rows, as Codex keeps it (0.162.1,
    transcript_view/input.rs): the speed a turn, whole rows scrolled, the
    fraction kept until the wheel turns the other way."""

    def __init__(self, speed):
        self.speed, self.pending = speed, 0.0

    def turn(self, way):
        d = -1.0 if way < 0 else 1.0
        if math.copysign(1.0, self.pending) != d:
            self.pending = 0.0
        self.pending += d * self.speed
        r = rust_round(self.pending)
        if r != 0 and abs(self.pending - r) < 1e-9:
            self.pending = r
        rows = math.trunc(self.pending)
        self.pending -= rows
        return rows
BOX = ("╭", "│", "╰")
LABEL = re.compile(r"\bb\d+r\d\d\b")


def wheel_bytes(up, x, y, side=False):
    b = (66 if up else 67) if side else (64 if up else 65)
    return f"\x1b[<{b};{x};{y}M".encode()


class Session:
    """One Codex under peekme, with a chat made of labelled rows."""

    def __init__(self, out, blocks, rep=False, size=(120, 40), block_rows=40, late=False,
                 speed=None):
        m.COLS, m.ROWS = size
        self.out = out
        self.used = set()
        self.burst_wait = 4.0 if late else 0.15
        self.lines_file = os.path.join(out, "lines")
        self.set_answer(6)
        env = m.clean_env(
            TERM="xterm-256color", PEEKME_FAKE_EXPLAINER="1",
            PEEKME_FAKE_LINES="@" + self.lines_file,
            PEEKME_EVENT_LOG=os.path.join(out, "events.jsonl"),
            DISPLAY="", WAYLAND_DISPLAY="")
        # `late`: Codex behind a relay that delays its scroll after its first,
        # empty answer to each wheel turn (a slower machine).
        relay = [sys.executable, os.path.join(HERE, "late_pty.py")] if late else []
        # Codex's wheel speed: its default 1.0, or set on its command line.
        setting = ["-c", f"tui.mouse_scroll_speed={speed}"] if speed else []
        self.model = Wheel(speed or 1.0)
        self.t = m.Term([m.PEEKME] + relay + [ra.real_binary("codex"), "-c",
                         "check_for_update_on_startup=false"] + setting, env,
                        os.path.join(out, "terminal.bin"))
        self.ready()
        for b in range(blocks):
            fill = "same row of a repeated output" if rep and b == 1 else None
            self.run_block(b, fill, block_rows)
        self.R = self.chat_rows()
        self.T = self.read_chat()
        if len(self.T) < blocks * block_rows:
            raise RuntimeError(f"read only {len(self.T)} rows of the chat")

    def set_answer(self, lines, delay=0):
        with open(self.lines_file, "w") as f:
            f.write(f"{lines} {delay}")

    def screen(self):
        return [r.rstrip() for r in self.t.screen()]

    def ready(self):
        end = time.time() + 60
        while time.time() < end:
            txt = "\n".join(self.screen())
            for prompt, keys in ra.AGENTS["codex"][4]:
                if prompt in txt:
                    self.t.send(keys)
                    time.sleep(2)
            if "OpenAI Codex" in txt and "Do you trust" not in txt:
                time.sleep(3)
                return
            time.sleep(0.5)
        raise RuntimeError("Codex did not start")

    def type(self, s):
        for ch in s.encode():
            self.t.send(bytes([ch]))
            time.sleep(0.01)
        time.sleep(0.3)
        self.t.send(b"\r")

    def run_block(self, b, fill=None, n=40):
        if fill:
            # Rows that all read the same, between two labelled ones.
            self.type(f"!printf 'b{b}r00 start\\n'; for i in $(seq 1 30); do echo '{fill}'; done;"
                      f" printf 'b{b}r99 end\\n'")
            last = f"b{b}r99"
        else:
            self.type(f"!for i in $(seq -w 1 {n}); do echo \"b{b}r$i words of row $i in block {b}\"; done")
            last = f"b{b}r{n:02d}"
        if not m.wait(lambda: any(last in r and "echo" not in r for r in self.screen()), 20):
            raise RuntimeError(f"block {b} did not show")
        time.sleep(1.5)

    def chat_rows(self):
        s = self.screen()
        composer = max(i for i, r in enumerate(s) if r.startswith("›"))
        return composer - 2

    def mark(self):
        with self.t.lock:
            return len(self.t.data)

    def wheel(self, up, n=1, gap=0.0, batch=1, side=None, at=None):
        """Turn the wheel; returns the rows each turn moves Codex's chat."""
        # Codex scrolls for the wheel over its chat only.
        x, y = (m.COLS // 2, self.R // 2) if at in (None, "auto") else at
        rows = []
        for _ in range(n):
            if at == "auto":
                # A row the box does not cover (a box scrolls its own text).
                chat = self.chat()
                y = 1 + min((r for r in range(self.R) if chat[r][:1] not in BOX),
                            key=lambda r: abs(r - self.R // 2))
            b = wheel_bytes(up, x, y) * batch
            if side:
                b += b"".join(wheel_bytes(s == "<", x, y, side=True) for s in side)
            self.t.send(b)
            rows += [self.model.turn(-1 if up else 1) for _ in range(batch)]
            if gap:
                time.sleep(gap)
        return rows

    def chat(self):
        return self.screen()[:self.R]

    def settle(self):
        """Wait until the screen stops changing (a late Codex takes a while)."""
        prev, same_since = None, time.time()
        end = time.time() + 20
        while time.time() < end:
            cur = self.screen()
            if cur != prev:
                prev, same_since = cur, time.time()
            elif time.time() - same_since > 0.6:
                return
            time.sleep(0.1)

    def to_end(self):
        self.t.send(b"\x1b[1;5F")  # Ctrl+End
        self.settle()

    def read_chat(self):
        self.t.send(b"\x1b[1;5H")  # Ctrl+Home
        self.settle()
        cur = self.chat()
        T = list(cur)
        while True:
            self.t.send(b"\x1b[6~")  # PageDown
            time.sleep(0.3)
            nxt = self.chat()
            if nxt == cur:
                break
            s = next((s for s in [self.R - 1] + list(range(1, self.R))
                      if nxt[:self.R - s] == cur[s:]), None)
            if s is None:
                raise RuntimeError("reading the chat: screens do not overlap")
            T += nxt[self.R - s:]
            cur = nxt
        if len(set(r for r in T if LABEL.search(r))) != len([r for r in T if LABEL.search(r)]):
            raise RuntimeError("labelled rows are not unique")
        return T

    def close(self):
        self.t.close()


class Case:
    def __init__(self, s, name):
        self.s, self.name = s, name
        self.errors = []
        self.marks = []  # (name, offset, expected move of off from the previous check or None)

    def err(self, msg):
        if len(self.errors) < 12:
            self.errors.append(msg)


def select(s, row, two_rows=False):
    """Drag over the label nearest to screen row `row` that no case picked
    before (peekme keeps answers: a second time the box would open with the
    old one); returns the selected rows' text."""
    chat = s.chat()
    rows = [i for i, r in enumerate(chat) if LABEL.search(r)
            and LABEL.search(r)[0] not in s.used
            and (not two_rows or (i + 1 < len(chat) and LABEL.search(chat[i + 1])))]
    r = min(rows, key=lambda i: abs(i - row))
    s.used.add(LABEL.search(chat[r])[0])
    if two_rows:
        s.used.add(LABEL.search(chat[r + 1])[0])
    m1 = LABEL.search(chat[r])
    # Codex copies up to the cell before the one the drag ends on.
    if two_rows:
        m2 = LABEL.search(chat[r + 1])
        a, b, r2 = (m1.start() + 1, r + 1), (m2.end() + 1, r + 2), r + 1
    else:
        a, b, r2 = (m1.start() + 1, r + 1), (m1.end() + 1, r + 1), r
    s.t.send(f"\x1b[<0;{a[0]};{a[1]}M\x1b[<32;{b[0]};{b[1]}M\x1b[<0;{b[0]};{b[1]}m".encode())
    time.sleep(0.6)
    return r, r2, chat[r:r2 + 1]


def frames(s, start, end, stops):
    s.t.save()
    out = subprocess.run([VTFRAMES, s.t.log_path, str(m.COLS), str(m.ROWS), str(start), str(end)]
                         + [str(x) for x in stops], capture_output=True, text=True).stdout
    res = []
    for line in out.splitlines():
        f = json.loads(line)
        res.append((f["off"], [r.rstrip() for r in f["rows"]]))
    return res


def box_run(chat):
    idx = [i for i, r in enumerate(chat) if r[:1] in BOX]
    if not idx:
        return None
    return idx[0], idx[-1] + 1, idx == list(range(idx[0], idx[-1] + 1))


def offsets(chat, T, I, H, ref):
    """Where `chat` sits in V (a set: repeated rows can fit more than one
    place), or an error string. `ref` is the whole box once its text is done."""
    R = len(chat)
    run = box_run(chat)
    if run:
        b0, b1, contiguous = run
        if not contiguous:
            return "box rows are not together"
        n = b1 - b0
        if b0 > 0 and (I - b0 < 0 or T[I - b0:I] != chat[:b0]):
            bad = next((r for r in range(b0) if I - b0 + r < 0 or T[I - b0 + r] != chat[r]), 0)
            return f"row {bad} above the box is not the chat right above it: {chat[bad]!r}"
        below = chat[b1:]
        if T[I:I + len(below)] != below:
            bad = next((r for r in range(len(below)) if I + r >= len(T) or T[I + r] != below[r]), 0)
            return f"row {b1 + bad} below the box is not the chat right below it: {below[bad]!r}"
        rows = chat[b0:b1]
        top, bottom = rows[0].startswith("╭"), rows[-1].startswith("╰")
        if b0 > 0 and not top:
            return "box top border missing"
        if b1 < R and not bottom:
            return "box bottom border missing"
        if ref is not None:
            if top and bottom and rows != ref:
                return f"box is {n} rows, not its {H}" if n != H else "box text differs"
            if not top and rows != ref[H - n:]:
                return "box cut at the top is not its bottom part"
            if not bottom and rows != ref[:n]:
                return "box cut at the bottom is not its top part"
        if top:
            return {I - b0}
        if H is None:
            return set()
        return {I + H - n}
    ks = [k for k in range(len(T) - R + 1) if T[k:k + R] == chat]
    if H is None:
        return set() if ks else "screen is not a window of the chat"
    offs = {k for k in ks if k + R <= I} | {k + H for k in ks if k >= I}
    return offs or "screen is not the chat with the box in it"


def check(case, open_at, end_at, sel_rows, p0, capped=False):
    """Check every frame from the box's first one to `end_at`, and each mark's move."""
    s, T, R = case.s, case.s.T, case.s.R
    S = [T.index(r) for r in sel_rows]
    fr = frames(s, open_at, end_at, [o for _, o, _ in case.marks])
    at = {o: rows for o, rows in fr}
    first = next((i for i, (_, rows) in enumerate(fr) if box_run(rows[:R])), None)
    if first is None:
        case.err("the box never showed")
        return
    # Above or below: where the box sits next to the selection when both show.
    I = H = ref = None
    done_at = case.done_at
    for o, rows in fr[first:]:
        run = box_run(rows[:R])
        if run and run[2]:
            if run[0] > 0 and rows[run[0] - 1] == T[S[-1]]:
                I = S[-1] + 1
            elif run[1] < R and rows[run[1]] == T[S[0]]:
                I = S[0]
            if I is not None:
                break
    if I is None:
        case.err("never saw the box right next to its selection")
        return
    # The whole box once its text is done: from the first frame after that
    # where it shows whole (scrolling while the text came may have cut it).
    if done_at is not None:
        for o, rows in fr:
            run = box_run(rows[:R])
            if o < done_at or not run or not run[2]:
                continue
            if rows[run[0]].startswith("╭") and rows[run[1] - 1].startswith("╰"):
                ref = rows[run[0]:run[1]]
                H = len(ref)
                break
        if ref is None:
            case.err("never saw the whole box once its text was done")
            return
        # Borders, the header (a row a selected row), a gap, lines and gaps.
        want_h = 2 * case.lines + 2 + len(sel_rows)
        if not case.capped and not case.nested and len(sel_rows) == 1 and H != want_h:
            case.err(f"box is {H} rows, not {want_h} for {case.lines} lines")
        if not case.scrolled_early:
            rows = at.get(done_at) or next(r for o, r in reversed(fr) if o <= done_at)
            run = box_run(rows[:R])
            if not run or rows[run[0]:run[1]] != ref:
                case.err("box not whole on the screen when its text was done")
                return
            if I == S[-1] + 1:
                sel_now, want = run[0] - 1, min(p0[-1], R - H - 1)
            else:
                sel_now, want = run[1], max(p0[0], H)
            if sel_now != want:
                case.err(f"selection moved to row {sel_now} at open, not {want}")
    # Every frame.
    if os.environ.get("SCROLL_DEBUG"):
        print(f"    {len(fr) - first} frames, I={I} H={H}")
    prev = None
    where = {}  # frame offset -> set of places in V, once the text is done
    for o, rows in fr[first:]:
        chat = rows[:R]
        settled = done_at is not None and o >= done_at
        got = offsets(chat, T, I, H if settled else None,
                      ref if settled and not capped else None)
        if isinstance(got, str):
            case.err(f"frame at byte {o}: {got}")
        elif settled:
            where[o] = got
        for r in rows[R:]:
            if r[:1] in BOX:
                case.err(f"frame at byte {o}: box drawn over Codex's input area")
                break
    if H is None or capped:
        return
    # The moves. Each turn moves 3 rows, stopping at either end of V. A turn
    # that reaches the end of Codex's chat partway may leave up to 2 rows
    # for the next turn that way (peekme waits until the end shows); never
    # more than the turns asked for.
    maxoff = len(T) + H - R
    off = last = None
    for name, o, move in case.marks:
        if o < done_at:
            continue
        rows = at.get(o) or next((r for x, r in reversed(fr) if x <= o), None)
        if rows is None:
            continue
        got = offsets(rows[:R], T, I, H, ref)
        if isinstance(got, str) or not got:
            if os.environ.get("SCROLL_DEBUG"):
                print(f"    {name:28} {got!r}")
            continue
        if off is not None and move is not None:
            want = off
            for d in move:
                want = max(0, min(maxoff, want + d))
            # The way of the turns (a slow wheel's turns may move no row).
            last_way = next((d for d in reversed(move) if d), 0)
            sign = (last_way > 0) - (last_way < 0)
            if sign:
                slack = max(abs(d) for d in move) - 1
                fits = [g for g in got if 0 <= (want - g) * sign <= slack]
            else:
                fits = [g for g in got if g == last]
            if not fits:
                case.err(f"{name}: moved to {sorted(got)}, want {want} (from {off})")
            # Every frame on the way: never past the turns sent, never back.
            if sign and last is not None and len({d > 0 for d in move if d}) == 1:
                way = [min(where[x], key=lambda g: abs(g - want)) for x in sorted(where)
                       if prev_o < x <= o]
                for a, b in zip([last] + way, way):
                    if (b - a) * sign < 0:
                        case.err(f"{name}: a frame on the way moved back ({a} -> {b})")
                        break
                if any((g - want) * sign > 0 for g in way):
                    case.err(f"{name}: a frame on the way went past {want}")
            off = want
            last = fits[0] if fits else min(got)
        else:
            off = last = min(got)
        prev_o = o
        if os.environ.get("SCROLL_DEBUG"):
            print(f"    {name:28} off {sorted(got)} of {maxoff}  (I={I} H={H} len(T)={len(T)})")
        if name.endswith("+1s") and prev is not None and at.get(prev[1]) is not None:
            if at[prev[1]][:R] != rows[:R]:
                case.err(f"{name}: the screen changed after the wheel stopped")
        prev = (name, o)


def chat_end(rows):
    """Where Codex's chat ends: two rows above its input area, or above its
    status while it works ("• Working (5s • esc to interrupt)")."""
    composer = max(i for i, r in enumerate(rows) if r.startswith("›"))
    busy = [r for r in range(max(0, composer - 8), composer) if "esc to interrupt" in rows[r]]
    if busy:
        r = busy[0]
        return r - 1 if r > 0 and not rows[r - 1].strip() else r
    return composer - 2


def check_live(case, open_at, end_at, sel_rows):
    """While Codex writes: the box stays whole and right next to its selection."""
    s = case.s
    fr = frames(s, open_at, end_at, [])
    seen = 0
    for o, rows in fr:
        R = chat_end(rows)
        chat = rows[:R]
        run = box_run(chat)
        if not run:
            continue
        b0, b1, contiguous = run
        if not contiguous:
            case.err(f"frame at byte {o}: box rows are not together")
            continue
        top, bottom = chat[b0].startswith("╭"), chat[b1 - 1].startswith("╰")
        if (b0 > 0 and not top) or (b1 < R and not bottom):
            case.err(f"frame at byte {o}: box border missing")
        # The rows on either side of the box, from the first frame showing
        # the box whole next to its selection; the same ever after.
        if not seen and top and bottom and 0 < b0 and b1 < R and sel_rows[-1] in (chat[b0 - 1], chat[b1]):
            seen = (chat[b0 - 1], chat[b1])
        if seen:
            if top and b0 > 0 and chat[b0 - 1] != seen[0]:
                case.err(f"frame at byte {o}: the row above the box is {chat[b0 - 1]!r}, not {seen[0]!r}")
            if bottom and b1 < R and chat[b1] != seen[1]:
                case.err(f"frame at byte {o}: the row below the box is {chat[b1]!r}, not {seen[1]!r}")
    if not seen:
        case.err("never saw the box next to its selection")


def run_case(s, name, sel_row, lines=6, delay=0, pre_up=0, actions=(), two_rows=False,
             close="esc", capped=False, live=None, nested=False):
    """Open a box at `sel_row` and run `actions`; returns the case."""
    case = Case(s, name)
    s.set_answer(lines, delay)
    s.to_end()
    if live:
        # Codex runs a command that prints a row every 1.5 s meanwhile; the
        # drag goes right after one, so the rows stay put under it.
        s.type(live)
        time.sleep(2)
        if not pre_up:
            before = s.chat()
            m.wait(lambda: s.chat() != before, 4, 0.05)
    if pre_up:
        s.wheel(True, pre_up, 0.01)
        s.settle()
    r1, r2, sel = select(s, sel_row, two_rows)
    open_at = s.mark()
    case.label = LABEL.search(sel[0])[0]
    s.t.send(b"\x1bp")
    case.done_at = None
    case.lines, case.capped, case.nested = lines, capped, nested
    case.scrolled_early = any(x[0] == "now" for x in actions)
    if not m.wait(lambda: any(r[:1] == "╭" for r in s.chat()), 5):
        case.err("box did not open")
        s.t.send(b"\x1b")
        return case
    if not any(r.startswith("╭") and f"peek · {case.label}" in r for r in s.chat()):
        case.err(f"box is not about {case.label}: "
                 + next(r for r in s.chat() if r.startswith("╭"))[:40])
    stream = lines * delay / 1000
    if not any(a[0] == "now" for a in actions):
        time.sleep(stream + 1.0)
        case.done_at = s.mark()
        case.marks.append(("open", case.done_at, None))
    for a in actions:
        kind = a[0]
        if kind == "now":
            continue
        if kind == "settle":
            time.sleep(stream + 1.0)
            case.done_at = s.mark()
            case.marks.append(("text done", case.done_at, None))
        elif kind == "slow":
            _, up, n = a[:3]
            at = a[3] if len(a) > 3 else None
            for i in range(n):
                rows = s.wheel(up, at=at)
                time.sleep(0.25)
                case.marks.append((f"{'up' if up else 'down'} {i + 1}/{n}", s.mark(), rows))
        elif kind == "burst":
            _, up, n = a[:3]
            opts = a[3] if len(a) > 3 else {}
            rows = s.wheel(up, n, opts.get("gap", 0.008), opts.get("batch", 1), opts.get("side"))
            # A late Codex (the relay) takes a while to answer a burst.
            wait = s.burst_wait
            time.sleep(wait)
            label = f"burst {'up' if up else 'down'} {n}x{opts.get('batch', 1)}"
            case.marks.append((label + f" +{int(wait * 1000)}ms", s.mark(), rows))
            time.sleep(1.0)
            case.marks.append((label + " +1s", s.mark(), []))
        elif kind == "page":
            # PageUp / PageDown: peekme sends Codex a page of wheel turns.
            _, up, n = a
            turns = max(1, int(rust_round((s.R - 1) / s.model.speed)))
            for i in range(n):
                s.t.send(b"\x1b[5~" if up else b"\x1b[6~")
                rows = [s.model.turn(-1 if up else 1) for _ in range(turns)]
                time.sleep(max(0.4, s.burst_wait))
                case.marks.append((f"page {'up' if up else 'down'} {i + 1}/{n}", s.mark(), rows))
        elif kind == "seq":
            # Several directions in one quick run: [(up, n), ...]
            moves = []
            for up, n in a[1]:
                moves += s.wheel(up, n, 0.008)
            time.sleep(s.burst_wait)
            case.marks.append(("reversing burst +50ms", s.mark(), moves))
            time.sleep(1.0)
            case.marks.append(("reversing burst +1s", s.mark(), []))
        elif kind == "nested":
            # Pick a word in the box and peek at it: a box in the box.
            chat = s.chat()
            r = next(i for i, row in enumerate(chat) if "Line 2 of" in row)
            c = chat[r].index("Line 2 of") + 1
            s.t.send(f"\x1b[<0;{c};{r + 1}M\x1b[<32;{c + 3};{r + 1}M\x1b[<0;{c + 3};{r + 1}m".encode())
            time.sleep(0.5)
            s.t.send(b"\x1bp")
            time.sleep(1.5)
            case.done_at = s.mark()
            case.marks.append(("box in the box", case.done_at, None))
    end = s.mark()
    if live:
        check_live(case, open_at, end, sel)
    else:
        check(case, open_at, end, sel, list(range(r1, r2 + 1)), capped)
    # Closing leaves Codex's own screen, and Codex agrees: one more turn up
    # moves that very screen by its step.
    if close == "esc":
        # Esc closes the innermost box.
        for _ in range(2 if nested else 1):
            s.t.send(b"\x1b")
            time.sleep(0.2)
    else:
        s.t.send(f"\x1b[<0;{m.COLS - 2};{s.R + 2}M\x1b[<0;{m.COLS - 2};{s.R + 2}m".encode())
    time.sleep(0.6)
    chat = s.chat()
    if box_run(chat):
        case.err("box still there after closing")
    elif live:
        # The chat changes as Codex writes: Codex's next scroll must still
        # line up with this screen, its chat rows a few rows further down.
        before = s.screen()
        s.wheel(True)
        time.sleep(0.4)
        after = s.screen()
        R = min(chat_end(before), chat_end(after))
        a, b = before[:R], after[:R]
        if not any(b[k:] == a[:R - k] for k in range(0, 7)):
            case.err("after closing Codex's next scroll does not match the screen")
    else:
        ks = [k for k in range(len(s.T) - s.R + 1) if s.T[k:k + s.R] == chat]
        if not ks:
            case.err("after closing the screen is not Codex's chat")
        else:
            back = -s.wheel(True)[0]
            time.sleep(0.3)
            after = s.chat()
            if not any(s.T[max(0, k - back):max(0, k - back) + s.R] == after for k in ks):
                case.err("after closing Codex's next scroll does not match the screen")
    return case


def cases_long(s):
    yield "below, slow both ways past both ends", lambda: run_case(
        s, "below-slow", 8, actions=[("slow", True, 12), ("slow", False, 20),
                                     ("burst", True, 120, {"gap": 0.003}),
                                     ("slow", True, 3), ("slow", False, 8)])
    yield "below, taller than the room below", lambda: run_case(
        s, "below-tall", 14, lines=12, actions=[("burst", True, 10), ("burst", False, 16),
                                                ("slow", False, 3), ("slow", True, 4)])
    yield "above", lambda: run_case(
        s, "above", s.R - 3 if s else 0, actions=[("slow", True, 5), ("burst", False, 9),
                                    ("slow", False, 2), ("slow", True, 3)])
    yield "above, taller than the room above", lambda: run_case(
        s, "above-tall", 23, lines=12, actions=[("slow", False, 4), ("burst", True, 8),
                                                ("slow", True, 2), ("burst", False, 14)])
    yield "trackpad: batches and sideways", lambda: run_case(
        s, "trackpad", 10, actions=[("burst", True, 6, {"batch": 5, "gap": 0.012}),
                                    ("burst", False, 8, {"batch": 5, "gap": 0.012, "side": "><>"}),
                                    ("burst", True, 10, {"side": "<<"}),
                                    ("burst", False, 14, {"side": ">"})])
    yield "chat scrolled up at open, reversing", lambda: run_case(
        s, "middle-reverse", 12, pre_up=20,
        actions=[("seq", [(True, 6), (False, 9), (True, 2)]), ("seq", [(False, 4), (True, 7)]),
                 ("burst", True, 40), ("burst", False, 40), ("slow", False, 2)])
    yield "far away and back", lambda: run_case(
        s, "far", 10, actions=[("burst", True, 60), ("burst", False, 60), ("burst", False, 6),
                               ("burst", True, 60), ("slow", True, 3), ("burst", False, 70)])
    yield "selection at the top of the chat", lambda: run_case(
        s, "top", 2, pre_up=150, actions=[("slow", True, 3), ("slow", False, 6),
                                          ("burst", True, 10), ("burst", False, 30)])
    yield "selection over two rows", lambda: run_case(
        s, "two-rows", 9, two_rows=True, actions=[("slow", True, 4), ("burst", False, 10)])
    yield "wheel over the box", lambda: run_case(
        s, "over-box", 6, actions=[("slow", True, 3, (20, 12)), ("slow", False, 6, (20, 12)),
                                   ("burst", False, 4)])
    yield "text streams in while scrolling", lambda: run_case(
        s, "streaming", 10, lines=12, delay=120,
        actions=[("now",), ("slow", True, 3), ("slow", False, 5), ("burst", False, 4),
                 ("settle",), ("slow", False, 3), ("slow", True, 6)])
    yield "text streams in at the end of the chat", lambda: run_case(
        s, "streaming-end", 20, lines=14, delay=100,
        actions=[("now",), ("burst", False, 6), ("slow", False, 3), ("settle",),
                 ("slow", False, 4), ("burst", True, 12)])
    yield "PageUp and PageDown", lambda: run_case(
        s, "pages", 12, actions=[("page", True, 3), ("page", False, 5), ("slow", True, 2),
                                 ("page", True, 2), ("burst", False, 30)])
    yield "close with a click", lambda: run_case(
        s, "click-close", 8, actions=[("burst", True, 5), ("burst", False, 9)], close="click")


def cases_more(s):
    yield "answer taller than the chat", lambda: run_case(
        s, "capped", 10, lines=24, capped=True,
        actions=[("slow", False, 4), ("slow", True, 4), ("burst", False, 30), ("burst", True, 30)])
    yield "box in the box, then scroll", lambda: run_case(
        s, "nested", 6, lines=2, nested=True,
        actions=[("nested",), ("slow", True, 3, "auto"), ("slow", False, 8, "auto"),
                 ("slow", True, 12, "auto")])


def cases_small(s):
    yield "small terminal, below", lambda: run_case(
        s, "small-below", 4, lines=3, actions=[("slow", True, 4), ("slow", False, 10),
                                               ("burst", True, 20), ("burst", False, 30)])
    yield "small terminal, tall answer", lambda: run_case(
        s, "small-tall", 6, lines=6, actions=[("slow", False, 6), ("burst", True, 12),
                                              ("burst", False, 20)])
    yield "small terminal, above", lambda: run_case(
        s, "small-above", s.R - 2 if s else 0, lines=4,
        actions=[("slow", False, 5), ("slow", True, 8), ("burst", False, 20)])


def cases_short(s):
    yield "short chat (Codex cannot scroll)", lambda: run_case(
        s, "short", 3, lines=10, actions=[("slow", False, 5), ("slow", True, 4),
                                          ("burst", False, 10), ("burst", True, 10)])


def cases_live(s):
    cmd = "!for i in $(seq -w 1 30); do echo \"live$i\"; sleep 1.5; done"
    yield "Codex writes while the box is open", lambda: run_case(
        s, "live", 8, live=cmd, actions=[("slow", True, 4), ("slow", False, 6),
                                         ("burst", True, 8), ("burst", False, 12)])
    yield "Codex writes, chat scrolled up", lambda: run_case(
        s, "live-up", 10, live=cmd, pre_up=10,
        actions=[("slow", False, 3), ("burst", True, 6), ("burst", False, 20)])


def cases_late(s):
    yield "late scroll: slow both ways past both ends", lambda: run_case(
        s, "late-slow", 8, actions=[("slow", True, 10), ("slow", False, 16),
                                    ("burst", True, 60, {"gap": 0.003}), ("slow", False, 6)])
    yield "late scroll: trackpad bursts", lambda: run_case(
        s, "late-trackpad", 12, actions=[("burst", True, 6, {"batch": 5, "gap": 0.012}),
                                         ("burst", False, 8, {"batch": 5, "gap": 0.012}),
                                         ("burst", True, 20), ("burst", False, 30)])
    yield "late scroll: chat scrolled up, far and back", lambda: run_case(
        s, "late-far", 10, pre_up=15, actions=[("burst", True, 40), ("burst", False, 70),
                                               ("page", True, 2), ("page", False, 3)])


def cases_speed(s):
    yield "slow both ways past both ends", lambda: run_case(
        s, "speed-slow", 8, actions=[("slow", True, 10), ("slow", False, 30),
                                     ("burst", True, 200, {"gap": 0.003}), ("slow", True, 3),
                                     ("slow", False, 9)])
    yield "trackpad bursts", lambda: run_case(
        s, "speed-trackpad", 12, actions=[("burst", True, 6, {"batch": 5, "gap": 0.012}),
                                          ("burst", False, 8, {"batch": 5, "gap": 0.012}),
                                          ("burst", True, 20, {"side": "<"}),
                                          ("burst", False, 40)])
    yield "reversing, pages", lambda: run_case(
        s, "speed-pages", 10, pre_up=30,
        actions=[("seq", [(True, 7), (False, 11), (True, 3)]), ("page", True, 2),
                 ("page", False, 4), ("slow", True, 3)])


def cases_rep(s):
    yield "repeated rows", lambda: run_case(
        s, "repeated", 6, pre_up=12, actions=[("slow", True, 6), ("slow", False, 10),
                                              ("burst", True, 15), ("burst", False, 25)])


SESSIONS = [("long", dict(blocks=4), cases_long), ("more", dict(blocks=3), cases_more),
            ("short", dict(blocks=1, block_rows=10), cases_short),
            ("repeated", dict(blocks=3, rep=True), cases_rep),
            ("small", dict(blocks=3, size=(80, 24)), cases_small),
            ("live", dict(blocks=2), cases_live),
            ("late", dict(blocks=3, late=True), cases_late),
            ("speed 0.5", dict(blocks=3, speed=0.5), cases_speed),
            ("speed 2.5", dict(blocks=3, speed=2.5), cases_speed),
            ("speed 3", dict(blocks=3, speed=3), cases_speed)]


def main():
    only = sys.argv[sys.argv.index("--only") + 1] if "--only" in sys.argv else None
    repeat = int(sys.argv[sys.argv.index("--repeat") + 1]) if "--repeat" in sys.argv else 1
    root = tempfile.mkdtemp(prefix="peekme-scroll-")
    total = passed = 0
    for sname, kw, cases in SESSIONS:
        todo = None
        if only:
            probe = list(cases(None))
            todo = [n for n, _ in probe if only in n or only in sname]
            if not todo:
                continue
        out = os.path.join(root, sname)
        os.makedirs(out, exist_ok=True)
        s = Session(out, kw["blocks"], kw.get("rep", False), kw.get("size", (120, 40)),
                    kw.get("block_rows", 40), kw.get("late", False), kw.get("speed"))
        try:
            for _ in range(repeat):
                for name, fn in cases(s):
                    if todo is not None and name not in todo:
                        continue
                    total += 1
                    case = fn()
                    if case.errors:
                        print(f"FAIL  {sname}|{name}  <- " + "; ".join(case.errors), flush=True)
                    else:
                        passed += 1
                        print(f"ok    {sname}|{name}", flush=True)
        finally:
            s.close()
    print(f"{passed}/{total} passed. Artifacts: {root}")
    sys.exit(0 if passed == total else 1)


if __name__ == "__main__":
    main()
