#!/usr/bin/env python3
"""A terminal window for end-to-end runs: one GTK3 VTE widget (the widget of
GNOME Terminal) running a command.

    python3 vtehost.py OUT_DIR COMMAND [ARGS...]

Every 200 ms it writes OUT_DIR/geometry.json (cell size and where the grid is
on the X screen) and OUT_DIR/screen.txt (the text VTE shows). The window has
no decorations under Xvfb, so the grid starts at the window's origin.
"""
import json
import os
import sys

import gi

gi.require_version("Gtk", "3.0")
gi.require_version("Vte", "2.91")
from gi.repository import GLib, Gtk, Vte  # noqa: E402

out, argv = sys.argv[1], sys.argv[2:]
win = Gtk.Window(title="vtehost")
term = Vte.Terminal()
term.set_size(80, 24)
win.add(term)
win.connect("destroy", Gtk.main_quit)
# VTE adds these to the parent environment (it does not replace it).
term.spawn_sync(Vte.PtyFlags.DEFAULT, os.getcwd(), argv, ["TERM=xterm-256color"],
                GLib.SpawnFlags.DEFAULT, None, None)
term.connect("child-exited", lambda *_: Gtk.main_quit())


def dump():
    _, x, y = term.get_window().get_origin()
    geo = dict(cw=term.get_char_width(), ch=term.get_char_height(), x=x, y=y,
               cols=term.get_column_count(), rows=term.get_row_count())
    tmp = os.path.join(out, "geometry.json.tmp")
    with open(tmp, "w") as f:
        json.dump(geo, f)
    os.replace(tmp, os.path.join(out, "geometry.json"))
    # The visible rows (VTE 0.68's Python binding returns text and attributes).
    text = term.get_text()
    text = text[0] if isinstance(text, tuple) else text
    tmp = os.path.join(out, "screen.txt.tmp")
    with open(tmp, "w") as f:
        f.write(text or "")
    os.replace(tmp, os.path.join(out, "screen.txt"))
    return True


win.show_all()
GLib.timeout_add(200, dump)
Gtk.main()
