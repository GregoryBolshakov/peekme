#!/usr/bin/env python3
"""iTerm2 -> ssh -> tmux (mouse on, set-clipboard on) -> peekme -> real Claude
Code, classic and full screen: what peekme sees after a drag with and without
Option held, then Option+P. Claude runs with a dummy API key (no model call)."""
import json, os, re, shlex, shutil, subprocess, sys, tempfile, time
HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE); sys.path.insert(0, os.path.dirname(HERE))
import matrix as m  # noqa: E402
import real_codex as rc  # noqa: E402
import run  # noqa: E402
import option_drag as od  # noqa: E402

KEY = "sk-ant-api03-" + "x" * 80


def setup_claude(work, tmux):
    claude = shutil.which("claude")
    home = os.path.join(work, "claude-home")
    folder = os.path.join(work, "cproject")
    os.makedirs(home); os.makedirs(folder)
    json.dump({"hasCompletedOnboarding": True, "lastOnboardingVersion": "9.9.9", "numStartups": 5,
               "customApiKeyResponses": {"approved": [KEY[-20:]], "rejected": []},
               "projects": {folder: {"hasTrustDialogAccepted": True, "hasCompletedProjectOnboarding": True}}},
              open(os.path.join(home, ".claude.json"), "w"))
    json.dump({"theme": "dark"}, open(os.path.join(home, "settings.json"), "w"))
    os.makedirs(os.path.join(work, "cbin"))
    shim = os.path.join(work, "cbin", "claude")
    path = ":".join([os.path.dirname(os.path.realpath(shutil.which("node") or claude)),
                     os.path.dirname(claude), os.path.dirname(os.path.abspath(tmux))])
    open(shim, "w").write(f"#!/bin/sh\ncd {shlex.quote(folder)}\nexport PATH={shlex.quote(path)}:\"$PATH\"\n"
                          f"export CLAUDE_CONFIG_DIR={shlex.quote(home)} ANTHROPIC_API_KEY={KEY}\n"
                          f"exec {shlex.quote(claude)} \"$@\"\n")
    os.chmod(shim, 0o755)
    return shim


def main():
    out = os.path.abspath(sys.argv[1])
    os.makedirs(out, exist_ok=True)
    work = tempfile.mkdtemp(prefix="peekme-claude-")
    env = m.Env(work, need_ssh=True)
    shim = setup_claude(work, env.tmux)
    conf = os.path.join(work, "user.tmux.conf")
    open(conf, "w").write(od.TMUX_CONF)
    run.sh("defaults", "write", "com.googlecode.iterm2", "AllowClipboardAccess", "-bool", "true")
    win = rc.Window(env, "iterm2", "meta", out)
    px = rc.calibrate(env, win)
    win.close()
    run.quit_terminals()
    n = 0
    for tui in ("default", "fullscreen"):
        for alt in (True, False):
            n += 1
            name = f"claude-{tui}-{'option' if alt else 'plain'}-drag"
            d = os.path.join(out, name); os.makedirs(d, exist_ok=True)
            events = os.path.join(d, "events.jsonl")
            sock = f"cl{n}"
            settings = json.dumps({"tui": tui})
            inner = " ".join(["env", f"PEEKME_EVENT_LOG={shlex.quote(events)}", "PEEKME_FAKE_EXPLAINER=1",
                              "TERM_PROGRAM=", "LC_TERMINAL=", shlex.quote(m.PEEKME), shlex.quote(shim),
                              "--settings", shlex.quote(settings)])
            remote = (f"LANG=C.UTF-8 LC_ALL=C.UTF-8 {shlex.quote(env.tmux)} -L {sock} "
                      f"-f {shlex.quote(conf)} new-session -x 100 -y 30 {shlex.quote(inner)}")
            line = " ".join(shlex.quote(a) for a in env.ssh_base) + " " + shlex.quote(remote)
            script = os.path.join(d, "run.command")
            open(script, "w").write("#!/bin/bash\nexec /bin/sh -c " + shlex.quote(line) + "\n")
            os.chmod(script, 0o755)
            pane = lambda: rc.pane(env, sock)  # noqa: E731
            run.launch("iterm2", "meta", script)
            ok = m.wait(lambda: any("Claude Code" in l for l in pane()), 60, 0.5)
            time.sleep(4)
            lines = pane()
            res = {"case": name, "started": bool(ok)}
            r = next((i for i, l in enumerate(lines) if "Claude Code" in l), None)
            if r is not None:
                c = lines[r].index("Claude Code")
                (x1, y1), (x2, y2) = px(c + 1, r + 1), px(c + 11, r + 1)
                subprocess.run(["pbcopy"], input=b"before")
                keys = ["kd:alt"] if alt else []
                run.sh("cliclick", "-w", "80", *keys, f"dd:{x1},{y1}", f"m:{(x1 + x2) // 2},{y1}",
                       f"du:{x2},{y2}", *(["ku:alt"] if alt else []))
                time.sleep(1.0)
                run.screenshot(os.path.join(d, "selected.png"))
                res["pasteboard"] = run.sh("pbpaste").stdout
                res["tmux_buffers"] = run.sh(env.tmux, "-L", sock, "list-buffers").stdout
                run.option_p()
                time.sleep(2)
                run.screenshot(os.path.join(d, "after.png"))
                res["events"] = [e for e in m.jsonl(events) if e["event"] != "start"]
            open(os.path.join(d, "pane.txt"), "w").write("\n".join(pane()))
            print(json.dumps(res)[:1200], flush=True)
            run.sh(env.tmux, "-L", sock, "kill-server")
            run.quit_terminals()
    env.stop()


if __name__ == "__main__":
    main()
