#!/usr/bin/env python3
"""Exercise real vkms setup control flow with hermetic command doubles.

No package installation, root access, or host kernel changes are performed.
These tests do not substitute for the native DRM smoke lane.
"""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
INSTALLER = ROOT / "scripts/ci-install-vkms"
SMOKE = ROOT / "scripts/tuna-drm-smoke"

# Each call is recorded before execution. Unexpected commands/arguments fail
# closed, including any attempt to run a real apt-get, modprobe, or cargo.
DOUBLE = r'''#!/usr/bin/python3
import json
import os
from pathlib import Path
import sys

name = Path(sys.argv[0]).name
args = sys.argv[1:]
with open(os.environ["CALLS"], "a") as stream:
    stream.write(json.dumps([name, *args]) + "\n")
kernel = os.environ["KERNEL"]
present = Path(os.environ["PRESENT"])
if name == "uname":
    assert args == ["-r"], args
    print(kernel)
elif name == "modinfo":
    assert args in (["-k", kernel, "vkms"],
                    ["-k", kernel, "-F", "filename", "vkms"]), args
    if not present.exists():
        raise SystemExit(1)
    print(f"/lib/modules/{kernel}/kernel/drivers/gpu/drm/vkms/vkms.ko.zst")
elif name == "apt-cache":
    assert len(args) == 2 and args[0] == "policy", args
    assert os.environ["LC_ALL"] == "C"
    if os.environ.get("POLICY_FAIL") == "1":
        raise SystemExit(100)
    package = args[1]
    candidate = json.loads(os.environ["CANDIDATES"]).get(package)
    if candidate is not None:
        print(f"{package}:\n  Installed: (none)\n  Candidate: {candidate}")
elif name == "sudo":
    assert args[0] in ("apt-get", "modprobe"), args
    os.execvp(args[0], args)
elif name == "apt-get":
    assert args[:-1] == ["-o", "Acquire::Retries=3", "-o", "Acquire::http::Timeout=20",
                        "-o", "Acquire::https::Timeout=20", "install", "-y"], args
    assert args[-1] in (f"linux-modules-extra-{kernel}", f"linux-modules-{kernel}"), args
    if os.environ.get("INSTALL_FAIL") == "1":
        raise SystemExit(100)
    if args[-1] == os.environ.get("PROVIDER"):
        present.touch()
elif name == "modprobe":
    assert args == ["vkms"], args
    if os.environ.get("MODPROBE_FAIL") == "1":
        print("modprobe: simulated kernel rejection", file=sys.stderr)
        raise SystemExit(1)
elif name == "readlink":
    # Even successful module loading cannot pass the smoke test without a
    # matching real device. Keep this test independent of the host's sysfs.
    assert args[0] == "-f", args
    print("/sys/devices/not-a-vkms-device")
else:
    raise SystemExit(f"unexpected command {name}: {args}")
'''


