"""Bound failure diagnostics without Cargo, a display, or new captures."""
import hashlib
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
# The CI freeze immediately after these tests requires a clean checkout.
sys.dont_write_bytecode = True
loader = importlib.machinery.SourceFileLoader(
    "minimize_failure_bundle", str(ROOT / "scripts/tuna-minimize-failure-bundle")
)
spec = importlib.util.spec_from_loader(loader.name, loader)
bundle = importlib.util.module_from_spec(spec)
loader.exec_module(bundle)


class FailureBundleTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.source = self.base / "full"
        self.source.mkdir()
        self.output = self.base / "subset"
        self.original = {}
        for relative, _ in bundle.MEMBERS:
            path = self.source / relative
            path.parent.mkdir(exist_ok=True)
            if relative.endswith(".png"):
                data = b"controlled existing image bytes"
            elif relative.endswith("SOURCE_COMMIT"):
                data = b"a" * 40 + b"\n"
            else:
                data = json.dumps({
                    "source_revision": "a" * 40,
                    "proof_sha256": "b" * 64,
                    "binaries": {"tuna-compositor": {
                        "sha256": "c" * 64, "source_path": "/private/path"}},
                    "future_secret": "must not be copied",
                }).encode()
            path.write_bytes(data)
            self.original[relative] = data

    def run_bundle(self, outcome="failure"):
        bundle.bundle(self.source, self.output, outcome)
        if outcome == "failure":
            return json.loads((self.output / "report.json").read_text())

    def status(self, report, name):
        return next(e["status"] for e in report["entries"] if e["source"] == name)

    def test_only_fixed_existing_members_and_sanitized_metadata(self):
        (self.source / "ltr/arbitrary.png").write_bytes(b"private")
        (self.source / "ltr/bin").mkdir()
        (self.source / "ltr/bin/executable").write_bytes(b"large binary")
        report = self.run_bundle()
        self.assertEqual(report["proof_outcome"], "failure")
        files = list(self.output.iterdir())
        self.assertEqual(len(files), bundle.FILE_LIMIT)
        self.assertLessEqual(sum(f.stat().st_size for f in files), bundle.TOTAL_LIMIT)
        self.assertLessEqual((self.output / "report.json").stat().st_size, bundle.REPORT_LIMIT)
        for entry in report["entries"]:
            original = self.original[entry["source"]]
            self.assertEqual((self.source / entry["source"]).read_bytes(), original)
            self.assertEqual(entry["source_bytes"], len(original))
            self.assertEqual(entry["source_sha256"], hashlib.sha256(original).hexdigest())
            copied = (self.output / entry["output"]).read_bytes()
            self.assertEqual(entry["sha256"], hashlib.sha256(copied).hexdigest())
            if entry["source"].endswith(".png"):
                self.assertEqual(copied, original)
            self.assertNotIn(b"private", copied)
            self.assertNotIn(b"future_secret", copied)
        metadata = json.loads((self.output / "ltr--manifest.json").read_text())
        self.assertEqual(metadata["direction"], "ltr")
        self.assertEqual(metadata["binary_sha256"]["tuna-compositor"], "c" * 64)

    def test_success_creates_nothing(self):
        self.run_bundle("success")
        self.assertFalse(self.output.exists())

    def test_missing_members_are_explicit(self):
        name = "rtl/22-interrupted-restore-retained.png"
        (self.source / name).unlink()
        self.assertEqual(self.status(self.run_bundle(), name), "missing")

    def test_symlink_file_is_not_copied(self):
        name = "ltr/21-interrupted-restore-before.png"
        path = self.source / name
        path.unlink()
        path.symlink_to(self.source / "rtl/21-interrupted-restore-before.png")
        self.assertEqual(self.status(self.run_bundle(), name), "unsafe-or-unreadable")

    def test_symlink_subdirectory_is_not_followed(self):
        (self.source / "ltr").rename(self.source / "original-ltr")
        (self.source / "ltr").symlink_to(self.source / "original-ltr", target_is_directory=True)
        report = self.run_bundle()
        self.assertTrue(all(e["status"] == "unsafe-or-unreadable"
                            for e in report["entries"] if e["source"].startswith("ltr/")))

    def test_symlink_source_or_ancestor_is_rejected(self):
        alias = self.base / "alias"
        alias.symlink_to(self.base, target_is_directory=True)
        with self.assertRaises(OSError):
            bundle.bundle(alias / "full", self.output, "failure")
        self.assertFalse(self.output.exists())

    def test_nonregular_fifo_and_directory_are_rejected_without_blocking(self):
        names = ["ltr/21-interrupted-restore-before.png", "ltr/22-interrupted-restore-retained.png"]
        for name in names:
            (self.source / name).unlink()
        os.mkfifo(self.source / names[0])
        (self.source / names[1]).mkdir()
        report = self.run_bundle()
        for name in names:
            self.assertEqual(self.status(report, name), "nonregular")

    def test_oversize_image_and_metadata_are_not_copied(self):
        names = ["ltr/21-interrupted-restore-before.png", "ltr/manifest.json"]
        for name, limit in bundle.MEMBERS:
            if name in names:
                with (self.source / name).open("wb") as stream:
                    stream.truncate(limit + 1)
        report = self.run_bundle()
        for name in names:
            self.assertEqual(self.status(report, name), "oversize")

    def test_malformed_metadata_is_rejected(self):
        name = "ltr/manifest.json"
        (self.source / name).write_text('{"proof_sha256":"not a digest"}')
        self.assertEqual(self.status(self.run_bundle(), name), "invalid-metadata")

    def test_aggregate_and_count_caps_are_enforced(self):
        with patch.object(bundle, "TOTAL_LIMIT", bundle.REPORT_LIMIT + 1):
            report = self.run_bundle()
        self.assertTrue(all(e["status"] == "total-limit" for e in report["entries"]))
        with patch.object(bundle, "FILE_LIMIT", 1):
            with self.assertRaises(ValueError):
                bundle.bundle(self.source, self.base / "count-capped", "failure")
        self.assertFalse((self.base / "count-capped").exists())

    def test_existing_destination_is_never_reused(self):
        self.output.mkdir()
        marker = self.output / "keep"
        marker.write_bytes(b"unchanged")
        with self.assertRaises(FileExistsError):
            self.run_bundle()
        self.assertEqual(list(self.output.iterdir()), [marker])
        self.assertEqual(marker.read_bytes(), b"unchanged")

    def test_destination_cannot_modify_full_artifact(self):
        with self.assertRaises(ValueError):
            bundle.bundle(self.source, self.source / "subset", "failure")
        self.assertFalse((self.source / "subset").exists())

    def test_ci_gates_diagnostics_on_proof_failure_and_preserves_full_upload(self):
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn("id: native_minimize", workflow)
        self.assertIn("if: failure() && steps.native_minimize.outcome == 'failure'", workflow)
        self.assertIn("steps.minimize_failure_bundle.outcome == 'success'", workflow)
        self.assertIn("if: always()\n        with:\n          name: native-minimize-proof\n"
                      "          path: ${{ runner.temp }}/native-minimize/\n          retention-days: 14", workflow)
        self.assertLess(workflow.index("name: native-minimize-proof"),
                        workflow.index("name: Collect bounded native minimize failure diagnostics"))


if __name__ == "__main__":
    unittest.main()
