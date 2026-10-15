#!/usr/bin/env python3
"""Protect the shared boot API used by the independent performance lane."""
import importlib.machinery
import importlib.util
import io
import ast
import base64
import json
import shlex
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch
from types import SimpleNamespace

loader = importlib.machinery.SourceFileLoader("vm_boot_lane", str(Path(__file__).resolve().parents[1] / "tuna-vm-lane"))
spec = importlib.util.spec_from_loader(loader.name, loader)
lane = importlib.util.module_from_spec(spec)
loader.exec_module(lane)


class DeviceInventoryProjection(unittest.TestCase):
    def inventory(self, state):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        fn = next(n for n in ast.parse(source.read_text()).body
                  if isinstance(n, ast.FunctionDef) and n.name == "inventory")
        def call(*args):
            if args[1] == "list-sessions":
                return "1 1000 tuna-test seat0 tty1"
            return "Id=1\nVTNr=1\nUser=1000\nService=greetd\nType=wayland\nClass=user"
        scope = {"json": json, "STATE": SimpleNamespace(read_text=lambda: json.dumps(state)),
                 "pathlib": SimpleNamespace(Path=lambda _: SimpleNamespace(iterdir=lambda: [])),
                 "OWNER": SimpleNamespace(pw_uid=1000),
                 "os": SimpleNamespace(geteuid=lambda: 0), "call": call,
                 "classify_pam_helpers": lambda _: (None, []), "pam_process_snapshot": lambda: []}
        exec(compile(ast.Module(body=[fn], type_ignores=[]), str(source), "exec"), scope)
        return scope["inventory"]()

    def state(self):
        return dict.fromkeys(("locked", "windows", "focused", "active_workspace",
                              "overview_open", "pointer_position", "native_relative_motion_count"))

    def test_live_device_state_survives_guest_projection(self):
        state = self.state()
        state["input_devices"] = [{"kind": "pointer", "accel_available": True,
                                   "accel_speed": 0.5, "natural_available": True,
                                   "natural_scroll": False}]
        self.assertEqual(self.inventory(state)["state"]["input_devices"], state["input_devices"])

    def test_missing_device_snapshot_refused_instead_of_empty_default(self):
        with self.assertRaisesRegex(KeyError, "input_devices"):
            self.inventory(self.state())


class OrcaUnreadyDiagnostic(unittest.TestCase):
    def run_probe(self, *, locked=False, changed=False, executable="/usr/bin/python3.14",
                  uid=1000, package="python", oversized=False, replaced=False):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        fn = next(n for n in ast.parse(source.read_text()).body
                  if isinstance(n, ast.FunctionDef) and n.name == "orca_child_diagnostic")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for pid, parent, ticks in ((57, 87, 100), (87, 1, 90)):
                path = root / str(pid)
                path.mkdir()
                fields = ["S", str(parent)] + ["0"] * 17 + [str(ticks)]
                (path / "stat").write_text(f"{pid} (private process name) " + " ".join(fields))
            (root / "57/environ").write_bytes(b"x" * (128 * 1024 + 1) if oversized else
                b"WAYLAND_DISPLAY=tuna-nested-87\0DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus\0SECRET=private-never-published\0")
            state_path = root / "state"
            state_path.write_text(json.dumps({"locked": changed, "screen_reader_pid": 57}))
            executable_reads = iter([57, 58 if replaced else 57])
            scope = {"pathlib": SimpleNamespace(Path=lambda path: root / str(path).split("/")[-1]),
                     "OWNER": SimpleNamespace(pw_uid=1000), "RUNTIME": "/run/user/1000",
                     "STATE": state_path, "os": SimpleNamespace(readlink=lambda path:
                         "/usr/bin/tuna-compositor" if path.parent.name == "87" else executable,
                         stat=lambda path: SimpleNamespace(st_dev=1, st_ino=next(executable_reads), st_size=100)),
                     "json": json, "subprocess": __import__("subprocess"),
                     "call": lambda *args: package}
            # Keep genuine file reads; only controlled /proc path/UID/exec fixtures substitute.
            exec(compile(ast.Module(body=[fn], type_ignores=[]), str(source), "exec"), scope)
            original_stat = Path.stat
            def owned_stat(path, *args, **kwargs):
                value = original_stat(path, *args, **kwargs)
                if path.name in {"57", "87"}:
                    return SimpleNamespace(st_uid=uid if path.name == "57" else 1000)
                return value
            with patch.object(Path, "stat", owned_stat):
                result = scope["orca_child_diagnostic"]({"locked": locked}, 57)
            self.assertNotIn("private", json.dumps(result))
            return result

    def test_original_unready_child_is_observed_without_claiming_ready(self):
        result = self.run_probe()
        self.assertEqual(result["status"], "observed")
        self.assertEqual((result["pid"], result["parent_pid"], result["start_ticks"]), (57, 87, 100))
        self.assertTrue(result["display_matches_parent"])
        self.assertTrue(result["session_bus_matches"])
        self.assertNotIn("ready", result)

    def test_locked_does_not_read_process(self):
        self.assertEqual(self.run_probe(locked=True), {"status": "locked"})

    def test_lock_transition_discards_all_original_identity(self):
        self.assertEqual(self.run_probe(changed=True), {"status": "changed"})

    def test_arbitrary_executable_not_published(self):
        self.assertEqual(self.run_probe(executable="/private/secret"), {"status": "executable-refused"})

    def test_wrong_uid_refused(self):
        self.assertEqual(self.run_probe(uid=0), {"status": "principal-refused"})

    def test_wrong_package_refused(self):
        self.assertEqual(self.run_probe(package="private-package"), {"status": "package-refused"})

    def test_executed_inode_replacement_discards_identity(self):
        self.assertEqual(self.run_probe(replaced=True), {"status": "changed"})

    def test_oversized_environment_refused(self):
        self.assertEqual(self.run_probe(oversized=True), {"status": "malformed"})


