#!/usr/bin/env python3
"""A tiny full-screen TUI that behaves like the agent CLIs toward the terminal.

    python3 fakeagent.py --style claude|copilot|codex --log FILE

It draws on the alternate screen with any-motion SGR mouse on, like Claude Code
(fullscreen), Copilot CLI and Codex. A mouse drag selects text, and the agent
reports it the way the real one does:

    claude   OSC 52 `c` (clipboard), frames wrapped in ?2026
    copilot  OSC 52 `p!;<b64>;`, no ?2026 frames, modifyOtherKeys 2 requested
    codex    the selection drawn in reverse video, frames wrapped in ?2026

Inside tmux ($TMUX set) Claude Code and Codex copy with `tmux load-buffer -w -`
instead of OSC 52, and Copilot wraps its OSC 52 in tmux passthrough. So does
this agent.

The input box is `❯` (Codex: `›`) between two rules, like the real ones. Every
chunk of input is appended to the log as JSON, so a test can tell whether a key
reached the agent or was taken by peekme. Ctrl+D quits.
"""
import argparse, base64, json, os, re, select, signal, subprocess, sys, termios, time, tty

LINES = [
    "● alpha bravo charlie delta echo",
    "  foxtrot golf hotel india juliet",
    "  kilo lima mike november oscar",
    "  papa quebec romeo sierra tango",
]
MOUSE = re.compile(rb"\x1b\[<(\d+);(\d+);(\d+)([Mm])")


class Agent:
    def __init__(self, style, log):
        self.style = style
        self.log = open(log, "a", buffering=1)
        self.buf = ""
        self.sel = None  # (row, col_start, col_end) on screen, 1-based
        self.press = None
        self.resize()

    def resize(self):
        size = os.get_terminal_size(sys.stdout.fileno())
        self.cols, self.rows = size.columns, size.lines

    def out(self, s):
        os.write(sys.stdout.fileno(), s.encode())

    def record(self, **kw):
        kw["t"] = round(time.time(), 3)
        self.log.write(json.dumps(kw, ensure_ascii=False) + "\n")

    def draw(self):
        sync = self.style != "copilot"
        s = "\x1b[?2026h" if sync else ""
        s += "\x1b[?25l\x1b[H\x1b[2J"
        s += f"\x1b[1;1Hfakeagent ({self.style})"
        for i, line in enumerate(LINES):
            row = 3 + i
            if self.style == "codex" and self.sel and self.sel[0] == row:
                _, a, b = self.sel
                line = line[: a - 1] + "\x1b[7m" + line[a - 1 : b] + "\x1b[0m" + line[b:]
            s += f"\x1b[{row};1H{line}"
        rule = "─" * self.cols
        prompt = "›" if self.style == "codex" else "❯"
        s += f"\x1b[{self.rows - 3};1H{rule}"
        s += f"\x1b[{self.rows - 2};1H{prompt} {self.buf}"
        s += f"\x1b[{self.rows - 1};1H{rule}"
        s += f"\x1b[{self.rows};1H  fake status line"
        s += f"\x1b[{self.rows - 2};{3 + len(self.buf)}H\x1b[?25h"
        if sync:
            s += "\x1b[?2026l"
        self.out(s)

    def screen_text(self, row, a, b):
        if not 3 <= row < 3 + len(LINES):
            return ""
        return LINES[row - 3][a - 1 : b]

    def mouse(self, b, x, y, release):
        if release:
            if self.press and self.press != (x, y) and self.press[1] == y:
                a, e = sorted((self.press[0], x))
                text = self.screen_text(y, a, e)
                self.sel = (y, a, e)
                self.record(kind="selected", text=text)
                if text.strip():
                    self.copy(text)
                self.draw()
            self.press = None
        elif b & 64:
            pass  # wheel
        elif b & 32:
            pass  # motion
        else:
            self.press = (x, y)
            if self.sel:
                self.sel = None
                self.draw()

    def copy(self, text):
        b64 = base64.b64encode(text.encode()).decode()
        in_tmux = bool(os.environ.get("TMUX"))
        if self.style in ("claude", "codex") and in_tmux:
            r = subprocess.run(["tmux", "load-buffer", "-w", "-"], input=text.encode(),
                               capture_output=True)
            self.record(kind="copied", via="tmux load-buffer", code=r.returncode)
        elif self.style == "claude":
            self.out(f"\x1b]52;c;{b64}\x07")
            self.record(kind="copied", via="osc52")
        elif self.style == "copilot":
            seq = f"\x1b]52;p!;{b64};\x07"
            if in_tmux:
                seq = "\x1bPtmux;" + seq.replace("\x1b", "\x1b\x1b") + "\x1b\\"
            self.out(seq)
            self.record(kind="copied", via="osc52" + (" in tmux passthrough" if in_tmux else ""))

    def input(self, data):
        self.record(kind="input", raw=data.decode("utf-8", "replace"), hex=data.hex())
        rest = data
        while rest:
            m = MOUSE.match(rest)
            if m:
                b, x, y = int(m[1]), int(m[2]), int(m[3])
                self.mouse(b, x, y, m[4] == b"m")
                rest = rest[m.end():]
                continue
            if rest[:1] == b"\x04":
                return False
            if rest[:1] == b"\r":
                self.record(kind="submit", text=self.buf)
                self.buf = ""
                rest = rest[1:]
                continue
            if rest[:1] == b"\x7f":
                self.buf = self.buf[:-1]
                rest = rest[1:]
                continue
            if rest[:1] == b"\x1b":
                # Some other sequence: skip it whole (CSI/OSC/SS3) or just ESC.
                m = re.match(rb"\x1b(\[[0-?]*[ -/]*[@-~]|\][^\x07]*\x07|O.|.)?", rest, re.S)
                rest = rest[m.end():] if m and m.end() else rest[1:]
                continue
            n = 1
            while n < len(rest) and rest[n] != 0x1b and rest[n:n + 1] not in (b"\r", b"\x7f", b"\x04"):
                n += 1
            self.buf += rest[:n].decode("utf-8", "replace")
            rest = rest[n:]
        self.draw()
        return True


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--style", choices=["claude", "copilot", "codex"], default="claude")
    ap.add_argument("--log", required=True)
    args = ap.parse_args()
    agent = Agent(args.style, args.log)
    fd = sys.stdin.fileno()
    saved = termios.tcgetattr(fd)
    tty.setraw(fd)
    modes = "\x1b[?1049h\x1b[?1000h\x1b[?1002h\x1b[?1003h\x1b[?1006h\x1b[?2004h"
    if args.style == "copilot":
        modes += "\x1b[>4;2m"
    agent.out(modes)
    signal.signal(signal.SIGWINCH, lambda *_: (agent.resize(), agent.draw()))
    agent.record(kind="start", style=args.style, cols=agent.cols, rows=agent.rows)
    agent.draw()
    try:
        while True:
            try:
                r, _, _ = select.select([fd], [], [], 0.5)
            except InterruptedError:
                continue
            if not r:
                continue
            data = os.read(fd, 4096)
            if not data or not agent.input(data):
                break
    finally:
        agent.out("\x1b[?1006l\x1b[?1003l\x1b[?1002l\x1b[?1000l\x1b[?2004l\x1b[?1049l")
        termios.tcsetattr(fd, termios.TCSADRAIN, saved)
        agent.record(kind="exit")


if __name__ == "__main__":
    main()
