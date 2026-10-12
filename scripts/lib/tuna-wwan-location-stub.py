#!/usr/bin/python3
"""ModemManager + GeoClue2 stubs for the WWAN/location proofs (#362).

Usage: tuna-wwan-location-stub.py LOG

Serves, on the bus at $DBUS_SYSTEM_BUS_ADDRESS (the proof's private
system bus):

- ModemManager1: one modem (ExampleTel, connected, 82% recent signal,
  no SIM lock) at /org/freedesktop/ModemManager1/Modem/0, the modem
  NetworkManager's wwan0 device points at. Every property read is
  appended to LOG timestamped ("<epoch> Modem.Get <prop>"), so a gate
  can prove the shell's rows come from live reads, not its model.
- GeoClue2: the Manager with AddAgent (appended timestamped with the
  caller's bus name: "<epoch> AddAgent <id> from <sender>") and a
  writable InUse property (writes appended timestamped too).

org.tuna.Proof.WwanLocation (always owned, so the proof can call it
while either daemon is gone) releases and reclaims each name, so
absent-service gates need no process restart. The names start owned.

Writes "ready" when all three names are owned.
"""
import os
import sys
import time

from gi.repository import Gio, GLib

LOG = sys.argv[1]

XML = """
<node>
  <interface name="org.freedesktop.ModemManager1.Modem">
    <property name="State" type="i" access="read"/>
    <property name="SignalQuality" type="(ub)" access="read"/>
    <property name="OperatorName" type="s" access="read"/>
    <property name="UnlockRequired" type="u" access="read"/>
  </interface>
  <interface name="org.freedesktop.GeoClue2.Manager">
    <method name="AddAgent"><arg type="s" direction="in"/></method>
    <property name="InUse" type="b" access="readwrite"/>
  </interface>
  <interface name="org.tuna.Proof.WwanLocation">
    <method name="ModemManagerPresent"><arg type="b" direction="in"/></method>
    <method name="GeoCluePresent"><arg type="b" direction="in"/></method>
  </interface>
</node>
"""
NODE = Gio.DBusNodeInfo.new_for_xml(XML)

MM = "org.freedesktop.ModemManager1"
MODEM = "/org/freedesktop/ModemManager1/Modem/0"
MODEM_IFACE = "org.freedesktop.ModemManager1.Modem"
GEOCLUE = "org.freedesktop.GeoClue2"
MANAGER = "/org/freedesktop/GeoClue2/Manager"
MANAGER_IFACE = "org.freedesktop.GeoClue2.Manager"
PROOF = "org.tuna.Proof.WwanLocation"
PROOF_PATH = "/org/tuna/Proof/WwanLocation"
PROOF_IFACE = "org.tuna.Proof.WwanLocation"

state = {
    (MODEM, MODEM_IFACE): {
        "State": GLib.Variant("i", 11),
        "SignalQuality": GLib.Variant("(ub)", (82, True)),
        "OperatorName": GLib.Variant("s", "ExampleTel"),
        "UnlockRequired": GLib.Variant("u", 1),
    },
    (MANAGER, MANAGER_IFACE): {
        "InUse": GLib.Variant("b", False),
    },
}

conn = Gio.DBusConnection.new_for_address_sync(
    os.environ["DBUS_SYSTEM_BUS_ADDRESS"],
    Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT
    | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
    None, None)


def log(line):
    with open(LOG, "a") as fh:
        fh.write(f"{time.time():.3f} {line}\n")


def method_call(c, sender, path, iface, method, params, invocation):
    if method == "AddAgent":
        log(f"AddAgent {params.unpack()[0]} from {sender}")
        invocation.return_value(None)
    elif method == "ModemManagerPresent":
        present(c, MM, params.unpack()[0])
        invocation.return_value(None)
    elif method == "GeoCluePresent":
        present(c, GEOCLUE, params.unpack()[0])
        invocation.return_value(None)
    else:
        invocation.return_dbus_error("org.freedesktop.DBus.Error.UnknownMethod", method)


def get_property(c, sender, path, iface, prop):
    if iface == MODEM_IFACE:
        log(f"Modem.Get {prop}")
    return state[(path, iface)][prop]


def set_property(c, sender, path, iface, prop, value):
    state[(path, iface)][prop] = value
    log(f"{'GeoClue' if iface == MANAGER_IFACE else iface}.Set {prop} {str(value.unpack()).lower()}")
    c.emit_signal(None, path, "org.freedesktop.DBus.Properties", "PropertiesChanged",
                  GLib.Variant("(sa{sv}as)", (iface, {prop: value}, [])))
    return True


def request(c, name):
    c.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                "RequestName", GLib.Variant("(su)", (name, 4)), None,
                Gio.DBusCallFlags.NONE, -1, None)


def present(c, name, owned):
    if owned:
        request(c, name)
    else:
        c.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus", "org.freedesktop.DBus",
                    "ReleaseName", GLib.Variant("(s)", (name,)), None,
                    Gio.DBusCallFlags.NONE, -1, None)


for path, iface in ((MODEM, MODEM_IFACE), (MANAGER, MANAGER_IFACE), (PROOF_PATH, PROOF_IFACE)):
    conn.register_object(path, NODE.lookup_interface(iface), method_call, get_property, set_property)

for name in (MM, GEOCLUE, PROOF):
    request(conn, name)

print("tuna-wwan-location-stub: ready", flush=True)
GLib.MainLoop().run()
