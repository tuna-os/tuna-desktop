#!/usr/bin/env python3
"""Fake-only tests for the explicit user-display/unit greeter join."""
import copy
import io
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import published_gdm_greeter as greeter
from published_dm_contract import ContractError


KERNEL_GROUP = greeter.kernel_group
MODE_VERIFIED = greeter.mode_verified
PID, UID, TICKS = 123, 120, 456
CGROUP = f"/user.slice/user-{UID}.slice/user@{UID}.service/session.slice/{greeter.UNIT}"
PROCESS = {"pid": PID, "uid": UID, "start_ticks": TICKS, "ppid": 88,
           "executable": "/usr/bin/gnome-shell", "sha256": "a" * 64,
           "exe_device": 1, "exe_inode": 2, "logind_session": None,
           "environment": {"XDG_SESSION_TYPE": "wayland"}}
SESSION = {"Id": "c1", "User": str(UID), "Service": "gdm-launch-environment",
           "Class": "greeter", "Type": "wayland", "Remote": "no", "Seat": "seat0",
           "Active": "yes", "State": "active", "VTNr": "2"}
UNIT = {"MainPID": str(PID), "ControlGroup": CGROUP, "FragmentPath": greeter.FRAGMENT,
        "DropInPaths": "", "ActiveState": "active", "SubState": "running"}
FILE = {"type": "file", "owner": "gnome-shell", "sha256": "b" * 64,
        "uid": 0, "mode": 0o644, "size": 1024,
        "resolved_path": greeter.FRAGMENT, "parent_links": []}


class Collector:
    def __init__(self):
        self.uid = 1000
        self.baseline = {"packages": dict(greeter.VERSIONS), "files": {greeter.FRAGMENT: dict(FILE)}}
        self.calls = []
        self.process_calls = []
        self.displays = ["c1", "c1"]
        self.units = [dict(UNIT), dict(UNIT)]
        self.processes = [copy.deepcopy(PROCESS), copy.deepcopy(PROCESS)]
        self.budget = 12

    def call(self, *argv):
        self.calls.append(argv)
        self.remaining()
        if argv[:2] == ("/usr/bin/loginctl", "show-user"):
            return "Display=" + self.displays.pop(0)
        if argv[0] == "/usr/bin/runuser":
            return "\n".join(k + "=" + v for k, v in self.units.pop(0).items())
        if argv[:2] == ("/usr/bin/pacman", "-Qqo"):
            return "gnome-shell"
        raise AssertionError("unexpected subprocess")

    def process(self, pid, environment=False):
        self.process_calls.append((pid, environment))
        self.remaining()
        return self.processes.pop(0)

    def remaining(self):
        if self.budget <= 0:
            raise ContractError("deadline")
        return self.budget


class Inventory:
    instances = []
    observed = FILE

    def __init__(self, root, query):
        self.root, self.query = root, query
        self.instances.append(self)

    def required(self, path, owner):
        assert path == greeter.FRAGMENT and owner == "gnome-shell"
        assert self.query(["-Qqo", "--", path]) == owner
        return dict(self.observed)


