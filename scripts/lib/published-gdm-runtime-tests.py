#!/usr/bin/env python3
"""Synthetic evidence and fake mounted paths only; never run a DM or container."""
import copy
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

import published_gdm_runtime as rt
from published_dm_contract import ContractError, Inventory

BOOT = "11111111-2222-3333-4444-555555555555"
OWNER = {"profile": rt.PROFILE, "session_id": "7", "uid": 1000, "vt": 2,
         "pid": 400, "start_ticks": 4000}


def example():
    env = {"PATH": "/usr/bin", "XDG_SESSION_TYPE": "wayland", "XDG_CURRENT_DESKTOP": "Tuna:GNOME",
           "XDG_SESSION_ID": "7", "GDMSESSION": "tuna", "DESKTOP_SESSION": "tuna",
           "TUNA_COMPOSITOR_STATE": "/run/user/1000/tuna-vm-state.json",
           "TUNA_LOCK_TRACE": "1", "TUNA_POINTER_TRACE": "1"}
    found = [{"path": rt.LAUNCHER, "present": True, "resolved": rt.LAUNCHER,
              "sha256": rt.REVIEWED_BYTES[rt.LAUNCHER]}]
    chain = [{"pid": i * 100, "ppid": (i - 1) * 100 if i > 1 else 1,
              "start_ticks": i * 1000, "uid": 0 if i <= 2 else 1000,
              "executable": name, "sha256": rt.REVIEWED_BYTES[name]}
             for i, name in enumerate(("/usr/bin/gdm", "/usr/lib/gdm-session-worker",
                  "/usr/lib/gdm-wayland-session", "/usr/bin/tuna-compositor"), 1)]
    evidence = {"schema_version": 1, "method": rt.METHOD, "gdm_source_commit": rt.GDM_SOURCE,
                "tuna_source_commit": rt.TUNA_SOURCE, "glib_source_commit": rt.GLIB_SOURCE, "source_build_reproduced": False,
                "direct_launcher_exec_observed": False, "boot_id": BOOT,
                "session_identity": dict(OWNER), "chain": chain, "environment": env,
                "gdm_environment": {"PATH": "/usr/bin"}, "helper_environment": {"PATH": "/usr/bin"},
                "user_manager_path": "/usr/bin", "exec_candidates": copy.deepcopy(found),
                "tryexec_candidates": copy.deepcopy(found), "child_candidates": copy.deepcopy(found),
                "session_entry": {"path": rt.SESSION,
                    "sha256": "feed3bd8bce86045926d23f05b9562039daef074f2ce14cf9ea586333f150acc"},
                "launcher": {"path": rt.LAUNCHER, "sha256": rt.REVIEWED_BYTES[rt.LAUNCHER]},
                "session_candidates": [{"path": p + "/tuna.desktop", "present": p + "/tuna.desktop" == rt.SESSION}
                                       for p in rt.session_search_roots({})]}
    for key in ("launch_files_verified", "session_search_verified", "executable_search_verified",
                "environment_verified", "process_chain_verified", "helper_command_verified",
                "compositor_command_verified"):
        evidence[key] = True
    login = {"boot_id": BOOT, "session_identity": dict(OWNER), "picker_selected": True,
             "wrong_password_rejected": True, "user_to_password_challenge_observed": True,
             "immutable_verified": True}
    return evidence, login


