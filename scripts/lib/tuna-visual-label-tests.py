#!/usr/bin/env python3
"""Frame labeling: honest labels and fail-closed comparison.

Covers scripts/lib/tuna-visual-label.py: baseline/feature/
indistinguishable/duplicate labels at the documented threshold, and the
--compare exit contract (0 differs, 1 same, 2 cannot compare).
"""
import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

try:
    from PIL import Image

    HAVE_PIL = True
except ImportError:
    HAVE_PIL = False

ROOT = Path(__file__).resolve().parents[2]
LABEL = ROOT / "scripts" / "lib" / "tuna-visual-label.py"


def solid(path, color, size=(1280, 800)):
    Image.new("RGB", size, color).save(path)


@unittest.skipUnless(HAVE_PIL, "needs PIL")
class LabelCase(unittest.TestCase):
    def label_dir(self, root, paint=None, extra=()):
        directory = Path(root)
        solid(directory / "00-baseline-desktop.png", (2, 60, 136))
        # Same scene, different bytes, sub-threshold pixels: not a dup.
        near = Image.new("RGB", (1280, 800), (2, 60, 136))
        near.putpixel((0, 0), (3, 61, 137))
        near.save(directory / "wallpaper-only.png")
        panel = Image.new("RGB", (1280, 800), (2, 60, 136))
        pixels = panel.load()
        for x in range(1280):
            for y in range(32):
                pixels[x, y] = (0, 0, 0)
        panel.save(directory / "with-panel.png")
        if paint:
            paint(directory)
        rows = []
        for name in ("00-baseline-desktop.png", "wallpaper-only.png",
                     "with-panel.png", *extra):
            blob = (directory / name).read_bytes()
            rows.append({"file": name, "state": "s", "epoch_ms": 1,
                         "sha256": hashlib.sha256(blob).hexdigest()})
        (directory / "frames.jsonl").write_text(
            "\n".join(json.dumps(row) for row in rows) + "\n", encoding="utf-8")
        result = subprocess.run(
            [sys.executable, str(LABEL), "--dir", str(directory),
             "--baseline", "00-baseline-desktop.png"],
            capture_output=True, text=True, timeout=60)
        self.assertEqual(result.returncode, 0, result.stderr)
        return {entry["file"]: entry
                for entry in json.loads((directory / "frames.json").read_text())["frames"]}

    def test_wallpaper_only_is_not_feature_proof(self):
        with tempfile.TemporaryDirectory() as directory:
            labels = self.label_dir(directory)
            self.assertEqual(labels["00-baseline-desktop.png"]["label"], "baseline")
            self.assertEqual(labels["wallpaper-only.png"]["label"],
                             "indistinguishable-from-baseline")
            self.assertEqual(labels["with-panel.png"]["label"], "feature")
            self.assertGreaterEqual(labels["with-panel.png"]["diff_pct"], 0.2)
            self.assertLess(labels["wallpaper-only.png"]["diff_pct"], 0.2)

    def test_byte_identical_frame_is_duplicate(self):
        def paint(directory):
            blob = (directory / "with-panel.png").read_bytes()
            (directory / "repeat.png").write_bytes(blob)

        with tempfile.TemporaryDirectory() as directory:
            labels = self.label_dir(directory, paint=paint, extra=("repeat.png",))
            self.assertEqual(labels["repeat.png"]["label"], "duplicate")
            self.assertEqual(labels["repeat.png"]["duplicate_of"], "with-panel.png")

    def test_compare_differs_and_same(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            solid(directory / "a.png", (2, 60, 136))
            changed = Image.new("RGB", (1280, 800), (2, 60, 136))
            pixels = changed.load()
            for x in range(1280):
                for y in range(32):
                    pixels[x, y] = (0, 0, 0)
            changed.save(directory / "b.png")
            same = Image.new("RGB", (1280, 800), (2, 60, 136))
            same.save(directory / "c.png")
            differs = subprocess.run(
                [sys.executable, str(LABEL), "--compare", str(directory / "a.png"),
                 str(directory / "b.png")], capture_output=True, text=True, timeout=60)
            self.assertEqual(differs.returncode, 0, differs.stderr)
            same_result = subprocess.run(
                [sys.executable, str(LABEL), "--compare", str(directory / "a.png"),
                 str(directory / "c.png")], capture_output=True, text=True, timeout=60)
            self.assertEqual(same_result.returncode, 1)

    def test_compare_fails_closed(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            solid(directory / "a.png", (2, 60, 136))
            solid(directory / "small.png", (2, 60, 136), size=(64, 64))
            missing = subprocess.run(
                [sys.executable, str(LABEL), "--compare", str(directory / "a.png"),
                 str(directory / "nope.png")], capture_output=True, text=True, timeout=60)
            self.assertEqual(missing.returncode, 2)
            mismatch = subprocess.run(
                [sys.executable, str(LABEL), "--compare", str(directory / "a.png"),
                 str(directory / "small.png")], capture_output=True, text=True, timeout=60)
            self.assertEqual(mismatch.returncode, 2)


if __name__ == "__main__":
    unittest.main()
