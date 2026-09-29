#!/usr/bin/env python3
"""The real Codex CLI in the real Terminal and iTerm2 on macOS (for a CI runner).

    python3 tests/e2e/macos/real_codex.py [--terminals terminal,iterm2] [--options default,meta]
                                          [--layers ssh+tmux,tmux] [--out DIR]

Like run.py, but the agent is the real `codex` (installed with npm), so what
Codex asks of the terminal (mouse modes, kitty keyboard flags, its own
selection) goes through tmux and SSH to a real Mac terminal. Codex is logged in
with a dummy API key and the explainer is canned, so nothing reaches a model.

Codex logs nothing about its input, so the window is calibrated with the fake
agent first (same terminal, same window), the word to drag is found with
`tmux capture-pane`, and the checks read peekme's event log and the pane.
"""
import argparse, os, re, shlex, shutil, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.dirname(HERE))
import matrix as m  # noqa: E402
import run  # noqa: E402

# A fresh Codex shows a logo until something is typed: `/status` puts text on
# screen without a model call.
READY = "Ask Codex"
WORD = "Collaboration"
WORD2 = "Permissions"


def setup_codex(work, tmux):
    """A shim that starts the real Codex in a trusted folder with a dummy login."""
    codex = shutil.which("codex")
    if not codex:
        sys.exit("codex not found (npm i -g @openai/codex)")
    home = os.path.join(work, "codex-home")
    folder = os.path.join(work, "project")
    os.makedirs(home)
    os.makedirs(folder)
    subprocess.run([codex, "login", "--with-api-key"], input=b"sk-dummy", check=True,
                   env=dict(os.environ, CODEX_HOME=home), capture_output=True)
    config = os.path.join(home, "config.toml")
    old = open(config).read() if os.path.exists(config) else ""
    with open(config, "w") as f:
        f.write(f'check_for_update_on_startup = false\n{old}\n[projects.{json_str(folder)}]\n'
                'trust_level = "trusted"\n')
    # Named `codex`, so peekme knows the agent by the command name.
    os.makedirs(os.path.join(work, "realbin"))
    shim = os.path.join(work, "realbin", "codex")
    # Over SSH the remote PATH is minimal, and the npm package needs node.
    path = ":".join([os.path.dirname(os.path.realpath(shutil.which("node") or codex)),
                     os.path.dirname(codex), os.path.dirname(os.path.abspath(tmux))])
    # CODEX_TRACE: record what Codex writes (BSD `script`) and what tmux tells it.
    with open(shim, "w") as f:
        f.write(f"#!/bin/sh\ncd {shlex.quote(folder)}\nexport PATH={shlex.quote(path)}:\"$PATH\"\n"
                f"export CODEX_HOME={shlex.quote(home)}\n"
                'if [ -n "$CODEX_TRACE" ]; then\n'
                "  tmux display-message -p '#{extended-keys-format}|#{mouse}|#{client_termname}|#{version}'"
                ' > "$CODEX_TRACE.tmux" 2>&1\n'
                '  env > "$CODEX_TRACE.env"\n'
                f'  exec script -q "$CODEX_TRACE" {shlex.quote(codex)} "$@"\nfi\n'
                f"exec {shlex.quote(codex)} \"$@\"\n")
    os.chmod(shim, 0o755)