class OrcaReadFailure(unittest.TestCase):
    def test_failed_observation_emits_safe_receipt_and_keeps_exit_one(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        tree = ast.parse(source.read_text())
        main = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == "main")
        branch = next(n for n in ast.walk(main) if isinstance(n, ast.If)
                      and isinstance(n.test, ast.Compare) and any(isinstance(v, ast.Constant) and v.value == "orca-read"
                          for v in n.test.comparators))
        def observe(): raise RuntimeError("private-error-text")
        scope = {"orca_observation": observe, "orca_read_error": lambda error: {"exception_type": type(error).__name__}, "json": json}
        with patch("sys.stdout", new_callable=io.StringIO) as out:
            with self.assertRaises(SystemExit) as caught:
                exec(compile(ast.Module(body=branch.body, type_ignores=[]), str(source), "exec"), scope)
        self.assertEqual(caught.exception.code, 1)
        self.assertTrue(caught.exception.__suppress_context__)
        self.assertEqual(json.loads(out.getvalue()), {"orca_read_error": {"exception_type": "RuntimeError"}})
        self.assertNotIn("private-error-text", out.getvalue())

    def test_failed_read_rejects_arbitrary_private_or_invalid_schema(self):
        actor = lane.guest_agent.GuestAgent.__new__(lane.guest_agent.GuestAgent)
        invalid = [{"exception_type": "private-token"},
                   {"exception_type": "CalledProcessError", "argv": "private-token"},
                   {"exception_type": "CalledProcessError", "returncode": True},
                   {"exception_type": "CalledProcessError", "returncode": 256},
                   {"exception_type": "RuntimeError", "returncode": 1},
                   {"exception_type": "RuntimeError", "errno": 1},
                   {"exception_type": "CalledProcessError", "returncode": 1,
                    "dbus_method": "SetLogFileForTesting", "stderr": "private-token"},
                   {"exception_type": "CalledProcessError", "returncode": 1,
                    "dbus_method": "GetVersion", "stderr": "x" * 513},
                   {"exception_type": "CalledProcessError", "returncode": 1,
                    "dbus_method": "GetVersion", "stderr": "\x00private-token"}]
        for value in invalid:
            output = base64.b64encode(json.dumps({"orca_read_error": value}).encode()).decode()
            failure = {"exited": True, "exitcode": 1, "out-data": output}
            with patch.object(actor, "command", side_effect=[{"pid": 10}, failure]):
                with self.assertRaisesRegex(RuntimeError, r"guest lifecycle orca-read failed \(exit=1\)") as caught:
                    actor.run("orca-read")
            self.assertNotIn("private-token", str(caught.exception))
        # Schema/privacy policy only; the retained real VM failure remains unexplained.

    def test_read_diagnostics_preserve_only_fixed_public_dbus_failure(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        tree = ast.parse(source.read_text())
        helper = next(node for node in tree.body if isinstance(node, ast.FunctionDef)
                      and node.name == "orca_read_error")
        scope = {"subprocess": lane.subprocess}
        exec(compile(ast.Module(body=[helper], type_ignores=[]), str(source), "exec"), scope)
        diagnose = scope["orca_read_error"]
        command = ["runuser", "-u", "tuna-test", "--", "env", "PRIVATE_ENV=do-not-copy",
                   "busctl", "--user", "call", "org.freedesktop.DBus", "/org/freedesktop/DBus",
                   "org.freedesktop.DBus", "GetConnectionUnixProcessID", "s", ":1.527"]
        error = lane.subprocess.CalledProcessError(1, command, stderr="Call failed: owner vanished\n" + "x" * 1024)
        result = diagnose(error)
        self.assertEqual(result["dbus_method"], "GetConnectionUnixProcessID")
        self.assertEqual(result["returncode"], 1)
        self.assertEqual(len(result["stderr"]), 512)
        self.assertNotIn("PRIVATE_ENV", json.dumps(result))
        for unsafe in (["orca", "--testing-token=do-not-copy"],
                       [*command[:-3], "SetLogFileForTesting", "ss", "do-not-copy"]):
            result = diagnose(lane.subprocess.CalledProcessError(1, unsafe, stderr="do-not-copy"))
            self.assertNotIn("stderr", result)
            self.assertNotIn("do-not-copy", json.dumps(result))
        self.assertEqual(diagnose(FileNotFoundError(2, "do-not-copy")),
                         {"exception_type": "FileNotFoundError", "errno": 2})
    def test_guest_transport_keeps_read_failure_and_other_action_output_private(self):
        diagnostic = {"orca_read_error": {"exception_type": "CalledProcessError",
                      "returncode": 1, "dbus_method": "GetVersion", "stderr": "Call failed: owner vanished"}}
        failed = {"exited": True, "exitcode": 1,
                  "out-data": base64.b64encode(json.dumps(diagnostic).encode()).decode(),
                  "err-data": base64.b64encode(b"private arbitrary traceback").decode()}
        actor = lane.guest_agent.GuestAgent.__new__(lane.guest_agent.GuestAgent)
        for action in ("orca-read", "orca-speech-start"):
            with patch.object(actor, "command", side_effect=[{"pid": 10}, failed]):
                with self.assertRaises(RuntimeError) as caught:
                    actor.run(action)
            message = str(caught.exception)
            self.assertNotIn("private arbitrary traceback", message)
            if action == "orca-read":
                self.assertIn("owner vanished", message)
                self.assertIn("failed (exit=1)", message)
            else:
                self.assertNotIn("owner vanished", message)
        malformed = ["!", base64.b64encode(b"\xff").decode(),
                     base64.b64encode(b"[").decode(), base64.b64encode(b"[]").decode(),
                     base64.b64encode(b"x" * 4097).decode(), "A" * 5465, None, ["not text"]]
        for output in malformed:
            with patch.object(actor, "command", side_effect=[{"pid": 10}, {**failed, "out-data": output}]):
                with self.assertRaisesRegex(RuntimeError, r"guest lifecycle orca-read failed \(exit=1\)"):
                    actor.run("orca-read")


class OrcaActivationCleanup(unittest.TestCase):
    def test_reset_failure_keeps_observations_and_original_error(self):
        observations = [{"enabled": True, "ready": False, "pid": 57}]
        original = RuntimeError("original activation failure")
        actor = SimpleNamespace(run=lambda action: (_ for _ in ()).throw(RuntimeError("private reset output")))
        with tempfile.TemporaryDirectory() as out:
            with self.assertRaises(RuntimeError) as caught:
                try:
                    raise original
                finally:
                    lane.finish_orca_activation(actor, out, observations)
            self.assertIs(caught.exception, original)
            self.assertEqual(json.loads((Path(out) / "orca-activation.json").read_text()), observations)
            receipt = (Path(out) / "orca-activation-cleanup.json").read_text()
            self.assertNotIn("private", receipt)
            self.assertEqual(json.loads(receipt), {"primary_error_type": "RuntimeError",
                "cleanup_errors": [{"action": "orca-reset", "exception_type": "RuntimeError"}]})

    def test_reset_failure_without_activation_error_still_fails(self):
        actor = SimpleNamespace(run=lambda action: (_ for _ in ()).throw(RuntimeError("private output")))
        with tempfile.TemporaryDirectory() as out:
            with self.assertRaisesRegex(RuntimeError, "Orca activation cleanup failed"):
                lane.finish_orca_activation(actor, out, [])
            self.assertEqual(json.loads((Path(out) / "orca-activation.json").read_text()), [])

    def test_success_retains_observations_and_clean_reset_receipt(self):
        calls = []
        actor = SimpleNamespace(run=lambda action: calls.append(action))
        with tempfile.TemporaryDirectory() as out:
            lane.finish_orca_activation(actor, out, [])
            self.assertEqual(calls, ["orca-reset"])
            self.assertEqual(json.loads((Path(out) / "orca-activation-cleanup.json").read_text()),
                             {"primary_error_type": None, "cleanup_errors": []})

    def test_unwritable_evidence_does_not_replace_original_failure(self):
        original = RuntimeError("original failure")
        actor = SimpleNamespace(run=lambda action: None)
        with tempfile.TemporaryDirectory() as out:
            blocked = Path(out) / "not-a-directory"
            blocked.write_text("file")
            with self.assertRaises(RuntimeError) as caught:
                try:
                    raise original
                finally:
                    lane.finish_orca_activation(actor, str(blocked), [])
            self.assertIs(caught.exception, original)
            with self.assertRaisesRegex(RuntimeError, "orca-evidence-retention"):
                lane.finish_orca_activation(actor, str(blocked), [])
class OrcaReadChurn(unittest.TestCase):
    """Transient guest churn during Orca start/stop is not a read failure (#543)."""
    SOURCE = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"

    def load(self, name, scope):
        fn = next(n for n in ast.parse(self.SOURCE.read_text()).body
                  if isinstance(n, ast.FunctionDef) and n.name == name)
        exec(compile(ast.Module(body=[fn], type_ignores=[]), str(self.SOURCE), "exec"), scope)
        return scope[name]

    def test_inventory_skips_runuser_session_that_closed_mid_listing(self):
        def call(*args, check=True):
            if args[1] == "list-sessions":
                return "2 1000 tuna-test seat0 tty1\n73 1000 tuna-test - -"
            if args[2] == "73":
                if check:
                    raise lane.subprocess.CalledProcessError(1, list(args))
                return ""
            return "Id=2\nVTNr=1\nUser=1000\nType=wayland\nService=greetd\nClass=user"
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory, "state")
            state.write_text(json.dumps({k: None for k in (
                "locked", "windows", "focused", "active_workspace", "overview_open",
                "pointer_position", "native_relative_motion_count")}))
            empty_proc = Path(directory, "proc")
            empty_proc.mkdir()
            scope = {"json": json, "STATE": state, "call": call, "os": lane.os,
                     "OWNER": SimpleNamespace(pw_uid=1000),
                     "pathlib": SimpleNamespace(Path=lambda path: empty_proc)}
            result = self.load("inventory", scope)()
        self.assertEqual(result["session"]["Id"], "2")

    def observe(self, owner_after_failure):
        def call(*args, user=False, check=True):
            if "GetNameOwner" in args:
                observe.lookups += 1
                if args[-1] != "org.gnome.Orca1.Service":
                    return ""
                return 's ":1.9"' if observe.lookups <= 2 else owner_after_failure
            if args[0] == "gsettings":
                return "true"
            return "orca 51.0-2"
        def owned(*_):
            raise lane.subprocess.CalledProcessError(1, ["busctl"])
        with tempfile.TemporaryDirectory() as directory:
            state = Path(directory, "state")
            state.write_text(json.dumps({"screen_reader_pid": 41, "screen_reader_state": "starting"}))
            scope = {"json": json, "STATE": state, "call": call, "shlex": shlex,
                     "subprocess": lane.subprocess, "owned_orca_observation": owned,
                     "orca_child_diagnostic": lambda *_: {"status": "observed"}}
            observe = self.load("orca_observation", scope)
            observe.lookups = 0
            return observe()

    def test_owner_vanishing_mid_read_is_an_unready_snapshot(self):
        result = self.observe("")
        self.assertEqual((result["ready"], result["owner"], result["owner_changed"]), (False, None, True))

    def test_failure_against_unchanged_owner_still_errors(self):
        with self.assertRaises(lane.subprocess.CalledProcessError):
            self.observe('s ":1.9"')


