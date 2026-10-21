#!/usr/bin/env python3
"""Saved answers outlive peekme: peek, quit, open the same chat again.

    python3 tests/e2e/reopen.py [--only NAME]

Real Codex, Claude Code, Copilot CLI, Kiro CLI and Pi, directly, the explainer
canned (no model calls). Each opens a saved chat, peeks at a row and at
words in the box (a box inside it), and quits: peekme and the agent are
killed, as when the window closes. Then the same chat opens again under a
new peekme. The row must be underlined again, and a click on it (full
screen) or Alt+P on the same words (inline screens, where the terminal keeps
plain clicks) must show the saved answer, with the inner one's underline.
About 5 minutes.
"""
import argparse, os, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import matrix as m  # noqa: E402
import real_agents as ra  # noqa: E402
import scroll as sc  # noqa: E402
import claude_session as cs  # noqa: E402
import kiro_session as ks  # noqa: E402
import pi_session as ps  # noqa: E402

SEL = "b2r35 words of row 35"


def underlined(t):
    t.save()
    out = subprocess.run([m.VTDUMP, t.log_path, str(m.COLS), str(m.ROWS)], capture_output=True,
                         text=True, env=dict(os.environ, VTDUMP_UNDERLINE="1")).stdout
    return [l for l in out.splitlines() if l.startswith("-- underline")]


def box_open(rows):
    return any(r.lstrip().startswith("╭") for r in rows)


def nested(t, rows, top=0, left=0):
    """Drag over "Line 2 of" in the open box and press Alt+P: a box inside."""
    pos = next(((i, r.index("Line 2 of")) for i, r in enumerate(rows) if "Line 2 of" in r), None)
    if not pos:
        return False
    i, c = pos
    y, c = top + i + 1, c + left
    t.send(f"\x1b[<0;{c + 1};{y}M\x1b[<32;{c + 5};{y}M\x1b[<0;{c + 5};{y}m".encode())
    time.sleep(0.6)
    t.send(b"\x1bp")
    time.sleep(2)
    return any(r.lstrip().startswith("│ ╭") for r in t.screen())


def full_screen(agent, out):
    """A row peeked in full screen; after the reopen a click shows its answer."""
    s = sc.Session(out, 2, False, (120, 40), 30, False, None, True, True, agent,
                   answers=os.path.join(out, "answers"))
    fails = []

    def check(ok, msg):
        if not ok:
            fails.append(msg)

    try:
        s.to_end()
        _, _, sel = sc.select(s, 10)
        label = sc.LABEL.search(sel[0])[0]
        s.t.send(b"\x1bp")
        time.sleep(2.5)
        check(box_open(s.chat()), "no box at the first peek")
        check(nested(s.t, s.chat(), s.top, s.left), "no box inside the box")
        s.t.send(b"\x1b")
        time.sleep(0.4)
        s.t.send(b"\x1b")
        time.sleep(1.0)
        check(not box_open(s.chat()), "the boxes did not close")
        check(any(label in l for l in underlined(s.t)), "not underlined after the peek")
        s.reopen(os.path.join(out, "terminal2.bin"))
        s.to_end()
        time.sleep(1.0)
        check(any(label in l for l in underlined(s.t)), "not underlined after the reopen")
        chat = s.chat()
        i = next((i for i, r in enumerate(chat) if label in r), None)
        if i is None:
            check(False, f"{label} not on screen after the reopen")
            return fails
        c = chat[i].index(label) + s.left
        y = s.top + i + 1
        s.t.send(f"\x1b[<0;{c + 3};{y}M\x1b[<0;{c + 3};{y}m".encode())
        time.sleep(1.5)
        rows = s.chat()
        check(any(r.startswith("╭") and label in r for r in rows), "a click opened no box")
        check(any("saved" in r for r in rows), "the box is not the saved answer")
        screen = s.t.screen()
        inner = {int(l.split()[3].rstrip(":")) for l in underlined(s.t) if '"Line"' in l}
        check(any("Line 2 of" in screen[r] for r in inner if r < len(screen)),
              "the inner answer has no underline")
    finally:
        s.close()
    return fails


