#!/usr/bin/env python3
"""Scrolling Codex's chat with a peek box open: real Codex (and Claude Code,
Copilot, Kiro, Pi in sessions of their own), full screen, Linux,
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

A chat with real prompts (a saved session, see chat_session.py) adds Codex's
prompt header: while the top of the screen is in an answer, its top row shows
that answer's prompt instead of the chat, and the chat rows start below it.
Codex's place in the chat still moves by its count, so at a turn boundary
the rows on screen move one row less or more as the header comes and goes;
the box moves with them.

The terminal answers Codex's colour queries as GNOME Terminal does (Codex
then tints its input area, a row taller above the prompt line), except in
the sessions marked `colors=False`, like a terminal that does not answer.

    python3 tests/e2e/scroll.py [--session NAME] [--only NAME] [--repeat N]
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
import chat_session  # noqa: E402
import claude_session  # noqa: E402
import copilot_session  # noqa: E402
import kiro_session  # noqa: E402
import pi_session  # noqa: E402
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

    def turn(self, way, now=None):
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


class ClaudeWheel:
    """Claude Code's rows for a turn of the wheel, as it counts them (2.1.299,
    the "window (native)" mode of Linux and macOS terminals): `base` rows
    (1, or CLAUDE_CODE_SCROLL_SPEED), more as turns come within 40 ms of each
    other, none for the first turn the other way, and a decaying "wheel
    mode" after a quick reversal. `now` is in milliseconds."""

    def __init__(self, base=1.0, accel=True):
        self.base, self.accel, self.speed = base, accel, base
        self.time, self.mult, self.dir = -1e9, base, 0
        self.pending_flip = self.wheel_mode = False
        self.burst = 0

    def turn(self, way, now=None):
        """Rows the turn moves the chat, down is plus."""
        d = -1 if way < 0 else 1
        return d * self.rows(d, time.monotonic() * 1000 if now is None else now)

    def rows(self, d, now):
        if self.wheel_mode and now - self.time > 1500:
            self.wheel_mode, self.burst, self.mult = False, 0, self.base
        if self.pending_flip:
            self.pending_flip = False
            if d != self.dir or now - self.time > 200:
                self.dir, self.time, self.mult = d, now, self.base
                return max(1, math.floor(self.mult))
            self.wheel_mode = True
        gap = now - self.time
        if d != self.dir and self.dir != 0:
            self.pending_flip, self.time = True, now
            return 0
        self.dir, self.time = d, now
        if self.wheel_mode:
            if gap < 5:
                self.burst += 1
                if self.burst >= 5:
                    self.wheel_mode, self.burst, self.mult = False, 0, self.base
                else:
                    return 1
            else:
                self.burst = 0
        if self.wheel_mode and self.accel:
            decay = 0.5 ** (gap / 150)
            cap = max(15 * min(self.base, 1), self.base * 2)
            self.mult = min(cap, 1 + (self.mult - 1) * decay + 15 * decay, self.mult + 3)
            return max(1, math.floor(self.mult))
        if gap > 40 or not self.accel:
            self.mult = self.base
        else:
            self.mult = min(max(6 * min(self.base, 1), self.base * 2), self.mult + 0.3)
        return max(1, math.floor(self.mult))


class CopilotWheel:
    """GitHub Copilot CLI's rows for a turn of the wheel (1.0.98): 3."""

    speed = 3.0

    def turn(self, way, now=None):
        return 3 if way > 0 else -3


class KiroWheel:
    """Kiro CLI's rows for a turn of the wheel (2.28.0): its setting
    `chat.fullscreenWheelRows`, 1 to 3, default 2."""

    def __init__(self, rows=2):
        self.speed = float(rows)

    def turn(self, way, now=None):
        return int(self.speed) if way > 0 else -int(self.speed)


class PiWheel:
    """Pi's rows for a wheel event (1.3.0, pi-tui's WheelScrollAccelerator):
    `fullscreenWheelScrollLines` when it is a number; else one row for an
    event that starts a gesture (the other way, or 200 ms after the last) or
    comes within 5 ms of the last, and min(6, max(1, 100 ms / average gap))
    plus the fraction carried otherwise. `now` is in milliseconds."""

    def __init__(self, rows=None):
        self.fixed = rows
        self.speed = float(rows or 1)
        self.last, self.way, self.avg, self.carry = None, 0, None, 0.0

    def turn(self, way, now=None):
        d = -1 if way < 0 else 1
        if self.fixed:
            return d * self.fixed
        now = time.monotonic() * 1000 if now is None else now
        gap = math.inf if self.last is None else now - self.last
        same = d == self.way and gap <= 200
        self.last, self.way = now, d
        if not same:
            self.avg, self.carry = None, 0.0
            return d
        if gap < 5:
            return d
        self.avg = gap if self.avg is None else (self.avg + gap) / 2
        lines = min(6.0, max(1.0, 100 / self.avg)) + self.carry
        whole = math.floor(lines)
        self.carry = lines - whole
        return d * whole


BOX = ("╭", "│", "╰")
# Claude Code's hint on the last chat row while scrolled up.
CLAUDE_HINT = "Jump to bottom (ctrl+End) ↓"
# Pi's, over its last chat row.
PI_HINT = "↓ Jump to latest message · Ctrl+End"
LABEL = re.compile(r"\bb\d+r\d\d\b")
# A box's bottom border saying how far its text scrolled ("12/30").
SCROLLED = re.compile(r"╰[^╯]*\s\d+/\d+\s[^╯]*╯")