class ConstraintProofCleanup(unittest.TestCase):
    def test_original_error_survives_all_independent_cleanup_failures(self):
        calls = []
        def run(action):
            calls.append(action)
            raise RuntimeError("failed " + action)
        def restore(*arguments):
            calls.append("restore")
            raise RuntimeError("failed restoration")
        original = RuntimeError("original admission failure")
        with tempfile.TemporaryDirectory() as out, patch.object(lane, "restore_corner_mouse", side_effect=restore):
            with self.assertRaises(RuntimeError) as caught:
                try:
                    raise original
                finally:
                    lane.finish_constraint_proof(SimpleNamespace(run=run), object(), {}, out,
                        {"principal": None, "primary_error": {"message": str(original)}})
            self.assertIs(caught.exception, original)
            self.assertEqual(calls, ["constraint-quit", "restore", "constraint-log"])
            actual = json.loads((Path(out) / "hot-corner-constraints.json").read_text())
            self.assertIsNone(actual["principal"])
            self.assertEqual(actual["primary_error"]["message"], str(original))
            self.assertEqual([error["action"] for error in actual["cleanup_errors"]],
                             ["constraint-quit", "original-mouse-restoration", "constraint-log-retention"])

    def test_cleanup_only_failure_rejects_instead_of_qualifying(self):
        def run(action):
            if action == "constraint-quit":
                raise RuntimeError("original principal quit failed")
            return {"stderr": "original client log", "truncated": False}
        with tempfile.TemporaryDirectory() as out, patch.object(lane, "restore_corner_mouse", return_value={"verified": True}):
            with self.assertRaisesRegex(RuntimeError, "actual constraint cleanup failed"):
                lane.finish_constraint_proof(SimpleNamespace(run=run), object(), {}, out,
                    {"principal": {"pid": 42}, "primary_error": None})
            actual = json.loads((Path(out) / "hot-corner-constraints.json").read_text())
            self.assertIsNone(actual["primary_error"])
            self.assertEqual(actual["cleanup_errors"][0]["action"], "constraint-quit")
            self.assertTrue((Path(out) / "hot-corner-constraint-mouse-restoration.json").exists())
            self.assertTrue((Path(out) / "hot-corner-constraint-stderr.json").exists())
class OrcaLifecycleAuthority(unittest.TestCase):
    def test_compositor_authority_comes_from_actual_backend(self):
        source = (Path(__file__).resolve().parents[2] / "crates/compositor/src/runtime.rs").read_text()
        self.assertIn("let hardware = matches!(self.backend, Backend::Drm(_));", source)
        self.assertIn("self.orca.set_enabled(enabled && hardware);", source)
        self.assertIn("let hardware = false;", source)
        self.assertTrue('doc["screen_reader_pid"] = serde_json::json!(self.orca.pid());' in source)

    def test_no_global_orca_service_control_or_replace(self):
        root = Path(__file__).resolve().parents[2]
        compositor = (root / "crates/compositor/src/orca.rs").read_text()
        shell = (root / "crates/shell-gtk/src/orca.rs").read_text()
        self.assertNotIn('"--replace"', compositor)
        self.assertNotIn('"orca.service"', compositor)
        self.assertIn('Command::new("/usr/bin/orca")', compositor)
        self.assertIn('libc::PR_SET_PDEATHSIG, libc::SIGTERM', compositor)
        self.assertIn('libc::getppid() != parent', compositor)
        self.assertNotIn('gio::bus_get_future', shell)
        self.assertIn('control.set_screen_reader(enabled)', shell)




class BootApi(unittest.TestCase):
    def test_qmp_command_accepts_protocol_name_argument(self):
        qmp = lane.baseline.Qmp.__new__(lane.baseline.Qmp)
        qmp.file = io.StringIO()
        qmp._read = lambda: {"return": {}}
        with tempfile.TemporaryDirectory() as out:
            lane.Recorder(qmp, out)
            qmp.cmd("trace-event-set-state", name="virtio_gpu_cmd_set_scanout", enable=True)
        wire = json.loads(qmp.file.getvalue())
        self.assertEqual(wire, {"execute": "trace-event-set-state", "arguments": {
            "name": "virtio_gpu_cmd_set_scanout", "enable": True}})

    def test_existing_four_value_performance_call_keeps_original_devices(self):
        with patch.object(lane.os.path, "exists", return_value=True), patch.object(lane.os, "access", return_value=True), patch.object(lane.subprocess, "Popen") as spawn:
            process, qmp, serial, kvm = lane.boot("disk.raw", "/out", "/scratch", 30)
            self.assertIs(process, spawn.return_value)
            self.assertEqual((qmp, serial, kvm), ("/scratch/qmp.sock", "/out/serial.log", True))
            command = spawn.call_args.args[0]
            self.assertIn("-enable-kvm", command)
            self.assertEqual(command[command.index("-m") + 1], "6144")
            self.assertEqual(command[command.index("-smp") + 1], "4")
            self.assertIn("pcie-root-port,id=tuna-gpu-port,chassis=1,slot=1", command)
            self.assertIn("virtio-vga,bus=tuna-gpu-port,x-pcie-pm-no-soft-reset=on,xres=1280,yres=800", command)
            shared = lane.baseline.gpu_devices()
            offset = command.index(shared[1]) - 1
            self.assertEqual(command[offset:offset + len(shared)], shared)
            self.assertNotIn("virtio-serial-pci", command)
            self.assertFalse(any("guest_agent" in value for value in command))

    def test_explicit_guest_agent_adds_transport_and_returns_socket(self):
        with patch.object(lane.os.path, "exists", return_value=True), patch.object(lane.os, "access", return_value=True), patch.object(lane.subprocess, "Popen") as spawn:
            process, qmp, serial, kvm, agent = lane.boot("disk.raw", "/out", "/scratch", 30, guest_agent=True)
            self.assertIs(process, spawn.return_value)
            self.assertEqual(agent, "/scratch/qga.sock")
            command = spawn.call_args.args[0]
            self.assertIn("virtio-serial-pci", command)
            self.assertIn("virtserialport,chardev=tuna-qga,name=org.qemu.guest_agent.0", command)


class NativeMouseSelection(unittest.TestCase):
    def mouse(self, name="QEMU Virtio Mouse", absolute=False, current=False, index=8):
        return {"name":name,"absolute":absolute,"current":current,"index":index}
    def test_native_device_is_opt_in_and_preserves_performance_default(self):
        with patch.object(lane.os.path,"exists",return_value=True), patch.object(lane.os,"access",return_value=True), patch.object(lane.subprocess,"Popen") as spawn:
            lane.boot("disk.raw","/out","/scratch",30,guest_agent=True)
            self.assertNotIn("virtio-mouse-pci,id=tuna-relative-mouse",spawn.call_args.args[0])
            lane.boot("disk.raw","/out","/scratch",30,guest_agent=True,native_relative_mouse=True)
            self.assertIn("virtio-mouse-pci,id=tuna-relative-mouse",spawn.call_args.args[0])
    def test_actual_selected_name_index_and_mode_are_observed(self):
        class Qmp:
            def __init__(self):self.calls=[]
            def cmd(qmp,command,**arguments):
                qmp.calls.append((command,arguments))
                if command=='human-monitor-command':return ''
                return [self.mouse(current=len(qmp.calls)>1)]
        qmp=Qmp();proof=lane.select_corner_mouse(qmp,True)
        self.assertEqual(qmp.calls,[('query-mice',{}),('human-monitor-command',{'command-line':'mouse_set 8'}),('query-mice',{})])
        self.assertTrue(proof['after'][0]['current'])
        self.assertFalse(proof['requested_absolute'])
    def test_wrong_name_duplicate_mode_or_index_cannot_be_selected(self):
        bad=[[self.mouse(name='QEMU PS/2 Mouse')],[self.mouse(),self.mouse(index=9)],
             [self.mouse(absolute=True)],[self.mouse(index=True)],[self.mouse(index=-1)]]
        for inventory in bad:
            with self.subTest(inventory=inventory):
                qmp=SimpleNamespace(cmd=lambda *args,**kwargs:inventory)
                with self.assertRaises(RuntimeError):lane.select_corner_mouse(qmp,True)
    def test_failed_or_wrong_current_handler_cannot_prove_selection(self):
        for result in ([self.mouse(current=False)],[self.mouse(name='vmmouse',absolute=True,current=True)],
                       [self.mouse(current=True,index=9)]):
            replies=iter([[self.mouse()],'',result])
            with self.subTest(result=result),self.assertRaises(RuntimeError):
                lane.select_corner_mouse(SimpleNamespace(cmd=lambda *args,**kwargs:next(replies)),True)
    def test_original_handler_must_exist_and_restore_its_exact_identity(self):
        original=self.mouse(name='vmmouse',absolute=True,current=True,index=7)
        replies=iter([[original,self.mouse()],'',[original,self.mouse()]])
        proof=lane.restore_corner_mouse(SimpleNamespace(cmd=lambda *args,**kwargs:next(replies)),original)
        self.assertEqual(proof['after'][0],original)
        with self.assertRaisesRegex(RuntimeError,'disappeared'):
            lane.restore_corner_mouse(SimpleNamespace(cmd=lambda *args,**kwargs:[self.mouse()]),original)


