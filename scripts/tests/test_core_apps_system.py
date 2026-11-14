"""Helper-validator controls only; no application or native CI qualification."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location(
    "core_system_guest", ROOT / "packaging/marlin/vm-lane/core-apps/system.py")
guest = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guest)


class Controls:
    def __init__(self, root):
        self.home = Path(root)
        self.fixture = self.home / "fixture"
        self.fixture.mkdir()
        self.uid = os.getuid()
        self.owner = SimpleNamespace(pw_uid=self.uid)
        self.nonce = "a1234567-1234-1234-1234-123456789abc"
        self.controls = {}

    def path(self, relative):
        return self.fixture / relative

    def save_control(self, name, value):
        self.controls[name] = copy.deepcopy(value)

    def read_control(self, name):
        if name not in self.controls:
            raise FileNotFoundError(name)
        return copy.deepcopy(self.controls[name])

    def control_exists(self, name):
        return name in self.controls

    def identity(self):
        return {"test_control_identity": self.nonce}


class ProcessAuthorityControls(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.context = Controls(self.directory.name)
        self.original = guest._identity(os.getpid())
        self.context.save_control("system-processes", {
            "selected": self.original, "unselected": self.original})

    def forge_user_authority(self):
        path = self.context.path("system/processes/identities.json")
        path.parent.mkdir(parents=True, exist_ok=True)
        forged = dict(self.original, pid=2147483647)
        path.write_text(json.dumps({"selected": forged, "unselected": self.original}))

    def test_user_pid_replacement_cannot_turn_live_process_into_termination(self):
        self.forge_user_authority()
        actual = guest._processes(self.context)
        self.assertEqual(actual["selected"]["pid"], os.getpid())
        self.assertEqual(actual["selected"]["start_ticks"], self.original["start_ticks"])

    def test_missing_original_authority_is_not_a_termination_receipt(self):
        self.forge_user_authority()
        self.context.controls.clear()
        with self.assertRaises(FileNotFoundError):
            guest._processes(self.context)

    def test_incomplete_original_authority_is_not_a_termination_receipt(self):
        for saved in ({}, {"selected": None, "unselected": self.original},
                      {"selected": dict(self.original, pid=0), "unselected": self.original}):
            with self.subTest(saved=saved):
                self.context.save_control("system-processes", saved)
                with self.assertRaises(RuntimeError):
                    guest._processes(self.context)

    def test_original_pid_start_executable_and_hash_are_all_required(self):
        for key, changed in (("start_ticks", self.original["start_ticks"] + 1),
                             ("exe", "/different/python"), ("exe_sha256", "0" * 64),
                             ("uid", self.context.uid + 1)):
            with self.subTest(key=key), mock.patch.object(
                    guest, "_identity", return_value=dict(self.original, **{key: changed})):
                with self.assertRaises(RuntimeError):
                    guest._processes(self.context)

    def test_priority_effect_does_not_change_process_identity(self):
        with mock.patch.object(guest, "_identity", return_value=dict(self.original, nice=19)):
            self.assertEqual(guest._processes(self.context)["selected"]["nice"], 19)

    def test_real_original_exit_is_distinct_from_live_negative(self):
        child = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
        try:
            selected = guest._identity(child.pid)
            self.context.save_control("system-processes", {
                "selected": selected, "unselected": self.original})
        finally:
            child.terminate()
            child.wait(timeout=5)
        actual = guest._processes(self.context)
        self.assertIsNone(actual["selected"])
        self.assertEqual(actual["unselected"]["pid"], os.getpid())

    def test_disk_user_expected_file_cannot_replace_missing_authority(self):
        path = self.context.path("system/disk/expected.json")
        path.parent.mkdir(parents=True)
        path.write_text("{}")
        with mock.patch.object(guest, "_block") as block:
            with self.assertRaises(FileNotFoundError):
                guest._disk_read(self.context)
            block.assert_not_called()

    def test_not_found_user_unit_cannot_hide_surviving_original_at_cleanup(self):
        self.context.uid = self.context.owner.pw_uid = 1000
        original = dict(self.original, uid=1000)
        self.context.save_control("system-processes", {"selected": original})
        self.context.user = mock.Mock(return_value=subprocess.CompletedProcess([], 0, "not-found\n", ""))
        with mock.patch.object(guest, "_identity", return_value=original):
            with self.assertRaisesRegex(RuntimeError, "original process survived"):
                guest.dispatch("cleanup", {}, self.context)
        self.assertFalse(self.context.control_exists("system-cleanup"))


class ArchPackageControls(unittest.TestCase):
    def test_real_owner_and_version_queries_preserve_executable_guards(self):
        path = mock.MagicMock()
        path.__str__.return_value = "/usr/bin/Xvnc"
        path.stat.return_value = SimpleNamespace(st_uid=0, st_mode=0o100755)
        context = SimpleNamespace(command=mock.Mock(side_effect=[
            subprocess.CompletedProcess([], 0, "tigervnc\n", ""),
            subprocess.CompletedProcess([], 0, "tigervnc 1.15.0-1\n", "")]))
        process = {"exe": "/usr/bin/Xvnc", "exe_sha256": "a" * 64}
        with mock.patch.object(guest, "Path", return_value=path), mock.patch.object(
                guest, "_digest", return_value=process["exe_sha256"]):
            result = guest._package(context, process)
        self.assertEqual(result["package"], "tigervnc 1.15.0-1")
        self.assertEqual(context.command.call_args_list, [
            mock.call(["pacman", "-Qqo", "/usr/bin/Xvnc"]),
            mock.call(["pacman", "-Q", "tigervnc"])])
        for mode, uid, digest in ((0o100777, 0, "a" * 64),
                                  (0o100755, 1000, "a" * 64),
                                  (0o100755, 0, "b" * 64)):
            with self.subTest(mode=mode, uid=uid, digest=digest):
                context.command.reset_mock()
                path.stat.return_value = SimpleNamespace(st_uid=uid, st_mode=mode)
                with mock.patch.object(guest, "Path", return_value=path), mock.patch.object(
                        guest, "_digest", return_value=digest), self.assertRaises(RuntimeError):
                    guest._package(context, process)
                context.command.assert_not_called()


class FiniteSoftwarePreparationControls(unittest.TestCase):
    current = "a" * 64
    previous = "b" * 64
    args = {"app_id": "org.gnome.gitg", "branch": "stable"}

    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.context = Controls(self.directory.name)
        repo = self.context.home / ".local/share/flatpak/repo"
        repo.mkdir(parents=True)
        self.config = repo / "config"
        self.config.write_text('[remote "flathub"]\nurl=https://dl.flathub.org/repo/\n'
                               'gpg-verify=true\ngpg-verify-summary=true\n')
        self.keyring = repo / "flathub.trustedkeys.gpg"
        self.keyring.write_bytes(b"validator-keyring-not-a-signature-proof")
        self.calls = []
        self.history = "Commit: " + self.current + "\nCommit: " + self.previous + "\n"
        self.context.user = self.command

    def command(self, argv, **kwargs):
        self.calls.append((argv, kwargs))
        if argv[:2] == ["flatpak", "remote-add"]:
            return subprocess.CompletedProcess(argv, 0, "", "")
        if argv[:2] == ["flatpak", "remotes"]:
            return subprocess.CompletedProcess(argv, 0, "flathub\thttps://dl.flathub.org/repo/\n", "")
        if argv[:2] == ["flatpak", "info"]:
            if "--show-commit" in argv:
                return subprocess.CompletedProcess(argv, 0, self.current + "\n", "")
            return subprocess.CompletedProcess(argv, 1, "", "error: app is not installed")
        if argv[:2] == ["flatpak", "remote-info"]:
            return subprocess.CompletedProcess(argv, 0, self.history, "")
        if argv[:2] == ["flatpak", "update"]:
            return subprocess.CompletedProcess(argv, 0, "actual command receipt\n", "")
        raise AssertionError("unexpected helper command " + repr(argv))

    def remote(self):
        with mock.patch.object(guest, "_owned", return_value=self.context.fixture), mock.patch.object(
                guest, "_write", side_effect=lambda context, path, content: path.write_bytes(content)):
            return guest._flatpak_remote_prepare(self.context, self.args)

    def history_plan(self):
        self.remote()
        guest._flatpak_catalog_prepare(self.context, self.args)
        return guest._flatpak_history_prepare(self.context, self.args)

    def test_each_stage_has_one_bounded_network_command_and_root_plan(self):
        stages = ((self.remote, "remote-add"),
                  (lambda: guest._flatpak_catalog_prepare(self.context, self.args), "update"),
                  (lambda: guest._flatpak_history_prepare(self.context, self.args), "remote-info"))
        for action, operation in stages:
            self.calls.clear()
            action()
            network = [(argv, kwargs) for argv, kwargs in self.calls
                       if argv[1] in ("remote-add", "update", "remote-info")]
            self.assertEqual(len(network), 1)
            self.assertEqual(network[0][0][1], operation)
            self.assertEqual(network[0][1]["timeout"], 18)
            self.assertLessEqual(sum(kwargs["timeout"] for _, kwargs in self.calls), 22)
        plan = self.context.read_control("system-software-plan")
        self.assertEqual((plan["current_commit"], plan["previous_commit"]), (self.current, self.previous))
        self.assertEqual(plan["stage"], "history")
        descriptor = Path(plan["flatpakref"])
        descriptor.write_text("user-replaced consumer input")
        self.assertEqual(guest._flatpak_plan(self.context, self.args)["current_commit"], self.current)

    def test_missing_plan_wrong_ref_or_changed_trust_cannot_start_catalog(self):
        with self.assertRaises(FileNotFoundError):
            guest._flatpak_catalog_prepare(self.context, self.args)
        self.remote()
        for change in ("wrong-ref", "keyring", "verification"):
            with self.subTest(change=change):
                config, keyring = self.config.read_bytes(), self.keyring.read_bytes()
                args = dict(self.args)
                if change == "wrong-ref":
                    args["app_id"] = "org.gnome.Other"
                elif change == "keyring":
                    self.keyring.write_bytes(b"replacement")
                else:
                    self.config.write_text(self.config.read_text().replace("gpg-verify=true", "gpg-verify=false"))
                self.calls.clear()
                with self.assertRaises(RuntimeError):
                    guest._flatpak_catalog_prepare(self.context, args)
                self.assertEqual(self.calls, [])
                self.config.write_bytes(config)
                self.keyring.write_bytes(keyring)

    def test_history_requires_two_real_commits_before_advancing(self):
        self.remote()
        guest._flatpak_catalog_prepare(self.context, self.args)
        self.history = "Commit: " + self.current + "\n"
        with self.assertRaises(RuntimeError):
            guest._flatpak_history_prepare(self.context, self.args)
        self.assertEqual(self.context.read_control("system-software-plan")["stage"], "catalog")

    def test_old_baseline_is_bound_to_original_history_and_prior_ui_install(self):
        self.history_plan()
        self.calls.clear()
        with self.assertRaises(FileNotFoundError):
            guest._flatpak_old_baseline(self.context, dict(self.args, commit=self.previous))
        self.context.save_control("system-software-installed", {"commit": self.current})
        with self.assertRaises(RuntimeError):
            guest._flatpak_old_baseline(self.context, dict(self.args, commit="c" * 64))
        self.assertEqual(self.calls, [])
        receipt = guest._flatpak_old_baseline(self.context, dict(self.args, commit=self.previous))
        self.assertEqual(receipt["requested_commit"], self.previous)
        self.assertNotIn("installed", receipt)
        self.assertEqual([argv[1] for argv, _ in self.calls], ["info", "update"])
        self.assertEqual([kwargs["timeout"] for _, kwargs in self.calls], [2, 18])
        self.assertEqual(receipt, self.context.read_control("system-software-old-baseline"))

    def test_arbitrary_software_targets_fail_before_any_user_command(self):
        for change in ({"app_id": "org.gnome.Other"}, {"branch": "beta"}, {"branch": ["stable"]},
                       {"app_id": ["org.gnome.gitg"]}):
            with self.subTest(change=change):
                self.calls.clear()
                with self.assertRaises(RuntimeError):
                    guest._flatpak_remote_prepare(self.context, dict(self.args, **change))
                self.assertEqual(self.calls, [])
                self.assertFalse(self.context.control_exists("system-software-plan"))


class RfbSocketAuthorityControls(unittest.TestCase):
    process = {"pid": 4711}

    def test_actual_ipv4_ipv6_socket_rows_bind_each_endpoint_to_original_pid(self):
        lines = [
            'LISTEN 0 5 127.0.0.1:5991 0.0.0.0:* users:(("Xvnc",pid=4711,fd=9))',
            'LISTEN 0 5 [::1]:5991 [::]:* users:(("Xvnc",pid=4711,fd=10))',
        ]
        rows = guest._rfb_socket_rows("\n".join(lines), self.process)
        self.assertEqual([row["endpoint"] for row in rows], ["127.0.0.1:5991", "[::1]:5991"])
        self.assertEqual([row["pid"] for row in rows], [4711, 4711])
        connection = '0 0 127.0.0.1:5991 127.0.0.1:43820 users:(("Xvnc",pid=4711,fd=11))'
        self.assertEqual(guest._rfb_socket_rows(connection, self.process)[0]["line"], connection)

    def test_wildcard_foreign_missing_or_shared_owner_cannot_be_service_proof(self):
        for line in (
            'LISTEN 0 5 0.0.0.0:5991 0.0.0.0:* users:(("Xvnc",pid=4711,fd=9))',
            'LISTEN 0 5 [::]:5991 [::]:* users:(("Xvnc",pid=4711,fd=9))',
            'LISTEN 0 5 127.0.0.1:5991 0.0.0.0:* users:(("Xvnc",pid=4712,fd=9))',
            'LISTEN 0 5 127.0.0.1:5991 0.0.0.0:*',
            'LISTEN 0 5 127.0.0.1:5991 0.0.0.0:* users:(("Xvnc",pid=4711,fd=9),("other",pid=4712,fd=9))',
        ):
            with self.subTest(line=line), self.assertRaises(RuntimeError):
                guest._rfb_socket_rows(line, self.process)

    def test_listener_absence_is_failure_but_no_established_connections_is_observation(self):
        context = SimpleNamespace(user=mock.Mock(
            return_value=subprocess.CompletedProcess([], 0, "", "")))
        with self.assertRaises(RuntimeError):
            guest._rfb_sockets(context, self.process, True)
        self.assertEqual(guest._rfb_sockets(context, self.process, False)["rows"], [])


if __name__ == "__main__":
    unittest.main()