class AnyRow(str):
    """Codex's prompt header on the top row: stands for whatever chat row is
    under it (it compares equal to any row)."""

    def __eq__(self, other):
        return True

    def __ne__(self, other):
        return False

    __hash__ = str.__hash__


class HintRow(str):
    """Claude's last chat row with its hint over it: it matches a chat row
    that reads the same outside the hint's columns (a column of room each
    side)."""

    def __new__(cls, row, hint=CLAUDE_HINT):
        obj = super().__new__(cls, row)
        obj.start = row.index(hint)
        obj.end = obj.start + len(hint)
        return obj

    def __eq__(self, other):
        n = max(len(self), len(other), self.end + 1)
        a, b = str(self).ljust(n), str(other).ljust(n)
        lo, hi = max(0, self.start - 1), self.end + 1
        return a[:lo] == b[:lo] and a[hi:] == b[hi:]

    def __ne__(self, other):
        return not self.__eq__(other)

    __hash__ = str.__hash__


class NoticeRow(str):
    """Pi's row with its " Copied! " notice at the right end, drawn over
    whatever chat text was there: it matches a chat row that reads the same
    left of the notice."""

    def __new__(cls, row, cut):
        obj = super().__new__(cls, row[:cut].rstrip())
        obj.cut = cut
        return obj

    def __eq__(self, other):
        return str(other)[:self.cut].rstrip() == str(self)

    def __ne__(self, other):
        return not self.__eq__(other)

    __hash__ = str.__hash__


def view(chat, s, holes=False):
    """`chat` as it is compared with the chat: a prompt header on its top row
    matches any row, and Claude's hint on its last row is not chat. With
    `holes`, a blank row matches any row too: Claude leaves rows blank for a
    frame or two while it scrolls fast, as it does without peekme (a row with
    text must still be the right one)."""
    if s is None:
        return chat
    chat = list(chat)
    if holes and s.agent == "claude":
        chat = [AnyRow(r) if not r.strip() else r for r in chat]
    if s.header and chat and chat[0].startswith(s.header):
        chat[0] = AnyRow(chat[0])
    if s.agent == "claude" and chat and CLAUDE_HINT in chat[-1]:
        bare = chat[-1].replace(CLAUDE_HINT, "").strip()
        chat[-1] = AnyRow(chat[-1]) if holes and not bare else HintRow(chat[-1])
    if s.agent == "pi" and chat and PI_HINT in chat[-1]:
        chat[-1] = HintRow(chat[-1], PI_HINT)
    return chat


def wheel_bytes(up, x, y, side=False):
    b = (66 if up else 67) if side else (64 if up else 65)
    return f"\x1b[<{b};{x};{y}M".encode()