class InputProbePlacement(unittest.TestCase):
    def test_click_uses_placement_after_first_buffer(self):
        class FrameReached(Exception):
            pass

        clicks = []
        inventory_calls = []
        provisional = {"state": {"windows": [
            {"app_id": "org.tuna.VmInput", "rect": [0, 0, 800, 600]}]}}
        placed = {"state": {"windows": [
            {"app_id": "org.tuna.VmInput", "rect": [500, 250, 400, 240]}]}}

        def run(action):
            if action == "inventory":
                inventory_calls.append(action)
                return provisional if len(inventory_calls) == 1 else placed
            if action == "input-ready":
                return {"pid": 42, "count": 0, "mapped": True,
                        "active": bool(clicks), "width": 400, "height": 240}
            self.assertEqual(action, "start-input-probe")
            return {"started": True}

        def frame(_path):
            raise FrameReached()

        qmp = SimpleNamespace(click=lambda x, y: clicks.append((x, y)), frame=frame)
        with tempfile.TemporaryDirectory() as out:
            with self.assertRaises(FrameReached):
                lane.run_lifecycle(qmp, SimpleNamespace(run=run), out, "/unused", lambda *_: None)
            self.assertEqual(clicks, [(600, 350)])
            mapped = json.loads((Path(out) / "lifecycle/input-mapped.json").read_text())
            self.assertEqual(mapped, placed)
            ready = json.loads((Path(out) / "lifecycle/input-ready.json").read_text())
            self.assertTrue(ready["active"])


class RepeatedCoverage(unittest.TestCase):
    def run_boots(self, root, manifests, returncodes=None):
        next_boot = iter(zip(manifests, returncodes or [0] * len(manifests)))

        def boot(command, check):
            self.assertFalse(check)
            manifest, code = next(next_boot)
            destination = Path(command[command.index("--out") + 1])
            destination.mkdir(parents=True, exist_ok=True)
            (destination / "manifest.json").write_text(json.dumps({"assertions": manifest}))
            return SimpleNamespace(returncode=code)

        args = SimpleNamespace(out=str(root), boots=5, disk="disk.raw", timeout=30,
                               tour=True, meta=[])
        with patch.object(lane.subprocess, "run", side_effect=boot):
            lane.repeat_boots(args)

    def test_final_lifecycle_check_does_not_claim_five_observations(self):
        startup = {"V-DRM": {"pass": True}}
        full = {**startup, "V-SUSPEND": {"pass": True}}
        with tempfile.TemporaryDirectory() as root:
            self.run_boots(root, [startup] * 4 + [full])
            proof = json.loads((Path(root) / "repeat-manifest.json").read_text())
            self.assertEqual(proof["assertion_coverage"]["V-DRM"]["passed_boots"], [1, 2, 3, 4, 5])
            self.assertEqual(proof["assertion_coverage"]["V-SUSPEND"]["observed_boots"], [5])
            lines = (Path(root) / "assertions.txt").read_text().splitlines()
            self.assertTrue(any(line.startswith("V-SUSPEND pass observed on 1/5") for line in lines))

    def test_missing_baseline_check_is_fatal_and_retained(self):
        startup = {"V-DRM": {"pass": True}}
        with tempfile.TemporaryDirectory() as root:
            with self.assertRaisesRegex(SystemExit, "repeated boot gate failed"):
                self.run_boots(root, [startup, {}, startup, startup, startup])
            proof = json.loads((Path(root) / "repeat-manifest.json").read_text())
            self.assertEqual(proof["assertion_coverage"]["V-DRM"]["missing_boots"], [2])
            self.assertIn("V-DRM fail observed on 4/5", (Path(root) / "assertions.txt").read_text())

    def test_observed_failure_and_failed_process_each_remain_fatal(self):
        startup = {"V-DRM": {"pass": True}}
        for last, codes in [({"V-DRM": {"pass": False}}, [0] * 5),
                            (startup, [0, 0, 0, 0, 1])]:
            with self.subTest(last=last, codes=codes), tempfile.TemporaryDirectory() as root:
                with self.assertRaisesRegex(SystemExit, "repeated boot gate failed"):
                    self.run_boots(root, [startup] * 4 + [last], codes)
                proof = json.loads((Path(root) / "repeat-manifest.json").read_text())
                self.assertEqual(proof["boot_results"][-1]["returncode"], codes[-1])
                self.assertIn("V-REPEAT fail", (Path(root) / "assertions.txt").read_text())


class PciCapabilities(unittest.TestCase):
    def probe(self, pm_control, pcie=True, cycle=False):
        helper = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        tree = ast.parse(helper.read_text())
        function = next(node for node in tree.body if isinstance(node, ast.FunctionDef)
                        and node.name == "gpu_pci_caps")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            drm = root / "drm"
            pci = root / "0000:01:00.0"
            (drm / "card1").mkdir(parents=True)
            pci.mkdir()
            (drm / "card1/device").symlink_to(pci, target_is_directory=True)
            data = bytearray(256)
            data[0x34] = 0x40
            data[0x40] = 1
            data[0x41] = 0x50 if pcie else (0x40 if cycle else 0)
            data[0x44:0x46] = pm_control.to_bytes(2, "little")
            if pcie:
                data[0x50] = 0x10
                data[0x51] = 0x40 if cycle else 0
            (pci / "config").write_bytes(data)
            namespace = {"pathlib": SimpleNamespace(Path=lambda path: drm)}
            exec(compile(ast.Module(body=[function], type_ignores=[]), str(helper), "exec"), namespace)
            return namespace["gpu_pci_caps"]()["card1"]

    def test_actual_no_soft_reset_and_pcie_are_reported(self):
        caps = self.probe(8)
        self.assertTrue(caps["no_soft_reset"])
        self.assertEqual(caps["power_state"], 0)
        self.assertEqual((caps["pm_capability"], caps["pcie_capability"]), (0x40, 0x50))
        self.assertEqual(caps["slot"], "0000:01:00.0")

    def test_d3hot_is_distinct_from_no_soft_reset(self):
        caps = self.probe(3)
        self.assertFalse(caps["no_soft_reset"])
        self.assertEqual(caps["power_state"], 3)

    def test_pm_without_pcie_cannot_masquerade_as_supported_topology(self):
        self.assertEqual(self.probe(8, pcie=False)["pcie_capability"], 0)

    def test_capability_cycle_is_bounded(self):
        self.assertTrue(self.probe(8, cycle=True)["no_soft_reset"])


class UsbBusDiscovery(unittest.TestCase):
    QTREE = """bus: main-system-bus
  type System
  dev: qemu-xhci, id ""
    bus: usb-bus.0
      type USB
      dev: usb-tablet, id ""
        port = "1"
      dev: usb-kbd, id ""
        port = "2"
"""

    def test_typed_usb_bus_wins(self):
        self.assertEqual(lane.usb_bus_from_qtree(self.QTREE), "usb-bus.0")

    def test_endpoint_devices_name_the_bus_without_a_type_line(self):
        qtree = self.QTREE.replace("      type USB\n", "")
        self.assertEqual(lane.usb_bus_from_qtree(qtree), "usb-bus.0")

    def test_controllers_never_pose_as_endpoints(self):
        qtree = ("bus: main-system-bus\n  type System\n"
                 "  dev: usb-ehci, id \"\"\n    bus: usb-bus.0\n")
        with self.assertRaises(RuntimeError):
            lane.usb_bus_from_qtree(qtree)

    def test_missing_usb_bus_raises(self):
        with self.assertRaises(RuntimeError):
            lane.usb_bus_from_qtree("bus: main-system-bus\n  type System\n")


