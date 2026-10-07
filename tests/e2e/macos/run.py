#!/usr/bin/env python3
"""End-to-end on macOS with the real Terminal and iTerm2 (for a CI runner).

    python3 tests/e2e/macos/run.py [--terminals terminal,iterm2] [--options default,meta]
                                   [--layers direct,tmux,ssh+tmux] [--agents claude,codex,copilot]
                                   [--out DIR]

Each case opens a terminal window running peekme around the fake agent (see
tests/e2e/fakeagent.py), possibly inside tmux or over SSH to this machine, and
drives it with real events: keystrokes through System Events (Option+P, x,
Esc) and a mouse drag through `cliclick`. So the bytes come from the terminal
itself, in its default settings ("default": Option+P types π) or with Option
as Meta ("meta"). The terminal is started without scripting it (a .command
file for Terminal, a startup profile for iTerm2): scripting an app needs a
consent click nobody can give on a runner, while sending keys does not.

Screen coordinates are calibrated per window: two clicks at known pixels, and
the fake agent's log says which cells they hit.

Checks as in matrix.py: the drag reaches the agent, the shortcut opens a box on
it and never reaches the agent, typing goes through, Esc closes the box; and
with nothing selected Option+P types π (default) or shows the "select text"
hint (meta).
"""
import argparse, json, os, plistlib, re, shlex, shutil, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, os.path.dirname(HERE))
import matrix as m  # noqa: E402

SGR = re.compile(r"\x1b\[<(\d+);(\d+);(\d+)([Mm])")
ITERM_PROFILES = os.path.expanduser("~/Library/Application Support/iTerm2/DynamicProfiles")


def sh(*args, timeout=30):
    return subprocess.run(args, capture_output=True, text=True, timeout=timeout)


def keystroke(expr):
    """expr: AppleScript after `tell application "System Events" to`."""
    sh("osascript", "-e", f'tell application "System Events" to {expr}')


def option_p():
    keystroke('keystroke "p" using option down')


def screenshot(path):
    sh("screencapture", "-x", path)


def quit_terminals():
    for app in ("Terminal", "iTerm2"):
        sh("killall", app)
    # iTerm2 keeps sessions running in its iTermServer helper and reattaches
    # them on the next launch, on top of the new window. End them all.
    for pattern in ("iTermServer", "fakeagent.py", "target/release/peekme", "/links/"):
        sh("pkill", "-f", pattern)
    m.wait(lambda: sh("pgrep", "-x", "iTerm2").returncode and sh("pgrep", "-x", "Terminal").returncode, 5)
    time.sleep(1)  # a relaunch right after the process ends can reach the dying instance
    # Otherwise the next launch restores the old windows on top of the new one.
    home = os.path.expanduser("~")
    for d in ("Library/Saved Application State/com.googlecode.iterm2.savedState",
              "Library/Saved Application State/com.apple.Terminal.savedState",
              "Library/Application Support/iTerm2/SavedState"):
        shutil.rmtree(os.path.join(home, d), ignore_errors=True)
    for f in ("Library/Application Support/iTerm2/restorable-state.sqlite",):
        try:
            os.remove(os.path.join(home, f))
        except OSError:
            pass


def launch(terminal, option, script):
    quit_terminals()
    if terminal == "terminal":
        terminal_option_as_meta(option == "meta")
        sh("open", "-a", "Terminal", script)
    else:
        os.makedirs(ITERM_PROFILES, exist_ok=True)
        opt = 2 if option == "meta" else 0  # 2 = Esc+, 0 = Normal (types π)
        with open(os.path.join(ITERM_PROFILES, "peekme.json"), "w") as f:
            json.dump({"Profiles": [{"Name": "peekme", "Guid": "peekme-e2e", "Custom Command": "Yes",
                                     "Command": script, "Option Key Sends": opt,
                                     "Right Option Key Sends": opt}]}, f)
        sh("defaults", "write", "com.googlecode.iterm2", "Default Bookmark Guid", "-string", "peekme-e2e")
        sh("open", "/Applications/iTerm.app")


def terminal_option_as_meta(on):
    """Option as Meta lives in the profile ("Basic" is the default one). Once
    Terminal has run, cfprefsd caches its prefs and an edit of the plist file
    is lost, so go through `defaults`."""
    r = subprocess.run(["defaults", "export", "com.apple.Terminal", "-"], capture_output=True)
    prefs = plistlib.loads(r.stdout) if r.returncode == 0 and r.stdout else {}
    for k in ("Default Window Settings", "Startup Window Settings"):
        prefs[k] = "Basic"
    prefs.setdefault("Window Settings", {}).setdefault("Basic", {})["useOptionAsMetaKey"] = on
    subprocess.run(["defaults", "import", "com.apple.Terminal", "-"], input=plistlib.dumps(prefs),
                   capture_output=True)
    r = subprocess.run(["defaults", "export", "com.apple.Terminal", "-"], capture_output=True)
    got = plistlib.loads(r.stdout).get("Window Settings", {}).get("Basic", {}).get("useOptionAsMetaKey")
    if got != on:
        print(f"      Terminal Option as Meta is {got}, wanted {on}", flush=True)


