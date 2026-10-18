// peekme helper for GNOME Shell 42-44. The same as ../esm/extension.js in the
// older module style; keep the two in step.
//
// On Wayland a terminal's selection and the pointer belong to the compositor.
// peekme (a program in the terminal) asks this helper over D-Bus for the
// selected text, when it last changed, and to move the pointer a little and
// back so the terminal reports which cell the pointer is on.

const {Clutter, Gio, GLib, Meta, St} = imports.gi;

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

    PrimaryAsync(_params, invocation) {
        St.Clipboard.get_default().get_text(St.ClipboardType.PRIMARY, (_c, text) => {
            invocation.return_value(new GLib.Variant('(s)', [text ?? '']));
        });
    }

    // Monotonic microseconds of the last new PRIMARY selection, 0 for none.
    SelectionChanged() {
        return this._changed;
    }

    // Move the pointer `step` pixels sideways and back, only while a window
    // has the focus and by at most MAX_STEP pixels.
    Nudge(step) {
        if (!global.display.focus_window || step < 1 || step > MAX_STEP)
            return;
        const [x, y] = global.get_pointer();
        const to = x + step < global.stage.width ? x + step : x - step;
        if (!this._pointer) {
            const seat = Clutter.get_default_backend().get_default_seat();
            this._pointer = seat.create_virtual_device(Clutter.InputDeviceType.POINTER_DEVICE);
        }
        this._pointer.notify_absolute_motion(GLib.get_monotonic_time(), to, y);
        GLib.timeout_add(GLib.PRIORITY_DEFAULT, HOLD_MS, () => {
            this._pointer?.notify_absolute_motion(GLib.get_monotonic_time(), x, y);
            return GLib.SOURCE_REMOVE;
        });
    }
}

let helper = null;

function init() {}

function enable() {
    helper = new Helper();
}

function disable() {
    helper?.destroy();
    helper = null;
}