class InterleavedShellStartup(unittest.TestCase):
    # Run 38102898073 repeat-04: serial/journal interleaving sheared the
    # shell's keybindings payload onto later records, leaving a bare
    # `tuna-shell-gtk: keybindings:` line that the old trailing-space
    # SHELL_RE missed (900 s harness hang; artifact 11689550567).
    FIXTURE = Path(__file__).resolve().parent / "fixtures" / "vm-lane-interleaved-shell-startup.log"

    def test_split_startup_prefix_is_recognized(self):
        text = self.FIXTURE.read_text()
        self.assertIsNotNone(lane.SHELL_RE.search(text))

    def test_unsplit_startup_line_still_matches(self):
        text = "tuna-shell-gtk: keybindings: 82 grabs, maximize on [\"<Super>Up\"]\n"
        self.assertIsNotNone(lane.SHELL_RE.search(text))

    def test_supervision_exhausted_still_rejects(self):
        text = self.FIXTURE.read_text() + lane.EXHAUSTED + "\n"
        shell = lane.SHELL_RE.search(text)
        self.assertTrue(shell)
        self.assertFalse(bool(shell) and lane.EXHAUSTED not in text)


class PointerEvidenceRetention(unittest.TestCase):
    def test_aggregate_does_not_overwrite_last_live_inventory(self):
        tree = ast.parse((Path(__file__).resolve().parents[1] / "tuna-vm-lane").read_text())
        aggregate = [n for n in ast.walk(tree) if isinstance(n, ast.Call)
                     and isinstance(n.func, ast.Attribute) and n.func.attr == "join"
                     and any(isinstance(a, ast.Constant) and a.value == "pointer-live-events.json"
                             for a in n.args)]
        self.assertEqual(len(aggregate), 1)
        overwritten = [n for n in ast.walk(tree) if isinstance(n, ast.Call)
                       and isinstance(n.func, ast.Attribute) and n.func.attr == "join"
                       and any(isinstance(a, ast.Constant) and a.value == "pointer-live.json"
                               for a in n.args)]
        self.assertEqual(overwritten, [])


class TourRecorderTests(unittest.TestCase):
    def recorder(self, directory, command, timeout=0.2):
        return lane.Recorder(SimpleNamespace(cmd=command), directory, timeout=timeout)

    def test_still_finishes_before_next_key_and_records_exact_frame(self):
        events = []
        def command(name, **args):
            events.append(name)
            if name == "screendump":
                Path(args["filename"]).write_bytes(b"completed pixels")
        with tempfile.TemporaryDirectory() as directory:
            recorder = self.recorder(directory, command)
            recorder.still("overview")
            recorder.qmp.cmd("send-key")
            self.assertEqual(events, ["screendump", "send-key"])
            self.assertEqual(recorder.stills, {"overview": 0})
            self.assertEqual(recorder.n, 1)
            self.assertEqual((Path(recorder.dir) / "f000000.png").read_bytes(), b"completed pixels")
            recorder.close()

    def test_periodic_and_still_frames_share_collision_free_indices(self):
        import threading
        entered, release = threading.Event(), threading.Event()
        calls = []
        def command(name, **args):
            calls.append(args["filename"])
            if len(calls) == 1:
                entered.set()
                self.assertTrue(release.wait(1))
            Path(args["filename"]).write_bytes(str(len(calls)).encode())
        with tempfile.TemporaryDirectory() as directory:
            recorder = self.recorder(directory, command, timeout=1)
            errors = []
            def capture():
                try:
                    recorder._capture()
                except BaseException as error:
                    errors.append(error)
            worker = threading.Thread(target=capture)
            worker.start()
            self.assertTrue(entered.wait(1))
            release.set()
            recorder.still("calendar")
            worker.join(1)
            self.assertFalse(worker.is_alive())
            self.assertEqual(errors, [])
            self.assertEqual(recorder.stills["calendar"], 1)
            self.assertEqual(recorder.n, 2)
            self.assertEqual(sorted(p.name for p in Path(recorder.dir).iterdir()),
                             ["f000000.png", "f000001.png"])
            recorder.close()

    def test_failed_capture_has_no_still_or_reused_frame(self):
        def command(name, **args):
            Path(args["filename"]).write_bytes(b"partial")
            raise RuntimeError("QMP refused")
        with tempfile.TemporaryDirectory() as directory:
            recorder = self.recorder(directory, command)
            with self.assertRaisesRegex(RuntimeError, "QMP refused"):
                recorder.still("lock")
            self.assertEqual(recorder.stills, {})
            self.assertEqual(recorder.n, 0)
            self.assertEqual(list(Path(recorder.dir).iterdir()), [])
            with self.assertRaisesRegex(RuntimeError, "capture failed"):
                recorder.close()

    def test_missing_frame_times_out_without_accepting_future_input(self):
        import time
        with tempfile.TemporaryDirectory() as directory:
            recorder = self.recorder(directory, lambda *args, **kwargs: None, timeout=0.03)
            start = time.monotonic()
            with self.assertRaises(TimeoutError):
                recorder.still("app-grid")
            self.assertLess(time.monotonic() - start, 1)
            with self.assertRaisesRegex(RuntimeError, "previously failed"):
                recorder.qmp.cmd("send-key")
            self.assertEqual(recorder.stills, {})
            with self.assertRaises(RuntimeError):
                recorder.close()

    def test_stalled_real_socket_times_out_and_recorder_thread_stops(self):
        import socket
        import threading
        left, right = socket.socketpair()
        entered = threading.Event()
        def command(*args, **kwargs):
            entered.set()
            return left.recv(1)
        try:
            qmp = SimpleNamespace(sock=left, cmd=command)
            with tempfile.TemporaryDirectory() as directory:
                recorder = lane.Recorder(qmp, directory, timeout=0.03)
                recorder.thread.start()
                self.assertTrue(entered.wait(1))
                with self.assertRaisesRegex(RuntimeError, "capture failed"):
                    recorder.close()
                self.assertFalse(recorder.thread.is_alive())
                self.assertEqual(recorder.n, 0)
                self.assertEqual(list(Path(recorder.dir).iterdir()), [])
                self.assertIs(qmp.cmd, recorder.original_cmd)
        finally:
            left.close()
            right.close()

    def test_frame_identity_collision_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            recorder = self.recorder(directory, lambda *args, **kwargs: self.fail("must not overwrite"))
            target = Path(recorder.dir) / "f000000.png"
            target.write_bytes(b"existing")
            with self.assertRaisesRegex(RuntimeError, "identity already exists"):
                recorder.still("intro")
            self.assertEqual(target.read_bytes(), b"existing")
            self.assertEqual(recorder.stills, {})
            with self.assertRaises(RuntimeError):
                recorder.close()

    def assert_continuous_transport_is_bounded(self, partial, background=False):
        import socket
        import threading
        import time
        left, right = socket.socketpair()
        stop, entered = threading.Event(), threading.Event()
        timers = []
        timer_class = threading.Timer
        def timer(*args, **kwargs):
            instance = timer_class(*args, **kwargs)
            timers.append(instance)
            return instance
        def server():
            right.settimeout(1)
            try:
                right.recv(4096)  # Genuine baseline.Qmp command was sent.
                entered.set()
                payload = b' ' if partial else b'{"event":"TEST"}\n'
                if partial:
                    right.sendall(b'{')
                while not stop.wait(0.002):
                    right.sendall(payload)
            except OSError:
                pass
        worker = threading.Thread(target=server)
        worker.start()
        qmp = lane.baseline.Qmp.__new__(lane.baseline.Qmp)
        qmp.sock = left
        qmp.file = left.makefile("rw")
        try:
            with tempfile.TemporaryDirectory() as directory, patch.object(lane.threading, "Timer", timer):
                recorder = lane.Recorder(qmp, directory, timeout=0.08)
                started = time.monotonic()
                if background:
                    recorder.thread.start()
                    self.assertTrue(entered.wait(1))
                    with self.assertRaisesRegex(RuntimeError, "capture failed"):
                        recorder.close()
                    self.assertFalse(recorder.thread.is_alive())
                else:
                    with self.assertRaises(TimeoutError):
                        recorder.still("overview")
                    with self.assertRaisesRegex(RuntimeError, "previously failed"):
                        recorder.qmp.cmd("send-key")
                    with self.assertRaisesRegex(RuntimeError, "capture failed"):
                        recorder.close()
                self.assertLess(time.monotonic() - started, 1)
                self.assertEqual(recorder.n, 0)
                self.assertEqual(recorder.stills, {})
                self.assertEqual(list(Path(recorder.dir).iterdir()), [])
                self.assertTrue(timers)
                self.assertTrue(all(not t.is_alive() for t in timers))
                self.assertEqual(left.recv(1), b'')  # Timeout permanently retires this transport.
        finally:
            stop.set()
            right.close()
            worker.join(1)
            qmp.file.close()
            left.close()
        self.assertFalse(worker.is_alive())

    def test_continuous_qmp_events_have_absolute_deadline(self):
        self.assert_continuous_transport_is_bounded(partial=False)

    def test_partial_qmp_line_dribble_has_absolute_deadline(self):
        self.assert_continuous_transport_is_bounded(partial=True)

    def test_background_event_flood_recorder_stops_without_timer_leak(self):
        self.assert_continuous_transport_is_bounded(partial=False, background=True)

    def test_success_cancels_watchdog_before_next_command(self):
        import socket
        import threading
        import time
        left, right = socket.socketpair()
        right.settimeout(1)
        timers, errors = [], []
        timer_class = threading.Timer
        def timer(*args, **kwargs):
            instance = timer_class(*args, **kwargs)
            timers.append(instance)
            return instance
        def server():
            try:
                with right.makefile("rw") as stream:
                    for _ in range(2):
                        request = json.loads(stream.readline())
                        if request["execute"] == "screendump":
                            Path(request["arguments"]["filename"]).write_bytes(b"complete")
                        stream.write('{"return":{}}\n')
                        stream.flush()
            except BaseException as error:
                errors.append(error)
        worker = threading.Thread(target=server)
        worker.start()
        qmp = lane.baseline.Qmp.__new__(lane.baseline.Qmp)
        qmp.sock, qmp.file = left, left.makefile("rw")
        try:
            with tempfile.TemporaryDirectory() as directory, patch.object(lane.threading, "Timer", timer):
                recorder = lane.Recorder(qmp, directory, timeout=0.08)
                recorder.still("intro")
                time.sleep(0.1)  # Beyond the canceled first watchdog's deadline.
                self.assertEqual(qmp.cmd("send-key"), {})
                recorder.close()
                self.assertEqual(len(timers), 2)
                self.assertTrue(all(not t.is_alive() for t in timers))
        finally:
            right.close()
            worker.join(1)
            qmp.file.close()
            left.close()
        self.assertFalse(worker.is_alive())
        self.assertEqual(errors, [])

    def test_mark_lock_wait_is_bounded(self):
        import threading
        held, release = threading.Event(), threading.Event()
        with tempfile.TemporaryDirectory() as directory:
            recorder = self.recorder(directory, lambda *args, **kwargs: None, timeout=0.02)
            def hold():
                with recorder.lock:
                    held.set()
                    release.wait(1)
            worker = threading.Thread(target=hold)
            worker.start()
            try:
                self.assertTrue(held.wait(1))
                with self.assertRaises(TimeoutError):
                    recorder.mark("overview")
                self.assertEqual(recorder.marks, [])
            finally:
                release.set()
                worker.join(1)
            self.assertFalse(worker.is_alive())
            with self.assertRaisesRegex(RuntimeError, "capture failed"):
                recorder.close()

    def test_existing_pending_capture_is_never_removed(self):
        with tempfile.TemporaryDirectory() as directory:
            recorder = self.recorder(directory, lambda *args, **kwargs: self.fail("must not overwrite"))
            target = Path(recorder.dir) / "f000000.png.pending"
            target.write_bytes(b"existing pending")
            with self.assertRaisesRegex(RuntimeError, "identity already exists"):
                recorder.still("intro")
            self.assertEqual(target.read_bytes(), b"existing pending")
            with self.assertRaises(RuntimeError):
                recorder.close()

    def test_periodic_indexing_and_marks_have_no_future_frame_offset(self):
        def command(name, **args):
            Path(args["filename"]).write_bytes(b"frame")
        with tempfile.TemporaryDirectory() as directory:
            recorder = self.recorder(directory, command)
            recorder._capture()
            recorder.mark("overview")
            recorder.still("overview")
            self.assertEqual(recorder.marks, [(1, "overview")])
            self.assertEqual(recorder.stills, {"overview": 1})
            self.assertEqual(recorder.n, 2)
            self.assertEqual([frame["index"] for frame in recorder.frames], [0, 1])
            self.assertEqual([frame["mode"] for frame in recorder.frames], ["periodic", "still"])
            for frame in recorder.frames:
                self.assertLessEqual(frame["requested_monotonic_ns"], frame["completed_monotonic_ns"])
                self.assertEqual(frame["sha256"], lane.baseline.sha256(
                    str(Path(recorder.dir) / f"f{frame['index']:06d}.png")))
            recorder.close()
        self.assertEqual(lane.TOUR_FPS, 2)
        self.assertEqual(lane.PLAY_FPS, 4)

    def test_failed_tour_capture_removes_owned_work_and_restores_command(self):
        original = lane.Recorder
        recorders = []
        def construct(qmp, directory):
            recorder = original(qmp, directory, timeout=0.02)
            recorder.thread = SimpleNamespace(start=lambda: None, ident=None)
            recorders.append(recorder)
            return recorder
        command = lambda *args, **kwargs: None
        qmp = SimpleNamespace(cmd=command, keys=lambda *args, **kwargs: None)
        with tempfile.TemporaryDirectory() as directory:
            with patch.object(lane, "Recorder", construct), patch.object(lane.subprocess, "run"), \
                    patch.object(lane.time, "sleep", lambda delay: None):
                with self.assertRaises(TimeoutError):
                    lane.run_tour(qmp, directory, "unused")
            self.assertFalse((Path(directory) / "tour/.work").exists())
            self.assertIs(qmp.cmd, command)
            self.assertEqual(recorders[0].stills, {})

    def test_tour_renders_current_cards_into_scratch_and_ci_has_dependencies(self):
        source = Path(lane.__file__).read_text()
        self.assertIn('os.path.join(CARDS, "render.py")', source)
        self.assertIn('"--out", cards_dir', source)
        self.assertIn('os.path.join(cards_dir, f"{card}.png")', source)
        self.assertNotIn('os.path.join(CARDS, f"{card}.png")', source)
        workflow = (Path(__file__).resolve().parents[2] / ".github/workflows/ci.yml").read_text()
        self.assertIn('qemu-utils ovmf ffmpeg python3-pil fonts-dejavu-core', workflow)



