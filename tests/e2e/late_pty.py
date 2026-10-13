#!/usr/bin/env python3
"""Run a command in a pty of its own and pass its output on, holding back
what comes after an update that changes nothing (Codex's first answer to a
wheel turn) for LATE_MS milliseconds: Codex on a slower machine, whose scroll
comes late after that first answer.

    python3 tests/e2e/late_pty.py codex -c ...
"""
import fcntl
import os
import pty
import select
import signal
import struct
import sys
import termios
import time
import tty

EMPTY = b"\x1b[?2026h\x1b[39m\x1b[49m\x1b[0m\x1b[?2026l"
LATE = int(os.environ.get("LATE_MS", "40")) / 1000


def size(fd):
    return fcntl.ioctl(fd, termios.TIOCGWINSZ, b"\0" * 8)


def main():
    pid, fd = pty.fork()
    if pid == 0:
        os.execvp(sys.argv[1], sys.argv[1:])
    fcntl.ioctl(fd, termios.TIOCSWINSZ, size(0))
    signal.signal(signal.SIGWINCH, lambda *_: fcntl.ioctl(fd, termios.TIOCSWINSZ, size(0)))
    tty.setraw(0)
    hold_until = 0.0
    while True:
        try:
            r, _, _ = select.select([0, fd], [], [], 0.05)
        except InterruptedError:
            continue
        if 0 in r:
            data = os.read(0, 65536)
            if not data:
                break
            os.write(fd, data)
        if fd in r:
            wait = hold_until - time.time()
            if wait > 0:
                time.sleep(wait)
            try:
                data = os.read(fd, 65536)
            except OSError:
                break
            if not data:
                break
            # Split after each empty update: what follows it comes late.
            hold_until = 0.0
            while data:
                i = data.find(EMPTY)
                if i < 0:
                    os.write(1, data)
                    break
                j = i + len(EMPTY)
                os.write(1, data[:j])
                data = data[j:]
                if data:
                    time.sleep(LATE)
                else:
                    hold_until = time.time() + LATE
    os.kill(pid, signal.SIGTERM)


if __name__ == "__main__":
    main()
