#!/usr/bin/env python3
"""A controllable fprintd for scripts/tuna-lock-auth-proof (#366, P-LK-02).

Usage: tuna-fprintd-mock.py --address BUS --log FILE.jsonl
                            [--user NAME] [--no-enrolled] [--scan-type press|swipe]

Owns net.reactivated.Fprint on the private bus at BUS (never the host's
system bus) and serves fprintd's D-Bus API with fprintd's shapes and
error names
(https://gitlab.freedesktop.org/libfprint/fprintd/-/blob/master/src/net.reactivated.Fprint.Device.xml,
https://gitlab.freedesktop.org/libfprint/fprintd/-/blob/master/src/net.reactivated.Fprint.Manager.xml,
https://gitlab.freedesktop.org/libfprint/fprintd/-/blob/master/src/device.c):

  /net/reactivated/Fprint/Manager   net.reactivated.Fprint.Manager
      GetDefaultDevice() -> o, GetDevices() -> ao
  /net/reactivated/Fprint/Device/0  net.reactivated.Fprint.Device
      ListEnrolledFingers(s) -> as, Claim(s), Release(),
      VerifyStart(s), VerifyStop(), signals VerifyFingerSelected(s) and
      VerifyStatus(s result, b done), properties name, num-enroll-stages,
      scan-type (press|swipe), finger-present, finger-needed

Like fprintd: the device has one claim, owned by the claiming D-Bus
sender; a second Claim is AlreadyInUse; VerifyStart/VerifyStop/Release
need the owner's claim (ClaimDevice otherwise, AlreadyInUse for another
sender); a finished verification stays the current action until
VerifyStop; Release cancels a running verification; and the claim is
released when its owner leaves the bus (NameOwnerChanged).

The proof drives the "finger" through a test-only interface on the
device object, net.reactivated.Fprint.Test:
      Press(s result, b done)  emit VerifyStatus while verifying
                               (error ...Test.Error.NotVerifying otherwise)
      SetEnrolled(b), SetScanType(s), SetPresent(b), State() -> s (JSON)

Every method call, emitted signal and automatic release is appended to
FILE.jsonl as one JSON object (kind, interface, member, sender, args,
outcome, wall and monotonic time), so the proof can assert the
Claim -> VerifyStart -> VerifyStop -> Release order. No secrets pass
through fprintd's API. On readiness one line is printed to stdout:
    tuna-fprintd-mock: ready <unique bus name>
"""
import argparse
import getpass
import json
import os
import signal
import sys
import time

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

BUS_NAME = "net.reactivated.Fprint"
MANAGER_PATH = "/net/reactivated/Fprint/Manager"
DEVICE_PATH = "/net/reactivated/Fprint/Device/0"
MANAGER_IFACE = "net.reactivated.Fprint.Manager"
DEVICE_IFACE = "net.reactivated.Fprint.Device"
TEST_IFACE = "net.reactivated.Fprint.Test"
ERROR = "net.reactivated.Fprint.Error."
TEST_ERROR = "net.reactivated.Fprint.Test.Error."
SCAN_TYPES = ("press", "swipe")

INTROSPECTION = f"""
<node>
  <interface name="{MANAGER_IFACE}">
    <method name="GetDevices"><arg type="ao" name="devices" direction="out"/></method>
    <method name="GetDefaultDevice"><arg type="o" name="device" direction="out"/></method>
  </interface>
  <interface name="{DEVICE_IFACE}">
    <method name="ListEnrolledFingers">
      <arg type="s" name="username" direction="in"/>
      <arg type="as" name="enrolled_fingers" direction="out"/>
    </method>
    <method name="Claim"><arg type="s" name="username" direction="in"/></method>
    <method name="Release"/>
    <method name="VerifyStart"><arg type="s" name="finger_name" direction="in"/></method>
    <method name="VerifyStop"/>
    <signal name="VerifyFingerSelected"><arg type="s" name="finger_name"/></signal>
    <signal name="VerifyStatus"><arg type="s" name="result"/><arg type="b" name="done"/></signal>
    <property name="name" type="s" access="read"/>
    <property name="num-enroll-stages" type="i" access="read"/>
    <property name="scan-type" type="s" access="read"/>
    <property name="finger-present" type="b" access="read"/>
    <property name="finger-needed" type="b" access="read"/>
  </interface>
  <interface name="{TEST_IFACE}">
    <method name="Press">
      <arg type="s" name="result" direction="in"/>
      <arg type="b" name="done" direction="in"/>
    </method>
    <method name="SetEnrolled"><arg type="b" name="enrolled" direction="in"/></method>
    <method name="SetScanType"><arg type="s" name="scan_type" direction="in"/></method>
    <method name="SetPresent"><arg type="b" name="present" direction="in"/></method>
    <method name="State"><arg type="s" name="state" direction="out"/></method>
  </interface>
</node>
"""