class SleepAuthRetirementTests(unittest.TestCase):
    def setUp(self):
        self.snapshot = {'state': {'locked': True, 'auth_owner': {
            'outer_pending': True, 'epoch': 7,
            'attempt': {'serial': 9, 'epoch': 7, 'origin': {'connection': 4, 'request': 12}}}},
            'processes': [{'executable': 'tuna-compositor', 'pid': 600, 'start_ticks': 1500}]}
        self.expected = lane.sleep_auth_attempt(self.snapshot)
        self.event = {'pid': 600, 'joined_ok': True, 'backend_accepted': True, 'reported_ok': True,
                      'applied': False, 'attempt_serial': 9, 'attempt_epoch': 7, 'current_epoch': 8,
                      'origin': {'connection': 4, 'request': 12}, 'origin_live': True, 'locked_after': True}
        self.denial = 'tuna-shell-gtk: lock command result applied=false request_id=12'

    def trace(self, event=None, denial=None):
        return ('journal tuna-compositor: lock authentication retired '
                + json.dumps(self.event if event is None else event) + '\n'
                + (self.denial if denial is None else denial) + '\n')

    def test_exact_pending_attempt_accepted_join_stale_epoch_and_shell_denial(self):
        self.assertEqual(lane.stale_sleep_auth_retirement(self.trace(), self.expected, 8), self.event)
        self.assertEqual(self.expected['start_ticks'], 1500)

    def test_wrong_password_panic_unreported_applied_dead_origin_or_unlocked_refused(self):
        for key, value in [('backend_accepted', False), ('joined_ok', False), ('reported_ok', False),
                           ('applied', True), ('origin_live', False), ('locked_after', False)]:
            with self.subTest(key=key):
                self.assertIsNone(lane.stale_sleep_auth_retirement(self.trace({**self.event, key: value}), self.expected, 8))

    def test_same_epoch_wrong_current_epoch_and_unrelated_attempt_refused(self):
        for event, current in [({**self.event, 'current_epoch': 7}, 7), (self.event, 9),
                               ({**self.event, 'attempt_serial': 10}, 8),
                               ({**self.event, 'pid': 601}, 8),
                               ({**self.event, 'origin': {'connection': 5, 'request': 12}}, 8),
                               ({**self.event, 'origin': {'connection': 4, 'request': 13}}, 8)]:
            self.assertIsNone(lane.stale_sleep_auth_retirement(self.trace(event), self.expected, current))

    def test_missing_malformed_wrong_type_extra_fields_and_duplicate_retirement_refused(self):
        for trace in ['', self.denial, 'tuna-compositor: lock authentication retired {broken',
                      self.trace({k: v for k, v in self.event.items() if k != 'backend_accepted'}),
                      self.trace({**self.event, 'backend_accepted': 1}),
                      self.trace({**self.event, 'attempt_serial': True}),
                      self.trace({**self.event, 'origin': None}),
                      self.trace({**self.event, 'extra': True}), self.trace() + self.trace(),
                      'lock authentication finished accepted=true applied=false\n' + self.denial]:
            self.assertIsNone(lane.stale_sleep_auth_retirement(trace, self.expected, 8))

    def test_shell_missing_wrong_request_or_success_refused(self):
        for denial in ['', self.denial.replace('12', '13'), self.denial.replace('false', 'true'),
                       'tuna-shell-gtk: lock command result applied=false']:
            self.assertIsNone(lane.stale_sleep_auth_retirement(self.trace(denial=denial), self.expected, 8))

    def test_pre_sleep_snapshot_missing_not_pending_or_bad_identity_refused(self):
        import copy
        snapshots = []
        for field, value in [('attempt', None), ('outer_pending', False), ('epoch', True), ('epoch', 8)]:
            changed = copy.deepcopy(self.snapshot); changed['state']['auth_owner'][field] = value; snapshots.append(changed)
        for field, value in [('serial', True), ('epoch', 0), ('origin', None)]:
            changed = copy.deepcopy(self.snapshot); changed['state']['auth_owner']['attempt'][field] = value; snapshots.append(changed)
        changed = copy.deepcopy(self.snapshot); changed['processes'] *= 2; snapshots.append(changed)
        changed = copy.deepcopy(self.snapshot); changed['state']['locked'] = False; snapshots.append(changed)
        for snapshot in snapshots:
            with self.assertRaises(ValueError):
                lane.sleep_auth_attempt(snapshot)

