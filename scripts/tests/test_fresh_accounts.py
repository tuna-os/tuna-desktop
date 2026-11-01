"""Record-free fresh-account journey (tunaOS#4092): host journey against a
simulated guest, plus the guest probe's record detection and its read-only
GDM/AccountsService diagnostics."""
import ast
import json
import os
import pathlib
import runpy
import select
import shutil
import subprocess
import sys
import tempfile
import time
import types
from types import SimpleNamespace
import unittest
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/lib"))
import fresh_accounts  # noqa: E402

GUEST = ROOT / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
CONF = "[daemon]\nFallbackSession=tuna\n"
NATIVE_SOURCE = "7b52bdf34be22bc60fef62b157be64b589b13f6a"


def candidate_manifest():
    """Synthetic identity only: these test hashes are never CI evidence."""
    return {
        "schema": 1, "source_repository": "tuna-os/tunaos-packages", "source_commit": NATIVE_SOURCE,
        "architecture": "x86_64",
        "packages": {
            name: {"file": f"{name}-51.1.1-1.1-x86_64.pkg.tar.zst", "version": "51.1.1-1.1", "sha256": digit * 64}
            for name, digit in (("gdm", "a"), ("libgdm", "b"))
        },
        "members": {
            path: {"package": package, "sha256": digit * 64}
            for path, package, digit in (("/usr/bin/gdm", "gdm", "c"),
                                         ("/usr/lib/gdm-session-worker", "gdm", "d"),
                                         ("/usr/lib/libgdm.so.1.0.0", "libgdm", "e"))
        },
    }


def candidate_policy():
    return {"packages": {name: {"returncode": 0, "stdout": f"{name} 51.1.1-1.1\n", "truncated": False}
                         for name in ("gdm", "libgdm")}}


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
        self.logged_out = None
        self.typed = []
        self.logins = []
        self.submitted = []
        self.state_calls = []
        self.trace_actions = []
        self.actions = []
        self.input_calls = 0
        self.policy_calls = []

    # QMP
    def keys(self, *names):
        self.input_calls += 1
        if names == ("ret",) and len(self.typed) >= 2:
            user, password = self.typed[-2], self.typed[-1]
            if password == "tuna" and user in self.uids:
                self.submitted.append(user)
            if (password == "tuna" and user in self.uids and self.logged_in is None
                    and not self.faults.get("reject_login")):
                self.logged_in, self.logged_out = user, None
                self.logins.append((self.boot, user))
                if not (self.faults.get("no_record") or self.faults.get("record_on_logout")):
                    self.records[user] = self.faults.get("saved", "[User]\nSession=tuna\nSystemAccount=false\n")
            self.typed = []

    def type_text(self, text):
        self.input_calls += 1
        self.typed.append(text)

    def frame(self, path):
        pass

    # guest agent
    def connect(self):
        return self

    def close(self):
        pass

    def run(self, action, *args):
        self.actions.append(action)
        if action == "gdm-policy":
            self.policy_calls.append((self.boot, self.input_calls, len(self.state_calls)))
            if self.faults.get("policy_error"):
                raise RuntimeError("PRIVATE-GUEST-ERROR")
            return self.faults.get("policy", candidate_policy())
        if action in ("fresh-trace-start", "fresh-trace-stop"):
            self.trace_actions.append((action, len(self.submitted)))
            if self.faults.get("trace_error"):
                raise RuntimeError("PRIVATE-DIAGNOSTIC-ERROR")
            return {"ready": True} if action.endswith("start") else {"profile": "headers-only", "events": []}
        if action == "greeter":
            boot = self.boot if not self.faults.get("stale_boot") else 1
            return {"ready": self.logged_in is None, "boot_id": f"boot-{boot}"}
        if action == "fresh-state":
            user = args[0]
            self.state_calls.append(user)
            if self.faults.get("logout_state_error") and self.logged_out == user:
                raise RuntimeError("guest lifecycle fresh-state failed (exit=1)")
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
            if self.logged_in == args[0] and self.faults.get("record_on_logout"):
                self.records[args[0]] = "[User]\nSession=tuna\nSystemAccount=false\n"
            if not self.faults.get("stuck_session"):
                self.logged_in, self.logged_out = None, args[0]
            return {"user": args[0]}
        if action == "reboot":
            before = f"boot-{self.boot}"
            self.boot += 1
            self.logged_in = self.logged_out = None
            return {"boot_id": before}
        raise ValueError(action)


def journey(guest, gdm_trace=False, candidate_env=None):
    results = {}
    clock = [0.0]

    def sleep(seconds):
        clock[0] += seconds

    environment = dict(os.environ)
    for name in ("TUNA_VM_GDM_NATIVE_CANDIDATE", "TUNA_VM_GDM_NATIVE_SOURCE_SHA"):
        environment.pop(name, None)
    environment.update(candidate_env or {})
    with tempfile.TemporaryDirectory() as out, mock.patch.dict(os.environ, environment, clear=True):
        run = fresh_accounts.Journey(guest, guest.connect, lambda greeter: greeter["ready"], out,
                                     lambda name, ok, detail: results.__setitem__(name, (ok, detail)),
                                     sleep=sleep, clock=lambda: clock[0], login_timeout=5, greeter_timeout=5,
                                     gdm_trace=gdm_trace)
        try:
            passed = run.run()
            error = None
        except RuntimeError as exc:
            passed, error = False, str(exc)
        path = pathlib.Path(out) / "fresh-accounts-receipts.json"
        receipts = json.loads(path.read_text()) if path.exists() else None
    return passed, results, error, receipts


def steps(receipts):
    return [receipt["step"] for receipt in receipts]