def json_str(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def pane(env, sock):
    r = subprocess.run([env.tmux, "-L", sock, "capture-pane", "-p"], capture_output=True, text=True)
    return r.stdout.split("\n") if r.returncode == 0 else []


class Window:
    """One terminal window for all cases of a terminal and Option setting: it
    runs each command line written to a FIFO. Relaunching the terminal per case
    left extra windows in front (Terminal restores sessions) that took the keys."""

    def __init__(self, env, terminal, option, out, tag="calibrate"):
        self.dir = os.path.join(out, f"{terminal}-{option}_window-{tag}")
        os.makedirs(self.dir, exist_ok=True)
        self.fifo = os.path.join(self.dir, "commands")
        os.mkfifo(self.fifo)
        script = os.path.join(self.dir, "run.command")
        with open(script, "w") as f:
            f.write(f"#!/bin/bash\nclear\nwhile true; do\n  cmd=$(cat {shlex.quote(self.fifo)})\n"
                    '  [ "$cmd" = exit ] && exit 0\n  clear\n  /bin/sh -c "$cmd"\n  clear\ndone\n')
        os.chmod(script, 0o755)
        run.launch(terminal, option, script)
        time.sleep(3)

    def run(self, argv):
        """Hand a command to the window; False if its loop is not listening."""
        line = argv[-1] if argv[:2] == ["/bin/sh", "-c"] else " ".join(map(shlex.quote, argv))
        deadline = time.time() + 20
        while time.time() < deadline:
            try:
                fd = os.open(self.fifo, os.O_WRONLY | os.O_NONBLOCK)
            except OSError:  # no reader yet
                time.sleep(0.5)
                continue
            os.write(fd, line.encode())
            os.close(fd)
            return True
        run.screenshot(os.path.join(self.dir, "not-listening.png"))
        return False

    def close(self):
        try:
            fd = os.open(self.fifo, os.O_WRONLY | os.O_NONBLOCK)
            os.write(fd, b"exit")
            os.close(fd)
        except OSError:
            pass


def calibrate(env, win):
    """Pixel of each cell in this window, from a fake-agent run in it."""
    case = dict(layers="direct", mouse="off", agent="codex", profile="n/a", n=900, extkeys="off")
    paths = {"events": os.path.join(win.dir, "events.jsonl"), "agent": os.path.join(win.dir, "agent.jsonl")}
    argv, _ = m.build_command(env, case, paths)
    if not win.run(argv):
        return None
    if not m.wait(lambda: any(e["kind"] == "start" for e in m.jsonl(paths["agent"])), 25):
        run.screenshot(os.path.join(win.dir, "no-start.png"))
        return None
    time.sleep(0.5)
    try:
        return run.calibrate(paths["agent"])
    except (TypeError, StopIteration, ZeroDivisionError):
        run.screenshot(os.path.join(win.dir, "no-mouse.png"))
        return None
    finally:
        run.sh("pkill", "-f", "fakeagent.py")
        time.sleep(1)


def run_case(env, win, case, px, out):
    name = f"{case['terminal']}-{case['option']}|{case['layers']}|mouse-{case['mouse']}|codex-real"
    d = os.path.join(out, name.replace("|", "_").replace("+", "-"))
    os.makedirs(d, exist_ok=True)
    paths = {"events": os.path.join(d, "events.jsonl"), "agent": os.path.join(d, "agent.jsonl")}
    trace = os.path.join(d, "codex.bin")
    argv, socks = m.build_command(env, dict(case, agent="../realbin/codex", extkeys="off",
                                            env={"CODEX_TRACE": trace}), paths)
    sock = socks[1] if case["layers"] == "ssh+tmux" else socks[0]
    fails = []

    def check(ok, msg):
        if not ok:
            fails.append(msg)
        return ok

    def events(kind):
        return [e for e in m.jsonl(paths["events"]) if e["event"] == kind]

    def save(tag):
        run.screenshot(os.path.join(d, f"{tag}.png"))
        with open(os.path.join(d, f"{tag}.txt"), "w") as f:
            f.write("\n".join(pane(env, sock)))

    try:
        if not check(win.run(argv), "the window does not take commands"):
            return name, fails
        if not check(m.wait(lambda: any(READY in l for l in pane(env, sock)), 60, 0.5),
                     "Codex never started"):
            save("no-start")
            return name, fails
        time.sleep(2)
        run.keystroke('keystroke "/status"')
        time.sleep(1)
        run.keystroke("key code 36")  # Return
        # The first Return can land in Codex's command popup: press it again.
        if not m.wait(lambda: any(WORD in l for l in pane(env, sock)), 4, 0.5):
            run.keystroke("key code 36")
        deadline, calm, last = time.time() + 30, None, None
        while time.time() < deadline:
            text = pane(env, sock)
            if any(WORD in l for l in text) and text == last:
                calm = calm or time.time()
                if time.time() - calm >= 3:
                    break
            else:
                calm = None
            last = text
            time.sleep(0.5)
        lines = pane(env, sock)
        row = next((i for i, l in enumerate(lines) if WORD in l), None)
        if not check(row is not None, f"{WORD!r} never showed"):
            save("no-start")
            return name, fails
        opened_before = 0
        if case["mouse"] == "off":
            # Codex asks tmux, sees mouse off and leaves the mouse to the
            # terminal: the drag is Terminal's or iTerm2's own selection, which
            # peekme cannot see over SSH. Option+P says so once, then types π.
            r = next(i for i, l in enumerate(lines) if WORD in l)
            c = lines[r].index(WORD)
            (x1, y1), (x2, y2) = px(c + 1, r + 1), px(c + len(WORD), r + 1)
            run.sh("cliclick", "-w", "80", f"dd:{x1},{y1}", f"m:{(x1 + x2) // 2},{y1}", f"du:{x2},{y2}")
            time.sleep(1.0)
            save("hidden-selected")
            for i in range(2):
                n = len(events("open"))
                run.option_p()
                time.sleep(1.5)
                later = events("open")[n:]
                save(f"hidden-{i}")
                if i == 0 or case["option"] == "meta":
                    if check(later and later[-1].get("hidden"), f"press {i + 1}: no box saying why"):
                        check(any("tmux set -g mouse on" in l for l in pane(env, sock)),
                              f"press {i + 1}: the box does not say how to fix it")
                    run.keystroke("key code 53")
                    time.sleep(1.0)
                else:
                    check(not later, "second press opened a box again")
                    check(any("π" in l for l in pane(env, sock)), "second press did not type π")
            return name, fails

        def explain(word, tag):
            """Drag over `word`, Option+P: a box on it, and π never reaches Codex."""
            nonlocal opened_before
            r = next((i for i, l in enumerate(lines) if word in l), None)
            if not check(r is not None, f"{word!r} not on screen"):
                return None
            c = lines[r].index(word)
            # capture-pane counts from 0, px from 1.
            (x1, y1), (x2, y2) = px(c + 1, r + 1), px(c + len(word), r + 1)
            run.sh("cliclick", "-w", "80", f"dd:{x1},{y1}", f"m:{(x1 + x2) // 2},{y1}", f"du:{x2},{y2}")
            time.sleep(1.0)
            save(f"{tag}-selected")
            run.option_p()
            opened = m.wait(lambda: events("open")[opened_before:], 8)
            if check(opened, f"{tag}: Option+P did not open a box"):
                sel = (opened[-1].get("selection") or "").strip()
                check(opened[-1].get("found") and sel and sel in word,
                      f"{tag}: box on {sel!r}, not on {word!r} (source {opened[-1].get('source')})")
            opened_before = len(events("open"))
            time.sleep(0.6)
            save(f"{tag}-box")
            check(not any("π" in l for l in pane(env, sock)), f"{tag}: π reached Codex")
            run.keystroke("key code 53")  # Esc
            check(m.wait(lambda: len(events("close")) >= opened_before, 5), f"{tag}: Esc did not close the box")
            time.sleep(1.0)
            return r, c

        first = explain(WORD, "first")
        lines = pane(env, sock)
        explain(WORD2, "second")
        if first is None:
            return name, fails
        row, col = first
        # Nothing selected: π is typed (default), or the "select text" hint opens (meta).
        cx, cy = px(col + len(WORD) + 8, row + 1)
        run.sh("cliclick", f"c:{cx},{cy}")
        time.sleep(1.0)
        n = len(events("open"))
        run.option_p()
        time.sleep(1.5)
        if case["option"] == "default":
            last_p = (events("option_p") or [{}])[-1]
            check(len(events("open")) == n, f"π with nothing selected opened a box ({last_p})")
            check(any("π" in l for l in pane(env, sock)), "π with nothing selected was not typed")
        else:
            later = events("open")[n:]
            check(later and not later[-1].get("found"), "Alt+P with nothing selected did not show the hint")
            run.keystroke("key code 53")
        save("end")
    finally:
        try:
            modes = re.findall(rb"\x1b\[\?(?:1000|1002|1003|1006|1007)[hl]", open(trace, "rb").read())
            tm = open(trace + ".tmux").read().strip()
            print(f"      mouse modes from Codex: {[x.decode()[1:] for x in modes]}  tmux: {tm}", flush=True)
        except OSError:
            pass
        with open(os.path.join(d, "events.txt"), "w") as f:
            f.write("\n".join(str(e) for e in m.jsonl(paths["events"])))
        for s in socks:
            run.sh(env.tmux, "-L", s, "kill-server")
    return name, fails


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--terminals", default="terminal,iterm2")
    ap.add_argument("--options", default="default,meta")
    ap.add_argument("--layers", default="ssh+tmux")
    ap.add_argument("--out", default="macos-e2e-out")
    args = ap.parse_args()
    for tool in ("cliclick", "tmux"):
        if not shutil.which(tool):
            sys.exit(f"{tool} not found (brew install {tool})")
    work = tempfile.mkdtemp(prefix="peekme-codex-")
    out = os.path.abspath(args.out)
    os.makedirs(out, exist_ok=True)
    layers = args.layers.split(",")
    env = m.Env(work, need_ssh=any("ssh" in l for l in layers))
    setup_codex(work, env.tmux)
    if "terminal" in args.terminals:
        run.sh("open", "-a", "Terminal")
        time.sleep(5)
        run.quit_terminals()
    # Codex copies a selection with OSC 52 (through tmux too). iTerm2 asks
    # first, with a banner that pushes the screen down a row.
    run.sh("defaults", "write", "com.googlecode.iterm2", "AllowClipboardAccess", "-bool", "true")
    failed, total = [], 0
    try:
        for terminal in args.terminals.split(","):
            for option in args.options.split(","):
                win = Window(env, terminal, option, out)
                px = calibrate(env, win)
                if px is None:
                    print(f"SKIP  {terminal}-{option}  <- calibration failed", flush=True)
                    failed.append(f"{terminal}-{option}")
                    continue
                n = 0
                for layer in layers:
                    for mouse in ("off", "on"):
                        total += 1
                        n += 1
                        case = dict(terminal=terminal, option=option, layers=layer, mouse=mouse,
                                    profile="n/a", n=n)
                        if terminal == "iterm2":
                            # iTerm2 takes no System Events keys in a window
                            # whose calibration agent was killed: a fresh window
                            # per case, at the same place (same pixels).
                            win.close()
                            win = Window(env, terminal, option, out, tag=f"{layer}-{mouse}")
                        name, fails = run_case(env, win, case, px, out)
                        print(("ok    " if not fails else "FAIL  ") + name +
                              ("" if not fails else "  <- " + "; ".join(fails)), flush=True)
                        if fails:
                            failed.append(name)
                win.close()
    finally:
        run.quit_terminals()
        env.stop()
    print(f"\n{total - len(failed)}/{total} passed. Artifacts: {out}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
