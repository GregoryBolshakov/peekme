#!/usr/bin/env python3
"""`peekme install --shell`: a new terminal's shell starts with peekme from its
first line, and what runs in that shell behaves.

    python3 tests/e2e/anyterm_shell.py

A private Xvfb runs a GTK3 VTE window (vtehost.py) with plain `bash`, the way
GNOME Terminal opens a tab, in a private HOME where `peekme install --shell`
wrote the startup file. Mouse and keys go in through XTEST. Checks:

- the shell starts under peekme, at the top of the window, and its children
  see PEEKME_ACTIVE=terminal;
- the box opens under the selected copy of a repeated line;
- a box that needs room moves the whole screen, not only the rows below the
  line where peekme started (what `peekme -- bash` typed in a tab did);
- `bash` typed in that shell is not started with peekme a second time;
- `claude` typed there (a fake agent, tests/e2e/fakeagent.py) gets its own
  peekme in agent mode, and the outer one leaves Alt+P to it;
- after `peekme uninstall --shell` the shell starts without peekme.

The explainer is canned (PEEKME_FAKE_EXPLAINER), so no model is called.
Needs: a release build, Xvfb, python3-gi with Vte 2.91, python-xlib. Skips
(exit 0) when one is missing.
"""
import json, os, shutil, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import anyterm_x11 as x11  # noqa: E402  (Window helpers, LOG)

try:
    from Xlib import display
except ImportError as e:
    print(f"skip: {e}")
    sys.exit(0)

PEEKME = x11.PEEKME
KEYS = {" ": "space", ".": "period", "-": "minus", "/": "slash", "\n": "Return"}
SCRIPTS = {
    "s": x11.LOG,
    "e": "echo active=$PEEKME_ACTIVE",
    "n": "for i in $(seq 1 12); do echo line $i; done",
}


class Shell(x11.Window):
    """One VTE window running plain `bash` with HOME `home`."""

    def __init__(self, disp, work, name, home, bin_dir):
        self.dir = os.path.join(work, name)
        os.makedirs(self.dir)
        env = {k: v for k, v in os.environ.items()
               if k not in ("PEEKME_ACTIVE", "TMUX", "TMUX_PANE", "SSH_CONNECTION",
                            "XDG_DATA_HOME", "BASH_ENV", "ENV")}
        env.update(DISPLAY=disp, PEEKME_FAKE_EXPLAINER="1", NO_AT_BRIDGE="1", HOME=home,
                   SHELL="/bin/bash", PATH=f"{bin_dir}:{os.environ['PATH']}",
                   XDG_STATE_HOME=os.path.join(self.dir, "state"),
                   PEEKME_EVENT_LOG=os.path.join(self.dir, "events.jsonl"),
                   FAKEAGENT_LOG=os.path.join(self.dir, "agent.jsonl"))
        self.proc = subprocess.Popen(
            [sys.executable, os.path.join(HERE, "vtehost.py"), self.dir, "/bin/bash"],
            env=env, stderr=subprocess.DEVNULL)
        self.d = display.Display(disp)
        x11.wait(lambda: self.screen().startswith("$ "), 10)
        self.geo = json.load(open(os.path.join(self.dir, "geometry.json")))
        self.type("cd\n")

    def type(self, text):
        for ch in text:
            self.key(KEYS.get(ch, ch))
        time.sleep(0.5)

    def starts(self):
        """Pids of the peekmes that started, in order."""
        return [e["pid"] for e in self.events("start")]

    def row_of(self, text, nth=0):
        hits = [i for i, r in enumerate(self.rows()) if r.startswith(text)]
        return hits[nth] if len(hits) > nth else None

    def agent_input(self):
        path = os.path.join(self.dir, "agent.jsonl")
        if not os.path.exists(path):
            return ""
        return "".join(e.get("raw", "") for e in map(json.loads, open(path))
                       if e.get("kind") == "input")


def setup_home(work):
    home = os.path.join(work, "home")
    os.makedirs(home)
    with open(os.path.join(home, ".bashrc"), "w") as f:
        f.write('PS1="$ "\n')
    # Ubuntu's /etc/bash.bashrc prints a sudo hint without this file (a
    # desktop user who ran sudo once has it); it would come before peekme.
    open(os.path.join(home, ".sudo_as_admin_successful"), "w").close()
    for name, body in SCRIPTS.items():
        with open(os.path.join(home, name), "w") as f:
            f.write(body + "\n")
    bin_dir = os.path.join(work, "bin")
    os.makedirs(bin_dir)
    claude = os.path.join(bin_dir, "claude")
    with open(claude, "w") as f:
        f.write(f"#!/bin/sh\nexec {sys.executable} {os.path.join(HERE, 'fakeagent.py')} "
                "--style claude --log \"$FAKEAGENT_LOG\" -- \"$@\"\n")
    os.chmod(claude, 0o755)
    return home, bin_dir


def peekme(home, *args):
    env = {k: v for k, v in os.environ.items() if k not in ("PEEKME_ACTIVE", "XDG_DATA_HOME")}
    env.update(HOME=home, SHELL="/bin/bash")
    return subprocess.run([PEEKME, *args], env=env, capture_output=True, text=True, timeout=30)


