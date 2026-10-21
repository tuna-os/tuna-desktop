#!/usr/bin/env python3
"""Synthetic public-control/QMP tests. Never execute a VM, image, PAM or GDM."""
import copy
import ast
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import published_gdm_login as gdm


BOOT = "12345678-1234-1234-1234-123456789abc"
IMAGE = "ghcr.io/tuna-os/marlin@sha256:" + "c" * 64
SUBJECT = {"image": IMAGE, "platform_digest": "sha256:" + "d" * 64}
PRINCIPAL = {"pid": 120, "start_ticks": 90, "uid": 120,
             "executable": "/usr/bin/gnome-shell", "logind_session": "c1",
             "direct_logind_session": None, "logind_join_method": "logind-user-display-and-gdm-user-unit"}
RECTS = {kind: [20 + index * 100, 100, 90, 30]
         for index, kind in enumerate(sorted(gdm.KINDS))}


class Fake:
    def __init__(self, route="fixture-user"):
        self.now = 0.0
        self.route = route
        self.page = "password" if route == "preselected-password" else "user"
        self.focused = None
        self.menu = False
        self.selected = False
        self.error = False
        self.entered = ""
        self.desktop = False
        self.negative_count = 0
        self.positive_count = 0
        self.actions = []
        self.observations = []
        self.frames = []
        self.transform = lambda action, value: value
        self.selection_works = True
        self.selection_lost = False
        self.wrong_starts_desktop = False
        self.correct_fails = False
        self.paint_works = True
        self.late_action = None
        self.late_paint = None
        self.raise_private = None
        self.reject_launch = False
        self.launch_calls = []
        self.last_inventory = None
        self.boot = BOOT
        self.principal = dict(PRINCIPAL)
        self.owner = {
            "profile": gdm.PROFILE, "boot_id": BOOT, "session_uid": 1000,
            "active_vt": "tty4",
            "session": {"Id": "7", "User": "1000", "Service": "gdm-password",
                        "Class": "user", "Type": "wayland", "Seat": "seat0",
                        "Remote": "no", "VTNr": "4", "Active": "yes", "State": "active"},
            "compositor_identity": {"pid": 200, "start_ticks": 300, "uid": 1000,
                                    "executable": "/usr/bin/tuna-compositor", "logind_session": "7"},
        }

    def monotonic(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds

    def control(self, kind, *, enabled=True):
        return {"kind": kind, "rect": list(RECTS[kind]), "showing": True,
                "enabled": enabled, "focused": self.focused == kind,
                "selected": kind == "tuna-session" and self.selected}

    def ui(self):
        kinds = []
        if self.page == "user":
            kinds = [self.route if self.route != "preselected-password" else "fixture-user"]
        elif self.page == "username":
            kinds = ["username-entry", "cancel"]
        elif self.page == "password":
            kinds = ["password-entry", "session-menu", "sign-in", "cancel"]
            if self.error:
                kinds.append("auth-error")
            if self.menu:
                kinds.append("tuna-session")
        return {"schema_version": 1, "profile": gdm.PROFILE, "boot_id": self.boot,
                "greeter": dict(self.principal), "active_vt": "tty1",
                "bus_owner_verified": True, "input_value_observed": False,
                "controls": [self.control(kind, enabled=kind != "auth-error") for kind in kinds]}

    def observe(self, action, *, timeout):
        self.observations.append((action, timeout))
        if self.raise_private == action:
            raise RuntimeError("private-password-or-environment-do-not-retain")
        if self.late_action == action:
            self.now += timeout + 1
        if action == "gdm-greeter":
            value = {"schema_version": 1, "profile": gdm.PROFILE, "boot_id": self.boot,
                     "greeter": dict(self.principal), "active_vt": "tty1", "ready": True,
                     "desktop_session_present": self.desktop}
        elif action == "gdm-ui":
            value = self.ui()
        elif action == "inventory":
            value = copy.deepcopy(self.owner) if self.desktop else {"ready": False}
            if self.desktop:
                value["launch_evidence"] = {"synthetic_launch_observation": True}
        elif action == "published":
            value = {"profile": gdm.PROFILE, "immutable_verified": True, "subject": dict(SUBJECT)}
        else:
            raise AssertionError(action)
        result = self.transform(action, value)
        if action == "inventory":
            self.last_inventory = result
        return result

    def paint(self, stage, *, timeout):
        self.frames.append(stage)
        if self.late_paint == stage:
            self.now += timeout + 1
        return {"painted": self.paint_works}

    def click(self, x, y):
        self.now += 0.01
        target = next(kind for kind, (a, b, w, h) in RECTS.items()
                      if a <= x < a + w and b <= y < b + h)
        self.actions.append(("click", target))
        if target == "fixture-user":
            self.page = "password"
        elif target == "not-listed":
            self.page = "username"
        elif target == "cancel":
            self.page = "user"
        elif target == "session-menu":
            self.menu = not self.menu
        elif target == "tuna-session":
            self.selected = self.selection_works
            self.menu = False
        elif target in ("password-entry", "username-entry"):
            self.focused = target

    def keys(self, *keys):
        self.now += 0.01
        self.actions.append(("keys", keys))
        if keys == ("ctrl", "a"):
            self.entered = ""
        elif keys == ("esc",):
            self.menu = False
        elif keys == ("ret",):
            if self.page == "username" and self.entered == "tuna-test":
                self.page = "password"
                self.focused = None
            elif self.page == "password":
                if self.entered == "incorrect":
                    self.negative_count += 1
                    self.error = True
                    self.desktop = self.wrong_starts_desktop
                    if self.selection_lost:
                        self.selected = False
                elif self.entered == "tuna":
                    self.positive_count += 1
                    self.desktop = not self.correct_fails
                else:
                    raise AssertionError("unexpected synthetic fixture input")
        else:
            self.entered += "-" if keys == ("minus",) else keys[0]

    def validate_launch(self, evidence, receipt):
        self.launch_calls.append((evidence, copy.deepcopy(receipt)))
        if self.reject_launch or evidence != {"synthetic_launch_observation": True}:
            raise gdm.ContractError("synthetic launch rejection")
        if evidence is not self.last_inventory["launch_evidence"]:
            raise AssertionError("host did not supply the actual observed evidence")
        if not all(receipt.get(key) is True for key in ("picker_selected", "wrong_password_rejected",
                   "user_to_password_challenge_observed", "immutable_verified")):
            raise AssertionError("host did not supply its actual public UI/password observations")
        if receipt["session_identity"] != gdm.validate_owner(self.last_inventory, self.boot):
            raise AssertionError("host did not supply the actual successful session identity")
        return {"effective_launch_verified": True, "direct_launcher_exec_observed": False,
                "method": "synthetic-validator-for-host-flow-tests",
                "observed": ["synthetic-runtime-observer"], "inferred": ["synthetic-source-inference"]}

    def run(self, **kwargs):
        # The independent runtime suite owns structural launch evidence checks.
        # Here the fake validator verifies the host joins its *actual* observed
        # UI and inventory rather than supplying fabricated acceptance fields.
        with patch.object(gdm, "validate_launch", side_effect=self.validate_launch):
            return gdm.login(self, self.observe, self.paint, monotonic=self.monotonic,
                             sleep=self.sleep, **kwargs)


class LoginFlow(unittest.TestCase):
    def failure(self, fake, pattern=None, **kwargs):
        with self.assertRaises(gdm.LoginError) as caught:
            fake.run(timeout=3, **kwargs)
        if pattern:
            self.assertRegex(str(caught.exception), pattern)
        receipt = caught.exception.receipt
        self.assertFalse(receipt["pass"])
        self.assertNotIn("private-password-or-environment", json.dumps(receipt))
        return receipt

    def test_visible_user_picker_negative_password_and_success(self):
        fake = Fake()
        result = fake.run()
        self.assertTrue(result["pass"])
        self.assertTrue(result["picker_selected"])
        self.assertTrue(result["wrong_password_rejected"])
        self.assertTrue(result["user_to_password_challenge_observed"])
        self.assertEqual((fake.negative_count, fake.positive_count), (1, 1))
        self.assertEqual(fake.actions.count(("click", "session-menu")), 3)
        self.assertEqual(fake.actions.count(("click", "tuna-session")), 1)
        self.assertEqual(result["session_identity"]["vt"], 4)
        self.assertFalse(result["direct_launcher_exec_observed"])
        self.assertTrue(result["effective_launch_verified"])
        self.assertTrue(result["launch_verdict"]["inferred"])
        self.assertEqual(len(fake.launch_calls), 1)
        self.assertFalse(fake.launch_calls[0][1]["pass"])
        self.assertEqual(fake.frames, ["greeter-before-login", "password-challenge", "picker-open",
                                      "picker-selected", "wrong-password-rejected", "authenticated-desktop"])
        self.assertNotIn("incorrect", json.dumps(result))
        budgets = [value for _, value in fake.observations]
        self.assertEqual(budgets, sorted(budgets, reverse=True))

    def test_not_listed_uses_observed_username_then_password(self):
        fake = Fake("not-listed")
        self.assertTrue(fake.run()["pass"])
        self.assertIn(("keys", ("minus",)), fake.actions)
        self.assertLess(fake.actions.index(("click", "not-listed")),
                        fake.actions.index(("click", "password-entry")))

    def test_preselected_password_is_reset_to_actual_user_selection(self):
        fake = Fake("preselected-password")
        self.assertTrue(fake.run()["pass"])
        self.assertLess(fake.actions.index(("click", "cancel")),
                        fake.actions.index(("click", "fixture-user")))

    def test_initial_username_challenge_is_supported(self):
        fake = Fake()
        fake.page = "username"
        self.assertTrue(fake.run()["pass"])

    def test_missing_disabled_hidden_and_ambiguous_picker_fail(self):
        for change in ("missing", "disabled", "hidden", "duplicate"):
            with self.subTest(change=change):
                fake = Fake()
                def transform(action, value):
                    if action != "gdm-ui":
                        return value
                    for control in list(value["controls"]):
                        if control["kind"] == "tuna-session":
                            if change == "missing":
                                value["controls"].remove(control)
                            elif change == "duplicate":
                                value["controls"].append(dict(control))
                            else:
                                control["enabled" if change == "disabled" else "showing"] = False
                    return value
                fake.transform = transform
                self.failure(fake)
                self.assertEqual(fake.negative_count, 0)

    def test_selected_state_is_mandatory_and_not_inferred_from_click(self):
        fake = Fake()
        fake.selection_works = False
        self.failure(fake, "selection was not observed")
        self.assertEqual(fake.negative_count, 0)

    def test_selection_must_survive_rejected_authentication(self):
        fake = Fake()
        fake.selection_lost = True
        self.failure(fake, "selection changed")
        self.assertEqual((fake.negative_count, fake.positive_count), (1, 0))

    def test_wrong_password_must_not_start_a_desktop(self):
        fake = Fake()
        fake.wrong_starts_desktop = True
        self.failure(fake, "before accepted password")
        self.assertEqual(fake.positive_count, 0)

    def test_wrong_password_requires_a_fresh_visible_error(self):
        fake = Fake()
        fake.error = True
        self.failure(fake, "stale authentication error")
        self.assertEqual(fake.negative_count, 0)
        fake = Fake()
        def without_error(action, value):
            if action == "gdm-ui":
                value["controls"] = [c for c in value["controls"] if c["kind"] != "auth-error"]
            return value
        fake.transform = without_error
        self.failure(fake, "deadline")
        self.assertEqual((fake.negative_count, fake.positive_count), (1, 0))

    def test_foreground_principal_and_accessibility_identity_must_match(self):
        changes = [("bus_owner_verified", False), ("input_value_observed", True),
                   ("boot_id", "22345678-1234-1234-1234-123456789abc"), ("active_vt", "tty2"),
                   ("greeter", {**PRINCIPAL, "pid": 121}), ("schema_version", True)]
        for key, value in changes:
            with self.subTest(key=key):
                fake = Fake()
                fake.transform = lambda action, data: {**data, key: value} if action == "gdm-ui" else data
                self.failure(fake)
                self.assertFalse(fake.actions)

    def test_original_greeter_cannot_change_midflow(self):
        fake = Fake()
        def replace(action, value):
            if fake.negative_count and action == "gdm-greeter":
                value["greeter"]["start_ticks"] += 1
            return value
        fake.transform = replace
        self.failure(fake, "original foreground greeter changed")

    def test_failed_password_and_late_observations_share_one_deadline(self):
        fake = Fake()
        fake.correct_fails = True
        result = self.failure(fake, "deadline")
        self.assertEqual(result["stage"], "desktop-identity")
        self.assertEqual((fake.negative_count, fake.positive_count), (1, 1))
        fake = Fake()
        fake.late_action = "gdm-ui"
        self.failure(fake, "deadline")
        self.assertFalse(fake.actions)

    def test_paint_is_required_and_late_paint_cannot_pass(self):
        fake = Fake()
        fake.paint_works = False
        self.failure(fake, "paint missing")
        self.assertFalse(fake.actions)
        fake = Fake()
        fake.late_paint = "authenticated-desktop"
        self.failure(fake, "deadline")

    def test_callback_exceptions_are_sanitized_and_preserve_partial_receipt(self):
        fake = Fake()
        fake.raise_private = "gdm-ui"
        result = self.failure(fake)
        self.assertEqual(result["painted_stages"], ["greeter-before-login"])
        self.assertEqual(result["error_type"], "RuntimeError")

    def test_immutable_payload_must_be_verified_before_and_after_login(self):
        for late in (False, True):
            fake = Fake()
            def reject(action, value):
                if action == "published" and (fake.desktop or not late):
                    value["immutable_verified"] = False
                return value
            fake.transform = reject
            self.failure(fake, "payload")

    def test_wrong_or_unjoined_desktop_is_rejected(self):
        for kind in ("service", "root", "pid-join", "path", "vt", "inactive", "boot"):
            fake = Fake()
            if kind == "service":
                fake.owner["session"]["Service"] = "gdm-autologin"
            elif kind == "root":
                fake.owner["session_uid"] = 0
            elif kind == "pid-join":
                fake.owner["compositor_identity"]["logind_session"] = "8"
            elif kind == "path":
                fake.owner["compositor_identity"]["executable"] = "/tmp/tuna-compositor"
            elif kind == "vt":
                fake.owner["active_vt"] = "tty1"
            elif kind == "inactive":
                fake.owner["session"].update(Active="no", State="online")
            else:
                fake.owner["boot_id"] = "22345678-1234-1234-1234-123456789abc"
            with self.subTest(kind=kind):
                self.failure(fake)

    def test_relogin_requires_same_boot_new_session_and_new_process_identity(self):
        previous = Fake().run()
        for mutation in ("unchanged", "same-session", "same-process", "new-boot", "success"):
            fake = Fake()
            if mutation in ("same-process", "success"):
                fake.owner["session"]["Id"] = "8"
                fake.owner["compositor_identity"]["logind_session"] = "8"
            if mutation in ("same-session", "success"):
                # A reused PID is safe only with a new kernel start identity.
                fake.owner["compositor_identity"]["start_ticks"] += 1
            if mutation == "new-boot":
                fake.boot = "22345678-1234-1234-1234-123456789abc"
            with self.subTest(mutation=mutation):
                if mutation == "success":
                    self.assertTrue(fake.run(previous=previous)["pass"])
                else:
                    self.failure(fake, previous=previous)

    def test_no_arbitrary_guest_text_or_fields_enter_receipts(self):
        fake = Fake()
        fake.transform = lambda action, value: {**value, "private": "never-retain-this"}
        result = fake.run()
        self.assertNotIn("never-retain-this", json.dumps(result))

    def test_bad_rectangles_and_unknown_control_state_fail_closed(self):
        for field, bad in (("rect", [-1, 0, 1, 1]), ("rect", [0, 0, 1281, 800]),
                           ("rect", [0, 0, True, 1]), ("focused", 1),
                           ("kind", "secret-field-value")):
            fake = Fake()
            def mutate(action, value):
                if action == "gdm-ui":
                    value["controls"][0][field] = bad
                return value
            fake.transform = mutate
            with self.subTest(field=field, bad=bad):
                self.failure(fake)

    def test_failure_receipt_callback_is_updated_in_place(self):
        fake = Fake()
        fake.paint_works = False
        receipt = {"stale": "discard"}
        result = self.failure(fake, receipt=receipt)
        self.assertIs(receipt, result)
        self.assertNotIn("stale", receipt)

    def test_qmp_socket_timeout_is_bounded_and_restored(self):
        class Socket:
            def __init__(self):
                self.value = None
                self.values = []
            def gettimeout(self):
                return self.value
            def settimeout(self, value):
                self.value = value
                self.values.append(value)
        fake = Fake()
        fake.sock = Socket()
        self.assertTrue(fake.run(timeout=10)["pass"])
        self.assertIsNone(fake.sock.value)
        self.assertTrue(all(v is None or 0 < v <= 10 for v in fake.sock.values))

    def test_launch_validator_failure_and_missing_evidence_propagate(self):
        for case in ("missing", "rejected"):
            fake = Fake()
            def corrupt(action, data):
                if action == "inventory" and fake.desktop and case == "missing":
                    del data["launch_evidence"]
                return data
            fake.transform = corrupt
            fake.reject_launch = case == "rejected"
            with self.subTest(case=case):
                self.failure(fake, "launch evidence")
                self.assertEqual(len(fake.launch_calls), 1)

    def test_runtime_published_subject_binds_the_exact_host_image_and_relogin(self):
        fake = Fake()
        result = fake.run(expected_image=IMAGE)
        self.assertEqual(result["subject"], SUBJECT)
        self.failure(Fake(), "host image", expected_image="ghcr.io/tuna-os/marlin@sha256:" + "e" * 64)
        fake = Fake()
        def changed(action, data):
            if action == "published" and fake.desktop:
                data["subject"]["platform_digest"] = "sha256:" + "e" * 64
            return data
        fake.transform = changed
        self.failure(fake, "payload changed", expected_image=IMAGE)
        result["subject"]["platform_digest"] = "sha256:" + "e" * 64
        self.failure(Fake(), "relogin published payload", previous=result, expected_image=IMAGE)

    def test_greeter_startup_retries_only_completed_observation_errors(self):
        fake = Fake()
        calls = 0
        original = fake.observe
        def observe(action, **kwargs):
            nonlocal calls
            if action == "gdm-greeter":
                calls += 1
                if calls == 1:
                    raise RuntimeError("not yet ready")
            return original(action, **kwargs)
        fake.observe = observe
        self.assertTrue(fake.run()["pass"])


class HostIntegration(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        path = Path(__file__).resolve().parents[1] / "tuna-vm-lane"
        loader = importlib.machinery.SourceFileLoader("published_gdm_host_tests", str(path))
        spec = importlib.util.spec_from_loader(loader.name, loader)
        cls.lane = importlib.util.module_from_spec(spec)
        loader.exec_module(cls.lane)
        cls.source = path.read_text()

    def test_default_profile_remains_current_greetd_and_gdm_is_explicit(self):
        tree = ast.parse(self.source)
        arg = next(n for n in ast.walk(tree) if isinstance(n, ast.Call)
                   and isinstance(n.func, ast.Attribute) and n.func.attr == "add_argument"
                   and n.args and isinstance(n.args[0], ast.Constant)
                   and n.args[0].value == "--display-manager-profile")
        values = {kw.arg: ast.literal_eval(kw.value) for kw in arg.keywords}
        self.assertEqual(values["default"], "current-greetd")
        self.assertEqual(values["choices"], ("current-greetd", "published-gdm"))

    def test_repeat_boots_forward_profile_and_preserve_five_pristine_observations(self):
        calls = []
        with tempfile.TemporaryDirectory() as directory:
            def run(command, check):
                calls.append(command)
                target = Path(command[command.index("--out") + 1])
                target.mkdir(parents=True, exist_ok=True)
                checks = {"V-GDM-LOGIN": {"pass": True}, "V-DRM": {"pass": True}}
                if "--tour" in command:
                    checks["V-RELOGIN"] = {"pass": True}
                (target / "manifest.json").write_text(json.dumps({
                    "display_manager_profile": "published-gdm", "assertions": checks}))
                return SimpleNamespace(returncode=0)
            args = SimpleNamespace(out=directory, disk="fixture.raw", timeout=120, boots=5,
                                   tour=True, greeter_login=False, require_kvm=True,
                                   orca_contract="distribution", meta=[], display_manager_profile="published-gdm")
            with patch.object(self.lane.subprocess, "run", side_effect=run):
                self.lane.repeat_boots(args)
            result = json.loads((Path(directory) / "repeat-manifest.json").read_text())
        self.assertEqual(len(calls), 5)
        self.assertTrue(all(command[command.index("--display-manager-profile") + 1] == "published-gdm"
                            and "--require-kvm" in command for command in calls))
        self.assertEqual(sum("--tour" in command for command in calls), 1)
        self.assertEqual(result["assertion_coverage"]["V-GDM-LOGIN"]["passed_boots"], [1, 2, 3, 4, 5])
        self.assertEqual(result["assertion_coverage"]["V-RELOGIN"]["observed_boots"], [5])

    def test_dynamic_away_vt_uses_actual_occupied_inventory(self):
        self.assertEqual(self.lane.published_away_vt({"session": {"VTNr": "4"}, "occupied_vts": [1, 2, 4]}), 3)
        for occupied in (None, [], [1], [1, 4, 4], [True, 4], list(range(1, 13))):
            with self.subTest(occupied=occupied), self.assertRaises(RuntimeError):
                self.lane.published_away_vt({"session": {"VTNr": "4"}, "occupied_vts": occupied})

    def test_lifecycle_adapter_rejects_identity_drift_and_pam_writes(self):
        fake = Fake()
        previous = fake.run()
        raw = SimpleNamespace(run=lambda *args, **kwargs: copy.deepcopy(fake.owner))
        agent = self.lane.PublishedSessionAgent(raw, previous)
        self.assertEqual(agent.run("inventory")["session"]["Id"], "7")
        fake.owner["compositor_identity"]["start_ticks"] += 1
        with self.assertRaises(RuntimeError):
            agent.run("inventory")
        for action in ("delay-unlock", "deny-unlock", "restore-unlock"):
            with self.subTest(action=action), self.assertRaisesRegex(RuntimeError, "cannot modify"):
                agent.run(action)

    def test_vt_away_is_explicit_inactive_and_original_identity_bound(self):
        fake = Fake()
        previous = fake.run()
        raw = SimpleNamespace(run=lambda *args, **kwargs: copy.deepcopy(fake.owner))
        agent = self.lane.PublishedSessionAgent(raw, previous)
        fake.owner["session"].update(Active="no", State="online")
        fake.owner["active_vt"] = "tty3"
        self.assertEqual(agent.inventory_away(3), fake.owner)
        with self.assertRaises(ValueError):
            agent.run("inventory")
        with self.assertRaises(ValueError):
            gdm.validate_owner(fake.owner, BOOT, require_active=False)
        for target in (4, 2, True, 13):
            with self.subTest(target=target), self.assertRaises(ValueError):
                agent.inventory_away(target)
        fake.owner["session"].update(Active="yes", State="active")
        with self.assertRaises(ValueError):
            agent.inventory_away(3)
        fake.owner["session"].update(Active="no", State="online")
        fake.owner["compositor_identity"]["start_ticks"] += 1
        with self.assertRaises(RuntimeError):
            agent.inventory_away(3)

    def test_host_relogin_derives_exact_previously_verified_image(self):
        fake = Fake()
        previous = fake.run()
        with tempfile.TemporaryDirectory() as directory, \
                patch.object(self.lane.published_gdm_login, "login", return_value=previous) as login:
            self.lane.gdm_login(fake, SimpleNamespace(run=fake.observe), directory,
                                lambda *args: None, previous=previous)
        self.assertEqual(login.call_args.kwargs["expected_image"], IMAGE)
        self.assertIs(login.call_args.kwargs["previous"], previous)
        previous.pop("subject")
        with tempfile.TemporaryDirectory() as directory, self.assertRaisesRegex(RuntimeError, "exact immutable image"):
            self.lane.gdm_login(fake, SimpleNamespace(run=fake.observe), directory,
                                lambda *args: None, previous=previous)

    def test_published_pam_branch_cannot_run_injected_delay_or_deny(self):
        tree = ast.parse(self.source)
        lifecycle = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "run_lifecycle")
        def actions(nodes):
            return [node.args[0].value for top in nodes for node in ast.walk(top)
                    if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
                    and isinstance(node.func.value, ast.Name) and node.func.value.id == "agent"
                    and node.func.attr == "run" and node.args and isinstance(node.args[0], ast.Constant)]
        branch = next(node for node in lifecycle.body if isinstance(node, ast.If)
                      and isinstance(node.test, ast.Name) and node.test.id == "gdm_profile"
                      and "delay-unlock" in actions(node.orelse))
        self.assertNotIn("delay-unlock", actions(branch.body))
        self.assertNotIn("restore-unlock", actions(branch.body))
        deny = next(node for node in lifecycle.body if isinstance(node, ast.If)
                    and "deny-unlock" in actions(node.body))
        self.assertIsInstance(deny.test, ast.UnaryOp)
        self.assertIsInstance(deny.test.op, ast.Not)
        self.assertEqual(deny.test.operand.id, "gdm_profile")

    def test_logout_requires_exact_original_retirement_and_live_semantic_paint(self):
        for failure in (None, "session", "compositor", "boot", "ui"):
            fake = Fake()
            previous = fake.run()
            fake.desktop = False
            fake.page = "user"
            calls, records = [], []
            def run(action, *arguments, **kwargs):
                calls.append((action, arguments))
                if action in ("logout", "gdm-departed"):
                    expected = {"boot_id": previous["boot_id"], "session_identity": previous["session_identity"]}
                    self.assertEqual(json.loads(arguments[0]), expected)
                if action == "logout":
                    return {"requested": "terminate original session"}
                if action == "gdm-departed":
                    return {"profile": "published-gdm", "boot_id": BOOT if failure != "boot" else "changed",
                            "original_session_gone": failure != "session",
                            "original_compositor_gone": failure != "compositor"}
                value = fake.observe(action, **kwargs)
                if action == "gdm-greeter":
                    value["greeter"]["extra_safe_identity"] = "not used"
                if action == "gdm-ui" and failure == "ui":
                    value["bus_owner_verified"] = False
                return value
            clock = SimpleNamespace(monotonic=fake.monotonic, sleep=fake.sleep)
            with self.subTest(failure=failure), tempfile.TemporaryDirectory() as directory, \
                    patch.object(self.lane, "time", clock), \
                    patch.object(self.lane, "gdm_paint", return_value={"painted": True}) as paint:
                if failure is None:
                    receipt = self.lane.published_logout(fake, SimpleNamespace(run=run), directory, previous,
                                                        lambda *args: records.append(args))
                    self.assertTrue(receipt["pass"])
                    self.assertEqual(paint.call_count, 1)
                    self.assertEqual(records[-1][:2], ("V-LOGOUT", True))
                else:
                    with self.assertRaises((RuntimeError, ValueError)):
                        self.lane.published_logout(fake, SimpleNamespace(run=run), directory, previous,
                                                  lambda *args: records.append(args))
                    self.assertEqual(paint.call_count, 0)
                    self.assertEqual(records[-1][:2], ("V-LOGOUT", False))


if __name__ == "__main__":
    unittest.main()
