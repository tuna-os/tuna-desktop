#!/usr/bin/env python3
"""Drive org.gnome.Mutter.ColorManager calibration like GNOME Settings.

Usage: color-calibration-client.py CONNECTOR RAMP_JSON STATUS_FILE

Opens a session with CalibrateMonitor (as cc-color-calibrate.c does),
checks GammaLutSize, programs the red/green/blue ramp from RAMP_JSON
with SetCrtcGammaLut and writes {"session": path, "gamma_lut_size": n}
to STATUS_FILE. Then it reads one line from stdin: "stop" calls Stop,
anything else (or EOF) leaves the bus without stopping, so the
compositor must close the session when the caller vanishes.
"""

import json
import sys

from gi.repository import Gio, GLib

BUS = "org.gnome.Mutter.ColorManager"


def main():
    connector, ramp_file, status_file = sys.argv[1:4]
    ramp = json.load(open(ramp_file, encoding="utf-8"))
    bus = Gio.bus_get_sync(Gio.BusType.SESSION, None)
    (session,) = bus.call_sync(
        BUS, "/org/gnome/Mutter/ColorManager", BUS, "CalibrateMonitor",
        GLib.Variant("(s)", (connector,)), GLib.VariantType("(o)"),
        Gio.DBusCallFlags.NONE, 5000, None).unpack()
    (size,) = bus.call_sync(
        BUS, session, "org.freedesktop.DBus.Properties", "Get",
        GLib.Variant("(ss)", (BUS + ".Calibration", "GammaLutSize")),
        GLib.VariantType("(v)"), Gio.DBusCallFlags.NONE, 5000, None).unpack()
    bus.call_sync(
        BUS, session, BUS + ".Calibration", "SetCrtcGammaLut",
        GLib.Variant("(aqaqaq)", (ramp["red"], ramp["green"], ramp["blue"])),
        None, Gio.DBusCallFlags.NONE, 5000, None)
    with open(status_file, "w", encoding="utf-8") as stream:
        json.dump({"session": session, "gamma_lut_size": size}, stream)
    line = sys.stdin.readline().strip()
    if line == "stop":
        bus.call_sync(BUS, session, BUS + ".Calibration", "Stop", None, None,
                      Gio.DBusCallFlags.NONE, 5000, None)
    bus.flush_sync(None)


if __name__ == "__main__":
    main()
