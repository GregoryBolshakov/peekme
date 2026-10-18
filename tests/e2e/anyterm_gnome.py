#!/usr/bin/env python3
"""peekme around a shell on GNOME Wayland: the helper extension is offered on
the first Alt+P, installed, and from the next login the box opens under the
copy the user selected.

    python3 tests/e2e/anyterm_gnome.py

Runs a private headless GNOME Shell (`--headless --wayland`) with its own
D-Bus, runtime folder and HOME, never the desktop's. A test-only extension
turns on the shell's unsafe mode so the test can move the pointer and type
through Clutter virtual devices (org.gnome.Shell.Eval). The terminal is a GTK3
VTE window (vtehost.py) running `peekme -- bash`; the explainer is canned.

The install asks extensions.gnome.org first; the helper is not published
there yet, so it falls back to a local copy (no network: the same).

Needs: gnome-shell, busctl, gsettings, python3-gi with Vte 2.91, a release
build. Skips (exit 0) without gnome-shell.
"""
import json, os, re, shutil, subprocess, sys, tempfile, time

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
PEEKME = os.path.join(ROOT, "target/release/peekme")
UUID = "peekme-helper@peekme.dev"
PROBE = "peekme-test-probe@local"
WAYLAND = "wl-peekme-test"

PROBE_JS = """function init() {}
function enable() { global.context.unsafe_mode = true; }
function disable() {}
"""


def shell_version():
    out = subprocess.run(["gnome-shell", "--version"], capture_output=True, text=True).stdout
    return out.split()[-1].split(".")[0] if out else None


def eval_js(js):
    out = subprocess.run(
        ["gdbus", "call", "--session", "--dest", "org.gnome.Shell", "--object-path",
         "/org/gnome/Shell", "--method", "org.gnome.Shell.Eval",
         "const {Clutter, GLib} = imports.gi; const T = () => GLib.get_monotonic_time(); " + js],
        capture_output=True, text=True).stdout
    m = re.match(r"\((true|false), '(.*)'\)", out.strip())
    if not m or m[1] != "true":
        raise RuntimeError(f"eval failed: {out.strip()} for {js[:60]}")
    return json.loads(m[2]) if m[2] else None


def wait(pred, timeout=5.0):
    end = time.time() + timeout
    while time.time() < end:
        v = pred()
        if v:
            return v
        time.sleep(0.1)
    return pred()


class Shell:
    def __init__(self, work):
        self.work, self.proc = work, None

    def start(self):
        log = open(os.path.join(self.work, "shell.log"), "a")
        self.proc = subprocess.Popen(
            ["gnome-shell", "--headless", "--wayland", "--no-x11", "--virtual-monitor", "1280x800",
             "--wayland-display", WAYLAND], stdout=log, stderr=log)
        sock = os.path.join(os.environ["XDG_RUNTIME_DIR"], WAYLAND)
        if not wait(lambda: os.path.exists(sock), 15):
            raise RuntimeError("gnome-shell did not start")
        wait(lambda: self.eval_ok(), 15)
        eval_js("const s = Clutter.get_default_backend().get_default_seat(); "
                "global._vp = s.create_virtual_device(Clutter.InputDeviceType.POINTER_DEVICE); "
                "global._vk = s.create_virtual_device(Clutter.InputDeviceType.KEYBOARD_DEVICE); "
                "Main.overview.hide(); 1")

    @staticmethod
    def eval_ok():
        try:
            return eval_js("1") == 1
        except RuntimeError:
            return False

    def stop(self):
        if self.proc:
            self.proc.terminate()
            try:
                self.proc.wait(10)
            except subprocess.TimeoutExpired:
                self.proc.kill()
                self.proc.wait()

    # Input, through the shell's virtual devices.
    def move(self, x, y):
        eval_js(f"global._vp.notify_absolute_motion(T(), {x}, {y}); 1")

    def button(self, down):
        state = "PRESSED" if down else "RELEASED"
        eval_js(f"global._vp.notify_button(T(), Clutter.BUTTON_PRIMARY, Clutter.ButtonState.{state}); 1")

    def key(self, keyval, alt=False):
        js = ""
        if alt:
            js += "global._vk.notify_keyval(T(), Clutter.KEY_Alt_L, Clutter.KeyState.PRESSED); "
        js += (f"global._vk.notify_keyval(T(), {keyval}, Clutter.KeyState.PRESSED); "
               f"global._vk.notify_keyval(T(), {keyval}, Clutter.KeyState.RELEASED); ")
        if alt:
            js += "global._vk.notify_keyval(T(), Clutter.KEY_Alt_L, Clutter.KeyState.RELEASED); "
        eval_js(js + "1")


