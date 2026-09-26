#!/usr/bin/env python3
"""
flicker_test.py - does swapping to the alternate screen flicker in THIS terminal?

Draws a dense, colourful Codex-like screen, then repeatedly swaps to the
alternate screen and redraws an identical copy there, then swaps back - exactly
what codex-peek does when entering/leaving peek mode. If the swap is clean, the
screen looks perfectly still while it strobes 7 times a second.

Phases (4 s each):
  0 CONTROL  frame written in small chunks with pauses  -> you SHOULD see flicker
  1 SINGLE   swap + full frame in one write()           -> what we get without mode 2026
  2 SYNC     same, wrapped in DEC mode 2026              -> what we get with mode 2026

Run it in the terminal you actually use (also inside tmux / VS Code if you use
those):   python3 spikes/flicker_test.py
Stdlib only. Ctrl+C restores the terminal.
"""

import os
import select
import shutil
import sys
import termios
import time
import tty

OUT = sys.stdout.fileno()
IN = sys.stdin.fileno()
ESC = "\x1b"


def w(s):
    os.write(OUT, s.encode())


def query_mode_2026():
    """DECRQM: CSI ? 2026 $ p -> CSI ? 2026 ; Ps $ y (1/2 = supported)."""
    w(f"{ESC}[?2026$p")
    buf = b""
    end = time.time() + 0.5
    while time.time() < end:
        r, _, _ = select.select([IN], [], [], 0.05)
        if r:
            buf += os.read(IN, 64)
            if b"$y" in buf:
                break
    if b"2026;" not in buf:
        return "no reply (unsupported)"
    ps = buf.split(b"2026;")[1][:1]
    return {b"0": "not recognised", b"1": "supported (set)", b"2": "supported (reset)",
            b"3": "permanently set", b"4": "permanently disabled"}.get(ps, repr(buf))


def frame(cols, rows, label):
    """A full-screen, SGR-heavy frame, roughly like a busy Codex session."""
    words = "fn explain(ctx: &ContextPack) -> Result<Stream> { let span = locate(sel)?; }  "
    lines = []
    for r in range(rows - 1):
        g = 20 + (r * 3) % 40
        text = (words * (cols // len(words) + 2))[r % 17: r % 17 + cols]
        # alternate styled runs so the frame is many bytes, like real TUI output
        parts = []
        for i in range(0, cols, 8):
            chunk = text[i:i + 8]
            fg = 16 + (r * 7 + i) % 216
            parts.append(f"{ESC}[38;5;{fg};48;2;{g};{g};{g + 10}m{chunk}")
        lines.append(f"{ESC}[{r + 1};1H" + "".join(parts) + f"{ESC}[0m")
    status = f" flicker test | {label} | Ctrl+C to quit ".ljust(cols)[:cols]
    lines.append(f"{ESC}[{rows};1H{ESC}[7m{status}{ESC}[0m")
    return "".join(lines)


def run_phase(label, mode, cols, rows, seconds=4.0, period=0.15):
    f = frame(cols, rows, label)
    w(f"{ESC}[2J" + f)                      # main screen shows the scene
    end = time.time() + seconds
    while time.time() < end:
        time.sleep(period)
        enter = f"{ESC}[?1049h{ESC}[2J" + f
        leave = f"{ESC}[?1049l"
        if mode == "control":
            w(f"{ESC}[?1049h{ESC}[2J")
            for i in range(0, len(f), 512):
                w(f[i:i + 512])
                time.sleep(0.002)
        elif mode == "single":
            w(enter)
        else:
            w(f"{ESC}[?2026h" + enter + f"{ESC}[?2026l")
        time.sleep(period)
        if mode == "sync":
            w(f"{ESC}[?2026h" + leave + f"{ESC}[?2026l")
        else:
            w(leave)
    return len(f.encode())


def main():
    if not os.isatty(IN) or not os.isatty(OUT):
        sys.exit("run this in a real terminal")
    cols, rows = shutil.get_terminal_size()
    cols, rows = cols or 80, rows or 24
    old = termios.tcgetattr(IN)
    tty.setcbreak(IN)  # keeps Ctrl+C working
    try:
        support = query_mode_2026()
        w(f"{ESC}[?25l")
        size = run_phase("0 CONTROL (should flicker)", "control", cols, rows)
        run_phase("1 SINGLE write, no sync", "single", cols, rows)
        run_phase("2 SYNC mode 2026", "sync", cols, rows)
    except KeyboardInterrupt:
        pass
    finally:
        w(f"{ESC}[?1049l{ESC}[?2026l{ESC}[0m{ESC}[?25h{ESC}[2J{ESC}[H")
        termios.tcsetattr(IN, termios.TCSADRAIN, old)
    print(f"terminal: TERM={os.environ.get('TERM')} TERM_PROGRAM={os.environ.get('TERM_PROGRAM', '-')} "
          f"TMUX={'yes' if os.environ.get('TMUX') else 'no'}  size={cols}x{rows}")
    print(f"mode 2026 (synchronized output): {support}")
    print(f"frame size: {size} bytes per swap")
    print("Did phase 1 or 2 show ANY flash, tearing or partial frame? Phase 0 is the reference.")


if __name__ == "__main__":
    main()
