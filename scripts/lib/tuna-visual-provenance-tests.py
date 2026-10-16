#!/usr/bin/env python3
"""Provenance pins the actual built tree, never the event payload.

Covers `tuna-visual-capture --provenance`: the recorded built head/tree
must equal the real checkout's commit/tree, and the event SHA stays a
separate informational field (the merge ref is not presented as tested).
"""
import json
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "tuna-visual-capture"


def git(*args):
    return subprocess.run(["git", "-C", str(ROOT), *args],
                          capture_output=True, text=True).stdout.strip()


class ProvenanceCase(unittest.TestCase):
    def test_recorded_tree_equals_real_tree(self):
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run(
                [str(SCRIPT), "--provenance", "--artifacts", directory],
                capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            manifest = json.loads((Path(directory) / "provenance.json").read_text())
            source = manifest["source"]
            self.assertEqual(source["built_head_sha"], git("rev-parse", "HEAD"))
            self.assertEqual(source["built_tree_sha"], git("rev-parse", "HEAD^{tree}"))
            self.assertIn("tree_diff_sha256", source)
            # The event SHA is informational and independent of the build.
            self.assertIn("event_sha", source)
            self.assertNotIn("merge_sha", source)

    def test_binaries_recorded(self):
        with tempfile.TemporaryDirectory() as directory:
            result = subprocess.run(
                [str(SCRIPT), "--provenance", "--artifacts", directory],
                capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stderr)
            manifest = json.loads((Path(directory) / "provenance.json").read_text())
            binaries = manifest["binaries"]
            self.assertIn("tuna-compositor", binaries)
            self.assertIn("tuna-shell-gtk", binaries)
            self.assertIn("tuna-ibus-bridge", binaries)


if __name__ == "__main__":
    unittest.main()
