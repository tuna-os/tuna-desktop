#!/usr/bin/env python3
"""Protect the shared boot API used by the independent performance lane."""
import importlib.machinery
import importlib.util
import io
import ast
import base64
import json
import os
import shlex
import subprocess
import tempfile
from pathlib import Path
import unittest
from unittest.mock import patch
from types import SimpleNamespace

loader = importlib.machinery.SourceFileLoader("vm_boot_lane", str(Path(__file__).resolve().parents[1] / "tuna-vm-lane"))
spec = importlib.util.spec_from_loader(loader.name, loader)
lane = importlib.util.module_from_spec(spec)
loader.exec_module(lane)


def guest_identity(prefix="tuna"):
    """The lifecycle helper's shipped-identity globals for a guest prefix."""
    compositor = f"{prefix}-compositor"
    return {"PREFIX": prefix, "COMPOSITOR": compositor, "COMPOSITOR_COMM": compositor[:15],
            "SHELL_GTK": f"{prefix}-shell-gtk", "NESTED": f"{prefix}-nested-",
            "LOCK_PAM": f"/etc/pam.d/{prefix}-lock"}


class DeviceInventoryProjection(unittest.TestCase):
    def inventory(self, state):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        fn = next(n for n in ast.parse(source.read_text()).body
                  if isinstance(n, ast.FunctionDef) and n.name == "inventory")
        def call(*args, check=True):
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

    def test_missing_device_snapshot_stays_absent_never_an_empty_list(self):
        # Published pre-key guests still project (ShippedSnapshotInventory);
        # their missing list must not read as "no devices", which consumers
        # would accept as a real empty inventory.
        projected = self.inventory(self.state())["state"]
        self.assertIn("input_devices", projected)
        self.assertIsNone(projected["input_devices"])


class OrcaCommandWindow(unittest.TestCase):
    def check(self, started=10.0, ready=10.5, before=11.0, after=11.1, samples=96000):
        return lane.require_speech_trigger_window({"receipt": {"started_monotonic": started,
            "ready_monotonic": ready}}, {"guest_monotonic": before}, {"guest_monotonic": after}, samples)
    def test_original_trigger_in_actual_bounded_pcm_window(self):
        self.check()
    def test_delayed_trigger_and_exact_end_refused_without_padding(self):
        for before, after in ((14.0, 14.1), (13.9, 14.0), (11.1, 11.0), (10.4, 11.0)):
            with self.subTest(before=before, after=after), self.assertRaises(RuntimeError):
                self.check(before=before, after=after)
    def test_finite_original_receipts_and_sample_bound_required(self):
        for kwargs in ({"started": float("nan")}, {"ready": float("inf")}, {"before": True},
                       {"after": -1}, {"samples": 96001}, {"samples": True}, {"ready": 9.9}):
            with self.subTest(kwargs=kwargs), self.assertRaises(RuntimeError): self.check(**kwargs)
    def test_actual_command_calls_window_validator_with_original_receipts(self):
        node = next(n for n in ast.walk(ast.parse(Path(lane.__file__).read_text()))
                    if isinstance(n, ast.FunctionDef) and n.name == "native_command")
        call = next(n for n in ast.walk(node) if isinstance(n, ast.Call)
                    and isinstance(n.func, ast.Name) and n.func.id == "require_speech_trigger_window")
        self.assertEqual([ast.unparse(n) for n in call.args],
                         ["start_speech", "trigger_before", "trigger_after", "len(values)"])


class OrcaPublicWindow(unittest.TestCase):
    def receipts(self):
        profile = {"name": "public-navigation", "rate": 24000, "channels": 1,
                   "sample_width": 2, "sample_count": 192000, "runtime_max_sec": 12}
        return ({"receipt": {"profile": dict(profile), "started_monotonic": 241.86064434,
                             "ready_monotonic": 242.236375991}},
                {"receipt": {"profile": dict(profile)}},
                {"guest_monotonic": 243.524247449}, {"guest_monotonic": 247.041191809})
    def test_original_actual_full_navigation_bracket_needs_actual_samples(self):
        args = self.receipts()
        lane.require_public_speech_window(*args, 192000)
        with self.assertRaises(RuntimeError): lane.require_public_speech_window(*args, 96000)
    def test_exact_actual_end_delayed_reversed_or_preready_trigger_rejected(self):
        for before, after in ((243, 249.86064434), (243, 250), (244, 243), (242, 243)):
            args = self.receipts()
            args[2]["guest_monotonic"] = before; args[3]["guest_monotonic"] = after
            with self.subTest(before=before, after=after), self.assertRaises(RuntimeError):
                lane.require_public_speech_window(*args, 192000)
    def test_finite_samples_and_original_matching_profile_required(self):
        for key, value in (("rate", True), ("sample_count", 192001), ("name", "native-command"),
                           ("channels", 2), ("runtime_max_sec", 13)):
            args = self.receipts(); args[1]["receipt"]["profile"][key] = value
            with self.subTest(key=key), self.assertRaises(RuntimeError):
                lane.require_public_speech_window(*args, 192000)
        for samples in (True, -1, 192001, 192000.0):
            with self.subTest(samples=samples), self.assertRaises(RuntimeError):
                lane.require_public_speech_window(*self.receipts(), samples)
        for value in (True, float("nan"), float("inf"), -1):
            args = self.receipts(); args[3]["guest_monotonic"] = value
            with self.subTest(value=value), self.assertRaises(RuntimeError):
                lane.require_public_speech_window(*args, 192000)
    def test_guest_fixed_profiles_and_changed_recording_refused(self):
        source = Path(lane.__file__).parents[1] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        nodes = [n for n in ast.parse(source.read_text()).body if isinstance(n, ast.FunctionDef)
                 and n.name in {"speech_capture_profile", "recording_profile"}]
        scope = {}; exec(compile(ast.Module(body=nodes, type_ignores=[]), "actual-profile", "exec"), scope)
        for name, count, limit in (("native-command", 96000, 8), ("public-navigation", 192000, 12)):
            profile = scope["speech_capture_profile"](name)
            self.assertEqual((profile["sample_count"], profile["runtime_max_sec"]), (count, limit))
            self.assertEqual(scope["recording_profile"]({"profile": profile}), profile)
            with self.assertRaises(RuntimeError):
                scope["recording_profile"]({"profile": dict(profile, channels=True)})
        for name in (True, [], "private", "public-navigation-extra"):
            with self.assertRaises(RuntimeError): scope["speech_capture_profile"](name)
    def test_public_profile_only_and_original_guard_before_lock_privacy(self):
        tree = ast.parse(Path(lane.__file__).read_text())
        public = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == "public_capture")
        native = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == "native_command")
        starts = lambda node: [n for n in ast.walk(node) if isinstance(n, ast.Call)
                and isinstance(n.func, ast.Attribute) and n.func.attr == "run" and n.args
                and isinstance(n.args[0], ast.Constant) and n.args[0].value == "orca-speech-start"]
        self.assertEqual([ast.literal_eval(a) for a in starts(public)[0].args],
                         ["orca-speech-start", "public-navigation"])
        self.assertEqual([ast.literal_eval(a) for a in starts(native)[0].args], ["orca-speech-start"])
        validator = next(n for n in ast.walk(public) if isinstance(n, ast.Call)
                         and isinstance(n.func, ast.Name) and n.func.id == "require_public_speech_window")
        self.assertEqual([ast.unparse(a) for a in validator.args],
                         ["start", "end", "before_trigger", "after_trigger", "len(samples)"])
        cleanup = next(n for n in ast.walk(public) if isinstance(n, ast.Try) and n.finalbody)
        self.assertTrue(any(isinstance(n, ast.Call) and isinstance(n.func, ast.Attribute)
                            and n.func.attr == 'run' and n.args and isinstance(n.args[0], ast.Constant)
                            and n.args[0].value == 'orca-speech-end' for n in ast.walk(cleanup.finalbody[0])))
        source = Path(lane.__file__).read_text()
        self.assertLess(source.index('public_capture("orca-quick-settings-volume"'), source.index('def pam_unlock('))


class OrcaCommandCleanup(unittest.TestCase):
    def exercise(self, failure, cleanup_failure=False):
        source = Path(lane.__file__).read_text()
        node = next(n for n in ast.walk(ast.parse(source))
                    if isinstance(n, ast.FunctionDef) and n.name == "native_command")
        calls = []
        original = RuntimeError("controlled original command failure")
        secondary = ValueError("controlled cleanup failure")
        class Agent:
            def run(self, action):
                calls.append(action)
                if action == "orca-speech-start":
                    if failure == "start": raise original
                    return {}
                if action == "orca-speech-end":
                    if cleanup_failure: raise secondary
                    return {}
                if action == "orca-speech-trigger" and failure == "trigger": raise original
                if action == "orca-read":
                    if failure == "read": raise original
                    return {"keyboard": {"grab_all": failure != "grab"}}
                return {}
        def keys(*args):
            if failure == "keys": raise original
        with tempfile.TemporaryDirectory() as temporary:
            scope = {"agent": Agent(), "qmp": SimpleNamespace(keys=keys),
                     "time": SimpleNamespace(sleep=lambda _: None), "os": os,
                     "json": json, "evidence": temporary}
            exec(compile(ast.Module(body=[node], type_ignores=[]), "actual-native-command", "exec"), scope)
            with self.assertRaises(Exception) as raised:
                scope["native_command"]("controlled", "controlled-test")
            if failure is None: self.assertIs(raised.exception, secondary)
            elif failure != "grab": self.assertIs(raised.exception, original)
            else: self.assertIsInstance(raised.exception, RuntimeError)
            self.assertEqual(calls.count("orca-speech-end"), 0 if failure == "start" else 1)
            receipt = Path(temporary, "controlled-failure.json")
            if failure == "start": self.assertFalse(receipt.exists())
            else:
                self.assertEqual(json.loads(receipt.read_text()), {
                    "primary_error_type": "RuntimeError" if failure is not None else None,
                    "cleanup_error_type": "ValueError" if cleanup_failure else None})
                self.assertNotIn("controlled original", receipt.read_text())
    def test_actual_midcommand_failures_close_original_recording_once(self):
        for failure in ("trigger", "keys", "read", "grab"):
            with self.subTest(failure=failure): self.exercise(failure)
    def test_actual_cleanup_failure_preserves_original_command_error(self):
        self.exercise("keys", cleanup_failure=True)
    def test_actual_cleanup_only_failure_remains_failed(self):
        self.exercise(None, cleanup_failure=True)
    def test_actual_start_failure_uses_existing_start_cleanup_only(self):
        self.exercise("start")


