"""Record-free fresh-account journey (tunaOS#4092): host journey against a
simulated guest, plus the guest probe's record detection."""
import ast
import json
import os
import pathlib
import runpy
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

ROOT = pathlib.Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/lib"))
import fresh_accounts  # noqa: E402

GUEST = ROOT / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
CONF = "[daemon]\nFallbackSession=tuna\n"


class FakeGuest:
    """A guest whose GDM logs in after the QMP keyboard typed credentials.
    Like the GNOME reference, login alone need not write a cache record."""

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
        self.now = 0.0
        self.phase = "username"
        self.prompt_since = 0.0
        self.sent = []
        self.frames = []

    # QMP
    def keys(self, *names):
        if names == ("esc",):
            self.typed = []
            self.phase = "username"
            self.prompt_since = self.now
        if names == ("ret",) and len(self.typed) == 1:
            self.phase = "password"
            self.prompt_since = self.now
        if names == ("ret",) and len(self.typed) >= 2:
            user, password = self.typed[-2], self.typed[-1]
            if password == "tuna" and user in self.uids and self.logged_in is None:
                self.logged_in = user
                self.logins.append((self.boot, user))
                if self.faults.get("write_record") or "saved" in self.faults:
                    self.records[user] = self.faults.get("saved", "[User]\nSession=tuna\nSystemAccount=false\n")
            self.typed = []

    def type_text(self, text):
        prompt = self.run("greeter-prompt")["prompt"]
        if prompt != ("username" if not self.typed else "password"):
            raise AssertionError("credentials typed before the real prompt was ready")
        self.sent.append((self.now, prompt, text))
        self.typed.append(text)

    def frame(self, path):
        self.frames.append((pathlib.Path(path).name, self.now, self.phase))

    # guest agent
    def connect(self):
        return self

    def close(self):
        pass

    def run(self, action, *args):
        if action == "greeter":
            boot = self.boot if not self.faults.get("stale_boot") else 1
            return {"ready": self.logged_in is None, "boot_id": f"boot-{boot}"}
        if action == "greeter-prompt":
            delay = self.faults.get(f"{self.phase}_delay", 0)
            ready = not self.faults.get("no_prompt") and self.now - self.prompt_since >= delay
            prompt = self.phase if ready else (
                "username" if self.phase == "password" and not self.faults.get("no_prompt") else None)
            return {"pid": 1131, "uid": 60578, "start_ticks": 1200, "atspi_owner": ":1.7",
                    "prompt": prompt}
        if action == "fresh-state":
            user = args[0]
            if user in self.faults.get("missing", ()):
                return {"user": user, "exists": False}
            return {"user": user, "exists": True, "uid": self.uids[user], "groups": self.groups[user],
                    "home": f"/var/home/{user}", "home_exists": True, "record": self.records.get(user),
                    "effective_session": (None if self.faults.get("lost_session") and self.boot > 1
                                          else self.faults.get("effective_session", "tuna")),
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
                self.phase = "username"
                self.prompt_since = self.now
            return {"user": args[0]}
        if action == "reboot":
            before = f"boot-{self.boot}"
            self.boot += 1
            self.logged_in = None
            self.phase = "username"
            self.prompt_since = self.now
            return {"boot_id": before}
        raise ValueError(action)


def journey(guest):
    results = {}
    clock = [0.0]

    def sleep(seconds):
        clock[0] += seconds
        guest.now = clock[0]

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
        passed, results, error, _ = journey(guest)
        self.assertTrue(passed, (results, error))
        self.assertTrue(all(ok for ok, _ in results.values()), results)
        self.assertEqual(guest.logins, [(1, "tunastd"), (1, "tunaadmin"), (1, "tunastd"), (1, "tunaadmin"),
                                        (2, "tunastd"), (2, "tunaadmin")])

    def test_late_focused_username_and_password_before_single_attempt(self):
        guest = FakeGuest(username_delay=1, password_delay=2)
        passed, results, error, _ = journey(guest)
        self.assertTrue(passed, (results, error))
        self.assertEqual(len(guest.sent), 12)  # Exactly one name/password pair per login.
        self.assertEqual([kind for _, kind, _ in guest.sent], ["username", "password"] * 6)
        self.assertGreaterEqual(guest.sent[0][0], 1)
        self.assertGreaterEqual(guest.sent[1][0] - guest.sent[0][0], 2)

    def test_manager_and_greeter_process_without_prompt_do_not_receive_keys(self):
        guest = FakeGuest(no_prompt=True)
        passed, results, error, receipts = journey(guest)
        self.assertFalse(passed)
        self.assertIn("greeter did not become ready", error)
        self.assertEqual(guest.sent, [])
        self.assertEqual(guest.now, 5)
        self.assertTrue(receipts)

    def test_prompt_waits_share_original_login_deadline(self):
        guest = FakeGuest(username_delay=2, password_delay=4)
        passed, results, error, _ = journey(guest)
        self.assertFalse(passed)
        self.assertIsNone(error)
        self.assertFailed(results, "V-FRESH-LOGIN-tunastd-first")
        self.assertIn("password prompt", results["V-FRESH-LOGIN-tunastd-first"][1])
        self.assertEqual(guest.sent, [(4, "username", "tunastd")])
        # Preflight uses its original 5s bound; login gets one original 5s bound,
        # not a new timeout per prompt or an authentication retry.
        self.assertEqual(guest.now, 7)
        self.assertEqual(guest.logins, [])
        self.assertEqual(guest.frames[-1][0], "tunastd-first-password-timeout.png")

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

    def test_saved_tuna_record_is_supported(self):
        passed, results, _, _ = journey(FakeGuest(write_record=True))
        self.assertTrue(passed, results)

    def test_effective_session_must_be_tuna(self):
        for session in ("", None, "gnome"):
            with self.subTest(session=session):
                passed, results, _, _ = journey(FakeGuest(effective_session=session))
                self.assertFalse(passed)
                self.assertFailed(results, "V-FRESH-SESSION-tunastd-first")

    def test_effective_session_must_reload_after_reboot(self):
        passed, results, _, _ = journey(FakeGuest(lost_session=True))
        self.assertFalse(passed)
        self.assertFailed(results, "V-FRESH-SESSION-RELOADED-tunastd")

    def test_saved_gnome_session_fails(self):
        passed, results, _, _ = journey(FakeGuest(saved="[User]\nSession=gnome\n"))
        self.assertFalse(passed)
        self.assertFailed(results, "V-FRESH-SESSION-tunastd-first")

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
        passed, results, error, _ = journey(FakeGuest(stale_boot=True))
        self.assertFalse(passed)
        self.assertIn("reboot", error)
        self.assertNotIn("V-FRESH-REBOOT", results)


class NativePromptTreeTest(unittest.TestCase):
    """Exercise the actual guest traversal, not a copied/extracted source body."""

    @classmethod
    def setUpClass(cls):
        with mock.patch("pwd.getpwnam", return_value=SimpleNamespace(pw_uid=1001)):
            cls.probe = staticmethod(runpy.run_path(str(GUEST))["greeter_prompt_entries"])
        cls.api = SimpleNamespace(
            Role=SimpleNamespace(TEXT=61, ENTRY=79, PASSWORD_TEXT=40),
            StateType=SimpleNamespace(**{name: name for name in
                ("FOCUSED", "SHOWING", "VISIBLE", "EDITABLE", "SENSITIVE", "ENABLED")}))
        cls.ready = {"FOCUSED", "SHOWING", "VISIBLE", "EDITABLE", "SENSITIVE", "ENABLED"}

    class Node:
        def __init__(self, role=39, flags=(), children=(), hidden=False):
            self.role, self.flags, self.children = role, set(flags), list(children)
            self.hidden = hidden

        def clear_cache(self):
            pass

        def get_state_set(self):
            return SimpleNamespace(contains=self.flags.__contains__)

        def get_role(self):
            return self.role

        def get_child_count(self):
            if self.hidden:
                raise AssertionError("hidden actor subtree must not be enumerated")
            return len(self.children)

        def get_child_at_index(self, index):
            return self.children[index]

    def observe(self, root):
        return self.probe(root, self.api, lambda method, *args: method(*args))

    def test_hidden_actor_population_does_not_exhaust_prompt_budget(self):
        # Native GNOME: 1267 unique actors, but only 131 visited with SHOWING
        # pruning. Application metadata itself lacks SHOWING and VISIBLE.
        prompt = self.Node(role=61, flags=self.ready)
        for _ in range(10):
            prompt = self.Node(flags={"SHOWING", "VISIBLE"}, children=[prompt])
        hidden = self.Node(children=[self.Node() for _ in range(1200)], hidden=True)
        root = self.Node(role=75, children=[prompt, hidden])
        self.assertEqual(self.observe(root), ["username"])

    def test_actual_password_role_is_distinct(self):
        root = self.Node(role=75, children=[self.Node(role=40, flags=self.ready)])
        self.assertEqual(self.observe(root), ["password"])

    def test_hidden_or_insensitive_entry_never_becomes_ready(self):
        for missing in ("SHOWING", "VISIBLE", "EDITABLE", "FOCUSED", "SENSITIVE", "ENABLED"):
            with self.subTest(missing=missing):
                root = self.Node(role=75, children=[
                    self.Node(role=40, flags=self.ready - {missing}, hidden=missing == "SHOWING")])
                self.assertEqual(self.observe(root), [])

    def test_showing_scene_retains_original_node_bound(self):
        root = self.Node(role=75, children=[
            self.Node(flags={"SHOWING"}, children=[self.Node(flags={"SHOWING"}) for _ in range(511)])])
        with self.assertRaisesRegex(RuntimeError, "tree exceeds bound"):
            self.observe(root)


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
            def call(*args):
                if args[0] == "id":
                    return "tunastd"
                if "FindUserByName" in args:
                    return json.dumps({"type": "o", "data": ["/org/freedesktop/Accounts/User1001"]})
                return json.dumps({"type": "s", "data": "tuna"})

            scope = {"pwd": SimpleNamespace(getpwnam=lambda name: entry), "os": os, "pathlib": pathlib,
                     "json": json, "ACCOUNTS_RECORD_DIR": users, "GDM_CUSTOM_CONF": conf, "call": call}
            exec(compile(ast.Module(body=body, type_ignores=[]), str(GUEST), "exec"), scope)
            state = scope["fresh_state"]("tunastd")
        return state

    def test_absent_record(self):
        state = self.probe({})
        self.assertIsNone(state["record"])
        self.assertEqual(state["effective_session"], "tuna")

    def test_present_record_detected(self):
        state = self.probe({"tunastd": "[User]\nSystemAccount=false\n"})
        self.assertEqual(state["record"], "[User]\nSystemAccount=false\n")
        self.assertFalse(fresh_accounts.record_free(state, False)[0])

    def test_absent_record_is_record_free(self):
        self.assertTrue(fresh_accounts.record_free(self.probe({}), False)[0])



if __name__ == "__main__":
    unittest.main()
