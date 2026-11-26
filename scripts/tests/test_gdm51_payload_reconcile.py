"""The gdm51 overlay may update exactly one shipped-payload entry.

The Marlin VM lane takes its shipped-payload receipt from whatever `gdm` the
repository carries, then installs a fixed native GDM candidate. Their
gdm.service units can differ for benign upstream reasons (gdm 51.2-1 added an
ordering line), which used to fail the whole lane. The overlay now accepts a
mismatch on that one unit when the installed bytes are the candidate archive's
own copy, and still refuses anything else (PAM files, session files, a second
mismatch, or a unit that is not the candidate's). The overlay's Python is
embedded in a Containerfile heredoc, so this extracts and runs the helper."""
import hashlib
import os
import pathlib
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parents[2]
CONTAINERFILE = ROOT / "packaging/marlin/vm-lane/gdm51/Containerfile"
UNIT_BEFORE = b"[Unit]\nAfter=rc-local.service\n"
UNIT_AFTER = b"[Unit]\nAfter=rc-local.service\nAfter=accounts-daemon.service\n"


def helpers():
    """Run the overlay's helper block (require, command, the unit constant and
    reconcile_candidate_unit) from the Containerfile heredoc in a namespace."""
    body = CONTAINERFILE.read_text().split("python3 - <<'PY'\n", 1)[1].split("\nPY\n", 1)[0]
    block = body[body.index("def require("):body.index("manifest, manifest_bytes")]
    namespace = {"__name__": "gdm51_overlay"}
    exec("import difflib, hashlib, json, os, re, stat, subprocess\nfrom pathlib import Path\n" + block, namespace)
    return namespace


def sha(data):
    return hashlib.sha256(data).hexdigest()


class Reconcile(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = pathlib.Path(self.tmp.name)
        self.ns = helpers()
        self.unit = self.root / "gdm.service"
        self.other = self.root / "gdm-password"
        self.other.write_bytes(b"auth required pam_unix.so\n")
        self.archive = self.root / "gdm.pkg"
        self.archive.write_bytes(b"x")
        self.ns["CANDIDATE_OWNED_UNIT"] = str(self.unit)
        shim = self.root / "bin"
        shim.mkdir()
        (shim / "bsdtar").write_text(
            "#!/bin/sh\n# test shim: -xOf <archive> <member> -> prints the candidate unit\n"
            f"cat {self.root}/candidate-unit\n")
        (shim / "bsdtar").chmod(0o755)
        old = os.environ["PATH"]
        os.environ["PATH"] = f"{shim}:{old}"
        self.addCleanup(lambda: os.environ.__setitem__("PATH", old))
        (self.root / "candidate-unit").write_bytes(UNIT_AFTER)

    def receipt(self, unit_digest):
        path = self.root / "receipt"
        path.write_text(f"{unit_digest}  {self.unit}\n{sha(self.other.read_bytes())}  {self.other}\n")
        return path

    def run_reconcile(self, receipt):
        return self.ns["reconcile_candidate_unit"](receipt, str(self.archive), UNIT_BEFORE)

    def test_unchanged_payload_needs_no_update(self):
        self.unit.write_bytes(UNIT_BEFORE)
        receipt = self.receipt(sha(UNIT_BEFORE))
        self.assertIsNone(self.run_reconcile(receipt))

    def test_candidate_owned_unit_is_the_only_entry_updated(self):
        self.unit.write_bytes(UNIT_AFTER)
        receipt = self.receipt(sha(UNIT_BEFORE))
        before_other = f"{sha(self.other.read_bytes())}  {self.other}"
        update = self.run_reconcile(receipt)
        self.assertEqual(update["before_sha256"], sha(UNIT_BEFORE))
        self.assertEqual(update["after_sha256"], sha(UNIT_AFTER))
        self.assertIn("+After=accounts-daemon.service", update["diff"])
        lines = receipt.read_text().splitlines()
        self.assertIn(f"{sha(UNIT_AFTER)}  {self.unit}", lines)
        self.assertIn(before_other, lines)

    def test_unit_that_is_not_the_candidate_copy_is_refused(self):
        self.unit.write_bytes(b"[Unit]\nExecStart=/bin/evil\n")
        receipt = self.receipt(sha(UNIT_BEFORE))
        with self.assertRaisesRegex(SystemExit, "not the candidate archive copy"):
            self.run_reconcile(receipt)

    def test_any_other_payload_mismatch_is_refused(self):
        self.unit.write_bytes(UNIT_AFTER)
        receipt = self.receipt(sha(UNIT_BEFORE))
        self.other.write_bytes(b"auth sufficient pam_permit.so\n")
        with self.assertRaisesRegex(SystemExit, "beyond the candidate-owned gdm.service"):
            self.run_reconcile(receipt)

    def test_other_mismatch_alone_is_refused(self):
        self.unit.write_bytes(UNIT_BEFORE)
        receipt = self.receipt(sha(UNIT_BEFORE))
        self.other.write_bytes(b"auth sufficient pam_permit.so\n")
        with self.assertRaisesRegex(SystemExit, "beyond the candidate-owned gdm.service"):
            self.run_reconcile(receipt)

    def test_receipt_entry_must_be_the_pre_install_unit(self):
        self.unit.write_bytes(UNIT_AFTER)
        receipt = self.receipt(sha(b"something else"))
        with self.assertRaisesRegex(SystemExit, "not the pre-install gdm.service"):
            self.run_reconcile(receipt)


if __name__ == "__main__":
    unittest.main()
