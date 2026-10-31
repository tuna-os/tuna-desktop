#!/usr/bin/env python3
"""Exercise the private colord boundary without sudo, mounts, or a real daemon."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
HELPER = ROOT / "scripts/lib/colord-startup.sh"


class ColordStartupTests(unittest.TestCase):
    def run_boundary(self, mode="exit", mount_status="0", sudo_status="0", stop=False):
        with tempfile.TemporaryDirectory() as tmp:
            directory = Path(tmp)
            binaries = directory / "bin"
            binaries.mkdir()
            scripts = {
                "sudo": 'if [ "$1" = -n ]; then shift; fi\n'
                        'exec 3>&- 4>&-\n'
                        'if [ "$1" = unshare ] && [ "$SUDO_STATUS" != 0 ]; then exit "$SUDO_STATUS"; fi\n'
                        'exec "$@"',
                "unshare": 'shift 3; exec "$@"',
                "mount": 'exit "$MOUNT_STATUS"',
                "setpriv": 'shift 3; exec "$@"',
                "stat": 'case "$*" in *store=*) echo "store=/var/lib/colord uid=0 gid=0 mode=1777";; *) exec /usr/bin/stat "$@";; esac',
                "journalctl": 'echo "journal permission denied" >&2; exit 1',
                "dmesg": 'echo "dmesg permission denied" >&2; exit 1',
                "daemon": 'case "$MODE" in\n'
                          'exit) echo daemon-out; echo daemon-err >&2; exit 42;;\n'
                          'signal) kill -TERM "$$";;\n'
                          'live) exec sleep 30;;\n'
                          'esac',
            }
            for name, body in scripts.items():
                path = binaries / name
                path.write_text("#!/bin/sh\n" + body + "\n")
                path.chmod(0o755)
            env = dict(os.environ, PATH=f"{binaries}:{os.environ['PATH']}",
                       MODE=mode, MOUNT_STATUS=mount_status, SUDO_STATUS=sudo_status)
            command = '''
set -eu
. "$1"
ROOT="$2"
ARTIFACTS="$3"
SYSTEM_BUS='unix:path=/private/test-bus'
COLORD_BIN="$4"
COLORD_PID=''
colord_start
'''
            if stop:
                command += '''
# Wait for the exec boundary, not merely the outer wrapper's birth.
i=0
until grep -q phase=daemon-exec "$COLORD_RECEIPT"; do
    i=$((i+1)); [ "$i" -lt 100 ]; sleep 0.02
done
colord_stop
[ -z "$COLORD_PID" ]
'''
            else:
                command += '''
i=0
while kill -0 "$COLORD_PID" 2>/dev/null; do
    i=$((i+1)); [ "$i" -lt 100 ]; sleep 0.02
done
colord_reap
first="$COLORD_WAIT_STATUS"
colord_reap
colord_failure_receipt
[ "$first" = "$COLORD_WAIT_STATUS" ]
printf 'observed=%s\\n' "$COLORD_WAIT_STATUS"
'''
            result = subprocess.run(
                ["sh", "-c", command, "sh", str(HELPER), str(ROOT), str(directory),
                 str(binaries / "daemon")], env=env, text=True, capture_output=True, timeout=15)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            return (result.stdout, (directory / "colord-startup.txt").read_text(),
                    (directory / "colord.log").read_text(),
                    (directory / "colord-launch.log").read_text())

    def test_exit_status_and_streams_survive_diagnostics_and_second_reap(self):
        stdout, receipt, daemon, launch = self.run_boundary()
        self.assertIn("observed=42", stdout)
        self.assertEqual(daemon, "daemon-out\ndaemon-err\n")
        self.assertEqual(launch, "")
        self.assertEqual(receipt.count("wrapper_status=42"), 1)
        self.assertIn("child_status=42", receipt)
        phases = ["phase=namespace-launch", "phase=mount", "phase=drop-uid", "phase=daemon-exec"]
        self.assertEqual(sorted(phases, key=receipt.index), phases)
        for evidence in ["wrapper_pid=", "child_pid=", "supervisor_pid=", "intended_uid=",
                         "intended_gid=", "bus=unix:path=/private/test-bus", "binary=",
                         "store=/var/lib/colord uid=0 gid=0 mode=1777",
                         "journal permission denied", "journal_status=1", "dmesg permission denied",
                         "dmesg_status=1"]:
            self.assertIn(evidence, receipt)

    def test_signaled_daemon_retains_shell_status_without_inventing_cause(self):
        stdout, receipt, daemon, _ = self.run_boundary(mode="signal")
        self.assertIn("observed=143", stdout)
        self.assertIn("child_status=143", receipt)
        self.assertIn("wrapper_status=143", receipt)
        self.assertIn("signal_candidate=15", receipt)
        self.assertIn("not proof of signal", receipt)
        self.assertEqual(daemon, "")

    def test_mount_failure_stops_before_privilege_drop(self):
        stdout, receipt, daemon, _ = self.run_boundary(mount_status="42")
        self.assertIn("observed=42", stdout)
        self.assertIn("phase=mount", receipt)
        self.assertNotIn("phase=drop-uid", receipt)
        self.assertNotIn("child_status=", receipt)
        self.assertEqual(daemon, "")

    def test_sudo_failure_stops_before_namespace(self):
        stdout, receipt, daemon, _ = self.run_boundary(sudo_status="42")
        self.assertIn("observed=42", stdout)
        self.assertIn("phase=namespace-launch", receipt)
        self.assertNotIn("phase=mount", receipt)
        self.assertEqual(daemon, "")

    def test_live_daemon_cleanup_is_owned_and_bounded(self):
        _, receipt, daemon, _ = self.run_boundary(mode="live", stop=True)
        self.assertIn("child_status=143", receipt)
        self.assertIn("wrapper_status=143", receipt)
        self.assertEqual(daemon, "")

    def test_cleanup_does_not_signal_a_reused_pid(self):
        with tempfile.TemporaryDirectory() as tmp:
            receipt = Path(tmp) / "receipt"
            with subprocess.Popen(["sleep", "30"]) as unrelated:
                try:
                    receipt.write_text(f"child_identity={unrelated.pid}:not-the-start-time\n")
                    result = subprocess.run(
                        ["sh", "-c", '. "$1"; COLORD_RECEIPT="$2"; COLORD_PID=""; colord_stop',
                         "sh", str(HELPER), str(receipt)], timeout=5, capture_output=True)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    self.assertIsNone(unrelated.poll())
                finally:
                    unrelated.terminate()
                    unrelated.wait(timeout=5)


if __name__ == "__main__":
    unittest.main()