class JourneyTest(unittest.TestCase):
    def assertFailed(self, results, name):
        self.assertIn(name, results)
        self.assertFalse(results[name][0], results[name][1])

    def test_trace_observes_only_first_standard_login(self):
        guest = FakeGuest()
        passed, results, error, receipts = journey(guest, gdm_trace=True)
        self.assertTrue(passed, (results, error))
        self.assertEqual(guest.trace_actions, [("fresh-trace-start", 0), ("fresh-trace-stop", 1)])
        self.assertEqual(len(guest.submitted), 6)
        self.assertEqual([step for step in steps(receipts) if "trace" in step],
                         ["tunastd-first-trace-start", "tunastd-first-trace-stop"])

    def test_trace_failure_does_not_change_physical_record_gate(self):
        guest = FakeGuest(no_record=True, trace_error=True)
        passed, results, error, receipts = journey(guest, gdm_trace=True)
        self.assertFalse(passed)
        self.assertFailed(results, "V-FRESH-RECORD-CREATED-tunastd")
        self.assertEqual(guest.trace_actions, [("fresh-trace-start", 0), ("fresh-trace-stop", 1)])
        self.assertNotIn("PRIVATE-DIAGNOSTIC-ERROR", json.dumps(receipts))

    def test_trace_stops_before_existing_selection_retry(self):
        guest = FakeGuest(reject_login=True)
        passed, results, error, receipts = journey(guest, gdm_trace=True)
        self.assertFalse(passed)
        self.assertEqual(guest.trace_actions, [("fresh-trace-start", 0), ("fresh-trace-stop", 1)])
        self.assertEqual(len(guest.submitted), len(fresh_accounts.SELECTIONS))

    def test_trace_stops_on_keyboard_exception(self):
        guest = FakeGuest()
        with mock.patch.object(guest, "type_text", side_effect=RuntimeError("input failed")):
            passed, results, error, receipts = journey(guest, gdm_trace=True)
        self.assertFalse(passed)
        self.assertEqual(guest.trace_actions, [("fresh-trace-start", 0), ("fresh-trace-stop", 0)])

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

    def test_first_round_records_state_after_each_logout(self):
        _, _, _, receipts = journey(FakeGuest())
        names = steps(receipts)
        for user in fresh_accounts.JOURNEY_USERS:
            self.assertEqual(names.count(f"{user}-first-logout-state"), 1)
            self.assertEqual(names.index(f"{user}-first-logout-state"), names.index(f"{user}-first-logout") + 1)
            self.assertNotIn(f"{user}-second-logout-state", names)
            self.assertNotIn(f"{user}-reboot-logout-state", names)
        self.assertFalse([name for name in names if name.endswith("-failure-state")])
        after = receipts[names.index("tunastd-first-logout-state")]["value"]
        self.assertEqual(after["user"], "tunastd")
        self.assertIsNotNone(after["record"])

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

    def test_failed_login_records_one_terminal_state(self):
        guest = FakeGuest(desktop=["tuna-session"])
        passed, results, _, receipts = journey(guest)
        self.assertFalse(passed)
        self.assertEqual(steps(receipts)[-2:], ["tunastd-first-session", "tunastd-first-failure-state"])
        self.assertEqual(receipts[-1]["value"]["user"], "tunastd")
        self.assertEqual(guest.logins, [(1, "tunastd")])
        self.assertNotIn("V-FRESH-RECORD-CREATED-tunastd", results)

    def test_no_session_records_one_terminal_state_without_new_attempts(self):
        guest = FakeGuest(reject_login=True)
        passed, results, _, receipts = journey(guest)
        self.assertFalse(passed)
        self.assertFailed(results, "V-FRESH-LOGIN-tunastd-first")
        self.assertEqual(guest.submitted, ["tunastd"] * len(fresh_accounts.SELECTIONS))
        self.assertEqual(guest.state_calls, ["tunastd", "tunaadmin", "tunaseeded", "tunastd"])
        self.assertEqual(steps(receipts)[-1], "tunastd-first-failure-state")

    def test_record_not_created_fails(self):
        guest = FakeGuest(no_record=True)
        passed, results, _, receipts = journey(guest)
        self.assertFalse(passed)
        self.assertFailed(results, "V-FRESH-RECORD-CREATED-tunastd")
        names = steps(receipts)
        self.assertEqual(names[names.index("tunastd-first-session"):],
                         ["tunastd-first-session", "tunastd-first-state", "tunastd-first-sudo",
                          "tunastd-first-logout-greeter", "tunastd-first-logout", "tunastd-first-logout-state"])
        self.assertIsNone(receipts[-1]["value"]["record"])
        self.assertEqual(guest.logins, [(1, "tunastd")])

    def test_record_only_after_logout_still_fails_first_session_gate(self):
        guest = FakeGuest(record_on_logout=True)
        passed, results, _, receipts = journey(guest)
        self.assertFalse(passed)
        self.assertFailed(results, "V-FRESH-RECORD-CREATED-tunastd")
        names = steps(receipts)
        self.assertIsNone(receipts[names.index("tunastd-first-state")]["value"]["record"])
        self.assertIsNotNone(receipts[names.index("tunastd-first-logout-state")]["value"]["record"])
        self.assertEqual(guest.logins, [(1, "tunastd")])

    def test_failed_post_logout_diagnostic_changes_no_gate(self):
        passed, results, error, receipts = journey(FakeGuest(logout_state_error=True))
        self.assertTrue(passed, (results, error))
        self.assertTrue(all(ok for ok, _ in results.values()), results)
        names = steps(receipts)
        self.assertEqual(receipts[names.index("tunastd-first-logout-state")]["value"],
                         {"user": "tunastd", "error": "RuntimeError"})

    def test_failed_diagnostic_keeps_missing_record_failure(self):
        passed, results, error, receipts = journey(FakeGuest(no_record=True, logout_state_error=True))
        self.assertFalse(passed)
        self.assertIsNone(error)
        self.assertFailed(results, "V-FRESH-RECORD-CREATED-tunastd")
        self.assertEqual(receipts[-1], {"step": "tunastd-first-logout-state",
                                        "value": {"user": "tunastd", "error": "RuntimeError"}})

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


class NativeCandidateManifestTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.path = pathlib.Path(temporary.name) / "manifest.json"
        self.env = {"TUNA_VM_GDM_NATIVE_CANDIDATE": str(self.path),
                    "TUNA_VM_GDM_NATIVE_SOURCE_SHA": NATIVE_SOURCE}
        patcher = mock.patch.dict(os.environ, self.env, clear=True)
        patcher.start()
        self.addCleanup(patcher.stop)

    def load(self, value):
        self.path.write_text(json.dumps(value))
        return fresh_accounts.native_candidate()

    def test_valid_strict_identity(self):
        value = candidate_manifest()
        self.assertEqual(self.load(value), value)

    def test_absent_environment_is_the_unchanged_default(self):
        with mock.patch.dict(os.environ, {}, clear=True):
            self.assertIsNone(fresh_accounts.native_candidate())

    def test_each_environment_value_alone_or_empty_fails(self):
        self.path.write_text(json.dumps(candidate_manifest()))
        for environment in (
                {"TUNA_VM_GDM_NATIVE_CANDIDATE": str(self.path)},
                {"TUNA_VM_GDM_NATIVE_SOURCE_SHA": NATIVE_SOURCE},
                {"TUNA_VM_GDM_NATIVE_CANDIDATE": "", "TUNA_VM_GDM_NATIVE_SOURCE_SHA": NATIVE_SOURCE},
                {"TUNA_VM_GDM_NATIVE_CANDIDATE": str(self.path), "TUNA_VM_GDM_NATIVE_SOURCE_SHA": ""},
                {"TUNA_VM_GDM_NATIVE_CANDIDATE": "", "TUNA_VM_GDM_NATIVE_SOURCE_SHA": ""}):
            with self.subTest(environment=environment), mock.patch.dict(os.environ, environment, clear=True):
                with self.assertRaises(ValueError):
                    fresh_accounts.native_candidate()

    def test_expected_source_is_separately_pinned(self):
        value = candidate_manifest()
        for source in ("f" * 40, NATIVE_SOURCE.upper(), NATIVE_SOURCE[:-1], "../" + NATIVE_SOURCE):
            with self.subTest(source=source):
                os.environ["TUNA_VM_GDM_NATIVE_SOURCE_SHA"] = source
                value["source_commit"] = source
                with self.assertRaises(ValueError):
                    self.load(value)

    def test_schema_rejects_wrong_values_types_and_unsafe_paths(self):
        cases = [
            (("schema",), True), (("schema",), 1.0), (("schema",), "1"), (("schema",), 2),
            (("source_repository",), "elsewhere/packages"),
            (("source_commit",), "f" * 40), (("source_commit",), NATIVE_SOURCE.upper()),
            (("source_commit",), None), (("architecture",), "aarch64"), (("architecture",), ["x86_64"]),
            (("packages",), []), (("members",), None),
            (("packages", "gdm", "version"), "51.1.1-1"),
            (("packages", "libgdm", "version"), "51.1.1-1"),
            (("packages", "gdm", "file"), "../gdm-51.1.1-1.1-x86_64.pkg.tar.zst"),
            (("packages", "gdm", "file"), "/gdm-51.1.1-1.1-x86_64.pkg.tar.zst"),
            (("packages", "libgdm", "file"), "gdm-51.1.1-1.1-x86_64.pkg.tar.zst"),
            (("packages", "libgdm", "file"), "libgdm-51.1.1-1.1-x86_64.pkg.tar.zst/extra"),
            (("members", "/usr/bin/gdm", "package"), "libgdm"),
            (("members", "/usr/lib/libgdm.so.1.0.0", "package"), "gdm"),
        ]
        for field in (("packages", "gdm", "sha256"), ("packages", "libgdm", "sha256"),
                      ("members", "/usr/bin/gdm", "sha256"),
                      ("members", "/usr/lib/gdm-session-worker", "sha256"),
                      ("members", "/usr/lib/libgdm.so.1.0.0", "sha256")):
            cases.extend((field, invalid) for invalid in ("a" * 63, "a" * 65, "A" * 64, "z" * 64, 123, None))
        for field, invalid in cases:
            value = candidate_manifest()
            parent = value
            for key in field[:-1]:
                parent = parent[key]
            parent[field[-1]] = invalid
            with self.subTest(field=field, invalid=invalid), self.assertRaises(ValueError):
                self.load(value)

    def test_every_mapping_rejects_missing_and_unknown_fields(self):
        for field in ((), ("packages",), ("packages", "gdm"), ("packages", "libgdm"), ("members",),
                      ("members", "/usr/bin/gdm"), ("members", "/usr/lib/gdm-session-worker"),
                      ("members", "/usr/lib/libgdm.so.1.0.0")):
            for change in ("missing", "unknown"):
                value = candidate_manifest()
                parent = value
                for key in field:
                    parent = parent[key]
                if change == "missing":
                    del parent[next(iter(parent))]
                else:
                    parent["unapproved"] = "PRIVATE"
                with self.subTest(field=field, change=change), self.assertRaises(ValueError):
                    self.load(value)

    def test_member_paths_must_be_exact(self):
        for path in ("usr/bin/gdm", "/usr/bin/../bin/gdm", "/usr/bin/gdm/extra",
                     "/usr/bin/gdm\0", "/usr/lib/libgdm.so.1", "../../gdm"):
            value = candidate_manifest()
            value["members"][path] = value["members"].pop("/usr/bin/gdm")
            with self.subTest(path=path), self.assertRaises(ValueError):
                self.load(value)

    def test_malformed_duplicate_nonfinite_and_non_utf8_json(self):
        encoded = json.dumps(candidate_manifest()).encode()
        variants = [b"", b"{", b"[]", b"null", b"\xff", encoded + b" trailing",
                    encoded.replace(b'"schema": 1', b'"schema": 1, "schema": 1'),
                    encoded.replace(b'"version": "51.1.1-1.1"',
                                    b'"version": "51.1.1-1.1", "version": "51.1.1-1.1"', 1),
                    encoded.replace(b'"sha256": "' + b"c" * 64 + b'"',
                                    b'"sha256": "' + b"c" * 64 + b'", "sha256": "' + b"c" * 64 + b'"'),
                    encoded.replace(b'"schema": 1', b'"schema": NaN'),
                    encoded.replace(b'"schema": 1', b'"schema": Infinity')]
        for data in variants:
            self.path.write_bytes(data)
            with self.subTest(data=data[:80]), self.assertRaises(ValueError):
                fresh_accounts.native_candidate()

    def test_regular_file_size_bound_is_inclusive(self):
        data = json.dumps(candidate_manifest()).encode()
        self.path.write_bytes(data + b" " * (16 * 1024 - len(data)))
        self.assertEqual(fresh_accounts.native_candidate(), candidate_manifest())
        self.path.write_bytes(data + b" " * (16 * 1024 + 1 - len(data)))
        with self.assertRaises(ValueError):
            fresh_accounts.native_candidate()

    def test_symlink_directory_fifo_and_missing_file_are_rejected(self):
        target = self.path.with_name("target.json")
        target.write_text(json.dumps(candidate_manifest()))
        self.path.symlink_to(target)
        with self.assertRaises(ValueError):
            fresh_accounts.native_candidate()
        self.path.unlink()
        self.path.mkdir()
        with self.assertRaises(ValueError):
            fresh_accounts.native_candidate()
        self.path.rmdir()
        os.mkfifo(self.path)
        with self.assertRaises(ValueError):
            fresh_accounts.native_candidate()
        self.path.unlink()
        with self.assertRaises(OSError):
            fresh_accounts.native_candidate()

    def test_file_replacement_before_open_is_rejected(self):
        self.path.write_text(json.dumps(candidate_manifest()))
        original_open = os.open

        def replaced(path, flags):
            replacement = self.path.with_name("replacement.json")
            replacement.write_text(json.dumps(candidate_manifest()))
            replacement.replace(self.path)
            return original_open(path, flags)

        with mock.patch.object(fresh_accounts.os, "open", side_effect=replaced):
            with self.assertRaises(ValueError):
                fresh_accounts.native_candidate()

    def test_file_change_during_read_is_rejected(self):
        self.path.write_text(json.dumps(candidate_manifest()))
        original_fstat = os.fstat
        calls = []

        def changed(fd):
            calls.append(fd)
            if len(calls) == 2:
                self.path.write_text("{}")
            return original_fstat(fd)

        with mock.patch.object(fresh_accounts.os, "fstat", side_effect=changed):
            with self.assertRaises(ValueError):
                fresh_accounts.native_candidate()


