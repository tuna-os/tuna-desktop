#!/usr/bin/python3
"""GTK3/X11 window with a real menu bar, a submenu and a tooltip.

Usage: tuna-x11-menu-window.py EVENTS LAYOUT

GTK3 menus and tooltips on X11 are override-redirect windows, and an open
menu holds GTK's pointer and keyboard grabs: the windows no window manager
manages. EVENTS gets one line per thing that happened in the client
("shown menu", "activate alpha", "hidden menu", "press window"); LAYOUT is
rewritten with the root-coordinate rectangles of the widgets a journey
points at, and of the open menus' items.
"""
import json
import os
from pathlib import Path
import sys

os.environ['GDK_BACKEND'] = 'x11'
# The journey reads root coordinates as output pixels.
os.environ['GDK_SCALE'] = '1'
import gi  # noqa: E402
gi.require_version('Gtk', '3.0')
from gi.repository import GLib, Gtk  # noqa: E402

GLib.set_prgname('tuna-x11-menu')
events = Path(sys.argv[1])
layout = Path(sys.argv[2])
events.write_text('')
loop = GLib.MainLoop()

# Flat colours a journey can tell from everything else on the screen:
# menus green, the item under the pointer yellow, the tooltip red. The
# items are wide enough for the submenu to reach past the window.
css = Gtk.CssProvider()
css.load_from_data(b'''
window.background { background-color: #1c71d8; }
menu, .menu, .popup { background-color: #26a269; border: none; padding: 0; margin: 0; box-shadow: none; border-radius: 0; }
menu menuitem { background-color: #26a269; color: white; min-height: 30px; min-width: 240px; padding: 0 8px; }
menu menuitem:hover { background-color: #e5a50a; }
tooltip, tooltip.background, .tooltip { background-color: #a51d2d; border: none; border-radius: 0; box-shadow: none; padding: 12px; margin: 0; }
tooltip * { background-color: #a51d2d; color: white; }
''')


def record(text):
    with events.open('a') as out:
        out.write(text + '\n')


def root_rect(widget):
    """The widget's allocation in root coordinates, or None while unmapped."""
    gdk_window = widget.get_window()
    toplevel = widget.get_toplevel()
    if gdk_window is None or not widget.get_mapped() or not toplevel.get_mapped():
        return None
    _, x, y = gdk_window.get_origin()
    allocation = widget.get_allocation()
    if widget.get_has_window():
        return [x, y, allocation.width, allocation.height]
    return [x + allocation.x, y + allocation.y, allocation.width, allocation.height]


tracked = {}


def write_layout():
    rects = {name: root_rect(widget) for name, widget in tracked.items()}
    temporary = layout.with_suffix('.tmp')
    temporary.write_text(json.dumps(rects))
    os.replace(temporary, layout)
    return True


def item(menu, name):
    entry = Gtk.MenuItem(label=name)
    entry.connect('activate', lambda *_: record(f'activate {name.lower()}'))
    menu.append(entry)
    tracked[name.lower()] = entry
    return entry


window = Gtk.Window(title='Tuna Menu Probe')
window.set_wmclass('tuna-x11-menu', 'tuna-x11-menu')
window.set_default_size(420, 260)
window.connect('destroy', lambda *_: loop.quit())
Gtk.StyleContext.add_provider_for_screen(
    window.get_screen(), css, Gtk.STYLE_PROVIDER_PRIORITY_APPLICATION + 1)
window.connect('button-press-event', lambda *_: record('press window'))

box = Gtk.Box(orientation=Gtk.Orientation.VERTICAL)
bar = Gtk.MenuBar()
top = Gtk.MenuItem(label='Probe')
menu = Gtk.Menu()
menu.connect('show', lambda *_: record('shown menu'))
menu.connect('hide', lambda *_: record('hidden menu'))
item(menu, 'Alpha')
item(menu, 'Bravo')
more = Gtk.MenuItem(label='More')
submenu = Gtk.Menu()
submenu.connect('show', lambda *_: record('shown submenu'))
submenu.connect('hide', lambda *_: record('hidden submenu'))
item(submenu, 'Charlie')
item(submenu, 'Delta')
more.set_submenu(submenu)
menu.append(more)
tracked['more'] = more
top.set_submenu(menu)
bar.append(top)
tracked['probe'] = top
box.pack_start(bar, False, False, 0)

hint = Gtk.Button(label='Hover for a tooltip')
hint.set_tooltip_text('Tuna tooltip probe')
hint.set_margin_top(60)
hint.set_halign(Gtk.Align.CENTER)
tracked['hint'] = hint
box.pack_start(hint, False, False, 0)

window.add(box)
window.show_all()
tracked['window'] = box
GLib.timeout_add(100, write_layout)
loop.run()
