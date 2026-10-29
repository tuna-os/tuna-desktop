#!/usr/bin/python3
"""A GTK 4 window with known geometry for the screen magnifier proof (#349).

Usage: tuna-magnifier-probe.py LOG [REQUESTS]

Maps one Gtk.Window (app id tuna-magnifier-probe) holding a "Probe"
button, an area filled green (#26a269) on its left half and orange
(#e66100) on its right half, and an entry. GTK's own AT-SPI
bridge stays on, so focus and caret events are real. Appends JSON lines
to LOG:

  {"bounds": {"button": [x, y, w, h], "area": [...], "entry": [...]}}
      once mapped, in window (frame) coordinates, the frame the
      compositor's window rect gives (titlebar included);
  {"click": [x, y]}  for every primary press on the area, area-local;
  {"text": "..."}    for every entry change.

REQUESTS, polled every 50 ms, holds {"generation": N, "request":
"focus-button" | "focus-entry"}; each new generation moves keyboard
focus there and acknowledges by writing N to REQUESTS.ack.
"""
import json
import os
import sys
from pathlib import Path

# Harness windows must not wait on portals or need GPU rendering.
os.environ.setdefault("ADW_DISABLE_PORTAL", "1")
os.environ.setdefault("GSK_RENDERER", "cairo")

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import GLib, Gtk  # noqa: E402

LOG = Path(sys.argv[1])
REQUESTS = sys.argv[2] if len(sys.argv) > 2 else None


def log(record):
    with LOG.open("a") as out:
        out.write(json.dumps(record) + "\n")


# CSS fills, not a cairo draw func: PyGObject's cairo converter
# (python3-gi-cairo) is not part of the proof's dependencies.
CSS = """
.probe-green { background: #26a269; }
.probe-orange { background: #e66100; }
"""


def bounds(widget, window):
    ok, rect = widget.compute_bounds(window)
    if not ok:
        return None
    return [
        round(rect.origin.x),
        round(rect.origin.y),
        round(rect.size.width),
        round(rect.size.height),
    ]


def build(loop):
    # No Gtk.Application: the probe needs no bus name of its own.
    win = Gtk.Window(title="Magnifier Probe")
    win.connect("close-request", lambda *_: loop.quit() or False)
    win.set_default_size(600, 420)
    box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=0)
    button = Gtk.Button(label="Probe")
    button.set_halign(Gtk.Align.START)
    box.append(button)
    provider = Gtk.CssProvider()
    if hasattr(provider, "load_from_string"):  # GTK 4.12+
        provider.load_from_string(CSS)
    else:
        provider.load_from_data(CSS, -1)
    Gtk.StyleContext.add_provider_for_display(
        win.get_display(), provider, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION
    )
    area = Gtk.Box(orientation=Gtk.Orientation.HORIZONTAL, homogeneous=True)
    area.set_size_request(600, 240)
    area.set_hexpand(True)
    area.set_vexpand(True)
    for css_class in ("probe-green", "probe-orange"):
        half = Gtk.Box()
        half.add_css_class(css_class)
        half.set_hexpand(True)
        area.append(half)
    click = Gtk.GestureClick(button=1)
    click.connect("pressed", lambda _g, _n, x, y: log({"click": [x, y]}))
    area.add_controller(click)
    box.append(area)
    entry = Gtk.Entry()
    entry.set_hexpand(True)
    entry.connect("changed", lambda e: log({"text": e.get_text()}))
    box.append(entry)
    win.set_child(box)
    win.set_focus(button)

    def report():
        parts = {"button": button, "area": area, "entry": entry}
        found = {name: bounds(widget, win) for name, widget in parts.items()}
        if any(value is None for value in found.values()):
            return True
        log({"bounds": found})
        return False

    win.connect("map", lambda *_: GLib.timeout_add(300, report))
    win.present()
    if REQUESTS:
        handled = [-1]

        def request():
            try:
                data = json.loads(Path(REQUESTS).read_text())
                generation = data["generation"]
                if generation != handled[0]:
                    target = {"focus-button": button, "focus-entry": entry}[data["request"]]
                    target.grab_focus()
                    handled[0] = generation
                    Path(REQUESTS + ".ack").write_text(str(generation))
            except (OSError, ValueError, KeyError):
                pass
            return True

        GLib.timeout_add(50, request)


GLib.set_prgname("tuna-magnifier-probe")
main_loop = GLib.MainLoop()
build(main_loop)
main_loop.run()