class NativeCandidateJourneyTest(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.path = pathlib.Path(temporary.name) / "manifest.json"
        self.path.write_text(json.dumps(candidate_manifest()))
        self.env = {"TUNA_VM_GDM_NATIVE_CANDIDATE": str(self.path),
                    "TUNA_VM_GDM_NATIVE_SOURCE_SHA": NATIVE_SOURCE}

    def test_success_probes_once_after_greeter_before_input_and_preserves_full_journey(self):
        policy = candidate_policy()
        policy.update({"templates": "PRIVATE-GUEST-PAYLOAD", "owner": "PRIVATE-GUEST-PAYLOAD"})
        policy["packages"]["accountsservice"] = {"stdout": "PRIVATE-GUEST-PAYLOAD"}
        guest = FakeGuest(policy=policy)
        with mock.patch.dict(os.environ, {"PRIVATE_ENV": "PRIVATE-ENV-PAYLOAD"}):
            passed, results, error, receipts = journey(guest, candidate_env=self.env)
        self.assertTrue(passed, (results, error))
        self.assertTrue(results["V-FRESH-GDM-NATIVE-CANDIDATE"][0])
        self.assertEqual(guest.actions[:2], ["greeter", "gdm-policy"])
        self.assertEqual(guest.policy_calls, [(1, 0, 0)])
        self.assertEqual(guest.logins, [(1, "tunastd"), (1, "tunaadmin"), (1, "tunastd"), (1, "tunaadmin"),
                                       (2, "tunastd"), (2, "tunaadmin")])
        self.assertEqual(receipts[1], {"step": "preflight-gdm-native-candidate", "value": candidate_manifest()})
        self.assertEqual(receipts[2], {"step": "preflight-gdm-native-installed",
                                       "value": {"gdm": "51.1.1-1.1", "libgdm": "51.1.1-1.1"}})
        rendered = json.dumps((receipts, results))
        for private in ("PRIVATE-GUEST-PAYLOAD", "PRIVATE-ENV-PAYLOAD", str(self.path)):
            self.assertNotIn(private, rendered)

    def test_no_manifest_does_not_probe_or_add_candidate_gate_or_receipts(self):
        guest = FakeGuest(policy_error=True)
        passed, results, error, receipts = journey(guest)
        self.assertTrue(passed, (results, error))
        self.assertEqual(guest.policy_calls, [])
        self.assertNotIn("V-FRESH-GDM-NATIVE-CANDIDATE", results)
        self.assertFalse(any("gdm-native" in name for name in steps(receipts)))

    def test_invalid_admission_stops_before_probe_and_any_input(self):
        environments = [
            {"TUNA_VM_GDM_NATIVE_CANDIDATE": str(self.path)},
            {"TUNA_VM_GDM_NATIVE_SOURCE_SHA": NATIVE_SOURCE},
            dict(self.env, TUNA_VM_GDM_NATIVE_SOURCE_SHA="f" * 40),
        ]
        for environment in environments:
            guest = FakeGuest()
            passed, results, error, _ = journey(guest, candidate_env=environment)
            with self.subTest(environment=environment):
                self.assertFalse(passed)
                self.assertIsNone(error)
                self.assertFalse(results["V-FRESH-GDM-NATIVE-CANDIDATE"][0])
                self.assertEqual(guest.actions, ["greeter"])
                self.assertEqual(guest.input_calls, 0)
        self.path.write_text('{"schema":1,"schema":1}')
        guest = FakeGuest()
        passed, results, error, receipts = journey(guest, candidate_env=self.env)
        self.assertFalse(passed)
        self.assertFalse(results["V-FRESH-GDM-NATIVE-CANDIDATE"][0])
        self.assertEqual(guest.actions, ["greeter"])
        self.assertEqual(guest.input_calls, 0)
        self.assertNotIn("preflight-gdm-native-candidate", steps(receipts))

    def test_wrong_missing_truncated_or_failed_guest_pair_stops_before_input(self):
        policies = [None, {}, {"packages": None}, {"packages": {}}, {"packages": []}]
        for name in ("gdm", "libgdm"):
            for replacement in (
                    None, {"error": "PRIVATE-GUEST-PAYLOAD"},
                    {"returncode": 1, "stdout": f"{name} 51.1.1-1.1\n", "truncated": False},
                    {"returncode": False, "stdout": f"{name} 51.1.1-1.1\n", "truncated": False},
                    {"returncode": 0, "stdout": f"{name} 51.1.1-1.1\n", "truncated": True},
                    {"returncode": 0, "stdout": f"{name} 51.1.1-1.1\n", "truncated": 0},
                    {"returncode": 0, "stdout": f"{name} 51.1.1-1.1\n"},
                    {"returncode": 0, "stdout": f"{name} 51.1.1-1.1\n", "truncated": False, "error": "PRIVATE"},
                    {"returncode": 0, "stdout": f"{name} 51.1.1-1\n", "truncated": False},
                    {"returncode": 0, "stdout": f"{name} 51.1.1-1.1", "truncated": False},
                    {"returncode": 0, "stdout": f"{name} 51.1.1-1.1\nextra\n", "truncated": False},
                    {"returncode": 0, "stdout": "other 51.1.1-1.1\n", "truncated": False},
                    {"returncode": 0, "stdout": ["51.1.1-1.1"], "truncated": False}):
                policy = candidate_policy()
                policy["packages"][name] = replacement
                policies.append(policy)
            policy = candidate_policy()
            del policy["packages"][name]
            policies.append(policy)
        for policy in policies:
            guest = FakeGuest(policy=policy)
            passed, results, error, receipts = journey(guest, candidate_env=self.env)
            with self.subTest(policy=policy):
                self.assertFalse(passed)
                self.assertIsNone(error)
                self.assertFalse(results["V-FRESH-GDM-NATIVE-CANDIDATE"][0])
                self.assertEqual(guest.policy_calls, [(1, 0, 0)])
                self.assertEqual(guest.input_calls, 0)
                self.assertEqual(guest.state_calls, [])
                self.assertNotIn("PRIVATE", json.dumps((receipts, results)))

    def test_guest_probe_exception_is_sanitized_and_not_retried(self):
        guest = FakeGuest(policy_error=True)
        passed, results, error, receipts = journey(guest, candidate_env=self.env)
        self.assertFalse(passed)
        self.assertIsNone(error)
        self.assertFalse(results["V-FRESH-GDM-NATIVE-CANDIDATE"][0])
        self.assertEqual(guest.policy_calls, [(1, 0, 0)])
        self.assertEqual(guest.input_calls, 0)
        self.assertNotIn("PRIVATE-GUEST-ERROR", json.dumps((receipts, results)))

    def test_correct_pair_cannot_manufacture_physical_record_pass(self):
        for faults in ({"no_record": True}, {"record_on_logout": True}):
            guest = FakeGuest(**faults)
            passed, results, error, receipts = journey(guest, candidate_env=self.env)
            with self.subTest(faults=faults):
                self.assertFalse(passed)
                self.assertIsNone(error)
                self.assertTrue(results["V-FRESH-GDM-NATIVE-CANDIDATE"][0])
                self.assertFalse(results["V-FRESH-RECORD-CREATED-tunastd"][0])
                self.assertNotIn("V-FRESH-REBOOT", results)
                self.assertEqual(guest.policy_calls, [(1, 0, 0)])
                self.assertEqual(guest.logins, [(1, "tunastd")])
                state = receipts[steps(receipts).index("tunastd-first-state")]["value"]
                self.assertIsNone(state["record"])


GUEST_FUNCTIONS = {"fresh_state", "record_presence", "template_observation", "accounts_get",
                   "accounts_properties", "bounded_command", "gdm_version_major", "gdm_policy"}
GUEST_CONSTANTS = {"ACCOUNTS_TEMPLATE_DIRS", "ACCOUNTS_TEMPLATE_KINDS", "ACCOUNTS_TEMPLATE_KEYS",
                   "ACCOUNTS_TEMPLATE_BOUND", "ACCOUNTS_PROPERTIES", "ACCOUNTS_TIMEOUT_MS",
                   "GDM_POLICY_PACKAGES", "DIAGNOSTIC_OUTPUT_BOUND", "GDM_VERSION"}


def guest_scope(**overrides):
    """The guest's diagnostic functions with their imports and constants;
    the module itself needs the image's fixture user to import."""
    tree = ast.parse(GUEST.read_text())
    body = [node for node in tree.body if isinstance(node, ast.Import)
            or (isinstance(node, ast.FunctionDef) and node.name in GUEST_FUNCTIONS)
            or (isinstance(node, ast.Assign) and any(isinstance(target, ast.Name) and target.id in GUEST_CONSTANTS
                                                     for target in node.targets))]
    scope = {}
    exec(compile(ast.Module(body=body, type_ignores=[]), str(GUEST), "exec"), scope)
    scope.update(overrides)
    return scope


class FakeVariant:
    def __init__(self, signature, value):
        self.signature, self.value = signature, value

    def get_child_value(self, index):
        return self.value[index]

    def get_variant(self):
        return self

    def get_type_string(self):
        return self.signature

    def unpack(self):
        return self.value


class FakeGLibError(Exception):
    def __init__(self, remote=None, domain="g-io-error-quark", code=24):
        super().__init__("PRIVATE-MESSAGE")
        self.remote, self.domain, self.code = remote, domain, code


class FakeBus:
    """The system bus as AccountsService answers Properties.Get."""

    def __init__(self, replies, during_get=None):
        self.replies, self.during_get, self.calls = replies, during_get, []

    def call_sync(self, name, path, interface, method, parameters, reply_type, flags, timeout, cancellable):
        self.calls.append((name, path, interface, method, parameters.signature, parameters.value,
                           reply_type, flags))
        if self.during_get:
            self.during_get()
        reply = self.replies[parameters.value[1]]
        if isinstance(reply, Exception):
            raise reply
        return FakeVariant("(v)", (reply,))


def fake_gi(bus):
    def bus_get_sync(kind, cancellable):
        if isinstance(bus, Exception):
            raise bus
        assert kind == "system"
        return bus
    repository = types.ModuleType("gi.repository")
    repository.Gio = SimpleNamespace(bus_get_sync=bus_get_sync, BusType=SimpleNamespace(SYSTEM="system"),
                                     DBusCallFlags=SimpleNamespace(NO_AUTO_START="no-auto-start"),
                                     DBusError=SimpleNamespace(get_remote_error=lambda error: error.remote))
    repository.GLib = SimpleNamespace(Variant=FakeVariant, VariantType=lambda signature: signature,
                                      Error=FakeGLibError)
    gi = types.ModuleType("gi")
    gi.repository = repository
    return mock.patch.dict(sys.modules, {"gi": gi, "gi.repository": repository})


UNKNOWN_OBJECT = "org.freedesktop.DBus.Error.UnknownObject"


def unexported():
    return FakeBus({name: FakeGLibError(UNKNOWN_OBJECT) for name in ("Session", "SessionType", "Saved",
                                                                      "LocalAccount")})


def template_roots(root):
    return tuple(str(pathlib.Path(root, name)) for name in ("run", "etc", "usr-local-share", "usr-share"))


def write_template(root, index, kind, text):
    path = pathlib.Path(template_roots(root)[index], "accountsservice/user-templates", kind)
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text)
    return path