class LaunchTests(unittest.TestCase):
    def test_complete_named_method_still_disclaims_direct_exec(self):
        proof = rt.validate_launch(*example())
        self.assertTrue(proof["effective_launch_verified"])
        self.assertFalse(proof["direct_launcher_exec_observed"])
        self.assertFalse(proof["source_build_reproduced"])
        self.assertEqual(len(proof["inferred"]), 2)

    def test_missing_real_picker_or_password_proof_fails(self):
        for key in ("picker_selected", "wrong_password_rejected", "user_to_password_challenge_observed", "immutable_verified"):
            e, l = example(); del l[key]
            with self.subTest(key=key), self.assertRaises(ContractError): rt.validate_launch(e, l)

    def test_missing_each_observation_gate_fails(self):
        for key in ("launch_files_verified", "session_search_verified", "executable_search_verified",
                    "environment_verified", "process_chain_verified", "helper_command_verified", "compositor_command_verified"):
            e, l = example(); e[key] = False
            with self.subTest(key=key), self.assertRaises(ContractError): rt.validate_launch(e, l)

    def test_source_or_direct_observation_overclaim_fails(self):
        for key, value in (("gdm_source_commit", "main"), ("tuna_source_commit", "main"),
                           ("source_build_reproduced", True), ("direct_launcher_exec_observed", True)):
            e, l = example(); e[key] = value
            with self.subTest(key=key), self.assertRaises(ContractError): rt.validate_launch(e, l)

    def test_wrong_backend_boot_or_original_session_fails(self):
        for change in (lambda e: e.update(boot_id="00000000-2222-3333-4444-555555555555"),
                       lambda e: e["session_identity"].update(profile="current-greetd"),
                       lambda e: e["session_identity"].update(session_id="8")):
            e, l = example(); change(e)
            with self.assertRaises(ContractError): rt.validate_launch(e, l)

    def test_altered_file_identity_fails_even_with_true_booleans(self):
        for key in ("launcher", "session_entry"):
            e, l = example(); e[key]["sha256"] = "a" * 64
            with self.subTest(key=key), self.assertRaises(ContractError): rt.validate_launch(e, l)

    def test_altered_process_owner_parent_hash_or_start_fails(self):
        for field, value in (("uid", 0), ("ppid", 999), ("sha256", "a" * 64), ("start_ticks", 1)):
            e, l = example(); e["chain"][3][field] = value
            with self.subTest(field=field), self.assertRaises(ContractError): rt.validate_launch(e, l)

    def test_wrong_session_or_path_environment_fails(self):
        for key, value in (("XDG_SESSION_TYPE", "x11"), ("XDG_SESSION_ID", "8"),
                           ("GDMSESSION", "gnome"), ("DESKTOP_SESSION", "gnome"),
                           ("PATH", "/tmp:/usr/bin"), ("TUNA_COMPOSITOR_STATE", "/tmp/state")):
            e, l = example(); e["environment"][key] = value
            with self.subTest(key=key), self.assertRaises(ContractError): rt.validate_launch(e, l)

    def test_arbitrary_environment_is_not_a_valid_receipt(self):
        for key in ("SECRET_TOKEN", "LD_PRELOAD", "TUNA_SHELL_BIN"):
            e, l = example(); e["environment"][key] = "do-not-emit"
            with self.subTest(key=key), self.assertRaises(ContractError): rt.validate_launch(e, l)

    def test_search_shadow_and_omitted_root_fail(self):
        e, l = example(); e["session_candidates"][0]["present"] = True
        with self.assertRaises(ContractError): rt.validate_launch(e, l)
        e, l = example(); e["session_candidates"].pop(0)
        with self.assertRaises(ContractError): rt.validate_launch(e, l)

    def test_helper_path_is_separate_from_child_path(self):
        e, l = example(); e["helper_environment"]["PATH"] = "/usr/local/bin:/usr/bin"
        with self.assertRaises(ContractError): rt.validate_launch(e, l)
        e["exec_candidates"].insert(0, {"path": "/usr/local/bin/tuna-session", "present": False})
        self.assertTrue(rt.validate_launch(e, l)["effective_launch_verified"])

    def test_tryexec_path_must_be_independently_complete(self):
        e, l = example(); e["gdm_environment"]["PATH"] = "/usr/local/bin:/usr/bin"
        with self.assertRaises(ContractError): rt.validate_launch(e, l)

    def test_untrusted_search_values_fail(self):
        for value in ("", "/tmp::/usr/bin", "./bin:/usr/bin", "/usr/../bin", "/usr//bin", "/usr/bin/"):
            with self.subTest(value=value), self.assertRaises(ContractError): rt.directory_list(value)

    def test_exact_wayland_search_order_and_xdg_default(self):
        self.assertEqual(rt.session_search_roots({}), ["/usr/share/gdm/greeter/wayland-sessions",
            "/usr/local/share/wayland-sessions", "/usr/share/wayland-sessions",
            "/usr/local/share/xsessions", "/usr/share/xsessions", "/etc/X11/sessions",
            "/etc/dm/Sessions", "/usr/share/gdm/BuiltInSessions"])
        self.assertEqual(rt.session_search_roots({"XDG_DATA_DIRS": "/opt/data:/usr/share"}),
            ["/usr/share/gdm/greeter/wayland-sessions", "/opt/data/wayland-sessions", "/usr/share/wayland-sessions",
             "/opt/data/xsessions", "/usr/share/xsessions", "/etc/X11/sessions",
             "/etc/dm/Sessions", "/usr/share/gdm/BuiltInSessions"])