class Tests(unittest.TestCase):
    def setUp(self):
        self.collector = Collector()
        Inventory.instances = []
        Inventory.observed = FILE
        self.patches = [patch.object(greeter.pwd, "getpwnam", return_value=SimpleNamespace(pw_uid=UID)),
                        patch.object(greeter, "Inventory", Inventory),
                        patch.object(greeter, "kernel_group", return_value=CGROUP),
                        patch.object(greeter, "mode_verified", return_value=True)]
        for p in self.patches:
            p.start()
            self.addCleanup(p.stop)

    def join(self, process=None, session=None):
        return greeter.join_greeter(self.collector, copy.deepcopy(PROCESS if process is None else process),
                                   dict(SESSION if session is None else session))

    def test_indirect_join_and_no_input_mutation(self):
        original = copy.deepcopy(PROCESS)
        result = greeter.join_greeter(self.collector, original, dict(SESSION))
        self.assertEqual(original, PROCESS)
        self.assertIsNone(result["direct_logind_session"])
        self.assertEqual(result["logind_session"], "c1")
        self.assertEqual(result["logind_join_method"], greeter.METHOD)
        self.assertEqual(result["greeter_join"]["display"], {"uid": UID, "session_id": "c1"})
        self.assertTrue(result["greeter_join"]["launch_mode_verified"])
        self.assertEqual(self.collector.process_calls, [(PID, True), (PID, True)])
        self.assertEqual(len(Inventory.instances), 2)
        self.assertIsNot(Inventory.instances[0], Inventory.instances[1])

    def test_exact_fixed_read_commands(self):
        self.join()
        calls = self.collector.calls
        login = ("/usr/bin/loginctl", "show-user", str(UID), "--property=Display")
        self.assertEqual(calls.count(login), 2)
        expected = ("/usr/bin/runuser", "-u", "gdm", "--", "/usr/bin/env", "-i",
                    "PATH=/usr/bin", "LC_ALL=C", f"XDG_RUNTIME_DIR=/run/user/{UID}",
                    f"DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/{UID}/bus",
                    "/usr/bin/systemctl", "--user", "show", greeter.UNIT, "--no-pager",
                    *("--property=" + k for k in greeter.PROPERTIES))
        self.assertEqual(calls.count(expected), 2)

    def test_existing_direct_join_must_agree(self):
        process = {**PROCESS, "logind_session": "c1"}
        self.collector.processes = [copy.deepcopy(process), copy.deepcopy(process)]
        self.assertEqual(self.join(process)["direct_logind_session"], "c1")
        with self.assertRaises(ContractError): self.join({**PROCESS, "logind_session": "c2"})

    def test_fixture_uid_must_not_substitute_for_gdm(self):
        with self.assertRaises(ContractError): self.join({**PROCESS, "uid": self.collector.uid})
        self.collector.uid = UID
        with self.assertRaises(ContractError): self.join()

    def test_tuple_must_be_active_local_gdm_greeter(self):
        for key, value in {"User": "1000", "Service": "gdm-password", "Class": "user",
                           "Type": "x11", "Remote": "yes", "Seat": "seat1", "Active": "no",
                           "State": "closing", "Id": "../bad", "VTNr": "0"}.items():
            with self.subTest(key=key), self.assertRaises(ContractError):
                self.join(session={**SESSION, key: value})

    def test_display_missing_different_and_changed_fail(self):
        for displays in (["", ""], ["c2", "c2"], ["c1", "c2"]):
            self.collector = Collector(); self.collector.displays = displays
            with self.subTest(displays=displays), self.assertRaises(ContractError): self.join()

    def test_unit_identity_properties_and_changes_fail(self):
        for key, value in {"MainPID": "124", "ControlGroup": "/wrong", "FragmentPath": "/tmp/unit",
                           "DropInPaths": "/run/user/120/override.conf", "ActiveState": "inactive",
                           "SubState": "dead"}.items():
            for index in (0, 1):
                self.collector = Collector(); self.collector.units[index][key] = value
                with self.subTest(key=key, index=index), self.assertRaises(ContractError): self.join()

    def test_pid_reuse_or_executable_environment_change_fails(self):
        for index in (0, 1):
            for key, value in {"start_ticks": 457, "sha256": "c" * 64,
                               "environment": {"XDG_SESSION_TYPE": "x11"}}.items():
                self.collector = Collector(); self.collector.processes[index][key] = value
                with self.subTest(index=index, key=key), self.assertRaises(ContractError): self.join()

    def test_wrong_kernel_cgroup_fails(self):
        with patch.object(greeter, "kernel_group", side_effect=[CGROUP, "/different"]):
            with self.assertRaises(ContractError): self.join()

    def test_old_preflight_or_changed_unit_cannot_pass(self):
        del self.collector.baseline["files"][greeter.FRAGMENT]
        with self.assertRaises(ContractError): self.join()
        self.collector = Collector()
        Inventory.observed = {**FILE, "sha256": "c" * 64}
        with self.assertRaises(ContractError): self.join()

    def test_unreviewed_package_version_fails(self):
        for package in greeter.VERSIONS:
            self.collector = Collector(); self.collector.baseline["packages"][package] = "unknown"
            with self.subTest(package=package), self.assertRaises(ContractError): self.join()

    def test_property_parser_rejects_ambiguity_and_control_data(self):
        for text in ("Display=c1\nDisplay=c2", "Display=c1\nUnknown=x", "Display=c1\t",
                     "Display", "", "Display=" + "x" * 8192):
            with self.subTest(text=text[:30]), self.assertRaises(ContractError):
                greeter.properties(text, ("Display",))

    def test_shared_deadline_is_enforced(self):
        self.collector.budget = 0
        with self.assertRaises(ContractError): self.join()

    def test_kernel_cgroup_reader_is_bounded_and_exact(self):
        # Exercise the real helper beneath the default fake used by join tests.
        real = KERNEL_GROUP
        for raw in (b"0::" + CGROUP.encode() + b"\n", b"1:cpu:/unexpected\n", b"x" * 4097):
            with patch.object(greeter.Path, "open", return_value=io.BytesIO(raw)):
                if raw.startswith(b"0::"):
                    self.assertEqual(real(self.collector, PID), CGROUP)
                else:
                    with self.assertRaises(ContractError): real(self.collector, PID)

    def test_only_exact_gdm_argv_shape_passes_without_export(self):
        real = MODE_VERIFIED
        for raw in (b"/usr/bin/gnome-shell\0--mode=gdm\0", b"gnome-shell\0--mode=gdm\0",
                    b"/usr/bin/gnome-shell\0--mode=user\0", b"/usr/bin/gnome-shell\0--mode=gdm\0secret\0"):
            with patch.object(greeter.Path, "open", return_value=io.BytesIO(raw)):
                if raw == b"/usr/bin/gnome-shell\0--mode=gdm\0":
                    self.assertTrue(real(self.collector, PID))
                else:
                    with self.assertRaises(ContractError): real(self.collector, PID)


if __name__ == "__main__":
    unittest.main()
