#!/usr/bin/python3
"""Stand in for GNOME Settings' Displays panel in proof harnesses.

Usage: tuna-monitor-labels-probe.py MARKER [--stranger]

Owns org.gnome.Settings (so org.gnome.Shell's caller allowlist admits it),
reads the monitors from Mutter's DisplayConfig, and asks the shell to
label them 1, 2, ... through org.gnome.Shell.ShowMonitorLabels, as
cc-display-panel.c's show_monitors_labels does. SIGUSR1 calls
HideMonitorLabels; SIGUSR2 leaves the bus without hiding them. Progress
lines go to MARKER: "monitors CONNECTOR...", "shown", "hidden".

With --stranger the probe does not own the Settings name and notes
"denied ERROR" with the error ShowMonitorLabels answers.
"""
import signal
import sys

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

MARKER = sys.argv[1]
STRANGER = "--stranger" in sys.argv[2:]
bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
loop = GLib.MainLoop()


def note(line):
    with open(MARKER, "a", encoding="utf-8") as fh:
        fh.write(line + "\n")


def call(name, path, iface, method, args, reply):
    return bus.call_sync(name, path, iface, method, args,
                         GLib.VariantType(reply) if reply else None,
                         Gio.DBusCallFlags.NONE, 5000, None)


def connectors():
    state = call("org.gnome.Mutter.DisplayConfig", "/org/gnome/Mutter/DisplayConfig",
                 "org.gnome.Mutter.DisplayConfig", "GetCurrentState", None,
                 "(ua((ssss)a(siiddada{sv})a{sv})a(iiduba(ssss)a{sv})a{sv})")
    return [monitor[0][0] for monitor in state.unpack()[1]]


def show():
    try:
        names = connectors()
        note("monitors " + " ".join(names))
        labels = {name: GLib.Variant("i", i + 1) for i, name in enumerate(names)}
        call("org.gnome.Shell", "/org/gnome/Shell", "org.gnome.Shell",
             "ShowMonitorLabels", GLib.Variant("(a{sv})", (labels,)), None)
    except GLib.Error as e:
        note("denied " + (Gio.dbus_error_get_remote_error(e) or e.message))
        return False
    note("shown")
    return True


def hide(*_):
    try:
        call("org.gnome.Shell", "/org/gnome/Shell", "org.gnome.Shell",
             "HideMonitorLabels", None, None)
    except GLib.Error as e:
        note("error " + e.message)
        return
    note("hidden")


def leave(*_):
    loop.quit()


def owned(*_):
    show()


GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, signal.SIGUSR1, lambda: (hide(), True)[1])
GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, signal.SIGUSR2, lambda: (leave(), True)[1])
if STRANGER:
    GLib.idle_add(lambda: (show(), loop.quit(), False)[2])
else:
    Gio.bus_own_name_on_connection(bus, "org.gnome.Settings",
                                   Gio.BusNameOwnerFlags.NONE, owned, None)
GLib.timeout_add_seconds(120, loop.quit)
loop.run()
