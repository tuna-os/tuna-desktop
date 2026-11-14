#!/usr/bin/python3
"""A StatusNotifierItem with a dbusmenu, for tray proofs.

Usage: tuna-tray-item-stub.py MARKER

Registers with org.kde.StatusNotifierWatcher (retrying until the shell
owns it) by its unique name and object path, KStatusNotifierItem-style,
titled "Tuna Desktop Tray Probe" with one menu row "Open Probe". Each
call appends one line to MARKER: firing the row "clicked 1", Activate
"activated", SecondaryActivate "secondary", Scroll "scroll DELTA
ORIENTATION".
"""
import sys

from gi.repository import Gio, GLib

MARKER = sys.argv[1]
XML = """
<node>
 <interface name="org.kde.StatusNotifierItem">
  <property name="Title" type="s" access="read"/>
  <property name="Status" type="s" access="read"/>
  <property name="IconName" type="s" access="read"/>
  <property name="IconPixmap" type="a(iiay)" access="read"/>
  <property name="AttentionIconName" type="s" access="read"/>
  <property name="AttentionIconPixmap" type="a(iiay)" access="read"/>
  <property name="ToolTip" type="(sa(iiay)ss)" access="read"/>
  <property name="Menu" type="o" access="read"/>
  <method name="Activate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
  <method name="SecondaryActivate"><arg type="i" direction="in"/><arg type="i" direction="in"/></method>
  <method name="Scroll"><arg type="i" direction="in"/><arg type="s" direction="in"/></method>
 </interface>
 <interface name="com.canonical.dbusmenu">
  <method name="GetLayout">
   <arg type="i" direction="in"/><arg type="i" direction="in"/><arg type="as" direction="in"/>
   <arg type="u" direction="out"/><arg type="(ia{sv}av)" direction="out"/>
  </method>
  <method name="Event">
   <arg type="i" direction="in"/><arg type="s" direction="in"/>
   <arg type="v" direction="in"/><arg type="u" direction="in"/>
  </method>
 </interface>
</node>
"""
NODE = Gio.DBusNodeInfo.new_for_xml(XML)
PROPS = {
    "Title": GLib.Variant("s", "Tuna Desktop Tray Probe"),
    "Status": GLib.Variant("s", "Active"),
    "IconName": GLib.Variant("s", "mail-unread-symbolic"),
    "IconPixmap": GLib.Variant("a(iiay)", []),
    "AttentionIconName": GLib.Variant("s", ""),
    "AttentionIconPixmap": GLib.Variant("a(iiay)", []),
    "ToolTip": GLib.Variant("(sa(iiay)ss)", ("", [], "Tray probe tip", "Menu opens on click")),
    "Menu": GLib.Variant("o", "/MenuBar"),
}


def mark(text):
    with open(MARKER, "a") as fh:
        fh.write(text + "\n")


def item_call(conn, sender, path, iface, method, params, invocation):
    if method == "Activate":
        mark("activated")
    elif method == "SecondaryActivate":
        mark("secondary")
    elif method == "Scroll":
        delta, orientation = params.unpack()
        mark(f"scroll {delta} {orientation}")
    invocation.return_value(None)


def menu_call(conn, sender, path, iface, method, params, invocation):
    if method == "GetLayout":
        row = GLib.Variant("(ia{sv}av)", (1, {"label": GLib.Variant("s", "Open Probe"),
                                              "enabled": GLib.Variant("b", True)}, []))
        root = (0, {"children-display": GLib.Variant("s", "submenu")}, [row])
        invocation.return_value(GLib.Variant("(u(ia{sv}av))", (1, root)))
    elif method == "Event":
        item_id, event, _, _ = params.unpack()
        if event == "clicked":
            mark(f"clicked {item_id}")
        invocation.return_value(None)


bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
bus.register_object("/StatusNotifierItem", NODE.lookup_interface("org.kde.StatusNotifierItem"),
                    item_call, lambda *a: PROPS[a[4]], None)
bus.register_object("/MenuBar", NODE.lookup_interface("com.canonical.dbusmenu"),
                    menu_call, None, None)


def register():
    try:
        bus.call_sync("org.kde.StatusNotifierWatcher", "/StatusNotifierWatcher",
                      "org.kde.StatusNotifierWatcher", "RegisterStatusNotifierItem",
                      GLib.Variant("(s)", (bus.get_unique_name() + "/StatusNotifierItem",)),
                      None, Gio.DBusCallFlags.NONE, 2000, None)
        print("tuna-tray-item-stub: registered", flush=True)
        return False
    except GLib.Error:
        return True  # watcher not up yet: retry


GLib.timeout_add(500, register)
GLib.MainLoop().run()
