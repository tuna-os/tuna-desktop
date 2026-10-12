"""Negative controls for the actual native proof's parent pixel checker."""
import ast
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
SOURCE = ast.parse((ROOT / "scripts/tuna-minimize-proof").read_text())
CHECK = next(node for node in ast.walk(SOURCE)
             if isinstance(node, ast.FunctionDef) and node.name == "check_pixel")


class FixtureImage:
    def __init__(self, pixel):
        self.pixel = pixel

    def getpixel(self, _point):
        return self.pixel


class BrightnessControlTests(unittest.TestCase):
    def setUp(self):
        namespace = {"baseline": FixtureImage((10, 20, 30))}
        exec(compile(ast.Module(body=[CHECK], type_ignores=[]),
                     "actual-minimize-proof-checker", "exec"), namespace)
        self.check = namespace["check_pixel"]
        self.rect = [100, 100, 500, 350]
        self.normal = FixtureImage((52, 101, 164))
        multiplier = 88 / 127
        self.multiplier = multiplier
        self.dim = FixtureImage(tuple(round(channel * multiplier)
                                      for channel in self.normal.pixel))

    def test_reference_dim_and_unset_modal_brightness_match(self):
        self.check(self.dim, self.rect, 1.0, self.multiplier)
        self.check(self.normal, self.rect, 1.0, 1.0)

    def test_undimmed_parent_cannot_pass_hidden_modal_dim_check(self):
        with self.assertRaises(AssertionError):
            self.check(self.normal, self.rect, 1.0, self.multiplier)

    def test_dimmed_parent_cannot_pass_unset_modal_negative_control(self):
        with self.assertRaises(AssertionError):
            self.check(self.dim, self.rect, 1.0, 1.0)


if __name__ == "__main__":
    unittest.main()
