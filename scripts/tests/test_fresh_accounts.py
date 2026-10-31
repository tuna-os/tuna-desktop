"""Record-free fresh-account journey (tunaOS#4092): host journey against a
simulated guest, plus the guest probe's record detection."""
import ast
import os
import pathlib
import sys
import tempfile
from types import SimpleNamespace
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/lib"))
import fresh_accounts  # noqa: E402

GUEST = ROOT / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
CONF = "[daemon]\nFallbackSession=tuna\n"


class FakeGuest:
    """A guest whose GDM logs an account in after the QMP keyboard typed its
    name and password, and whose AccountsService writes the record then."""

    def __init__(self, **faults):
        self.faults = faults
        self.boot = 1
        self.records = {"tunaseeded": "[User]\nSystemAccount=false\n"}
        self.records.update(faults.get("records", {}))
        self.groups = {"tunastd": ["tunastd"], "tunaadmin": ["tunaadmin", "wheel"],
                       "tunaseeded": ["tunaseeded"]}
        self.groups.update(faults.get("groups", {}))
        self.uids = {"tunastd": 1001, "tunaadmin": 1002, "tunaseeded": 1003}
        self.logged_in = None
        self.typed = []
        self.logins = []

    # QMP
    def keys(self, *names):
        if names == ("ret",) and len(self.typed) >= 2:
            user, password = self.typed[-2], self.typed[-1]
            if password == "tuna" and user in self.uids and self.logged_in is None:
                self.logged_in = user
                self.logins.append((self.boot, user))
                if not self.faults.get("no_record"):
                    self.records[user] = self.faults.get("saved", "[User]\nSession=tuna\nSystemAccount=false\n")
            self.typed = []

    def type_text(self, text):
        self.typed.append(text)

    def frame(self, path):
        pass

    # guest agent
    def connect(self):
        return self

    def close(self):
        pass

    def run(self, action, *args):
        if action == "greeter":
            boot = self.boot if not self.faults.get("stale_boot") else 1
            return {"ready": self.logged_in is None, "boot_id": f"boot-{boot}"}
        if action == "fresh-state":
            user = args[0]
            if user in self.faults.get("missing", ()):
                return {"user": user, "exists": False}
            return {"user": user, "exists": True, "uid": self.uids[user], "groups": self.groups[user],
                    "home": f"/var/home/{user}", "home_exists": True, "record": self.records.get(user),
                    "custom_conf": self.faults.get("custom_conf", CONF)}
        if action == "fresh-session":
            user = args[0]
            sessions = desktop = []
            if self.logged_in == user:
                sessions = [{"Id": "3", "VTNr": "2", "User": str(self.uids[user]),
                             "Service": "gdm-password", "State": "active"}]
                desktop = self.faults.get("desktop", ["tuna-session", "tuna-compositor", "tuna-shell-gtk"])
            return {"user": user, "uid": self.uids[user], "sessions": sessions, "desktop": desktop}
        if action == "fresh-sudo":
            user = args[0]
            allowed = "wheel" in self.groups[user] or self.faults.get("std_sudo")
            return {"user": user, "returncode": 0 if allowed else 1, "policy_denied": not allowed,
                    "sudo_present": True}
        if action == "fresh-logout":
            if not self.faults.get("stuck_session"):
                self.logged_in = None
            return {"user": args[0]}
        if action == "reboot":
            before = f"boot-{self.boot}"
            self.boot += 1
            self.logged_in = None
            return {"boot_id": before}
        raise ValueError(action)


def journey(guest):
    results = {}
    clock = [0.0]

    def sleep(seconds):
        clock[0] += seconds

    with tempfile.TemporaryDirectory() as out:
        run = fresh_accounts.Journey(guest, guest.connect, lambda greeter: greeter["ready"], out,
                                     lambda name, ok, detail: results.__setitem__(name, (ok, detail)),
                                     sleep=sleep, clock=lambda: clock[0], login_timeout=5, greeter_timeout=5)
        try:
            passed = run.run()
            error = None
        except RuntimeError as exc:
            passed, error = False, str(exc)
        receipts = (pathlib.Path(out) / "fresh-accounts-receipts.json").exists()
    return passed, results, error, receipts