def last_motion(agent_log):
    """(col, row) of the last mouse report the agent received."""
    last = None
    for e in m.jsonl(agent_log):
        if e["kind"] == "input":
            for b, x, y, k in SGR.findall(e["raw"]):
                last = (int(x), int(y))
    return last


def front_window():
    """(left, top, width, height) of the frontmost app's front window, or None."""
    r = sh("osascript", "-e", 'tell application "System Events" to tell '
           '(first process whose frontmost is true) to get {position, size} of front window')
    try:
        left, top, width, height = (int(v) for v in r.stdout.split(","))
    except ValueError:
        return None
    return left, top, width, height


def calibrate(agent_log):
    """A function (col, row) -> pixel at the cell's center. The terminal
    reports every pointer move (any-motion mode), so sweeping the pointer finds
    the exact pixels where a column and a row begin."""
    def at(x, y):
        sh("cliclick", f"m:{x},{y}")
        time.sleep(0.15)
        return last_motion(agent_log)

    # Sweep inside the front window: its size depends on the profile's font,
    # which is smaller once Terminal has run before on the runner (key probe).
    x0, y0, dx, dy = 300, 250, 300, 150
    win = front_window()
    if win:
        left, top, width, height = win
        x0, y0 = left + width // 4, top + height // 3
        dx, dy = width // 2, height // 3
    def reported(x, y):
        """Move to a new cell and wait for its report: the pointer may still
        sit where the last window's sweep left it, which reports nothing."""
        n = len(m.jsonl(agent_log))
        sh("cliclick", f"m:{x},{y}")
        m.wait(lambda: len(m.jsonl(agent_log)) > n, 2, 0.05)
        return last_motion(agent_log)

    reported(x0 + dx // 2, y0 + dy // 2)
    (ca, ra), (cb, rb) = reported(x0, y0), reported(x0 + dx, y0 + dy)
    w, h = dx / (cb - ca), dy / (rb - ra)
    first_col = at(x0, y0)[0]
    xb = next(x for x in range(x0, x0 + int(w) + 3) if at(x, y0)[0] != first_col)
    first_row = at(x0, y0)[1]
    yb = next(y for y in range(y0, y0 + int(h) + 3) if at(x0, y)[1] != first_row)
    col0, row0 = first_col + 1, first_row + 1  # cells that start at xb, yb

    def px(col, row):
        return round(xb + (col - col0 + 0.5) * w), round(yb + (row - row0 + 0.5) * h)

    return px


def run_case(env, case, out):
    name = f"{case['terminal']}-{case['option']}|{case['layers']}|mouse-{case['mouse']}|{case['agent']}"
    d = os.path.join(out, name.replace("|", "_").replace("+", "-"))
    os.makedirs(d, exist_ok=True)
    paths = {"events": os.path.join(d, "events.jsonl"), "agent": os.path.join(d, "agent.jsonl")}
    argv, socks = m.build_command(env, dict(case, extkeys="off"), paths)
    script = os.path.join(d, "run.command")
    with open(script, "w") as f:
        f.write("#!/bin/bash\nexec " + " ".join(shlex.quote(a) for a in argv) + "\n")
    os.chmod(script, 0o755)
    fails = []

    def check(ok, msg):
        if not ok:
            fails.append(msg)
        return ok

    def events(kind):
        return [e for e in m.jsonl(paths["events"]) if e["event"] == kind]

    def agent(kind):
        return [e for e in m.jsonl(paths["agent"]) if e["kind"] == kind]

    try:
        launch(case["terminal"], case["option"], script)
        if not check(m.wait(lambda: agent("start"), 25), "the agent never started"):
            screenshot(os.path.join(d, "no-start.png"))
            return name, fails
        time.sleep(0.5)
        try:
            px = calibrate(paths["agent"])
        except (TypeError, StopIteration, ZeroDivisionError):
            screenshot(os.path.join(d, "no-mouse.png"))
            check(False, "mouse moves did not reach the agent")
            return name, fails
        (x1, y1), (x2, y2) = px(3, 3), px(13, 3)
        sh("cliclick", "-w", "80", f"dd:{x1},{y1}", f"m:{(x1 + x2) // 2},{y1}", f"du:{x2},{y2}")
        check(m.wait(lambda: any(e.get("text") == m.SELECTED for e in agent("selected")), 5),
              "the drag did not select 'alpha bravo'")
        time.sleep(0.2)
        option_p()
        opened = m.wait(lambda: events("open"), 6)
        if check(opened, "Option+P did not open a box"):
            check(opened[-1].get("selection") == m.SELECTED and opened[-1].get("found"),
                  f"box on the wrong selection: {opened[-1].get('selection')!r}")
        m.wait(lambda: False, 0.4)  # let the canned text draw
        screenshot(os.path.join(d, "box.png"))
        leaked = [e for e in agent("input") if "π" in e["raw"] or "\x1bp" in e["raw"]]
        check(not leaked, "Option+P reached the agent")
        keystroke('keystroke "x"')
        check(m.wait(lambda: any("x" in e["raw"] for e in agent("input")), 5), "typing did not reach the agent")
        keystroke("key code 53")  # Esc
        check(m.wait(lambda: events("close"), 5), "Esc did not close the box")
        time.sleep(0.3)
        # Nothing selected: π is typed (default), or the "select text" hint opens (meta).
        cx, cy = px(20, 6)
        sh("cliclick", f"c:{cx},{cy}")
        # Go on once the agent got the click (it clears the selection), not after a guess.
        m.wait(lambda: any(re.search(r"\x1b\[<0;20;6m", e["raw"]) for e in agent("input")), 4)
        time.sleep(0.3)
        n = len(events("open"))
        option_p()
        if case["option"] == "default":
            m.wait(lambda: any("π" in e["raw"] for e in agent("input")), 3)
        else:
            m.wait(lambda: len(events("open")) > n, 3)
        if case["option"] == "default":
            check(any("π" in e["raw"] for e in agent("input")), "π with nothing selected was not typed")
            check(len(events("open")) == n, "π with nothing selected opened a box")
        else:
            later = events("open")[n:]
            check(later and not later[-1].get("found"), "Alt+P with nothing selected did not show the hint")
            keystroke("key code 53")
    finally:
        screenshot(os.path.join(d, "end.png"))
        keystroke('keystroke "d" using control down')
        time.sleep(0.3)
        for s in socks:
            sh(env.tmux, "-L", s, "kill-server")
    return name, fails


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--terminals", default="terminal,iterm2")
    ap.add_argument("--options", default="default,meta")
    ap.add_argument("--layers", default="direct,tmux,ssh+tmux")
    ap.add_argument("--agents", default="claude,codex,copilot")
    ap.add_argument("--out", default="macos-e2e-out")
    args = ap.parse_args()
    for tool in ("cliclick", "tmux"):
        if not shutil.which(tool):
            sys.exit(f"{tool} not found (brew install {tool})")
    todo, n = [], 0
    for terminal in args.terminals.split(","):
        for option in args.options.split(","):
            for layer in args.layers.split(","):
                for mouse in (["off"] if layer == "direct" else ["on"] if layer == "tmux" else ["off"]):
                    for agent in args.agents.split(","):
                        # Started the way users start it: `claude` is our link (via_link).
                        todo.append(dict(terminal=terminal, option=option, layers=layer, mouse=mouse,
                                         agent=agent, profile="n/a", n=n, via_link=True))
                        n += 1
    work = tempfile.mkdtemp(prefix="peekme-macos-")
    out = os.path.abspath(args.out)
    os.makedirs(out, exist_ok=True)
    env = m.Env(work, need_ssh=any("ssh" in c["layers"] for c in todo))
    failed = []
    if "terminal" in args.terminals:
        # Let Terminal write its preferences once before we edit them.
        sh("open", "-a", "Terminal")
        time.sleep(5)
        quit_terminals()
    setup_fails = {}
    try:
        for c in todo:
            key = f"{c['terminal']}-{c['option']}"
            if setup_fails.get(key, 0) >= 2:
                # This terminal cannot be driven on this machine (a dialog, a
                # permission): the rest of its cases would fail the same way.
                print(f"SKIP  {key}|{c['layers']}|mouse-{c['mouse']}|{c['agent']}  <- two setup failures before")
                failed.append(key)
                continue
            name, fails = run_case(env, c, out)
            setup = any(f in ("the agent never started", "mouse moves did not reach the agent") for f in fails)
            setup_fails[key] = setup_fails.get(key, 0) + 1 if setup else 0
            print(("ok    " if not fails else "FAIL  ") + name + ("" if not fails else "  <- " + "; ".join(fails)),
                  flush=True)
            if fails:
                failed.append(name)
    finally:
        quit_terminals()
        env.stop()
    print(f"\n{len(todo) - len(failed)}/{len(todo)} passed. Artifacts: {out}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