class FakeInventory(Inventory):
    def record(self, name):
        result = super().record(name)
        if result["type"] != "missing": result.update(uid=0, gid=0)
        return result


class MountedSearchTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(); self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name); self.root.chmod(0o755)
        path = self.root / "usr/bin/tuna-session"; path.parent.mkdir(parents=True)
        path.write_bytes(b"original"); path.chmod(0o755)
        self.observer = FakeInventory(self.root, lambda args: "tuna-desktop")
        self.observer.packages = {"tuna-desktop": "1"}
        self.baseline = {"files": {rt.LAUNCHER: dict(self.observer.record(rt.LAUNCHER))}}

    def test_real_mounted_candidate_hash(self):
        r = rt.executable_search(self.observer, {"PATH": "/usr/local/bin:/usr/bin"}, self.baseline)
        self.assertFalse(r[0]["present"]); self.assertEqual(r[-1]["resolved"], rt.LAUNCHER)

    def test_present_shadow_fails(self):
        path = self.root / "usr/local/bin/tuna-session"; path.parent.mkdir(parents=True)
        path.write_bytes(b"shadow"); path.chmod(0o755)
        with self.assertRaises(ContractError): rt.executable_search(self.observer, {"PATH": "/usr/local/bin:/usr/bin"}, self.baseline)

    def test_writable_absence_parent_fails(self):
        path = self.root / "usr/local"; path.mkdir(); path.chmod(0o777)
        with self.assertRaises(ContractError): rt.executable_search(self.observer, {"PATH": "/usr/local/bin:/usr/bin"}, self.baseline)

    def test_changed_file_fails(self):
        (self.root / "usr/bin/tuna-session").write_bytes(b"changed")
        self.observer.files.clear()
        with self.assertRaises(ContractError): rt.executable_search(self.observer, {"PATH": "/usr/bin"}, self.baseline)