class GuestStateTest(unittest.TestCase):
    """The guest probe itself reports a record exactly when the file exists,
    and its diagnostics never read as the persistence the gates require."""

    def probe(self, records, bus=None, templates=(), during_get=None):
        with tempfile.TemporaryDirectory() as root:
            users = pathlib.Path(root, "users")
            users.mkdir()
            conf = pathlib.Path(root, "custom.conf")
            conf.write_text(CONF)
            for name, text in records.items():
                (users / name).write_text(text)
            for index, kind, text in templates:
                write_template(root, index, kind, text)
            bus = unexported() if bus is None else bus
            if during_get is not None:
                bus.during_get = lambda: during_get(users)
            entry = SimpleNamespace(pw_uid=1001, pw_dir=root)
            scope = guest_scope(pwd=SimpleNamespace(getpwnam=lambda name: entry), ACCOUNTS_RECORD_DIR=users,
                                GDM_CUSTOM_CONF=conf, call=lambda *args: "tunastd",
                                ACCOUNTS_TEMPLATE_DIRS=template_roots(root))
            with fake_gi(bus):
                state = scope["fresh_state"]("tunastd")
            json.dumps(state)  # The receipt must serialize.
        return state

    def test_absent_record(self):
        state = self.probe({})
        self.assertIsNone(state["record"])
        self.assertEqual(state["accounts"]["record_before"], {"present": False})
        self.assertEqual(state["accounts"]["record_after"], {"present": False})

    def test_present_record_detected(self):
        state = self.probe({"tunastd": "[User]\nSystemAccount=false\n"})
        self.assertEqual(state["record"], "[User]\nSystemAccount=false\n")
        self.assertEqual(state["accounts"]["record_before"], {"present": True, "regular": True})
        self.assertFalse(fresh_accounts.record_free(state, False)[0])

    def test_absent_record_is_record_free(self):
        self.assertTrue(fresh_accounts.record_free(self.probe({}), False)[0])

    def test_effective_properties_are_exact_read_only_gets(self):
        bus = FakeBus({"Session": FakeVariant("s", ""), "SessionType": FakeVariant("s", "wayland"),
                       "Saved": FakeVariant("b", False),
                       "LocalAccount": FakeGLibError("org.freedesktop.Accounts.Error.Failed")})
        state = self.probe({}, bus=bus)
        self.assertEqual(state["accounts"]["effective"], {
            "object": "/org/freedesktop/Accounts/User1001",
            "properties": {"Session": {"value": ""}, "SessionType": {"value": "wayland"},
                           "Saved": {"value": False},
                           "LocalAccount": {"error": "org.freedesktop.Accounts.Error.Failed"}}})
        self.assertEqual(bus.calls, [
            ("org.freedesktop.Accounts", "/org/freedesktop/Accounts/User1001", "org.freedesktop.DBus.Properties",
             "Get", "(ss)", ("org.freedesktop.Accounts.User", name), "(v)", "no-auto-start")
            for name in ("Session", "SessionType", "Saved", "LocalAccount")])

    def test_unexported_object_is_an_error_not_an_empty_session(self):
        properties = self.probe({})["accounts"]["effective"]["properties"]
        self.assertEqual(properties, {name: {"error": UNKNOWN_OBJECT}
                                      for name in ("Session", "SessionType", "Saved", "LocalAccount")})
        self.assertNotIn("PRIVATE-MESSAGE", json.dumps(properties))

    def test_unavailable_bus_and_odd_replies_are_explicit(self):
        properties = self.probe({}, bus=FakeGLibError())["accounts"]["effective"]["properties"]
        self.assertEqual(set(properties), {"Session", "SessionType", "Saved", "LocalAccount"})
        self.assertTrue(all(value == {"error": "bus:FakeGLibError"} for value in properties.values()))
        bus = FakeBus({"Session": FakeGLibError(None, "g-io-error-quark", 24), "SessionType": TypeError("x"),
                       "Saved": FakeVariant("s", "true"), "LocalAccount": FakeVariant("b", True)})
        properties = self.probe({}, bus=bus)["accounts"]["effective"]["properties"]
        self.assertEqual(properties, {"Session": {"error": "g-io-error-quark:24"},
                                      "SessionType": {"error": "TypeError"},
                                      "Saved": {"error": "unexpected-type", "type": "s"},
                                      "LocalAccount": {"value": True}})

    def test_record_presence_brackets_property_collection(self):
        def create(users):
            (users / "tunastd").write_text("[User]\nSession=tuna\n")
        state = self.probe({}, bus=FakeBus({name: FakeVariant("s", "tuna") if name.startswith("Session")
                                            else FakeVariant("b", True)
                                            for name in ("Session", "SessionType", "Saved", "LocalAccount")}),
                           during_get=create)
        self.assertIsNone(state["record"])
        self.assertEqual(state["accounts"]["record_before"], {"present": False})
        self.assertEqual(state["accounts"]["record_after"], {"present": True, "regular": True})
        # Neither the later file nor Saved=true substitutes for the gate's record.
        self.assertFalse(fresh_accounts.record_created(state)[0])

    def test_templates_follow_precedence_and_allowlist(self):
        state = self.probe({}, templates=(
            (1, "standard", "[User]\nSession=\nIcon=/var/home/ICON-MARK/face\nSession=tuna\n[Other]\nXSession=gnome\n"),
            (3, "standard", "[User]\nSession=tuna\n"),
            (3, "administrator", "[User]\nSessionType=wayland\nSystemAccount=false\n")))
        templates = state["accounts"]["templates"]
        standard, administrator = templates["standard"], templates["administrator"]
        self.assertEqual([candidate["present"] for candidate in standard["candidates"]], [False, True, False, True])
        self.assertTrue(standard["winner"].endswith("/etc/accountsservice/user-templates/standard"))
        self.assertEqual(standard["keys"], {"Session": ["", "tuna"], "SessionType": [], "XSession": [],
                                            "SystemAccount": []})
        self.assertEqual(standard["user_groups"], 1)
        self.assertTrue(administrator["winner"].endswith("/usr-share/accountsservice/user-templates/administrator"))
        self.assertEqual(administrator["keys"], {"Session": [], "SessionType": ["wayland"], "XSession": [],
                                                 "SystemAccount": ["false"]})
        self.assertNotIn("ICON-MARK", json.dumps(templates))

    def test_missing_templates_have_no_winner(self):
        templates = self.probe({})["accounts"]["templates"]
        for kind in ("standard", "administrator"):
            self.assertIsNone(templates[kind]["winner"])
            self.assertNotIn("keys", templates[kind])
            self.assertFalse(any(candidate["present"] for candidate in templates[kind]["candidates"]))


