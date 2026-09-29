#!/usr/bin/env python3
"""End-to-end with the real agent CLIs through tmux and SSH (no model calls).

    python3 tests/e2e/real_agents.py [--only SUBSTRING] [--jobs N] [--out DIR]

Like matrix.py, but the agent is the real `claude` (full screen and classic
screen), `codex` or `copilot`. The explainer stays canned
(PEEKME_FAKE_EXPLAINER), so nothing is sent to a model: the test drags over a
word on the agent's startup screen, presses the shortcut, and checks peekme's
event log and the screen. This catches what a fake agent cannot: how each real
agent copies a selection inside tmux, how it draws, which keys it asks for.

Run it from a directory Claude Code already trusts (this repo). Copilot's
folder-trust question is answered with "Yes" for the session only, Codex's
update prompt with Esc (skip once). Needs the agents installed and logged in.
"""
import argparse, concurrent.futures as cf, itertools, os, re, shlex, shutil, subprocess, sys, tempfile, time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import matrix as m  # noqa: E402

# name -> (argv after the binary, words that show it is ready, word to drag,
#          dialogs as (text on screen, keys to send))
AGENTS = {
    "claude-fullscreen": ("claude", ["--settings", '{"tui":"fullscreen"}'], "Claude Code", "Claude",
                          []),
    "claude-classic": ("claude", ["--settings", '{"tui":"default"}'], "Claude Code", "Claude", []),
    # No update prompt (only for this run; the user's config is not touched).
    "codex": ("codex", ["-c", "check_for_update_on_startup=false"], "OpenAI Codex", "OpenAI",
              [("Update available", b"\x1b")]),
    "copilot": ("copilot", ["--no-auto-update"], "uses AI", "Copilot", [("Do you trust", b"\r")]),
}


def real_binary(name):
    """The agent itself, not a peekme link or function in front of it."""
    for d in os.environ.get("PATH", "").split(os.pathsep):
        p = os.path.join(d, name)
        if os.path.isfile(p) and os.access(p, os.X_OK):
            real = os.path.realpath(p)
            if os.path.basename(real) != "peekme":
                return p
    return None


def find_word(lines, word, skip_rows=0):
    """1-based (row, first col, last col) of `word` on the screen."""
    for r, line in enumerate(lines[skip_rows:], start=skip_rows):
        c = line.find(word)
        if c >= 0:
            return r + 1, c + 1, c + len(word)
    return None


def run_case(env, case, out):
    name = f"{case['layers']}|mouse-{case['mouse']}|{case['profile']}|{case['agent']}"
    d = os.path.join(out, name.replace("|", "_").replace("+", "-"))
    os.makedirs(d, exist_ok=True)
    events_path = os.path.join(d, "events.jsonl")
    binary, args, ready, word, dialogs = AGENTS[case["agent"]]
    agent = real_binary(binary)
    if not agent:
        return name, [f"{binary} not installed"]
    tmux_dir = os.path.dirname(os.path.abspath(env.tmux))
    # The local PATH, literally: over SSH the remote shell's PATH may lack node
    # (agents installed with nvm) - on this test machine it is the same host.
    path = tmux_dir + os.pathsep + os.environ.get("PATH", "")
    inner = " ".join(["cd", shlex.quote(os.getcwd()), "&&", "env", f"PATH={shlex.quote(path)}",
                      *([f"LD_LIBRARY_PATH={shlex.quote(env.libs)}"] if env.libs else []),
                      "PEEKME_FAKE_EXPLAINER=1", f"PEEKME_EVENT_LOG={shlex.quote(events_path)}",
                      "DISPLAY=", "WAYLAND_DISPLAY=",
                      shlex.quote(m.PEEKME), shlex.quote(agent), *map(shlex.quote, args)])
    case = dict(case, extkeys="off")
    argv, socks = layered(env, case, inner)
    term_env = {k: v for k, v in os.environ.items()
                if not k.startswith(("CLAUDECODE", "CLAUDE_CODE_", "TMUX"))}
    term_env["TERM"] = "xterm-256color"
    t = m.Term(argv, term_env, os.path.join(d, "terminal.bin"))
    fails = []
    hotkey = m.PROFILES[case["profile"]]

    def events(kind):
        return [e for e in m.jsonl(events_path) if e["event"] == kind]

    def check(ok, msg):
        if not ok:
            fails.append(msg)
        return ok

    try:
        # Ready: the agent's screen is up and no startup dialog has shown for
        # 3 s (some appear after the banner).
        deadline, calm = time.time() + 60, None
        while time.time() < deadline:
            text = "\n".join(t.screen())
            dialog = next((keys for prompt, keys in dialogs if prompt in text), None)
            if dialog:
                t.send(dialog)
                calm = None
                time.sleep(2)
                continue
            if ready in text:
                calm = calm or time.time()
                if time.time() - calm >= 3:
                    break
            time.sleep(0.5)
        if not check(ready in "\n".join(t.screen()), f"{case['agent']} never showed {ready!r}"):
            return name, fails
        time.sleep(2)
        before = t.screen()
        where = find_word(before, word)
        if not check(where, f"{word!r} not on screen"):
            return name, fails
        row, first, last = where
        t.send(f"\x1b[<0;{first};{row}M\x1b[<32;{(first + last) // 2};{row}M"
               f"\x1b[<32;{last};{row}M\x1b[<0;{last};{row}m".encode())
        time.sleep(1.0)
        t.send(hotkey)
        opened = m.wait(lambda: events("open"), 8)
        if check(opened, "shortcut did not open a box"):
            sel = opened[-1].get("selection") or ""
            check(opened[-1].get("found") and sel.strip() and sel.strip() in before[row - 1],
                  f"box on the wrong selection: {sel!r}")
        check(m.wait(lambda: any("Test explanation" in l for l in t.screen()), 5), "explanation not on screen")
        t.send(b"\x1b")
        check(m.wait(lambda: events("close"), 5), "Esc did not close the box")
        time.sleep(1.0)
        if case["profile"] == "mac-default" and case["agent"] == "claude-classic":
            # tmux handled the drag: peekme cannot see the click that clears
            # it, so a copy counts for π for 15 s. Wait that out.
            time.sleep(16)
        if case["profile"] == "mac-default":
            # Nothing selected any more: π is a letter for the agent. A click on
            # the same text row clears the selection in every agent (Codex
            # keeps its highlight after a click on empty space).
            click = f"\x1b[<0;{last + 6};{row}M\x1b[<0;{last + 6};{row}m".encode()
            t.send(click)
            time.sleep(1.0)
            n = len(events("open"))
            t.send("π".encode())
            time.sleep(1.5)
            check(len(events("open")) == n, "π with nothing selected opened a box")
            check(any("π" in l for l in t.screen()), "π with nothing selected was not typed")
            t.send(b"\x7f")
    finally:
        t.close()
        for s in socks:
            subprocess.run([env.tmux, "-L", s, "kill-server"], capture_output=True,
                           env=dict(os.environ, LD_LIBRARY_PATH=env.libs))
        with open(os.path.join(d, "screen.txt"), "w") as f:
            f.write("\n".join(t.screen()))
    return name, fails


