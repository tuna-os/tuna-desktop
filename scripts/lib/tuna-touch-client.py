#!/usr/bin/env python3
"""GTK 4 client for tuna-drm-smoke's D-TOUCH (P-DS-04, #345).

A fullscreen window whose GtkGestureClick logs what a real toolkit sees of
a touchscreen through the compositor: one JSON object per line on stdout
for each press and release (position in the window), "ready" with the
window's size once mapped, and "configure" with the new size whenever it
changes afterwards (a rotated output resizes the fullscreen window).
"""

import json
import sys

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import GLib, Gtk  # noqa: E402


def emit(record):
    print(json.dumps(record), flush=True)


def on_click(kind):
    def handler(gesture, n_press, x, y):
        emit({"event": kind, "x": round(x, 2), "y": round(y, 2), "n_press": n_press,
              "button": gesture.get_current_button()})

    return handler


def activate(app):
    window = Gtk.ApplicationWindow(application=app, title="tuna-touch-client")
    area = Gtk.DrawingArea(hexpand=True, vexpand=True)
    click = Gtk.GestureClick(button=0)
    click.connect("pressed", on_click("pressed"))
    click.connect("released", on_click("released"))
    area.add_controller(click)
    window.set_child(area)
    size = {"reported": None}

    def resized(_area, width, height):
        # Size changes after "ready" only; the first allocations are
        # what "ready" reports.
        if size["reported"] is not None and size["reported"] != (width, height):
            size["reported"] = (width, height)
            emit({"event": "configure", "width": width, "height": height})

    def mapped(_widget):
        GLib.timeout_add(300, report)

    def report():
        size["reported"] = (area.get_width(), area.get_height())
        emit({"event": "ready", "width": area.get_width(), "height": area.get_height()})
        return False

    area.connect("resize", resized)
    window.connect("map", mapped)
    window.fullscreen()
    window.present()


def main():
    app = Gtk.Application(application_id="org.tuna.TouchProof")
    app.connect("activate", activate)
    return app.run([sys.argv[0]])


if __name__ == "__main__":
    sys.exit(main())
