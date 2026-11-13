#!/usr/bin/python3
"""A real libayatana-appindicator item, for tray proofs (P-TR-01).

Usage: tuna-tray-ayatana.py MARKER [NAME]

Runs a GTK 3 AyatanaAppIndicator3 indicator — the library Ubuntu and
Fedora apps use — titled and described NAME (default "Tuna Ayatana
Probe"), labelled "7", whose Gtk.Menu holds "Ayatana _Open", a
separator, the check item "Ayatana Check" (active) and the submenu
"Ayatana More" > "Ayatana Deep". The middle-click target is "Ayatana
Open". Each activation, toggle and scroll appends one line to MARKER
("activate <label>", "toggled Ayatana Check <True|False>",
"scroll <delta> <direction>"); "ready" is appended once the indicator
exists. Signals drive live changes:

  SIGUSR1  label "8", status NeedsAttention (attention icon and
           description "<NAME> Alert")
  SIGUSR2  toggle Passive / Active
  SIGHUP   append the row "Ayatana Added"

The backend is whatever GDK picks: GDK_BACKEND=x11 with an XWayland
DISPLAY makes it an X11 client; the indicator itself is D-Bus either way.
"""
import signal
import sys

import gi

gi.require_version("Gtk", "3.0")
gi.require_version("AyatanaAppIndicator3", "0.1")
from gi.repository import AyatanaAppIndicator3 as AppIndicator  # noqa: E402
from gi.repository import GLib, Gtk  # noqa: E402

MARKER = sys.argv[1]
NAME = sys.argv[2] if len(sys.argv) > 2 else "Tuna Ayatana Probe"


def mark(text):
    with open(MARKER, "a", encoding="utf-8") as fh:
        fh.write(text + "\n")


indicator = AppIndicator.Indicator.new(
    "tuna-tray-ayatana-" + NAME.replace(" ", "-").lower(),
    "mail-unread-symbolic",
    AppIndicator.IndicatorCategory.APPLICATION_STATUS,
)
indicator.set_title(NAME)
indicator.set_icon_full("mail-unread-symbolic", NAME)
indicator.set_attention_icon_full("dialog-warning-symbolic", NAME + " Alert")
indicator.set_label("7", "88")

menu = Gtk.Menu()
open_item = Gtk.MenuItem.new_with_mnemonic("Ayatana _Open")
open_item.connect("activate", lambda _: mark("activate Ayatana Open"))
menu.append(open_item)
menu.append(Gtk.SeparatorMenuItem())
check = Gtk.CheckMenuItem(label="Ayatana Check")
check.set_active(True)
check.connect("toggled", lambda item: mark(f"toggled Ayatana Check {item.get_active()}"))
menu.append(check)
more = Gtk.MenuItem(label="Ayatana More")
submenu = Gtk.Menu()
deep = Gtk.MenuItem(label="Ayatana Deep")
deep.connect("activate", lambda _: mark("activate Ayatana Deep"))
submenu.append(deep)
more.set_submenu(submenu)
menu.append(more)
menu.show_all()
indicator.set_menu(menu)
indicator.set_secondary_activate_target(open_item)
indicator.connect(
    "scroll-event", lambda _, delta, direction: mark(f"scroll {delta} {int(direction)}")
)
indicator.set_status(AppIndicator.IndicatorStatus.ACTIVE)


def attention():
    indicator.set_label("8", "88")
    indicator.set_status(AppIndicator.IndicatorStatus.ATTENTION)
    return GLib.SOURCE_CONTINUE


def passive():
    if indicator.get_status() == AppIndicator.IndicatorStatus.PASSIVE:
        indicator.set_status(AppIndicator.IndicatorStatus.ACTIVE)
    else:
        indicator.set_status(AppIndicator.IndicatorStatus.PASSIVE)
    return GLib.SOURCE_CONTINUE


def added():
    item = Gtk.MenuItem(label="Ayatana Added")
    item.connect("activate", lambda _: mark("activate Ayatana Added"))
    item.show()
    menu.append(item)
    return GLib.SOURCE_CONTINUE


GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, signal.SIGUSR1, attention)
GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, signal.SIGUSR2, passive)
GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, signal.SIGHUP, added)
GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, signal.SIGTERM, Gtk.main_quit)
mark("ready")
Gtk.main()
