#!/usr/bin/python3
"""A GTK 4 window with two text entries for the on-screen keyboard proof (#349).

Usage: tuna-osk-probe.py LOG

Maps one Gtk.Window (app id tuna-osk-probe) holding entry "a", entry
"b" and a "Done" button at its top, clear of the on-screen keyboard.
The entries use GTK's own Wayland input method module (text-input-v3),
as GNOME's clients do. Appends JSON lines to LOG, each stamped with
CLOCK_MONOTONIC seconds ("t"), the clock the proof's state watcher uses:

  {"bounds": {"a": [x, y, w, h], "b": [...], "done": [...]}}
      once mapped, in window (frame) coordinates;
  {"press": [x, y], "t": T}       every primary press in the window;
  {"focus": "a" | "b" | "done" | null, "t": T}  every focus change;
  {"text": {"a": "...", "b": "..."}, "t": T}     every entry change.
"""
import json
import os
import sys
import time
from pathlib import Path

# Harness windows must not wait on portals or need GPU rendering.
os.environ.setdefault("ADW_DISABLE_PORTAL", "1")
os.environ.setdefault("GSK_RENDERER", "cairo")

import gi

gi.require_version("Gtk", "4.0")
from gi.repository import GLib, Gtk  # noqa: E402

LOG = Path(sys.argv[1])


def log(record):
    record = dict(record, t=time.monotonic())
    with LOG.open("a") as out:
        out.write(json.dumps(record) + "\n")


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
    win = Gtk.Window(title="OSK Probe")
    win.connect("close-request", lambda *_: loop.quit() or False)
    win.set_default_size(600, 300)
    box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
    box.set_margin_top(12)
    box.set_margin_start(12)
    box.set_margin_end(12)
    entries = {}
    for name in ("a", "b"):
        entry = Gtk.Entry()
        entry.set_hexpand(True)
        entry.set_size_request(-1, 48)
        box.append(entry)
        entries[name] = entry
    done = Gtk.Button(label="Done")
    done.set_size_request(-1, 48)
    box.append(done)
    win.set_child(box)
    win.set_focus(done)
    widgets = dict(entries, done=done)

    def texts(_entry):
        log({"text": {name: entry.get_text() for name, entry in entries.items()}})

    for name, entry in entries.items():
        entry.connect("changed", texts)
        entry.connect("preedit-changed", lambda _entry, text, field=name:
                      log({"preedit": text, "field": field}))

    def focus(*_):
        widget = win.get_focus()
        owner = None
        for name, candidate in widgets.items():
            if widget is not None and (widget == candidate or widget.is_ancestor(candidate)):
                owner = name
        log({"focus": owner})

    win.connect("notify::focus-widget", focus)
    # Capture phase, never claimed: the entries and button still get it.
    click = Gtk.GestureClick(button=1)
    click.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
    click.connect("pressed", lambda _g, _n, x, y: log({"press": [x, y]}))
    win.add_controller(click)
    keys = Gtk.EventControllerKey()
    keys.set_propagation_phase(Gtk.PropagationPhase.CAPTURE)
    keys.connect("key-pressed", lambda _keys, key, _code, _mods:
                 log({"key": key, "pressed": True}) or False)
    keys.connect("key-released", lambda _keys, key, _code, _mods:
                 log({"key": key, "pressed": False}))
    win.add_controller(keys)

    def report():
        found = {name: bounds(widget, win) for name, widget in widgets.items()}
        if any(value is None for value in found.values()):
            return True
        log({"bounds": found})
        return False

    win.connect("map", lambda *_: GLib.timeout_add(300, report))
    win.present()


GLib.set_prgname("tuna-osk-probe")
main_loop = GLib.MainLoop()
build(main_loop)
main_loop.run()
