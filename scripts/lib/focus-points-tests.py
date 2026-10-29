#!/usr/bin/env python3
"""Unit tests for tuna-focus-points.py, the focus-policy proof's targeting."""
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

spec = importlib.util.spec_from_file_location(
    "focus_points", Path(__file__).with_name("tuna-focus-points.py")
)
focus_points = importlib.util.module_from_spec(spec)
spec.loader.exec_module(focus_points)


def state(**overrides):
    doc = {
        "active_workspace": 1,
        "focused": 2,
        "minimized": [],
        "stacking": [1, 2],
        "windows": [
            {"id": 1, "workspace": 1, "rect": [100, 100, 400, 300]},
            {"id": 2, "workspace": 1, "rect": [150, 150, 400, 300]},
        ],
        "layers": [
            {"namespace": "tuna-panel", "rect": [0, 0, 1280, 32]},
            {"namespace": "tuna-shell-overview", "rect": [0, 0, 1280, 800]},
        ],
    }
    doc.update(overrides)
    return doc


def run(doc, *args):
    with tempfile.NamedTemporaryFile("w", suffix=".json") as file:
        json.dump(doc, file)
        file.flush()
        printed = []
        original = print
        focus_points.print = lambda *values: printed.append(" ".join(map(str, values)))
        try:
            status = focus_points.main(["tuna-focus-points.py", file.name, *args])
        finally:
            focus_points.print = original
        return status, printed


class FocusPoints(unittest.TestCase):
    def test_beneath_finds_a_point_where_the_lower_window_is_topmost(self):
        status, printed = run(state(), "beneath", "1", "2")
        self.assertEqual(status, 0)
        window, x, y = map(int, printed[0].split())
        self.assertEqual(window, 1)
        self.assertTrue(100 + 24 <= x < 150 or 100 + 60 <= y < 150)
        self.assertFalse(150 <= x < 550 and 150 <= y < 450, "inside the upper window")

    def test_beneath_skips_the_focused_the_top_and_unlisted_windows(self):
        self.assertEqual(run(state(focused=1), "beneath", "1", "2")[0], 1)
        self.assertEqual(run(state(), "beneath", "2")[0], 1)

    def test_visible_bottom_right_third_stays_inside_that_third(self):
        status, printed = run(state(), "visible", "2", "br")
        self.assertEqual(status, 0)
        x, y = map(int, printed[0].split())
        self.assertGreater(x, 150 + 2 * 400 // 3)
        self.assertGreater(y, 150 + 2 * 300 // 3)

    def test_desktop_avoids_windows_and_the_panel_but_not_full_output_chrome(self):
        status, printed = run(state(), "desktop", "1280", "800")
        self.assertEqual(status, 0)
        x, y = map(int, printed[0].split())
        for rect in ([100, 100, 400, 300], [150, 150, 400, 300], [0, 0, 1280, 32]):
            self.assertFalse(focus_points.inside(rect, x, y))

    def test_other_workspaces_and_minimized_windows_do_not_occlude(self):
        doc = state(minimized=[2])
        self.assertEqual(run(doc, "beneath", "1", "2")[0], 1, "1 is now on top")
        doc = state(stacking=[1, 2, 3])
        doc["windows"].append({"id": 3, "workspace": 2, "rect": [0, 0, 1280, 800]})
        self.assertEqual(run(doc, "beneath", "1")[0], 0)


if __name__ == "__main__":
    unittest.main()
