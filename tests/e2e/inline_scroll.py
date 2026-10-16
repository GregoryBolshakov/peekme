#!/usr/bin/env python3
"""Long boxes over an agent's inline screen: real Kiro CLI (inline) and real
Claude Code (classic screen), Linux, no tmux, no model calls (the fake
explainer writes the boxes).

Inline the terminal keeps the chat in its own history. With a box open peekme
takes the mouse and scrolls that chat itself, with the boxes printed in it, as
tall as their text. No box ever scrolls its own text. Every frame from the
box's first to its last must be a window of

    V = T[:I] + box + T[I:]

where T is the chat as the terminal has it (its history, then the screen
above the agent's input area) and I the row after the selection (at it, for a
box above). A 12-line answer with a 12-line answer inside it (54 rows, more
than the screen) is scrolled to the oldest row of the history with the wheel,
over the boxes and beside them, then to the chat's last row a page at a time,
and back. After Esc the terminal shows the agent's own screen, and the chat
as before.

    python3 tests/e2e/inline_scroll.py [--agent kiro|claude]
"""
import os
import subprocess
import sys
import tempfile
import time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import claude_session  # noqa: E402
import kiro_session  # noqa: E402
import matrix as m  # noqa: E402
import real_agents as ra  # noqa: E402
import scroll as sc  # noqa: E402

LABEL = "b2r30"
WORDS = f"{LABEL} words of row 30 in block 2"
LINES = 12
# Outer box: its first line, then a blank row and a line for each of LINES,
# borders; the box inside it the same.
H = 2 * (2 * LINES + 3)


def with_history(t):
    """The terminal's history, then its screen."""
    t.save()
    out = subprocess.run([m.VTDUMP, t.log_path, str(m.COLS), str(m.ROWS)],
                         env=dict(os.environ, VTDUMP_HISTORY="100000"),
                         capture_output=True, text=True).stdout.splitlines()
    end = max(i for i, l in enumerate(out) if l.startswith("-- cursor"))
    return [r.rstrip() for r in out[:end]]


def settle(t, quiet=0.8, limit=15):
    prev, since, end = None, time.time(), time.time() + limit
    while time.time() < end:
        cur = t.screen()
        if cur != prev:
            prev, since = cur, time.time()
        elif time.time() - since > quiet:
            return cur
        time.sleep(0.1)
    return prev


def frames(t, start, end):
    t.save()
    out = subprocess.run([sc.VTFRAMES, t.log_path, str(m.COLS), str(m.ROWS), str(start), str(end)],
                         capture_output=True, text=True).stdout
    import json
    return [(f["off"], [r.rstrip() for r in f["rows"]])
            for f in map(json.loads, out.splitlines())]


