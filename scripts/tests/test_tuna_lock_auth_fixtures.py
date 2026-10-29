#!/usr/bin/env python3
"""Runtime self-tests of the lock-auth proof fixtures (#366, P-LK-02).

Drives scripts/lib/tuna-fprintd-mock.py on a private dbus-daemon with
real D-Bus clients, and scripts/lib/tuna-pam-otp.c compiled and stacked
behind pam_matrix under pam_wrapper, through libpam's own
pam_authenticate. These prove the fixtures the nested proof
(scripts/tuna-lock-auth-proof) relies on; they make no product claim.

Run with the distro Python (python3-gi, ctypes over the system libpam):
    /usr/bin/python3 scripts/tests/test_tuna_lock_auth_fixtures.py
"""
import json
import os
import pathlib
import secrets
import shutil
import subprocess
import sys
import tempfile
import time
import unittest

import gi

gi.require_version("Gio", "2.0")
from gi.repository import Gio, GLib  # noqa: E402

ROOT = pathlib.Path(__file__).resolve().parents[2]
MOCK = ROOT / "scripts/lib/tuna-fprintd-mock.py"
OTP_SOURCE = ROOT / "scripts/lib/tuna-pam-otp.c"
PAM_WRAPPER = pathlib.Path("/usr/lib/x86_64-linux-gnu/libpam_wrapper.so")
PAM_MATRIX = pathlib.Path("/usr/lib/x86_64-linux-gnu/pam_wrapper/pam_matrix.so")
DEVICE = "/net/reactivated/Fprint/Device/0"
MANAGER = "/net/reactivated/Fprint/Manager"
BUS_CONFIG = """<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>tuna-proof-system</type>
  <listen>unix:path={socket}</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow user="*"/>
    <allow own="*"/>
    <allow send_type="method_call"/>
    <allow send_type="signal"/>
    <allow send_type="method_return"/>
    <allow send_type="error"/>
    <allow receive_type="method_call"/>
    <allow receive_type="method_return"/>
    <allow receive_type="error"/>
    <allow receive_type="signal"/>
  </policy>
</busconfig>
"""


def wait_until(predicate, timeout=5.0, step=0.02):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if predicate():
            return True
        time.sleep(step)
    return predicate()