class OrcaFixturePackages(unittest.TestCase):
    def test_actual_upstream_export_create_requires_explicit_command(self):
        source = Path(__file__).resolve().parents[2] / ".github/workflows/ci.yml"
        lines = [line.strip() for line in source.read_text().splitlines()
                 if "probe_container=$(sudo podman create" in line]
        self.assertEqual(len(lines), 1)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sudo = root / "sudo"
            sudo.write_text('#!/bin/sh\nexec "$@"\n')
            sudo.chmod(0o755)
            podman = root / "podman"
            podman.write_text("#!/usr/bin/env python3\nimport sys\n"
                "assert sys.argv[1:] == ['create','localhost/marlin-tuna-vm:ci','/usr/bin/true']\n"
                "print('controlled-inspect-container')\n")
            podman.chmod(0o755)
            environment = {**os.environ, "PATH": str(root) + os.pathsep + os.environ["PATH"]}
            for line, success in [(lines[0], True), (lines[0].replace(" /usr/bin/true", ""), False)]:
                result = subprocess.run(["bash", "-eu", "-c", line + '\nprintf "%s\\n" "$probe_container"\n'],
                    env=environment, capture_output=True, text=True, timeout=10)
                if success:
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertEqual(result.stdout.strip(), "controlled-inspect-container")
                else:
                    self.assertNotEqual(result.returncode, 0)
        # Controlled shell admission only; no Podman image/runtime proof.

    def test_actual_fixture_session_fixes_inherited_locale_before_child_exec(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-session"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            command = root / "systemd-cat"
            command.write_text("#!/usr/bin/env python3\nimport json,os\nprint(json.dumps({k:os.environ[k] for k in ['LC_ALL','LANG','LANGUAGE','XDG_SESSION_TYPE']}))\n")
            command.chmod(0o755)
            result = subprocess.run(["bash", str(source)], env={**os.environ,
                "PATH": str(root) + os.pathsep + os.environ["PATH"], "XDG_RUNTIME_DIR": str(root),
                "LC_ALL": "C", "LANG": "fr_FR.UTF-8", "LANGUAGE": "fr"},
                capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout), {"LC_ALL": "C.UTF-8", "LANG": "C.UTF-8",
                "LANGUAGE": "C", "XDG_SESSION_TYPE": "wayland"})
            self.assertTrue((root / "tuna-vm-orca-token").is_file())
        # Fixture setup policy only, not a genuine reader or translated-label proof.

    def run_builder_admission(self, upgraded="2.58.9", failed=False):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/orca51/build"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / "bin"
            binary.mkdir()
            stub = """#!/usr/bin/env python3
import json,os,pathlib,sys
name=pathlib.Path(sys.argv[0]).name
args=sys.argv[1:]
with open(os.environ['CALLS'],'a') as stream: stream.write(json.dumps([name,*args])+'\\n')
if name=='pacman':
 if args and args[0]=='-Syu':
  if os.environ['FAIL_UPGRADE']=='true': sys.exit(7)
  pathlib.Path(os.environ['UPGRADED']).touch()
 elif args==['-Q','at-spi2-core']:
  print('at-spi2-core '+(os.environ['NEW_ATSPI'] if pathlib.Path(os.environ['UPGRADED']).exists() else '2.58.5'))
 elif args==['-Q','orca']: print('orca 50.3')
elif name=='vercmp':
 assert args==[os.environ['NEW_ATSPI'],'2.58.6']
 print(-1 if tuple(map(int,args[0].split('.'))) < (2,58,6) else 0)
"""
            for name in ("pacman", "useradd", "install", "chown", "runuser", "cp", "vercmp"):
                command = binary / name
                command.write_text(stub)
                command.chmod(0o755)
            # Execute the unchanged production admission/decision block. Only its
            # fixed artifact root is relocated; every mutating command is a stub.
            body = source.read_text().split("# A sufficiently new", 1)[0]
            body = body.replace("/orca51-artifacts", str(root / "artifacts"))
            result = subprocess.run(["bash", "-c", body], env={**os.environ,
                "PATH": str(binary) + os.pathsep + os.environ["PATH"], "CALLS": str(root / "calls"),
                "UPGRADED": str(root / "upgraded"), "NEW_ATSPI": upgraded,
                "FAIL_UPGRADE": "true" if failed else "false"}, capture_output=True, text=True, timeout=10)
            calls = [json.loads(line) for line in (root / "calls").read_text().splitlines()]
            receipts = {p.name: p.read_text().strip() for p in (root / "artifacts").iterdir()}
            return result, calls, receipts

    def test_builder_decides_atspi_after_coherent_upgrade_and_never_downgrades(self):
        result, calls, receipts = self.run_builder_admission()
        self.assertEqual(result.returncode, 0, result.stderr)
        upgrades = [call for call in calls if call[:2] == ["pacman", "-Syu"]]
        self.assertEqual(len(upgrades), 1)
        self.assertFalse(any(call[:2] == ["pacman", "-Sy"] for call in calls))
        self.assertEqual(receipts["base-atspi.txt"], "at-spi2-core 2.58.5")
        self.assertEqual(receipts["coherent-atspi.txt"], "at-spi2-core 2.58.9")
        self.assertIn(["vercmp", "2.58.9", "2.58.6"], calls)
        self.assertFalse(any(call[0] == "runuser" for call in calls))
        self.assertFalse(any(call[:2] == ["pacman", "-U"] for call in calls))
        result, calls, _ = self.run_builder_admission("2.58.5")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(any(call[0] == "runuser" for call in calls))
        self.assertTrue(any(call[:2] == ["pacman", "-U"] for call in calls))

    def test_failed_coherent_upgrade_stops_before_fresh_decision_or_package_build(self):
        result, calls, receipts = self.run_builder_admission(failed=True)
        self.assertEqual(result.returncode, 7)
        self.assertEqual(sum(call == ["pacman", "-Q", "at-spi2-core"] for call in calls), 1)
        self.assertNotIn("coherent-atspi.txt", receipts)
        self.assertFalse(any(call[0] in ("useradd", "runuser", "vercmp") for call in calls))

    def run_runtime_admission(self, upgraded="2.60.0", failed=False, query_failed=False):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/orca51/Containerfile"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            binary = root / "bin"
            binary.mkdir()
            stub = """#!/usr/bin/env python3
import json,os,pathlib,sys
name=pathlib.Path(sys.argv[0]).name
args=sys.argv[1:]
with open(os.environ['CALLS'],'a') as stream: stream.write(json.dumps([name,*args])+'\\n')
if name=='pacman':
 if args and args[0]=='-Syu':
  if os.environ['FAIL_UPGRADE']=='true': sys.exit(7)
  pathlib.Path(os.environ['UPGRADED']).touch()
 elif args==['-Q','at-spi2-core']:
  if os.environ.get('FAIL_QUERY')=='true': sys.exit(9)
  print('at-spi2-core '+(os.environ['NEW_ATSPI'] if pathlib.Path(os.environ['UPGRADED']).exists() else '2.58.5'))
 elif args==['-Q','orca']: print('orca 50.3')
elif name=='vercmp':
 assert args==[os.environ['NEW_ATSPI'],'2.58.6']
 print(-1 if tuple(map(int,args[0].split('.'))) < (2,58,6) else 0)
"""
            for name in ("pacman", "vercmp", "tuna-image-kernels"):
                command = binary / name
                command.write_text(stub)
                command.chmod(0o755)
            # Execute the actual final-stage transaction through kernel validation.
            # Mutators are stubs; fixed paths are only inert package arguments.
            body = source.read_text().split("RUN set -eu;", 1)[1].split("&& pacman -Qi", 1)[0]
            body = body.replace("\\\n", " ").strip()
            body = body.replace("/usr/libexec/tuna-image-kernels", str(binary / "tuna-image-kernels"))
            result = subprocess.run(["bash", "-euo", "pipefail", "-c", body], env={**os.environ,
                "PATH": str(binary) + os.pathsep + os.environ["PATH"], "CALLS": str(root / "calls"),
                "UPGRADED": str(root / "upgraded"), "NEW_ATSPI": upgraded,
                "FAIL_UPGRADE": "true" if failed else "false", "FAIL_QUERY": "true" if query_failed else "false"}, capture_output=True, text=True, timeout=10)
            calls = [json.loads(line) for line in (root / "calls").read_text().splitlines()]
            return result, calls

    def test_actual_runtime_transaction_does_not_downgrade_sufficient_atspi(self):
        result, calls = self.run_runtime_admission()
        self.assertEqual(result.returncode, 0, result.stderr)
        install = next(call for call in calls if call[:2] == ["pacman", "-U"])
        self.assertEqual(len(install), 4)
        self.assertTrue(install[-1].endswith("/orca-*.pkg.tar.zst"))
        self.assertEqual(calls[-1], ["tuna-image-kernels", "vm"])
        result, calls = self.run_runtime_admission("2.58.5")
        self.assertEqual(result.returncode, 0, result.stderr)
        install = next(call for call in calls if call[:2] == ["pacman", "-U"])
        self.assertEqual(len(install), 5)
        self.assertTrue(install[-1].endswith("/at-spi2-core-*.pkg.tar.zst"))
        result, calls = self.run_runtime_admission(failed=True)
        self.assertEqual(result.returncode, 7)
        self.assertFalse(any(call[:2] == ["pacman", "-U"] or call[0] == "tuna-image-kernels" for call in calls))
        result, calls = self.run_runtime_admission(query_failed=True)
        self.assertEqual(result.returncode, 9)
        self.assertFalse(any(call[:2] == ["pacman", "-U"] or call[0] == "tuna-image-kernels" for call in calls))
        # Source execution with stubs, not an actual dependency/kernel qualification.

    def test_separate_runtime_upgrade_and_package_install_precede_kernel_validation(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/orca51/Containerfile"
        self.assertIn('SHELL ["/bin/bash", "-o", "pipefail", "-c"]', source.read_text())
        body = source.read_text().split("RUN set -eu; pacman -Syu", 1)[1]
        self.assertLess(body.index("pacman -U"), body.index("tuna-image-kernels vm"))
        self.assertLess(body.index("tuna-image-kernels vm"), body.index("pacman -Qi orca"))
        helper = (source.parents[2] / "tuna-image-kernels").read_text()
        self.assertIn('case "$stage" in preview|vm)', helper)
        self.assertNotIn("pacman -Sy --", source.read_text())
        # Source ordering only; actual package graph/kernel proof remains CI/VM.


class OrcaStagedDiagnostic(unittest.TestCase):
    def agent_scope(self, *names):
        source = Path(__file__).resolve().parent / "vm_guest_agent.py"
        tree = ast.parse(source.read_text())
        nodes = [n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name in set(names)]
        consts = [n for n in tree.body if isinstance(n, ast.Assign)
                  and any(isinstance(t, ast.Name) and t.id == "FOCUS_STAGES" for t in n.targets)]
        scope = {"json": json, "base64": base64, "binascii": __import__("binascii")}
        exec(compile(ast.Module(body=nodes + consts, type_ignores=[]), str(source), "exec"), scope)
        return scope
    def guest_scope(self, *names):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        tree = ast.parse(source.read_text())
        nodes = [n for n in tree.body if (isinstance(n, ast.FunctionDef) and n.name in set(names))
                 or (isinstance(n, ast.Assign) and any(isinstance(t, ast.Name)
                     and t.id in {"FOCUS_ERROR_TYPES", "FOCUS_STAGES"} for t in n.targets))]
        scope = {"json": json, "subprocess": subprocess}
        exec(compile(ast.Module(body=nodes, type_ignores=[]), str(source), "exec"), scope)
        return scope
    def test_focus_failure_schema_accepts_fixed_stage_and_legacy_shape(self):
        scope = self.agent_scope("valid_focus_error", "focus_failure_diagnostic")
        decode = scope["focus_failure_diagnostic"]
        def encode(value): return base64.b64encode(json.dumps(value).encode()).decode()
        staged = {"focus_error_type": "RuntimeError", "focus_stage": "reader-principal"}
        self.assertEqual(decode(encode(staged)), staged)
        legacy = {"focus_error_type": "KeyError"}
        self.assertEqual(decode(encode(legacy)), legacy)
        for bad in [dict(staged, focus_stage="private-zone"), dict(staged, focus_stage="helper ",
                    focus_error_type="private"), dict(staged, argv="private"),
                    {"focus_stage": "helper"}, {"focus_error_type": "OtherError", "focus_stage": "helper"},
                    dict(staged, focus_stage=["helper"])]:
            self.assertIsNone(decode(encode(bad)))
        self.assertIsNone(decode("not-base64"))
        # Fault injection verifies schema bounds; no live guest round trip.
    def test_native_focus_failures_preserve_only_fixed_types_across_guest_and_host(self):
        from contextlib import redirect_stdout
        decode = self.agent_scope("valid_focus_error", "focus_failure_diagnostic")["focus_failure_diagnostic"]
        for name in ("TypeError", "AttributeError", "CalledProcessError", "TimeoutExpired", "Error"):
            with self.subTest(error_type=name):
                scope = self.guest_scope("shell_navigation_focus")
                scope["orca_observation"] = lambda: {"ready": True, "state": "active", "display": ["wayland-0"]}
                scope["STATE"] = SimpleNamespace(read_text=lambda: '{"locked": false}')
                scope["inventory"] = lambda: {"processes": [{"executable": "tuna-shell-gtk", "uid": 1000,
                                                             "pid": 1083, "start_ticks": 1187}]}
                scope["OWNER"] = SimpleNamespace(pw_uid=1000)
                def helper(*args, **kwargs):
                    raise subprocess.CalledProcessError(1, args,
                        output=json.dumps({"focus_error_type": name}), stderr="private query and argument")
                scope["call"] = helper
                stream = io.StringIO()
                with redirect_stdout(stream), self.assertRaises(SystemExit) as exited:
                    scope["shell_navigation_focus"]("shell", 1083, 1187, controlled=True)
                self.assertEqual(exited.exception.code, 1)
                encoded = base64.b64encode(stream.getvalue().encode()).decode()
                self.assertEqual(decode(encoded), {"focus_error_type": name, "focus_stage": "helper"})
                self.assertNotIn("private", stream.getvalue())
                # The same errors before/after the helper remain typed failures.
                error_type = type(name, (Exception,), {})
                def failed_principal():
                    raise error_type("private reader observation")
                scope["orca_observation"] = failed_principal
                stream = io.StringIO()
                with redirect_stdout(stream), self.assertRaises(SystemExit) as exited:
                    scope["shell_navigation_focus"]("shell", controlled=True)
                self.assertEqual(exited.exception.code, 1)
                encoded = base64.b64encode(stream.getvalue().encode()).decode()
                self.assertEqual(decode(encoded), {"focus_error_type": name, "focus_stage": "reader-principal"})
                self.assertNotIn("private", stream.getvalue())
        # Injected failure transport only. Genuine AT-SPI navigation still needs CI.

    def test_speech_failure_schema_accepts_observation_stage(self):
        scope = self.agent_scope("speech_failure_diagnostic")
        decode = scope["speech_failure_diagnostic"]
        cleanup = {"unit_departed": True, "presenter_closed": True, "errors": []}
        def encode(value): return base64.b64encode(json.dumps({"orca_speech_error": value}).encode()).decode()
        staged = {"exception_type": "CalledProcessError", "cleanup": cleanup,
                  "returncode": 1, "stage": "selection"}
        self.assertEqual(decode(encode(staged)), staged)
        unstaged = {"exception_type": "RuntimeError", "cleanup": cleanup}
        self.assertEqual(decode(encode(unstaged)), unstaged)
        for bad in [dict(staged, stage="private-probe"), dict(staged, stage="selection ",
                    returncode=256), dict(staged, argv="private"), dict(staged, stage=["selection"])]:
            self.assertIsNone(decode(encode(bad)))
        # Fault injection verifies schema bounds; no live audio capture.
    def test_focus_reader_principal_failure_names_stage(self):
        from contextlib import redirect_stdout
        scope = self.guest_scope("shell_navigation_focus")
        def unreadable(): raise RuntimeError("original genuine Orca 51 is required")
        scope["orca_observation"] = unreadable
        stream = io.StringIO()
        with redirect_stdout(stream):
            with self.assertRaises(SystemExit) as exited:
                scope["shell_navigation_focus"]("shell", controlled=True)
        self.assertEqual(exited.exception.code, 1)
        self.assertEqual(json.loads(stream.getvalue()),
                         {"focus_error_type": "RuntimeError", "focus_stage": "reader-principal"})
        # Fault injection verifies the stage envelope; no genuine reader observation.
    def test_focus_helper_failure_names_helper_stage(self):
        from contextlib import redirect_stdout
        scope = self.guest_scope("shell_navigation_focus")
        scope["orca_observation"] = lambda: {"ready": True, "state": "active", "display": ["wayland-0"]}
        scope["STATE"] = SimpleNamespace(read_text=lambda: '{"locked": false}')
        scope["inventory"] = lambda: {"processes": [{"executable": "tuna-shell-gtk", "uid": 1000,
                                                     "pid": 1083, "start_ticks": 1187}]}
        scope["OWNER"] = SimpleNamespace(pw_uid=1000)
        def helper(*args, **kwargs):
            raise subprocess.CalledProcessError(1, args,
                output=json.dumps({"focus_error_type": "RuntimeError"}))
        scope["call"] = helper
        stream = io.StringIO()
        with redirect_stdout(stream):
            with self.assertRaises(SystemExit) as exited:
                scope["shell_navigation_focus"]("shell", 1083, 1187, controlled=True)
        self.assertEqual(exited.exception.code, 1)
        self.assertEqual(json.loads(stream.getvalue()),
                         {"focus_error_type": "RuntimeError", "focus_stage": "helper"})
        # Fault injection verifies the helper stage; no genuine shell navigation.
    def test_speech_end_failure_carries_observation_stage(self):
        from contextlib import redirect_stdout
        scope = self.guest_scope("fail_speech", "speech_error_type")
        cleanup = {"unit_departed": True, "presenter_closed": True, "errors": []}
        stream = io.StringIO()
        with redirect_stdout(stream):
            with self.assertRaises(SystemExit):
                scope["fail_speech"](RuntimeError("original observation failed"), cleanup, "selection")
        diagnostic = json.loads(stream.getvalue())["orca_speech_error"]
        self.assertEqual((diagnostic["exception_type"], diagnostic["stage"]), ("RuntimeError", "selection"))
        with self.assertRaises(ValueError):
            scope["fail_speech"](RuntimeError("x"), cleanup, "private-probe")
        # Fault injection verifies the stage envelope; no genuine recording lifecycle.


class ShellStartupDiagnostics(unittest.TestCase):
    def test_transport_failure_is_retained_and_closed(self):
        agent = unittest.mock.Mock()
        agent.run.side_effect = TimeoutError("bounded capture expired")
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / "stack.json"
            with patch.object(lane.guest_agent, "GuestAgent", return_value=agent):
                lane.dump_shell_stack("socket", destination)
            self.assertEqual(json.loads(destination.read_text()),
                             {"error": "bounded capture expired"})
            agent.run.assert_called_once_with("shell-stack", timeout=120)
            agent.close.assert_called_once()

    def test_backtraces_survive_transport(self):
        agent = unittest.mock.Mock()
        observed = {"shells": [{"pid": 1234, "threads": [], "backtraces": "stack"}]}
        agent.run.return_value = observed
        with tempfile.TemporaryDirectory() as directory:
            destination = Path(directory) / "stack.json"
            with patch.object(lane.guest_agent, "GuestAgent", return_value=agent):
                lane.dump_shell_stack("socket", destination)
            self.assertEqual(json.loads(destination.read_text()), observed)
            agent.close.assert_called_once()

    def probe(self, gdb_error=None):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        function = next(node for node in ast.parse(source.read_text()).body
                        if isinstance(node, ast.FunctionDef) and node.name == "shell_stack")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for pid, executable in ((1234, "tuna-shell-gtk"), (1235, "unrelated")):
                process = root / str(pid)
                process.mkdir()
                (process / "exe").symlink_to("/usr/bin/" + executable)
                task = process / "task" / str(pid)
                task.mkdir(parents=True)
                for name, text in (("comm", executable), ("wchan", "futex_wait"),
                                   ("stack", "kernel wait"), ("syscall", "202"),
                                   ("stat", f"{pid} ({executable}) S")):
                    (task / name).write_text(text)
            run = unittest.mock.Mock(return_value=SimpleNamespace(stdout="#0 futex_wait", stderr=""))
            if gdb_error:
                run.side_effect = gdb_error
            scope = {"pathlib": SimpleNamespace(Path=lambda path: root if path == "/proc" else Path(path)),
                     "os": os,
                     "OWNER": SimpleNamespace(pw_uid=os.getuid()), "SHELL_GTK": "tuna-shell-gtk",
                     "subprocess": SimpleNamespace(run=run, TimeoutExpired=lane.subprocess.TimeoutExpired)}
            exec(compile(ast.Module(body=[function], type_ignores=[]), str(source), "exec"), scope)
            result = scope["shell_stack"]()
            run.assert_called_once()
            command = run.call_args.args[0]
            self.assertEqual(command[-2:], ["-p", "1234"])
            self.assertIn("thread apply all bt", command)
            self.assertIn("set print frame-arguments none", command)
            self.assertEqual(run.call_args.kwargs["timeout"], 90)
            return result["shells"][0]

    def test_only_session_shell_is_attached_and_every_thread_is_retained(self):
        result = self.probe()
        self.assertEqual(result["pid"], 1234)
        self.assertEqual(result["threads"][0]["wait_site"], "futex_wait")
        self.assertEqual(result["threads"][0]["state"], "S")
        self.assertEqual(result["backtraces"], "#0 futex_wait")

    def test_gdb_failure_preserves_kernel_wait_information(self):
        result = self.probe(lane.subprocess.TimeoutExpired("gdb", 90))
        self.assertEqual(result["threads"][0]["wait_site"], "futex_wait")
        self.assertEqual(result["backtraces"], "unavailable: TimeoutExpired")


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
                     "call": lambda *args: package, **guest_identity()}
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

    def test_strip_stop_failure_record_reaches_the_lane_error(self):
        actor = lane.guest_agent.GuestAgent.__new__(lane.guest_agent.GuestAgent)
        record = {"exception_type": "FileNotFoundError", "errno": 2, "stage": "launcher",
                  "message": "[Errno 2] No such file or directory: '/proc/11088/exe'"}
        failed = {"exited": True, "exitcode": 1,
                  "out-data": base64.b64encode(json.dumps({"strip_stop_error": record}).encode()).decode(),
                  "err-data": base64.b64encode(b"private arbitrary traceback").decode()}
        with patch.object(actor, "command", side_effect=[{"pid": 10}, failed]):
            with self.assertRaises(RuntimeError) as caught:
                actor.run("strip-stop", "a" * 32)
        message = str(caught.exception)
        self.assertIn("guest lifecycle strip-stop failed (exit=1): ", message)
        self.assertIn('"stage": "launcher"', message)
        self.assertNotIn("private arbitrary traceback", message)
        for invalid in ({**record, "stage": "other"}, {**record, "message": "x" * 513},
                        {**record, "message": "a\x00b"}, {**record, "exception_type": "SystemExit"},
                        {**record, "extra": 1}, {"exception_type": "RuntimeError"}):
            output = base64.b64encode(json.dumps({"strip_stop_error": invalid}).encode()).decode()
            with patch.object(actor, "command", side_effect=[{"pid": 10}, {**failed, "out-data": output}]):
                with self.assertRaises(RuntimeError) as caught:
                    actor.run("strip-stop", "a" * 32)
            self.assertEqual(str(caught.exception), "guest lifecycle strip-stop failed (exit=1)")


