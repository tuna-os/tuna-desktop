#!/usr/bin/env python3
"""Walkthrough freshness: refresh only from exact successful artifacts.

Covers the new `refresh`/`freshness` behavior of scripts/tuna-walkthrough:
refusing partial or mixed-source bundles and non-proof frames, writing a
per-frame manifest with source IDs, and reporting CURRENT/STALE/MISSING
without ever labeling existing images current automatically.
"""
import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts" / "tuna-walkthrough"
MANIFEST_TSV = ROOT / "docs" / "walkthrough.tsv"
VISUAL_TSV = ROOT / "docs" / "walkthrough-visual.tsv"
RUN_ID = "38240110439"
MERGE_SHA = "7b6c85922af14f4c239a4f958262b657adc6b03f"
HEAD_SHA = "9dc2efb27f20087301abf3c6dbc65fe02294cfcb"


def tsv_ids(path):
    ids = []
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            if line.strip() and not line.startswith("#"):
                ids.append(line.rstrip("\n").split("\t")[0])
    return ids


def visual_map():
    mapping = {}
    with open(VISUAL_TSV, encoding="utf-8") as handle:
        for line in handle:
            if line.strip() and not line.startswith("#"):
                ident, stage, frame = line.rstrip("\n").split("\t")[:3]
                mapping[ident] = (stage, frame)
    return mapping


