#!/usr/bin/env python3
"""End-to-end matrix: peekme through tmux and SSH, as different terminals.

    python3 tests/e2e/matrix.py [--quick] [--only SUBSTRING] [--jobs N] [--out DIR]

Each case starts `peekme` around a fake agent (tests/e2e/fakeagent.py, named
claude / copilot / codex so peekme treats it as that agent) inside some layers:

    direct         terminal -> peekme
    tmux           terminal -> tmux -> peekme
    ssh            terminal -> ssh -> peekme
    ssh+tmux       terminal -> ssh -> tmux -> peekme
    tmux+ssh+tmux  terminal -> tmux -> ssh -> tmux -> peekme

tmux runs with every combination of `mouse on|off` and `extended-keys on|off`.
This script plays the terminal: it sends exactly what a terminal sends for
Option+P / Alt+P (`π` from a Mac terminal in its default settings, `ESC p` with
Option as Meta or on Linux) and SGR mouse reports for a drag.

Checks per case: the drag reaches the agent; the shortcut opens a box on the
dragged text and never reaches the agent; typing with the box open reaches the
agent; Esc closes it and the screen is as before; with nothing selected, `π` is
typed into the agent. The explainer is canned (PEEKME_FAKE_EXPLAINER), so no
model is called.

Needs: a release build (`cargo build --release --bins --examples`), tmux, and for the
SSH layers `sshd` + `ssh` (a private sshd is started on 127.0.0.1 as this user).
Set E2E_TMUX / E2E_SSHD to use binaries that are not on PATH, and
E2E_LD_LIBRARY_PATH if they need extra libraries.
"""
import argparse, base64, concurrent.futures as cf, itertools, json, os, pty, select, shlex, shutil
import signal, struct, subprocess, sys, tempfile, termios, fcntl, threading, time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
PEEKME = os.path.join(ROOT, "target/release/peekme")
VTDUMP = os.path.join(ROOT, "target/release/examples/vtdump")
COLS, ROWS = 120, 40

PROFILES = {
    # What the terminal sends for the shortcut.
    "mac-default": "π".encode(),      # Terminal, iTerm2, Ghostty... with Option not set as Meta
    "meta": b"\x1bp",                 # Option as Meta / Esc+, and Linux terminals' Alt
}
# Drag over "alpha bravo" on the agent's first text row (1-based cells).
DRAG = b"\x1b[<0;3;3M\x1b[<32;8;3M\x1b[<32;13;3M\x1b[<0;13;3m"
CLICK = b"\x1b[<0;30;6M\x1b[<0;30;6m"
SELECTED = "alpha bravo"


