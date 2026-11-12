#!/usr/bin/python3
"""GNOME Clocks or GNOME Weather as gnome-shell's date menu sees them on
the session bus (P-PN-04, #1036): the app's well-known name and the
properties it exports for the shell.

Usage: tuna-shell-integration-stub.py clocks|weather FILE

FILE holds the exported properties as GVariant text of type a{sv}:
`{'Locations': <[...]>}` for Clocks (org.gnome.Shell.ClocksIntegration),
with `'AutomaticLocation': <false>` beside it for Weather
(org.gnome.Shell.WeatherIntegration). Rewriting FILE changes the
properties and emits PropertiesChanged, as the app does when the user
edits its locations. Prints "ready" once it owns the name.
"""
import sys
from gi.repository import Gio, GLib

APPS = {
    "clocks": ("org.gnome.clocks", "/org/gnome/clocks", '''<node>
<interface name="org.gnome.Shell.ClocksIntegration">
<property name="Locations" type="av" access="read"/>
</interface></node>'''),
    "weather": ("org.gnome.Weather", "/org/gnome/Weather", '''<node>
<interface name="org.gnome.Shell.WeatherIntegration">
<property name="AutomaticLocation" type="b" access="read"/>
<property name="Locations" type="av" access="read"/>
</interface></node>'''),
}

if len(sys.argv) != 3 or sys.argv[1] not in APPS:
    print(__doc__, file=sys.stderr)
    sys.exit(2)
BUS, PATH, XML = APPS[sys.argv[1]]
interface = Gio.DBusNodeInfo.new_for_xml(XML).interfaces[0]


def read():
    with open(sys.argv[2], encoding="utf-8") as source:
        text = source.read()
    return text, GLib.Variant.parse(GLib.VariantType.new("a{sv}"), text, None, None)


text, properties = read()


def get_property(_conn, _sender, _path, _interface, name):
    return properties.lookup_value(name, None)


def poll():
    global text, properties
    try:
        new_text, new_properties = read()
    except (OSError, GLib.Error):
        # Mid-rewrite: the next poll reads the whole file.
        return True
    if new_text != text:
        text, properties = new_text, new_properties
        connection.emit_signal(
            None, PATH, "org.freedesktop.DBus.Properties", "PropertiesChanged",
            GLib.Variant.new_tuple(GLib.Variant("s", interface.name), properties,
                                   GLib.Variant("as", [])))
    return True


connection = Gio.bus_get_sync(Gio.BusType.SESSION, None)
connection.register_object(PATH, interface, None, get_property, None)
reply = connection.call_sync(
    "org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName",
    GLib.Variant("(su)", (BUS, 4)), None, Gio.DBusCallFlags.NONE, -1, None)
if reply.unpack()[0] not in (1, 4):
    raise RuntimeError(f"{BUS} already has an owner on the proof bus")
GLib.timeout_add(200, poll)
print("ready", flush=True)
GLib.MainLoop().run()
