#!/usr/bin/env python3
"""peekme around a shell in a real terminal widget on X11: the box must open
under the copy the user selected, and Alt+P must stay the shell's when
nothing new is selected.

    python3 tests/e2e/anyterm_x11.py

A private Xvfb runs a GTK3 VTE window (GNOME Terminal's widget, see
vtehost.py) with `peekme -- bash`. Mouse and keys go in through XTEST, like a
person's; the selection is the terminal's own (X11 PRIMARY). The explainer is
canned (PEEKME_FAKE_EXPLAINER), so no model is called.

Needs: a release build, Xvfb, python3-gi with Vte 2.91, python-xlib. Skips
(exit 0) when one is missing.
"""
import json, os, shutil, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
PEEKME = os.path.join(ROOT, "target/release/peekme")

try:
    import gi  # noqa: F401
    from Xlib import X, XK, display
    from Xlib.ext import xtest
except ImportError as e:
    print(f"skip: {e}")
    sys.exit(0)

# Four copies of one warning: rows 1, 3, 5, 7.
LOG = "".join(f"echo 'build {i}: ok'; echo 'warning: unused variable x'; " for i in range(1, 5))


class Window:
    """One VTE window running `peekme -- bash` on display `disp`."""

    def __init__(self, disp, work, name):
        self.dir = os.path.join(work, name)
        os.makedirs(self.dir)
        env = {k: v for k, v in os.environ.items()
               if k not in ("PEEKME_ACTIVE", "TMUX", "TMUX_PANE", "SSH_CONNECTION")}
        env.update(DISPLAY=disp, PEEKME_FAKE_EXPLAINER="1", NO_AT_BRIDGE="1",
                   XDG_STATE_HOME=os.path.join(self.dir, "state"),
                   PEEKME_EVENT_LOG=os.path.join(self.dir, "events.jsonl"),
                   PS1="$ ", HOME=self.dir)
        rc = os.path.join(self.dir, "start.sh")
        with open(rc, "w") as f:
            f.write(f"{LOG}\nexec bash --norc --noprofile -i\n")
        self.proc = subprocess.Popen(
            [sys.executable, os.path.join(HERE, "vtehost.py"), self.dir,
             PEEKME, "--", "/bin/bash", rc],
            env=env, stderr=subprocess.DEVNULL)
        self.d = display.Display(disp)
        wait(lambda: "build 4: ok" in self.screen(), 10)
        self.geo = json.load(open(os.path.join(self.dir, "geometry.json")))

    def screen(self):
        try:
            return open(os.path.join(self.dir, "screen.txt")).read()
        except FileNotFoundError:
            return ""

    def rows(self):
        return self.screen().split("\n")

    def px(self, row, col):
        g = self.geo
        return g["x"] + col * g["cw"] + g["cw"] // 2, g["y"] + row * g["ch"] + g["ch"] // 2

    def move(self, x, y):
        xtest.fake_input(self.d, X.MotionNotify, x=x, y=y)
        self.d.sync()

    def drag(self, row, c0, c1):
        """Select cells c0..c1 of `row` with the left button."""
        x0, y = self.px(row, c0)
        x1, _ = self.px(row, c1)
        x0 -= self.geo["cw"] // 2 - 1
        x1 += self.geo["cw"] // 2 - 1
        self.move(x0, y)
        time.sleep(0.05)
        xtest.fake_input(self.d, X.ButtonPress, 1)
        self.d.sync()
        for i in range(1, 6):
            time.sleep(0.03)
            self.move(x0 + (x1 - x0) * i // 5, y)
        xtest.fake_input(self.d, X.ButtonRelease, 1)
        self.d.sync()
        time.sleep(0.2)

    def key(self, sym, alt=False):
        kc = self.d.keysym_to_keycode(XK.string_to_keysym(sym))
        alt_kc = self.d.keysym_to_keycode(XK.XK_Alt_L)
        seq = ([(alt_kc, X.KeyPress)] if alt else []) + [(kc, X.KeyPress), (kc, X.KeyRelease)]
        seq += [(alt_kc, X.KeyRelease)] if alt else []
        for code, ev in seq:
            xtest.fake_input(self.d, ev, code)
            self.d.sync()
            time.sleep(0.02)

    def box_row(self):
        """Row of the box's top border, or None."""
        for i, r in enumerate(self.rows()):
            if r.startswith("╭─"):
                return i
        return None

    def events(self, kind):
        path = os.path.join(self.dir, "events.jsonl")
        if not os.path.exists(path):
            return []
        return [e for e in map(json.loads, open(path)) if e.get("event") == kind]

    def close(self):
        self.proc.kill()
        self.proc.wait()
        self.d.close()


def wait(pred, timeout=5.0):
    end = time.time() + timeout
    while time.time() < end:
        v = pred()
        if v:
            return v
        time.sleep(0.1)
    return pred()


def main():
    for tool in ("Xvfb",):
        if not shutil.which(tool):
            print(f"skip: {tool} not found")
            return 0
    if not os.path.exists(PEEKME):
        sys.exit("build first: cargo build --release")
    try:
        gi.require_version("Vte", "2.91")
    except ValueError as e:
        print(f"skip: {e}")
        return 0
    work = tempfile.mkdtemp(prefix="peekme-anyterm-")
    disp = ":%d" % (90 + os.getpid() % 9)
    xvfb = subprocess.Popen(["Xvfb", disp, "-screen", "0", "1280x800x24", "-nolisten", "tcp"],
                            stderr=subprocess.DEVNULL)
    time.sleep(1)
    fails = []

    def case(name, fn):
        w = Window(disp, work, name)
        try:
            problem = fn(w)
        except Exception as e:  # a broken case must not hide the others
            problem = f"error: {e!r}"
        finally:
            w.close()
        print(("FAIL " if problem else "ok   ") + name + (f": {problem}" if problem else ""))
        if problem:
            fails.append(name)

    def second_copy(w):
        w.drag(3, 9, 23)  # "unused variable" on row 3, the second of four
        w.key("p", alt=True)
        top = wait(w.box_row)
        nudge = w.events("nudge")
        if top != 4:
            return f"box top border on row {top}, want 4 (under the second copy); nudge {nudge}"
        if not nudge or nudge[0].get("cell") is None:
            return f"no nudge report: {nudge}"
        return None

    def ask_on_second_copy(w):
        w.drag(5, 9, 23)  # the third copy
        w.key("q", alt=True)
        top = wait(w.box_row)
        return None if top == 6 else f"ask box on row {top}, want 6"

    def one_copy(w):
        w.drag(4, 0, 6)  # "build 3"
        w.key("p", alt=True)
        top = wait(w.box_row)
        if top != 5:
            return f"box on row {top}, want 5"
        return "a nudge for text on screen once" if w.events("nudge_start") else None

    def nothing_selected(w):
        w.key("p", alt=True)
        time.sleep(1)
        if w.box_row() is not None:
            return "a box with nothing selected"
        return None if w.events("hotkey_to_child") else "Alt+P did not go to bash"

    def typed_after_selecting(w):
        w.drag(3, 9, 23)
        w.key("x")
        w.key("p", alt=True)
        time.sleep(1)
        if w.box_row() is not None:
            return "a box for a selection older than the last key"
        return None

    def esc_restores(w):
        before = w.rows()
        w.drag(1, 9, 23)
        w.key("p", alt=True)
        if wait(w.box_row) != 2:
            return f"box on row {w.box_row()}, want 2"
        w.key("Escape")
        wait(lambda: w.box_row() is None)
        after = w.rows()
        diff = [(i, b, a) for i, (b, a) in enumerate(zip(before, after)) if b.rstrip() != a.rstrip()]
        return f"rows differ after Esc: {diff}" if diff else None

    for name, fn in [("second copy", second_copy), ("ask on the third copy", ask_on_second_copy),
                     ("text on screen once", one_copy), ("nothing selected", nothing_selected),
                     ("key typed after the selection", typed_after_selecting),
                     ("Esc restores the screen", esc_restores)]:
        case(name, fn)
    xvfb.terminate()  # lets Xvfb remove its socket and lock
    xvfb.wait()
    print(f"\n{6 - len(fails)}/6 passed")
    if not fails:
        shutil.rmtree(work, ignore_errors=True)
    else:
        print(f"logs in {work}")
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