def layered(env, case, inner):
    lib = f"LD_LIBRARY_PATH={shlex.quote(env.libs)} " if env.libs else ""

    def tmux(cmd, sock):
        conf = env.tmux_conf(case["mouse"], case["extkeys"])
        return (f"{lib}{shlex.quote(env.tmux)} -L {sock} -f {shlex.quote(conf)} "
                f"new-session -x {m.COLS} -y {m.ROWS} {shlex.quote(cmd)}")

    def ssh(cmd):
        # sshd does not pass the locale on here; real servers have one. Without
        # a UTF-8 locale tmux draws every non-ASCII character as `_`.
        remote = "LANG=C.UTF-8 LC_ALL=C.UTF-8 " + cmd
        return " ".join(shlex.quote(a) for a in env.ssh_base) + " " + shlex.quote(remote)

    s1, s2 = f"real{case['n']}a", f"real{case['n']}b"
    line = {"direct": lambda: inner, "tmux": lambda: tmux(inner, s1),
            "ssh+tmux": lambda: ssh(tmux(inner, s2))}[case["layers"]]()
    return ["/bin/sh", "-c", line], (s1, s2)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--only", default="")
    ap.add_argument("--jobs", type=int, default=4)
    ap.add_argument("--out", default=None)
    args = ap.parse_args()
    todo = []
    for layer, agent, profile in itertools.product(["direct", "tmux", "ssh+tmux"], AGENTS, m.PROFILES):
        for mouse in (["off"] if layer == "direct" else ["off", "on"]):
            todo.append(dict(layers=layer, agent=agent, profile=profile, mouse=mouse))
    # Claude's classic screen leaves the mouse to the terminal (or to tmux with
    # `mouse on`). A pseudo-terminal has no selection of its own, so those
    # cases belong to the macOS job with real terminals.
    todo = [c for c in todo if not (c["agent"] == "claude-classic" and c["mouse"] == "off")]
    todo = [c for c in todo if args.only in f"{c['layers']}|mouse-{c['mouse']}|{c['profile']}|{c['agent']}"]
    for i, c in enumerate(todo):
        c["n"] = i
    work = tempfile.mkdtemp(prefix="peekme-real-")
    out = args.out or os.path.join(work, "cases")
    env = m.Env(work, need_ssh=any("ssh" in c["layers"] for c in todo))
    failed = []
    try:
        with cf.ThreadPoolExecutor(args.jobs) as pool:
            for name, fails in pool.map(lambda c: run_case(env, c, out), todo):
                print(("ok    " if not fails else "FAIL  ") + name + ("" if not fails else "  <- " + "; ".join(fails)),
                      flush=True)
                if fails:
                    failed.append(name)
    finally:
        env.stop()
    print(f"\n{len(todo) - len(failed)}/{len(todo)} passed. Artifacts: {out}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
