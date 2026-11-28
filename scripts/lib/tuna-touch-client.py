#!/usr/bin/env python3
"""GTK 4 client for tuna-drm-smoke's D-TOUCH (P-DS-04, #345).

A fullscreen window whose drawing area logs what a real toolkit sees of a
touchscreen through the compositor, one JSON object per line on stdout:

  {"event": "ready", "width", "height"}       once mapped
  {"event": "configure", "width", "height"}   the size changed afterwards
                                              (a rotated output resizes the
                                              fullscreen window)
  {"event": "pressed"|"released", "x", "y", "n_press", "button"}
                                              GtkGestureClick
  {"event": "raw", "type", "seq", "source", "x", "y"}
                                              every event reaching the area
                                              (GtkEventControllerLegacy,
                                              capture phase, never handled):
                                              type is the GdkEventType nick
                                              (touch-begin, touch-update,
                                              touch-end, touch-cancel,
                                              button-press, motion-notify,
                                              ...), source the device's
                                              GdkInputSource nick, x/y area
                                              coordinates or null
  {"event": "drag-begin"|"drag-update"|"drag-end", "seq", "x", "y",
   "start_x", "start_y"}                      GtkGestureDrag (touch only);
                                              x/y the absolute point
  {"event": "zoom-begin"|"zoom-scale"|"zoom-end"|"zoom-cancel", "seq",
   "scale", "x", "y"}                         GtkGestureZoom; x/y the
                                              bounding-box centre or null
  {"event": "gesture-cancel", "gesture": "click"|"drag"|"zoom", "seq"}
                                              GtkGesture::cancel

Sequence ids ("seq") are integers, null for events without a sequence
(pointer events). GDK's Wayland backend names a touch's GdkEventSequence
after its wl_touch id (GDK_SLOT_TO_EVENT_SEQUENCE), and the boxed type's
copy function is the identity, so every event and gesture signal of one
touch hands over the same C pointer. PyGObject wraps it in a new Python
object each time, but gobject.GBoxed's __hash__ and __eq__ compare that
pointer, so the wrapper keys a dict reliably. The compositor reuses touch
ids for later touches, so a raw touch-begin always allocates the next id
for its pointer; the mapping stays until the pointer begins again, so
gesture signals emitted after the raw end or cancel still resolve.
"""

import json
import sys

import gi

gi.require_version("Gdk", "4.0")
gi.require_version("Graphene", "1.0")
gi.require_version("Gtk", "4.0")
from gi.repository import Gdk, GLib, Graphene, Gtk  # noqa: E402


def emit(record):
    print(json.dumps(record), flush=True)


def rounded(value):
    return None if value is None else round(value, 2)


class Sequences:
    """Stable integer ids for GdkEventSequence pointers (see above)."""

    def __init__(self):
        self.ids = {}
        self.next = 0

    def begin(self, sequence):
        self.next += 1
        self.ids[sequence] = self.next
        return self.next

    def get(self, sequence):
        if sequence is None:
            return None
        if sequence not in self.ids:
            return self.begin(sequence)
        return self.ids[sequence]


SEQUENCES = Sequences()


def on_click(kind):
    def handler(gesture, n_press, x, y):
        emit({"event": kind, "x": round(x, 2), "y": round(y, 2), "n_press": n_press,
              "button": gesture.get_current_button()})

    return handler


def area_point(area, event):
    """The event's position in the area's coordinates, or (None, None)."""
    found, x, y = event.get_position()
    if not found:
        return None, None
    native = area.get_native()
    if native is None:
        return None, None
    # Event positions are surface coordinates; the native widget sits at
    # its surface transform inside the surface.
    sx, sy = native.get_surface_transform()
    ok, point = native.compute_point(area, Graphene.Point().init(x - sx, y - sy))
    if not ok:
        return None, None
    return point.x, point.y


def on_raw(controller, event):
    # PyGObject may hand the signal's GdkEvent over as None (a fundamental
    # type it cannot always marshal); the controller still has it.
    event = event if event is not None else controller.get_current_event()
    if event is None:
        return False
    area = controller.get_widget()
    kind = event.get_event_type()
    sequence = event.get_event_sequence()
    if kind == Gdk.EventType.TOUCH_BEGIN and sequence is not None:
        seq = SEQUENCES.begin(sequence)
    else:
        seq = SEQUENCES.get(sequence)
    device = event.get_device()
    x, y = area_point(area, event)
    emit({"event": "raw", "type": kind.value_nick, "seq": seq,
          "source": device.get_source().value_nick if device is not None else None,
          "x": rounded(x), "y": rounded(y)})
    return False


def on_cancel(name):
    def handler(_gesture, sequence):
        emit({"event": "gesture-cancel", "gesture": name, "seq": SEQUENCES.get(sequence)})

    return handler


def drag_record(gesture, kind, dx, dy):
    found, start_x, start_y = gesture.get_start_point()
    record = {"event": kind, "seq": SEQUENCES.get(gesture.get_current_sequence())}
    if found:
        record.update({"x": round(start_x + dx, 2), "y": round(start_y + dy, 2),
                       "start_x": round(start_x, 2), "start_y": round(start_y, 2)})
    else:
        record.update({"x": None, "y": None, "start_x": None, "start_y": None})
    emit(record)


def zoom_record(gesture, kind, sequence, scale):
    found, x, y = gesture.get_bounding_box_center()
    emit({"event": kind, "seq": SEQUENCES.get(sequence), "scale": round(scale, 4),
          "x": round(x, 2) if found else None, "y": round(y, 2) if found else None})


def activate(app):
    window = Gtk.ApplicationWindow(application=app, title="tuna-touch-client")
    area = Gtk.DrawingArea(hexpand=True, vexpand=True)

    raw = Gtk.EventControllerLegacy(propagation_phase=Gtk.PropagationPhase.CAPTURE)
    raw.connect("event", on_raw)
    area.add_controller(raw)

    click = Gtk.GestureClick(button=0)
    click.connect("pressed", on_click("pressed"))
    click.connect("released", on_click("released"))
    click.connect("cancel", on_cancel("click"))
    area.add_controller(click)

    drag = Gtk.GestureDrag(touch_only=True)
    drag.connect("drag-begin", lambda g, x, y: drag_record(g, "drag-begin", 0.0, 0.0))
    drag.connect("drag-update", lambda g, dx, dy: drag_record(g, "drag-update", dx, dy))
    drag.connect("drag-end", lambda g, dx, dy: drag_record(g, "drag-end", dx, dy))
    drag.connect("cancel", on_cancel("drag"))
    area.add_controller(drag)

    zoom = Gtk.GestureZoom()
    zoom.connect("begin", lambda g, s: zoom_record(g, "zoom-begin", s, g.get_scale_delta()))
    zoom.connect("scale-changed", lambda g, scale: zoom_record(
        g, "zoom-scale", g.get_last_updated_sequence(), scale))
    zoom.connect("end", lambda g, s: zoom_record(g, "zoom-end", s, g.get_scale_delta()))
    zoom.connect("cancel", lambda g, s: zoom_record(g, "zoom-cancel", s, g.get_scale_delta()))
    zoom.connect("cancel", on_cancel("zoom"))
    area.add_controller(zoom)

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