class FprintdMock(unittest.TestCase):
    def setUp(self):
        self.tmp = pathlib.Path(tempfile.mkdtemp(prefix="tuna-fp-mock-"))
        (self.tmp / "bus.conf").write_text(BUS_CONFIG.format(socket=self.tmp / "bus"))
        self.bus = subprocess.Popen(
            ["dbus-daemon", "--config-file", str(self.tmp / "bus.conf"), "--nofork",
             "--print-address=1"], stdout=subprocess.PIPE, text=True)
        self.address = self.bus.stdout.readline().strip()
        self.assertTrue(self.address.startswith("unix:path="), self.address)
        self.log = self.tmp / "calls.jsonl"
        self.mock = self.start_mock()
        self.clients = []

    def start_mock(self, *extra):
        mock = subprocess.Popen(
            [sys.executable, str(MOCK), "--address", self.address, "--log", str(self.log),
             "--user", "tuna-proof-user", *extra],
            stdout=subprocess.PIPE, text=True)
        line = mock.stdout.readline()
        self.assertTrue(line.startswith("tuna-fprintd-mock: ready :"), line)
        return mock

    def tearDown(self):
        for client in self.clients:
            if not client.is_closed():
                client.close_sync(None)
        for process in (self.mock, self.bus):
            process.terminate()
            process.wait(timeout=5)
        shutil.rmtree(self.tmp)

    def client(self):
        conn = Gio.DBusConnection.new_for_address_sync(
            self.address,
            Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT
            | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)
        self.clients.append(conn)
        return conn

    def call(self, conn, member, args=None, path=DEVICE, iface="net.reactivated.Fprint.Device"):
        reply = conn.call_sync("net.reactivated.Fprint", path, iface, member, args, None,
                               Gio.DBusCallFlags.NONE, 5000, None)
        return reply.unpack() if reply is not None else None

    def refused(self, conn, member, args=None, **kw):
        with self.assertRaises(GLib.Error) as raised:
            self.call(conn, member, args, **kw)
        return Gio.DBusError.get_remote_error(raised.exception)

    def press(self, conn, result, done):
        return self.call(conn, "Press", GLib.Variant("(sb)", (result, done)),
                         iface="net.reactivated.Fprint.Test")

    def state(self, conn):
        return json.loads(self.call(conn, "State", iface="net.reactivated.Fprint.Test")[0])

    def entries(self):
        return [json.loads(line) for line in self.log.read_text().splitlines()]

    def test_manager_device_and_properties_have_fprintd_shapes(self):
        conn = self.client()
        self.assertEqual(self.call(conn, "GetDefaultDevice", path=MANAGER,
                                   iface="net.reactivated.Fprint.Manager"), (DEVICE,))
        self.assertEqual(self.call(conn, "GetDevices", path=MANAGER,
                                   iface="net.reactivated.Fprint.Manager"), ([DEVICE],))
        for user in ("", "tuna-proof-user"):
            self.assertEqual(self.call(conn, "ListEnrolledFingers", GLib.Variant("(s)", (user,))),
                             (["right-index-finger"],))
        self.assertEqual(
            self.refused(conn, "ListEnrolledFingers", GLib.Variant("(s)", ("someone-else",))),
            "net.reactivated.Fprint.Error.PermissionDenied")
        props = self.call(conn, "GetAll", GLib.Variant("(s)", ("net.reactivated.Fprint.Device",)),
                          iface="org.freedesktop.DBus.Properties")[0]
        self.assertEqual(props["scan-type"], "press")
        self.assertEqual(set(props), {"name", "num-enroll-stages", "scan-type",
                                      "finger-present", "finger-needed"})
        self.call(conn, "SetScanType", GLib.Variant("(s)", ("swipe",)),
                  iface="net.reactivated.Fprint.Test")
        self.assertEqual(self.call(conn, "Get", GLib.Variant("(ss)", (
            "net.reactivated.Fprint.Device", "scan-type")),
            iface="org.freedesktop.DBus.Properties")[0], "swipe")
        self.assertEqual(self.refused(conn, "SetScanType", GLib.Variant("(s)", ("wave",)),
                                      iface="net.reactivated.Fprint.Test"),
                         "org.freedesktop.DBus.Error.InvalidArgs")

    def test_enrollment_and_presence_controls(self):
        conn = self.client()
        self.call(conn, "SetEnrolled", GLib.Variant("(b)", (False,)), iface="net.reactivated.Fprint.Test")
        self.assertEqual(self.refused(conn, "ListEnrolledFingers", GLib.Variant("(s)", ("",))),
                         "net.reactivated.Fprint.Error.NoEnrolledPrints")
        self.call(conn, "Claim", GLib.Variant("(s)", ("",)))
        self.assertEqual(self.refused(conn, "VerifyStart", GLib.Variant("(s)", ("any",))),
                         "net.reactivated.Fprint.Error.NoEnrolledPrints")
        self.call(conn, "Release")
        self.call(conn, "SetPresent", GLib.Variant("(b)", (False,)), iface="net.reactivated.Fprint.Test")
        self.assertEqual(self.refused(conn, "GetDefaultDevice", path=MANAGER,
                                      iface="net.reactivated.Fprint.Manager"),
                         "net.reactivated.Fprint.Error.NoSuchDevice")
        self.assertEqual(self.call(conn, "GetDevices", path=MANAGER,
                                   iface="net.reactivated.Fprint.Manager"), ([],))

    def test_started_without_prints(self):
        self.mock.terminate()
        self.mock.wait(timeout=5)
        self.mock = self.start_mock("--no-enrolled", "--scan-type", "swipe")
        conn = self.client()
        self.assertEqual(self.refused(conn, "ListEnrolledFingers", GLib.Variant("(s)", ("",))),
                         "net.reactivated.Fprint.Error.NoEnrolledPrints")
        self.assertEqual(self.state(conn)["scan_type"], "swipe")

    def test_claim_verify_signals_and_errors_follow_fprintd(self):
        owner, other = self.client(), self.client()
        signals = []
        owner.signal_subscribe("net.reactivated.Fprint", "net.reactivated.Fprint.Device", None,
                               DEVICE, None, Gio.DBusSignalFlags.NONE,
                               lambda c, s, p, i, member, params: signals.append((member, params.unpack())))
        context = GLib.MainContext.default()

        def pump_for(count):
            return wait_until(lambda: context.iteration(False) or len(signals) >= count)

        self.assertEqual(self.refused(owner, "VerifyStart", GLib.Variant("(s)", ("any",))),
                         "net.reactivated.Fprint.Error.ClaimDevice")
        self.assertEqual(self.refused(owner, "Release"), "net.reactivated.Fprint.Error.ClaimDevice")
        self.call(owner, "Claim", GLib.Variant("(s)", ("",)))
        self.assertEqual(self.refused(other, "Claim", GLib.Variant("(s)", ("",))),
                         "net.reactivated.Fprint.Error.AlreadyInUse")
        self.assertEqual(self.refused(other, "VerifyStart", GLib.Variant("(s)", ("any",))),
                         "net.reactivated.Fprint.Error.AlreadyInUse")
        self.assertEqual(self.refused(owner, "VerifyStop"),
                         "net.reactivated.Fprint.Error.NoActionInProgress")
        self.assertEqual(self.refused(owner, "Press", GLib.Variant("(sb)", ("verify-match", True)),
                                      iface="net.reactivated.Fprint.Test"),
                         "net.reactivated.Fprint.Test.Error.NotVerifying")
        self.call(owner, "VerifyStart", GLib.Variant("(s)", ("any",)))
        self.assertTrue(pump_for(1))
        self.assertEqual(signals[0], ("VerifyFingerSelected", ("any",)))
        self.assertEqual(self.refused(owner, "VerifyStart", GLib.Variant("(s)", ("any",))),
                         "net.reactivated.Fprint.Error.AlreadyInUse")
        self.press(other, "verify-retry-scan", False)
        self.press(other, "verify-no-match", True)
        self.assertTrue(pump_for(3))
        self.assertEqual(signals[1:3], [("VerifyStatus", ("verify-retry-scan", False)),
                                        ("VerifyStatus", ("verify-no-match", True))])
        # Finished, but still the current action until VerifyStop.
        self.assertEqual(self.refused(owner, "Press", GLib.Variant("(sb)", ("verify-match", True)),
                                      iface="net.reactivated.Fprint.Test"),
                         "net.reactivated.Fprint.Test.Error.NotVerifying")
        self.assertEqual(self.refused(owner, "VerifyStart", GLib.Variant("(s)", ("any",))),
                         "net.reactivated.Fprint.Error.AlreadyInUse")
        self.call(owner, "VerifyStop")
        self.call(owner, "VerifyStart", GLib.Variant("(s)", ("any",)))
        self.press(other, "verify-match", True)
        self.assertTrue(pump_for(5))
        self.assertEqual(signals[4], ("VerifyStatus", ("verify-match", True)))
        self.call(owner, "VerifyStop")
        self.call(owner, "Release")
        self.assertFalse(self.state(other)["claimed"])
        order = [e["member"] for e in self.entries()
                 if e["kind"] == "call" and e["outcome"] == "ok"
                 and e["member"] in ("Claim", "VerifyStart", "VerifyStop", "Release")]
        self.assertEqual(order, ["Claim", "VerifyStart", "VerifyStop", "VerifyStart", "VerifyStop", "Release"])
        statuses = [e["args"] for e in self.entries() if e["kind"] == "signal" and e["member"] == "VerifyStatus"]
        self.assertEqual(statuses, [["verify-retry-scan", False], ["verify-no-match", True], ["verify-match", True]])

    def test_release_cancels_a_running_verification(self):
        conn = self.client()
        self.call(conn, "Claim", GLib.Variant("(s)", ("",)))
        self.call(conn, "VerifyStart", GLib.Variant("(s)", ("any",)))
        self.call(conn, "Release")
        state = self.state(conn)
        self.assertEqual((state["claimed"], state["action"]), (False, None))
        self.assertIn("ReleaseStopsVerify", [e["member"] for e in self.entries() if e["kind"] == "auto"])

    def test_vanished_owner_releases_the_claim(self):
        owner, watcher = self.client(), self.client()
        self.call(owner, "Claim", GLib.Variant("(s)", ("",)))
        self.call(owner, "VerifyStart", GLib.Variant("(s)", ("any",)))
        owner_name = owner.get_unique_name()
        owner.close_sync(None)
        self.assertTrue(wait_until(lambda: not self.state(watcher)["claimed"]))
        vanished = [e for e in self.entries() if e["kind"] == "auto" and e["member"] == "ClientVanished"]
        self.assertEqual([(e["sender"], e["args"]) for e in vanished], [(owner_name, [True])])
        self.call(watcher, "Claim", GLib.Variant("(s)", ("",)))

    def test_second_owner_is_refused(self):
        second = subprocess.run(
            [sys.executable, str(MOCK), "--address", self.address, "--log", str(self.tmp / "second.jsonl")],
            capture_output=True, text=True, timeout=10)
        self.assertEqual(second.returncode, 1)
        self.assertIn("already owned", second.stderr)


