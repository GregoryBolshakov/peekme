#!/usr/bin/env python3
"""The real Codex CLI in the real Terminal and iTerm2 on macOS (for a CI runner).

    python3 tests/e2e/macos/real_codex.py [--terminals terminal,iterm2] [--options default,meta]
                                          [--layers tmux,ssh+tmux] [--out DIR]

Like run.py, but the agent is the real `codex` (installed with npm), so what
Codex asks of the terminal (mouse modes, kitty keyboard flags, its own
selection) goes through tmux and SSH to a real Mac terminal. Codex is logged in
with a dummy API key and the explainer is canned, so nothing reaches a model.

Codex logs nothing about its input, so the window is calibrated with the fake
agent first (same terminal, same window), the word to drag is found with
`tmux capture-pane`, and the checks read peekme's event log and the pane.
"""
import argparse, os, shlex, shutil, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
sys.path.insert(0, os.path.dirname(HERE))
import matrix as m  # noqa: E402
import run  # noqa: E402

# A fresh Codex shows a logo until something is typed: `/status` puts text on
# screen without a model call.
READY = "Ask Codex"
WORD = "Collaboration"


def setup_codex(work):
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
    shim = os.path.join(work, "bin", "codex-real")
    # Over SSH the remote PATH is minimal, and the npm package needs node.
    path = os.path.dirname(os.path.realpath(shutil.which("node") or codex)) + ":" + os.path.dirname(codex)
    with open(shim, "w") as f:
        f.write(f"#!/bin/sh\ncd {shlex.quote(folder)}\nexport PATH={shlex.quote(path)}:\"$PATH\"\n"
                f"export CODEX_HOME={shlex.quote(home)}\nexec {shlex.quote(codex)} \"$@\"\n")
    os.chmod(shim, 0o755)


def json_str(s):
    return '"' + s.replace("\\", "\\\\").replace('"', '\\"') + '"'


def pane(env, sock):
    r = subprocess.run([env.tmux, "-L", sock, "capture-pane", "-p"], capture_output=True, text=True)
    return r.stdout.split("\n") if r.returncode == 0 else []


def calibrate(env, terminal, option, out):
    """Pixel of each cell in this terminal's window, from a fake-agent run."""
    case = dict(terminal=terminal, option=option, layers="direct", mouse="off", agent="codex",
                profile="n/a", n=900, extkeys="off")
    d = os.path.join(out, f"{terminal}-{option}_calibrate")
    os.makedirs(d, exist_ok=True)
    paths = {"events": os.path.join(d, "events.jsonl"), "agent": os.path.join(d, "agent.jsonl")}
    argv, _ = m.build_command(env, case, paths)
    script = os.path.join(d, "run.command")
    with open(script, "w") as f:
        f.write("#!/bin/bash\nexec " + " ".join(shlex.quote(a) for a in argv) + "\n")
    os.chmod(script, 0o755)
    run.launch(terminal, option, script)
    if not m.wait(lambda: any(e["kind"] == "start" for e in m.jsonl(paths["agent"])), 25):
        run.screenshot(os.path.join(d, "no-start.png"))
        return None
    time.sleep(0.5)
    try:
        return run.calibrate(paths["agent"])
    except (TypeError, StopIteration, ZeroDivisionError):
        run.screenshot(os.path.join(d, "no-mouse.png"))
        return None
    finally:
        run.keystroke('keystroke "d" using control down')
        time.sleep(0.5)


def run_case(env, case, px, out):
    name = f"{case['terminal']}-{case['option']}|{case['layers']}|mouse-{case['mouse']}|codex-real"
    d = os.path.join(out, name.replace("|", "_").replace("+", "-"))
    os.makedirs(d, exist_ok=True)
    paths = {"events": os.path.join(d, "events.jsonl"), "agent": os.path.join(d, "agent.jsonl")}
    argv, socks = m.build_command(env, dict(case, agent="codex-real", extkeys="off"), paths)
    script = os.path.join(d, "run.command")
    with open(script, "w") as f:
        f.write("#!/bin/bash\nexec " + " ".join(shlex.quote(a) for a in argv) + "\n")
    os.chmod(script, 0o755)
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
        run.launch(case["terminal"], case["option"], script)
        if not check(m.wait(lambda: any(READY in l for l in pane(env, sock)), 60, 0.5),
                     "Codex never started"):
            save("no-start")
            return name, fails
        time.sleep(2)
        run.keystroke('keystroke "/status"')
        time.sleep(1)
        run.keystroke("key code 36")  # Return
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
        col = lines[row].index(WORD)
        # capture-pane counts from 0, px from 1.
        (x1, y1), (x2, y2) = px(col + 1, row + 1), px(col + len(WORD), row + 1)
        run.sh("cliclick", "-w", "80", f"dd:{x1},{y1}", f"m:{(x1 + x2) // 2},{y1}", f"du:{x2},{y2}")
        time.sleep(1.0)
        save("selected")
        run.option_p()
        opened = m.wait(lambda: events("open"), 8)
        if check(opened, "Option+P did not open a box"):
            sel = (opened[-1].get("selection") or "").strip()
            check(opened[-1].get("found") and sel and sel in lines[row],
                  f"box on the wrong selection: {sel!r}")
        time.sleep(0.6)
        save("box")
        check(not any("π" in l for l in pane(env, sock)), "π reached Codex")
        run.keystroke("key code 53")  # Esc
        check(m.wait(lambda: events("close"), 5), "Esc did not close the box")
        time.sleep(1.0)
        # Nothing selected: π is typed (default), or the "select text" hint opens (meta).
        cx, cy = px(col + len(WORD) + 8, row + 1)
        run.sh("cliclick", f"c:{cx},{cy}")
        time.sleep(1.0)
        n = len(events("open"))
        run.option_p()
        time.sleep(1.5)
        if case["option"] == "default":
            check(len(events("open")) == n, "π with nothing selected opened a box")
            check(any("π" in l for l in pane(env, sock)), "π with nothing selected was not typed")
        else:
            later = events("open")[n:]
            check(later and not later[-1].get("found"), "Alt+P with nothing selected did not show the hint")
            run.keystroke("key code 53")
        save("end")
    finally:
        with open(os.path.join(d, "events.txt"), "w") as f:
            f.write("\n".join(str(e) for e in m.jsonl(paths["events"])))
        run.keystroke('keystroke "c" using control down')
        time.sleep(0.3)
        for s in socks:
            run.sh(env.tmux, "-L", s, "kill-server")
    return name, fails


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--terminals", default="terminal,iterm2")
    ap.add_argument("--options", default="default,meta")
    ap.add_argument("--layers", default="tmux,ssh+tmux")
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
    setup_codex(work)
    if "terminal" in args.terminals:
        run.sh("open", "-a", "Terminal")
        time.sleep(5)
        run.quit_terminals()
    failed, total = [], 0
    try:
        for terminal in args.terminals.split(","):
            for option in args.options.split(","):
                px = calibrate(env, terminal, option, out)
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
                        name, fails = run_case(env, case, px, out)
                        print(("ok    " if not fails else "FAIL  ") + name +
                              ("" if not fails else "  <- " + "; ".join(fails)), flush=True)
                        if fails:
                            failed.append(name)
    finally:
        run.quit_terminals()
        env.stop()
    print(f"\n{total - len(failed)}/{total} passed. Artifacts: {out}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
