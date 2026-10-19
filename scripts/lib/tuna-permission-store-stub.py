#!/usr/bin/python3
"""Private-session PermissionStore for the live shortcut consent and location proofs."""
import json
import sys
from gi.repository import Gio, GLib

BUS = "org.freedesktop.impl.portal.PermissionStore"
PATH = "/org/freedesktop/impl/portal/PermissionStore"
permissions = {}
location_permissions = {}
TABLES = {("gnome", "shortcuts-inhibitor"): permissions,
          ("location", "location"): location_permissions}
node = Gio.DBusNodeInfo.new_for_xml('''<node><interface name="org.freedesktop.impl.portal.PermissionStore">
<method name="Lookup"><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="a{sas}" direction="out"/><arg type="v" direction="out"/></method>
<method name="Set"><arg type="s" direction="in"/><arg type="b" direction="in"/><arg type="s" direction="in"/><arg type="a{sas}" direction="in"/><arg type="v" direction="in"/></method>
<method name="SetPermission"><arg type="s" direction="in"/><arg type="b" direction="in"/><arg type="s" direction="in"/><arg type="s" direction="in"/><arg type="as" direction="in"/></method>
</interface></node>''')

def call(_conn, _sender, _path, _interface, method, params, invocation):
    args = params.unpack()
    key = (args[0], args[2] if method in ("Set", "SetPermission") else args[1])
    table = TABLES.get(key)
    if table is None:
        invocation.return_dbus_error("org.freedesktop.portal.Error.NotFound", "Unknown table/id")
        return
    if method == "Lookup":
        invocation.return_value(GLib.Variant("(a{sas}v)", (table, GLib.Variant("s", ""))))
    elif method == "Set":
        table.clear()
        table.update(args[3])
        invocation.return_value(GLib.Variant("()", ()))
    else:
        table[args[3]] = args[4]
        if key == ("gnome", "shortcuts-inhibitor"):
            with open(sys.argv[1], "w", encoding="utf-8") as out:
                json.dump(table, out)
        invocation.return_value(GLib.Variant("()", ()))

connection = Gio.bus_get_sync(Gio.BusType.SESSION, None)
connection.register_object(PATH, node.interfaces[0], call, None, None)
reply = connection.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus", "RequestName", GLib.Variant("(su)", (BUS, 4)), None, Gio.DBusCallFlags.NONE, -1, None)
if reply.unpack()[0] not in (1, 4):
    raise RuntimeError("PermissionStore already has an owner on the proof bus")
print("ready", flush=True)
GLib.MainLoop().run()
