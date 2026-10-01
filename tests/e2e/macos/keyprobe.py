#!/usr/bin/env python3
"""What Option+P sends in Terminal and iTerm2 once an app asks for the keyboard
modes Codex asks for (kitty flags, no modifyOtherKeys). For a CI runner.

    python3 tests/e2e/macos/keyprobe.py [--terminals terminal,iterm2] [--options default,meta]

Each case prints the raw bytes of: the kitty-flags query reply, Option+P, plain p.
"""
import argparse, os, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.dirname(HERE))
import run  # noqa: E402

RECORDER = r'''#!/usr/bin/env python3
import os, select, sys, termios, time, tty
out, flags = sys.argv[1], sys.argv[2]
fd = sys.stdin.fileno()
saved = termios.tcgetattr(fd)
tty.setraw(fd)
os.write(1, ("\x1b[>4;0m\x1b[>" + flags + "u\x1b[?u").encode() + b"KEYPROBE ready\r\n")
data, end = b"", time.time() + 25
try:
    while time.time() < end and not os.path.exists(out + ".stop"):
        r, _, _ = select.select([fd], [], [], 0.2)
        if r:
            data += os.read(fd, 4096)
            open(out + ".part", "wb").write(data)
finally:
    os.write(1, b"\x1b[<u")
    termios.tcsetattr(fd, termios.TCSADRAIN, saved)
    open(out, "wb").write(data)
'''


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--terminals", default="terminal")
    ap.add_argument("--options", default="default,meta")
    ap.add_argument("--flags", default="7,5")
    args = ap.parse_args()
    work = tempfile.mkdtemp(prefix="keyprobe-")
    rec = os.path.join(work, "rec.py")
    open(rec, "w").write(RECORDER)
    print("macOS", os.popen("sw_vers -productVersion").read().strip(), flush=True)
    for terminal in args.terminals.split(","):
        for option in args.options.split(","):
            for flags in args.flags.split(","):
                out = os.path.join(work, f"{terminal}-{option}-{flags}.bin")
                script = os.path.join(work, f"{terminal}-{option}-{flags}.command")
                with open(script, "w") as f:
                    f.write(f"#!/bin/bash\nexec python3 {rec} {out} {flags}\n")
                os.chmod(script, 0o755)
                run.launch(terminal, option, script)
                time.sleep(4)
                run.option_p()
                time.sleep(0.7)
                run.keystroke('keystroke "p"')
                time.sleep(0.7)
                open(out + ".stop", "w").close()
                time.sleep(1.5)
                data = open(out, "rb").read() if os.path.exists(out) else \
                    (open(out + ".part", "rb").read() if os.path.exists(out + ".part") else None)
                print(f"{terminal}-{option} flags={flags}: {data!r}", flush=True)
    run.quit_terminals()


if __name__ == "__main__":
    main()