class VkmsSetup(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir()
        for name in ("uname", "modinfo", "apt-cache", "sudo", "apt-get",
                     "modprobe", "readlink", "cargo"):
            path = self.bin / name
            path.write_text(DOUBLE)
            path.chmod(0o755)
        self.log = self.root / "calls.jsonl"
        self.present = self.root / "vkms-present"
        self.kernel = "7.0.0-1017-azure"

    def run_setup(self, *, candidates=None, provider="", present=False,
                  kernel=None, smoke=False, **overrides):
        if present:
            self.present.touch()
        env = dict(os.environ, PATH=f"{self.bin}:/usr/bin:/bin",
                   CALLS=str(self.log), PRESENT=str(self.present),
                   KERNEL=kernel or self.kernel,
                   CANDIDATES=json.dumps(candidates or {}), PROVIDER=provider,
                   LC_ALL="not-the-C-locale")
        env.update(overrides)
        command = ["/bin/sh", str(INSTALLER)]
        if smoke:
            command = ["/bin/sh", "-ec", '"$1" && "$2" --artifacts "$3"',
                       "sh", str(INSTALLER), str(SMOKE), str(self.root / "artifacts")]
        self.result = subprocess.run(command, env=env, capture_output=True,
                                     text=True, timeout=10)
        self.calls = [json.loads(line) for line in self.log.read_text().splitlines()]
        return self.result

    def installed(self):
        return [call[-1] for call in self.calls if call[0] == "apt-get"]

    def test_present_vkms_needs_no_package_candidate_or_install(self):
        result = self.run_setup(present=True)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.installed(), [])
        self.assertNotIn("apt-cache", [call[0] for call in self.calls])
        self.assertIn(f"/lib/modules/{self.kernel}/", result.stdout)

    def test_old_kernel_installs_the_matching_extra_package(self):
        kernel = "6.17.0-1022-azure"
        package = f"linux-modules-extra-{kernel}"
        result = self.run_setup(kernel=kernel, candidates={package: "6.17.0-1022.22"},
                                provider=package)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.installed(), [package])
        self.assertIn(f"/lib/modules/{kernel}/", result.stdout)

    def test_new_kernel_can_install_regular_modules_without_an_extra_candidate(self):
        package = f"linux-modules-{self.kernel}"
        result = self.run_setup(candidates={package: "7.0.0-1017.17~24.04.1"},
                                provider=package)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.installed(), [package])

    def test_none_candidate_is_not_installed(self):
        extra = f"linux-modules-extra-{self.kernel}"
        package = f"linux-modules-{self.kernel}"
        result = self.run_setup(candidates={extra: "(none)", package: "7.0"},
                                provider=package)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.installed(), [package])

    def test_no_candidates_is_fatal(self):
        result = self.run_setup()
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.installed(), [])
        self.assertIn(f"vkms is unavailable for running kernel {self.kernel}", result.stderr)

    def test_only_none_candidates_is_fatal(self):
        result = self.run_setup(candidates={f"linux-modules-extra-{self.kernel}": "(none)",
                                           f"linux-modules-{self.kernel}": "(none)"})
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.installed(), [])

    def test_other_kernel_candidate_cannot_satisfy_running_kernel(self):
        result = self.run_setup(candidates={"linux-modules-extra-6.17.0-1022-azure": "6.17"})
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.installed(), [])

    def test_policy_failure_is_fatal_without_install(self):
        result = self.run_setup(POLICY_FAIL="1")
        self.assertEqual(result.returncode, 100)
        self.assertEqual(self.installed(), [])

    def test_install_failure_is_fatal_even_when_another_candidate_exists(self):
        extra = f"linux-modules-extra-{self.kernel}"
        package = f"linux-modules-{self.kernel}"
        result = self.run_setup(candidates={extra: "7.0", package: "7.0"},
                                provider=package, INSTALL_FAIL="1")
        self.assertEqual(result.returncode, 100)
        self.assertEqual(self.installed(), [extra])

    def test_successful_install_without_vkms_is_fatal(self):
        package = f"linux-modules-extra-{self.kernel}"
        result = self.run_setup(candidates={package: "7.0"})
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.installed(), [package])
        self.assertIn("vkms is unavailable", result.stderr)

    def test_regular_package_can_supply_vkms_if_extra_does_not(self):
        extra = f"linux-modules-extra-{self.kernel}"
        package = f"linux-modules-{self.kernel}"
        result = self.run_setup(candidates={extra: "7.0", package: "7.0"}, provider=package)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.installed(), [extra, package])

    def test_present_module_does_not_mask_modprobe_failure(self):
        result = self.run_setup(present=True, smoke=True, MODPROBE_FAIL="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("modprobe: simulated kernel rejection", result.stderr)
        self.assertNotIn("cargo", [call[0] for call in self.calls])
        self.assertEqual((self.root / "artifacts/assertions.txt").read_text(), "")

    def test_installed_module_does_not_mask_modprobe_failure(self):
        package = f"linux-modules-extra-{self.kernel}"
        result = self.run_setup(candidates={package: "7.0"}, provider=package,
                                smoke=True, MODPROBE_FAIL="1")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("modprobe: simulated kernel rejection", result.stderr)
        self.assertEqual(self.installed(), [package])
        self.assertNotIn("cargo", [call[0] for call in self.calls])

    def test_successful_modprobe_without_vkms_device_is_fatal(self):
        result = self.run_setup(present=True, smoke=True)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no vkms card appeared", result.stderr)
        self.assertNotIn("cargo", [call[0] for call in self.calls])
        self.assertEqual((self.root / "artifacts/assertions.txt").read_text(), "")

    def test_workflow_keeps_tests_and_setup_on_merge_ref_before_native_proof(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        drm_job = workflow.split("\n  drm-smoke:", 1)[1].split("\n  grid-cancel-comparison:", 1)[0]
        self.assertNotIn('"linux-modules-extra-$(uname -r)"', drm_job)
        self.assertIn("shellcheck scripts/ci-install-vkms", drm_job)
        self.assertLess(drm_job.index("run: scripts/ci-install-vkms"),
                        drm_job.index("run: scripts/tuna-drm-smoke --artifacts"))
        self.assertNotIn("continue-on-error", drm_job)
        check_job = workflow.split("\n  check:", 1)[1].split("\n  fprint-phase-proof:", 1)[0]
        test_command = "run: /usr/bin/python3 scripts/tests/test_ci_install_vkms.py"
        self.assertNotIn("scripts/tests/test_ci_install_vkms.py", check_job)
        self.assertIn("ref: ${{ github.event.pull_request.head.sha || github.sha }}", check_job)
        # actions/checkout's default is the event's merge ref on a PR.
        # An older PR head need not contain a helper added on the base branch.
        checkout = drm_job.split("- uses: actions/checkout@", 1)[1].split(test_command, 1)[0]
        self.assertNotIn("ref:", checkout)
        self.assertLess(drm_job.index(test_command),
                        drm_job.index("name: Use the Ubuntu archive for DRM dependencies"))
        self.assertLess(drm_job.index(test_command), drm_job.index("apt-get"))


if __name__ == "__main__":
    unittest.main()