class GdmPolicyTest(unittest.TestCase):
    """The build diagnostic reproduces tuna-gdm-default-session.sh's choice."""

    def test_native_admission_reads_the_existing_policy_package_response(self):
        def command(argv):
            if argv[:2] == ["pacman", "-Q"]:
                version = "51.1.1-1.1" if argv[2] in ("gdm", "libgdm") else "23.13.9-1"
                stdout = f"{argv[2]} {version}\n".encode()
            elif argv[-1] == "--version":
                stdout = b"GDM 51.1.1\n"
            else:
                stdout = b""
            return {"returncode": 0, "stdout": stdout.decode(), "truncated": False}, stdout

        scope = guest_scope(bounded_command=command, template_observation=lambda kind: {})
        with mock.patch.object(shutil, "which", return_value="/usr/bin/gdm"):
            policy = scope["gdm_policy"]()
        self.assertEqual(fresh_accounts.native_installed_pair(policy),
                         {"gdm": "51.1.1-1.1", "libgdm": "51.1.1-1.1"})

    def test_version_parse_matches_the_helper(self):
        major = guest_scope()["gdm_version_major"]
        self.assertEqual(major(b"GDM 51.1.1\n", 0), 51)
        self.assertEqual(major(b"gdm3 50.0-1", 0), 50)
        self.assertEqual(major(b"GDM 51.1\0\n\n", 0), 51)
        self.assertIsNone(major(b"GDM 51.1.1\n", 1))
        self.assertIsNone(major(b"GDM 51.1.1\nwarning", 0))
        self.assertIsNone(major(b"GDM 51", 0))
        self.assertIsNone(major(b"", 0))

    def policy(self, script, gdm=True, pacman=True):
        with tempfile.TemporaryDirectory() as root:
            binaries = pathlib.Path(root, "bin")
            binaries.mkdir()
            real = pathlib.Path(root, "real/gdm-daemon")
            real.parent.mkdir()
            real.write_text(script)
            real.chmod(0o755)
            if gdm:
                (binaries / "gdm").symlink_to(real)
            if pacman:
                (binaries / "pacman").write_text('#!/bin/sh\necho "pacman $*"\n')
                (binaries / "pacman").chmod(0o755)
            write_template(root, 1, "standard", "[User]\nSession=\nIcon=/ICON-MARK\n")
            scope = guest_scope(ACCOUNTS_TEMPLATE_DIRS=template_roots(root))
            with mock.patch.dict(os.environ, {"PATH": f"{binaries}:{os.environ.get('PATH', '/usr/bin:/bin')}"}):
                result = scope["gdm_policy"](search_path=str(binaries))
            json.dumps(result)
            resolved = os.path.realpath(real)
        return result, str(binaries / "gdm"), resolved

    def test_capable_gdm_selects_empty_policy_with_provenance(self):
        result, binary, real = self.policy("#!/bin/sh\n# daemon/FallbackSession\necho 'GDM 51.1.1'\n")
        self.assertEqual((result["binary"], result["realpath"]), (binary, real))
        self.assertEqual(result["version"], {"returncode": 0, "stdout": "GDM 51.1.1\n", "truncated": False})
        self.assertEqual(result["version_major"], 51)
        self.assertEqual(result["fallback_literal"]["returncode"], 0)
        self.assertEqual(result["policy"], "empty")
        self.assertEqual(result["packages"]["libgdm"]["stdout"], "pacman -Q libgdm\n")
        self.assertEqual(result["owner"]["stdout"], f"pacman -Qo {real}\n")
        self.assertEqual(result["templates"]["standard"]["keys"]["Session"], [""])
        self.assertNotIn("ICON-MARK", json.dumps(result))

    def test_missing_capability_or_version_keeps_legacy(self):
        result = self.policy("#!/bin/sh\necho 'GDM 51.1.1'\n")[0]
        self.assertEqual((result["fallback_literal"]["returncode"], result["policy"]), (1, "legacy"))
        result = self.policy("#!/bin/sh\n# daemon/FallbackSession\necho 'GDM 50.3'\n")[0]
        self.assertEqual((result["version_major"], result["policy"]), (50, "legacy"))
        result = self.policy("#!/bin/sh\n# daemon/FallbackSession\necho 'GDM 51.1.1'\nexit 3\n")[0]
        self.assertEqual(result["version"]["returncode"], 3)
        self.assertEqual((result["version_major"], result["policy"]), (None, "legacy"))

    def test_absent_binary_and_tools_are_explicit(self):
        result = self.policy("", gdm=False, pacman=False)[0]
        self.assertEqual((result["binary"], result["realpath"]), (None, None))
        self.assertEqual(result["version"], {"error": "no-binary"})
        self.assertEqual(result["fallback_literal"], {"error": "no-binary"})
        self.assertEqual(result["owner"], {"error": "no-binary"})
        self.assertEqual(result["policy"], "legacy")
        self.assertTrue(all(set(value) in ({"error"}, {"returncode", "stdout", "truncated"})
                            for value in result["packages"].values()))

    def test_version_output_is_bounded(self):
        result = self.policy("#!/bin/sh\nprintf 'x%.0s' $(seq 1000)\n")[0]
        self.assertEqual(len(result["version"]["stdout"]), 256)
        self.assertTrue(result["version"]["truncated"])
        self.assertEqual(result["policy"], "legacy")