PAM_HARNESS = r"""
import ctypes, json, sys
service, user, answers = sys.argv[1], sys.argv[2], sys.argv[3:]
ctypes.CDLL("libpam.so.0", mode=ctypes.RTLD_GLOBAL)
scope = ctypes.CDLL(None)  # LD_PRELOAD (pam_wrapper) first, as for any PAM application
libc = ctypes.CDLL("libc.so.6")
libc.calloc.restype = ctypes.c_void_p
libc.strdup.restype = ctypes.c_void_p
libc.strdup.argtypes = [ctypes.c_char_p]
class Message(ctypes.Structure):
    _fields_ = [("style", ctypes.c_int), ("msg", ctypes.c_char_p)]
class Response(ctypes.Structure):
    _fields_ = [("resp", ctypes.c_void_p), ("retcode", ctypes.c_int)]
CONV = ctypes.CFUNCTYPE(ctypes.c_int, ctypes.c_int, ctypes.POINTER(ctypes.POINTER(Message)),
                        ctypes.POINTER(ctypes.POINTER(Response)), ctypes.c_void_p)
class Conv(ctypes.Structure):
    _fields_ = [("conv", CONV), ("appdata", ctypes.c_void_p)]
seen = []
def converse(count, messages, out, _):
    block = libc.calloc(count, ctypes.sizeof(Response))
    replies = ctypes.cast(block, ctypes.POINTER(Response))
    for i in range(count):
        style, text = messages[i].contents.style, messages[i].contents.msg.decode()
        seen.append([style, text])
        if style in (1, 2):  # PAM_PROMPT_ECHO_OFF / ECHO_ON
            if not answers:
                return 19  # PAM_CONV_ERR
            replies[i].resp = libc.strdup(answers.pop(0).encode())
    out[0] = replies
    return 0
conv = Conv(CONV(converse), None)
handle = ctypes.c_void_p()
assert scope.pam_start(service.encode(), user.encode(), ctypes.byref(conv), ctypes.byref(handle)) == 0
auth = scope.pam_authenticate(handle, 0)
account = scope.pam_acct_mgmt(handle, 0) if auth == 0 else None
scope.pam_end(handle, auth)
print(json.dumps({"auth": auth, "account": account, "messages": seen, "unused": len(answers)}))
"""


