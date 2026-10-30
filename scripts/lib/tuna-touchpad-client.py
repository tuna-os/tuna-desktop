#!/usr/bin/env python3
"""GTK 4 client for tuna-drm-smoke's D-TOUCHPAD-SWIPE (P-IN-02, #593).

A fullscreen window logging what a real toolkit receives from the
compositor while a touchpad is used: one JSON object per line on stdout
for each scroll (GtkEventControllerScroll: deltas, unit, source), each
touchpad gesture event that reaches the client through pointer-gestures
(GDK's TOUCHPAD_SWIPE/PINCH/HOLD with phase and finger count), and each
pointer motion the same event controller sees (its liveness).
"""

import json
import sys

import gi

gi.require_version("Gtk", "4.0")
gi.require_version("Gdk", "4.0")
from gi.repository import Gdk, GLib, Gtk  # noqa: E402

GESTURES = {
    Gdk.EventType.TOUCHPAD_SWIPE: "touchpad-swipe",
    Gdk.EventType.TOUCHPAD_PINCH: "touchpad-pinch",
    Gdk.EventType.TOUCHPAD_HOLD: "touchpad-hold",
}


def emit(record):
    print(json.dumps(record), flush=True)


def on_scroll(controller, dx, dy):
    event = controller.get_current_event()
    device = event.get_device() if event else None
    emit(
        {
            "event": "scroll",
            "dx": round(dx, 3),
            "dy": round(dy, 3),
            "unit": controller.get_unit().value_nick,
            "source": device.get_source().value_nick if device else None,
        }
    )
    return True


def on_event(controller, _event):
    # PyGObject hands the GdkEvent over as None; the controller has it.
    event = controller.get_current_event()
    if event is None:
        return False
    kind = event.get_event_type()
    if kind in GESTURES:
        emit(
            {
                "event": GESTURES[kind],
                "phase": event.get_touchpad_gesture_phase().value_nick,
                "fingers": event.get_touchpad_n_fingers(),
            }
        )
    elif kind == Gdk.EventType.MOTION_NOTIFY:
        found, x, y = event.get_position()
        emit({"event": "motion", "x": round(x, 1), "y": round(y, 1)} if found else {"event": "motion"})
    return False


def activate(app):
    window = Gtk.ApplicationWindow(application=app, title="tuna-touchpad-client")
    area = Gtk.DrawingArea(hexpand=True, vexpand=True)
    scroll = Gtk.EventControllerScroll(flags=Gtk.EventControllerScrollFlags.BOTH_AXES)
    scroll.connect("scroll", on_scroll)
    area.add_controller(scroll)
    legacy = Gtk.EventControllerLegacy()
    legacy.connect("event", on_event)
    window.add_controller(legacy)
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
    app = Gtk.Application(application_id="org.tuna.TouchpadProof")
    app.connect("activate", activate)
    return app.run([sys.argv[0]])


if __name__ == "__main__":
    sys.exit(main())
