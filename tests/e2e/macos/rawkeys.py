#!/usr/bin/env python3
"""Record what a terminal sends, for debugging: turn on SGR mouse, save raw input for N seconds.

    python3 tests/e2e/macos/rawkeys.py OUT_FILE SECONDS
"""
import os, select, sys, termios, time, tty
out, secs = sys.argv[1], float(sys.argv[2])
fd = sys.stdin.fileno()
saved = termios.tcgetattr(fd)
tty.setraw(fd)
os.write(1, b"\x1b[?1000h\x1b[?1002h\x1b[?1006h" + b"RAWKEYS ready - type now\r\n" + b"word1 word2 word3 word4\r\n")
data = b""
end = time.time() + secs
try:
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.2)
        if r:
            data += os.read(fd, 4096)
finally:
    os.write(1, b"\x1b[?1006l\x1b[?1002l\x1b[?1000l")
    termios.tcsetattr(fd, termios.TCSADRAIN, saved)
    open(out, "wb").write(data)
