#!/usr/bin/env python3
import importlib.util
import sys

# Helper imports must preserve the admitted source tree throughout the proof.
sys.dont_write_bytecode = True
from pathlib import Path
import unittest
from PIL import Image

spec = importlib.util.spec_from_file_location('wellbeing_pixels', Path(__file__).with_name('wellbeing-pixels.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class PixelControls(unittest.TestCase):
    def fixtures(self, desaturation, brightness):
        before = Image.new('RGB', (12, 4))
        regions = [{'name': name, 'kind': kind, 'rect': [index * 4, 0, 4, 4]} for index, (name, kind) in enumerate([('window', 'texture'), ('wallpaper-clear', 'solid'), ('client-cursor', 'cursor')])]
        after = before.copy()
        for region, color in zip(regions, [(224, 32, 64), (32, 160, 80), (208, 64, 16)]):
            for y in range(4):
                for x in range(region['rect'][0], region['rect'][0] + 4):
                    before.putpixel((x, y), color)
                    transformed = color if region['kind'] == 'cursor' else tuple(round(c) for c in module.transform(color, desaturation, brightness))
                    after.putpixel((x, y), transformed)
        return before, after, regions

    def test_ramp_end_off_and_independent_effects(self):
        for desaturation, brightness in [(0, 1), (.75, .7), (1, .6), (1, 1), (0, .6)]:
            module.verify(*self.fixtures(desaturation, brightness), desaturation, brightness)

    def test_missing_solid_effect_fails(self):
        before, after, regions = self.fixtures(1, .6)
        after.paste(before.crop((4, 0, 8, 4)), (4, 0))
        with self.assertRaises(AssertionError):
            module.verify(before, after, regions, 1, .6)

    def test_cursor_effect_leak_fails(self):
        before, after, regions = self.fixtures(1, .6)
        after.paste((40, 40, 40), (8, 0, 12, 4))
        with self.assertRaises(AssertionError):
            module.verify(before, after, regions, 1, .6)

    def test_diagnostic_only_or_neutral_fixture_fails(self):
        before, after, regions = self.fixtures(1, .6)
        with self.assertRaises(AssertionError):
            module.verify(before, after, regions[:1], 1, .6)
        before.paste((80, 80, 80), (0, 0, 4, 4))
        after.paste((48, 48, 48), (0, 0, 4, 4))
        with self.assertRaises(AssertionError):
            module.verify(before, after, regions, 1, .6)


if __name__ == '__main__':
    unittest.main()
