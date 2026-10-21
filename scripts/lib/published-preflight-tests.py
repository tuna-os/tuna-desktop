#!/usr/bin/env python3
"""Pure/fake-command tests; never execute Podman, Skopeo or an image."""
import copy
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]


def load(name, path):
    loader = importlib.machinery.SourceFileLoader(name, str(path))
    spec = importlib.util.spec_from_loader(name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


m = load("published_preflight", ROOT / "scripts/tuna-published-preflight")
fixtures = load("published_dm_test_fixtures", ROOT / "scripts/lib/published-dm-contract-tests.py")


def encoded(value):
    return json.dumps(value, separators=(",", ":")).encode()


class FakeCommands:
    def __init__(self, report):
        self.report, self.calls = report, []
        self.config = {"architecture": "amd64", "os": "linux",
                       "rootfs": {"type": "layers", "diff_ids": ["sha256:" + "d" * 64]}}
        self.config_raw = encoded(self.config)
        self.manifest = {"schemaVersion": 2, "mediaType": next(iter(m.MANIFEST_TYPES)),
                         "config": {"mediaType": next(iter(m.CONFIG_TYPES)), "digest": m.digest(self.config_raw),
                                    "size": len(self.config_raw)}, "layers": [{"digest": "sha256:" + "e" * 64}]}
        self.manifest_raw = encoded(self.manifest)
        self.index = {"schemaVersion": 2, "mediaType": next(iter(m.INDEX_TYPES)),
                      "manifests": [{"mediaType": self.manifest["mediaType"], "digest": m.digest(self.manifest_raw),
                                     "size": len(self.manifest_raw), "platform": {"os": "linux", "architecture": "amd64"}}]}
        self.index_raw = encoded(self.index)
        self.image = m.REPOSITORY + "@" + m.digest(self.index_raw)
        self.cid = "c" * 64
        self.exit_code = 0
        self.mutations = {}

    def __call__(self, stage, argv, **kwargs):
        self.calls.append((stage, argv, kwargs))
        if stage in self.mutations:
            result = self.mutations[stage]
            if isinstance(result, Exception):
                raise result
            return result, 0
        if stage == "registry-subject":
            return self.index_raw, 0
        if stage == "registry-platform":
            return self.manifest_raw, 0
        if stage == "registry-config":
            return self.config_raw, 0
        if stage == "local-image":
            return encoded([{"Id": m.digest(self.config_raw)[7:], "Architecture": "amd64", "Os": "linux",
                             "RootFS": {"Layers": self.config["rootfs"]["diff_ids"]}, "Digest": m.digest(self.index_raw)}]), 0
        if stage == "container-create":
            mount = next(arg for arg in argv if arg.startswith("--mount="))
            self.source = mount.split(",src=", 1)[1].split(",dst=", 1)[0]
            self.target = mount.split(",dst=", 1)[1].split(",ro=", 1)[0]
            self.arguments = argv[argv.index(m.digest(self.config_raw)) + 1:]
            self.name = next(item for item in argv if item.startswith("--name="))[7:]
            self.staged_modes = {name: (Path(self.source) / name).stat().st_mode & 0o777 for name in
                                 (".", "lib", "tuna-published-dm-inventory", "lib/published_dm_contract.py")}
            return self.cid.encode() + b"\n", 0
        if stage.startswith("container-inspect-"):
            value = self.container(after=stage.endswith("after"))
            return encoded([value]), 0
        if stage == "inventory":
            value = copy.deepcopy(self.report)
            value["subject"] = {"image": self.image, "platform_digest": m.digest(self.manifest_raw),
                                "binding": "caller-declared"}
            return encoded(value), self.exit_code
        if stage == "container-cleanup":
            return b"", 0
        if stage == "container-cleanup-confirm-absent":
            return b"", 1
        raise AssertionError(stage)

    def container(self, after=False):
        return {"Id": self.cid, "Name": getattr(self, "name", "test"), "Image": m.digest(self.config_raw)[7:], "Path": "/usr/bin/python3",
                "Args": self.arguments, "HostConfig": {"ReadonlyRootfs": True, "NetworkMode": "none",
                    "Privileged": False, "Tmpfs": {}, "SecurityOpt": ["no-new-privileges"],
                    "PidMode": "private", "IpcMode": "private", "UTSMode": "private",
                    "PidsLimit": 64, "LogConfig": {"Type": "none"}},
                "Config": {"User": "0:0", "Env": m.ENVIRONMENT,
                           "Labels": {"org.tuna.preflight.owner": self.target},
                           "Healthcheck": {"Test": ["NONE"]}, "Timeout": 150}, "EffectiveCaps": [], "BoundingCaps": [],
                "Mounts": [{"Type": "bind", "Source": self.source, "Destination": self.target, "RW": False}],
                "State": {"Status": "exited" if after else "created", "Running": False,
                          "ExitCode": self.exit_code, "OOMKilled": False, "Error": ""}}


class Orchestration(unittest.TestCase):
    def setUp(self):
        self.fixture = fixtures.MountedInventory()
        self.fixture.setUp()
        self.addCleanup(self.fixture.doCleanups)
        self.report = self.fixture.collect()
        self.fake = FakeCommands(self.report)
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.out = Path(self.temp.name)

    def execute(self):
        result = m.execute(self.fake.image, self.out, self.fake)
        receipt = json.loads((self.out / "published-image-preflight.json").read_text())
        return result, receipt

    def test_complete_digest_container_and_inventory_chain(self):
        code, receipt = self.execute()
        self.assertEqual(code, 0, receipt)
        self.assertTrue(receipt["image_binding_verified"])
        self.assertTrue(receipt["inventory_subject_join_verified"])
        self.assertTrue(receipt["container_removed"])
        self.assertFalse(receipt["runtime_qualified"])
        self.assertFalse(receipt["effective_launch_verified"])
        self.assertFalse(receipt["inventory"]["effective_launch_verified"])
        stages = [call[0] for call in self.fake.calls]
        self.assertLess(stages.index("container-inspect-before"), stages.index("inventory"))
        self.assertEqual(stages[-1], "container-cleanup-confirm-absent")
        self.assertNotIn(self.fake.source, json.dumps(receipt))
        self.assertEqual(receipt["container"]["observed_invocation"]["args"], self.fake.arguments)
        self.assertTrue(m.valid_digest(receipt["runner_sha256"]))
        self.assertEqual(self.fake.staged_modes, {".": 0o555, "lib": 0o555,
                         "tuna-published-dm-inventory": 0o444, "lib/published_dm_contract.py": 0o444})

    def test_create_uses_verified_image_id_and_all_restrictions(self):
        self.execute()
        argv = next(args for stage, args, _ in self.fake.calls if stage == "container-create")
        for required in ("--pull=never", "--read-only", "--read-only-tmpfs=false", "--image-volume=ignore",
                         "--network=none", "--cap-drop=all", "--security-opt=no-new-privileges",
                         "--unsetenv-all", "--http-proxy=false", "--no-healthcheck", "--systemd=false",
                         "--log-driver=none", "--timeout=150", "--entrypoint=/usr/bin/python3"):
            self.assertIn(required, argv)
        self.assertIn(m.digest(self.fake.config_raw), argv)
        self.assertEqual(self.fake.arguments[:4], ["-I", "-S", "-B", "-c"])
        self.assertIn("os.path.realpath(p)==p", self.fake.arguments[4])
        self.assertTrue(next(item for item in argv if item.startswith("--mount=")).endswith(",ro=true"))
        self.assertNotIn("--privileged", argv)

    def test_metadata_digest_mismatch_stops_before_container(self):
        for stage in ("registry-subject", "registry-platform", "registry-config"):
            self.fake.calls.clear()
            self.fake.mutations = {stage: b"{}"}
            code, receipt = self.execute()
            with self.subTest(stage=stage):
                self.assertEqual(code, 1)
                self.assertEqual(receipt["failure_code"], "metadata-content-digest-or-size-mismatch")
                self.assertFalse(any(call[0] == "container-create" for call in self.fake.calls))

    def test_ambiguous_platform_is_rejected(self):
        self.fake.index["manifests"].append(copy.deepcopy(self.fake.index["manifests"][0]))
        self.fake.index_raw = encoded(self.fake.index)
        self.fake.image = m.REPOSITORY + "@" + m.digest(self.fake.index_raw)
        self.assertEqual(self.execute()[1]["failure_code"], "ambiguous-or-missing-amd64-platform")

    def test_single_manifest_subject_is_bound_without_guessing_an_index(self):
        self.fake.index_raw = self.fake.manifest_raw
        self.fake.image = m.REPOSITORY + "@" + m.digest(self.fake.manifest_raw)
        code, receipt = self.execute()
        self.assertEqual(code, 0, receipt)
        self.assertEqual(receipt["binding"]["platform_digest"], m.digest(self.fake.manifest_raw))
        self.assertFalse(any(call[0] == "registry-platform" for call in self.fake.calls))

    def test_descriptor_size_is_verified(self):
        self.fake.index["manifests"][0]["size"] += 1
        self.fake.index_raw = encoded(self.fake.index)
        self.fake.image = m.REPOSITORY + "@" + m.digest(self.fake.index_raw)
        self.assertEqual(self.execute()[1]["failure_code"], "metadata-content-digest-or-size-mismatch")

    def test_local_image_wrong_id_fails_before_create(self):
        self.fake.mutations["local-image"] = encoded([{"Id": "f" * 64}])
        self.assertEqual(self.execute()[1]["failure_code"], "local-image-binding-mismatch")
        self.assertFalse(any(call[0] == "container-create" for call in self.fake.calls))

    def test_local_image_diff_ids_must_match_hashed_config(self):
        raw, _ = self.fake("local-image", [])
        value = json.loads(raw)
        value[0]["RootFS"]["Layers"] = ["sha256:" + "f" * 64]
        self.fake.mutations["local-image"] = encoded(value)
        self.assertEqual(self.execute()[1]["failure_code"], "local-image-binding-mismatch")

    def test_failed_inventory_keeps_sanitized_evidence_and_cleanup(self):
        self.fake.exit_code = 1
        self.report.update(static_inventory_ready=False, errors=["private token=SECRET\n::error::injected"])
        code, receipt = self.execute()
        self.assertEqual(code, 1)
        self.assertEqual(receipt["failure_code"], "static-inventory-rejected")
        self.assertEqual(receipt["inventory"]["errors"], ["inventory-check-failed"])
        self.assertNotIn("SECRET", json.dumps(receipt))
        self.assertIn("/usr/bin/gdm", receipt["inventory"]["files"])
        self.assertTrue(receipt["container_removed"])

    def test_timeout_and_invalid_json_still_remove_container(self):
        for failure in (m.Failure("command-timeout"), b"not json with SECRET"):
            self.fake.mutations["inventory"] = failure
            code, receipt = self.execute()
            self.assertEqual(code, 1)
            self.assertTrue(receipt["container_removed"])
            self.assertNotIn("SECRET", json.dumps(receipt))

    def test_cleanup_failure_cannot_pass(self):
        self.fake.mutations["container-cleanup"] = m.Failure("command-timeout")
        code, receipt = self.execute()
        self.assertEqual(code, 1)
        self.assertEqual(receipt["failure_code"], "container-cleanup-failed")

    def test_uncertain_create_never_recovers_starts_retries_or_claims_cleanup(self):
        original = self.fake.__call__
        def uncertain(stage, argv, **kwargs):
            value = original(stage, argv, **kwargs)
            if stage == "container-create":
                raise m.Failure("command-timeout")
            return value
        code = m.execute(self.fake.image, self.out, uncertain)
        receipt = json.loads((self.out / "published-image-preflight.json").read_text())
        self.assertEqual(code, 1)
        self.assertFalse(receipt["create_confirmed"])
        self.assertFalse(receipt["container_cleanup_terminal"])
        self.assertTrue(receipt["command_groups_settled"])
        self.assertEqual(sum(call[0] == "container-create" for call in self.fake.calls), 1)
        self.assertEqual(self.fake.calls[-1][0], "container-create")
        self.assertNotIn("container_removed", receipt)

    def test_invalid_created_id_stays_uncertain_without_name_recovery(self):
        self.fake.mutations["container-create"] = b"invalid id"
        code, receipt = self.execute()
        self.assertEqual(code, 1)
        self.assertEqual(receipt["failure_code"], "invalid-created-container-id")
        self.assertFalse(receipt["create_confirmed"])
        self.assertFalse(receipt["container_cleanup_terminal"])
        self.assertEqual(self.fake.calls[-1][0], "container-create")

    def test_successful_create_requires_ownership_before_cleanup(self):
        self.fake.mutations["container-inspect-before"] = encoded([{"Id": self.fake.cid}])
        code, receipt = self.execute()
        self.assertEqual(code, 1)
        self.assertTrue(receipt["create_confirmed"])
        self.assertFalse(receipt["container_cleanup_terminal"])
        self.assertFalse(any(stage.startswith("container-cleanup") or stage == "inventory"
                             for stage, _, _ in self.fake.calls))

    def test_unsettled_create_never_recovers_or_retries(self):
        original = self.fake.__call__
        def uncertain(stage, argv, **kwargs):
            value = original(stage, argv, **kwargs)
            if stage == "container-create":
                uncertain.settled = False
                raise m.Failure("command-group-retirement-unverified")
            return value
        self.assertEqual(m.execute(self.fake.image, self.out, uncertain), 1)
        receipt = json.loads((self.out / "published-image-preflight.json").read_text())
        self.assertFalse(receipt["command_groups_settled"])
        self.assertFalse(receipt["container_cleanup_terminal"])
        self.assertFalse(any(stage.startswith("container-cleanup") or stage == "inventory"
                             for stage, _, _ in self.fake.calls))
        self.assertEqual(sum(stage == "container-create" for stage, _, _ in self.fake.calls), 1)

    def test_remove_success_needs_confirmed_absence(self):
        self.fake.mutations["container-cleanup-confirm-absent"] = b""
        code, receipt = self.execute()
        self.assertEqual(code, 1)
        self.assertFalse(receipt["container_removed"])


    def test_partial_layer_and_runtime_claims_cannot_pass(self):
        for field, value in (("evidence_kind", "registry-layer-prefix"), ("runtime_qualified", True),
                             ("effective_launch_verified", True)):
            self.fake.report = copy.deepcopy(self.report)
            self.fake.report[field] = value
            self.assertEqual(self.execute()[0], 1)

    def test_unexpected_nested_fields_are_not_persisted(self):
        self.report["files"]["/usr/bin/gdm"]["environment"] = {"TOKEN": "SECRET"}
        self.report["subject"]["private"] = "SECRET"
        self.report["environment"] = {"TOKEN": "SECRET"}
        _, receipt = self.execute()
        self.assertNotIn("SECRET", json.dumps(receipt))


class PureBinding(unittest.TestCase):
    def setUp(self):
        self.fake = FakeCommands({})
        self.binding = m.verify_registry(self.fake, self.fake.image)
        command, self.args = m.container_command(self.binding, Path("/safe/helper"), "/__tuna_helper", "test")
        self.fake.source, self.fake.target, self.fake.arguments = "/safe/helper", "/__tuna_helper", self.args
        self.value = self.fake.container()

    def verify(self, value, exited=None):
        m.verify_container(value, self.fake.cid, self.binding, Path(self.fake.source), self.fake.target, self.args, exited)

    def test_caps_nil_or_empty_pass_but_missing_or_nonempty_fail(self):
        for field in ("EffectiveCaps", "BoundingCaps"):
            for value in (None, []):
                candidate = copy.deepcopy(self.value)
                candidate[field] = value
                self.verify(candidate)
            candidate[field] = ["CAP_SYS_ADMIN"]
            with self.assertRaises(m.Failure):
                self.verify(candidate)
            del candidate[field]
            with self.assertRaises(m.Failure):
                self.verify(candidate)

    def test_isolation_command_environment_and_mount_tampering_fail(self):
        changes = [lambda d: d.update(Image="f" * 64), lambda d: d.update(Path="/bin/sh"),
                   lambda d: d.update(Args=[]), lambda d: d["HostConfig"].update(ReadonlyRootfs=False),
                   lambda d: d["HostConfig"].update(NetworkMode="host"),
                   lambda d: d["HostConfig"].update(Privileged=True),
                   lambda d: d["HostConfig"].update(Tmpfs={"/run": "rw"}),
                   lambda d: d["HostConfig"].update(SecurityOpt=[]),
                   lambda d: d["HostConfig"].update(SecurityOpt=["no-new-privileges", "seccomp=unconfined"]),
                   lambda d: d["HostConfig"].update(PidMode="host"),
                   lambda d: d["HostConfig"].update(IpcMode="shareable"),
                   lambda d: d["HostConfig"].update(UTSMode="host"),
                   lambda d: d["HostConfig"].update(PidsLimit=0),
                   lambda d: d["HostConfig"].update(PidsLimit=True),
                   lambda d: d["HostConfig"].update(LogConfig={"Type": "journald"}),
                   lambda d: d["Config"].update(Env=m.ENVIRONMENT + ["LD_PRELOAD=/evil"]),
                   lambda d: d["Config"].update(Healthcheck={"Test": ["CMD", "/bin/evil"]}),
                   lambda d: d["Config"].update(StartupHealthCheck={"Test": ["CMD", "/bin/evil"]}),
                   lambda d: d["Config"].update(SystemdMode=True),
                   lambda d: d["Config"].update(SystemdMode=None),
                   lambda d: d["Config"].update(Timeout=0),
                   lambda d: d["Config"].update(Timeout=True),
                   lambda d: d["Mounts"][0].update(RW=True),
                   lambda d: d["Mounts"][0].update(Destination="/usr/bin"),
                   lambda d: d["State"].update(Status="running", Running=True)]
        for change in changes:
            candidate = copy.deepcopy(self.value)
            change(candidate)
            with self.subTest(change=change), self.assertRaises(m.Failure):
                self.verify(candidate)

    def test_post_exit_checks_exit_oom_and_error(self):
        value = self.fake.container(after=True)
        for status in ("stopped", "exited"):
            value["State"]["Status"] = status
            self.verify(value, exited=0)
        for field, bad in (("ExitCode", 1), ("Running", True), ("OOMKilled", True), ("Error", "failure")):
            candidate = copy.deepcopy(value)
            candidate["State"][field] = bad
            with self.assertRaises(m.Failure):
                self.verify(candidate, exited=0)

    def test_rootful_host_check_observes_existing_capability_only(self):
        with patch.object(m.sys, "platform", "linux"), patch.object(m.os, "geteuid", return_value=0):
            with patch.object(m.Path, "read_text", return_value="CapEff:\t0000000000200000\n"):
                m.verify_supervisor_host()
            for value in ("", "CapEff:\t0000000000000000\n", "CapEff:\tinvalid\n"):
                with patch.object(m.Path, "read_text", return_value=value):
                    with self.assertRaisesRegex(m.Failure, "rootful-linux-supervisor-required"):
                        m.verify_supervisor_host()
        with patch.object(m.os, "geteuid", return_value=1000):
            with self.assertRaises(m.Failure):
                m.verify_supervisor_host()

    def test_duplicate_json_and_nonfinite_values_fail(self):
        for raw in (b'{"key":1,"key":2}', b'{"key":NaN}'):
            with self.assertRaises(m.Failure):
                m.decode(raw)


class BoundedCommands(unittest.TestCase):
    def test_stderr_is_suppressed_and_host_environment_is_not_forwarded(self):
        observations = []
        with patch.dict(os.environ, {"PREFLIGHT_PRIVATE": "SECRET"}):
            output, code = m.Commands(observations)("test", [sys.executable, "-c",
                "import os,sys; print(os.getenv('PREFLIGHT_PRIVATE')); sys.stderr.write('TOKEN=SECRET')"])
        self.assertEqual(code, 0)
        self.assertEqual(output, b"None\n")
        self.assertEqual(observations[0]["stderr"]["bytes"], 12)
        self.assertNotIn("SECRET", json.dumps(observations))

    def test_stdout_stderr_and_time_are_bounded(self):
        cases = [("print('x'*1000)", {"limit": 100}, "stdout-limit"),
                 ("import sys;sys.stderr.write('x'*1000)", {}, "stderr-limit"),
                 ("import time;time.sleep(8)", {"timeout": 4}, "command-timeout")]
        with patch.object(m, "MAX_STDERR", 100):
            for code, options, error in cases:
                with self.subTest(error=error), self.assertRaisesRegex(m.Failure, error):
                    m.Commands([])("test", [sys.executable, "-c", code], **options)

    def test_cleanup_is_allowed_after_main_deadline(self):
        runner = m.Commands([])
        runner.deadline = 0
        self.assertEqual(runner("container-cleanup", [sys.executable, "-c", "pass"])[1], 0)

    def test_cleanup_operations_share_one_deadline(self):
        runner = m.Commands([])
        runner.cleanup_deadline = 0
        with self.assertRaisesRegex(m.Failure, "cleanup-deadline"):
            runner("container-cleanup-recover", [sys.executable, "-c", "pass"])

    def test_exited_leader_with_descendant_pipes_is_retired_before_reap(self):
        observations = []
        with tempfile.TemporaryDirectory() as temp:
            marker = str(Path(temp) / "late")
            code = ("import os,time; pid=os.fork(); "
                    "os._exit(0) if pid else None; "
                    "time.sleep(8); open(" + repr(marker) + ",'w').write('late')")
            runner = m.Commands(observations)
            with patch.object(m.subprocess.Popen, "poll", side_effect=AssertionError("leader reaped early")):
                with self.assertRaisesRegex(m.Failure, "command-timeout"):
                    runner("test", [sys.executable, "-c", code], timeout=4)
            self.assertTrue(runner.settled)
            self.assertTrue(observations[0]["command_group_retired"])
            self.assertFalse(Path(marker).exists())

    def test_completed_leader_does_not_leave_closed_pipe_descendants(self):
        observations = []
        code = ("import os,time; pid=os.fork(); os._exit(0) if pid else None; "
                "os.close(1); os.close(2); time.sleep(3)")
        kills = []
        original = m.os.killpg
        def record(pid, sig):
            self.assertEqual(os.getpgid(pid), pid)
            # WNOWAIT proves that the exited leader still anchors this PGID.
            self.assertIsNotNone(os.waitid(os.P_PID, pid, os.WEXITED | os.WNOHANG | os.WNOWAIT))
            kills.append(pid)
            return original(pid, sig)
        with patch.object(m.os, "killpg", side_effect=record):
            self.assertEqual(m.Commands(observations)("test", [sys.executable, "-c", code])[1], 0)
        self.assertTrue(kills)
        self.assertFalse(m.group_is_live(kills[0]))

    def test_unverified_retirement_blocks_all_later_commands(self):
        observations = []
        runner = m.Commands(observations)
        with patch.object(m, "group_is_live", return_value=True):
            with self.assertRaisesRegex(m.Failure, "command-group-retirement-unverified"):
                runner("test", [sys.executable, "-c", "pass"], timeout=.4)
        self.assertFalse(runner.settled)
        self.assertFalse(observations[0]["command_group_retired"])
        with patch.object(m.subprocess, "Popen", side_effect=AssertionError("must not launch")):
            with self.assertRaisesRegex(m.Failure, "prior-command-group-unsettled"):
                runner("container-cleanup-exists", [sys.executable, "-c", "pass"])
        # Test-only fixture teardown, after proving production retains its anchor.
        for process in runner.unsettled:
            process.wait(timeout=1)

    def test_retirement_receives_existing_shared_deadline(self):
        runner = m.Commands([])
        runner.deadline = time.monotonic() + 10
        with patch.object(runner, "retire", wraps=runner.retire) as retire:
            self.assertEqual(runner("test", [sys.executable, "-c", "pass"], timeout=30)[1], 0)
        self.assertEqual(retire.call_args.args[1], runner.deadline)
        self.assertTrue(runner.settled)


class SafeArtifacts(unittest.TestCase):
    def test_symlink_output_directory_is_refused(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "real").mkdir()
            (root / "link").symlink_to(root / "real", target_is_directory=True)
            with self.assertRaises(OSError):
                m.ReceiptFile(root / "link")
            self.assertEqual(list((root / "real").iterdir()), [])

    def test_existing_symlink_and_hardlink_receipts_are_never_truncated(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            victim = root / "other"
            victim.write_text("preserve")
            out = root / "out"
            out.mkdir()
            receipt = out / "published-image-preflight.json"
            for link in (lambda: receipt.symlink_to(victim), lambda: os.link(victim, receipt)):
                link()
                m.ReceiptFile(out).write("{}")
                self.assertEqual(victim.read_text(), "preserve")
                self.assertEqual(receipt.read_text(), "{}\n")
                self.assertEqual(receipt.stat().st_mode & 0o777, 0o644)
                receipt.unlink()

    def test_open_directory_is_anchored_and_writable_root_is_refused(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            out = root / "out"
            out.mkdir()
            writer = m.ReceiptFile(out)
            moved = root / "moved"
            out.rename(moved)
            other = root / "other"
            other.mkdir()
            out.symlink_to(other, target_is_directory=True)
            writer.write("{}")
            self.assertEqual(list(other.iterdir()), [])
            self.assertTrue((moved / "published-image-preflight.json").is_file())
            other.chmod(0o777)
            with self.assertRaisesRegex(m.Failure, "unsafe-artifact-directory"):
                m.ReceiptFile(other)


class WorkflowContract(unittest.TestCase):
    def test_preflight_precedes_fixtures_and_policy_remains(self):
        text = (ROOT / ".github/workflows/marlin-tuna-nightly.yml").read_text()
        positions = [text.index(fragment) for fragment in ("sudo podman pull", "test \"$(sudo podman image inspect",
                     "scripts/tuna-published-preflight", "sudo podman build", "bootc install to-disk")]
        self.assertEqual(positions, sorted(positions))
        self.assertIn("--out published-vm-artifacts", text)
        self.assertIn("sudo -n /usr/bin/python3 -I -S -B scripts/tuna-published-preflight", text)
        self.assertEqual(m.PODMAN, ["/usr/bin/podman", "--remote=false"])
        self.assertIn("timeout-minutes: 90", text)
        self.assertIn("permissions:\n  contents: read", text)
        self.assertIn("cron: '17 2 * * *'", text)
        self.assertIn("if: always()", text)
        containerfile = (ROOT / "packaging/marlin/vm-lane/Containerfile.published").read_text()
        self.assertIn("/usr/libexec/tunaos/greetd-session /etc/greetd/config.toml /etc/greetd/gtkgreet.css", containerfile)
        self.assertIn("! grep -q '\\[initial_session\\]'", containerfile)

    def test_both_pure_suites_are_wired_beside_existing_vm_tests(self):
        text = (ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn("      - run: python3 scripts/lib/vm-boot-tests.py\n"
                      "      - run: python3 scripts/lib/published-dm-contract-tests.py\n"
                      "      - run: python3 scripts/lib/published-preflight-tests.py\n", text)


if __name__ == "__main__":
    unittest.main()
