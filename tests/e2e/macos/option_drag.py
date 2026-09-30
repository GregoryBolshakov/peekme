#!/usr/bin/env python3
"""Option+drag selection with the real Codex, over SSH, in tmux with the mouse on.

    python3 tests/e2e/macos/option_drag.py [--terminals iterm2,terminal] [--options default,meta]
                                           [--out DIR]

The setup people use with a cloud dev box: a Mac terminal, SSH, tmux with
`mouse on` and `set-clipboard on`, Codex inside. Codex takes the mouse, so to
select text the terminal's own way people hold Option while they drag. That
selection stays in the Mac terminal (it may copy it to the Mac clipboard). This
records what peekme sees when Option+P comes next, and whether the terminal
answers a clipboard query (OSC 52 `?`) that tmux can pass on.
"""
import argparse, json, os, re, shlex, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.dirname(HERE))
import matrix as m  # noqa: E402
import real_codex as rc  # noqa: E402
import run  # noqa: E402

# The settings that matter from a real tmux.conf of this setup.
TMUX_CONF = """set -g mouse on
set -s escape-time 10
set -g set-clipboard on
set -ag terminal-overrides ',xterm*:Ms=\\E]52;c;%p2%s\\7'
bind -T copy-mode    MouseDragEnd1Pane send-keys -X copy-selection
bind -T copy-mode-vi MouseDragEnd1Pane send-keys -X copy-selection
"""