def inline(agent, out):
    """Claude Code's classic screen, Kiro's inline one, Pi's regular one: Alt+P on the words
    (PEEKME_SELECTION) after the reopen shows the saved answer."""
    m.COLS, m.ROWS = 120, 40
    env = m.clean_env(TERM="xterm-256color", PEEKME_FAKE_EXPLAINER="1", PEEKME_SELECTION=SEL,
                      PEEKME_ANSWERS_DIR=os.path.join(out, "answers"), DISPLAY="",
                      WAYLAND_DISPLAY="", PEEKME_EVENT_LOG=os.path.join(out, "events.jsonl"))
    env.pop("CLAUDE_CODE_CHILD_SESSION", None)
    home = None
    if agent == "claude":
        home, sid = cs.config_dir(out, 3, 40)
        env["CLAUDE_CONFIG_DIR"] = home
        argv = [m.PEEKME, ra.real_binary("claude"), "--settings", '{"tui":"default"}',
                "--resume", sid]
    elif agent == "pi":
        agent_dir, path = ps.pi_agent(out, 3, 40)
        env.update(PI_CODING_AGENT_DIR=agent_dir, PI_OFFLINE="1", PI_SKIP_VERSION_CHECK="1",
                   PI_TELEMETRY="0")
        argv = [m.PEEKME, ra.real_binary("pi"), "--tui-mode", "regular", "--session", path]
    else:
        khome, sid = ks.kiro_home(out, 3, 40, fullscreen=False)
        env["KIRO_HOME"] = khome
        argv = [m.PEEKME, ra.real_binary("kiro-cli"), "chat", "--resume-id", sid]
    fails = []

    def check(ok, msg):
        if not ok:
            fails.append(msg)

    try:
        for run in (1, 2):
            t = m.Term(argv, env, os.path.join(out, f"terminal{run}.bin"), colors=True)
            try:
                m.wait(lambda: SEL in "\n".join(t.screen()), 30, 0.5)
                time.sleep(3)
                check(SEL in "\n".join(t.screen()), f"run {run}: the chat did not show")
                if run == 2:
                    check(any("b2r35" in l for l in underlined(t)),
                          "not underlined after the reopen")
                t.send(b"\x1bp")
                time.sleep(2.5)
                rows = t.screen()
                check(box_open(rows), f"run {run}: no box")
                saved = any("saved" in r for r in rows)
                check(saved == (run == 2), f"run {run}: saved answer {saved}")
                t.send(b"\x1b")
                time.sleep(1.0)
                if run == 1:
                    check(any("b2r35" in l for l in underlined(t)), "not underlined after the peek")
            finally:
                t.close()
            time.sleep(1.5)
    finally:
        if home:
            cs.forget_sign_in(home)
    return fails


CASES = {
    "codex": lambda out: full_screen("codex", out),
    "claude": lambda out: full_screen("claude", out),
    "copilot": lambda out: full_screen("copilot", out),
    "kiro": lambda out: full_screen("kiro", out),
    "claude classic": lambda out: inline("claude", out),
    "kiro inline": lambda out: inline("kiro", out),
    "pi": lambda out: full_screen("pi", out),
    "pi regular": lambda out: inline("pi", out),
}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", action="append", help="run only these cases")
    args = ap.parse_args()
    work = tempfile.mkdtemp(prefix="peekme-reopen-")
    names = args.only or list(CASES)
    bad = 0
    for name in names:
        out = os.path.join(work, name.replace(" ", "-"))
        os.makedirs(out, exist_ok=True)
        try:
            fails = CASES[name](out)
        except Exception as e:  # noqa: BLE001
            fails = [f"error: {e!r}"[:200]]
        bad += bool(fails)
        print(f"{'FAIL' if fails else 'ok  '} {name}" + "".join(f"\n     {f}" for f in fails),
              flush=True)
    print(f"{len(names) - bad}/{len(names)} passed  (logs in {work})")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
