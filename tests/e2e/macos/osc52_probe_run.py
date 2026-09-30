#!/usr/bin/env python3
"""Open Terminal and iTerm2 with the clipboard holding known text and record
whether each answers an OSC 52 clipboard read."""
import os, subprocess, sys, time
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE); sys.path.insert(0, os.path.dirname(HERE))
import run  # noqa: E402

out = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else "macos-option-drag-out")
os.makedirs(out, exist_ok=True)
run.sh("open", "-a", "Terminal"); time.sleep(5); run.quit_terminals()
for terminal, prefs in (("terminal", {}), ("iterm2", {"AllowClipboardAccess": "false"}),
                        ("iterm2", {"AllowClipboardAccess": "true"})):
    for k, v in prefs.items():
        run.sh("defaults", "write", "com.googlecode.iterm2", k, "-bool", v)
    subprocess.run(["pbcopy"], input=b"probe clipboard text")
    tag = terminal + "".join(f"-{k}={v}" for k, v in prefs.items())
    res = os.path.join(out, f"osc52-{tag}.bin")
    script = os.path.join(out, f"osc52-{tag}.command")
    with open(script, "w") as f:
        f.write(f"#!/bin/bash\nexec {sys.executable} {os.path.join(HERE, 'osc52_probe.py')} {res}\n")
    os.chmod(script, 0o755)
    run.launch(terminal, "default", script)
    for _ in range(40):
        if os.path.exists(res):
            break
        time.sleep(0.5)
    time.sleep(0.5)
    run.screenshot(os.path.join(out, f"osc52-{tag}.png"))
    print(tag, repr(open(res, "rb").read()) if os.path.exists(res) else "no result", flush=True)
run.quit_terminals()
