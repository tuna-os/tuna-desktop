"""scripts/tuna-remote-security must show why its container proof failed.

A stand-in `podman` plays the proof: no image, compositor or portal runs here.
"""
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import textwrap
import unittest

ROOT = Path(__file__).parents[2]
PODMAN = textwrap.dedent("""\
    #!/usr/bin/env bash
    set -euo pipefail
    [ "$1" = run ] || exit 0
    out=
    for argument in "$@"; do case "$argument" in *:/out) out="${argument%:/out}" ;; esac; done
    printf 'first line\\n' >"$out/compositor.log"
    : >"$out/window.log"
    printf 'not a log\\n' >"$out/input.json"
    sleep 0.05
    seq 1 60 | sed 's/^/eis line /' >"$out/eis-lifetime.log"
    echo 'remote-desktop-reference: line 23: /out/eis-lifetime /out/remote-ei > /out/eis-lifetime.log 2>&1 exited 1' >&2
    exit "$FAKE_PROOF_STATUS"
    """)


class FailureReport(unittest.TestCase):
    def run_wrapper(self, status):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            (tmp / "bin").mkdir()
            podman = tmp / "bin/podman"
            podman.write_text(PODMAN)
            podman.chmod(0o755)
            for name in ("tuna-compositor", "tuna-shell-gtk"):
                binary = tmp / "candidate/usr/bin" / name
                binary.parent.mkdir(parents=True, exist_ok=True)
                binary.write_text("#!/bin/sh\n")
                binary.chmod(0o755)
            out = tmp / "out"
            (out / "runtime").mkdir(parents=True)
            runtime_socket = socket.socket(socket.AF_UNIX)
            runtime_socket.bind(str(out / "runtime/remote-proof"))
            try:
                result = subprocess.run(
                    [str(ROOT / "scripts/tuna-remote-security"), "--candidate", str(tmp / "candidate"), "--out", str(out)],
                    env=dict(os.environ, PATH=f"{tmp / 'bin'}:{os.environ['PATH']}", FAKE_PROOF_STATUS=str(status)),
                    text=True, capture_output=True, timeout=30)
            finally:
                runtime_socket.close()
            return result, sorted(path.name for path in out.rglob("*") if path.is_socket())

    def test_failed_proof_prints_log_ends_and_keeps_its_status(self):
        result, sockets = self.run_wrapper(7)
        self.assertEqual(result.returncode, 7)
        lines = result.stderr.splitlines()
        self.assertIn("remote-desktop-reference: line 23: /out/eis-lifetime /out/remote-ei > /out/eis-lifetime.log 2>&1 exited 1", lines)
        self.assertTrue(any(line.startswith("tuna-remote-security: reference proof exited 7;") for line in lines))
        # Oldest first, so the log written last is nearest the job's error.
        self.assertLess(lines.index("--- compositor.log"), lines.index("--- eis-lifetime.log"))
        self.assertIn("first line", lines)
        # Only the last 40 lines of a long log.
        self.assertIn("eis line 21", lines)
        self.assertIn("eis line 60", lines)
        self.assertNotIn("eis line 20", lines)
        # Empty logs and non-log evidence add nothing.
        self.assertNotIn("--- window.log", lines)
        self.assertNotIn("not a log", lines)
        self.assertEqual(sockets, [])

    def test_passed_proof_prints_no_report(self):
        result, sockets = self.run_wrapper(0)
        self.assertEqual(result.returncode, 0)
        self.assertNotIn("tuna-remote-security:", result.stderr)
        self.assertNotIn("---", result.stderr)
        self.assertEqual(sockets, [])


if __name__ == "__main__":
    unittest.main()
