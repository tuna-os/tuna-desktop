#!/usr/bin/python3
"""A solid-colour GTK3 window on the compositor's XWayland.

The proof sets `_NET_WM_ICON_GEOMETRY` and sends the ICCCM IconicState
request with xprop/xdotool, so the wire is the X server's own.
"""
import os

os.environ["GDK_BACKEND"] = "x11"

import gi  # noqa: E402

gi.require_version("Gtk", "3.0")
from gi.repository import GLib, Gtk  # noqa: E402

GLib.set_prgname("tuna-x11-icon-geometry")
css = Gtk.CssProvider()
css.load_from_data(b"window, .background { background: rgb(200,40,60); }")
loop = GLib.MainLoop()
window = Gtk.Window(title="Tuna X11 Icon Geometry")
window.set_wmclass("tuna-x11-icon-geometry", "tuna-x11-icon-geometry")
window.get_style_context().add_provider(css, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION)
window.set_default_size(400, 300)
window.connect("destroy", lambda *_: loop.quit())
window.show_all()
loop.run()
