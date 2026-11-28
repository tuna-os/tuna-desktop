#!/usr/bin/env python3
import importlib.util
import json
from pathlib import Path
import re
import subprocess
import unittest
from PIL import Image

spec = importlib.util.spec_from_file_location("cursor_pixels", Path(__file__).with_name("cursor-pixels.py"))
proof = importlib.util.module_from_spec(spec)
spec.loader.exec_module(proof)


class CursorPixelsTests(unittest.TestCase):
    def image(self, side=24):
        image = Image.new("RGB", (100, 100), (143, 63, 208))
        image.paste((49, 95, 199), (46, 44, 46 + side, 44 + side))
        return image

    def test_actual_bounds_and_hotspot(self):
        receipt = proof.verify(self.image(), 50, 50, 24, [4, 6], [49, 95, 199])
        self.assertEqual(receipt["matching_pixels"], 576)

    def test_cropped_image_rejected(self):
        with self.assertRaises(AssertionError):
            proof.verify(self.image(23), 50, 50, 24, [4, 6], [49, 95, 199])

    def test_wrong_hotspot_rejected(self):
        with self.assertRaises(AssertionError):
            proof.verify(self.image(), 50, 50, 24, [3, 6], [49, 95, 199])

    def test_wrong_color_rejected(self):
        with self.assertRaises(AssertionError):
            proof.verify(self.image(), 50, 50, 24, [4, 6], [25, 173, 125])


class FractionalScaleStateTests(unittest.TestCase):
    """Use the proof's own predicates, not a second copy of its contract."""

    @classmethod
    def setUpClass(cls):
        script = Path(__file__).resolve().parents[1] / "tuna-gtk-shell-proof"
        source = script.read_text()
        cls.filters = {}
        for label in ("48px image at 125%", "scalable cursor at 125%"):
            matches = re.findall(r"^cstate '([^']+)' \"" + re.escape(label) + r"\"",
                                 source, re.MULTILINE)
            if len(matches) != 1:
                raise AssertionError(f"expected one cursor-scale predicate for {label}")
            cls.filters[label] = matches[0]

    def accepted(self, label, **changes):
        legacy = label == "48px image at 125%"
        cursor = {
            "name": "default",
            "source": "client",
            "scale": 1.25,
            "image_theme": "TunaProofBase" if legacy else "TunaProofSvg",
            "image": [48, 48] if legacy else [30, 30],
            "logical": [24.0, 24.0],
        }
        cursor.update(changes)
        result = subprocess.run(["jq", "-e", self.filters[label]],
                                input=json.dumps({"cursor": cursor}), text=True,
                                capture_output=True, timeout=5)
        self.assertIn(result.returncode, (0, 1), result.stderr)
        return result.returncode == 0

    def test_legacy_loads_ceiled_scale_but_svg_rasterizes_physical_size(self):
        # CI 41175449190 reached the SVG state [30,30]/[24,24] at 1.25
        # after the legacy's actual 30px pixel check. Expecting [48,48]
        # for that SVG prevented the fractional ripple proof from running.
        for label in self.filters:
            with self.subTest(label=label):
                self.assertTrue(self.accepted(label))

    def test_wrong_raster_sizes_are_still_rejected(self):
        for label, sizes in (
                ("48px image at 125%", ([24, 24], [30, 30])),
                ("scalable cursor at 125%", ([24, 24], [48, 48], [30, 29]))):
            for size in sizes:
                with self.subTest(label=label, image=size):
                    self.assertFalse(self.accepted(label, image=size))

    def test_theme_and_logical_size_remain_required(self):
        for label in self.filters:
            for changes in ({"image_theme": "Adwaita"}, {"logical": [30, 30]}):
                with self.subTest(label=label, changes=changes):
                    self.assertFalse(self.accepted(label, **changes))


if __name__ == "__main__":
    unittest.main()
