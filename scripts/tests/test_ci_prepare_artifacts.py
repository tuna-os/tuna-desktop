#!/usr/bin/env python3
"""Run scripts/ci-prepare-artifacts on real trees and check the gtk-shell upload contract.

The script tests use actual FIFOs, sockets, symlinks and executables in a
temporary directory. The workflow test reads ci.yml; it does not upload
anything and does not substitute for the gtk-shell job.
"""
import hashlib
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import unittest

import yaml

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/ci-prepare-artifacts"
WORKFLOW = ROOT / ".github/workflows/ci.yml"


def run(*arguments, cwd):
    # A blocking open of a FIFO would hang here instead of failing.
    return subprocess.run([str(SCRIPT), *arguments], cwd=cwd, text=True, capture_output=True, timeout=30)


class RemoveUnzippableEntries(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.tree = self.root / "artifacts"
        (self.tree / "home/.cache/ibus").mkdir(parents=True)
        (self.tree / "nested.log").write_text("log\n")

    def tearDown(self):
        self.temporary.cleanup()

    def test_fifo_socket_and_dangling_symlink_are_removed_and_named(self):
        os.mkfifo(self.tree / "audio-events")
        listener = socket.socket(socket.AF_UNIX)
        self.addCleanup(listener.close)
        listener.bind(str(self.tree / "home/.cache/ibus/dbus-test"))
        (self.tree / "dangling").symlink_to("missing-target")
        (self.tree / "alias.log").symlink_to("nested.log")

        result = run("artifacts", cwd=self.root)

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        self.assertEqual(sorted(path.relative_to(self.tree).as_posix()
                                for path in self.tree.rglob("*") if not path.is_dir()),
                         ["alias.log", "ci-removed-entries.txt", "nested.log"])
        self.assertEqual(sorted((self.tree / "ci-removed-entries.txt").read_text().splitlines()),
                         ["fifo\taudio-events", "socket\thome/.cache/ibus/dbus-test",
                          "symbolic link\tdangling"])
        self.assertIn("removed 3 non-regular entries from artifacts", result.stdout)

    def test_clean_tree_is_left_alone(self):
        result = run("artifacts", cwd=self.root)
        self.assertEqual((result.returncode, result.stdout, result.stderr), (0, "", ""))
        self.assertEqual([path.name for path in self.tree.iterdir() if path.is_file()], ["nested.log"])

    def test_missing_directory_does_not_stop_the_later_ones(self):
        os.mkfifo(self.tree / "audio-events")
        result = run("never-created", "artifacts", cwd=self.root)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("never-created absent, skipping", result.stdout)
        self.assertFalse((self.tree / "audio-events").exists())


class DetachBulkyEntries(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.tree = self.root / "gtk-shell-artifacts"
        self.files = {
            "bin/tuna-compositor": b"\x7fELF compositor" * 4096,
            "bin/tuna-shell-gtk": b"\x7fELF shell",
            "bin/nested/libgtk4-layer-shell.so.1": b"\x7fELF layer shell",
            "a11y/bin/gtk-tests": b"\x7fELF tests",
        }
        self.evidence = {
            "assertions.txt": b"G-PANEL pass\n",
            "a11y/assertions.txt": b"G-A11Y pass\n",
            "a11y/bin/build-admission.json": b"{}\n",
        }
        for name, data in {**self.files, **self.evidence}.items():
            path = self.tree / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_bytes(data)
        (self.tree / "bin/tuna-compositor").chmod(0o755)

    def tearDown(self):
        self.temporary.cleanup()

    def tree_files(self, base):
        return {path.relative_to(base).as_posix(): path.read_bytes()
                for path in base.rglob("*") if path.is_file()}

    def detach(self, *paths):
        return run("--detach", "gtk-shell-binaries", "gtk-shell-artifacts", *paths, cwd=self.root)

    def test_named_entries_move_and_the_evidence_records_their_hashes(self):
        result = self.detach("bin", "a11y/bin/gtk-tests")

        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stderr, "")
        binaries = self.root / "gtk-shell-binaries"
        self.assertEqual(self.tree_files(binaries), self.files)
        self.assertTrue(os.access(binaries / "bin/tuna-compositor", os.X_OK))
        record = (self.tree / "ci-detached-entries.txt").read_text()
        self.assertEqual(self.tree_files(self.tree),
                         {**self.evidence, "ci-detached-entries.txt": record.encode()})
        # bin (sorted by path), then a11y/bin/gtk-tests: argument order.
        self.assertEqual(record.splitlines(), [
            f"{hashlib.sha256(self.files[name]).hexdigest()}  {name}"
            for name in ("bin/nested/libgtk4-layer-shell.so.1", "bin/tuna-compositor",
                         "bin/tuna-shell-gtk", "a11y/bin/gtk-tests")])
        checked = subprocess.run(["sha256sum", "--check", "--strict", str(self.tree / "ci-detached-entries.txt")],
                                 cwd=binaries, text=True, capture_output=True)
        self.assertEqual(checked.returncode, 0, checked.stdout + checked.stderr)
        self.assertIn("detached 4 files from gtk-shell-artifacts to gtk-shell-binaries", result.stdout)

    def test_absent_entry_is_skipped_and_the_rest_still_moves(self):
        (self.tree / "a11y/bin/gtk-tests").unlink()
        result = self.detach("bin", "a11y/bin/gtk-tests")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("gtk-shell-artifacts/a11y/bin/gtk-tests absent, skipping", result.stdout)
        self.assertFalse((self.tree / "bin").exists())
        self.assertFalse((self.root / "gtk-shell-binaries/a11y").exists())
        self.assertEqual(len((self.tree / "ci-detached-entries.txt").read_text().splitlines()), 3)

    def test_absent_directory_or_entries_create_nothing(self):
        before = self.tree_files(self.tree)
        for arguments in (("--detach", "gtk-shell-binaries", "never-created", "bin"),
                          ("--detach", "gtk-shell-binaries", "gtk-shell-artifacts", "target", "a11y/bin/missing")):
            with self.subTest(arguments=arguments):
                result = run(*arguments, cwd=self.root)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("absent, skipping", result.stdout)
                self.assertFalse((self.root / "gtk-shell-binaries").exists())
                self.assertEqual(self.tree_files(self.tree), before)

    def test_occupied_destination_fails_and_keeps_the_entry_with_the_evidence(self):
        (self.root / "gtk-shell-binaries/bin").mkdir(parents=True)
        (self.root / "gtk-shell-binaries/bin/earlier").write_text("kept\n")
        result = self.detach("bin", "a11y/bin/gtk-tests")
        self.assertEqual(result.returncode, 1)
        self.assertIn("could not detach gtk-shell-artifacts/bin to gtk-shell-binaries/bin", result.stderr)
        self.assertEqual(self.tree_files(self.tree / "bin"),
                         {name.removeprefix("bin/"): data for name, data in self.files.items()
                          if name.startswith("bin/")})
        self.assertEqual(self.tree_files(self.root / "gtk-shell-binaries"),
                         {"bin/earlier": b"kept\n", "a11y/bin/gtk-tests": self.files["a11y/bin/gtk-tests"]})
        self.assertEqual((self.tree / "ci-detached-entries.txt").read_text().splitlines(),
                         [f"{hashlib.sha256(self.files['a11y/bin/gtk-tests']).hexdigest()}  a11y/bin/gtk-tests"])

    def test_paths_outside_the_artifact_directory_and_short_usage_are_rejected(self):
        (self.root / "outside").write_text("not evidence\n")
        before = self.tree_files(self.root)
        for arguments in (("--detach", "gtk-shell-binaries", "gtk-shell-artifacts", "bin", "../outside"),
                          ("--detach", "gtk-shell-binaries", "gtk-shell-artifacts", str(self.root / "outside")),
                          ("--detach", "gtk-shell-binaries", "gtk-shell-artifacts", "a11y/../../outside"),
                          ("--detach", "gtk-shell-binaries", "gtk-shell-artifacts", "-delete"),
                          ("--detach", "gtk-shell-binaries", "gtk-shell-artifacts", ""),
                          ("--detach", "gtk-shell-binaries", "gtk-shell-artifacts"),
                          ("--detach",)):
            with self.subTest(arguments=arguments):
                result = run(*arguments, cwd=self.root)
                self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
                self.assertEqual(self.tree_files(self.root), before)


class GtkShellUploadContract(unittest.TestCase):
    """#1063: the required evidence must not wait behind the executables."""

    def setUp(self):
        self.steps = yaml.safe_load(WORKFLOW.read_text())["jobs"]["gtk-shell"]["steps"]
        self.job = yaml.safe_load(WORKFLOW.read_text())["jobs"]["gtk-shell"]

    def upload(self, name):
        found = [(index, step) for index, step in enumerate(self.steps)
                 if "upload-artifact" in step.get("uses", "") and step["with"]["name"] == name]
        self.assertEqual(len(found), 1, name)
        return found[0]

    def test_evidence_is_prepared_detached_then_uploaded_before_the_executables(self):
        prepared = [index for index, step in enumerate(self.steps)
                    if "scripts/ci-prepare-artifacts" in step.get("run", "")]
        self.assertEqual(len(prepared), 1)
        prepare = self.steps[prepared[0]]
        self.assertEqual(prepare["if"], "always()")
        self.assertEqual(prepare["run"].split("\n"), [
            "scripts/ci-prepare-artifacts gtk-shell-artifacts",
            "scripts/ci-prepare-artifacts --detach gtk-shell-binaries gtk-shell-artifacts bin a11y/bin/gtk-tests",
            ""])
        evidence_index, evidence = self.upload("gtk-shell")
        binaries_index, binaries = self.upload("gtk-shell-binaries")
        self.assertEqual([evidence_index, binaries_index], [prepared[0] + 1, prepared[0] + 2])
        self.assertEqual(binaries_index, len(self.steps) - 1)
        self.assertEqual(evidence["with"]["path"], "gtk-shell-artifacts/")
        self.assertEqual(binaries["with"]["path"], "gtk-shell-binaries/")

    def test_both_uploads_are_required_and_bounded_inside_the_job_deadline(self):
        _, evidence = self.upload("gtk-shell")
        _, binaries = self.upload("gtk-shell-binaries")
        for step in (evidence, binaries):
            self.assertEqual(step["if"], "always()")
            self.assertEqual(step["with"]["if-no-files-found"], "error")
            self.assertNotIn("continue-on-error", step)
        self.assertEqual((evidence["timeout-minutes"], binaries["timeout-minutes"]), (5, 10))
        self.assertLess(evidence["timeout-minutes"] + binaries["timeout-minutes"], self.job["timeout-minutes"])

    def test_the_parity_ledger_still_reads_the_evidence_upload(self):
        _, evidence = self.upload("gtk-shell")
        self.assertEqual(evidence["id"], "gtk_artifact")
        self.assertEqual(self.job["outputs"]["proof_artifact_id"], "${{ steps.gtk_artifact.outputs.artifact-id }}")
        # The executables the proofs ran from are still produced inside the evidence tree.
        produced = "\n".join(step.get("run", "") for step in self.steps)
        self.assertIn("gtk-shell-artifacts/a11y/bin/gtk-tests", produced)
        self.assertIn("mkdir -p gtk-shell-artifacts/bin", produced)


if __name__ == "__main__":
    unittest.main()