class OrcaObservationRetry(unittest.TestCase):
    def test_transient_guest_read_failure_is_retried_until_state_arrives(self):
        actual = {"enabled": True, "ready": True, "state": "active", "pid": 57, "owner": ":1.7",
                  "package": "orca 51.0-1"}
        agent = SimpleNamespace(run=unittest.mock.Mock(side_effect=[RuntimeError("bus not ready"), actual]))
        observations = []
        with patch.object(lane.time, "monotonic", side_effect=[0, 0, 1]), patch.object(lane.time, "sleep"):
            self.assertIs(lane.wait_for_orca_state(agent, True, observations), actual)
        self.assertEqual(agent.run.call_count, 2)
        self.assertEqual(observations, [actual])

    def test_missing_orca_package_fails_immediately(self):
        missing = {"enabled": False, "ready": False, "state": "disabled", "pid": 0, "owner": None,
                   "package": None}
        agent = SimpleNamespace(run=unittest.mock.Mock(return_value=missing))
        with patch.object(lane.time, "monotonic", return_value=0), patch.object(lane.time, "sleep"):
            with self.assertRaisesRegex(RuntimeError, "ships no distribution Orca package"):
                lane.wait_for_orca_state(agent, False, [])
        self.assertEqual(agent.run.call_count, 1)

    def test_repeated_guest_read_failure_still_times_out(self):
        agent = SimpleNamespace(run=unittest.mock.Mock(side_effect=RuntimeError("bus not ready")))
        observations = []
        with patch.object(lane.time, "monotonic", side_effect=[0, 0, 21]), patch.object(lane.time, "sleep"):
            with self.assertRaisesRegex(RuntimeError, "last observation error: RuntimeError"):
                lane.wait_for_orca_state(agent, True, observations)
        self.assertEqual(observations, [])


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
                "pointer_position", "native_relative_motion_count", "input_devices")}))
            empty_proc = Path(directory, "proc")
            empty_proc.mkdir()
            scope = {"json": json, "STATE": state, "call": call, "os": lane.os,
                     "OWNER": SimpleNamespace(pw_uid=1000),
                     "pathlib": SimpleNamespace(Path=lambda path: empty_proc),
                     "classify_pam_helpers": lambda _: (None, []), "pam_process_snapshot": lambda: []}
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
class PublicOrcaNavigation(unittest.TestCase):
    def focus_module(self):
        file = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-shell-focus"
        loader = importlib.machinery.SourceFileLoader("public_focus_policy", str(file))
        spec = importlib.util.spec_from_loader(loader.name, loader)
        module = importlib.util.module_from_spec(spec)
        loader.exec_module(module)
        return module

    def test_focus_receipt_discards_private_names_roles_and_hidden_controls(self):
        module = self.focus_module()
        self.assertEqual(module.public_node("Search", "entry", True, True, False),
                         {"label": "Search", "role": "entry", "focused": True, "active": False})
        for name, role, showing in [("private password", "entry", True), ("Search", "password text", True),
                                     ("Search", "entry", False), ("private filename", "frame", True)]:
            self.assertIsNone(module.public_node(name, role, showing, True, True))
        tree = ast.parse(Path(module.__file__).read_text())
        # Default observation still has no text queries. Only the separately
        # admitted controlled-query helper may compare its fixed five characters.
        queries = {n.func.attr for fn in tree.body if isinstance(fn,ast.FunctionDef) and fn.name != "controlled_query"
                   for n in ast.walk(fn) if isinstance(n, ast.Call) and isinstance(n.func, ast.Attribute)}
        self.assertFalse(queries & {"get_text", "get_description", "get_value", "get_editable_text", "get_action", "do_action", "grab_focus"})
        observer = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == "observe")
        hidden = next(n for n in ast.walk(observer) if isinstance(n, ast.If) and ast.dump(n.test) == ast.dump(ast.parse("depth and not showing", mode="eval").body))
        self.assertIsInstance(hidden.body[0], ast.Continue)

    def test_locked_guard_rejects_before_any_accessibility_tree_query(self):
        module = self.focus_module()
        tree = ast.parse(Path(module.__file__).read_text())
        observe = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == "observe")
        guard = next(n for n in observe.body if isinstance(n, ast.If)
                     and isinstance(n.test, ast.Subscript) and isinstance(n.test.slice, ast.Constant)
                     and n.test.slice.value == "locked")
        scope = {"json": json, "state": SimpleNamespace(read_text=lambda: '{"locked": true}')}
        with self.assertRaisesRegex(RuntimeError, "never queried"):
            exec(compile(ast.Module(body=[guard], type_ignores=[]), module.__file__, "exec"), scope)
        guard_index = observe.body.index(guard)
        first_tree = next(i for i,n in enumerate(observe.body) if isinstance(n, ast.Assign)
                          and isinstance(n.value, ast.Call) and isinstance(n.value.func, ast.Name)
                          and n.value.func.id == "accessibility_query"
                          and isinstance(n.value.args[0], ast.Attribute) and n.value.args[0].attr == "get_desktop")
        self.assertLess(guard_index, first_tree)
        # Guard/query ordering policy only, never a genuine reader/privacy qualification.

    def test_all_principal_queries_share_lock_guard_and_dbus_deadline(self):
        module = self.focus_module()
        tree = ast.parse(Path(module.__file__).read_text())
        query = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == "accessibility_query")
        calls = []
        def remaining():
            calls.append("guard")
            raise RuntimeError("locked")
        scope = {"remaining_budget": remaining, "Atspi": SimpleNamespace(set_timeout=lambda *args: None)}
        exec(compile(ast.Module(body=[query], type_ignores=[]), module.__file__, "exec"), scope)
        with self.assertRaisesRegex(RuntimeError, "locked"):
            scope["accessibility_query"](lambda: calls.append("private query"))
        self.assertEqual(calls, ["guard"])
        for n in ast.walk(tree):
            if isinstance(n, ast.Call) and isinstance(n.func, ast.Attribute):
                self.assertNotIn(n.func.attr, {"get_desktop", "get_child_count", "get_child_at_index", "get_process_id"})
        bus = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == "bus_query")
        connection = SimpleNamespace(call_sync=lambda *args: calls.append(args[-2]) or "reply")
        scope = {"remaining_budget": lambda: 75, "Gio": SimpleNamespace(DBusCallFlags=SimpleNamespace(NONE=0))}
        exec(compile(ast.Module(body=[bus], type_ignores=[]), module.__file__, "exec"), scope)
        self.assertEqual(scope["bus_query"](connection, "bus", "path", "iface", "method", None, None), "reply")
        self.assertEqual(calls[-1], 75)
        # Source ordering/shared call budget only; synchronous GI connection setup
        # itself is not claimed to have an eight-second hard completion bound.

    def test_accessibility_peer_credentials_require_original_owner_pid_and_uid(self):
        module = self.focus_module()
        tree = ast.parse(Path(module.__file__).read_text())
        credentials = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == "credentials")
        refusal = next(n for n in credentials.body if isinstance(n, ast.If))
        predicate = compile(ast.Expression(refusal.test), module.__file__, "eval")
        values = {"GetNameOwner": ":1.27", "GetConnectionUnixProcessID": 933, "GetConnectionUnixUser": 1000}
        scope = {"values": values, "owner": ":1.27", "pid": 933, "uid": 1000}
        self.assertFalse(eval(predicate, scope))
        for key, invalid in (("GetNameOwner", ":1.28"), ("GetConnectionUnixProcessID", 934), ("GetConnectionUnixUser", 0)):
            self.assertTrue(eval(predicate, {**scope, "values": {**values, key: invalid}}))

    def test_public_speech_tokens_never_retain_arbitrary_presenter_text(self):
        entries = [{"kind": "speech", "text": "Search entry private trailing text"},
                   {"kind": "speech", "text": "Take Screenshot button"},
                   {"kind": "interrupt"}, {"kind": "speech", "text": "research private"}]
        self.assertEqual(lane.public_speech_tokens(entries), ["search", "take screenshot"])
        self.assertEqual(lane.public_speech_tokens([{ "kind": "speech", "text": "private filename"}]), [])
        with self.assertRaises(RuntimeError):
            lane.public_speech_tokens(["private malformed payload"])

    def test_public_capture_rejects_triggers_outside_original_pcm_window(self):
        file = Path(__file__).resolve().parents[1] / "tuna-vm-lane"
        tree = ast.parse(file.read_text())
        capture = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == "public_capture")
        validator = next(n for n in ast.walk(capture) if isinstance(n, ast.Call)
                         and isinstance(n.func, ast.Name) and n.func.id == "require_public_speech_window")
        self.assertEqual([ast.unparse(arg) for arg in validator.args],
                         ["start", "end", "before_trigger", "after_trigger", "len(samples)"])
        # Retain the original four-second actual-data negatives: larger profile
        # never creates samples, padding, or a renewed start clock.
        start, end, before, after = OrcaPublicWindow().receipts()
        start["receipt"].update(started_monotonic=10.0, ready_monotonic=10.5)
        for first, last in ((10.4, 11.0), (11.0, 10.9), (13.9, 14.0), (14.1, 14.2)):
            before["guest_monotonic"] = first; after["guest_monotonic"] = last
            with self.subTest(first=first, last=last), self.assertRaises(RuntimeError):
                lane.require_public_speech_window(start, end, before, after, 96000)
        volume = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == "focus_volume")
        calls = [n for n in ast.walk(volume) if isinstance(n, ast.Call)
                 and isinstance(n.func, ast.Attribute) and n.func.attr == "keys"]
        self.assertEqual([tuple(ast.literal_eval(arg) for arg in n.args) for n in calls], [("tab",)])
        # Clock/source policy only: an actual VM still must qualify the fresh recording.

    def test_navigation_actions_are_physical_and_capture_failure_always_closes(self):
        file = Path(__file__).resolve().parents[1] / "tuna-vm-lane"
        tree = ast.parse(file.read_text())
        capture = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == "public_capture")
        guard = next(n for n in capture.body if isinstance(n, ast.Try) and n.finalbody)
        calls = []
        class Agent:
            def run(self, action):
                calls.append(action)
                return {}
        original = RuntimeError("original public focus failure")
        def action(): raise original
        scope = {"agent": Agent(), "action": action, "primary": None, "cleanup_error": None,"stage":"trigger-before"}
        exec(compile(ast.Module(body=[guard], type_ignores=[]), str(file), "exec"), scope)
        self.assertIs(scope["primary"], original)
        self.assertIn("orca-speech-end", calls)
        for name, chord in (("open_search", ("meta_l",)), ("focus_result", ("tab",)), ("open_quick", ("meta_l", "s"))):
            helper = next(n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef) and n.name == name)
            call = next(n for n in ast.walk(helper) if isinstance(n, ast.Call) and isinstance(n.func, ast.Attribute) and n.func.attr == "keys")
            self.assertEqual(tuple(ast.literal_eval(arg) for arg in call.args), chord)
        # Actual source contracts/fault ordering only; the genuine VM must still qualify navigation.