def run(agent, out):
    os.makedirs(out, exist_ok=True)
    lines_file = os.path.join(out, "lines")
    with open(lines_file, "w") as f:
        f.write(f"{LINES} 0")
    events = os.path.join(out, "events.jsonl")
    if agent == "kiro":
        home, sid = kiro_session.kiro_home(out, 3, 40, fullscreen=False)
        extra = {"KIRO_HOME": home}
        argv = [m.PEEKME, ra.real_binary("kiro-cli"), "chat", "--resume-id", sid]
    else:
        home, sid = claude_session.config_dir(out, 3, 40)
        extra = {"CLAUDE_CONFIG_DIR": home}
        argv = [m.PEEKME, ra.real_binary("claude"), "--settings", '{"tui":"default"}',
                "--resume", sid]
    env = m.clean_env(**extra, TERM="xterm-256color", PEEKME_FAKE_EXPLAINER="1",
                      PEEKME_FAKE_LINES="@" + lines_file, PEEKME_EVENT_LOG=events,
                      PEEKME_SELECTION=WORDS, PEEKME_CHILD_LOG=os.path.join(out, "child.bin"),
                      DISPLAY="", WAYLAND_DISPLAY="")
    env.pop("CLAUDE_CODE_CHILD_SESSION", None)
    t = m.Term(argv, env, os.path.join(out, "terminal.bin"))

    def ev(kind):
        return [e for e in m.jsonl(events) if e["event"] == kind]

    def turn(send, limit=200):
        """Send until the chat stops moving; returns the view's places."""
        offs, still = [], 0
        for i in range(limit):
            n = len(ev("view_scroll"))
            send(i)
            time.sleep(0.12)
            new = ev("view_scroll")[n:]
            if not new:
                still += 1
                if still == 3:
                    break
                continue
            still = 0
            offs += [e["off"] for e in new]
        return offs

    fails = []
    try:
        if not m.wait(lambda: any("b2r40" in r for r in t.screen()), 60):
            return ["the chat never showed"]
        before = settle(t)
        if not any(LABEL in r for r in before):
            return [f"{LABEL} is not on the screen"]
        full = with_history(t)
        hist = len(full) - m.ROWS
        with t.lock:
            open_at = len(t.data)
        t.send(b"\x1bp")
        if not m.wait(lambda: ev("open"), 8):
            return ["Alt+P opened no box"]
        o = ev("open")[-1]
        strip = o.get("live_input")
        if strip is None:
            return ["no input area found under the chat"]
        T = full[:hist + strip]
        at = [i for i, r in enumerate(T) if LABEL in r]
        if len(at) != 1:
            return [f"{LABEL} is {len(at)} times in the chat"]
        S = at[0]
        I = S + 1 if o["box_top"] > S - hist else S
        if not m.wait(lambda: any(f"Line {LINES} of" in r for r in t.screen()), 8):
            return ["the answer never came"]
        scr = settle(t)
        row = next(r for r, l in enumerate(scr) if "Line 1 of the test" in l)
        col = scr[row].index("Line 1 of") + 1
        y = row + 1
        t.send(f"\x1b[<0;{col};{y}M\x1b[<32;{col + 3};{y}M\x1b[<0;{col + 3};{y}m".encode())
        time.sleep(0.3)
        t.send(b"\x1bp")
        if not m.wait(lambda: ev("nested_open"), 6):
            return ["a drag in the box and Alt+P opened no box inside it"]
        time.sleep(0.5)
        settle(t)
        with t.lock:
            done_at = len(t.data)

        # Up to the oldest row, the wheel over the boxes and beside them.
        spots = [(60, 12), (3, 25), (60, 30)]
        up = turn(lambda i: t.send(sc.wheel_bytes(True, *spots[i % 3])))
        settle(t)
        steps = [b - a for a, b in zip(up, up[1:])]
        if any(d != -3 for d in steps[:-1]) or (steps and not -3 <= steps[-1] < 0):
            fails.append(f"turns up moved {steps}, not 3 rows each")
        if not up or up[-1] != -hist:
            fails.append(f"stopped at {up[-1] if up else None}, not the history's first row {-hist}")
        top = t.screen()
        if top[0] != T[0]:
            fails.append(f"top row {top[0]!r}, not the chat's first {T[0]!r}")
        # Down a page at a time to the chat's last row past the boxes.
        down = turn(lambda i: t.send(b"\x1b[6~"))
        settle(t)
        if not down or down[-1] != H:
            fails.append(f"pages down stopped at {down[-1] if down else None}, not {H}")
        last = t.screen()[strip - 1]
        if last != T[-1]:
            fails.append(f"last chat row {last!r}, not {T[-1]!r}")
        # And back a little: a page up, wheel turns down.
        t.send(b"\x1b[5~")
        time.sleep(0.3)
        for _ in range(4):
            t.send(sc.wheel_bytes(False, 60, 12))
            time.sleep(0.1)
        settle(t)
        with t.lock:
            end_at = len(t.data)
        t.send(b"\x1b")
        time.sleep(0.4)
        t.send(b"\x1b")
        if not m.wait(lambda: ev("close"), 5):
            fails.append("Esc did not close the box")
        after = settle(t)
        for name, scr in (("before", before), ("after", after)):
            with open(os.path.join(out, f"{name}.txt"), "w") as f:
                f.write("\n".join(scr))
        # The agent's own screen: its status line may have changed meanwhile.
        own = subprocess.run([m.VTDUMP, os.path.join(out, "child.bin"), str(m.COLS), str(m.ROWS)],
                             capture_output=True, text=True).stdout.splitlines()[:m.ROWS]
        own = [r.rstrip() for r in own]
        if after != own:
            rows = [i for i in range(m.ROWS) if after[i] != own[i]]
            fails.append(f"screen not the agent's after Esc (rows {rows[:6]})")
        if after[:strip] != before[:strip]:
            rows = [i for i in range(strip) if after[i] != before[i]]
            fails.append(f"chat not as before after Esc (rows {rows[:6]})")

        fr = frames(t, open_at, end_at)
        R = strip
        ref = sc.box_from_parts(fr, done_at, R, H)
        if ref is None:
            fails.append("the whole box never showed in parts that agree")
        bad = 0
        for off, rows in fr:
            chat = rows[:R]
            if any(sc.SCROLLED.search(r) for r in rows):
                fails.append(f"frame {off}: a box scrolls its own text")
                bad += 1
            done = off >= done_at and ref is not None
            got = sc.offsets(chat, T, I, H if done else None, ref if done else None)
            if isinstance(got, str):
                fails.append(f"frame {off}: {got}")
                bad += 1
            if bad >= 6:
                break
        with open(os.path.join(out, "summary.txt"), "w") as f:
            f.write(f"frames {len(fr)} hist {hist} strip {strip} I {I} up {up} down {down}\n")
        if not fr:
            fails.append("no frames")
    finally:
        t.close()
        if agent == "claude":
            claude_session.forget_sign_in(home)
    return fails


def main():
    agents = ["kiro", "claude"]
    if "--agent" in sys.argv:
        agents = [sys.argv[sys.argv.index("--agent") + 1]]
    work = tempfile.mkdtemp(prefix="peekme-inline-scroll-")
    failed = 0
    for agent in agents:
        fails = run(agent, os.path.join(work, agent))
        print(("ok    " if not fails else "FAIL  ") + agent
              + ("" if not fails else "  <- " + "; ".join(fails[:8])))
        failed += bool(fails)
    print(f"\n{len(agents) - failed}/{len(agents)} passed. Artifacts: {work}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