class Session:
    """One Codex (or Claude Code) under peekme, with a chat made of labelled rows."""

    def __init__(self, out, blocks, rep=False, size=(120, 40), block_rows=40, late=False,
                 speed=None, prompts=False, colors=True, agent="codex", sidebar=False,
                 answers=None):
        m.COLS, m.ROWS = size
        self.out = out
        self.used = set()
        # Pi gets its turns in bursts 150 ms apart (see src/pi/scroll.rs): a
        # long burst takes it a while.
        self.burst_wait = 4.0 if late else (1.0 if agent == "pi" else 0.15)
        self.lines_file = os.path.join(out, "lines")
        self.set_answer(6)
        self.agent = agent
        # Claude takes turns of the wheel 40 ms apart or more as one row each,
        # and closer ones faster: bursts stay well inside that. Pi takes
        # events closer than 5 ms as a row each, else up to 6 (100 ms over
        # the average gap): its bursts go in one write (None), a row each,
        # unless a case asks for a gap.
        self.burst_gap = None if agent == "pi" else (0.012 if agent == "claude" else 0.008)
        self.child_log = os.path.join(out, "child.bin")
        self.child_at = {}
        prompts = prompts or agent in ("claude", "copilot", "kiro", "pi")
        self.prompts = prompts
        # Codex and Claude pin the prompt of what is on screen on the top row;
        # Copilot shows no such header.
        self.header = None if agent in ("copilot", "kiro", "pi") else (
            ("❯ q" if agent == "claude" else "› q") if prompts else None)
        # Screen row of the chat's first row (Copilot: under its tab bar),
        # and frames with the box drawn above it.
        self.top, self.above = 0, []
        # Columns left of Copilot's chat: its sidebar and divider, when open.
        self.left = 0
        extra, resume, self.codex_home, self.claude_home = {}, [], None, None
        if agent == "pi":
            # A saved chat in an agent folder of its own (full screen is Pi's default).
            agent_dir, path = pi_session.pi_agent(out, blocks, block_rows, long_prompt=(2,),
                                                  repeated=(1,) if rep else (),
                                                  wheel_rows=int(speed) if speed else None)
            extra = {"PI_CODING_AGENT_DIR": agent_dir, "PI_OFFLINE": "1",
                     "PI_SKIP_VERSION_CHECK": "1", "PI_TELEMETRY": "0"}
            resume = ["--session", path]
        elif agent == "kiro":
            # A saved chat in a KIRO_HOME of its own, starting full screen.
            home, sid = kiro_session.kiro_home(out, blocks, block_rows, long_prompt=(2,),
                                               repeated=(1,) if rep else (),
                                               wheel_rows=int(speed) if speed else None)
            extra = {"KIRO_HOME": home}
            resume = ["chat", "--resume-id", sid]
        elif agent == "copilot":
            # A saved chat in a COPILOT_HOME of its own, trusting this folder.
            home, sid = copilot_session.copilot_home(out, blocks, block_rows, long_prompt=(2,),
                                                     repeated=(1,) if rep else ())
            extra = {"COPILOT_HOME": home}
            resume = ["--no-auto-update", "--resume", sid]
        elif agent == "claude":
            # A saved chat in a config folder of its own, full screen.
            home, sid = claude_session.config_dir(out, blocks, block_rows, long_prompt=(2,),
                                                  repeated=(1,) if rep else ())
            extra, self.claude_home = {"CLAUDE_CONFIG_DIR": home}, home
            resume = ["--settings", '{"tui":"fullscreen"}', "--resume", sid]
        elif prompts:
            # A saved chat of `blocks` turns, each a prompt and an answer.
            home, sid = chat_session.codex_home(out, blocks, block_rows, long_prompt=(2,))
            extra, resume, self.codex_home = {"CODEX_HOME": home}, ["resume", sid], home
        env = m.clean_env(**extra,
            TERM="xterm-256color", PEEKME_FAKE_EXPLAINER="1",
            PEEKME_FAKE_LINES="@" + self.lines_file,
            PEEKME_EVENT_LOG=os.path.join(out, "events.jsonl"),
            PEEKME_CHILD_LOG=self.child_log,
            DISPLAY="", WAYLAND_DISPLAY="")
        # This test may run inside Claude Code, whose child marker would turn
        # off saving and add a footer row.
        env.pop("CLAUDE_CODE_CHILD_SESSION", None)
        # With the canned answers peekme keeps none between runs, unless told where.
        if answers:
            env["PEEKME_ANSWERS_DIR"] = answers
        # `late`: Codex behind a relay that delays its scroll after its first,
        # empty answer to each wheel turn (a slower machine).
        relay = [sys.executable, os.path.join(HERE, "late_pty.py")] if late else []
        # Codex's wheel speed: its default 1.0, or set on its command line.
        setting = ["-c", f"tui.mouse_scroll_speed={speed}"] if speed else []
        if agent == "pi":
            self.model = PiWheel(int(speed) if speed else None)
            argv = [m.PEEKME, ra.real_binary("pi")] + resume
        elif agent == "kiro":
            self.model = KiroWheel(int(speed) if speed else 2)
            argv = [m.PEEKME, ra.real_binary("kiro-cli")] + resume
        elif agent == "copilot":
            self.model = CopilotWheel()
            argv = [m.PEEKME, ra.real_binary("copilot")] + resume
        elif agent == "claude":
            self.model = ClaudeWheel(speed or 1.0)
            if speed:
                env["CLAUDE_CODE_SCROLL_SPEED"] = str(speed)
            argv = [m.PEEKME, ra.real_binary("claude")] + resume
        else:
            self.model = Wheel(speed or 1.0)
            argv = [m.PEEKME] + relay + [ra.real_binary("codex"), "-c",
                                          "check_for_update_on_startup=false"] + setting + resume
        self.argv, self.env, self.colors = argv, env, colors
        self.t = m.Term(argv, env, os.path.join(out, "terminal.bin"), colors=colors)
        self.ready()
        if sidebar:
            # The left arrow with nothing typed opens Copilot's sidebar.
            self.t.send(b"\x1b[D")
            time.sleep(1.5)
            rows = self.t.screen()
            self.left = 1 + next(c for c in range(8, m.COLS // 2)
                                 if all(len(r) > c and r[c] == "│" for r in rows))
        for b in range(0 if prompts else blocks):
            fill = "same row of a repeated output" if rep and b == 1 else None
            self.run_block(b, fill, block_rows)
        self.R = self.chat_rows()
        self.T = self.read_chat()
        if len(self.T) < blocks * block_rows:
            raise RuntimeError(f"read only {len(self.T)} rows of the chat")
        self.find_prompts()
        self.pos = self.end_pos
        with open(os.path.join(out, "T.json"), "w") as f:
            json.dump({"T": self.T, "R": self.R}, f)

    def set_answer(self, lines, delay=0):
        with open(self.lines_file, "w") as f:
            f.write(f"{lines} {delay}")

    def screen(self):
        return [r.rstrip() for r in self.t.screen()]

    def ready(self):
        end = time.time() + 60
        while time.time() < end:
            txt = "\n".join(self.screen())
            if self.prompts and LABEL.search(txt):
                time.sleep(3)
                return
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
        """Chat rows: all above the status (Codex) or notice (Claude) row,
        two above the prompt line."""
        s = self.screen()
        if self.agent == "pi":
            # Above the blank row over the upper of Pi's two lowest rules.
            rules = [i for i, r in enumerate(s) if r.startswith("─") and r.count("─") * 5 >= m.COLS * 4]
            return rules[-2] - 1
        if self.agent == "kiro":
            # Above the blank row over Kiro's rule, the one above its prompt.
            prompt = max(i for i, r in enumerate(s) if r.startswith("›"))
            rule = max(i for i in range(prompt) if s[i].startswith("─"))
            return rule - 1 if not s[rule - 1].strip() else rule
        if self.agent == "copilot":
            # The rows of Copilot's scrollbar, in the last column.
            bar = [i for i, r in enumerate(s) if len(r) == m.COLS and r.endswith("┃")]
            self.top = bar[0]
            if bar != list(range(bar[0], bar[-1] + 1)):
                raise RuntimeError(f"Copilot's scrollbar is not one run: {bar}")
            return len(bar)
        if self.agent == "claude":
            composer = max(i for i, r in enumerate(s) if r.startswith("❯") and i > 0
                           and s[i - 1].strip() and set(s[i - 1].strip()) == {"─"})
        else:
            composer = max(i for i, r in enumerate(s) if r.startswith("›"))
        return composer - 2

    def mark(self):
        with self.t.lock:
            at = len(self.t.data)
        # Claude's own screen at this moment, to see where its chat really is.
        if self.agent == "claude" and os.path.exists(self.child_log):
            self.child_at[at] = os.path.getsize(self.child_log)
        return at

    def claude_places(self, marks):
        """Where Claude's own chat was (its top chat row in T) at each of
        `marks`, from its output under peekme; None where it does not fit
        one place."""
        sizes = [self.child_at.get(o) for o in marks]
        stops = sorted({x for x in sizes if x})
        if not stops:
            return [None] * len(marks)
        out = subprocess.run([VTFRAMES, self.child_log, str(m.COLS), str(m.ROWS), "0", "0"]
                             + [str(x) for x in stops], capture_output=True, text=True).stdout
        screens = {json.loads(l)["off"]: [r.rstrip() for r in json.loads(l)["rows"]]
                   for l in out.splitlines()}
        R, T = self.R, self.T
        places = {}
        for at, rows in screens.items():
            chat = view(rows[:R], self, holes=True)
            ks = [k for k in range(len(T) - R + 1) if T[k:k + R] == chat]
            places[at] = ks[0] if len(ks) == 1 else None
        return [places.get(x) if x else None for x in sizes]

    def find_prompts(self):
        """Codex's prompts in T, for where it shows its prompt header: a
        prompt's rows are the two blank rows before it, its own rows and a
        blank row after (Codex 0.162.1, seen on its screen)."""
        self.cells = set()
        first = None
        if self.prompts:
            for p, row in enumerate(self.T):
                if row.startswith("› q"):
                    e = p + 1
                    while e < len(self.T) and self.T[e].strip():
                        e += 1
                    self.cells |= set(range(p - 2, e + 1))
                    first = p - 2 if first is None else first
        self.first_prompt = first
        # Where Codex rests at the end of its chat: the last rows under the
        # header, or a full screen of chat when a prompt is at the top.
        n = len(self.T)
        if self.agent == "claude":
            # Claude's header covers the top row and moves nothing.
            self.first_prompt = None
        self.end_pos = n - (self.R - 1) if self.header_at(n - (self.R - 1)) else n - self.R

    def header_at(self, s):
        """Codex shows its prompt header with chat row `s` at the top of the chat."""
        return (self.first_prompt is not None and s > self.first_prompt
                and s not in self.cells)

    def place_after(self, p, d):
        """The agent's place after a turn of `d` rows from `p`. Copilot goes
        on to the bottom from a row short of it (it follows new text there)."""
        q = max(0, min(self.end_pos, p + d))
        if self.agent == "copilot" and q >= self.end_pos - 1:
            q = self.end_pos
        return q

    def moved(self, rows):
        """Codex's place after turns of `rows`: (place, the rows it took)."""
        pos = self.place_after(self.pos, rows)
        took = pos - self.pos
        self.pos = pos
        return took

    def wheel(self, up, n=1, gap=0.0, batch=1, side=None, at=None, track=True):
        """Turn the wheel; returns the rows each turn moves the agent's chat.
        A `gap` of None sends the `n` turns in one write."""
        if gap is None:
            n, batch, gap = 1, n * batch, 0.0
        # Codex scrolls for the wheel over its chat only.
        x, y = (m.COLS // 2, self.R // 2) if at in (None, "auto", "side") else at
        y += self.top
        rows = []
        for _ in range(n):
            if at == "side":
                # Over Copilot's sidebar, beside a chat row: it scrolls the
                # sidebar, not the chat.
                chat = self.chat()
                x = 5
                y = self.top + 1 + min((r for r in range(self.R) if chat[r][:1] not in BOX),
                                       key=lambda r: abs(r - self.R // 2))
            if at == "auto":
                # A row the box does not cover (a box scrolls its own text).
                chat = self.chat()
                y = self.top + 1 + min((r for r in range(self.R) if chat[r][:1] not in BOX),
                                       key=lambda r: abs(r - self.R // 2))
            b = wheel_bytes(up, x, y) * batch
            if side:
                b += b"".join(wheel_bytes(s == "<", x, y, side=True) for s in side)
            self.t.send(b)
            now = time.monotonic() * 1000
            rows += [0 if at == "side" else self.model.turn(-1 if up else 1, now)
                     for _ in range(batch)]
            if track and at in (None, "auto"):
                for r in rows[-batch:]:
                    self.moved(r)
            if gap:
                time.sleep(gap)
        return rows

    def strip(self, rows):
        """Rows without Copilot's sidebar, which stays put; the box covers it.
        Pi's scrollbar in the last column is not chat either."""
        if self.agent == "pi":
            # Its " Copied! " notice after a drag is not chat either.
            return [r if r[:1] in BOX else
                    NoticeRow(r, m.COLS - 10) if r[:m.COLS - 1].rstrip().endswith("Copied!") else
                    r[:m.COLS - 1].rstrip() for r in rows]
        return [r if r[:1] in BOX else r[self.left:] for r in rows] if self.left else rows

    def chat(self):
        return self.strip(self.screen()[self.top:self.top + self.R])

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
        self.t.send(b"\x1b[F" if self.agent == "kiro" else b"\x1b[1;5F")  # (Ctrl+)End
        self.settle()
        self.pos = getattr(self, "end_pos", 0)

    def read_chat_claude(self):
        """Claude's chat, from the top, ten turns of the wheel at a time (60 ms
        apart: a row each). Its header covers the top row and its hint the
        last one while scrolled; neither shows at the bottom."""
        self.t.send(b"\x1b[1;5H")  # Ctrl+Home
        self.settle()
        R = self.R
        cur = self.chat()
        T = cur[:R - 1]
        while True:
            self.wheel(False, 10, 0.06, track=False)
            self.settle()
            nxt = self.chat()
            if nxt == cur:
                break
            k = next((k for k in range(1, R - 2) if nxt[1:R - 1 - k] == cur[1 + k:R - 1]), None)
            if k is None:
                raise RuntimeError("reading the chat: screens do not overlap")
            T += nxt[R - 1 - k:R - 1]
            cur = nxt
        if CLAUDE_HINT in cur[R - 1] or T[-1] != cur[R - 2]:
            raise RuntimeError("reading the chat: not at its end")
        T.append(cur[R - 1])
        return T

    def read_chat_pi(self):
        """Pi's chat, from the top, a page at a time (its rows less 4). Its
        hint covers the last row while scrolled up, and not at the bottom."""
        self.t.send(b"\x1b[1;5H")  # Ctrl+Home
        self.settle()
        R = self.R
        cur = self.chat()
        T = cur[:R - 1]
        while True:
            self.t.send(b"\x1b[6~")
            self.settle()
            nxt = self.chat()
            if nxt == cur:
                break
            k = next((k for k in [R - 4] + list(range(1, R - 1))
                      if nxt[:R - 1 - k] == cur[k:R - 1]), None)
            if k is None:
                raise RuntimeError("reading the chat: screens do not overlap")
            T += nxt[R - 1 - k:R - 1]
            cur = nxt
        if PI_HINT in cur[R - 1] or T[-1] != cur[R - 2]:
            raise RuntimeError("reading the chat: not at its end")
        T.append(cur[R - 1])
        return T

    def read_chat(self):
        if self.agent == "claude":
            T = self.read_chat_claude()
        elif self.agent == "pi":
            T = self.read_chat_pi()
        else:
            T = self.read_chat_paged()
        if len(set(r for r in T if LABEL.search(r))) != len([r for r in T if LABEL.search(r)]):
            raise RuntimeError("labelled rows are not unique")
        # A block of repeated rows has 30 of them: read where rows repeat,
        # screens fit more than one way.
        same = sum(r.strip().lstrip("-•· ").strip() == "same row of a repeated output" for r in T)
        if same not in (0, 30):
            raise RuntimeError(f"reading the chat: {same} repeated rows, not 30")
        return T

    def read_chat_paged(self):
        self.t.send(b"\x1b[H" if self.agent == "kiro" else b"\x1b[1;5H")  # (Ctrl+)Home
        self.settle()
        cur = self.chat()
        T = list(cur)
        # Under a prompt header only the rows below the top one are chat.
        h = 1 if self.header else 0
        while True:
            # PageDown; with a prompt header a page can leave no chat row in
            # common, so eight wheel turns there. Copilot's page is all its
            # rows: eight turns there too.
            # Copilot moves 3 rows a turn: as many turns as leave rows in
            # common, and that shift first where rows repeat.
            step = int(self.model.speed) if self.agent in ("copilot", "kiro") else 1
            turns = max(1, min(8, (self.R - 4) // step)) if step > 1 or self.agent == "kiro" else 8
            if h or self.agent in ("copilot", "kiro"):
                self.t.send(wheel_bytes(False, m.COLS // 2, self.top + self.R // 2) * turns)
            else:
                self.t.send(b"\x1b[6~")
            time.sleep(0.3)
            m.wait(lambda: self.chat()[h:] != cur[h:], 1.5, 0.05)
            time.sleep(0.1)
            nxt = self.chat()
            if nxt[h:] == cur[h:]:
                break
            first = step * turns if self.agent in ("copilot", "kiro") else (
                self.R - 4 if self.agent == "pi" else self.R - 1 - h)
            s = next((s for s in [first] + list(range(1, self.R - h))
                      if nxt[h:self.R - s] == cur[s + h:]), None)
            if s is None:
                raise RuntimeError("reading the chat: screens do not overlap")
            T += nxt[self.R - s:]
            cur = nxt
        return T

    def reopen(self, log):
        """Quit (peekme and the agent killed, as with a closed window) and
        open the same chat again under a new peekme, writing to `log`."""
        self.t.close()
        time.sleep(1.5)
        self.t = m.Term(self.argv, self.env, log, colors=self.colors)
        self.ready()

    def close(self):
        self.t.close()
        if self.codex_home:
            chat_session.forget_sign_in(self.codex_home)
        if self.claude_home:
            claude_session.forget_sign_in(self.claude_home)


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
    a, b = (a[0] + s.left, a[1] + s.top), (b[0] + s.left, b[1] + s.top)
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
        rows = [r.rstrip() for r in f["rows"]]
        # Above the chat (Copilot's tab bar) nothing of the box shows.
        if any(r[:1] in BOX for r in rows[:s.top]):
            s.above.append(f["off"])
        res.append((f["off"], s.strip(rows[s.top:])))
    return res


def box_run(chat):
    idx = [i for i, r in enumerate(chat) if r[:1] in BOX]
    if not idx:
        return None
    return idx[0], idx[-1] + 1, idx == list(range(idx[0], idx[-1] + 1))


def offsets(chat, T, I, H, ref, s=None, holes=False):
    """Where `chat` sits in V (a set: repeated rows can fit more than one
    place), or an error string. `ref` is the whole box once its text is done."""
    chat = view(chat, s, holes)
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
            if not top and not bottom:
                # A box taller than the chat, cut at both ends: a window of it.
                js = {j + H - len(ref) for j in range(len(ref) - n + 1) if ref[j:j + n] == rows}
                return {I + j for j in js} or "box cut at both ends is not a part of it"
            if top and bottom and rows != ref:
                return f"box is {n} rows, not its {H}" if n != H else "box text differs"
            if not top and rows != ref[H - n:]:
                return "box cut at the top is not its bottom part"
            if not bottom and rows != ref[:n]:
                return "box cut at the bottom is not its top part"
        if top:
            return {I - b0}
        if H is None or not bottom:
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
            ref = box_from_parts(fr, done_at, R, box_rows(s.agent, case.lines, sel_rows,
                                                          case.nested))
            H = len(ref) if ref else None
        if ref is None:
            case.err("never saw the whole box once its text was done")
            return
        # A box never scrolls its own text: it is as tall as all of it,
        # taller than the chat too.
        want_h = box_rows(s.agent, case.lines, sel_rows, case.nested)
        if not case.capped and len(sel_rows) == 1 and H != want_h:
            case.err(f"box is {H} rows, not {want_h} for {case.lines} lines")
        if not case.scrolled_early and H <= R:
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
                      ref if settled and not capped else None, s, holes=True)
        if isinstance(got, str):
            case.err(f"frame at byte {o}: {got}")
        elif settled:
            where[o] = got
        if o in s.above:
            case.err(f"frame at byte {o}: box drawn above the chat")
        if any(r[:1] in BOX and SCROLLED.search(r) for r in chat):
            case.err(f"frame at byte {o}: a box scrolls its own text")
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
    # Codex's place in its chat before each mark's turns (chats with prompts).
    pos, pos_at = case.s0, []
    for _, _, move in case.marks:
        pos_at.append(pos)
        for d in move or []:
            pos = s.place_after(pos, d)
    # Claude's count of a fast run can be a few rows off what it moves
    # (f122): its own screen says where its chat really went.
    places = s.claude_places([o for _, o, _ in case.marks]) if s.agent == "claude" else None
    place = None
    off = last = None
    for i, (name, o, move) in enumerate(case.marks):
        if o < done_at:
            continue
        rows = at.get(o) or next((r for x, r in reversed(fr) if x <= o), None)
        if rows is None:
            continue
        got = offsets(rows[:R], T, I, H, ref, s)
        if isinstance(got, str) or not got:
            if os.environ.get("SCROLL_DEBUG"):
                print(f"    {name:28} {got!r}")
            continue
        now_place = places[i] if places else None
        if places is not None and os.environ.get("SCROLL_DEBUG"):
            print(f"    {name:28} Claude's chat at {now_place} (end {s.end_pos})")
        if off is not None and move is not None and place is not None and now_place is not None:
            # Claude: the box moves just as Claude's chat did; at either end
            # the rest of the turns' rows are peekme's to show.
            moved = now_place - place
            rest = sum(move) - moved
            at_end = now_place in (0, s.end_pos) and rest * sum(move) > 0
            want = max(0, min(maxoff, off + moved + (rest if at_end else 0)))
        elif off is not None and move is not None:
            want, p = off, pos_at[i]
            for d in move:
                # The rows Codex moves, one less or more where its prompt
                # header comes or goes; the rest of the turn past either end
                # of its chat is peekme's to show.
                raw = max(0, min(s.end_pos, p + d))
                q = s.place_after(p, d)
                # Copilot's turn from a row short of its bottom goes a row on.
                shift = d + (q - raw) - (s.header_at(q) - s.header_at(p))
                want = max(0, min(maxoff, want + shift))
                p = q
        if off is not None and move is not None:
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
            # (Claude itself can step a few rows back and forth on the way;
            # its frames are each checked, its place at the marks too.)
            if sign and last is not None and len({d > 0 for d in move if d}) == 1 \
                    and s.agent != "claude":
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
        place = now_place
        prev_o = o
        if os.environ.get("SCROLL_DEBUG"):
            print(f"    {name:28} off {sorted(got)} of {maxoff}  (I={I} H={H} len(T)={len(T)})")
        if name.endswith("+1s") and prev is not None and at.get(prev[1]) is not None:
            if at[prev[1]][:R] != rows[:R]:
                case.err(f"{name}: the screen changed after the wheel stopped")
        prev = (name, o)


def box_from_parts(fr, done_at, R, H):
    """The whole box of `H` rows from frames after `done_at` that show only
    part of it: its top (cut at the bottom of the chat) and its bottom (cut
    at the top), where they overlap and agree. A box nearly as tall as the
    chat can pass from one to the other in one frame (Copilot's paced
    turns move several rows a frame)."""
    head = tail = []
    for o, rows in fr:
        run = box_run(rows[:R])
        if o < done_at or not run or not run[2]:
            continue
        part = rows[run[0]:run[1]]
        if part[0].startswith("╭") and run[1] == R and len(part) > len(head):
            head = part
        if part[-1].startswith("╰") and run[0] == 0 and len(part) > len(tail):
            tail = part
    if not head or not tail or len(head) + len(tail) <= H:
        return None
    lap = len(head) + len(tail) - H
    if head[len(head) - lap:] != tail[:lap]:
        return None
    return head + tail[lap:]


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


def box_rows(agent, lines, sel_rows, nested):
    """Rows of the fake answer's box: borders, the header (a row a selected
    row), a gap, then the lines, one list as the agent draws it (Kiro puts a
    blank row between its loose items); a box inside has the same, its header
    one row."""
    text = 2 * lines - 1 if agent == "kiro" else lines
    return text + 3 + len(sel_rows) + (text + 4 if nested else 0)


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
        # Pi counts events 10 ms apart as up to 6 rows each, and events in
        # one write as a row each.
        if s.agent == "pi":
            s.wheel(True, 1, batch=pre_up)
        else:
            s.wheel(True, pre_up, 0.01)
        s.settle()
    r1, r2, sel = select(s, sel_row, two_rows)
    case.s0 = s.pos
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
            rows = s.wheel(up, n, opts.get("gap", s.burst_gap), opts.get("batch", 1),
                           opts.get("side"))
            # A late Codex (the relay) takes a while to answer a burst.
            wait = s.burst_wait
            time.sleep(wait)
            label = f"burst {'up' if up else 'down'} {n}x{opts.get('batch', 1)}"
            case.marks.append((label + f" +{int(wait * 1000)}ms", s.mark(), rows))
            time.sleep(1.0)
            case.marks.append((label + " +1s", s.mark(), []))
        elif kind == "page":
            # PageUp / PageDown: peekme sends Codex a page of wheel turns.
            # Claude takes the keys itself: half its viewport (the chat rows
            # under its header) a press.
            _, up, n = a
            turns = max(1, int(rust_round((s.R - 1) / s.model.speed)))
            for i in range(n):
                s.t.send(b"\x1b[5~" if up else b"\x1b[6~")
                if s.agent == "claude":
                    rows = [(-1 if up else 1) * ((s.R - 1) // 2)]
                elif s.agent == "copilot":
                    # Copilot pages its chat itself: all its rows a press.
                    rows = [(-1 if up else 1) * s.R]
                elif s.agent == "kiro":
                    # Kiro too: its rows less 2.
                    rows = [(-1 if up else 1) * (s.R - 2)]
                elif s.agent == "pi":
                    # Pi too: its rows less 4.
                    rows = [(-1 if up else 1) * (s.R - 4)]
                else:
                    rows = [s.model.turn(-1 if up else 1) for _ in range(turns)]
                for r in rows:
                    s.moved(r)
                time.sleep(max(0.4, s.burst_wait))
                case.marks.append((f"page {'up' if up else 'down'} {i + 1}/{n}", s.mark(), rows))
        elif kind == "seq":
            # Several directions in one quick run: [(up, n), ...]
            moves = []
            for up, n in a[1]:
                moves += s.wheel(up, n, s.burst_gap)
            time.sleep(s.burst_wait)
            case.marks.append(("reversing burst +50ms", s.mark(), moves))
            time.sleep(1.0)
            case.marks.append(("reversing burst +1s", s.mark(), []))
        elif kind == "nested":
            # Pick a word in the box and peek at it: a box in the box.
            chat = s.chat()
            r = next(i for i, row in enumerate(chat) if "Line 2 of" in row)
            c = chat[r].index("Line 2 of") + 1
            y = s.top + r + 1
            s.t.send(f"\x1b[<0;{c};{y}M\x1b[<32;{c + 3};{y}M\x1b[<0;{c + 3};{y}m".encode())
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
        y = s.top + s.R + (1 if s.agent == "copilot" else 2)
        s.t.send(f"\x1b[<0;{m.COLS - 2};{y}M\x1b[<0;{m.COLS - 2};{y}m".encode())
    time.sleep(0.6)
    # Claude may still be drawing (a blank row, its header) after the box went.
    if s.agent == "claude":
        s.settle()
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
            case.err("after closing the agent's next scroll does not match the screen")
    else:
        # Claude can leave rows blank a while after a fast scroll, with or
        # without peekme (f128): a blank row stands for any.
        chat = view(chat, s, holes=True)
        ks = [k for k in range(len(s.T) - s.R + 1) if s.T[k:k + s.R] == chat]
        if not ks:
            case.err("after closing the screen is not the agent's chat")
            if os.environ.get("SCROLL_DEBUG"):
                print("\n".join(f"      {i:2} {r[:90]}" for i, r in enumerate(s.chat())))
        else:
            back = -s.wheel(True)[0]
            time.sleep(0.3)
            after = view(s.chat(), s, holes=True)
            # A prompt header coming or going moves the rows one less or more.
            near = (-1, 0, 1) if s.prompts else (0,)
            if not any(s.T[max(0, k - back + e):max(0, k - back + e) + s.R] == after
                       for k in ks for e in near):
                case.err("after closing the agent's next scroll does not match the screen")
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
        s, "tall", 10, lines=24,
        actions=[("slow", False, 4), ("slow", True, 4), ("burst", False, 30), ("burst", True, 30)])
    yield "box in the box, then scroll", lambda: run_case(
        s, "nested", 6, lines=2, nested=True,
        actions=[("nested",), ("slow", True, 3, "auto"), ("slow", False, 8, "auto"),
                 ("slow", True, 12, "auto")])
    # The wheel over the boxes: the chat scrolls, never a box's own text.
    # Together the two boxes are taller than the chat: 16 lines are 39 rows,
    # 12 are 54 for Kiro (a blank row between its loose items).
    yield "long box in a long box, wheel over them", lambda: run_case(
        s, "nested-long", 6, lines=12 if s and s.agent == "kiro" else 16, nested=True,
        actions=[("nested",), ("slow", False, 20), ("slow", True, 30), ("burst", False, 40),
                 ("burst", True, 40), ("page", False, 2), ("page", True, 2)])


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


def cases_prompts(s):
    # Four turns of 40 rows: the prompts sit about 45 rows apart, the third
    # one is long (four rows, cut short in the header).
    yield "box low, slow across a prompt both ways", lambda: run_case(
        s, "prompt-low", s.R - 6 if s else 0, actions=[("slow", True, 20), ("slow", False, 26)])
    yield "box high, slow across a prompt both ways", lambda: run_case(
        s, "prompt-high", 6, pre_up=8, actions=[("slow", True, 12), ("slow", False, 16),
                                                ("slow", True, 4)])
    yield "bursts over several prompts", lambda: run_case(
        s, "prompt-bursts", 12, actions=[("burst", True, 70), ("burst", False, 80),
                                         ("burst", True, 6), ("burst", False, 10)])
    yield "pages over prompts", lambda: run_case(
        s, "prompt-pages", 10, actions=[("page", True, 3), ("page", False, 4)])
    yield "long prompt at the top", lambda: run_case(
        s, "prompt-long", 14, pre_up=54, actions=[("slow", True, 9), ("slow", False, 12),
                                                  ("burst", True, 5), ("burst", False, 5)])


def cases_claude(s):
    """Claude Code and Copilot full screen: every Codex case that does not
    need Codex itself (its speed setting, its late relay, its writing)."""
    yield from cases_prompts(s)
    yield from cases_long(s)
    yield from cases_more(s)


def cases_sidebar(s):
    """Copilot with its sidebar open, left of the chat."""
    yield "sidebar: slow both ways past both ends", lambda: run_case(
        s, "sidebar-slow", 8, actions=[("slow", True, 6), ("slow", False, 14),
                                       ("burst", True, 40, {"gap": 0.003}), ("slow", False, 4)])
    yield "sidebar: selection over two rows", lambda: run_case(
        s, "sidebar-two", 9, two_rows=True, actions=[("slow", True, 4), ("burst", False, 10)])
    yield "sidebar: the wheel over the sidebar", lambda: run_case(
        s, "sidebar-wheel", 10, actions=[("slow", True, 3), ("slow", True, 3, "side"),
                                         ("slow", False, 2, "side"), ("slow", False, 5)])
    yield "sidebar: PageUp and PageDown", lambda: run_case(
        s, "sidebar-pages", 12, actions=[("page", True, 2), ("page", False, 3)])


def cases_rep(s):
    yield "repeated rows", lambda: run_case(
        s, "repeated", 6, pre_up=12, actions=[("slow", True, 6), ("slow", False, 10),
                                              ("burst", True, 15), ("burst", False, 25)])


SESSIONS = [("long", dict(blocks=4), cases_long), ("more", dict(blocks=3, colors=False), cases_more),
            ("short", dict(blocks=1, block_rows=10), cases_short),
            ("repeated", dict(blocks=3, rep=True), cases_rep),
            ("small", dict(blocks=3, size=(80, 24), colors=False), cases_small),
            ("live", dict(blocks=2), cases_live),
            ("late", dict(blocks=3, late=True), cases_late),
            ("speed 0.5", dict(blocks=3, speed=0.5), cases_speed),
            ("speed 2.5", dict(blocks=3, speed=2.5), cases_speed),
            ("speed 3", dict(blocks=3, speed=3), cases_speed),
            ("prompts", dict(blocks=4, prompts=True), cases_prompts),
            ("prompts speed 3", dict(blocks=4, prompts=True, speed=3), cases_prompts),
            ("claude", dict(blocks=4, agent="claude"), cases_claude),
            ("claude repeated", dict(blocks=3, agent="claude", rep=True), cases_rep),
            ("claude small", dict(blocks=3, agent="claude", size=(80, 24)), cases_small),
            ("copilot", dict(blocks=4, agent="copilot"), cases_claude),
            ("copilot repeated", dict(blocks=3, agent="copilot", rep=True), cases_rep),
            ("copilot small", dict(blocks=3, agent="copilot", size=(80, 24)), cases_small),
            ("copilot sidebar", dict(blocks=3, agent="copilot", sidebar=True), cases_sidebar),
            ("kiro", dict(blocks=4, agent="kiro"), cases_claude),
            ("kiro repeated", dict(blocks=3, agent="kiro", rep=True), cases_rep),
            ("kiro small", dict(blocks=3, agent="kiro", size=(80, 24)), cases_small),
            ("kiro speed 1", dict(blocks=3, agent="kiro", speed=1), cases_speed),
            ("kiro speed 3", dict(blocks=3, agent="kiro", speed=3), cases_speed),
            ("pi", dict(blocks=4, agent="pi"), cases_claude),
            ("pi repeated", dict(blocks=3, agent="pi", rep=True), cases_rep),
            ("pi small", dict(blocks=3, agent="pi", size=(80, 24)), cases_small),
            ("pi speed 1", dict(blocks=3, agent="pi", speed=1), cases_speed),
            ("pi speed 3", dict(blocks=3, agent="pi", speed=3), cases_speed)]


def main():
    only = sys.argv[sys.argv.index("--only") + 1] if "--only" in sys.argv else None
    repeat = int(sys.argv[sys.argv.index("--repeat") + 1]) if "--repeat" in sys.argv else 1
    session = sys.argv[sys.argv.index("--session") + 1] if "--session" in sys.argv else None
    root = tempfile.mkdtemp(prefix="peekme-scroll-")
    total = passed = 0
    for sname, kw, cases in SESSIONS:
        if session is not None and sname != session:
            continue
        todo = None
        if only:
            probe = list(cases(None))
            todo = [n for n, _ in probe if any(o in n or o in sname for o in only.split("|"))]
            if not todo:
                continue
        out = os.path.join(root, sname)
        os.makedirs(out, exist_ok=True)
        s = Session(out, kw["blocks"], kw.get("rep", False), kw.get("size", (120, 40)),
                    kw.get("block_rows", 40), kw.get("late", False), kw.get("speed"),
                    kw.get("prompts", False), kw.get("colors", True), kw.get("agent", "codex"),
                    kw.get("sidebar", False))
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