class ControlledOverviewDiagnostic(unittest.TestCase):
    def module(self):
        path=Path(lane.__file__).parents[1]/"packaging/marlin/vm-lane/tuna-vm-shell-focus"
        loader=importlib.machinery.SourceFileLoader("controlled_search_policy",str(path))
        spec=importlib.util.spec_from_loader(loader.name,loader)
        module=importlib.util.module_from_spec(spec);loader.exec_module(module)
        return module

    def test_actual_controlled_query_requires_unique_entry_and_five_characters(self):
        module=self.module();calls=[]
        class Text:
            def get_character_count(self):calls.append("count");return count
            def get_text(self,start,end):calls.append((start,end));return value
        class Entry:
            def get_text_iface(self):calls.append("interface");return Text()
        entry=Entry()
        query=lambda method,*args:method(*args)
        for entries in ([],[entry,entry]):
            self.assertIsNone(module.controlled_query(entries,query));self.assertEqual(calls,[])
        for count,value,expected in ((0,"private",False),(6,"private",False),(True,"private",False),(5.0,"private",False),(5,"disks",True),(5,"other",False)):
            calls.clear();self.assertIs(module.controlled_query([entry],query),expected)
            self.assertEqual(calls,["interface","count"]+([(0,5)] if type(count) is int and count==5 else []))

    def catalog(self,changes=None,replaced=False):
        module=self.module()
        with tempfile.TemporaryDirectory() as directory:
            path=Path(directory)/"receipt";uid=os.getuid()
            mapped=SimpleNamespace(st_dev=7,st_ino=9,st_uid=0,st_mode=0o100444,st_size=200,
                                   st_mtime_ns=2000000003,st_ctime_ns=4000000005)
            row={"schema":1,"pid":34,"uid":uid,"start_ticks":57,"executed_key":list(module.executable_key(mapped)),
                 "sequence":1,"sampled_monotonic_ns":100000000000,"known_query_matches":True,
                 "expected_catalog_count":1,"expected_label_matches":True,"expected_ranked_count":1,
                 "rendered_target_count":1,"search_window_visible":True,"results_container_visible":True,"controlled_source_provenance":True,"authentication_boundary":False}
            row.update(changes or {});path.write_text(json.dumps(row));path.chmod(0o600)
            old_stat=os.fstat;count=0
            def fstat(fd):
                nonlocal count
                value=old_stat(fd);count+=1
                if count==2 and replaced:
                    path.rename(path.with_name("original"));path.write_text('{}');path.chmod(0o600)
                return value
            module.pathlib=SimpleNamespace(Path=lambda name:SimpleNamespace(lstat=lambda:mapped)
                              if name=="/usr/bin/tuna-shell-gtk" else path)
            module.os=SimpleNamespace(open=os.open,fdopen=os.fdopen,fstat=fstat,
                stat=lambda name:mapped,readlink=lambda name:"/usr/bin/tuna-shell-gtk",
                O_RDONLY=os.O_RDONLY,O_NOFOLLOW=os.O_NOFOLLOW,O_NONBLOCK=os.O_NONBLOCK)
            module.time=SimpleNamespace(monotonic_ns=lambda:100000000001)
            return module.catalog_receipt(34,uid,57)

    def test_actual_catalog_receipt_keeps_original_fd_and_cached_source_scope(self):
        value=self.catalog();self.assertEqual(value["sample"]["expected_catalog_count"],1)
        self.assertEqual(value["sample_scope"],"original-show-results")
        self.assertEqual(len(value["fd_receipt"]["sha256"]),64)
        self.assertIs(value["authentication_boundary"],False)

    def test_actual_catalog_rejects_private_unknown_fields_changed_principal_and_expiry(self):
        for changes in ({"message":"private"},{"pid":35},{"uid":True},{"start_ticks":58},
                        {"known_query_matches":False},{"expected_catalog_count":True},
                        {"expected_ranked_count":7},{"rendered_target_count":0},
                        {"search_window_visible":1},{"results_container_visible":"private"},
                        {"expected_catalog_count":2},{"executed_key":[1]*9},
                        {"sampled_monotonic_ns":100000000002},{"sampled_monotonic_ns":1},
                        {"authentication_boundary":True},{"controlled_source_provenance":False}):
            with self.subTest(fields=list(changes)),self.assertRaises(ValueError):self.catalog(changes)
        with self.assertRaises(ValueError):self.catalog(replaced=True)

    def test_controlled_shell_replacement_or_lock_refuses_before_observer_rpc(self):
        from contextlib import redirect_stdout
        path=Path(lane.__file__).parents[1]/"packaging/marlin/vm-lane/tuna-vm-lifecycle"
        tree=ast.parse(path.read_text())
        fn=next(n for n in tree.body if isinstance(n,ast.FunctionDef) and n.name=="shell_navigation_focus")
        consts=[n for n in tree.body if isinstance(n,ast.Assign) and any(isinstance(t,ast.Name)
                and t.id in {"FOCUS_ERROR_TYPES","FOCUS_STAGES"} for t in n.targets)]
        calls=[]
        for locked in (True,False):
            scope={"orca_observation":lambda:{"ready":True,"state":"active"},"json":json,
                   "STATE":SimpleNamespace(read_text=lambda:json.dumps({"locked":locked})),
                   "inventory":lambda:{"processes":[{"executable":"tuna-shell-gtk","uid":1000,"pid":35,"start_ticks":58}]},
                   "OWNER":SimpleNamespace(pw_uid=1000),"call":lambda *args,**kw:calls.append(args),
                   "subprocess":subprocess}
            exec(compile(ast.Module(body=[fn]+consts,type_ignores=[]),str(path),"exec"),scope)
            stream=io.StringIO()
            with redirect_stdout(stream):
                with self.assertRaises(SystemExit) as exited:
                    scope["shell_navigation_focus"]("shell",34,57,controlled=True)
            self.assertEqual(exited.exception.code,1)
            self.assertEqual(json.loads(stream.getvalue()),
                             {"focus_error_type":"RuntimeError","focus_stage":"reader-principal"})
            self.assertEqual(calls,[])

    def test_finite_host_summary_discards_private_or_oversized_values(self):
        good={"known_query_matches":True,"Disks_result":{"present":1,"showing":1,"focused":0},"focused_role_classes":["other"]}
        self.assertEqual(lane.safe_navigation_diagnostic(good),good)
        for value in (dict(good,message="private"),dict(good,known_query_matches="private"),
                      dict(good,Disks_result={"present":True,"showing":1,"focused":0}),
                      dict(good,focused_role_classes=["private"]),dict(good,focused_role_classes=["other"]*513)):
            self.assertIsNone(lane.safe_navigation_diagnostic(value))

    def test_actual_capture_finite_stage_preserves_primary_and_owned_end(self):
        tree=ast.parse(Path(lane.__file__).read_text());fn=next(n for n in ast.walk(tree) if isinstance(n,ast.FunctionDef) and n.name=="public_capture")
        calls=[];original=RuntimeError("private focus error")
        class Agent:
            def run(self,name,*args):calls.append(name);return {}
        def action():raise original
        with tempfile.TemporaryDirectory() as out:
            scope={"agent":Agent(),"os":os,"json":json,"evidence":out,"navigation":[],
                   "safe_navigation_diagnostic":lane.safe_navigation_diagnostic}
            exec(compile(ast.Module(body=[fn],type_ignores=[]),"actual-public-capture","exec"),scope)
            with self.assertRaises(RuntimeError) as error:scope["public_capture"]("orca-overview-result","disks",action)
            self.assertIs(error.exception,original)
            self.assertEqual(calls,["orca-speech-start","orca-speech-trigger","orca-speech-end"])
            data=json.loads((Path(out)/"orca-overview-result-failure.json").read_text())
            self.assertEqual(data["primary_stage"],"action-focus");self.assertIsNone(data["cleanup_stage"])
            self.assertNotIn("private",json.dumps(data))


