#!/usr/bin/python3
"""A minimal MPRIS player for the GTK shell proof's G-GSD-MPRIS.

Usage: tuna-mpris-player-stub.py MARKER [SECONDS]

Owns org.mpris.MediaPlayer2.tunaproof and serves /org/mpris/MediaPlayer2
with org.mpris.MediaPlayer2 and org.mpris.MediaPlayer2.Player: the
methods and properties gsd-media-keys' mpris-controller.c reads or calls
(PlayPause/Play/Pause/Next/Previous/Stop, Seek, PlaybackStatus,
LoopStatus, Shuffle). MARKER gets "ready" once the name is owned,
"properties SENDER" the first time a peer reads its properties (gsd's
proxy loading), and "call METHOD SENDER" for every method call. Exits
after SECONDS (default 300).
"""
import sys

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

NAME = "org.mpris.MediaPlayer2.tunaproof"
PATH = "/org/mpris/MediaPlayer2"
PLAYER = "org.mpris.MediaPlayer2.Player"
XML = """
<node>
  <interface name="org.mpris.MediaPlayer2">
    <method name="Raise"/>
    <method name="Quit"/>
    <property name="CanQuit" type="b" access="read"/>
    <property name="CanRaise" type="b" access="read"/>
    <property name="HasTrackList" type="b" access="read"/>
    <property name="Identity" type="s" access="read"/>
    <property name="DesktopEntry" type="s" access="read"/>
    <property name="SupportedUriSchemes" type="as" access="read"/>
    <property name="SupportedMimeTypes" type="as" access="read"/>
  </interface>
  <interface name="org.mpris.MediaPlayer2.Player">
    <method name="Next"/>
    <method name="Previous"/>
    <method name="Pause"/>
    <method name="PlayPause"/>
    <method name="Stop"/>
    <method name="Play"/>
    <method name="Seek"><arg type="x" direction="in" name="Offset"/></method>
    <method name="SetPosition">
      <arg type="o" direction="in" name="TrackId"/>
      <arg type="x" direction="in" name="Position"/>
    </method>
    <method name="OpenUri"><arg type="s" direction="in" name="Uri"/></method>
    <signal name="Seeked"><arg type="x" name="Position"/></signal>
    <property name="PlaybackStatus" type="s" access="read"/>
    <property name="LoopStatus" type="s" access="readwrite"/>
    <property name="Rate" type="d" access="readwrite"/>
    <property name="Shuffle" type="b" access="readwrite"/>
    <property name="Metadata" type="a{sv}" access="read"/>
    <property name="Volume" type="d" access="readwrite"/>
    <property name="Position" type="x" access="read"/>
    <property name="MinimumRate" type="d" access="read"/>
    <property name="MaximumRate" type="d" access="read"/>
    <property name="CanGoNext" type="b" access="read"/>
    <property name="CanGoPrevious" type="b" access="read"/>
    <property name="CanPlay" type="b" access="read"/>
    <property name="CanPause" type="b" access="read"/>
    <property name="CanSeek" type="b" access="read"/>
    <property name="CanControl" type="b" access="read"/>
  </interface>
</node>
"""

MARKER = sys.argv[1]
SECONDS = int(sys.argv[2]) if len(sys.argv) > 2 else 300
state = {"PlaybackStatus": "Paused", "LoopStatus": "None", "Shuffle": False,
         "Rate": 1.0, "Volume": 1.0}
readers = set()


def note(line):
    with open(MARKER, "a", encoding="utf-8") as fh:
        fh.write(line + "\n")


def value(name):
    fixed = {
        "CanQuit": GLib.Variant("b", False), "CanRaise": GLib.Variant("b", False),
        "HasTrackList": GLib.Variant("b", False),
        "Identity": GLib.Variant("s", "Tuna Proof Player"),
        "DesktopEntry": GLib.Variant("s", "tuna-proof-player"),
        "SupportedUriSchemes": GLib.Variant("as", []),
        "SupportedMimeTypes": GLib.Variant("as", []),
        "Metadata": GLib.Variant("a{sv}", {
            "mpris:trackid": GLib.Variant("o", "/org/tuna/proof/track/1"),
            "xesam:title": GLib.Variant("s", "Tuna Proof Track"),
        }),
        "Position": GLib.Variant("x", 0),
        "MinimumRate": GLib.Variant("d", 1.0), "MaximumRate": GLib.Variant("d", 1.0),
    }
    if name in fixed:
        return fixed[name]
    if name in ("PlaybackStatus", "LoopStatus"):
        return GLib.Variant("s", state[name])
    if name == "Shuffle":
        return GLib.Variant("b", state[name])
    if name in ("Rate", "Volume"):
        return GLib.Variant("d", state[name])
    return GLib.Variant("b", True)  # CanGoNext, CanGoPrevious, CanPlay, ...


def set_status(conn, status):
    state["PlaybackStatus"] = status
    conn.emit_signal(None, PATH, "org.freedesktop.DBus.Properties", "PropertiesChanged",
                     GLib.Variant("(sa{sv}as)", (PLAYER, {
                         "PlaybackStatus": GLib.Variant("s", status)}, [])))


def on_call(conn, sender, _path, _iface, method, _params, invocation):
    note(f"call {method} {sender}")
    status = state["PlaybackStatus"]
    transitions = {
        "PlayPause": "Paused" if status == "Playing" else "Playing",
        "Play": "Playing", "Pause": "Paused", "Stop": "Stopped",
    }
    invocation.return_value(None)
    if method in transitions and transitions[method] != status:
        set_status(conn, transitions[method])


def on_get(_conn, sender, _path, _iface, name):
    if sender not in readers:
        readers.add(sender)
        note(f"properties {sender}")
    return value(name)


def on_set(_conn, _sender, _path, _iface, name, variant):
    if name in state:
        state[name] = variant.unpack()
    return True


def on_bus(conn, _name):
    for iface in Gio.DBusNodeInfo.new_for_xml(XML).interfaces:
        conn.register_object(PATH, iface, on_call, on_get, on_set)


Gio.bus_own_name(Gio.BusType.SESSION, NAME, Gio.BusNameOwnerFlags.NONE,
                 on_bus, lambda *_: note("ready"), lambda *_: note("lost"))
loop = GLib.MainLoop()
GLib.timeout_add_seconds(SECONDS, loop.quit)
loop.run()
