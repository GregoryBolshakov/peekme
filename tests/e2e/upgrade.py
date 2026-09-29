#!/usr/bin/env python3
"""Upgrade path: a setup written by an older `peekme install` gets new agents,
and links left pointing at an older copy of peekme move to the new one.

    python3 tests/e2e/upgrade.py

A fake HOME holds what peekme 0.1.x wrote: a block with only the `codex`
function and only a `codex` link. Updating peekme (cargo, npm, Homebrew) never
runs it, so the first interactive start of the new binary must bring the block
and the links up to date, say so once, and leave everything else alone.

Second case: two copies of peekme (an old one from cargo, a new one from npm).
The old copy's `peekme install` left `codex` pointing at it, so `codex` kept
starting the old peekme. The first start of the new one moves the link.
"""
import os, re, shutil, subprocess, sys, tempfile

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import matrix as m  # noqa: E402

OLD_BLOCK = """# >>> peekme >>>
# Opens Codex with peekme when you type `codex`. Remove with: peekme uninstall
function codex {
  if [ -x "$HOME/.local/share/peekme/bin/codex" ]; then "$HOME/.local/share/peekme/bin/codex" "$@"; else command codex "$@"; fi
}
case ":$PATH:" in
  *":$HOME/.local/share/peekme/bin:"*) ;;
  *) export PATH="$HOME/.local/share/peekme/bin:$PATH" ;;
esac
# <<< peekme <<<
"""


def start(home):
    """One interactive start of peekme, as typing `codex` would do."""
    log = os.path.join(home, "terminal.bin")
    t = m.Term([m.PEEKME, "--", "/bin/sh", "-c", "sleep 1"],
               m.clean_env(HOME=home, SHELL="/bin/bash"), log)
    m.wait(lambda: not t.alive, 10)
    t.close()
    return re.findall(rb"peekme: [^\r\n]*", open(log, "rb").read())


def main():
    home = tempfile.mkdtemp(prefix="peekme-upgrade-")
    bindir = os.path.join(home, ".local/share/peekme/bin")
    os.makedirs(bindir)
    rc = os.path.join(home, ".bashrc")
    with open(rc, "w") as f:
        f.write("export A=1\n\n" + OLD_BLOCK + "alias ll='ls -l'\n")
    os.symlink(m.PEEKME, os.path.join(bindir, "codex"))
    fails = []
    said = start(home)
    links = sorted(os.listdir(bindir))
    text = open(rc).read()
    if links != ["claude", "codex", "copilot"]:
        fails.append(f"links after the first start: {links}")
    for agent in ("codex", "claude", "copilot"):
        if f"function {agent} {{" not in text:
            fails.append(f"no `{agent}` function in the block")
    if not (text.startswith("export A=1\n") and text.endswith("alias ll='ls -l'\n")):
        fails.append("lines outside the block changed")
    if len(said) != 1:
        fails.append(f"expected one note, got {said}")
    if start(home):
        fails.append("the second start changed something again")
    shutil.rmtree(home, ignore_errors=True)
    print("ok    upgrade from a 0.1 setup" if not fails else "FAIL  upgrade  <- " + "; ".join(fails))
    fails2 = two_copies()
    print("ok    link to an older copy moves" if not fails2 else "FAIL  two copies  <- " + "; ".join(fails2))
    sys.exit(1 if fails or fails2 else 0)


def two_copies():
    home = tempfile.mkdtemp(prefix="peekme-copies-")
    bindir = os.path.join(home, ".local/share/peekme/bin")
    os.makedirs(bindir)
    old = os.path.join(home, ".cargo/bin/peekme")
    os.makedirs(os.path.dirname(old))
    with open(old, "w") as f:
        f.write("#!/bin/sh\necho 'peekme 0.1.0'\n")
    os.chmod(old, 0o755)
    with open(os.path.join(home, ".bashrc"), "w") as f:
        f.write(OLD_BLOCK)
    os.symlink(old, os.path.join(bindir, "codex"))
    fails = []
    said = start(home)
    target = os.path.realpath(os.path.join(bindir, "codex"))
    if target != os.path.realpath(m.PEEKME):
        fails.append(f"`codex` still leads to {target}")
    if not any(b"older peekme (0.1.0)" in n for n in said):
        fails.append(f"no note about the old copy: {said}")
    if start(home):
        fails.append("the second start changed something again")
    shutil.rmtree(home, ignore_errors=True)
    return fails


if __name__ == "__main__":
    main()
