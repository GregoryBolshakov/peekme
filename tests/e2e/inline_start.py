#!/usr/bin/env python3
"""An agent that draws relative to where it starts, under a shell prompt.

    python3 tests/e2e/inline_start.py

Claude Code's classic screen draws from wherever the cursor is, with relative
moves only, and never asks where that is. So peekme must learn the start row
itself, and must never paint the rows above it: they hold the user's earlier
shell output, which peekme has not seen. The terminal here prints shell lines
first (a few, or a screenful so the screen scrolls), then starts peekme around
a fake agent in that style, directly, in tmux and over SSH + tmux.

Checks: the box opens right under the selected text as the terminal shows it,
the earlier shell lines stay as they were while the box is open, and after
Esc the screen is exactly as before.
"""
import os, shlex, sys, tempfile, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import matrix as m  # noqa: E402


def run(env, layer, shell_lines, n, out):
    name = f"{layer}|{shell_lines} shell lines"
    d = os.path.join(out, f"{layer}-{shell_lines}".replace("+", "-"))
    os.makedirs(d, exist_ok=True)
    paths = {"events": os.path.join(d, "events.jsonl"), "agent": os.path.join(d, "agent.jsonl")}
    case = dict(layers=layer, mouse="off", extkeys="off", agent="classic", n=n,
                env={"PEEKME_SELECTION": "alpha bravo"})
    argv, socks = m.build_command(env, case, paths)
    shell = "".join(f"user@host:~$ echo shell line {i}\\r\\nshell line {i}\\r\\n" for i in range(shell_lines // 2))
    line = f"printf %b {shlex.quote(shell + 'user@host:~$ claude' + chr(92) + 'r' + chr(92) + 'n')}; {argv[2]}"
    t = m.Term(["/bin/sh", "-c", line], m.clean_env(TERM="xterm-256color"), os.path.join(d, "terminal.bin"))
    fails = []

    def events(kind):
        return [e for e in m.jsonl(paths["events"]) if e["event"] == kind]

    try:
        if not m.wait(lambda: any("fake status line" in l for l in t.screen()), 15):
            return name, ["agent never drew"]
        time.sleep(0.5)
        before = t.screen()
        sel_row = max(i for i, l in enumerate(before) if "alpha bravo" in l)
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
    failed = 0
    try:
        n = 0
        for layer in layers:
            for shell_lines in (2, 34):
                name, fails = run(env, layer, shell_lines, n, out)
                n += 1
                print(("ok    " if not fails else "FAIL  ") + name + ("" if not fails else "  <- " + "; ".join(fails)))
                failed += bool(fails)
    finally:
        env.stop()
    print(f"\n{2 * len(layers) - failed}/{2 * len(layers)} passed. Artifacts: {out}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
