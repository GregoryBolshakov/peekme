#!/usr/bin/env python3
"""Option+P after a selection made with tmux, when tmux stays in copy mode.

    python3 tests/e2e/copy_mode.py

With `mouse on`, a drag over an agent that leaves the mouse alone (Claude
Code's classic screen, Codex in its scrollback mode) is tmux's selection.
Many configs bind the end of a drag to `copy-selection`, which copies and stays
in copy mode, so the next keys go to tmux and Option+P used to do nothing.
peekme binds Option+P in copy mode for its own pane: the key must open a box on
the dragged text, in tmux directly and over SSH, as `π` and as Alt+P.
"""
import os, subprocess, sys, tempfile, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import matrix as m  # noqa: E402

CONF = """set -g mouse on
set -s escape-time 10
set -g set-clipboard on
bind -T copy-mode    MouseDragEnd1Pane send-keys -X copy-selection
bind -T copy-mode-vi MouseDragEnd1Pane send-keys -X copy-selection
"""


def run_case(env, case, out):
    name = f"{case['layers']}|{case['profile']}|{case['keys']}"
    d = os.path.join(out, name.replace("|", "_").replace("+", "-"))
    os.makedirs(d, exist_ok=True)
    paths = {"events": os.path.join(d, "events.jsonl"), "agent": os.path.join(d, "agent.jsonl")}
    argv, socks = m.build_command(env, dict(case, agent="classic", mouse="on", extkeys="off"), paths)
    t = m.Term(argv, m.clean_env(TERM="xterm-256color"), os.path.join(d, "terminal.bin"))
    hotkey = m.PROFILES[case["profile"]]
    fails = []

    def check(ok, msg):
        if not ok:
            fails.append(msg)
        return ok

    def events(kind):
        return [e for e in m.jsonl(paths["events"]) if e["event"] == kind]

    try:
        if not check(m.wait(lambda: any("alpha bravo" in l for l in t.screen()), 15), "agent never showed"):
            return name, fails
        time.sleep(0.5)
        t.send(m.DRAG)
        time.sleep(1.0)
        check(any("[0/" in l for l in t.screen()[:2]), "the drag did not put tmux in copy mode")
        t.send(hotkey)
        opened = m.wait(lambda: events("open"), 6)
        if check(opened, "Option+P in copy mode did not open a box"):
            # tmux's emacs copy mode leaves out the cell the drag ended on.
            sel = opened[-1].get("selection") or ""
            check(opened[-1].get("found") and sel in (m.SELECTED, m.SELECTED[:-1]), f"box on {sel!r}")
        leaked = [e for e in m.jsonl(paths["agent"]) if e["kind"] == "input" and hotkey.hex() in e["hex"]]
        check(not leaked, "the shortcut reached the agent")
        t.send(b"\x1b")
        check(m.wait(lambda: events("close"), 5), "Esc did not close the box")
    finally:
        t.send(b"\x04")
        time.sleep(0.2)
        t.close()
        for s in socks:
            subprocess.run([env.tmux, "-L", s, "kill-server"], capture_output=True,
                           env=dict(os.environ, LD_LIBRARY_PATH=env.libs))
        with open(os.path.join(d, "screen.txt"), "w") as f:
            f.write("\n".join(t.screen()))
    return name, fails


def main():
    work = tempfile.mkdtemp(prefix="peekme-copymode-")
    out = os.path.join(work, "cases")
    env = m.Env(work, need_ssh=True)
    conf = os.path.join(work, "copy-mode.conf")
    failed, n = [], 0
    try:
        for keys in ("emacs", "vi"):
            with open(conf + keys, "w") as f:
                f.write(CONF + f"set -g mode-keys {keys}\n")
            env.tmux_conf = lambda mouse, ext, c=conf + keys: c
            for layers in ("tmux", "ssh+tmux"):
                for profile in m.PROFILES:
                    case = dict(layers=layers, profile=profile, keys=keys, n=n)
                    n += 1
                    name, fails = run_case(env, case, out)
                    print(("ok    " if not fails else "FAIL  ") + name
                          + ("" if not fails else "  <- " + "; ".join(fails)), flush=True)
                    if fails:
                        failed.append(name)
    finally:
        env.stop()
    print(f"\n{n - len(failed)}/{n} passed. Artifacts: {out}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