class CollectorBoundaryTests(unittest.TestCase):
    def collector(self, seconds=2):
        import time
        collector = object.__new__(rt.Collector)
        collector.deadline = time.monotonic() + seconds
        return collector

    def test_capped_stdout_fails_before_unbounded_capture(self):
        import sys
        c = self.collector()
        with self.assertRaisesRegex(ContractError, "output exceeds bound"):
            c.call(sys.executable, "-c", "import os; os.write(1, b'x' * 3000000)")

    def test_capped_stderr_never_appears_in_error(self):
        import sys
        c = self.collector()
        with self.assertRaises(ContractError) as caught:
            c.call(sys.executable, "-c", "import os; os.write(2, b'private' * 10000)")
        self.assertNotIn("private", str(caught.exception))

    def test_read_command_success_and_failure(self):
        import sys
        self.assertEqual(self.collector().call(sys.executable, "-c", "print('public')"), "public")
        with self.assertRaisesRegex(ContractError, "read command failed"):
            self.collector().call(sys.executable, "-c", "raise SystemExit(3)")

    def test_exited_leader_with_descendant_pipe_is_killed_before_reap(self):
        import os
        import sys
        with tempfile.TemporaryDirectory() as directory:
            pidfile = str(Path(directory) / "child")
            code = ("import os,time; p=os.fork(); "
                    "open(" + repr(pidfile) + ", 'w').write(str(p)) if p else None; "
                    "os._exit(0) if p else time.sleep(30)")
            real_killpg = os.killpg
            calls = []
            def killpg(pid, sig):
                # A zombie leader is still available with WNOWAIT at retirement.
                anchored = os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT)
                calls.append(anchored is not None)
                return real_killpg(pid, sig)
            with patch.object(rt.os, "killpg", side_effect=killpg):
                with self.assertRaisesRegex(ContractError, "deadline exceeded"):
                    self.collector(2.0).call(sys.executable, "-c", code)
            self.assertEqual(calls, [True])
            child = int(Path(pidfile).read_text())
            # Killed descendants may remain zombies until the container init reaps them.
            path = Path("/proc") / str(child) / "stat"
            import time
            until = time.monotonic() + 1
            while path.exists() and time.monotonic() < until:
                if path.read_text().rsplit(")", 1)[1].split()[0] == "Z": break
                time.sleep(0.01)
            self.assertTrue(not path.exists() or path.read_text().rsplit(")", 1)[1].split()[0] == "Z")

    def test_unsettled_command_poison_retains_leader_anchor(self):
        import sys
        c = self.collector()
        c.live_group = lambda group: (_ for _ in ()).throw(ContractError("injected scan failure"))
        before = len(rt._UNSETTLED_COMMANDS)
        with self.assertRaises(ContractError): c.call(sys.executable, "-c", "pass")
        self.assertTrue(c.poisoned)
        self.assertEqual(len(rt._UNSETTLED_COMMANDS), before + 1)
        with self.assertRaisesRegex(ContractError, "collector is unsettled"):
            c.call(sys.executable, "-c", "pass")
        # This test command had no descendants; release only its owned dead anchor.
        rt._UNSETTLED_COMMANDS.pop().wait(timeout=1)

    def test_runtime_inventory_queries_use_collector_deadline(self):
        with patch.object(rt, "base_receipt", return_value={"packages": {}}), \
             patch.object(rt.Path, "read_text", return_value=BOOT), \
             patch.object(rt.Collector, "call", return_value="tuna-desktop") as call:
            c = rt.Collector(uid=1000)
            self.assertEqual(c.observer.owner(rt.LAUNCHER), "tuna-desktop")
            call.assert_called_once_with("/usr/bin/pacman", "-Qqo", "--", rt.LAUNCHER)

    def test_same_pid_start_does_not_hide_exec_uid_parent_or_environment_change(self):
        base = {"pid": 1, "start_ticks": 2, "uid": 1000, "ppid": 3,
                "executable": "/usr/bin/tuna-compositor", "exe_device": 4, "exe_inode": 5,
                "sha256": "a" * 64, "logind_session": "7", "environment": {"PATH": "/usr/bin"}}
        changes = {"uid": 0, "ppid": 9, "executable": "/bin/false", "exe_inode": 6,
                   "sha256": "b" * 64, "logind_session": "8", "environment": {"PATH": "/tmp"}}
        for key, value in changes.items():
            c = self.collector()
            c.process = lambda *a, **kw: dict(base, **{key: value})
            with self.subTest(key=key), self.assertRaises(ContractError): c.recheck_processes([base])



