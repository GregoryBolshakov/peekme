#!/usr/bin/env python3
"""An agent that draws relative to where it starts, under a shell prompt.

    python3 tests/e2e/inline_start.py

Claude Code's classic screen draws from wherever the cursor is, with relative
moves only, and never asks where that is. So peekme must learn the start row
itself, and must never paint the rows above it: they hold the user's earlier
shell output, which peekme has not seen. The terminal here prints shell lines
first (a few, or a screenful so the screen scrolls), then starts peekme around
a fake agent in that style, directly, in tmux and over SSH + tmux. Real Pi's
regular screen (`--tui-mode regular`, a saved chat in an agent folder of its
own) draws the same way and gets the same cases.

Checks: the box opens right under the selected text as the terminal shows it,
the earlier shell lines stay as they were while the box is open, a drag in the
box and Alt+P open a box inside it (the agent takes no mouse and tmux has the
mouse off), the two long answers together are taller than the screen and the
wheel over them scrolls the chat with both boxes in it (peekme does that
itself, inline), never the text inside a box, and after Esc the screen is
exactly as before.
"""
import os, re, shlex, sys, tempfile, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import matrix as m  # noqa: E402
import pi_session  # noqa: E402
import real_agents as ra  # noqa: E402

# A box that scrolls its own text shows where it is in its bottom border.
SCROLLED = re.compile(r"╰[^╯]*\s\d+/\d+\s[^╯]*╯")


def pi_run(env, work):
    """Real Pi on its regular screen, opening a saved chat of labelled rows
    (`b0rNN`): the words after `env`, and what shows it is ready and what is
    selected."""
    agent, path = pi_session.pi_agent(work, 1, 30)
    pi = ra.real_binary("pi")
    tmux_dir = os.path.dirname(os.path.abspath(env.tmux))
    # Over SSH the remote PATH may lack node (Pi installed with nvm).
    path_env = f"{tmux_dir}:{os.path.dirname(pi)}:" + os.environ.get("PATH", "")
    words = [f"PATH={shlex.quote(path_env)}", f"PI_CODING_AGENT_DIR={shlex.quote(agent)}",
             "PI_OFFLINE=1", "PI_SKIP_VERSION_CHECK=1", "PI_TELEMETRY=0",
             shlex.quote(m.PEEKME), shlex.quote(pi), "--no-skills", "--tui-mode", "regular",
             "--session", shlex.quote(path)]
    return words, "b0r30", "b0r20 words of row 20 in block 0"