def pbpaste():
    return run.sh("pbpaste").stdout


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--terminals", default="iterm2,terminal")
    ap.add_argument("--options", default="default,meta")
    ap.add_argument("--out", default="macos-option-drag-out")
    ap.add_argument("--plain", action="store_true", help="drag without Option held")
    args = ap.parse_args()
    work = tempfile.mkdtemp(prefix="peekme-optdrag-")
    out = os.path.abspath(args.out)
    os.makedirs(out, exist_ok=True)
    env = m.Env(work, need_ssh=True)
    rc.setup_codex(work, env.tmux)
    conf = os.path.join(work, "user.tmux.conf")
    with open(conf, "w") as f:
        f.write(TMUX_CONF)
    run.sh("defaults", "write", "com.googlecode.iterm2", "AllowClipboardAccess", "-bool", "true")
    if "terminal" in args.terminals:
        run.sh("open", "-a", "Terminal")
        time.sleep(5)
        run.quit_terminals()
    report = []
    n = 0
    try:
        for terminal in args.terminals.split(","):
            for option in args.options.split(","):
                n += 1
                name = f"{terminal}-{option}"
                d = os.path.join(out, name)
                os.makedirs(d, exist_ok=True)
                res = {"case": name}
                report.append(res)
                # Pixels of the cells, from the fake agent in the same kind of window.
                win = rc.Window(env, terminal, option, d)
                px = rc.calibrate(env, win)
                win.close()
                run.quit_terminals()
                if px is None:
                    res["error"] = "calibration failed"
                    continue
                events = os.path.join(d, "events.jsonl")
                trace = os.path.join(d, "codex.bin")
                sock = f"od{n}"
                inner = " ".join(["env", f"PEEKME_EVENT_LOG={shlex.quote(events)}", "PEEKME_FAKE_EXPLAINER=1", "PEEKME_LOG_INPUT=1",
                                  f"CODEX_TRACE={shlex.quote(trace)}", "TERM_PROGRAM=", "LC_TERMINAL=",
                                  shlex.quote(m.PEEKME), shlex.quote(os.path.join(work, "realbin", "codex"))])
                remote = (f"LANG=C.UTF-8 LC_ALL=C.UTF-8 {shlex.quote(env.tmux)} -L {sock} "
                          f"-f {shlex.quote(conf)} new-session -x 100 -y 30 {shlex.quote(inner)}")
                line = " ".join(shlex.quote(a) for a in env.ssh_base) + " " + shlex.quote(remote)
                script = os.path.join(d, "run.command")
                with open(script, "w") as f:
                    f.write("#!/bin/bash\nexec /bin/sh -c " + shlex.quote(line) + "\n")
                os.chmod(script, 0o755)

                def pane():
                    return rc.pane(env, sock)

                def save(tag):
                    run.screenshot(os.path.join(d, f"{tag}.png"))
                    with open(os.path.join(d, f"{tag}.txt"), "w") as f:
                        f.write("\n".join(pane()))

                run.launch(terminal, option, script)
                if not m.wait(lambda: any(rc.READY in l for l in pane()), 60, 0.5):
                    res["error"] = "Codex never started"
                    save("no-start")
                    continue
                time.sleep(2)
                for _ in range(3):
                    run.keystroke('keystroke "/status"')
                    if m.wait(lambda: any("/status" in l for l in pane()), 3, 0.5):
                        break
                    run.sh("cliclick", "c:{},{}".format(*px(60, 3)))
                    time.sleep(1)
                time.sleep(1)
                run.keystroke("key code 36")
                if not m.wait(lambda: any(rc.WORD in l for l in pane()), 4, 0.5):
                    run.keystroke("key code 36")
                if not m.wait(lambda: any(rc.WORD in l for l in pane()), 20, 0.5):
                    res["error"] = "no /status"
                    save("no-status")
                    run.option_p()
                    time.sleep(2)
                    res["events"] = m.jsonl(events)
                    res["pane"] = pane()[-8:]
                    continue
                time.sleep(3)
                lines = pane()
                r = next(i for i, l in enumerate(lines) if rc.WORD in l)
                c = lines[r].index(rc.WORD)
                (x1, y1), (x2, y2) = px(c + 1, r + 1), px(c + len(rc.WORD), r + 1)
                subprocess.run(["pbcopy"], input=b"before")
                # Option held through the drag: the terminal's own selection.
                # Over three rows (the calibration can be a row off in iTerm2).
                (x1, y1), (x2, y2) = px(c + 1, r), px(c + len(rc.WORD), r + 2)
                mods = ([], []) if args.plain else (["kd:alt"], ["ku:alt"])
                run.sh("cliclick", "-w", "80", *mods[0], f"dd:{x1},{y1}", f"m:{(x1 + x2) // 2},{(y1 + y2) // 2}",
                       f"du:{x2},{y2}", *mods[1])
                time.sleep(1.0)
                save("selected")
                res["clipboard_after_drag"] = pbpaste()
                res["tmux_buffers_after_drag"] = run.sh(env.tmux, "-L", sock, "list-buffers").stdout
                run.option_p()
                time.sleep(2)
                save("after-option-p")
                res["events"] = m.jsonl(events)
                res["pi_typed"] = any("π" in l for l in pane())
                res["codex_modes"] = sorted({x.decode() for x in re.findall(
                    rb"\x1b\[[?>=<][0-9;]*[a-zA-Z~]", open(trace, "rb").read())}) if os.path.exists(trace) else []
                run.keystroke("key code 53")
                time.sleep(1)
                # Does a clipboard query reach the Mac terminal and come back?
                # tmux asks the terminal with `refresh-client -l` and stores the
                # answer as a new paste buffer.
                subprocess.run(["pbcopy"], input=b"clipboard probe text")
                before = run.sh(env.tmux, "-L", sock, "list-buffers").stdout
                client = run.sh(env.tmux, "-L", sock, "list-clients", "-F", "#{client_name}").stdout.split()
                q = run.sh(env.tmux, "-L", sock, "refresh-client", *(["-t", client[0]] if client else []), "-l")
                time.sleep(2)
                res["refresh_client_l"] = {"rc": q.returncode, "err": q.stderr.strip(),
                                           "before": before,
                                           "after": run.sh(env.tmux, "-L", sock, "list-buffers").stdout}
                save("end")
                run.sh(env.tmux, "-L", sock, "kill-server")
                time.sleep(1)
    finally:
        run.quit_terminals()
        env.stop()
        with open(os.path.join(out, "report.json"), "w") as f:
            json.dump(report, f, indent=1, default=str)
        for r in report:
            print(json.dumps(r, default=str)[:1500], flush=True)


if __name__ == "__main__":
    main()