class Term:
    """A pseudo-terminal we drive like a terminal emulator would."""

    def __init__(self, argv, env, log_path):
        self.data = bytearray()
        self.log_path = log_path
        self.pid, self.fd = pty.fork()
        if self.pid == 0:
            fcntl.ioctl(0, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
            os.execvpe(argv[0], argv, env)
        fcntl.ioctl(self.fd, termios.TIOCSWINSZ, struct.pack("HHHH", ROWS, COLS, 0, 0))
        self.alive = True
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        while self.alive:
            try:
                r, _, _ = select.select([self.fd], [], [], 0.2)
                if not r:
                    continue
                b = os.read(self.fd, 65536)
            except OSError:
                break
            if not b:
                break
            self.data.extend(b)
            # Answer the queries a real terminal answers (tmux waits for some).
            if b"\x1b[6n" in b:
                os.write(self.fd, b"\x1b[1;1R")
            if b"\x1b[c" in b or b"\x1b[0c" in b:
                os.write(self.fd, b"\x1b[?62;22c")
            if b"\x1b[>c" in b or b"\x1b[>0c" in b:
                os.write(self.fd, b"\x1b[>41;354;0c")
        self.alive = False

    def send(self, b):
        os.write(self.fd, b)

    def screen(self):
        with open(self.log_path, "wb") as f:
            f.write(self.data)
        out = subprocess.run([VTDUMP, self.log_path, str(COLS), str(ROWS)],
                             capture_output=True, text=True).stdout
        return out.splitlines()[:ROWS]

    def close(self):
        self.alive = False
        try:
            os.kill(self.pid, signal.SIGKILL)
            os.waitpid(self.pid, 0)
        except OSError:
            pass
        with open(self.log_path, "wb") as f:
            f.write(self.data)


def jsonl(path):
    try:
        with open(path, encoding="utf-8") as f:
            return [json.loads(l) for l in f if l.strip()]
    except FileNotFoundError:
        return []


def wait(pred, timeout=6.0, step=0.1):
    end = time.time() + timeout
    while time.time() < end:
        v = pred()
        if v:
            return v
        time.sleep(step)
    return pred()


class Env:
    """Shared setup: agent shims, tmux and sshd."""

    def __init__(self, work, need_ssh):
        self.work = work
        self.tmux = os.environ.get("E2E_TMUX") or shutil.which("tmux")
        self.libs = os.environ.get("E2E_LD_LIBRARY_PATH", "")
        if not self.tmux:
            sys.exit("tmux not found (set E2E_TMUX)")
        self.bin = os.path.join(work, "bin")
        os.makedirs(self.bin)
        for style in ("claude", "copilot", "codex"):
            p = os.path.join(self.bin, style)
            with open(p, "w") as f:
                f.write(f"#!/bin/sh\nexec {shlex.quote(sys.executable)} "
                        f"{shlex.quote(os.path.join(HERE, 'fakeagent.py'))} "
                        f"--style {style} --log \"$FAKEAGENT_LOG\"\n")
            os.chmod(p, 0o755)
        self.sshd = None
        if need_ssh:
            self.start_sshd()

    def start_sshd(self):
        sshd = os.environ.get("E2E_SSHD") or shutil.which("sshd") or "/usr/sbin/sshd"
        d = os.path.join(self.work, "ssh")
        os.makedirs(d)
        for name in ("host", "client"):
            subprocess.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", os.path.join(d, name)], check=True)
        shutil.copy(os.path.join(d, "client.pub"), os.path.join(d, "authorized_keys"))
        self.port = 22000 + os.getpid() % 1000
        cfg = os.path.join(d, "sshd_config")
        with open(cfg, "w") as f:
            f.write(f"Port {self.port}\nListenAddress 127.0.0.1\nHostKey {d}/host\n"
                    f"AuthorizedKeysFile {d}/authorized_keys\nPidFile {d}/pid\nUsePAM no\n"
                    "StrictModes no\nPasswordAuthentication no\nKbdInteractiveAuthentication no\n"
                    "PrintMotd no\nPrintLastLog no\n")
        env = dict(os.environ, LD_LIBRARY_PATH=self.libs)
        self.sshd = subprocess.Popen([sshd, "-f", cfg, "-D", "-e"], env=env,
                                     stdout=subprocess.DEVNULL, stderr=open(os.path.join(d, "log"), "w"))
        self.ssh_base = ["ssh", "-tt", "-p", str(self.port), "-i", os.path.join(d, "client"),
                         "-o", "StrictHostKeyChecking=no", "-o", "UserKnownHostsFile=/dev/null",
                         "-o", "LogLevel=ERROR", "127.0.0.1"]
        probe = ["ssh"] + self.ssh_base[2:] + ["true"]  # without -tt
        ok = wait(lambda: subprocess.run(probe, capture_output=True).returncode == 0, timeout=10, step=0.5)
        if not ok:
            sys.exit("could not start the private sshd; see " + os.path.join(d, "log"))

    def stop(self):
        if self.sshd:
            self.sshd.kill()

    def tmux_conf(self, mouse, extkeys):
        path = os.path.join(self.work, f"tmux-{mouse}-{extkeys}.conf")
        with open(path, "w") as f:
            f.write(f"set -g mouse {mouse}\nset -s extended-keys {extkeys}\n")
            if extkeys == "on":
                f.write("set -as terminal-features 'xterm*:extkeys'\n")
        return path


def build_command(env, case, paths):
    """The argv the "terminal" starts, and the shell line that runs peekme."""
    tmux_dir = os.path.dirname(os.path.abspath(env.tmux))
    inner = " ".join([
        "env", f"PATH={shlex.quote(tmux_dir)}:\"$PATH\"",
        *([f"LD_LIBRARY_PATH={shlex.quote(env.libs)}"] if env.libs else []),
        f"PEEKME_FAKE_EXPLAINER=1", f"PEEKME_EVENT_LOG={shlex.quote(paths['events'])}",
        f"FAKEAGENT_LOG={shlex.quote(paths['agent'])}", "TERM_PROGRAM=", "LC_TERMINAL=",
        # Never read this desktop's own selection.
        "DISPLAY=", "WAYLAND_DISPLAY=",
        shlex.quote(PEEKME), shlex.quote(os.path.join(env.bin, case["agent"])),
    ])
    lib = f"LD_LIBRARY_PATH={shlex.quote(env.libs)} " if env.libs else ""

    def tmux(cmd, sock):
        conf = env.tmux_conf(case["mouse"], case["extkeys"])
        return (f"{lib}{shlex.quote(env.tmux)} -L {sock} -f {shlex.quote(conf)} "
                f"new-session -x {COLS} -y {ROWS} {shlex.quote(cmd)}")

    def ssh(cmd):
        # sshd does not pass the locale on here; real servers have one. Without
        # a UTF-8 locale tmux draws every non-ASCII character as `_`.
        remote = "LANG=C.UTF-8 LC_ALL=C.UTF-8 " + cmd
        return " ".join(shlex.quote(a) for a in env.ssh_base) + " " + shlex.quote(remote)

    s1, s2 = f"e2e{case['n']}a", f"e2e{case['n']}b"
    build = {
        "direct": lambda: inner,
        "tmux": lambda: tmux(inner, s1),
        "ssh": lambda: ssh(inner),
        "ssh+tmux": lambda: ssh(tmux(inner, s2)),
        "tmux+ssh+tmux": lambda: tmux(ssh(tmux(inner, s2)), s1),
    }
    line = build[case["layers"]]()
    return ["/bin/sh", "-c", line], (s1, s2)


def run_case(env, case, out):
    name = f"{case['layers']}|mouse-{case['mouse']}|ext-{case['extkeys']}|{case['profile']}|{case['agent']}"
    safe = name.replace("|", "_").replace("+", "-")
    d = os.path.join(out, safe)
    os.makedirs(d, exist_ok=True)
    paths = {"events": os.path.join(d, "events.jsonl"), "agent": os.path.join(d, "agent.jsonl")}
    argv, socks = build_command(env, case, paths)
    term_env = dict(os.environ, TERM="xterm-256color")
    term_env.pop("TMUX", None)
    t = Term(argv, term_env, os.path.join(d, "terminal.bin"))
    fails = []
    hotkey = PROFILES[case["profile"]]

    def agent_log(kind=None):
        return [e for e in jsonl(paths["agent"]) if kind is None or e["kind"] == kind]

    def events(kind):
        return [e for e in jsonl(paths["events"]) if e["event"] == kind]

    def check(ok, msg):
        if not ok:
            fails.append(msg)
        return ok

    try:
        if not check(wait(lambda: any("alpha bravo" in l for l in t.screen()), 15), "agent screen never appeared"):
            return name, fails
        time.sleep(0.5)
        # 1. A drag reaches the agent, which reports the selection.
        t.send(DRAG)
        check(wait(lambda: any(e.get("text") == SELECTED for e in agent_log("selected"))),
              "drag did not reach the agent")
        time.sleep(0.4)
        before = t.screen()
        # 2. The shortcut opens a box on the dragged text.
        t.send(hotkey)
        opened = wait(lambda: events("open"))
        if check(opened, "shortcut did not open a box"):
            check(opened[-1].get("found") and opened[-1].get("selection") == SELECTED,
                  f"box opened on the wrong selection: {opened[-1].get('selection')!r}")
        check(wait(lambda: any("Test explanation" in l for l in t.screen())), "explanation not on screen")
        leaked = [e for e in agent_log("input") if hotkey.hex() in e["hex"]]
        check(not leaked, "the shortcut reached the agent")
        # 3. Typing with the box open goes to the agent.
        t.send(b"x")
        check(wait(lambda: any("x" in e["raw"] for e in agent_log("input"))), "typing did not reach the agent")
        check(not events("close"), "typing closed the box")
        # 4. Esc closes it; the text rows are as before.
        time.sleep(0.3)
        t.send(b"\x1b")
        check(wait(lambda: events("close")), "Esc did not close the box")
        time.sleep(1.0)
        after = t.screen()
        check(after[:ROWS // 2] == before[:ROWS // 2], "screen not restored after Esc")
        # 5. With nothing selected, π is typed (Mac terminal default only).
        if case["profile"] == "mac-default":
            t.send(CLICK)
            time.sleep(0.6)
            n = len(events("open"))
            t.send("π".encode())
            check(wait(lambda: any("π" in e["raw"] for e in agent_log("input"))),
                  "π with nothing selected was not typed")
            time.sleep(0.3)
            check(len(events("open")) == n, "π with nothing selected opened a box")
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


def cases(quick):
    layers = ["direct", "tmux", "ssh", "ssh+tmux", "tmux+ssh+tmux"]
    out = []
    for layer, profile, agent in itertools.product(layers, PROFILES, ["claude", "copilot", "codex"]):
        tmux_variants = [("off", "off")] if "tmux" not in layer else list(
            itertools.product(["off", "on"], ["off", "on"]))
        for mouse, ext in tmux_variants:
            out.append(dict(layers=layer, profile=profile, agent=agent, mouse=mouse, extkeys=ext))
    if quick:
        out = [c for c in out if c["agent"] == "claude" or c["layers"] == "ssh+tmux"]
    for i, c in enumerate(out):
        c["n"] = i
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--quick", action="store_true", help="Claude-style agent only, all agents for ssh+tmux")
    ap.add_argument("--only", default="", help="run cases whose name contains this")
    ap.add_argument("--jobs", type=int, default=6)
    ap.add_argument("--out", default=None, help="where to keep per-case artifacts")
    args = ap.parse_args()
    for p in (PEEKME, VTDUMP):
        if not os.path.exists(p):
            sys.exit(f"missing {p}: run `cargo build --release --bins --examples`")
    todo = cases(args.quick)
    if args.only:
        todo = [c for c in todo if args.only in
                f"{c['layers']}|mouse-{c['mouse']}|ext-{c['extkeys']}|{c['profile']}|{c['agent']}"]
    work = tempfile.mkdtemp(prefix="peekme-e2e-")
    out = args.out or os.path.join(work, "cases")
    env = Env(work, need_ssh=any("ssh" in c["layers"] for c in todo))
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
