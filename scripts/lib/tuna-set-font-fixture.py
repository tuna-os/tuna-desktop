#!/usr/bin/python3
"""GTK font-settings fixture for G-SET-FONTS (#594).

Usage: tuna-set-font-fixture.py OUT_JSON

Maps one window with two sample labels and reports what a GTK app
observes for GNOME's document and monospace font keys. GTK has no
Gtk.Settings properties for these (only gtk-font-name): real GNOME
apps such as gnome-terminal and gedit read them directly through
Gio.Settings, and so does this fixture. OUT_JSON is rewritten on map
and on every settings change with:

  app_document, app_monospace: Gio.Settings org.gnome.desktop.interface
      document-font-name / monospace-font-name, as the app read them

The labels render through Pango markup `font_desc` from those values,
so a visible repaint proves the same app-observed read the JSON
asserts. Exits when the window closes.
"""

import json
import os
import sys
from pathlib import Path

# Harness windows must not wait on portals or need GPU rendering.
os.environ.setdefault("ADW_DISABLE_PORTAL", "1")
os.environ.setdefault("GSK_RENDERER", "cairo")

import gi  # noqa: E402

gi.require_version("Gtk", "4.0")
gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib, Gtk  # noqa: E402

OUT = Path(sys.argv[1])
DOCUMENT_SAMPLE = "Document Ag 0123"
MONOSPACE_SAMPLE = "Mono Ag 0123 ll1II0O"


def snapshot(desktop):
    return {
        "source": "Gio.Settings org.gnome.desktop.interface (as GNOME apps read it)",
        "app_document": desktop.get_string("document-font-name"),
        "app_monospace": desktop.get_string("monospace-font-name"),
    }


def main():
    desktop = Gio.Settings.new("org.gnome.desktop.interface")
    GLib.set_prgname("tuna-set-fonts-fixture")
    window = Gtk.Window(title="Tuna Set Fonts Fixture")
    window.set_default_size(640, 300)
    box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL, spacing=12)
    box.set_margin_top(24)
    box.set_margin_start(24)
    box.set_margin_end(24)
    document_label = Gtk.Label()
    monospace_label = Gtk.Label()
    box.append(document_label)
    box.append(monospace_label)
    window.set_child(box)

    def refresh():
        try:
            report = snapshot(desktop)
        except Exception as exc:  # keep the window alive for diagnosis
            report = {"error": str(exc)}
        else:
            document_label.set_markup(
                f'<span font_desc="{GLib.markup_escape_text(report["app_document"])}">'
                f"{GLib.markup_escape_text(DOCUMENT_SAMPLE)}</span>")
            monospace_label.set_markup(
                f'<span font_desc="{GLib.markup_escape_text(report["app_monospace"])}">'
                f"{GLib.markup_escape_text(MONOSPACE_SAMPLE)}</span>")
        tmp = OUT.with_suffix(".tmp")
        tmp.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
        tmp.rename(OUT)
        return True

    desktop.connect("changed::document-font-name", lambda *_: refresh())
    desktop.connect("changed::monospace-font-name", lambda *_: refresh())
    # The keyfile backend notifies on save; poll as well so a missed
    # signal delays the report instead of hanging the gate.
    GLib.timeout_add(500, refresh)
    loop = GLib.MainLoop()
    window.connect("close-request", lambda *_: loop.quit() or False)
    window.present()
    refresh()
    loop.run()


main()
