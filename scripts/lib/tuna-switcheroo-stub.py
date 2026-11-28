#!/usr/bin/python3
"""A switcheroo-control service for app-menu proofs (#1182).

Usage: tuna-switcheroo-stub.py

Serves net.hadess.SwitcherooControl on the system bus (the proof's
private one): HasDualGpu true and two GPUs, an integrated default and a
discrete one whose Environment is DRI_PRIME=1 and a vendor variable.
The proof starts and kills it to show the menu follow the service.
"""
from gi.repository import Gio, GLib

NAME = "net.hadess.SwitcherooControl"
PATH = "/net/hadess/SwitcherooControl"
XML = """
<node><interface name="net.hadess.SwitcherooControl">
  <property name="HasDualGpu" type="b" access="read"/>
  <property name="NumGPUs" type="u" access="read"/>
  <property name="GPUs" type="aa{sv}" access="read"/>
</interface></node>
"""
GPUS = [
    {"Name": GLib.Variant("s", "Integrated"),
     "Default": GLib.Variant("b", True),
     "Discrete": GLib.Variant("b", False),
     "Environment": GLib.Variant("as", ["DRI_PRIME", "0"])},
    {"Name": GLib.Variant("s", "Discrete"),
     "Default": GLib.Variant("b", False),
     "Discrete": GLib.Variant("b", True),
     "Environment": GLib.Variant("as", ["DRI_PRIME", "1", "TUNA_PROOF_GPU", "discrete"])},
]


def get_property(conn, sender, path, iface, prop):
    if prop == "HasDualGpu":
        return GLib.Variant("b", True)
    if prop == "NumGPUs":
        return GLib.Variant("u", len(GPUS))
    if prop == "GPUs":
        return GLib.Variant("aa{sv}", GPUS)
    return None


def on_bus(conn, name):
    info = Gio.DBusNodeInfo.new_for_xml(XML).interfaces[0]
    conn.register_object(PATH, info, None, get_property, None)


def on_name(conn, name):
    print("tuna-switcheroo-stub: ready", flush=True)


Gio.bus_own_name(Gio.BusType.SYSTEM, NAME, Gio.BusNameOwnerFlags.NONE,
                 on_bus, on_name, None)
GLib.MainLoop().run()