def run(env, layer, shell_lines, n, out, pi=None):
    agent = "pi" if pi else "classic"
    name = f"{agent}|{layer}|{shell_lines} shell lines"
    d = os.path.join(out, f"{agent}-{layer}-{shell_lines}".replace("+", "-"))
    os.makedirs(d, exist_ok=True)
    paths = {"events": os.path.join(d, "events.jsonl"), "agent": os.path.join(d, "agent.jsonl")}
    ready, selected = "fake status line", "alpha bravo"
    case = dict(layers=layer, mouse="off", extkeys="off", agent="classic", n=n,
                env={"PEEKME_SELECTION": selected, "PEEKME_FAKE_LINES": "12"})
    if pi:
        case["run"], ready, selected = pi
        case["env"]["PEEKME_SELECTION"] = selected
    argv, socks = m.build_command(env, case, paths)
    shell = "".join(f"user@host:~$ echo shell line {i}\\r\\nshell line {i}\\r\\n" for i in range(shell_lines // 2))
    typed = "user@host:~$ " + agent + chr(92) + "r" + chr(92) + "n"
    line = f"printf %b {shlex.quote(shell + typed)}; {argv[2]}"
    t = m.Term(["/bin/sh", "-c", line], m.clean_env(TERM="xterm-256color"), os.path.join(d, "terminal.bin"))
    fails = []

    def events(kind):
        return [e for e in m.jsonl(paths["events"]) if e["event"] == kind]

    try:
        if not m.wait(lambda: any(ready in l for l in t.screen()), 30 if pi else 15):
            return name, ["agent never drew"]
        time.sleep(3 if pi else 0.5)
        before = t.screen()
        sel_row = max(i for i, l in enumerate(before) if selected in l)
        shell_rows = [i for i, l in enumerate(before) if "shell line" in l or "user@host" in l]
        t.send(b"\x1bp")
        opened = m.wait(lambda: events("open"), 6)
        if not opened:
            return name, ["Alt+P did not open a box"]
        o = opened[-1]
        if not o.get("found"):
            fails.append(f"selection not found: {o.get('selection')!r}")
        during = t.screen()
        if o.get("box_top") == sel_row + 1:
            pass
        elif o.get("box_top", 0) + 0 < sel_row:  # drawn above: must end right at the selection
            if not any("peek" in l for l in during[:sel_row]):
                fails.append(f"box at row {o.get('box_top')} does not touch the selection on row {sel_row}")
        else:
            fails.append(f"box at row {o.get('box_top')}, the terminal shows the selection on row {sel_row}")
        changed = [r for r in shell_rows if during[r] != before[r]]
        if changed:
            fails.append(f"shell output painted over on rows {changed}")
        # The agent takes no mouse and tmux has it off, but peekme turns mouse
        # reports on while the box is open, and tmux passes them on: a drag
        # over a word in the box and Alt+P open a box inside it.
        if m.wait(lambda: any("Test explanation" in l for l in t.screen()), 6):
            scr = t.screen()
            row = next(r for r, l in enumerate(scr) if "Test explanation" in l)
            col, y = scr[row].index("explanation"), row + 1
            t.send((f"\x1b[<0;{col + 1};{y}M\x1b[<32;{col + 6};{y}M"
                    f"\x1b[<32;{col + 11};{y}M\x1b[<0;{col + 11};{y}m").encode())
            time.sleep(0.3)
            t.send(b"\x1bp")
            nested = m.wait(lambda: events("nested_open"), 4)
            if not nested:
                fails.append("a drag in the box and Alt+P opened no box inside it")
            elif nested[-1].get("words") != "explanation":
                fails.append(f"inner box on the wrong words: {nested[-1].get('words')!r}")
            time.sleep(0.6)
            # The wheel over the boxes: the chat with both in it scrolls.
            screens = []
            for up in (False,) * 24 + (True,) * 3:
                t.send(f"\x1b[<{64 + (not up)};{col + 1};{y}M".encode())
                time.sleep(0.15)
                screens.append(t.screen())
            if not events("view_scroll"):
                fails.append("the wheel did not scroll the chat with the boxes in it")
            if any(SCROLLED.search(l) for scr in screens for l in scr):
                fails.append("a box scrolls its own text")
            if not any(l.startswith("╰") for scr in screens for l in scr[:-1]):
                fails.append("the boxes' bottom never came into view")
            painted = sorted({r for scr in screens for r in shell_rows if scr[r] != before[r]})
            if painted:
                fails.append(f"shell output painted over while scrolling, rows {painted}")
            t.send(b"\x1b")
            time.sleep(0.4)
        else:
            fails.append("explanation not on screen")
        t.send(b"\x1b")
        m.wait(lambda: events("close"), 5)
        time.sleep(0.8)
        after = t.screen()
        if "tmux" in layer:
            # tmux's own status row (it renames the window when peekme starts).
            before, during, after = before[:-1], during[:-1], after[:-1]
        for label, scr in (("before", before), ("during", during), ("after", after)):
            with open(os.path.join(d, f"{label}.txt"), "w") as f:
                f.write("\n".join(scr))
        if after != before:
            rows = [i for i in range(min(len(after), len(before))) if after[i] != before[i]]
            fails.append(f"screen not as before after Esc (rows {rows[:6]})")
    finally:
        t.send(b"\x04")
        time.sleep(0.2)
        t.close()
        for s in socks:
            import subprocess
            subprocess.run([env.tmux, "-L", s, "kill-server"], capture_output=True,
                           env=dict(os.environ, LD_LIBRARY_PATH=env.libs))
    return name, fails


def main():
    work = tempfile.mkdtemp(prefix="peekme-inline-")
    out = os.path.join(work, "cases")
    layers = ["direct", "tmux", "ssh+tmux"]
    env = m.Env(work, need_ssh=True)
    agents = [None]
    if ra.real_binary("pi"):
        agents.append("pi")
    else:
        print("pi not installed: its cases are skipped")
    failed = total = 0
    try:
        n = 0
        for agent in agents:
            for layer in layers:
                for shell_lines in (2, 34):
                    pi = pi_run(env, os.path.join(work, f"pi-{n}")) if agent else None
                    name, fails = run(env, layer, shell_lines, n, out, pi)
                    n += 1
                    total += 1
                    print(("ok    " if not fails else "FAIL  ") + name + ("" if not fails else "  <- " + "; ".join(fails)))
                    failed += bool(fails)
    finally:
        env.stop()
    print(f"\n{total - failed}/{total} passed. Artifacts: {out}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
