import copy
import importlib.util
from pathlib import Path
import unittest
import json
import tempfile
import sys
import types
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("grid_cancel", Path(__file__).parents[1] / "lib/tuna_grid_cancel_proof.py")
proof = importlib.util.module_from_spec(spec)
spec.loader.exec_module(proof)


class CancelEvidence(unittest.TestCase):
    def setUp(self):
        self.before = dict(locked=False, overview_open=True, overview_app_grid=True,
                           client_dnd_active=True, rendered_frames=10, seat_pressed_key_count=0)
        self.after = dict(self.before, client_dnd_active=False, rendered_frames=11)

    def test_actual_positive_shape(self):
        proof.validate(self.before, self.after, b"[]\n", b"[]\n")

    def test_old_escape_close_is_not_cancellation(self):
        self.after.update(overview_open=False, client_dnd_active=True)
        with self.assertRaises(ValueError):
            proof.validate(self.before, self.after, b"[]\n", b"[]\n")

    def test_bad_states_fail_closed(self):
        for name, value in (("locked", True), ("overview_open", False),
                            ("overview_app_grid", False), ("client_dnd_active", True),
                            ("rendered_frames", 10), ("rendered_frames", True),
                            ("rendered_frames", None), ("seat_pressed_key_count", 1), ("seat_pressed_key_count", True)):
            with self.subTest(name=name, value=value):
                after = dict(self.after, **{name: value})
                with self.assertRaises(ValueError):
                    proof.validate(self.before, after, b"[]\n", b"[]\n")

    def test_missing_fields_fail_closed(self):
        for key in self.after:
            after = copy.copy(self.after)
            del after[key]
            with self.subTest(key=key), self.assertRaises(ValueError):
                proof.validate(self.before, after, b"[]\n", b"[]\n")

    def test_empty_invalid_or_changed_layout_fails(self):
        for a, b in ((b"", b""), (b" ", b" "), (None, None), (b"[]\n", b"[changed]\n")):
            with self.subTest(layout=a), self.assertRaises(ValueError):
                proof.validate(self.before, self.after, a, b)

    def test_missing_active_before_fails(self):
        self.before["client_dnd_active"] = False
        with self.assertRaises(ValueError):
            proof.validate(self.before, self.after, b"[]\n", b"[]\n")


class LiveTreeEvidence(unittest.TestCase):
    @staticmethod
    def tree():
        root = dict(role="application", name="tuna-shell-gtk", description="", depth=0,
                    showing=True, checked=False, selected=False, focused=False, bounds=None, process_id=123)
        button = dict(root, role="push button", name="Next Page", depth=1, bounds=[0, 0, 20, 20])
        return [root, button]

    def test_positive_fresh_owner_tree(self):
        proof.validate_tree(self.tree(), 123)

    def test_any_transport_error_fails_even_with_live_anchor(self):
        nodes = self.tree() + [dict(role="error", name="gone", depth=2, showing=False)]
        with self.assertRaises(ValueError): proof.validate_tree(nodes, 123)

    def test_malformed_nodes_fail(self):
        for mutation in (None, {}, dict(self.tree()[1], showing=1),
                         dict(self.tree()[1], checked="false"), dict(self.tree()[1], role=None),
                         dict(self.tree()[1], bounds=[0, 0, True, 2]),
                         dict(self.tree()[1], bounds=[0, 0, -1, 2]),
                         dict(self.tree()[1], bounds=[0, 0, float("nan"), 2]),
                         dict(self.tree()[1], depth=True), dict(self.tree()[1], depth=3)):
            with self.subTest(node=mutation), self.assertRaises(ValueError):
                proof.validate_tree(self.tree() + [mutation], 123)

    def test_truthy_showing_does_not_supply_anchor(self):
        nodes = self.tree()
        nodes[1]["showing"] = "true"
        with self.assertRaises(ValueError): proof.validate_tree(nodes, 123)

    def test_hidden_or_unallocated_anchor_fails(self):
        for key, value in (("showing", False), ("bounds", None), ("bounds", [0, 0, 0, 0])):
            nodes = self.tree()
            nodes[1][key] = value
            with self.subTest(key=key), self.assertRaises(ValueError): proof.validate_tree(nodes, 123)

    def test_missing_wrong_and_bool_owner_fail(self):
        for value in (None, 124, True):
            nodes = self.tree()
            nodes[0]["process_id"] = value
            with self.subTest(pid=value), self.assertRaises(ValueError): proof.validate_tree(nodes, 123)

    def test_live_owner_missing_tree_and_replacement_fail(self):
        with tempfile.TemporaryDirectory() as tmp:
            proc = Path(tmp) / "proc"
            (proc / "123").mkdir(parents=True)
            stat = proc / "123/stat"
            stat.write_text("123 (GTK name) " + " ".join(["S"] + ["0"] * 18 + ["456"]))
            tree = Path(tmp) / "tree.json"
            tree.write_text(json.dumps(self.tree()))
            proof.validate_live_tree(123, "456", tree, proc)
            with self.assertRaises(ValueError): proof.validate_live_tree(123, "789", tree, proc)
            tree.unlink()
            with self.assertRaises(FileNotFoundError): proof.validate_live_tree(123, "456", tree, proc)
            tree.write_text("invalid JSON")
            with self.assertRaises(json.JSONDecodeError): proof.validate_live_tree(123, "456", tree, proc)


class StrictDumpTransport(unittest.TestCase):
    def test_production_bounds_transport_failure_is_exposed_in_strict_mode(self):
        fake_api = types.SimpleNamespace(WINDOW_COORDS=1)
        with patch.dict(sys.modules, pyatspi=fake_api):
            dump_spec = importlib.util.spec_from_file_location("actual_dump", Path(__file__).parents[1] / "lib/tuna-a11y-dump.py")
            dump = importlib.util.module_from_spec(dump_spec)
            dump_spec.loader.exec_module(dump)
        class TransportFailed:
            def queryComponent(self): raise RuntimeError("D-Bus transport failed")
        class NoComponent:
            def queryComponent(self): raise NotImplementedError("no Component interface")
        self.assertIsNone(dump.bounds(TransportFailed()))  # Existing callers preserve compatibility.
        with self.assertRaises(RuntimeError): dump.bounds(TransportFailed(), strict=True)
        self.assertIsNone(dump.bounds(NoComponent(), strict=True))


if __name__ == "__main__":
    unittest.main()
