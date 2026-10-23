#!/usr/bin/env python3
"""End-to-end with the real agent CLIs through tmux and SSH (no model calls).

    python3 tests/e2e/real_agents.py [--only SUBSTRING] [--jobs N] [--out DIR]

Like matrix.py, but the agent is the real `claude` (full screen and classic
screen), `codex`, `copilot` or `kiro-cli` (inline and after `/fullscreen`). The explainer stays canned
(PEEKME_FAKE_EXPLAINER), so nothing is sent to a model: the test drags over a
word on the agent's startup screen, presses the shortcut, and checks peekme's
event log and the screen. This catches what a fake agent cannot: how each real
agent copies a selection inside tmux, how it draws, which keys it asks for.

Run it from a directory Claude Code already trusts (this repo). Copilot gets
an isolated config for that directory, without changing the owner's trust
settings. Codex's update prompt is dismissed with Esc (skip once). Needs the
agents installed and logged in.
"""
import argparse, concurrent.futures as cf, itertools, json, os, re, shlex, shutil, subprocess, sys, tempfile, time

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
              [("Update available", b"\x1b"), ("Hooks need review", b"\x1b")]),
    "copilot": ("copilot", ["--no-auto-update"], "uses AI", "Copilot", []),
    # Kiro starts inline and leaves the mouse to the terminal; `/fullscreen`
    # (typed, then Enter once its menu has caught up) takes the mouse.
    "kiro-inline": ("kiro-cli", [], "ask a question", "Kiro", []),
    "kiro-fullscreen": ("kiro-cli", [], "ask a question", "Entered", []),
}
# Typed once the agent is ready, and the text that shows it took effect.
START = {"kiro-fullscreen": ([b"/fullscreen", b"\r"], "Entered fullscreen")}


# Agents that take the mouse themselves, so the wheel reaches them.
WHEEL_TO_AGENT = ("codex", "claude-fullscreen", "copilot", "kiro-fullscreen")


def real_binary(name):
    """The agent itself, not a peekme link or function in front of it."""
    for d in os.environ.get("PATH", "").split(os.pathsep):
        p = os.path.join(d, name)
        if os.path.isfile(p) and os.access(p, os.X_OK):
            real = os.path.realpath(p)
            if os.path.basename(real) != "peekme":
                return p
    return None


def isolated_copilot(work):
    """A temporary launcher carries the test config through SSH as well."""
    binary = real_binary("copilot")
    if not binary:
        return None
    home = os.path.join(work, "copilot-home")
    bindir = os.path.join(work, "agent-bin")
    os.makedirs(home)
    os.makedirs(bindir)
    source = os.path.join(os.environ.get("COPILOT_HOME", os.path.expanduser("~/.copilot")), "config.json")
    try:
        with open(source) as f:
            config = json.loads("\n".join(l for l in f if not l.lstrip().startswith("//")))
    except (OSError, ValueError):
        config = {}
    # Login identities use the existing keyring. Never copy tokens or the
    # owner's trustedFolders into the temporary config.
    allowed = ("firstLaunchAt", "appTipShown", "lastLoggedInUser", "loggedInUsers",
               "reasoningSummariesCleanupDone", "model", "lastUsedModel")
    config = {k: config[k] for k in allowed if k in config}
    config["trustedFolders"] = [os.getcwd()]
    with open(os.path.join(home, "config.json"), "w") as f:
        json.dump(config, f)
    launcher = os.path.join(bindir, "copilot")
    with open(launcher, "w") as f:
        f.write("#!/bin/sh\nexport COPILOT_HOME=" + shlex.quote(home)
                + "\nexec " + shlex.quote(binary) + ' "$@"\n')
    os.chmod(launcher, 0o700)
    return bindir


KIRO_SESSIONS = os.path.expanduser("~/.kiro/sessions/cli")


def kiro_sessions():
    try:
        return {f[:-5] for f in os.listdir(KIRO_SESSIONS) if f.endswith(".json")}
    except OSError:
        return set()


def delete_empty_kiro_sessions(before):
    """Every Kiro start opens a session; remove the empty ones this run made."""
    for sid in kiro_sessions() - before:
        log = os.path.join(KIRO_SESSIONS, sid + ".jsonl")
        if os.path.exists(log) and os.path.getsize(log) == 0:
            subprocess.run([real_binary("kiro-cli") or "kiro-cli", "chat", "--delete-session", sid],
                           capture_output=True)
            # Typed lines (`/fullscreen`) are kept beside it and outlive the delete.
            try:
                os.remove(os.path.join(KIRO_SESSIONS, sid + ".history"))
            except OSError:
                pass


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
    term_env = {k: v for k, v in m.clean_env(TERM="xterm-256color").items()
                if not k.startswith(("CLAUDECODE", "CLAUDE_CODE_"))}
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
        if case["agent"] in START:
            keys, done = START[case["agent"]]
            for k in keys:
                t.send(k)
                time.sleep(0.8)
            if not check(m.wait(lambda: any(done in l for l in t.screen()), 10), f"{done!r} never showed"):
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
        if case["agent"] in WHEEL_TO_AGENT:
            # The wheel goes to the agent (it scrolls its chat); the box stays.
            t.send(f"\x1b[<64;{first};{row}M\x1b[<65;{first};{row}M".encode())
            time.sleep(1.5)
            check(not events("close"), "the wheel closed the box")
            check(any("Test explanation" in l for l in t.screen()), "box gone after the wheel")
        t.send(b"\x1b")
        check(m.wait(lambda: events("close"), 5), "Esc did not close the box")
        time.sleep(1.0)
        if case["profile"] == "mac-default" and case["agent"] in ("claude-classic", "kiro-inline"):
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
    # Inline agents and Codex inside tmux with its mouse off leave selection
    # to the terminal. A PTY has no native selection. The macOS jobs use real
    # terminals to check these cases, including the SSH diagnostic for Codex.
    todo = [c for c in todo if not (c["mouse"] == "off" and (
        c["agent"] in ("claude-classic", "kiro-inline")
        or (c["agent"] == "codex" and c["layers"] != "direct")))]
    todo = [c for c in todo if args.only in f"{c['layers']}|mouse-{c['mouse']}|{c['profile']}|{c['agent']}"]
    for i, c in enumerate(todo):
        c["n"] = i
    work = tempfile.mkdtemp(prefix="peekme-real-")
    copilot_bin = isolated_copilot(work) if any(c["agent"] == "copilot" for c in todo) else None
    if copilot_bin:
        os.environ["PATH"] = copilot_bin + os.pathsep + os.environ.get("PATH", "")
    out = args.out or os.path.join(work, "cases")
    env = m.Env(work, need_ssh=any("ssh" in c["layers"] for c in todo))
    failed = []
    kiro_before = kiro_sessions()
    try:
        with cf.ThreadPoolExecutor(args.jobs) as pool:
            for name, fails in pool.map(lambda c: run_case(env, c, out), todo):
                print(("ok    " if not fails else "FAIL  ") + name + ("" if not fails else "  <- " + "; ".join(fails)),
                      flush=True)
                if fails:
                    failed.append(name)
    finally:
        env.stop()
        delete_empty_kiro_sessions(kiro_before)
    print(f"\n{len(todo) - len(failed)}/{len(todo)} passed. Artifacts: {out}")
    sys.exit(1 if failed else 0)


if __name__ == "__main__":
    main()
