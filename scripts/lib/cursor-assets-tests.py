#!/usr/bin/env python3
"""Cheap data-integrity regressions; native rendering is tested separately."""
import importlib.machinery
import importlib.util
from pathlib import Path
import shutil
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
loader = importlib.machinery.SourceFileLoader("cursor_assets", str(ROOT / "scripts/tuna-cursor-assets-check"))
spec = importlib.util.spec_from_loader(loader.name, loader)
checker = importlib.util.module_from_spec(spec)
loader.exec_module(checker)


class CursorAssetsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name) / "Adwaita"
        shutil.copytree(ROOT / "share/tuna-desktop/icons/Adwaita", self.root, symlinks=True)

    def test_staged_copy_matches_upstream(self):
        self.assertTrue(checker.verify(self.root)["verified"])

    def test_changed_svg_rejected(self):
        path = next(self.root.rglob("*.svg"))
        with path.open("a") as output:
            output.write("<!-- changed -->")
        with self.assertRaises(AssertionError):
            checker.verify(self.root)

    def test_alias_redirect_rejected(self):
        path = self.root / "cursors_scalable/move"
        path.unlink()
        path.symlink_to("wait", target_is_directory=True)
        with self.assertRaises(AssertionError):
            checker.verify(self.root)

    def test_missing_license_rejected(self):
        (self.root / "LICENSE-MIT").unlink()
        with self.assertRaises(AssertionError):
            checker.verify(self.root)


if __name__ == "__main__":
    unittest.main()