class PrivatePamSleepControls(unittest.TestCase):
    def setUp(self):
        self.legacy = SleepAuthRetirementTests()
        self.legacy.setUp()
        self.expected = self.legacy.expected
        self.desktop = dict(pid=600, uid=1000, ppid=2, pgrp=2, start_ticks=1500,
                            executable="tuna-compositor", exe_device=1, exe_inode=77)
        self.helper = dict(self.desktop, pid=700, ppid=600, pgrp=700, start_ticks=1700)
        self.delay = dict(self.helper, pid=701, ppid=700, start_ticks=1800, executable="sleep", exe_inode=78)
        self.observed = {"real_pam_delay_running": True, "compositor": self.desktop,
                         "helpers": [self.helper], "delays": [self.delay],
                         "auth_owner": self.legacy.snapshot["state"]["auth_owner"]}
        self.original = lane.sleep_auth_helper_observation(self.observed, self.legacy.snapshot)
        self.physical = dict(helper_absent=True, delay_absent=True, owned_group_members=[], original=self.original)
        self.event = dict(pid=600, joined_ok=True, retired=True, authenticated=False,
                          denied=True, cancel_requested=True, reported_ok=False, applied=False,
                          attempt_serial=9, attempt_epoch=7, current_epoch=8,
                          origin=dict(connection=4, request=12), locked_after=True)

    def trace(self, event=None, denial=None):
        return 'tuna-compositor: lock authentication owned retirement ' + json.dumps(self.event if event is None else event) + '\n' + (self.legacy.denial if denial is None else denial)

    def match(self, trace=None, original=None, physical=None, current=8):
        return lane.cancelled_sleep_auth_retirement(self.trace() if trace is None else trace,
            self.expected, current, self.original if original is None else original,
            self.physical if physical is None else physical)

    def test_original_helper_delay_and_canceled_actual_retired_false_positive(self):
        self.assertEqual(self.match(), self.event)

    def test_no_helper_arbitrary_sleep_or_wrong_original_attempt_refused(self):
        import copy
        changes = [dict(self.observed, real_pam_delay_running=False),
                   dict(self.observed, helpers=[]), dict(self.observed, delays=[]),
                   dict(self.observed, delays=[dict(self.delay, ppid=600)]),
                   dict(self.observed, helpers=[dict(self.helper, exe_inode=78)]),
                   dict(self.observed, helpers=[dict(self.helper, pgrp=600)]),
                   dict(self.observed, helpers=[dict(self.helper, uid=1001)]),
                   dict(self.observed, compositor=dict(self.desktop, start_ticks=1501)),
                   dict(self.observed, helpers=[self.helper, self.helper]),
                   dict(self.observed, delays=[dict(self.delay, pid=True)])]
        changed = copy.deepcopy(self.observed); changed['auth_owner']['attempt']['serial'] = 10; changes.append(changed)
        for value in changes:
            with self.subTest(value=value), self.assertRaises(ValueError):
                lane.sleep_auth_helper_observation(value, self.legacy.snapshot)
        for original in ({}, dict(self.original, helper=None), dict(self.original, attempt={**self.expected, 'serial':10})):
            self.assertIsNone(self.match(original=original))

    def test_cancel_without_retirement_unretired_success_unlock_or_wrong_receipt_refused(self):
        for key, value in [('joined_ok',False), ('retired',False), ('authenticated',True),
                           ('denied',False), ('cancel_requested',False), ('reported_ok',True),
                           ('applied',True), ('locked_after',False), ('attempt_serial',10),
                           ('attempt_epoch',6), ('current_epoch',7), ('pid',601), ('retired',1),
                           ('origin',{'connection':5,'request':12})]:
            with self.subTest(key=key):
                self.assertIsNone(self.match(trace=self.trace({**self.event,key:value})))
        self.assertIsNone(self.match(current=9))
        for physical in [dict(self.physical,helper_absent=False), dict(self.physical,delay_absent=False),
                         dict(self.physical,owned_group_members=[dict(pid=702,start_ticks=1801)]),
                         dict(self.physical,original=dict(self.original,helper=dict(self.helper,start_ticks=1800))), {},
                         dict(self.physical,helper_absent=1)]:
            self.assertIsNone(self.match(physical=physical))

    def test_missing_malformed_duplicate_or_wrong_shell_denial_refused(self):
        for trace in ['', self.legacy.trace(), self.trace()+ '\n'+self.trace(),
                      'tuna-compositor: lock authentication owned retirement {broken',
                      self.trace({**self.event,'extra':True}),
                      self.trace({key:value for key,value in self.event.items() if key != 'retired'}),
                      self.trace(denial=''), self.trace(denial=self.legacy.denial.replace('12','13')),
                      self.trace(denial=self.legacy.denial.replace('false','true'))]:
            self.assertIsNone(self.match(trace=trace))

    def production(self):
        source = Path(__file__).resolve().parents[2] / 'packaging/marlin/vm-lane/tuna-vm-lifecycle'
        wanted = {'auth_process','public_auth_identity','classify_pam_helpers','pam_process_snapshot','delayed_auth_state','auth_retirement_state'}
        nodes = [node for node in ast.parse(source.read_text()).body if isinstance(node,ast.FunctionDef) and node.name in wanted]
        scope = dict(pathlib=__import__('pathlib'), os=__import__('os'), json=json,
                     OWNER=SimpleNamespace(pw_uid=1000), PAM_HELPER_FLAG=b'--internal-pam-conversation')
        exec(compile(ast.Module(body=nodes,type_ignores=[]),str(source),'exec'),scope)
        return scope

    def test_actual_production_helper_classifier_rejects_wrong_fixed_flag_parent_inode_and_uid(self):
        scope = self.production()
        def private(row, flag=False):
            return dict(row,_marked=flag,_private=flag,_state='S')
        desktop, helper = private(self.desktop), private(self.helper,True)
        self.assertEqual(scope['classify_pam_helpers']([desktop,helper]),(desktop,[helper]))
        for bad in [dict(helper,_private=False), dict(helper,ppid=999), dict(helper,exe_inode=78),
                    dict(helper,uid=1001), dict(helper,pgrp=600), dict(helper,_state='Z')]:
            self.assertEqual(scope['classify_pam_helpers']([desktop,bad])[1],[])
        self.assertEqual(scope['classify_pam_helpers']([desktop,dict(desktop,pid=601),helper]),(None,[]))

    def test_actual_production_process_reader_and_absence_reject_pid_replacement_and_zombie(self):
        import os, pathlib
        scope = self.production()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root/'tuna-compositor'; binary.write_bytes(b'owned controlled file')
            def process(pid,parent,group,ticks,args):
                path=root/str(pid); path.mkdir()
                fields=['S',str(parent),str(group)]+['0']*16+[str(ticks)]
                (path/'stat').write_text(f'{pid} (controlled) '+ ' '.join(fields))
                (path/'cmdline').write_bytes(args)
                (path/'exe').symlink_to(binary)
            process(600,2,2,1500,b'/usr/bin/tuna-compositor\0')
            process(700,600,700,1700,b'/proc/self/fd/11\0--internal-pam-conversation\0')
            def proc_path(path):
                return root if path=='/proc' else root/str(path).split('/')[-1]
            scope['pathlib']=SimpleNamespace(Path=proc_path)
            scope['OWNER']=SimpleNamespace(pw_uid=os.getuid())
            scope['auth_process'].__globals__['pathlib']=scope['pathlib']
            desktop=scope['auth_process'](600); helper=scope['auth_process'](700)
            self.assertFalse(desktop['_private']); self.assertTrue(helper['_private'])
            expected=dict(compositor=scope['public_auth_identity'](desktop),helper=scope['public_auth_identity'](helper),delay=dict(self.delay),attempt=self.expected)
            # A missing exe while /proc/<pid> still exists is not PID absence.
            (root/'700/exe').unlink()
            absent=scope['auth_retirement_state'](expected)
            self.assertFalse(absent['helper_absent']); self.assertTrue(absent['owned_group_members'])
            (root/'700/exe').symlink_to(binary)
            (root/'700/stat').write_text('700 (controlled) '+ ' '.join(['S','600','700']+['0']*16+['1701']))
            with self.assertRaisesRegex(RuntimeError,'PID was replaced'):
                scope['auth_retirement_state'](expected)

    def test_runtime_observer_is_typed_retired_and_legacy_accepted_epoch_control_preserved(self):
        source=Path(__file__).resolve().parents[2]/'crates/compositor/src/runtime.rs'
        text=source.read_text()
        self.assertIn('let cancel_requested = worker.cancel.is_canceled();',text)
        self.assertIn('"retired":matches!(result, crate::pam_backend::Verification::Retired { .. })',text)
        self.assertIn('"cancel_requested":cancel_requested',text)
        self.assertEqual(lane.stale_sleep_auth_retirement(self.legacy.trace(),self.expected,8),self.legacy.event)