class OrcaLifecycleAuthority(unittest.TestCase):
    def test_presenter_setup_attempt_and_reservation_failures_use_original_cleanup(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        tree = ast.parse(source.read_text())
        main = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "main")
        setup = next(node for node in ast.walk(main) if isinstance(node, ast.Try)
                     and any(isinstance(item, ast.Assign) and isinstance(item.value, ast.Call)
                             and isinstance(item.value.func, ast.Name) and item.value.func.id == "reserve_recording"
                             for item in node.body))
        cleanups, calls = [], []
        primary = lane.subprocess.TimeoutExpired(["busctl", "hidden-token"], 15)
        def rpc(*args, **kwargs):
            calls.append(args)
            raise primary
        def cleanup(recording, token, unit_started, presenter_started):
            cleanups.append((recording["owner"], presenter_started))
            return {"unit_departed": True, "presenter_closed": True, "errors": []}
        def failed(error, receipt): raise error
        scope = {"reserve_recording": lambda: (9, {"ino": 1}), "create_evidence": lambda *args: {"ino": 2},
                 "OWNER": SimpleNamespace(pw_uid=1000, pw_gid=1000), "pcm": "pcm", "log": "log",
                 "actual": {"owner": ":1.724"}, "unit": "original-unit", "selection": {}, "token": "hidden-token",
                 "call": rpc, "cleanup_speech": cleanup, "fail_speech": failed,
                 "unit_started": False, "presenter_started": False, "recorder": None, "reservation": None,
                 "file_identity": lambda info: {"ino": 1},
                 "RECORDING_STATE": SimpleNamespace(lstat=lambda: None, unlink=lambda: None),
                 "os": SimpleNamespace(close=lambda fd: None)}
        with self.assertRaises(lane.subprocess.TimeoutExpired) as error:
            exec(compile(ast.Module(body=[setup], type_ignores=[]), str(source), "exec"), scope)
        self.assertIs(error.exception, primary)
        self.assertEqual(cleanups, [(":1.724", True)])
        self.assertEqual(calls[0][6], "SetLogFileForTesting")
        calls.clear(); cleanups.clear()
        def occupied(): raise FileExistsError()
        scope.update(reserve_recording=occupied, reservation=None, presenter_started=False)
        with self.assertRaises(FileExistsError):
            exec(compile(ast.Module(body=[setup], type_ignores=[]), str(source), "exec"), scope)
        self.assertEqual(calls, [])
        self.assertEqual(cleanups, [(":1.724", False)])
        # Actual source ordering/fault policy only; no genuine Orca RPC qualification.

    def test_speech_failure_exits_without_token_bearing_exception_traceback(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        tree = ast.parse(source.read_text())
        helpers = [node for node in tree.body if isinstance(node, ast.FunctionDef)
                   and node.name in {"speech_error_type", "fail_speech"}]
        scope = {"json": json, "subprocess": lane.subprocess}
        exec(compile(ast.Module(body=helpers, type_ignores=[]), str(source), "exec"), scope)
        error = lane.subprocess.CalledProcessError(1, ["busctl", "hidden-token"], stderr="private-presenter")
        with patch("sys.stdout", new_callable=io.StringIO) as out:
            with self.assertRaises(SystemExit) as exit_error:
                scope["fail_speech"](error, {"unit_departed": True, "presenter_closed": True, "errors": []})
        self.assertEqual(exit_error.exception.code, 1)
        self.assertTrue(exit_error.exception.__suppress_context__)
        output = out.getvalue()
        self.assertNotIn("hidden-token", output)
        self.assertNotIn("private-presenter", output)
        self.assertEqual(json.loads(output)["orca_speech_error"]["returncode"], 1)

    def test_recorder_collection_requires_exact_properties_and_original_departure(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        tree = ast.parse(source.read_text())
        helper = next(node for node in tree.body if isinstance(node, ast.FunctionDef)
                      and node.name == "recorder_unit_departure")
        scope = {}
        exec(compile(ast.Module(body=[helper], type_ignores=[]), str(source), "exec"), scope)
        observe = scope["recorder_unit_departure"]
        loaded = "LoadState=loaded\nActiveState=inactive\nMainPID=0\n"
        collected = "LoadState=not-found\nActiveState=inactive\nMainPID=0\n"
        self.assertEqual(observe(0, 0, loaded, True)["properties"]["LoadState"], "loaded")
        self.assertEqual(observe(5, 4, collected, True)["stop_returncode"], 5)
        for stop, show, output, departed in [
                (0, 0, loaded.replace("inactive", "active"), True),
                (0, 0, loaded.replace("MainPID=0", "MainPID=42"), True),
                (5, 0, loaded, True), (0, 1, loaded, True),
                (5, 1, collected, True), (5, 4, collected, False), (5, 4, collected, None),
                (0, 0, loaded, False), (0, 0, "", True),
                (0, 0, collected + "Private=secret\n", True),
                (0, 0, collected + "MainPID=0\n", True),
                (0, 0, "LoadState=not-found\n", True), (True, 0, loaded, True),
                (0, 0, loaded.replace("loaded", "failed"), True)]:
            with self.subTest(output=output, departed=departed), self.assertRaises(RuntimeError):
                observe(stop, show, output, departed)
        # Synthetic systemd property policy only; actual installed VM output remains mandatory.

    def test_recorder_departure_distinguishes_original_from_recycled_pid(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        tree = ast.parse(source.read_text())
        helper = next(node for node in tree.body if isinstance(node, ast.FunctionDef)
                      and node.name == "original_recorder_departed")
        ticks = 123
        def text(): return "123 (pw-record) " + " ".join(["0"] * 19 + [str(ticks)])
        scope = {}
        class Proc:
            def __truediv__(self, name): return SimpleNamespace(read_text=text)
        scope["pathlib"] = SimpleNamespace(Path=lambda path: Proc())
        exec(compile(ast.Module(body=[helper], type_ignores=[]), str(source), "exec"), scope)
        original = {"pid": 123, "start_ticks": 123, "uid": 1000}
        self.assertFalse(scope["original_recorder_departed"](original))
        ticks = 456
        self.assertTrue(scope["original_recorder_departed"](original))
        def vanished(): raise FileNotFoundError()
        text = vanished
        self.assertTrue(scope["original_recorder_departed"](original))
        # Kernel observation policy only, not a genuine recorder lifetime receipt.

    def test_speech_failure_schema_rejects_private_or_unbounded_fields(self):
        source = Path(__file__).resolve().parent / "vm_guest_agent.py"
        tree = ast.parse(source.read_text())
        helper = next(node for node in tree.body if isinstance(node, ast.FunctionDef)
                      and node.name == "speech_failure_diagnostic")
        scope = {"json": json, "base64": base64, "binascii": __import__("binascii")}
        exec(compile(ast.Module(body=[helper], type_ignores=[]), str(source), "exec"), scope)
        decode = scope["speech_failure_diagnostic"]
        valid = {"exception_type": "RuntimeError", "cleanup": {"unit_departed": True,
                 "presenter_closed": False, "errors": [{"stage": "owned_presenter_close",
                 "exception_type": "CalledProcessError", "returncode": 1}]}}
        def encode(value): return base64.b64encode(json.dumps({"orca_speech_error": value}).encode()).decode()
        self.assertEqual(decode(encode(valid)), valid)
        self.assertEqual(decode(encode({**valid, "returncode": 1}))["returncode"], 1)
        for code in (True, 256, "private"):
            self.assertIsNone(decode(encode({**valid, "returncode": code})))
        for bad in [None, "x" * 5465, base64.b64encode(b"\xff").decode(), "not-base64"]:
            self.assertIsNone(decode(bad))
        import copy
        for field, value in [("stage", "private-token"), ("stderr", "private-presenter-text"),
                             ("returncode", 256), ("returncode", True), ("exception_type", "private-message")]:
            invalid = copy.deepcopy(valid)
            invalid["cleanup"]["errors"][0][field] = value
            self.assertIsNone(decode(encode(invalid)))
        invalid = copy.deepcopy(valid)
        invalid["cleanup"]["errors"] *= 4
        self.assertIsNone(decode(encode(invalid)))
        self.assertIsNone(decode(encode({**valid, "argv": "private"})))

    def test_owned_speech_cleanup_retains_both_failures_without_command_data(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        tree = ast.parse(source.read_text())
        helpers = [node for node in tree.body if isinstance(node, ast.FunctionDef)
                   and node.name in {"speech_error_type", "cleanup_speech"}]
        calls = []
        def failed(*args, **kwargs):
            calls.append(args)
            raise lane.subprocess.CalledProcessError(1, args, stderr="private token/presenter content")
        scope = {"call": failed, "subprocess": lane.subprocess, "stop_recording_unit": lambda recording: failed("systemctl", "--user", "stop", recording["unit"])}
        exec(compile(ast.Module(body=helpers, type_ignores=[]), str(source), "exec"), scope)
        result = scope["cleanup_speech"]({"unit": "original-owned-unit", "owner": ":1.724"}, "hidden-token")
        self.assertEqual([item["stage"] for item in result["errors"]], ["owned_unit_stop", "owned_presenter_close"])
        self.assertFalse(result["unit_departed"])
        self.assertFalse(result["presenter_closed"])
        self.assertEqual(calls[0][3], "original-owned-unit")
        self.assertEqual(calls[1][3], ":1.724")
        self.assertNotIn("private", json.dumps(result))
        self.assertNotIn("hidden-token", json.dumps(result))
        # Fault injection verifies authority and error privacy; no live systemd/presenter proof.

    def test_speech_end_always_cleans_up_before_original_failure_is_raised(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        tree = ast.parse(source.read_text())
        main = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == "main")
        guard = next(node for node in ast.walk(main) if isinstance(node, ast.Try)
                     and any(isinstance(item, ast.Assign) and any(isinstance(t, ast.Name) and t.id == "cleanup"
                         for t in item.targets) for item in node.finalbody))
        cleaned = []
        primary = RuntimeError("original observation failed")
        def selection(*args): raise primary
        scope = {"speech_selection": selection, "orca_observation": lambda: {}, "recording": {}, "token": "hidden",
                 "cleanup_speech": lambda *args: cleaned.append(True) or {"errors": []}, "primary": None,
                 "pathlib": SimpleNamespace(Path=lambda *args: SimpleNamespace(read_text=lambda: "hidden")), "RUNTIME": "/run/test"}
        exec(compile(ast.Module(body=[guard], type_ignores=[]), str(source), "exec"), scope)
        self.assertIs(scope["primary"], primary)
        self.assertEqual(cleaned, [True])
        # Fault injection verifies cleanup ordering; no genuine runtime/audio qualification.

    def test_recording_read_pins_original_nofollow_file_and_bounds(self):
        import os
        import stat
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        tree = ast.parse(source.read_text())
        names = {"file_identity", "create_evidence", "read_evidence"}
        helpers = [node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name in names]
        scope = {"os": os, "stat": stat}
        exec(compile(ast.Module(body=helpers, type_ignores=[]), str(source), "exec"), scope)
        with tempfile.TemporaryDirectory() as root:
            path = Path(root) / "capture"
            identity = scope["create_evidence"](path, os.getuid(), os.getgid())
            self.assertEqual(identity["mode"], 0o600)
            path.write_bytes(b"actual bounded bytes")
            self.assertEqual(scope["read_evidence"](path, identity, 64), b"actual bounded bytes")
            with self.assertRaisesRegex(RuntimeError, "bound"):
                scope["read_evidence"](path, identity, 4)
            with self.assertRaises(FileExistsError):
                scope["create_evidence"](path, os.getuid(), os.getgid())
            path.chmod(0o644)
            with self.assertRaisesRegex(RuntimeError, "identity changed"):
                scope["read_evidence"](path, identity, 64)
            path.chmod(0o600)
            original = path.with_suffix(".original")
            path.rename(original)
            path.write_bytes(b"replacement")
            with self.assertRaisesRegex(RuntimeError, "identity changed"):
                scope["read_evidence"](path, identity, 64)
            path.unlink()
            path.symlink_to(original)
            with self.assertRaises(OSError):
                scope["read_evidence"](path, identity, 64)
        # Actual filesystem authority policy; no claim of live recorder or audio qualification.

    def test_recording_graph_requires_original_sink_serial_and_directional_ports(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        tree = ast.parse(source.read_text())
        helper = next(node for node in tree.body if isinstance(node, ast.FunctionDef)
                      and node.name == "require_sink_graph")
        scope = {}
        exec(compile(ast.Module(body=[helper], type_ignores=[]), str(source), "exec"), scope)
        sink = {"id": 1, "type": "PipeWire:Interface:Node", "props": {"object.serial": 20}}
        synth = {"id": 2, "type": "PipeWire:Interface:Node", "props": {"object.serial": 21}}
        objects = [sink, synth,
                   {"id": 3, "type": "PipeWire:Interface:Port", "info": {"direction": "output"}, "props": {"node.id": 2}},
                   {"id": 4, "type": "PipeWire:Interface:Port", "info": {"direction": "input"}, "props": {"node.id": 1}},
                   {"id": 5, "type": "PipeWire:Interface:Link", "info": {"output-node-id": 2, "input-node-id": 1,
                      "output-port-id": 3, "input-port-id": 4}}]
        scope["require_sink_graph"]({"objects": objects}, sink, synth)
        with self.assertRaisesRegex(RuntimeError, "duplicate"):
            scope["require_sink_graph"]({"objects": objects + [sink]}, sink, synth)
        objects[3]["info"]["direction"] = "output"
        with self.assertRaisesRegex(RuntimeError, "port/direction"):
            scope["require_sink_graph"]({"objects": objects}, sink, synth)
        objects[3]["info"]["direction"] = "input"
        objects[0] = {"id": 1, "type": "PipeWire:Interface:Node", "props": {"object.serial": 99}}
        with self.assertRaisesRegex(RuntimeError, "identity changed"):
            scope["require_sink_graph"]({"objects": objects}, sink, synth)
        # Graph schema policy only, not genuine PipeWire association evidence.

    def test_actual_51_speech_properties_require_original_owner_and_real_module(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        tree = ast.parse(source.read_text())
        helper = next(node for node in tree.body if isinstance(node, ast.FunctionDef)
                      and node.name == "speech_selection")
        reader = {"ready": True, "state": "active", "pid": 7274, "start_ticks": 22069,
                  "owner": ":1.724", "uid": 1000, "bus_pid": 7274, "bus_uid": 1000,
                  "version": ["s", "51.0"], "display": ["tuna-nested-872"],
                  "parent_pid": 872, "compositor_pid": 872, "bus_name": "org.gnome.Orca1.Service"}
        calls = []
        properties = {"CurrentServer": 's "Speech Dispatcher"', "CurrentSynthesizer": 's "espeak-ng"'}
        def call(*args, **kwargs):
            calls.append(args)
            return properties[args[-1]]
        scope = {"call": call, "shlex": __import__("shlex"), "orca_observation": lambda: reader,
                 "time": SimpleNamespace(monotonic=lambda: 1)}
        exec(compile(ast.Module(body=[helper], type_ignores=[]), str(source), "exec"), scope)
        self.assertEqual(scope["speech_selection"](reader)["selection"]["CurrentSynthesizer"], "espeak-ng")
        self.assertTrue(all(args[3] == reader["owner"] for args in calls))
        calls.clear()
        with self.assertRaisesRegex(RuntimeError, "Orca 51"):
            scope["speech_selection"]({**reader, "version": ["s", "50.3"]})
        self.assertEqual(calls, [])
        properties["CurrentSynthesizer"] = 's "dummy"'
        with self.assertRaisesRegex(RuntimeError, "real espeak"):
            scope["speech_selection"](reader)
        properties["CurrentSynthesizer"] = 's "espeak-ng"'
        scope["orca_observation"] = lambda: {**reader, "start_ticks": 22070}
        with self.assertRaisesRegex(RuntimeError, "identity changed"):
            scope["speech_selection"](reader)
        # Property/identity policy only, not actual Orca/SSIP or audio proof.
    def test_setting_mutation_ack_never_observes_vanishing_reader(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        tree = ast.parse(source.read_text())
        helper = next(node for node in tree.body if isinstance(node, ast.FunctionDef)
                      and node.name == "mutate_orca_setting")
        calls = []
        value = "false"
        def call(*args, **kwargs):
            calls.append(args)
            if args[:2] == ("gsettings", "get"):
                return value
            return ""
        def departed_reader():
            raise RuntimeError("original reader bus is already gone")
        scope = {"call": call, "orca_observation": departed_reader}
        exec(compile(ast.Module(body=[helper], type_ignores=[]), str(source), "exec"), scope)
        self.assertEqual(scope["mutate_orca_setting"]("orca-off"),
                         {"requested_enabled": False, "readback_enabled": False})
        self.assertEqual(calls[0][-1], "false")
        value = "true"
        self.assertEqual(scope["mutate_orca_setting"]("orca-on")["readback_enabled"], True)
        self.assertEqual(scope["mutate_orca_setting"]("orca-reset")["requested_enabled"], None)
        value = "false"
        with self.assertRaisesRegex(RuntimeError, "readback disagrees"):
            scope["mutate_orca_setting"]("orca-on")
        value = "malformed"
        with self.assertRaisesRegex(RuntimeError, "readback disagrees"):
            scope["mutate_orca_setting"]("orca-reset")
        # The existing host await remains mandatory; this acknowledgment claims no process departure.

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
            else:
                self.assertNotIn("owner vanished", message)
        malformed = ["!", base64.b64encode(b"\xff").decode(),
                     base64.b64encode(b"[").decode(), base64.b64encode(b"[]").decode(),
                     base64.b64encode(b"x" * 4097).decode(), "A" * 5465, None, ["not text"]]
        for output in malformed:
            with patch.object(actor, "command", side_effect=[{"pid": 10}, {**failed, "out-data": output}]):
                with self.assertRaisesRegex(RuntimeError, r"guest lifecycle orca-read failed \(exit=1\)"):
                    actor.run("orca-read")
        # Transport/unit refusal evidence only; actual bus failure comes from CI.

    def test_negative_control_requires_exact_remote_denial_and_distinct_same_uid(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        tree = ast.parse(source.read_text())
        result = next(node for node in ast.walk(tree) if isinstance(node, ast.Dict)
                      and any(isinstance(key, ast.Constant) and key.value == "denied" for key in node.keys))
        expression = next(value for key, value in zip(result.keys, result.values)
                          if isinstance(key, ast.Constant) and key.value == "denied")
        compiled = compile(ast.Expression(expression), str(source), "eval")
        actor = {"accepted": False, "access_denied": True,
                 "error_name": "org.freedesktop.DBus.Error.AccessDenied",
                 "caller_uid": 1000, "caller_pid": 7200}
        def qualifies(actual):
            return eval(compiled, {"actor": actual, "OWNER": SimpleNamespace(pw_uid=1000),
                                   "before": {"pid": 6874}})
        self.assertTrue(qualifies(actor))
        for invalid in ({**actor, "accepted": True}, {**actor, "access_denied": False},
                        {**actor, "error_name": "org.freedesktop.DBus.Error.NoReply"},
                        {**actor, "error_name": None}, {**actor, "caller_uid": 0},
                        {**actor, "caller_pid": 6874}):
            self.assertFalse(qualifies(invalid))
        # Parsing/refusal policy only. The VM still has to obtain the error from
        # the actual compositor, and preserve its genuine reader's authority.

    def test_native_grabs_wait_for_same_actual_reader_and_retain_samples(self):
        reader = {"pid": 6914, "start_ticks": 21798, "owner": ":1.685",
                  "uid": 1000, "bus_pid": 6914, "bus_uid": 1000,
                  "version": ["s", "50.3"], "display": ["tuna-nested-840"],
                  "parent_pid": 840, "compositor_pid": 840,
                  "bus_name": "org.gnome.Orca.Service", "ready": True, "state": "active",
                  "keyboard": {"owner": ":1.685", "reader_pid": 6914, "watch": True,
                               "modifier_count": 0, "keystroke_count": 0}}
        pending = {**reader, "keyboard": dict(reader["keyboard"])}
        ready = {**reader, "keyboard": {**reader["keyboard"], "modifier_count": 2,
                                       "keystroke_count": 464}}
        observations = [reader]
        with patch.object(lane.time, "monotonic", return_value=0), patch.object(lane.time, "sleep"):
            samples = iter([pending, ready])
            self.assertIs(lane.await_orca_keyboard(reader, lambda: next(samples), observations), ready)
        self.assertEqual(observations, [reader, pending, ready])
        with patch.object(lane.time, "monotonic", return_value=0):
            with self.assertRaisesRegex(RuntimeError, "before deadline"):
                lane.await_orca_keyboard(reader, lambda: self.fail("expired wait read"), [], timeout=0)
        for replacement in ({**ready, "pid": 6915}, {**ready, "start_ticks": 21799},
                            {**ready, "owner": ":1.686"},
                            {**ready, "keyboard": {**ready["keyboard"], "owner": ":1.686"}},
                            {**ready, "keyboard": {**ready["keyboard"], "owner": None}}):
            observations = [reader]
            with patch.object(lane.time, "monotonic", return_value=0), patch.object(lane.time, "sleep"):
                with self.assertRaisesRegex(RuntimeError, "changed before"):
                    lane.await_orca_keyboard(reader, lambda: replacement, observations)
            self.assertEqual(observations, [reader, replacement])
        # Pure wait policy only: the CI VM must still supply real PID/UID/bus
        # credentials and genuine Orca WatchKeyboard/SetKeyGrabs observations.

    def test_negative_control_consumes_actual_inventory_identity_contract(self):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        tree = ast.parse(source.read_text())
        producer = next(node for node in tree.body
                        if isinstance(node, ast.FunctionDef) and node.name == "inventory")
        record = next(node for node in ast.walk(producer) if isinstance(node, ast.Dict)
                      and any(isinstance(key, ast.Constant) and key.value == "start_ticks"
                              for key in node.keys))
        stat = ["0"] * 20
        stat[19] = "18000"
        actual = eval(compile(ast.Expression(record), str(source), "eval"), {
            "path": SimpleNamespace(name="874"), "uid": 1000,
            "executable": "tuna-compositor", "stat": stat})
        consumer = next(node for node in tree.body
                        if isinstance(node, ast.FunctionDef) and node.name == "owned_compositor")
        scope = {"OWNER": SimpleNamespace(pw_uid=1000)}
        exec(compile(ast.Module(body=[consumer], type_ignores=[]), str(source), "exec"), scope)
        select = scope["owned_compositor"]
        self.assertIs(select([actual, {**actual, "executable": "tuna-shell-gtk"}]), actual)
        for invalid in ([], [{**actual, "uid": 1001}], [actual, {**actual, "pid": 875}]):
            with self.assertRaises(RuntimeError):
                select(invalid)
        # This verifies producer/consumer shape and refusal policy. Only the
        # genuine VM can qualify actual bus credentials and reader exclusion.

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
    def test_already_woken_held_key_prompt_does_not_submit_empty_password(self):
        tree = ast.parse((Path(__file__).resolve().parents[1] / "tuna-vm-lane").read_text())
        helper = next(node for node in ast.walk(tree)
                      if isinstance(node, ast.FunctionDef) and node.name == "pam_unlock")
        events = []
        qmp = SimpleNamespace(keys=lambda *keys: events.append(keys),
                              type_text=lambda text: events.append("fixture typed"))
        scope = {"qmp": qmp, "time": SimpleNamespace(sleep=lambda delay: None),
                 "await_state": lambda predicate, timeout: predicate({"state": {"locked": False}})}
        exec(compile(ast.Module(body=[helper], type_ignores=[]), "<actual-pam-helper>", "exec"), scope)
        scope["pam_unlock"](wake=False)
        self.assertEqual(events, [("ctrl", "a"), "fixture typed", ("ret",)])
        events.clear()
        scope["pam_unlock"]()
        self.assertEqual(events, [("ret",), ("ctrl", "a"), "fixture typed", ("ret",)])
        # Actual prompt/PAM transitions still require the genuine hardware VM.

    def test_actual_orca_commands_use_distinct_qmp_qcodes_for_chords(self):
        tree = ast.parse((Path(__file__).resolve().parents[1] / "tuna-vm-lane").read_text())
        command = next(node for node in ast.walk(tree)
                       if isinstance(node, ast.FunctionDef) and node.name == "native_command")
        chords = [tuple(ast.literal_eval(arg) for arg in node.args)
                  for node in ast.walk(command) if isinstance(node, ast.Call)
                  and isinstance(node.func, ast.Attribute) and node.func.attr == "keys"]
        self.assertIn(("insert", "h"), chords)
        self.assertIn(("shift", "f4"), chords)
        for chord in chords:
            self.assertTrue(set(chord) <= {"insert", "h", "shift", "f4", "esc"})
            qmp = lane.baseline.Qmp.__new__(lane.baseline.Qmp)
            qmp.file = io.StringIO()
            qmp._read = lambda: {"return": {}}
            with patch.object(lane.baseline.time, "sleep"):
                qmp.keys(*chord)
            wire = json.loads(qmp.file.getvalue())
            self.assertEqual(wire["execute"], "send-key")
            self.assertEqual(wire["arguments"]["keys"],
                             [{"type": "qcode", "data": name} for name in chord])

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
                               tour=True, greeter_login=False, require_kvm=False,
                               meta=[], orca_contract="distribution")
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


def tour_frame(fill, boxes=()):
    """A WIDTH x HEIGHT RGB frame: fill, then (x0, y0, x1, y1, rgb) boxes."""
    w = lane.WIDTH
    px = bytearray(bytes(fill) * (w * lane.HEIGHT))
    for x0, y0, x1, y1, rgb in boxes:
        row = bytes(rgb) * (x1 - x0)
        for y in range(y0, y1):
            px[(y * w + x0) * 3:(y * w + x1) * 3] = row
    return bytes(px)


class TourViewPredicates(unittest.TestCase):
    GREY, NAVY, WHITE = (36, 36, 40), (8, 24, 52), (250, 250, 250)
    WINDOWS = (190, 140, 1085, 690, WHITE)
    SWITCHER = [WINDOWS, (358, 307, 922, 492, GREY), (376, 325, 536, 425, WHITE),
                (584, 325, 696, 425, WHITE), (749, 325, 900, 425, WHITE)]

    def views(self, px):
        return [name for name in ("overview_windows_shown", "switcher_shown", "desktop_shown")
                if getattr(lane, name)(px)]

    def test_overview_with_window_previews(self):
        frame = tour_frame(self.GREY, [(180, 108, 1100, 660, self.NAVY), (440, 420, 840, 670, self.WHITE)])
        self.assertEqual(self.views(frame), ["overview_windows_shown"])

    def test_search_and_app_grid_are_not_the_window_overview(self):
        icons = [(x, y, x + 48, y + 48, (200, 60, 40)) for x in range(170, 1110, 125) for y in (270, 395)]
        self.assertEqual(self.views(tour_frame(self.GREY, icons)), [])
        self.assertEqual(self.views(tour_frame(self.GREY)), [])

    def test_switcher_island_over_windows(self):
        self.assertEqual(self.views(tour_frame(self.NAVY, self.SWITCHER)), ["switcher_shown"])

    def test_desktop_and_calendar_are_not_the_switcher(self):
        self.assertEqual(self.views(tour_frame(self.NAVY, [self.WINDOWS])), ["desktop_shown"])
        calendar = tour_frame(self.NAVY, [(263, 38, 1017, 531, self.GREY)])
        self.assertEqual(self.views(calendar), ["desktop_shown"])

    def test_await_still_waits_for_the_view_and_flags_one_that_never_appears(self):
        header = f"P6\n{lane.WIDTH} {lane.HEIGHT}\n255\n".encode()
        probes = [tour_frame(self.NAVY, [self.WINDOWS]), tour_frame(self.NAVY, self.SWITCHER)]
        def command(name, **args):
            path = Path(args["filename"])
            if path.name == "probe.ppm":
                path.write_bytes(header + (probes.pop(0) if len(probes) > 1 else probes[0]))
            else:
                path.write_bytes(b"png pixels")
        with tempfile.TemporaryDirectory() as directory:
            recorder = lane.Recorder(SimpleNamespace(cmd=command), directory, timeout=1)
            self.assertTrue(recorder.await_still("alt-tab", lane.switcher_shown, timeout=5, settle=0))
            self.assertFalse(recorder.await_still("overview-windows", lane.overview_windows_shown,
                                                  timeout=0.05, settle=0))
            self.assertEqual(recorder.stills, {"alt-tab": 0, "overview-windows": 1})
            self.assertEqual(recorder.unconfirmed, ["overview-windows"])
            self.assertEqual(sorted(p.name for p in Path(recorder.dir).iterdir()),
                             ["f000000.png", "f000001.png"])
            recorder.close()


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
        # pam_exec's child calls setsid() before exec: it leads its own group.
        self.delay = dict(self.helper, pid=701, ppid=700, pgrp=701, start_ticks=1800, executable="sleep", exe_inode=78)
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
                   dict(self.observed, delays=[dict(self.delay, pgrp=700)]),
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
                     OWNER=SimpleNamespace(pw_uid=1000), PAM_HELPER_FLAG=b'--internal-pam-conversation',
                     **guest_identity())
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
            # An orphan the kernel reparented (ppid 600 -> 1) is the same
            # process, present rather than absent, never a replacement.
            (root/'700/stat').write_text('700 (controlled) '+ ' '.join(['S','1','700']+['0']*16+['1700']))
            present=scope['auth_retirement_state'](expected)
            self.assertFalse(present['helper_absent']); self.assertTrue(present['owned_group_members'])
            (root/'700/stat').write_text('700 (controlled) '+ ' '.join(['S','600','700']+['0']*16+['1701']))
            with self.assertRaisesRegex(RuntimeError,'PID was replaced'):
                scope['auth_retirement_state'](expected)
            (root/'700/stat').write_text('700 (controlled) '+ ' '.join(['S','1','600']+['0']*16+['1700']))
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


class ShippedSnapshotInventory(unittest.TestCase):
    # Run 37696030597: the graphical PAM login produced a real greetd
    # session, yet every inventory() probe failed because the published
    # tuna-desktop 0.1.0-2 snapshot predates the newer instrumentation keys.
    # Session detection must not hard-require keys the shipped binary
    # never emits.
    SHIPPED_STATE = {"locked": False, "windows": [], "focused": None,
                     "active_workspace": 0, "overview_open": True}

    def run_inventory(self, state, sessions, prefix="tuna"):
        source = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-lifecycle"
        function = next(node for node in ast.parse(source.read_text()).body
                        if isinstance(node, ast.FunctionDef) and node.name == "inventory")
        uid = os.getuid()
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            proc = root / "proc"
            (proc / "1234").mkdir(parents=True)
            identity = guest_identity(prefix)
            (root / identity["COMPOSITOR"]).touch()
            (proc / "1234/exe").symlink_to(root / identity["COMPOSITOR"])
            fields = ["S", "1"] + ["0"] * 17 + ["4242"]
            (proc / "1234/stat").write_text(f"1234 ({identity['COMPOSITOR_COMM']}) " + " ".join(fields))
            state_path = root / "tuna-vm-state.json"
            state_path.write_text(json.dumps(state))
            descriptions = {
                "1": "Id=1\nVTNr=1\nUser=959\nType=tty\nService=greetd\nClass=greeter",
                "3": (f"Id=3\nVTNr=1\nUser={uid}\nType=tty\n"
                      "Service=greetd\nClass=user"),
                "4": (f"Id=4\nVTNr=\nUser={uid}\nType=unspecified\n"
                      "Service=systemd-user\nClass=manager"),
            }
            def fake_call(*args, **kwargs):
                if args[:2] == ("loginctl", "list-sessions"):
                    return "".join(f"{sid} {line}\n" for sid, line in sessions)
                if args[:2] == ("loginctl", "show-session"):
                    return descriptions[args[2]]
                raise AssertionError(f"unexpected probe: {args}")
            import pathlib as real_pathlib
            scope = {"json": json, "os": os,
                     "pathlib": SimpleNamespace(
                         Path=lambda path: proc if str(path) == "/proc" else real_pathlib.Path(path)),
                     "OWNER": SimpleNamespace(pw_uid=uid, pw_name="tuna-test"),
                     "RUNTIME": f"/run/user/{uid}", "STATE": state_path,
                     "call": fake_call, **identity,
                     # Owned PAM helper accounting is proven by PrivatePamSleepControls.
                     "classify_pam_helpers": lambda _: (None, []), "pam_process_snapshot": lambda: []}
            exec(compile(ast.Module(body=[function], type_ignores=[]), str(source), "exec"), scope)
            return scope["inventory"]()

    def test_shipped_snapshot_without_newer_keys_still_detects_session(self):
        uid = os.getuid()
        result = self.run_inventory(dict(self.SHIPPED_STATE), [
            ("1", "959 greeter seat0 tty1"),
            ("3", f"{uid} tuna-test seat0 tty1"),
            ("4", f"{uid} tuna-test - -"),
        ])
        self.assertEqual(result["session"]["Id"], "3")
        self.assertEqual(result["session"]["Service"], "greetd")
        self.assertEqual(result["session_uid"], uid)
        self.assertTrue(any(p["executable"] == "tuna-compositor" for p in result["processes"]))
        self.assertTrue(result["state"]["overview_open"])
        self.assertIsNone(result["state"]["pointer_position"])
        self.assertIsNone(result["state"]["native_relative_motion_count"])

    def test_published_guest_with_pre_rename_binaries_still_detects_session(self):
        # The published flavor can still ship the package from before #505.
        uid = os.getuid()
        result = self.run_inventory(dict(self.SHIPPED_STATE), [
            ("1", "959 greeter seat0 tty1"),
            ("3", f"{uid} tuna-test seat0 tty1"),
        ], prefix="roost")  # tuna-rename: keep
        self.assertEqual(result["session"]["Id"], "3")
        self.assertEqual([p["executable"] for p in result["processes"]],
                         ["roost-compositor"])  # tuna-rename: keep

    def test_missing_owner_session_still_rejected(self):
        uid = os.getuid()
        state = {**self.SHIPPED_STATE, "pointer_position": [0, 0],
                 "native_relative_motion_count": 0}
        with self.assertRaisesRegex(RuntimeError, "expected exactly one greetd user session"):
            self.run_inventory(state, [("4", f"{uid} tuna-test - -")])


class PreRenameGuestLog(unittest.TestCase):
    # tuna-rename: keep-begin
    # The published flavor can still ship binaries from before #505.
    def test_pre_rename_binary_lines_read_as_current_names(self):
        text = lane.normalize_guest_log(
            "session[9]: roost-compositor: drm: output Virtual-1 1280x800\n"
            "session[9]: roost-shell-gtk: keybindings: 64 grabs\n"
            "tuna-vm-health: roost-test ready\n")
        self.assertRegex(text, lane.DRM_RE)
        self.assertRegex(text, lane.SHELL_RE)
        self.assertIn("tuna-vm-health: roost-test ready", text)

    def test_pre_rename_executables_compare_by_role(self):
        self.assertEqual(lane.guest_role("roost-compositor"), "tuna-compositor")
        self.assertEqual(lane.guest_role("tuna-shell-gtk"), "tuna-shell-gtk")
        self.assertEqual(lane.guest_role("nautilus"), "nautilus")
    # tuna-rename: keep-end


class PredatingGuestQualification(unittest.TestCase):
    # Run 37700332957: the published tuna-desktop 0.1.0-2 guest runs a healthy
    # session whose snapshot reports every newer instrumentation key as
    # None, and its shell moves no media-key volume. Newer-behavior gates
    # qualify on such guests instead of failing the whole lifecycle; the
    # current-build lane keeps proving them strictly.
    SHIPPED_STATE = {"locked": False, "windows": [], "focused": None,
                     "active_workspace": 0, "overview_open": False,
                     "pointer_position": None, "native_relative_motion_count": None,
                     "mouse_left_handed": None, "touchpad_left_handed": None,
                     "hot_corners": None}
    CURRENT_STATE = {"locked": False, "windows": [], "focused": None,
                     "active_workspace": 0, "overview_open": False,
                     "pointer_position": [0, 0], "native_relative_motion_count": 0,
                     "mouse_left_handed": False, "touchpad_left_handed": False,
                     "hot_corners": True}
    STRICT_SERIAL = ("\n".join([
        "tuna-vm-health: audio-hda ready",
        "tuna-vm-health: volume=0.40",
        "tuna-vm-health: volume=0.46",
        "tuna-vm-health: volume=0.40",
        "tuna-vm-health: volume=0.40 [MUTED]",
        "tuna-vm-health: volume=0.40",
    ]) + "\n")
    BASELINE_SERIAL = ("tuna-vm-health: audio-hda ready\n"
                       "tuna-vm-health: volume=0.40\n")

    def verdict(self, text, state=None):
        with tempfile.TemporaryDirectory() as directory:
            lifecycle = Path(directory) / "lifecycle"
            lifecycle.mkdir()
            if state is not None:
                (lifecycle / "before.json").write_text(json.dumps({"state": state}))
            return lane.media_volume_ok(text, str(lifecycle))

    def test_predating_snapshot_qualifies(self):
        self.assertTrue(lane.predates_newer_snapshot(dict(self.SHIPPED_STATE)))

    def test_current_snapshot_stays_strict(self):
        self.assertFalse(lane.predates_newer_snapshot(dict(self.CURRENT_STATE)))

    def test_partial_snapshot_stays_strict(self):
        # Fail closed: a guest with only some newer keys still takes the
        # strict path rather than silently qualifying.
        state = {**self.SHIPPED_STATE, "mouse_left_handed": False}
        self.assertFalse(lane.predates_newer_snapshot(state))

    def test_sleep_auth_qualifies_on_published_guest_without_auth_owner(self):
        state = {**self.CURRENT_STATE, "auth_owner": None}
        self.assertTrue(lane.sleep_auth_qualifies(True, state))
        self.assertTrue(lane.sleep_auth_qualifies(True, dict(self.SHIPPED_STATE)))

    def test_sleep_auth_stays_strict_on_current_build_lane(self):
        # marlin-vm runs the branch's own build: never qualified, even if
        # the key were absent.
        self.assertFalse(lane.sleep_auth_qualifies(False, dict(self.SHIPPED_STATE)))
        owner = {"epoch": 1}
        self.assertFalse(lane.sleep_auth_qualifies(True, {**self.CURRENT_STATE, "auth_owner": owner}))

    def test_sheared_lock_result_record_reassembles(self):
        # Run 38855306157 serial: one shell eprintln! split across records.
        serial = "\n".join([
            "[  330.422991] tuna-vm-session[1104]: tuna-shell-gtk: lock password submitted",
            "[  330.424643] tuna-vm-session[1108]: tuna-ibus-bridge: text field left",
            "[  330.439349] tuna-vm-session[1104]: tuna-shell-gtk: lock command result applied=",
            "[  330.441502] tuna-vm-session[957]: tuna-compositor: lock authentication retired",
            "[  330.443201] tuna-vm-session[1104]: false",
            '[  330.444105] tuna-vm-session[957]: {"',
            "[  330.444875] tuna-vm-session[1104]:  request_id=",
            "[  330.445852] tuna-vm-session[957]: applied",
            "[  330.446000] tuna-vm-session[1104]: 55",
        ])
        records = lane.session_records(serial)
        self.assertIn("tuna-shell-gtk: lock command result applied=false request_id=55", records)
        self.assertIn("tuna-shell-gtk: lock password submitted", records)
        self.assertIn("tuna-compositor: lock authentication retired{\"applied", records)

    def test_unsheared_true_result_never_reads_as_refusal(self):
        records = lane.session_records(
            "[ 1.0] tuna-vm-session[1104]: tuna-shell-gtk: lock command result applied=true request_id=7")
        self.assertFalse(any("applied=false" in line for line in records))

    def test_media_volume_strict_sequence_passes(self):
        ok, detail = self.verdict(self.STRICT_SERIAL, self.CURRENT_STATE)
        self.assertTrue(ok)
        self.assertNotIn("predates", detail)

    def test_media_volume_qualifies_on_predating_guest(self):
        ok, detail = self.verdict(self.BASELINE_SERIAL, self.SHIPPED_STATE)
        self.assertTrue(ok)
        self.assertIn("predates", detail)

    def test_media_volume_stays_strict_on_current_guest(self):
        ok, _ = self.verdict(self.BASELINE_SERIAL, self.CURRENT_STATE)
        self.assertFalse(ok)

    def test_media_volume_stays_strict_without_predation_evidence(self):
        ok, _ = self.verdict(self.BASELINE_SERIAL, None)
        self.assertFalse(ok)

    def test_lock_chord_held_until_lock_owns_input_on_predating_guest(self):
        # Run 37727015469: a quick Super release on the 0.1.0-2 guest is
        # dropped before the lock surface maps, latching Super so the
        # password never reaches PAM. Hold the chord on such guests only.
        self.assertGreaterEqual(lane.lock_chord_hold_ms(True), 1000)

    def test_lock_chord_stays_quick_on_current_guest(self):
        # Current guests keep exercising the quick-release regression path.
        self.assertEqual(lane.lock_chord_hold_ms(False), 80)

    def test_media_volume_requires_hda_and_baseline(self):
        ok, _ = self.verdict("tuna-vm-health: volume=0.40\n", self.SHIPPED_STATE)
        self.assertFalse(ok)
        ok, _ = self.verdict("tuna-vm-health: audio-hda ready\n", self.SHIPPED_STATE)
        self.assertFalse(ok)


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


class GreeterSessionList(unittest.TestCase):
    # tunaOS#3110: gtkgreet lists /etc/greetd/environments verbatim, so the
    # Marlin picker showed the raw `tuna-session` command.
    def probe(self, sessions, override=False):
        return {"sessions": sessions, "greeter_command_override": override}

    def test_display_name_preselected_passes(self):
        ok, detail = lane.greeter_list_ok(self.probe(["Tuna Desktop"]))
        self.assertTrue(ok, detail)

    def test_raw_command_fails(self):
        ok, detail = lane.greeter_list_ok(self.probe(["tuna-session"]))
        self.assertFalse(ok)
        self.assertIn("tuna-session", detail)

    def test_name_not_preselected_fails(self):
        self.assertFalse(lane.greeter_list_ok(self.probe(["tuna-session", "Tuna Desktop"]))[0])

    def test_empty_list_fails(self):
        self.assertFalse(lane.greeter_list_ok(self.probe([]))[0])

    def test_command_override_fails(self):
        self.assertFalse(lane.greeter_list_ok(self.probe(["Tuna Desktop"], override=True))[0])

    def test_missing_override_evidence_fails_closed(self):
        self.assertFalse(lane.greeter_list_ok({"sessions": ["Tuna Desktop"]})[0])



if __name__ == "__main__":
    unittest.main()