def main():
    if not shutil.which("Xvfb"):
        print("skip: Xvfb not found")
        return 0
    if not os.path.exists(PEEKME):
        sys.exit("build first: cargo build --release")
    try:
        x11.gi.require_version("Vte", "2.91")
    except ValueError as e:
        print(f"skip: {e}")
        return 0
    work = tempfile.mkdtemp(prefix="peekme-shell-")
    home, bin_dir = setup_home(work)
    got = peekme(home, "install", "--shell")
    if got.returncode:
        sys.exit(f"peekme install --shell: {got.stderr}")
    disp = ":%d" % (80 + os.getpid() % 9)
    xvfb = subprocess.Popen(["Xvfb", disp, "-screen", "0", "1280x800x24", "-nolisten", "tcp"],
                            stderr=subprocess.DEVNULL)
    time.sleep(1)
    fails = []

    def case(name, fn):
        w = Shell(disp, work, name, home, bin_dir)
        try:
            problem = fn(w)
        except Exception as e:  # a broken case must not hide the others
            problem = f"error: {e!r}"
        finally:
            w.close()
        print(("FAIL " if problem else "ok   ") + name + (f": {problem}" if problem else ""))
        if problem:
            fails.append(name)

    def starts_at_the_top(w):
        start = w.events("start")
        if len(start) != 1:
            return f"{len(start)} peekme starts, want 1"
        if start[0].get("cursor") != [0, 0]:
            return f"started with the cursor at {start[0].get('cursor')}, want [0, 0]"
        w.type(". e\n")
        return None if "active=terminal" in w.screen() else f"no active=terminal:\n{w.screen()}"

    def second_copy(w):
        w.type(". s\n")
        row = w.row_of("warning", 1)
        w.drag(row, 9, 23)  # "unused variable", the second of four copies
        w.key("p", alt=True)
        top = x11.wait(w.box_row)
        return None if top == row + 1 else f"box on row {top}, want {row + 1}"

    def no_split(w):
        w.type(". n\n")
        before = [r.rstrip() for r in w.rows()]
        row = w.row_of("line 11")
        w.drag(row, 0, 6)
        w.key("p", alt=True)
        top = x11.wait(w.box_row)
        if top is None:
            return "no box"
        after = [r.rstrip() for r in w.rows()][:top]
        # One shift for every row above the box: the screen moved as a whole.
        for k in range(len(before)):
            if all(i + k < len(before) and a == before[i + k] for i, a in enumerate(after)):
                return None
        return f"rows above the box moved apart:\n" + "\n".join(after)

    def shell_in_the_shell(w):
        w.type("bash\n")
        w.type(". e\n")
        if "active=terminal" not in w.screen():
            return f"inner bash lost PEEKME_ACTIVE:\n{w.screen()}"
        return None if len(w.starts()) == 1 else f"peekme started {len(w.starts())} times"

    def agent_in_the_shell(w):
        w.type("claude\n")
        if not x11.wait(lambda: "fakeagent (claude)" in w.screen(), 10):
            return f"no fake agent:\n{w.screen()}"
        pids = w.starts()
        if len(pids) != 2:
            return f"{len(pids)} peekme starts, want 2 (the shell's and the agent's)"
        outer, inner = pids
        w.drag(2, 2, 12)  # "alpha bravo" on the agent's first line
        w.key("p", alt=True)
        if x11.wait(w.box_row) is None:
            return f"no box over the agent:\n{w.screen()}"
        if "\x1bp" in w.agent_input():
            return "Alt+P reached the agent"
        took = [e["event"] for e in map(json.loads, open(os.path.join(w.dir, "events.jsonl")))
                if e.get("pid") == outer and e["event"] in ("option_p", "nudge_start",
                                                              "hotkey_to_child")]
        if took:
            return f"the shell's peekme handled keys too: {took}"
        return None if any(e["pid"] == inner for e in w.events("open")) else \
            "the box is not the agent's peekme's"

    def uninstalled(w):
        if w.starts():
            return "peekme started after uninstall --shell"
        w.type(". e\n")
        return None if "active=\n" in w.screen() + "\n" else f"PEEKME_ACTIVE set:\n{w.screen()}"

    cases = [("starts with peekme at the top", starts_at_the_top),
             ("box under the second copy", second_copy),
             ("the whole screen moves for the box", no_split),
             ("bash in the shell is not wrapped again", shell_in_the_shell),
             ("claude in the shell gets its own peekme", agent_in_the_shell)]
    for name, fn in cases:
        case(name, fn)
    got = peekme(home, "uninstall", "--shell")
    if got.returncode or "peekme shell" in open(os.path.join(home, ".bashrc")).read():
        print(f"FAIL uninstall --shell: {got.stdout}{got.stderr}")
        fails.append("uninstall --shell")
    else:
        case("no peekme after uninstall --shell", uninstalled)
    xvfb.terminate()
    xvfb.wait()
    total = len(cases) + 1
    print(f"\n{total - len(fails)}/{total} passed")
    if not fails:
        shutil.rmtree(work, ignore_errors=True)
    else:
        print(f"logs in {work}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