TRACE_PATH = ROOT / "packaging/marlin/vm-lane/tuna-vm-gdm-trace"
TRACE = SimpleNamespace(**runpy.run_path(str(TRACE_PATH)))


class GdmHeaderTraceTest(unittest.TestCase):
    def call(self, serial="7", member="SetSession", path=TRACE.OBJECT):
        return f"mc\t1.000001\t{serial}\t:1.20\torg.freedesktop.Accounts\t{path}\t{TRACE.INTERFACE}\t{member}"

    def test_only_target_setters_and_correlated_replies_survive(self):
        headers = TRACE.Headers()
        headers.add(self.call())
        headers.add(self.call(path="/org/freedesktop/Accounts/User1002"))
        headers.add(self.call(member="SetPassword"))
        headers.add("mr\t1.000002\t91\t:1.3\t:1.99\t7")
        headers.add("mr\t1.000003\t92\t:1.3\t:1.20\t7")
        headers.add(self.call(serial="8", member="SetSessionType"))
        headers.add("err\t1.000004\t93\t:1.3\t:1.20\t8")
        self.assertEqual([event["kind"] for event in headers.events], ["call", "return", "call", "error"])
        self.assertEqual([event["reply_serial"] for event in headers.events if "reply_serial" in event], [7, 8])
        self.assertEqual(headers.pending, {})

    def test_payload_lines_and_event_flood_are_discarded(self):
        headers = TRACE.Headers()
        for line in ('   string "PRIVATE-PASSWORD"', 'PRIVATE-ERROR-BODY', "method call payload"):
            headers.add(line)
        self.assertEqual(headers.events, [])
        for number in range(TRACE.EVENT_BOUND + 10):
            headers.add(self.call(serial=str(number)))
        self.assertEqual(len(headers.events), TRACE.EVENT_BOUND)
        self.assertNotIn("PRIVATE", json.dumps(headers.events))

    def test_worker_marker_projection_never_enables_debug(self):
        with tempfile.TemporaryDirectory() as root:
            proc = pathlib.Path(root, "123")
            proc.mkdir()
            (proc / "comm").write_text("gdm-session-wor\n")
            (proc / "cmdline").write_bytes(b"gdm-session-worker [pam/gdm-password]\0")
            executable = pathlib.Path(root, "gdm-session-worker")
            executable.touch()
            (proc / "exe").symlink_to(executable)
            private = "PRIVATE-CREDENTIAL"
            lines = [
                {"_PID": "123", "MESSAGE": "Gdm: GdmSessionWorker: saving account details for user tunastd"},
                {"_PID": "123", "MESSAGE": f"GdmSessionWorker: session name set to tuna {private}"},
                {"_PID": "999", "MESSAGE": "GdmSessionWorker: state SESSION_OPENED"},
            ]
            completed = (0, b"\n".join(json.dumps(row).encode() for row in lines))
            real_path = pathlib.Path
            run = mock.Mock(return_value=completed)
            with mock.patch.dict(TRACE.worker_markers.__globals__,
                                 {"pathlib": SimpleNamespace(Path=lambda path: real_path(root if path == "/proc" else path)),
                                  "bounded_journal": run}):
                result = TRACE.worker_markers(1000.0)
            self.assertEqual(result["markers"], [{"pid": 123, "marker": "saving-account-details"}])
            self.assertFalse(result["debug_enabled_by_probe"])
            self.assertNotIn(private, json.dumps(result))
            self.assertEqual(run.call_args.args[0][0], "journalctl")
            self.assertEqual(run.call_count, 1)

    def test_missing_worker_markers_are_inconclusive(self):
        with tempfile.TemporaryDirectory() as root:
            with mock.patch.dict(TRACE.worker_markers.__globals__,
                                 {"pathlib": SimpleNamespace(Path=lambda _: pathlib.Path(root))}):
                result = TRACE.worker_markers(1000.0)
        self.assertEqual(result["availability"], "unavailable-or-debug-disabled")
        self.assertEqual(result["markers"], [])
        self.assertFalse(result["debug_enabled_by_probe"])

    def test_journal_output_is_bounded_before_parsing(self):
        status, output = TRACE.bounded_journal([sys.executable, "-c", "print('PRIVATE' * 20000)"])
        self.assertEqual((status, output), ("journal-output-bound", b""))

    @unittest.skipUnless(all(shutil.which(name) for name in ("dbus-daemon", "dbus-monitor", "dbus-send")),
                         "private D-Bus test needs dbus tools")
    def test_real_profile_has_headers_and_correlated_error_but_no_argument(self):
        # An isolated bus, never the host's system bus or real AccountsService.
        daemon = subprocess.Popen(["dbus-daemon", "--session", "--nofork", "--print-address=1"],
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True)
        monitor = None
        try:
            address = daemon.stdout.readline().strip()
            monitor = subprocess.Popen(["dbus-monitor", "--address", address, "--profile", *TRACE.RULES],
                                       stdout=subprocess.PIPE, stderr=subprocess.DEVNULL)
            data = b""
            deadline = time.monotonic() + 3
            while b"#type\t" not in data and time.monotonic() < deadline:
                if select.select([monitor.stdout], [], [], 0.1)[0]:
                    data += os.read(monitor.stdout.fileno(), 4096)
            self.assertIn(b"#type\t", data)
            subprocess.run(["dbus-send", f"--bus={address}", "--print-reply", "--reply-timeout=1000",
                            "--dest=org.freedesktop.Accounts", TRACE.OBJECT,
                            f"{TRACE.INTERFACE}.SetSession", "string:PRIVATE-SESSION-ARGUMENT"],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=2, check=False)
            headers = TRACE.Headers()
            deadline = time.monotonic() + 3
            while not any(event["kind"] == "error" for event in headers.events) and time.monotonic() < deadline:
                if select.select([monitor.stdout], [], [], 0.1)[0]:
                    data += os.read(monitor.stdout.fileno(), 4096)
                    headers = TRACE.Headers()
                    for line in data.decode().splitlines():
                        headers.add(line)
            self.assertEqual([event["kind"] for event in headers.events], ["call", "error"])
            self.assertNotIn(b"PRIVATE-SESSION-ARGUMENT", data)
            self.assertNotIn(b"ServiceUnknown", data)  # No error name/body in profile mode.
        finally:
            if monitor is not None:
                monitor.terminate()
                monitor.wait(timeout=2)
                monitor.stdout.close()
            daemon.terminate()
            daemon.wait(timeout=2)
            daemon.stdout.close()


if __name__ == "__main__":
    unittest.main()
