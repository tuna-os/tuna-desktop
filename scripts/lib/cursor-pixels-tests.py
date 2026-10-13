#!/usr/bin/env python3
import importlib.util
from pathlib import Path
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


if __name__ == "__main__":
    unittest.main()
