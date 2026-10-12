#!/usr/bin/env python3
"""Focused negative controls for the live folder observation predicate."""
import importlib.util
from pathlib import Path
import unittest

spec = importlib.util.spec_from_file_location("folder_watch_proof", Path(__file__).with_name("gtk-folder-watch-proof.py"))
proof = importlib.util.module_from_spec(spec)
spec.loader.exec_module(proof)


class ObservationTests(unittest.TestCase):
    def test_only_current_showing_names_qualify(self):
        nodes = [{"name": "Live", "showing": True}, {"name": "Old", "showing": False}]
        self.assertTrue(proof.qualifies(nodes, ["Live"], ["Old"]))
        self.assertFalse(proof.qualifies(nodes, ["Missing"], ["Old"]))
        self.assertFalse(proof.qualifies(nodes, ["Live"], ["Live"]))
        self.assertFalse(proof.qualifies([], ["Live"], []))

    def test_stale_showing_sibling_is_rejected(self):
        nodes = [{"name": "Live", "showing": True}, {"name": "Old", "showing": True}]
        self.assertFalse(proof.qualifies(nodes, ["Live"], ["Old"]))


if __name__ == "__main__":
    unittest.main()
