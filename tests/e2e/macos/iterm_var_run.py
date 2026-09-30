#!/usr/bin/env python3
"""Does iTerm2 tell a program what is selected (ReportVariable), and does the
answer come back through tmux? Selects with Option held, like over an app that
takes the mouse."""
import os, subprocess, sys, tempfile, time
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE); sys.path.insert(0, os.path.dirname(HERE))
import matrix as m  # noqa: E402
import real_codex as rc  # noqa: E402
import run  # noqa: E402

out = os.path.abspath(sys.argv[1])
os.makedirs(out, exist_ok=True)
work = tempfile.mkdtemp(prefix="peekme-var-")
env = m.Env(work, need_ssh=True)
run.sh("defaults", "write", "com.googlecode.iterm2", "AllowClipboardAccess", "-bool", "true")
win = rc.Window(env, "iterm2", "default", out)
px = rc.calibrate(env, win)
win.close()
run.quit_terminals()
probe = os.path.join(HERE, "iterm_var_probe.py")
for tag, passthrough in (("direct", None), ("ssh+tmux-passthrough-off", "off"), ("ssh+tmux-passthrough-on", "on")):
    res = os.path.join(out, f"var-{tag}.txt")
    cmd = f"{sys.executable} {probe} {res} {'tmux' if passthrough else 'direct'}"
    if passthrough:
        conf = os.path.join(work, f"tmux-{passthrough}.conf")
        open(conf, "w").write(f"set -g mouse on\nset -g allow-passthrough {passthrough}\n")
        remote = f"LANG=C.UTF-8 {env.tmux} -L v{passthrough} -f {conf} new-session -x 100 -y 30 '{cmd}'"
        cmd = " ".join(env.ssh_base) + f" \"{remote}\""
    script = os.path.join(out, f"var-{tag}.command")
    open(script, "w").write(f"#!/bin/bash\nexec {cmd}\n")
    os.chmod(script, 0o755)
    run.launch("iterm2", "default", script)
    time.sleep(3)
    (x1, y1), (x2, y2) = px(7, 1), px(19, 1)
    run.sh("cliclick", "-w", "80", "kd:alt", f"dd:{x1},{y1}", f"m:{(x1 + x2) // 2},{y1}", f"du:{x2},{y2}", "ku:alt")
    time.sleep(0.5)
    run.screenshot(os.path.join(out, f"var-{tag}.png"))
    for _ in range(30):
        if os.path.exists(res):
            break
        time.sleep(0.5)
    print(tag, "| pasteboard:", repr(run.sh("pbpaste").stdout), flush=True)
    print(open(res).read() if os.path.exists(res) else "no result", flush=True)
    run.quit_terminals()
env.stop()
