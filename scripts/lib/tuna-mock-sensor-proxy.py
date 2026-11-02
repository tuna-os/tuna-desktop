#!/usr/bin/env python3
"""Mock iio-sensor-proxy for tuna-drm-smoke's D-ORIENT (P-DS-04, #345).

Owns net.hadess.SensorProxy on the system bus (DBUS_SYSTEM_BUS_ADDRESS,
a private bus in the proof) and serves /net/hadess/SensorProxy with
iio-sensor-proxy's interface: HasAccelerometer and
AccelerometerOrientation (plus the light and proximity properties, all
absent), ClaimAccelerometer / ReleaseAccelerometer and the other claims.
Property changes emit org.freedesktop.DBus.Properties.PropertiesChanged,
as the real daemon does.

Writes "ready" once the name is owned and "claim SENDER" / "release
SENDER" for every accelerometer claim and release. Reads one command per
line on stdin, replying "ok" or "error ..." per line:

  orientation normal|bottom-up|left-up|right-up|undefined
  accel 1|0         an accelerometer appears / disappears
  quit
"""

import sys

from gi.repository import Gio, GLib

NAME = "net.hadess.SensorProxy"
PATH = "/net/hadess/SensorProxy"
INTERFACE = "net.hadess.SensorProxy"
ORIENTATIONS = ("normal", "bottom-up", "left-up", "right-up", "undefined")

XML = f"""
<node>
  <interface name="{INTERFACE}">
    <property name="HasAccelerometer" type="b" access="read"/>
    <property name="AccelerometerOrientation" type="s" access="read"/>
    <property name="HasAmbientLight" type="b" access="read"/>
    <property name="LightLevelUnit" type="s" access="read"/>
    <property name="LightLevel" type="d" access="read"/>
    <property name="HasProximity" type="b" access="read"/>
    <property name="ProximityNear" type="b" access="read"/>
    <method name="ClaimAccelerometer"/>
    <method name="ReleaseAccelerometer"/>
    <method name="ClaimLight"/>
    <method name="ReleaseLight"/>
    <method name="ClaimProximity"/>
    <method name="ReleaseProximity"/>
  </interface>
</node>
"""


def emit(line):
    print(line, flush=True)


class SensorProxy:
    def __init__(self, loop):
        self.loop = loop
        self.connection = None
        self.props = {
            "HasAccelerometer": GLib.Variant("b", True),
            "AccelerometerOrientation": GLib.Variant("s", "normal"),
            "HasAmbientLight": GLib.Variant("b", False),
            "LightLevelUnit": GLib.Variant("s", "lux"),
            "LightLevel": GLib.Variant("d", 0.0),
            "HasProximity": GLib.Variant("b", False),
            "ProximityNear": GLib.Variant("b", False),
        }

    def method_call(self, _conn, sender, _path, _iface, method, _params, invocation):
        if method == "ClaimAccelerometer":
            emit(f"claim {sender}")
        elif method == "ReleaseAccelerometer":
            emit(f"release {sender}")
        invocation.return_value(None)

    def get_property(self, _conn, _sender, _path, _iface, name):
        return self.props[name]

    def bus_acquired(self, connection, _name):
        self.connection = connection
        info = Gio.DBusNodeInfo.new_for_xml(XML).interfaces[0]
        connection.register_object(PATH, info, self.method_call, self.get_property, None)

    def name_acquired(self, _connection, _name):
        emit("ready")

    def name_lost(self, _connection, name):
        emit(f"error lost {name}")
        self.loop.quit()

    def set(self, name, value):
        if self.props[name].equal(value):
            return
        self.props[name] = value
        self.connection.emit_signal(
            None,
            PATH,
            "org.freedesktop.DBus.Properties",
            "PropertiesChanged",
            GLib.Variant("(sa{sv}as)", (INTERFACE, {name: value}, [])),
        )

    def run(self, words):
        cmd = words[0]
        if self.connection is None:
            raise ValueError("not on the bus yet")
        if cmd == "orientation":
            if words[1] not in ORIENTATIONS:
                raise ValueError(f"unknown orientation {words[1]}")
            self.set("AccelerometerOrientation", GLib.Variant("s", words[1]))
        elif cmd == "accel":
            self.set("HasAccelerometer", GLib.Variant("b", words[1] == "1"))
        else:
            raise ValueError(f"unknown command {cmd}")

    def stdin_ready(self, channel, _condition):
        # The channel buffers: handle every complete line it holds, not
        # just the one that made the descriptor readable.
        while True:
            line = channel.readline()
            if not line:
                # End of input: the driver closed the pipe.
                self.loop.quit()
                return False
            words = line.split()
            if words and words[0] == "quit":
                self.loop.quit()
                return False
            if words:
                try:
                    self.run(words)
                    emit("ok")
                except Exception as error:  # reported to the driver, never fatal
                    emit(f"error {error}")
            if not channel.get_buffer_condition() & GLib.IOCondition.IN:
                return True


def main():
    loop = GLib.MainLoop()
    proxy = SensorProxy(loop)
    Gio.bus_own_name(
        Gio.BusType.SYSTEM,
        NAME,
        Gio.BusNameOwnerFlags.NONE,
        proxy.bus_acquired,
        proxy.name_acquired,
        proxy.name_lost,
    )
    GLib.io_add_watch(
        GLib.IOChannel.unix_new(sys.stdin.fileno()),
        GLib.PRIORITY_DEFAULT,
        GLib.IOCondition.IN | GLib.IOCondition.HUP | GLib.IOCondition.ERR,
        proxy.stdin_ready,
    )
    loop.run()


if __name__ == "__main__":
    main()