class FreshnessCase(unittest.TestCase):
    def make_bundle(self, root, *, statuses=None, labels=None, attempts=None,
                    run_ids=None, heads=None):
        """Fabricate a visual-* bundle dir with one frame per mapped id."""
        mapping = visual_map()
        statuses = statuses or {}
        labels = labels or {}
        attempts = attempts or {}
        run_ids = run_ids or {}
        heads = heads or {}
        bundle = Path(root) / "bundle"
        stages = sorted({stage for stage, _ in mapping.values()})
        for stage in stages:
            stagedir = bundle / ("visual-" + stage)
            stagedir.mkdir(parents=True)
            (stagedir / "stage.json").write_text(json.dumps({
                "stage": stage,
                "status": statuses.get(stage, "ok"),
                "finished_utc": "2026-10-12T11:00:00Z",
            }), encoding="utf-8")
            (stagedir / "provenance.json").write_text(json.dumps({
                "source": {"head_sha": heads.get(stage, HEAD_SHA),
                           "diff_sha256": "tree" + HEAD_SHA[:8]},
                "packages": {"xvfb": "2:21.1.4-2ubuntu2"},
                "image": {"github_run_id": run_ids.get(stage, RUN_ID),
                          "github_run_attempt": attempts.get(stage, "1"),
                          "github_ref": "refs/heads/w3-visual-capture"},
            }), encoding="utf-8")
            entries = []
            for ident, (mapped_stage, frame) in mapping.items():
                if mapped_stage != stage:
                    continue
                blob = b"PNG:" + ident.encode()
                (stagedir / frame).write_bytes(blob)
                entries.append({"file": frame, "sha256": hashlib.sha256(blob).hexdigest()})
            (stagedir / "frames.json").write_text(
                json.dumps({"frames": [
                    dict(e, label=labels.get(e["file"], "feature")) for e in entries
                ]}), encoding="utf-8")
        return bundle

    def refresh(self, bundle, dest, *extra):
        return subprocess.run(
            [str(SCRIPT), "refresh", str(bundle), "--dest", str(dest),
             "--run-id", RUN_ID, "--merge-sha", MERGE_SHA, *extra],
            capture_output=True, text=True)

    def freshness(self, dest):
        return subprocess.run(
            [str(SCRIPT), "freshness", "--dest", str(dest)],
            capture_output=True, text=True)

    def test_visual_map_covers_every_walkthrough_id(self):
        want = tsv_ids(MANIFEST_TSV)
        self.assertEqual(len(want), 46)
        mapping = visual_map()
        self.assertEqual(sorted(mapping), sorted(want))

    def test_refresh_copies_exact_bytes_and_manifest(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = self.make_bundle(directory)
            dest = Path(directory) / "dest"
            result = self.refresh(bundle, dest, "--branch", "w3-visual-capture")
            self.assertEqual(result.returncode, 0, result.stderr)
            manifest = json.loads((dest / "walkthrough" / "manifest.json").read_text())
            self.assertEqual(manifest["run_id"], RUN_ID)
            self.assertEqual(manifest["merge_sha"], MERGE_SHA)
            self.assertEqual(manifest["branch"], "w3-visual-capture")
            self.assertEqual(manifest["source"]["head_sha"], HEAD_SHA)
            self.assertIn("xvfb", manifest["source"]["packages"])
            self.assertEqual(len(manifest["frames"]), 46)
            for ident in tsv_ids(MANIFEST_TSV):
                entry = manifest["frames"][ident]
                blob = (dest / "walkthrough" / (ident + ".png")).read_bytes()
                self.assertEqual(entry["sha256"], hashlib.sha256(blob).hexdigest())
                self.assertEqual(entry["capture"], "ok")
                self.assertEqual(entry["artifact"], "visual-" + entry["stage"])
            self.assertTrue((dest / "walkthrough.md").exists())

    def test_refresh_refuses_partial_stage(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = self.make_bundle(directory, statuses={"input": "partial"})
            dest = Path(directory) / "dest"
            result = self.refresh(bundle, dest)
            self.assertEqual(result.returncode, 1)
            self.assertIn("not ok", result.stderr)
            self.assertIn("partial run", result.stderr)
            self.assertFalse((dest / "walkthrough" / "manifest.json").exists())

    def test_refresh_refuses_mixed_attempts(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = self.make_bundle(directory, attempts={"input": "2"})
            dest = Path(directory) / "dest"
            result = self.refresh(bundle, dest)
            self.assertEqual(result.returncode, 1)
            self.assertIn("mixed attempts", result.stderr)

    def test_refresh_refuses_wrong_run_id(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = self.make_bundle(directory, run_ids={"panel": "111"})
            dest = Path(directory) / "dest"
            result = self.refresh(bundle, dest)
            self.assertEqual(result.returncode, 1)
            self.assertIn(RUN_ID, result.stderr)

    def test_refresh_refuses_indistinguishable_frame(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = self.make_bundle(directory, labels={"qs-grid.png": "indistinguishable-from-baseline"})
            dest = Path(directory) / "dest"
            result = self.refresh(bundle, dest)
            self.assertEqual(result.returncode, 1)
            self.assertIn("quick-settings", result.stderr)
            self.assertIn("not feature proof", result.stderr)

    def test_refresh_resolves_duplicate_to_original(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = self.make_bundle(directory)
            stagedir = bundle / "visual-panel"
            frames = json.loads((stagedir / "frames.json").read_text())
            for entry in frames["frames"]:
                if entry["file"] == "panel-desktop.png":
                    entry["label"] = "duplicate"
                    entry["duplicate_of"] = "00-baseline-desktop.png"
            (stagedir / "frames.json").write_text(json.dumps(frames))
            (stagedir / "00-baseline-desktop.png").write_bytes(b"PNG:baseline")
            dest = Path(directory) / "dest"
            result = self.refresh(bundle, dest)
            self.assertEqual(result.returncode, 0, result.stderr)
            manifest = json.loads((dest / "walkthrough" / "manifest.json").read_text())
            self.assertEqual(manifest["frames"]["panel"]["resolved_from"], "00-baseline-desktop.png")
            self.assertEqual((dest / "walkthrough" / "panel.png").read_bytes(), b"PNG:baseline")

    def test_freshness_current_after_refresh(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = self.make_bundle(directory)
            dest = Path(directory) / "dest"
            self.assertEqual(self.refresh(bundle, dest).returncode, 0)
            result = self.freshness(dest)
            self.assertEqual(result.returncode, 0, result.stdout)
            self.assertIn("freshness 46/46 current", result.stdout)

    def test_freshness_flags_edited_image_stale(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = self.make_bundle(directory)
            dest = Path(directory) / "dest"
            self.assertEqual(self.refresh(bundle, dest).returncode, 0)
            (dest / "walkthrough" / "panel.png").write_bytes(b"tampered")
            result = self.freshness(dest)
            self.assertEqual(result.returncode, 1)
            self.assertIn("STALE   panel", result.stdout)
            self.assertIn("CURRENT calendar", result.stdout)

    def test_freshness_flags_missing_image(self):
        with tempfile.TemporaryDirectory() as directory:
            bundle = self.make_bundle(directory)
            dest = Path(directory) / "dest"
            self.assertEqual(self.refresh(bundle, dest).returncode, 0)
            (dest / "walkthrough" / "panel.png").unlink()
            result = self.freshness(dest)
            self.assertEqual(result.returncode, 1)
            self.assertIn("MISSING panel", result.stdout)

    def test_freshness_without_manifest_is_all_stale(self):
        with tempfile.TemporaryDirectory() as directory:
            dest = Path(directory) / "dest"
            (dest / "walkthrough").mkdir(parents=True)
            (dest / "walkthrough" / "panel.png").write_bytes(b"PNG:panel")
            result = self.freshness(dest)
            self.assertEqual(result.returncode, 1)
            self.assertIn("every frame is STALE", result.stdout)
            self.assertNotIn("CURRENT", result.stdout)

    def test_existing_check_ignores_freshness(self):
        # Pin the gate-safe contract: `check` still validates only the
        # markdown text and image existence, so a bogus manifest cannot
        # change its verdict.
        manifest_path = ROOT / "docs" / "walkthrough" / "manifest.json"
        had_manifest = manifest_path.exists()
        saved = manifest_path.read_bytes() if had_manifest else None
        try:
            manifest_path.write_bytes(b"not json")
            check = subprocess.run(
                [str(SCRIPT), "check"], capture_output=True, text=True)
            self.assertEqual(check.returncode, 0, check.stderr)
        finally:
            if saved is None:
                manifest_path.unlink(missing_ok=True)
            else:
                manifest_path.write_bytes(saved)


if __name__ == "__main__":
    unittest.main()
