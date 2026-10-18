// peekme helper for GNOME Shell 45 and later. The same as
// ../legacy/extension.js as an ES module; keep the two in step.
//
// On Wayland a terminal's selection and the pointer belong to the compositor.
// peekme (a program in the terminal) asks this helper over D-Bus for the
// selected text, when it last changed, and to move the pointer a little and
// back so the terminal reports which cell the pointer is on.

import Clutter from 'gi://Clutter';
import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import St from 'gi://St';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const IFACE = `<node>
  <interface name="dev.peekme.Helper1">
    <method name="Version"><arg type="s" direction="out"/></method>
    <method name="Primary"><arg type="s" direction="out"/></method>
    <method name="SelectionChanged"><arg type="x" direction="out"/></method>
    <method name="Nudge"><arg type="i" direction="in"/></method>
  </interface>
</node>`;

const MAX_STEP = 32;
const HOLD_MS = 20;
const NOT_FOCUSED = 'dev.peekme.Helper1.Error.NotFocused';

// The parent of process `pid`, from /proc/<pid>/stat; 0 when unknown.
function parentOf(pid) {
    try {
        const [, bytes] = GLib.file_get_contents(`/proc/${pid}/stat`);
        const stat = new TextDecoder().decode(bytes);
        // "pid (name) state ppid ...": the name may hold spaces and parens.
        return parseInt(stat.slice(stat.lastIndexOf(')') + 2).split(' ')[1]) || 0;
    } catch {
        return 0;
    }
}

// Whether process `pid` runs under the focused window's program, like a
// shell and peekme under the terminal the user is typing in.
function inFocusedWindow(pid) {
    const owner = global.display.focus_window?.get_pid() ?? 0;
    for (let p = pid, hops = 0; owner > 1 && p > 1 && hops < 64; p = parentOf(p), hops++) {
        if (p === owner)
            return true;
    }
    return false;
}

class Helper {
    constructor() {
        this._changed = 0;
        this._pointer = null;
        this._selection = global.display.get_selection().connect('owner-changed', (_s, type) => {
            if (type === Meta.SelectionType.SELECTION_PRIMARY)
                this._changed = GLib.get_monotonic_time();
        });
        this._dbus = Gio.DBusExportedObject.wrapJSObject(IFACE, this);
        this._dbus.export(Gio.DBus.session, '/dev/peekme/Helper');
        this._name = Gio.bus_own_name(Gio.BusType.SESSION, 'dev.peekme.Helper',
            Gio.BusNameOwnerFlags.NONE, null, null, null);
    }

    destroy() {
        global.display.get_selection().disconnect(this._selection);
        this._dbus.unexport();
        Gio.bus_unown_name(this._name);
        this._pointer = null;
    }

    Version() {
        return '1';
    }

    // Run `answer` only for a caller inside the focused window: the selection
    // is not handed to any other program on the session bus.
    _ifFocused(invocation, answer) {
        Gio.DBus.session.call('org.freedesktop.DBus', '/org/freedesktop/DBus',
            'org.freedesktop.DBus', 'GetConnectionUnixProcessID',
            new GLib.Variant('(s)', [invocation.get_sender()]), new GLib.VariantType('(u)'),
            Gio.DBusCallFlags.NONE, -1, null, (bus, res) => {
                let pid = 0;
                try {
                    [pid] = bus.call_finish(res).deepUnpack();
                } catch {
                    pid = 0;
                }
                if (inFocusedWindow(pid))
                    answer();
                else
                    invocation.return_dbus_error(NOT_FOCUSED, 'Only a program in the focused window may ask');
            });
    }

    PrimaryAsync(_params, invocation) {
        this._ifFocused(invocation, () => {
            St.Clipboard.get_default().get_text(St.ClipboardType.PRIMARY, (_c, text) => {
                invocation.return_value(new GLib.Variant('(s)', [text ?? '']));
            });
        });
    }

    // Monotonic microseconds of the last new PRIMARY selection, 0 for none.
    SelectionChangedAsync(_params, invocation) {
        this._ifFocused(invocation, () => {
            invocation.return_value(new GLib.Variant('(x)', [this._changed]));
        });
    }

    // Move the pointer `step` pixels sideways and back, by at most MAX_STEP.
    NudgeAsync([step], invocation) {
        this._ifFocused(invocation, () => {
            invocation.return_value(null);
            this._nudge(step);
        });
    }

    _nudge(step) {
        if (step < 1 || step > MAX_STEP)
            return;
        const [x, y] = global.get_pointer();
        const to = x + step < global.stage.width ? x + step : x - step;
        if (!this._pointer) {
            // global.stage.context is there from GNOME 47; GNOME 51 has
            // no Clutter.get_default_backend().
            const backend = global.stage.context?.get_backend() ?? Clutter.get_default_backend();
            const seat = backend.get_default_seat();
            this._pointer = seat.create_virtual_device(Clutter.InputDeviceType.POINTER_DEVICE);
        }
        this._pointer.notify_absolute_motion(GLib.get_monotonic_time(), to, y);
        GLib.timeout_add(GLib.PRIORITY_DEFAULT, HOLD_MS, () => {
            this._pointer?.notify_absolute_motion(GLib.get_monotonic_time(), x, y);
            return GLib.SOURCE_REMOVE;
        });
    }
}

export default class PeekmeHelper extends Extension {
    enable() {
        this._helper = new Helper();
    }

    disable() {
        this._helper?.destroy();
        this._helper = null;
    }
}
