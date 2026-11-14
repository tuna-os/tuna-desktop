#!/usr/bin/python3
"""Parser/geometry regression checks; these are not browser journey receipts."""
from pathlib import Path
import runpy
import unittest


HELPER = runpy.run_path(str(Path(__file__).resolve().parents[1] / "tuna-toplevel-drag-proof"))


class ToplevelDragProofTests(unittest.TestCase):
    def test_requires_actual_protocol_requests_not_a_global_or_fallback(self):
        counts = HELPER["trace_counts"]('''
[1] wl_registry#2.global(37, "xdg_toplevel_drag_manager_v1", 1)
[2] -> wl_registry#2.bind(37, "xdg_toplevel_drag_manager_v1", 1, new id [unknown]#10)
[3] -> zcr_extended_drag_v1#11.drag(12)
[4] fake_attach_record=true
''')
        self.assertEqual(counts, {"create": 0, "attach": 0, "drop": 0, "cancel": 0})

    def test_counts_real_wayland_debug_messages(self):
        counts = HELPER["trace_counts"]('''
[1] -> xdg_toplevel_drag_manager_v1#10.get_xdg_toplevel_drag(new id xdg_toplevel_drag_v1#12, wl_data_source#11)
[2] -> xdg_toplevel_drag_v1#12.attach(xdg_toplevel#13, 90, 18)
[3] wl_data_source#11.dnd_drop_performed()
[4] wl_data_source#11.cancelled()
''')
        self.assertEqual(counts, {"create": 1, "attach": 1, "drop": 1, "cancel": 1})

    def test_window_must_follow_both_pointer_axes(self):
        follows = HELPER["following"]
        self.assertTrue(follows([20, 30, 800, 600], [100, 70, 800, 600], (80, 40)))
        self.assertFalse(follows([20, 30, 800, 600], [20, 30, 800, 600], (80, 40)))
        self.assertFalse(follows([20, 30, 800, 600], [100, 30, 800, 600], (80, 40)))
        self.assertFalse(follows([20, 30, 800, 600], [180, 110, 800, 600], (80, 40)))


if __name__ == "__main__":
    unittest.main()