class Window:
    """A VTE window running `peekme -- bash` in the private shell."""

    def __init__(self, shell, work, name):
        self.shell, self.dir = shell, os.path.join(work, name)
        os.makedirs(self.dir)
        start = os.path.join(self.dir, "start.sh")
        with open(start, "w") as f:
            f.write("for i in 1 2 3 4; do echo \"build $i: ok\"; echo 'warning: unused variable x'; done\n"
                    "exec bash --norc --noprofile -i\n")
        env = {k: v for k, v in os.environ.items() if k not in ("DISPLAY", "PEEKME_ACTIVE")}
        env.update(GDK_BACKEND="wayland", WAYLAND_DISPLAY=WAYLAND, XDG_SESSION_TYPE="wayland",
                   XDG_CURRENT_DESKTOP="ubuntu:GNOME", PEEKME_FAKE_EXPLAINER="1", NO_AT_BRIDGE="1",
                   PEEKME_EVENT_LOG=os.path.join(self.dir, "events.jsonl"),
                   XDG_STATE_HOME=os.path.join(self.dir, "state"))
        self.proc = subprocess.Popen(
            [sys.executable, os.path.join(HERE, "vtehost.py"), self.dir, PEEKME, "--",
             "/bin/bash", start], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        wait(lambda: "build 4: ok" in self.screen(), 10)
        frame = eval_js("const w = global.get_window_actors().map(a => a.meta_window)"
                        ".find(w => w.get_title() == 'vtehost'); w.activate(global.get_current_time()); "
                        "const f = w.get_frame_rect(); JSON.stringify([f.x, f.y, f.width, f.height])")
        frame = json.loads(frame) if isinstance(frame, str) else frame
        self.geo = json.load(open(os.path.join(self.dir, "geometry.json")))
        g = self.geo
        # GTK draws the title bar on top; the grid (with 1 px padding) ends at the bottom.
        self.x0 = frame[0] + 1
        self.y0 = frame[1] + frame[3] - g["rows"] * g["ch"] - 1
        time.sleep(0.5)

    def screen(self):
        try:
            return open(os.path.join(self.dir, "screen.txt")).read()
        except FileNotFoundError:
            return ""

    def rows(self):
        return self.screen().split("\n")

    def drag(self, row, c0, c1):
        g = self.geo
        y = self.y0 + row * g["ch"] + g["ch"] // 2
        x0, x1 = self.x0 + c0 * g["cw"] + 1, self.x0 + (c1 + 1) * g["cw"] - 2
        self.shell.move(x0, y)
        self.shell.button(True)
        for i in range(1, 6):
            self.shell.move(x0 + (x1 - x0) * i // 5, y)
        self.shell.button(False)
        time.sleep(0.3)

    def box_row(self):
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


KEY_P, KEY_Q, KEY_X, KEY_RETURN, KEY_ESCAPE = 112, 113, 120, 65293, 65307


def inner(work):
    home = os.environ["HOME"]
    ext = os.path.join(home, ".local/share/gnome-shell/extensions")
    os.makedirs(os.path.join(ext, PROBE))
    with open(os.path.join(ext, PROBE, "metadata.json"), "w") as f:
        json.dump({"uuid": PROBE, "name": "test probe", "description": "test only",
                   "shell-version": [shell_version()]}, f)
    with open(os.path.join(ext, PROBE, "extension.js"), "w") as f:
        f.write(PROBE_JS)
    subprocess.run(["gsettings", "set", "org.gnome.shell", "enabled-extensions", f"['{PROBE}']"])
    subprocess.run(["gsettings", "set", "org.gnome.shell", "disable-user-extensions", "false"])
    shell = Shell(work)
    fails = []

    def case(name, fn):
        w = Window(shell, work, name.replace(" ", "-"))
        try:
            problem = fn(w)
        except Exception as e:
            problem = f"error: {e!r}"
        finally:
            w.close()
        print(("FAIL " if problem else "ok   ") + name + (f": {problem}" if problem else ""))
        if problem:
            fails.append(name)

    def offer_and_install(w):
        w.drag(3, 9, 23)
        shell.key(KEY_P, alt=True)
        if not wait(lambda: "GNOME Shell extension" in w.screen()):
            return f"no offer:\n{w.screen()[:400]}"
        shell.key(KEY_RETURN)
        if not wait(lambda: "next login" in w.screen(), 20):
            return f"no after-login message: {w.events('helper_installed')}"
        if not os.path.exists(os.path.join(ext, UUID, "extension.js")):
            return "no local copy"
        listed = subprocess.run(["gsettings", "get", "org.gnome.shell", "enabled-extensions"],
                                capture_output=True, text=True).stdout
        return None if UUID in listed else f"not enabled for the next login: {listed}"

    def second_copy(w):
        w.drag(3, 9, 23)
        shell.key(KEY_P, alt=True)
        top = wait(w.box_row)
        nudge = w.events("nudge")
        if top != 4:
            return f"box top on row {top}, want 4; nudge {nudge}"
        return None if nudge and nudge[0].get("cell") else f"no nudge report: {nudge}"

    def ask_third_copy(w):
        w.drag(5, 9, 23)
        shell.key(KEY_Q, alt=True)
        top = wait(w.box_row)
        return None if top == 6 else f"ask box on row {top}, want 6"

    def typed_after(w):
        w.drag(3, 9, 23)
        shell.key(KEY_X)
        shell.key(KEY_P, alt=True)
        time.sleep(1.2)
        return "a box for a selection older than the last key" if w.box_row() is not None else None

    try:
        shell.start()
        case("offer and install", offer_and_install)
        shell.stop()
        shell.start()  # a new login: GNOME loads the helper
        state = eval_js(f"Main.extensionManager.lookup('{UUID}')?.state ?? -1")
        print(f"helper state after the new login: {state}")
        for name, fn in [("second copy", second_copy), ("ask on the third copy", ask_third_copy),
                         ("key typed after the selection", typed_after)]:
            case(name, fn)
    finally:
        shell.stop()
    print(f"\n{4 - len(fails)}/4 passed")
    return 1 if fails else 0


def main():
    if "--inner" in sys.argv:
        return inner(sys.argv[sys.argv.index("--inner") + 1])
    for tool in ("gnome-shell", "busctl", "gsettings", "dbus-run-session"):
        if not shutil.which(tool):
            print(f"skip: {tool} not found")
            return 0
    if not os.path.exists(PEEKME):
        sys.exit("build first: cargo build --release")
    work = tempfile.mkdtemp(prefix="peekme-gnome-")
    # Wayland socket paths must stay under 108 bytes: a short runtime folder.
    runtime = tempfile.mkdtemp(prefix="pmrt-", dir="/tmp")
    home = os.path.join(work, "home")
    os.makedirs(home)
    env = {k: v for k, v in os.environ.items()
           if k not in ("DISPLAY", "WAYLAND_DISPLAY", "DBUS_SESSION_BUS_ADDRESS", "PEEKME_ACTIVE")}
    env.update(HOME=home, XDG_RUNTIME_DIR=runtime, XDG_CONFIG_HOME=os.path.join(home, ".config"),
               XDG_DATA_HOME=os.path.join(home, ".local/share"),
               XDG_CACHE_HOME=os.path.join(home, ".cache"), XDG_SESSION_TYPE="wayland")
    # Services the private bus starts (portals, Nautilus...) are noisy: their
    # output goes to a log, the test's own lines to the terminal.
    with open(os.path.join(work, "session.log"), "w") as log:
        code = subprocess.run(["dbus-run-session", "--", sys.executable, os.path.abspath(__file__),
                               "--inner", work], env=env, stderr=log).returncode
    shutil.rmtree(runtime, ignore_errors=True)
    if code == 0:
        shutil.rmtree(work, ignore_errors=True)
    else:
        print(f"logs in {work}")
    return code


if __name__ == "__main__":
    sys.exit(main())