class JourneyTest(unittest.TestCase):
    def assertFailed(self, results, name):
        self.assertIn(name, results)
        self.assertFalse(results[name][0], results[name][1])

    def test_positive_full_journey(self):
        guest = FakeGuest()
        passed, results, error, receipts = journey(guest)
        self.assertTrue(passed, (results, error))
        self.assertTrue(receipts)
        self.assertTrue(all(ok for ok, _ in results.values()), results)
        self.assertEqual(guest.logins, [(1, "tunastd"), (1, "tunaadmin"), (1, "tunastd"), (1, "tunaadmin"),
                                        (2, "tunastd"), (2, "tunaadmin")])
        for name in ("V-FRESH-RECORD-FREE-tunastd", "V-FRESH-RECORD-FREE-tunaadmin", "V-FRESH-CONTROL",
                     "V-FRESH-RECORD-CREATED-tunastd", "V-FRESH-SUDO-tunaadmin", "V-FRESH-SUDO-tunastd",
                     "V-FRESH-REBOOT", "V-FRESH-RECORD-KEPT-tunaadmin", "V-FRESH-LOGOUT-tunaadmin-reboot"):
            self.assertIn(name, results)

    def test_preseeded_record_is_not_record_free(self):
        passed, results, _, _ = journey(FakeGuest(records={"tunastd": "[User]\nSession=tuna\n"}))
        self.assertFalse(passed)
        self.assertFailed(results, "V-FRESH-RECORD-FREE-tunastd")
        self.assertNotIn("V-FRESH-LOGIN-tunastd-first", results)

    def test_sessionless_record_is_not_record_free(self):
        passed, results, _, _ = journey(FakeGuest(records={"tunaadmin": "[User]\nSystemAccount=false\n"}))
        self.assertFalse(passed)
        self.assertFailed(results, "V-FRESH-RECORD-FREE-tunaadmin")

    def test_control_must_be_seeded(self):
        guest = FakeGuest()
        del guest.records["tunaseeded"]
        passed, results, _, _ = journey(guest)
        self.assertFalse(passed)
        self.assertFailed(results, "V-FRESH-CONTROL")

    def test_missing_fixture_accounts(self):
        passed, results, _, _ = journey(FakeGuest(missing=("tunastd", "tunaseeded")))
        self.assertFalse(passed)
        self.assertFailed(results, "V-FRESH-RECORD-FREE-tunastd")
        self.assertFailed(results, "V-FRESH-CONTROL")

    def test_autologin_rejected(self):
        passed, results, _, _ = journey(FakeGuest(custom_conf=CONF + "AutomaticLoginEnable=True\n"))
        self.assertFalse(passed)
        self.assertFailed(results, "V-FRESH-RECORD-FREE-tunastd")

    def test_admin_group_swapped(self):
        passed, results, _, _ = journey(FakeGuest(groups={"tunastd": ["tunastd", "wheel"]}))
        self.assertFalse(passed)
        self.assertFailed(results, "V-FRESH-RECORD-FREE-tunastd")

    def test_gnome_fallback_session_fails(self):
        passed, results, _, _ = journey(FakeGuest(desktop=["gnome-session-b", "gnome-shell"]))
        self.assertFalse(passed)
        self.assertFailed(results, "V-FRESH-LOGIN-tunastd-first")

    def test_no_compositor_fails(self):
        passed, results, _, _ = journey(FakeGuest(desktop=["tuna-session"]))
        self.assertFalse(passed)
        self.assertFailed(results, "V-FRESH-LOGIN-tunastd-first")

    def test_record_not_created_fails(self):
        passed, results, _, _ = journey(FakeGuest(no_record=True))
        self.assertFalse(passed)
        self.assertFailed(results, "V-FRESH-RECORD-CREATED-tunastd")

    def test_saved_gnome_session_fails(self):
        passed, results, _, _ = journey(FakeGuest(saved="[User]\nSession=gnome\n"))
        self.assertFalse(passed)
        self.assertFailed(results, "V-FRESH-RECORD-CREATED-tunastd")

    def test_standard_sudo_fails(self):
        passed, results, _, _ = journey(FakeGuest(std_sudo=True))
        self.assertFalse(passed)
        self.assertFailed(results, "V-FRESH-SUDO-tunastd")

    def test_admin_without_sudo_fails(self):
        self.assertFalse(fresh_accounts.sudo_ok({"user": "tunaadmin", "returncode": 1, "policy_denied": True,
                                                 "sudo_present": True}, True)[0])
        # A wrong password or missing sudo is not a policy refusal.
        self.assertFalse(fresh_accounts.sudo_ok({"user": "tunastd", "returncode": 1, "policy_denied": False,
                                                 "sudo_present": True}, False)[0])
        self.assertFalse(fresh_accounts.sudo_ok({"user": "tunastd", "returncode": 1, "policy_denied": True,
                                                 "sudo_present": False}, False)[0])

    def test_session_surviving_logout_fails(self):
        passed, results, _, _ = journey(FakeGuest(stuck_session=True))
        self.assertFalse(passed)
        self.assertNotIn("V-FRESH-LOGIN-tunaadmin-first", results)

    def test_stale_greeter_is_not_a_reboot(self):
        passed, results, error, receipts = journey(FakeGuest(stale_boot=True))
        self.assertFalse(passed)
        self.assertIn("reboot", error)
        self.assertNotIn("V-FRESH-REBOOT", results)
        self.assertTrue(receipts)


class GuestStateTest(unittest.TestCase):
    """The guest probe itself reports a record exactly when the file exists."""

    def probe(self, records):
        tree = ast.parse(GUEST.read_text())
        body = [node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "fresh_state"]
        with tempfile.TemporaryDirectory() as root:
            users = pathlib.Path(root, "users")
            users.mkdir()
            conf = pathlib.Path(root, "custom.conf")
            conf.write_text(CONF)
            for name, text in records.items():
                (users / name).write_text(text)
            entry = SimpleNamespace(pw_uid=1001, pw_dir=root)
            scope = {"pwd": SimpleNamespace(getpwnam=lambda name: entry), "os": os, "pathlib": pathlib,
                     "ACCOUNTS_RECORD_DIR": users, "GDM_CUSTOM_CONF": conf, "call": lambda *args: "tunastd"}
            exec(compile(ast.Module(body=body, type_ignores=[]), str(GUEST), "exec"), scope)
            state = scope["fresh_state"]("tunastd")
        return state

    def test_absent_record(self):
        state = self.probe({})
        self.assertIsNone(state["record"])

    def test_present_record_detected(self):
        state = self.probe({"tunastd": "[User]\nSystemAccount=false\n"})
        self.assertEqual(state["record"], "[User]\nSystemAccount=false\n")
        self.assertFalse(fresh_accounts.record_free(state, False)[0])

    def test_absent_record_is_record_free(self):
        self.assertTrue(fresh_accounts.record_free(self.probe({}), False)[0])


if __name__ == "__main__":
    unittest.main()
