#!/usr/bin/env python3
"""Does the terminal answer a clipboard read (OSC 52 `?`)? Run by the probe
driver inside Terminal or iTerm2; writes what came back to argv[1]."""
import os, select, sys, termios, time, tty

out = sys.argv[1]
fd = sys.stdin.fileno()
old = termios.tcgetattr(fd)
tty.setraw(fd)
got = b""
try:
    for q in (b"\x1b]52;c;?\x07", b"\x1b]52;c;?\x1b\\", b"\x1b]52;;?\x07"):
        os.write(sys.stdout.fileno(), q)
        end = time.time() + 2
        while time.time() < end:
            r, _, _ = select.select([fd], [], [], 0.1)
            if r:
                got += os.read(fd, 4096)
        got += b"|"
finally:
    termios.tcsetattr(fd, termios.TCSADRAIN, old)
    with open(out, "wb") as f:
        f.write(got)
