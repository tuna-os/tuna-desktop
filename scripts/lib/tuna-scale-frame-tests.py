#!/usr/bin/env python3
import importlib.util
from pathlib import Path
import unittest
from PIL import Image

spec = importlib.util.spec_from_file_location('scale', Path(__file__).with_name('tuna-scale-frame-proof.py'))
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)

class ScaleFrame(unittest.TestCase):
    def setUp(self):
        self.frame = Image.new('RGB', (1280, 800), 'black')
        self.state = {'windows': [{'id': 3, 'app_id': 'tuna-test-gamma', 'rect': [384, 220, 640, 420]}]}
        self.nodes = [{'name': 'Close', 'showing': True, 'bounds': [600, 8, 24, 24]}]
        for x in range(480, 1280):
            self.frame.putpixel((x, 796), (230, 97, 0))
    def test_actual_bottom_pixels_and_close_inside_frame(self):
        self.assertEqual(module.validate(self.frame, self.state, self.nodes)['close_physical_center'], [1245, 300])
    def test_original_offscreen_gamma_rejected(self):
        self.state['windows'][0]['rect'] = [420, 306, 640, 420]
        with self.assertRaisesRegex(ValueError, 'outside actual frame'):
            module.validate(self.frame, self.state, self.nodes)
    def test_desired_geometry_without_actual_bottom_pixels_rejected(self):
        with self.assertRaisesRegex(ValueError, 'marker absent'):
            module.validate(Image.new('RGB', (1280, 800)), self.state, self.nodes)
    def test_offscreen_close_button_rejected(self):
        self.nodes[0]['bounds'][0] = 635
        with self.assertRaisesRegex(ValueError, 'Close button outside'):
            module.validate(self.frame, self.state, self.nodes)

if __name__ == '__main__':
    unittest.main()