class DynamicGreeterTests(unittest.TestCase):
    def setUp(self):
        from types import SimpleNamespace
        from unittest.mock import Mock
        import published_gdm_greeter as join
        self.join = join
        self.row = {"Id": "1", "User": "60578", "Name": "gdm-greeter",
                    "Service": "gdm-launch-environment", "Class": "greeter", "Type": "wayland",
                    "Remote": "no", "Seat": "seat0", "Active": "yes", "State": "active", "VTNr": "1"}
        self.account = SimpleNamespace(pw_name="gdm-greeter", pw_uid=60578, pw_gid=65534,
                                      pw_dir="/run/gdm/gdm-greeter", pw_shell="/usr/bin/nologin")
        self.principal = {"pid": 930, "uid": 60578, "start_ticks": 100, "executable": "/usr/bin/gnome-shell",
                          "exe_device": 1, "exe_inode": 2, "logind_session": "1",
                          "greeter_join": {"account": {"name": "gdm-greeter", "uid": 60578,
                                                        "source": "observed-logind-greeter"}}}
        self.c = rt.Collector.__new__(rt.Collector)
        self.c.uid, self.c.boot_id = 1000, BOOT
        self.c.remaining = Mock(return_value=12)
        self.c.manager = Mock(return_value={"pid": 700, "start_ticks": 80, "uid": 0})
        self.c.sessions = Mock(return_value=[dict(self.row)])
        self.c.find_process = Mock(return_value=dict(self.principal))
        self.c.active_vt = Mock(return_value="tty1")
        for p in (patch.object(join.pwd, "getpwuid", return_value=self.account),
                  patch.object(join.pwd, "getpwnam", return_value=self.account),
                  patch.object(join, "join_greeter", side_effect=lambda c, p, r: dict(p))):
            p.start(); self.addCleanup(p.stop)

    def test_actual_51_1_dynamic_uid_is_selected_from_unique_session(self):
        result = self.c.greeter()
        self.assertTrue(result["ready"])
        self.c.find_process.assert_called_once_with(60578, "/usr/bin/gnome-shell")
        self.assertTrue(all(call.args == ("gdm-greeter",) for call in self.join.pwd.getpwnam.call_args_list))
        self.assertEqual(self.c.sessions.call_count, 2)
        self.assertEqual(self.c.manager.call_count, 2)

    def test_starting_or_missing_greeter_does_not_select_an_account(self):
        for manager, rows, reason in ((None, [], "manager-starting"), ({"pid": 700}, [], "no-active-greeter")):
            self.c.manager.return_value = manager; self.c.sessions.return_value = rows
            result = self.c.greeter()
            self.assertFalse(result["ready"]); self.assertEqual(result["reason"], reason)
        self.join.pwd.getpwuid.assert_not_called(); self.c.find_process.assert_not_called()

    def test_ambiguous_wrong_account_and_fixture_uid_are_refused(self):
        for rows in ([self.row, dict(self.row, Id="3", User="60579")],
                     [dict(self.row, Name="gdm")], [dict(self.row, User="1000")], [dict(self.row, User="0")]):
            self.c.sessions.return_value = rows
            with self.subTest(rows=rows), self.assertRaises(ContractError): self.c.greeter()
        self.c.find_process.assert_not_called()

    def test_wrong_service_or_class_is_never_selected(self):
        for key, value in (("Service", "gdm-password"), ("Class", "user"), ("Seat", "seat1"), ("Active", "no")):
            self.c.sessions.return_value = [dict(self.row, **{key: value})]
            self.assertFalse(self.c.greeter()["ready"])
        self.c.find_process.assert_not_called()

    def test_session_uid_vt_and_manager_changes_are_refused(self):
        from unittest.mock import Mock
        for changes in ({"User": "60579"}, {"Id": "3"}, {"Name": "gdm-greeter-2"}, {"VTNr": "2"}):
            self.c.sessions = Mock(side_effect=[[self.row], [dict(self.row, **changes)]])
            with self.subTest(changes=changes), self.assertRaises(ContractError): self.c.greeter()
        self.c.sessions = Mock(return_value=[self.row])
        self.c.manager = Mock(side_effect=[{"pid": 700}, {"pid": 701}])
        with self.assertRaises(ContractError): self.c.greeter()

    def test_ui_uses_bound_dynamic_account_and_explicit_uid(self):
        from unittest.mock import Mock
        ready = {"schema_version": 1, "profile": rt.PROFILE, "ready": True,
                 "greeter": self.principal, "active_vt": "tty1", "boot_id": BOOT}
        self.c.greeter = Mock(return_value=ready)
        public = {k: self.principal[k] for k in ("pid", "uid", "start_ticks", "executable", "exe_device", "exe_inode")}
        public["session_id"] = "1"
        self.c.call = Mock(return_value=json.dumps({"available": True, "bus_owner_verified": True,
                "input_value_observed": False, "principal": public, "controls": []}))
        self.c.ui()
        argv = self.c.call.call_args.args
        self.assertEqual(argv[:4], ("/usr/bin/runuser", "-u", "gdm-greeter", "--"))
        self.assertEqual(argv[-2:], ("--uid", "60578"))
        changed = copy.deepcopy(ready); changed["greeter"]["greeter_join"]["account"]["name"] = "gdm-greeter-2"
        self.c.greeter = Mock(side_effect=[ready, changed])
        with self.assertRaises(ContractError): self.c.ui()


if __name__ == "__main__": unittest.main()
