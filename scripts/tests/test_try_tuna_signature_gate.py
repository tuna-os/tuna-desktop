#!/usr/bin/env python3
"""The try-tuna-desktop pre-pull signature gate fails fast on unsigned builds.

Runs scripts/try-tuna-desktop against stubbed host tools (no network, no
pull, no VM): an unsigned published tag must die before `podman pull`
instead of after a multi-GB download, while a signed tag still flows into
the authoritative pinned-digest check.
"""
import os
import pathlib
import subprocess
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
SCRIPT = pathlib.Path(os.environ.get("TRY_TUNA_SCRIPT", ROOT / "scripts/try-tuna-desktop"))
FAKE_IMAGE = "ghcr.io/example/unsigned:test"
FAKE_DIGEST = "sha256:" + "0" * 64


def write_stub(path, body):
    path.write_text("#!/bin/sh\n" + body + "\n")
    path.chmod(0o755)


def make_bin(tmp):
    """Stub every host tool try-tuna-desktop demands, logging calls."""
    log = pathlib.Path(tmp) / "calls"
    log.mkdir()
    for name in ("qemu-system-x86_64", "qemu-system-aarch64", "qemu-img"):
        write_stub(pathlib.Path(tmp) / name, "exit 0")
    # The disk preflight is not under test; report 30 GiB so the gate under
    # test is always reached (real `tail` still parses the output).
    write_stub(pathlib.Path(tmp) / "df", 'echo "Avail"; echo 31457280')
    write_stub(pathlib.Path(tmp) / "podman", """\
echo "podman $@" >> "$STUB_LOG/podman.log"
if [ "$1" = "image" ]; then echo FAKE_DIGEST; fi
exit 0
""".replace("FAKE_DIGEST", FAKE_DIGEST))
    write_stub(pathlib.Path(tmp) / "corral", """\
echo "corral $@" >> "$STUB_LOG/corral.log"
if [ "$1" = "plugin" ] && [ "$2" = "list" ]; then echo "bootc /stub/corral-bootc"; fi
if [ "$1" = "bootc" ] && [ "$2" = "status" ]; then echo "VM NAMESPACE BOOTC IMAGE"; fi
if [ "$1" = "list" ]; then echo "NAME BACKEND STATUS"; fi
exit 0
""")
    return log


COSIGN_STUB = """\
echo "cosign $@" >> "$STUB_LOG/cosign.log"
ref="$2"
case "$ref" in
  *@*) kind=digest ;;
  *) kind=tag ;;
esac
mode="$(eval echo \\$COSIGN_${kind}_MODE)"
if [ "$mode" = "signed" ]; then exit 0; fi
echo "Error: $mode" >&2
exit 1
"""


def run_script(tmp, log, tag_mode, digest_mode="unsigned"):
    write_stub(pathlib.Path(tmp) / "cosign", COSIGN_STUB)
    env = dict(os.environ,
               PATH=tmp + os.pathsep + os.environ["PATH"],
               STUB_LOG=str(log),
               COSIGN_tag_MODE=tag_mode,
               COSIGN_digest_MODE=digest_mode,
               TRY_TUNA_IMAGE=FAKE_IMAGE,
               TRY_TUNA_NO_OPEN="1")
    return subprocess.run(["bash", str(SCRIPT)], cwd=tmp, env=env,
                          capture_output=True, text=True, timeout=120)


def read_lines(log, name):
    path = log / name
    return path.read_text().splitlines() if path.exists() else []


class SignatureGateTests(unittest.TestCase):
    def test_unsigned_tag_dies_before_pull(self):
        with tempfile.TemporaryDirectory() as tmp:
            log = make_bin(tmp)
            result = run_script(tmp, log, tag_mode="no signatures found")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("no signatures published for " + FAKE_IMAGE, result.stderr)
            self.assertIn("TRY_TUNA_IMAGE", result.stderr)
            cosign_calls = read_lines(log, "cosign.log")
            refs = [c.split()[2] for c in cosign_calls if c.startswith("cosign verify ")]
            self.assertIn(FAKE_IMAGE, refs,
                          f"expected a tag-form precheck, saw: {cosign_calls}")
            podman_calls = read_lines(log, "podman.log")
            self.assertFalse(any("pull" in c for c in podman_calls),
                             f"must not pull an unverifiable image, saw: {podman_calls}")

    def test_signed_tag_still_faces_pinned_digest_check(self):
        with tempfile.TemporaryDirectory() as tmp:
            log = make_bin(tmp)
            result = run_script(tmp, log, tag_mode="signed",
                                digest_mode="no signatures found")
            self.assertNotEqual(result.returncode, 0)
            podman_calls = read_lines(log, "podman.log")
            self.assertTrue(any("pull" in c for c in podman_calls),
                            "a signed tag must proceed to the pull")
            self.assertIn("refusing to boot an unverified image", result.stderr)

    def test_inconclusive_precheck_warns_and_proceeds(self):
        with tempfile.TemporaryDirectory() as tmp:
            log = make_bin(tmp)
            result = run_script(tmp, log, tag_mode="connection reset by peer",
                                digest_mode="no signatures found")
            self.assertNotEqual(result.returncode, 0)
            self.assertIn("inconclusive", result.stderr)
            podman_calls = read_lines(log, "podman.log")
            self.assertTrue(any("pull" in c for c in podman_calls),
                            "an inconclusive precheck must not block the authoritative check")
            self.assertIn("refusing to boot an unverified image", result.stderr)


if __name__ == "__main__":
    unittest.main()