class S3AbsoluteDeadline(unittest.TestCase):
    def execute(self, phase, *, late_action=None, late_predicate=None, advance_action=None, deadline=5):
        source = Path(__file__).resolve().parents[1] / "tuna-vm-lane"
        tree = ast.parse(source.read_text())
        lifecycle = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == "run_lifecycle")
        inventory = next(n for n in lifecycle.body if isinstance(n, ast.FunctionDef) and n.name == "inventory")
        helpers = [n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name in ("s3_remaining", "s3_observe")]
        needle = 'delayed-auth-state' if phase == 'admission' else 'sleep-auth-pending'
        loop = next(n for n in ast.walk(lifecycle) if isinstance(n, ast.While) and needle in ast.dump(n))
        now = [0.0]
        calls = []
        class Agent:
            def run(self, action, *args, **kwargs):
                calls.append((action, kwargs))
                if action == advance_action:
                    now[0] += 1
                if action == late_action:
                    now[0] = deadline
                if action == 'delayed-auth-state':
                    return {'real_pam_delay_running': True}
                if action == 'inventory':
                    return {'state': {'locked': True, 'auth_owner': {'epoch': 8}}}
                return {'helper_absent': True}
        def predicate(name, result):
            def run(*args):
                if name == late_predicate: now[0] = deadline
                return result
            return run
        with tempfile.TemporaryDirectory() as tmp:
            scope = dict(time=SimpleNamespace(monotonic=lambda:now[0], sleep=lambda d:now.__setitem__(0, now[0]+d)),
                         agent=Agent(), evidence=tmp, os=lane.os, json=json, deadline=deadline,
                         desktop={'pid':1,'start_ticks':2}, observation_index=0,
                         before={}, same_apps=predicate('same_apps', True),
                         sleep_auth_attempt=predicate('attempt', {'serial':9}),
                         sleep_auth_helper_observation=predicate('helper', {'helper':7}),
                         cancelled_sleep_auth_retirement=predicate('retirement', {'retired':True}),
                         read_text=lambda p:'actual serial', serial_path='serial', stale_auth_offset=0,
                         original_method={}, original_auth={}, stale_rejected=False,
                         record=lambda *args:None)
            exec(compile(ast.Module(body=helpers+[inventory], type_ignores=[]),str(source),'exec'),scope)
            try:
                exec(compile(ast.Module(body=[loop], type_ignores=[]),str(source),'exec'),scope)
            except RuntimeError as error:
                self.assertIn('deadline expired', str(error))
                return calls, False, Path(tmp,'sleep-auth-retirement.json').exists()
            return calls, True, Path(tmp,'sleep-auth-retirement.json').exists()

    def test_actual_admission_loop_clips_both_reads(self):
        calls, accepted, receipt = self.execute('admission')
        self.assertTrue(accepted)
        self.assertEqual([action for action,_ in calls], ['delayed-auth-state','inventory'])
        self.assertTrue(all(0 < kw['timeout'] <= 5 for _,kw in calls))
        self.assertFalse(receipt)

    def test_second_qga_operation_uses_recomputed_remaining_budget(self):
        for phase, action, budget in [('admission','delayed-auth-state',5), ('retirement','inventory',25)]:
            with self.subTest(phase=phase):
                calls,accepted,_ = self.execute(phase,advance_action=action,deadline=budget)
                self.assertTrue(accepted)
                self.assertEqual(calls[0][1]['timeout'],budget)
                self.assertEqual(calls[1][1]['timeout'],budget-1)

    def test_actual_admission_rejects_late_read_and_validation(self):
        for action in ('delayed-auth-state','inventory'):
            with self.subTest(action=action):
                _,accepted,_ = self.execute('admission',late_action=action)
                self.assertFalse(accepted)
        for name in ('same_apps','attempt','helper'):
            with self.subTest(predicate=name):
                _,accepted,_ = self.execute('admission',late_predicate=name)
                self.assertFalse(accepted)

    def test_actual_retirement_loop_clips_inventory_and_physical_reads(self):
        calls,accepted,receipt = self.execute('retirement',deadline=25)
        self.assertTrue(accepted); self.assertTrue(receipt)
        self.assertEqual([action for action,_ in calls], ['inventory','auth-retirement-state'])
        self.assertTrue(all(0 < kw['timeout'] <= 25 for _,kw in calls))

    def test_actual_retirement_rejects_late_read_or_matcher_before_receipt(self):
        for action in ('inventory','auth-retirement-state'):
            with self.subTest(action=action):
                _,accepted,receipt = self.execute('retirement',late_action=action,deadline=25)
                self.assertFalse(accepted); self.assertFalse(receipt)
        for name in ('same_apps','retirement'):
            with self.subTest(predicate=name):
                _,accepted,receipt = self.execute('retirement',late_predicate=name,deadline=25)
                self.assertFalse(accepted); self.assertFalse(receipt)

    def test_default_inventory_call_remains_unchanged(self):
        source = Path(__file__).resolve().parents[1] / "tuna-vm-lane"
        lifecycle = next(n for n in ast.parse(source.read_text()).body if isinstance(n,ast.FunctionDef) and n.name=='run_lifecycle')
        inventory = next(n for n in lifecycle.body if isinstance(n,ast.FunctionDef) and n.name=='inventory')
        calls=[]
        with tempfile.TemporaryDirectory() as tmp:
            scope=dict(agent=SimpleNamespace(run=lambda *a,**kw: calls.append((a,kw)) or {}),
                       evidence=tmp,os=lane.os,json=json,
                       s3_observe=lambda *a,**kw: (_ for _ in ()).throw(AssertionError('unexpected deadline path')))
            exec(compile(ast.Module(body=[inventory],type_ignores=[]),str(source),'exec'),scope)
            self.assertEqual(scope['inventory']('ordinary'),{})
        self.assertEqual(calls,[(('inventory',),{})])


class InterleavedShellStartup(unittest.TestCase):
    # Run 38102898073 repeat-04: serial/journal interleaving sheared the
    # shell's keybindings payload onto later records, leaving a bare
    # `tuna-shell-gtk: keybindings:` line that the old trailing-space
    # SHELL_RE missed (900 s harness hang; artifact 11689550567).
    FIXTURE = Path(__file__).resolve().parent / "fixtures" / "vm-lane-interleaved-shell-startup.log"

    def test_split_startup_prefix_is_recognized(self):
        text = self.FIXTURE.read_text()
        self.assertIsNotNone(lane.SHELL_RE.search(text))

    def test_unsplit_startup_line_still_matches(self):
        text = "tuna-shell-gtk: keybindings: 82 grabs, maximize on [\"<Super>Up\"]\n"
        self.assertIsNotNone(lane.SHELL_RE.search(text))

    def test_supervision_exhausted_still_rejects(self):
        text = self.FIXTURE.read_text() + lane.EXHAUSTED + "\n"
        shell = lane.SHELL_RE.search(text)
        self.assertTrue(shell)
        self.assertFalse(bool(shell) and lane.EXHAUSTED not in text)


if __name__ == "__main__":
    unittest.main()
