#!/usr/bin/env python3
"""GTK 4 client for tuna-drm-smoke's D-TABLET (P-IN-05, #344).

A fullscreen window whose GtkGestureStylus logs what a real toolkit sees of
the compositor's zwp_tablet_v2: one JSON object per line on stdout for each
stylus proximity, down, motion and up (position in the window, pressure,
distance, tilt, tool type) and each key press (the keybinding actions).
"""

import json
import sys

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
from gi.repository import Gdk, GLib, Gtk  # noqa: E402


def emit(record):
    print(json.dumps(record), flush=True)


def axis(gesture, use):
    found, value = gesture.get_axis(use)
    return value if found else None


def on_stylus(kind):
    def handler(gesture, x, y):
        tool = gesture.get_device_tool()
        emit(
            {
                "event": kind,
                "x": round(x, 2),
                "y": round(y, 2),
                "pressure": axis(gesture, Gdk.AxisUse.PRESSURE),
                "distance": axis(gesture, Gdk.AxisUse.DISTANCE),
                "xtilt": axis(gesture, Gdk.AxisUse.XTILT),
                "ytilt": axis(gesture, Gdk.AxisUse.YTILT),
                "tool": tool.get_tool_type().value_nick if tool else None,
                "serial": f"{tool.get_serial():x}" if tool else None,
            }
        )

    return handler


def on_key(_controller, keyval, keycode, state):
    emit(
        {
            "event": "key",
            "key": Gdk.keyval_name(keyval),
            "keycode": keycode,
            "shift": bool(state & Gdk.ModifierType.SHIFT_MASK),
            "control": bool(state & Gdk.ModifierType.CONTROL_MASK),
            "alt": bool(state & Gdk.ModifierType.ALT_MASK),
        }
    )
    return False


def activate(app):
    window = Gtk.ApplicationWindow(application=app, title="tuna-tablet-client")
    area = Gtk.DrawingArea(hexpand=True, vexpand=True)
    stylus = Gtk.GestureStylus()
    for kind in ("proximity", "down", "motion", "up"):
        stylus.connect(kind, on_stylus(kind))
    area.add_controller(stylus)
    keys = Gtk.EventControllerKey()
    keys.connect("key-pressed", on_key)
    window.add_controller(keys)
    window.set_child(area)

    def mapped(_widget):
        GLib.timeout_add(300, report)

    def report():
        emit({"event": "ready", "width": area.get_width(), "height": area.get_height()})
        return False

    window.connect("map", mapped)
    window.fullscreen()
    window.present()


def main():
    app = Gtk.Application(application_id="org.tuna.TabletProof")
    app.connect("activate", activate)
    return app.run([sys.argv[0]])


if __name__ == "__main__":
    sys.exit(main())