PAM_PRESENT = PAM_WRAPPER.exists() and PAM_MATRIX.exists()


@unittest.skipUnless(PAM_PRESENT or os.environ.get("TUNA_REQUIRE_PAM_WRAPPER"),
                     "pam_wrapper/pam_matrix not installed")
class OtpPamModule(unittest.TestCase):
    PYTHON = "/usr/bin/python3"

    @classmethod
    def setUpClass(cls):
        if not PAM_PRESENT:
            raise AssertionError(f"TUNA_REQUIRE_PAM_WRAPPER: {PAM_WRAPPER} and {PAM_MATRIX} are required")
        cls.tmp = pathlib.Path(tempfile.mkdtemp(prefix="tuna-pam-otp-"))
        cls.module = cls.tmp / "tuna-pam-otp.so"
        subprocess.run(["cc", "-Wall", "-Wextra", "-Werror", "-shared", "-fPIC", "-o",
                        str(cls.module), str(OTP_SOURCE), "-lpam"], check=True)
        cls.user = "tuna-proof-user"
        cls.password = "pw-" + secrets.token_hex(6)
        cls.code = str(secrets.randbelow(900000) + 100000)
        services = cls.tmp / "services"
        services.mkdir()
        (cls.tmp / "passdb").write_text(f"{cls.user}:{cls.password}:tuna-lock\n")
        passdb = cls.tmp / "passdb"
        (services / "tuna-lock").write_text(
            f"auth requisite {PAM_MATRIX} passdb={passdb}\n"
            f"auth required {cls.module} code={cls.code}\n"
            f"account required {PAM_MATRIX} passdb={passdb}\n")
        (services / "tuna-otp-nocode").write_text(f"auth required {cls.module}\n")
        cls.services = services

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tmp)

    def authenticate(self, service, *answers):
        env = dict(os.environ, LD_PRELOAD=str(PAM_WRAPPER), PAM_WRAPPER="1",
                   PAM_WRAPPER_SERVICE_DIR=str(self.services))
        result = subprocess.run([self.PYTHON, "-c", PAM_HARNESS, service, self.user, *answers],
                                capture_output=True, text=True, env=env, timeout=20)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def test_password_then_info_then_visible_code_succeeds(self):
        outcome = self.authenticate("tuna-lock", self.password, self.code)
        self.assertEqual(outcome["messages"], [
            [1, "Password: "],
            [4, "A verification code was sent to your device"],
            [2, "Verification code:"],
        ])
        self.assertEqual((outcome["auth"], outcome["account"], outcome["unused"]), (0, 0, 0))

    def test_wrong_code_fails_after_the_code_step(self):
        outcome = self.authenticate("tuna-lock", self.password, "000000" if self.code != "000000" else "111111")
        self.assertEqual([m[1] for m in outcome["messages"]][-1], "Verification code:")
        self.assertEqual(outcome["auth"], 7)  # PAM_AUTH_ERR

    def test_code_prefix_or_extension_fails(self):
        for answer in (self.code[:-1], self.code + "0", ""):
            self.assertEqual(self.authenticate("tuna-lock", self.password, answer)["auth"], 7, answer)

    def test_wrong_password_stops_before_the_code(self):
        outcome = self.authenticate("tuna-lock", "not-" + self.password, self.code)
        self.assertEqual(outcome["messages"], [[1, "Password: "]])
        self.assertNotEqual(outcome["auth"], 0)
        self.assertEqual(outcome["unused"], 1)

    def test_conversation_failure_and_missing_code_fail_closed(self):
        self.assertNotEqual(self.authenticate("tuna-lock", self.password)["auth"], 0)
        outcome = self.authenticate("tuna-otp-nocode", "anything")
        self.assertEqual((outcome["auth"], outcome["messages"]), (3, []))  # PAM_SERVICE_ERR


if __name__ == "__main__":
    unittest.main(verbosity=2)
