#!/usr/bin/env python3
"""Inside iTerm2: print known text, wait while the driver selects it, then ask
iTerm2 for session variables (OSC 1337 ReportVariable), directly or through
tmux passthrough. Writes the raw answers to argv[1]."""
import base64, os, select, sys, termios, time, tty

out, wrap = sys.argv[1], sys.argv[2] == "tmux"
fd = sys.stdin.fileno()
os.write(1, b"\x1b[2J\x1b[Halpha bravo charlie delta\r\n")
old = termios.tcgetattr(fd)
tty.setraw(fd)
res = []
try:
    time.sleep(6)  # the driver drags over "bravo charlie" now
    for name in ("session.selection", "selection", "session.selectionLength", "session.columns",
                 "session.mouseReportingMode"):
        q = b"\x1b]1337;ReportVariable=" + base64.b64encode(name.encode()) + b"\x07"
        if wrap:
            q = b"\x1bPtmux;" + q.replace(b"\x1b", b"\x1b\x1b") + b"\x1b\\"
        os.write(1, q)
        got, end = b"", time.time() + 1.5
        while time.time() < end:
            r, _, _ = select.select([fd], [], [], 0.1)
            if r:
                got += os.read(fd, 4096)
        res.append(f"{name}: {got!r}")
finally:
    termios.tcsetattr(fd, termios.TCSADRAIN, old)
    open(out, "w").write("\n".join(res) + "\n")