class Refused(Exception):
    """A D-Bus error reply: (error name, message)."""

    def __init__(self, name, message):
        super().__init__(message)
        self.name = name
        self.message = message


def plain(value):
    """JSON form of a GLib.Variant inside a logged signal."""
    if isinstance(value, GLib.Variant):
        return value.unpack()
    raise TypeError(type(value).__name__)


class MockFprintd:
    def __init__(self, conn, log_path, user, enrolled, scan_type):
        self.conn = conn
        self.log = open(log_path, "a", buffering=1, encoding="utf-8")
        self.user = user
        self.enrolled = ["right-index-finger"] if enrolled else []
        self.scan_type = scan_type
        self.present = True
        # The claim: {"sender": unique name, "username": str} or None.
        self.claim = None
        # None, or "verify" (running or finished until VerifyStop).
        self.action = None
        self.verify_done = False
        self.counts = {}

    # --- bookkeeping -------------------------------------------------

    def record(self, **entry):
        entry["wall"] = time.time()
        entry["mono"] = time.monotonic()
        self.log.write(json.dumps(entry, sort_keys=True, default=plain) + "\n")

    def state(self):
        return {
            "present": self.present,
            "enrolled": list(self.enrolled),
            "scan_type": self.scan_type,
            "claimed": self.claim is not None,
            "claim_owner": self.claim["sender"] if self.claim else None,
            "action": self.action,
            "verify_done": self.verify_done,
            "counts": dict(self.counts),
        }

    def emit(self, iface, member, signature, values):
        self.conn.emit_signal(None, DEVICE_PATH, iface, member,
                              GLib.Variant(signature, values))
        self.record(kind="signal", interface=iface, member=member, args=list(values))

    def properties_changed(self, changed):
        self.emit("org.freedesktop.DBus.Properties", "PropertiesChanged", "(sa{sv}as)",
                  (DEVICE_IFACE, {key: value for key, value in changed.items()}, []))

    def set_finger_needed(self, before):
        if before != self.finger_needed():
            self.properties_changed({"finger-needed": GLib.Variant("b", self.finger_needed())})

    def finger_needed(self):
        return self.action == "verify" and not self.verify_done

    def own_user(self, username):
        return username in ("", self.user)

    def require_claim(self, sender):
        if self.claim is None:
            raise Refused(ERROR + "ClaimDevice", "Device was not claimed before use")
        if self.claim["sender"] != sender:
            raise Refused(ERROR + "AlreadyInUse", "Device already in use by another user")

    def stop_verify(self):
        before = self.finger_needed()
        self.action = None
        self.verify_done = False
        self.set_finger_needed(before)

    # --- D-Bus entry points ------------------------------------------

    def method_call(self, conn, sender, path, iface, member, params, invocation):
        args = params.unpack() if params is not None else ()
        self.counts[member] = self.counts.get(member, 0) + 1
        try:
            handler = {
                MANAGER_IFACE: self.manager,
                DEVICE_IFACE: self.device,
                TEST_IFACE: self.test,
            }.get(iface)
            if handler is None:
                raise Refused("org.freedesktop.DBus.Error.UnknownInterface", iface)
            reply = handler(sender, path, member, args)
        except Refused as refused:
            self.record(kind="call", interface=iface, member=member, sender=sender,
                        args=list(args), outcome=refused.name)
            invocation.return_dbus_error(refused.name, refused.message)
            return
        self.record(kind="call", interface=iface, member=member, sender=sender,
                    args=list(args), outcome="ok")
        invocation.return_value(reply)

    def get_property(self, conn, sender, path, iface, name):
        values = {
            "name": GLib.Variant("s", "Tuna Proof Fingerprint Reader"),
            "num-enroll-stages": GLib.Variant("i", 5),
            "scan-type": GLib.Variant("s", self.scan_type),
            "finger-present": GLib.Variant("b", False),
            "finger-needed": GLib.Variant("b", self.finger_needed()),
        }
        self.record(kind="property", interface=iface, member=name, sender=sender)
        return values.get(name)

    def manager(self, sender, path, member, args):
        if path != MANAGER_PATH:
            raise Refused("org.freedesktop.DBus.Error.UnknownObject", path)
        if member == "GetDevices":
            return GLib.Variant("(ao)", ([DEVICE_PATH] if self.present else [],))
        if member == "GetDefaultDevice":
            if not self.present:
                raise Refused(ERROR + "NoSuchDevice", "No devices available")
            return GLib.Variant("(o)", (DEVICE_PATH,))
        raise Refused("org.freedesktop.DBus.Error.UnknownMethod", member)

    def device(self, sender, path, member, args):
        if path != DEVICE_PATH or not self.present:
            raise Refused("org.freedesktop.DBus.Error.UnknownObject", path)
        if member == "ListEnrolledFingers":
            (username,) = args
            if not self.own_user(username):
                raise Refused(ERROR + "PermissionDenied", "Not Authorized: net.reactivated.fprint.device.setusername")
            if not self.enrolled:
                raise Refused(ERROR + "NoEnrolledPrints", "Failed to discover prints")
            return GLib.Variant("(as)", (list(self.enrolled),))
        if member == "Claim":
            (username,) = args
            if self.claim is not None:
                raise Refused(ERROR + "AlreadyInUse", "Device was already claimed")
            if not self.own_user(username):
                raise Refused(ERROR + "PermissionDenied", "Not Authorized: net.reactivated.fprint.device.setusername")
            self.claim = {"sender": sender, "username": username or self.user}
            return None
        if member == "Release":
            self.require_claim(sender)
            if self.action is not None:
                # fprintd cancels the running action before closing.
                self.record(kind="auto", member="ReleaseStopsVerify", sender=sender)
                self.stop_verify()
            self.claim = None
            return None
        if member == "VerifyStart":
            (finger,) = args
            self.require_claim(sender)
            if self.action is not None:
                raise Refused(ERROR + "AlreadyInUse", "Verification already in progress")
            if not self.enrolled:
                raise Refused(ERROR + "NoEnrolledPrints", "No fingerprints enrolled")
            if finger not in ("any", "") and finger not in self.enrolled:
                raise Refused(ERROR + "NoEnrolledPrints", f"Fingerprint for finger {finger} is not enrolled")
            before = self.finger_needed()
            self.action = "verify"
            self.verify_done = False
            # Reply first, then announce the selection, as fprintd does.
            GLib.idle_add(self.announce_selected, finger or "any")
            self.set_finger_needed(before)
            return None
        if member == "VerifyStop":
            self.require_claim(sender)
            if self.action != "verify":
                raise Refused(ERROR + "NoActionInProgress", "No verification in progress")
            self.stop_verify()
            return None
        raise Refused("org.freedesktop.DBus.Error.UnknownMethod", member)

    def announce_selected(self, finger):
        if self.action == "verify":
            self.emit(DEVICE_IFACE, "VerifyFingerSelected", "(s)", (finger,))
        return GLib.SOURCE_REMOVE

    def test(self, sender, path, member, args):
        if path != DEVICE_PATH:
            raise Refused("org.freedesktop.DBus.Error.UnknownObject", path)
        if member == "Press":
            result, done = args
            if self.action != "verify" or self.verify_done:
                raise Refused(TEST_ERROR + "NotVerifying", "No verification is waiting for a finger")
            before = self.finger_needed()
            if done:
                self.verify_done = True
            self.emit(DEVICE_IFACE, "VerifyStatus", "(sb)", (result, bool(done)))
            self.set_finger_needed(before)
            return None
        if member == "SetEnrolled":
            (enrolled,) = args
            self.enrolled = ["right-index-finger"] if enrolled else []
            return None
        if member == "SetScanType":
            (scan_type,) = args
            if scan_type not in SCAN_TYPES:
                raise Refused("org.freedesktop.DBus.Error.InvalidArgs", scan_type)
            self.scan_type = scan_type
            self.properties_changed({"scan-type": GLib.Variant("s", scan_type)})
            return None
        if member == "SetPresent":
            (present,) = args
            self.present = bool(present)
            return None
        if member == "State":
            return GLib.Variant("(s)", (json.dumps(self.state(), sort_keys=True),))
        raise Refused("org.freedesktop.DBus.Error.UnknownMethod", member)

    def name_owner_changed(self, conn, sender, path, iface, member, params):
        name, old, new = params.unpack()
        if self.claim is None or name != self.claim["sender"] or new:
            return
        # fprintd's _fprint_device_client_vanished: drop the claim.
        self.record(kind="auto", member="ClientVanished", sender=name,
                    args=[self.action is not None])
        if self.action is not None:
            self.stop_verify()
        self.claim = None


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--address", required=True,
                        help="private bus address (never the host system bus)")
    parser.add_argument("--log", required=True, help="JSON-lines call log")
    parser.add_argument("--user", default=getpass.getuser())
    parser.add_argument("--no-enrolled", action="store_true")
    parser.add_argument("--scan-type", default="press", choices=SCAN_TYPES)
    args = parser.parse_args()

    conn = Gio.DBusConnection.new_for_address_sync(
        args.address,
        Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT
        | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION,
        None, None)
    mock = MockFprintd(conn, args.log, args.user, not args.no_enrolled, args.scan_type)
    node = Gio.DBusNodeInfo.new_for_xml(INTROSPECTION)
    for path, iface in ((MANAGER_PATH, MANAGER_IFACE), (DEVICE_PATH, DEVICE_IFACE),
                        (DEVICE_PATH, TEST_IFACE)):
        conn.register_object(path, node.lookup_interface(iface),
                             mock.method_call, mock.get_property, None)
    conn.signal_subscribe("org.freedesktop.DBus", "org.freedesktop.DBus",
                          "NameOwnerChanged", "/org/freedesktop/DBus", None,
                          Gio.DBusSignalFlags.NONE, mock.name_owner_changed)
    reply = conn.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus",
                           "org.freedesktop.DBus", "RequestName",
                           GLib.Variant("(su)", (BUS_NAME, 4)),  # DO_NOT_QUEUE
                           GLib.VariantType("(u)"), Gio.DBusCallFlags.NONE, 5000, None)
    if reply.unpack()[0] != 1:  # PRIMARY_OWNER
        print("tuna-fprintd-mock: net.reactivated.Fprint is already owned", file=sys.stderr)
        return 1
    mock.record(kind="ready", member="RequestName", sender=conn.get_unique_name(),
                args=[os.getpid()])

    loop = GLib.MainLoop()
    conn.connect("closed", lambda *_: loop.quit())
    for signum in (signal.SIGTERM, signal.SIGINT):
        GLib.unix_signal_add(GLib.PRIORITY_DEFAULT, signum, lambda: (loop.quit(), False)[1])
    print(f"tuna-fprintd-mock: ready {conn.get_unique_name()}", flush=True)
    loop.run()
    mock.record(kind="exit", member="exit", args=[mock.state()])
    return 0


if __name__ == "__main__":
    sys.exit(main())
