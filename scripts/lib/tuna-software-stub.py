#!/usr/bin/python3
"""GNOME Software's action group for app-menu proofs (#1182).

Usage: tuna-software-stub.py MARKER

Serves org.gtk.Actions as org.gnome.Software at /org/gnome/Software on
the session bus. Each Activate call appends "<action> <parameters>" to
MARKER, so the proof can read what App Details asked for.
"""
import sys

from gi.repository import Gio, GLib

MARKER = sys.argv[1]
XML = """
<node><interface name="org.gtk.Actions">
  <method name="Activate">
    <arg type="s" direction="in"/><arg type="av" direction="in"/>
    <arg type="a{sv}" direction="in"/></method>
</interface></node>
"""


def method_call(conn, sender, path, iface, method, params, invocation):
    if method == "Activate":
        action, args, _platform = params.unpack()
        with open(MARKER, "a") as fh:
            fh.write(f"{action} {args}\n")
        invocation.return_value(None)


def on_bus(conn, name):
    info = Gio.DBusNodeInfo.new_for_xml(XML).interfaces[0]
    conn.register_object("/org/gnome/Software", info, method_call, None, None)


def on_name(conn, name):
    print("tuna-software-stub: ready", flush=True)


Gio.bus_own_name(Gio.BusType.SESSION, "org.gnome.Software",
                 Gio.BusNameOwnerFlags.NONE, on_bus, on_name, None)
GLib.MainLoop().run()
