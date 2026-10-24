#!/usr/bin/env python3
"""Option+P after a selection made with tmux, when tmux stays in copy mode.

    python3 tests/e2e/copy_mode.py

With `mouse on`, a drag over an agent that leaves the mouse alone (Claude
Code's classic screen, Codex in its scrollback mode) is tmux's selection.
Many configs bind the end of a drag to `copy-selection`, which copies and stays
in copy mode, so the next keys go to tmux and Option+P used to do nothing.
peekme binds Option+P, Option+Q and Option+Shift+P (the old ask key) in copy
mode for its own pane: the key must open a box on the dragged text, in tmux
directly and over SSH, as `π` / `œ` / `∏` and as Alt+P / Alt+Q / Alt+Shift+P.

The agent leaves the mouse alone, so tmux keeps plain clicks too. peekme binds
a click in its pane to hand it over: after a peek, a click on the underlined
text must open the saved answer.

The same cases run over real Pi on its regular screen (a saved chat in an
agent folder of its own), when `pi` is installed.
"""
import itertools, os, subprocess, sys, tempfile, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import inline_start  # noqa: E402
import matrix as m  # noqa: E402
import real_agents as ra  # noqa: E402

CONF = """set -g mouse on
set -s escape-time 10
set -g set-clipboard on
bind -T copy-mode    MouseDragEnd1Pane send-keys -X copy-selection
bind -T copy-mode-vi MouseDragEnd1Pane send-keys -X copy-selection
"""


def run_case(env, case, out):
    pi = case["agent"] == "pi"
    name = f"{case['agent']}|{case['layers']}|{case['profile']}|{case['keys']}|{case['shortcut']}"
    d = os.path.join(out, name.replace("|", "_").replace("+", "-"))
    os.makedirs(d, exist_ok=True)
    paths = {"events": os.path.join(d, "events.jsonl"), "agent": os.path.join(d, "agent.jsonl")}
    ready, drag, selected, click = "alpha bravo", m.DRAG, m.SELECTED, b"\x1b[<0;6;3M\x1b[<0;6;3m"
    run = {}
    if pi:
        run["run"], ready, _ = inline_start.pi_run(env, os.path.join(d, "pi"))
        selected = "b0r20 words"
    argv, socks = m.build_command(env, dict(case, agent="classic", mouse="on", extkeys="off", **run),
                                  paths)
    t = m.Term(argv, m.clean_env(TERM="xterm-256color"), os.path.join(d, "terminal.bin"))
    ask = case["shortcut"] != "peek"
    keys = {"peek": m.PROFILES, "ask": m.ASK, "old-ask": m.ASK_OLD}[case["shortcut"]]
    hotkey = keys[case["profile"]]
    fails = []

    def check(ok, msg):
        if not ok:
            fails.append(msg)
        return ok

    def events(kind):
        return [e for e in m.jsonl(paths["events"]) if e["event"] == kind]

    try:
        if not check(m.wait(lambda: any(ready in l for l in t.screen()), 30 if pi else 15),
                     "agent never showed"):
            return name, fails
        time.sleep(3 if pi else 0.5)
        if pi:
            # Over "b0r20 words" (its 11 cells, as the drag over "alpha bravo"),
            # and a click later on its "0r2".
            scr = t.screen()
            y = next(i for i, l in enumerate(scr) if selected in l) + 1
            x = scr[y - 1].index(selected) + 1
            drag = (f"\x1b[<0;{x};{y}M\x1b[<32;{x + 5};{y}M"
                    f"\x1b[<32;{x + 10};{y}M\x1b[<0;{x + 10};{y}m").encode()
            click = f"\x1b[<0;{x + 2};{y}M\x1b[<0;{x + 2};{y}m".encode()
        t.send(drag)
        time.sleep(1.0)
        check(any("[0/" in l for l in t.screen()[:2]), "the drag did not put tmux in copy mode")
        t.send(hotkey)
        opened = m.wait(lambda: events("open"), 6)
        if check(opened, "Option+P in copy mode did not open a box"):
            # tmux's emacs copy mode leaves out the cell the drag ended on.
            sel = opened[-1].get("selection") or ""
            check(opened[-1].get("found") and sel in (selected, selected[:-1]), f"box on {sel!r}")
            check(bool(opened[-1].get("ask")) == ask, f"ask is {opened[-1].get('ask')!r}")
        leaked = [e for e in m.jsonl(paths["agent"]) if e["kind"] == "input" and hotkey.hex() in e["hex"]]
        check(not leaked, "the shortcut reached the agent")
        # Pi keeps no log: what it got shows in its editor, between its rules.
        rules = [i for i, l in enumerate(t.screen()) if l.startswith("─" * 20)]
        typed = pi and len(rules) >= 2 and t.screen()[rules[-2] + 1].strip()
        check(not typed, f"the shortcut reached Pi: {typed!r}")
        if not ask:
            check(m.wait(lambda: events("mark"), 6), "the explanation left no mark")
        t.send(b"\x1b")
        check(m.wait(lambda: events("close"), 5), "Esc did not close the box")
        if not ask:
            time.sleep(1.2)  # peekme drops mouse reports for 1 s after it turned them off
            n = len(events("open"))
            t.send(click)  # on "alpha" (Pi: on "b0r20")
            check(m.wait(lambda: events("mark_click"), 5), "a click on the mark did nothing")
            check(m.wait(lambda: len(events("open")) > n, 5), "a click on the mark opened no box")
            clicks = [e for e in m.jsonl(paths["agent"]) if e["kind"] == "input"
                      and click[2:].split(b"M")[0].decode() in e["raw"]]
            check(not clicks, "the click on the mark reached the agent")
            t.send(b"\x1b")
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
    agents = ["classic"] + (["pi"] if ra.real_binary("pi") else [])
    if "pi" not in agents:
        print("pi not installed: its cases are skipped")
    try:
        for agent, keys in itertools.product(agents, ("emacs", "vi")):
            with open(conf + keys, "w") as f:
                f.write(CONF + f"set -g mode-keys {keys}\n")
            env.tmux_conf = lambda mouse, ext, c=conf + keys: c
            for layers in ("tmux", "ssh+tmux"):
                for profile, shortcut in itertools.product(m.PROFILES, ("peek", "ask", "old-ask")):
                    case = dict(layers=layers, profile=profile, keys=keys, shortcut=shortcut, n=n,
                                agent=agent)
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
