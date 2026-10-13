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


class NeutralDragGeometry(unittest.TestCase):
    def setUp(self):
        self.nodes = LiveTreeEvidence.tree()
        self.nodes[1].update(bounds=[900, 10, 60, 60])
        def node(role, name, bounds):
            return dict(self.nodes[1], role=role, name=name, bounds=bounds)
        self.nodes += [node('table', '', [100, 10, 800, 350]),
                       node('table', '', [1100, 10, 800, 350]),
                       node('table cell', 'Folder', [750, 20, 113, 113]),
                       node('button', 'Folder', [750, 20, 113, 113])]
        self.state = dict(locked=False, overview_open=True, overview_app_grid=True,
                          client_dnd_active=True, pointer_position=[500, 200],
                          layers=[dict(namespace='tuna-shell-appgrid', rect=[0, 232, 1000, 400])])

    def active(self):
        nodes = copy.deepcopy(self.nodes)
        nodes[1].update(role='grouping', name='Next page preview', bounds=[762, 0, 238, 400])
        return nodes

    def test_normal_source_and_allocated_hint_positive(self):
        point = proof.neutral_point(self.nodes, self.state, 123)
        state = dict(self.state, pointer_position=point)
        proof.validate_neutral(self.active(), state, 123, point, self.nodes)
        self.assertLess(point[0], 759)
        self.assertNotEqual(point, [806, 308])  # Rightmost folder center is not a neutral target.

    def test_actual_inflated_hint_rejects_nominal_hint_assumption(self):
        point = [800, 432]  # Outside nominal 100px hint, inside actual 238px hint.
        with self.assertRaises(ValueError):
            proof.validate_neutral(self.active(), dict(self.state, pointer_position=point), 123, point, self.nodes)

    def test_both_hint_regions_are_forbidden(self):
        nodes = self.active()
        nodes += [dict(nodes[1], name='Previous page preview', bounds=[0, 0, 200, 400])]
        with self.assertRaises(ValueError):
            proof.validate_neutral(nodes, self.state, 123, [150, 432], self.nodes)

    def test_tile_and_border_are_forbidden(self):
        for point in ([800, 300], [747, 300]):
            with self.subTest(point=point), self.assertRaises(ValueError):
                proof.validate_neutral(self.active(), dict(self.state, pointer_position=point), 123, point, self.nodes)

    def test_wrong_page_and_missing_hint_fail(self):
        for name in ('Previous page preview', 'Next Page', 'Previous Page', ''):
            nodes = self.active(); nodes[1]['name'] = name
            with self.subTest(name=name), self.assertRaises(ValueError):
                proof.validate_neutral(nodes, self.state, 123, [500, 432], self.nodes)

    def test_source_opposite_arrow_fails(self):
        nodes = copy.deepcopy(self.nodes); nodes[1]['name'] = 'Previous Page'
        with self.assertRaises(ValueError): proof.neutral_point(nodes, self.state, 123)

    def test_wrong_owner_pointer_or_native_drag_fail(self):
        point = proof.neutral_point(self.nodes, self.state, 123)
        for state, pid in [(dict(self.state, pointer_position=[1, 2]), 123),
                           (dict(self.state, pointer_position=point, client_dnd_active=False), 123),
                           (dict(self.state, pointer_position=point), 124)]:
            with self.subTest(pid=pid, state=state), self.assertRaises(ValueError):
                proof.validate_neutral(self.active(), state, pid, point, self.nodes)

    def test_missing_ambiguous_or_transitioning_allocation_fails(self):
        for rect in (None, [0, 0, 0, 400], [0, 0, True, 400]):
            state = copy.deepcopy(self.state); state['layers'][0]['rect'] = rect
            with self.subTest(rect=rect), self.assertRaises(ValueError):
                proof.neutral_point(self.nodes, state, 123)
        state = copy.deepcopy(self.state); state['layers'] *= 2
        with self.assertRaises(ValueError): proof.neutral_point(self.nodes, state, 123)
        nodes = copy.deepcopy(self.nodes); nodes[3]['bounds'][0] = 990
        with self.assertRaises(ValueError): proof.neutral_point(nodes, self.state, 123)

    def test_no_empty_region_fails_closed(self):
        nodes = copy.deepcopy(self.nodes); nodes[4]['bounds'] = [100, 10, 800, 350]
        with self.assertRaises(ValueError): proof.neutral_point(nodes, self.state, 123)

    def test_actual_cli_checks_source_owner_and_writes_admission(self):
        with tempfile.TemporaryDirectory() as tmp:
            tree, source, state, receipt = (Path(tmp) / name for name in ('tree', 'source', 'state', 'receipt'))
            point = proof.neutral_point(self.nodes, self.state, 123)
            tree.write_text(json.dumps(self.active()))
            source.write_text(json.dumps(self.nodes))
            state.write_text(json.dumps(dict(self.state, pointer_position=point)))
            argv = ['proof', 'neutral-check', '123', '456', str(tree), str(state),
                    str(point[0]), str(point[1]), str(source), str(receipt)]
            with patch.object(sys, 'argv', argv), patch.object(proof, 'validate_live_tree') as owner:
                proof.main()
            self.assertEqual(owner.call_count, 3)
            self.assertTrue(json.loads(receipt.read_text())['first_page_observed'])
            self.assertEqual(json.loads(receipt.read_text())['point'], point)
            receipt.unlink()
            with patch.object(sys, 'argv', argv), patch.object(proof, 'validate_live_tree', side_effect=[None, None, ValueError('owner replaced')]):
                with self.assertRaises(ValueError): proof.main()
            self.assertFalse(receipt.exists())

    def test_changed_actual_page_with_stale_next_preview_fails(self):
        point = proof.neutral_point(self.nodes, self.state, 123)
        nodes = self.active(); nodes[4]['name'] = 'Different Page App'
        with self.assertRaisesRegex(ValueError, 'carousel page changed'):
            proof.validate_neutral(nodes, dict(self.state, pointer_position=point), 123, point, self.nodes)

    def test_production_input_order_and_existing_negative_controls_retained(self):
        source = (Path(__file__).parents[1] / 'tuna-gtk-shell-proof').read_text()
        selection = source.index('neutral_coordinates=')
        press = source.index('xdotool mousedown 1', selection)
        move = source.index('xdotool mousemove --window "$WID" "$grid_neutral_x"', press)
        peek = source.index('grid_cancel_dump "$ARTIFACTS/a11y-folder-page-peek.json"', move)
        observed = source.index('active neutral pointer or first page not observed', peek)
        escape = source.index('xdotool key Escape', observed)
        release = source.index('xdotool mouseup 1', escape)
        self.assertLess(selection, press); self.assertLess(move, peek)
        self.assertLess(observed, escape); self.assertLess(escape, release)
        for gate in ('normal Next Page arrow did not return', 'cancelled drag changed saved layout'):
            # Layout predicate lives in its production contract, not an injected oracle.
            if gate == 'cancelled drag changed saved layout':
                self.assertIn(gate, (Path(__file__).parents[1] / 'lib/tuna_grid_cancel_proof.py').read_text())
            else: self.assertIn(gate, source)
        self.assertIn('fail G-GRID-DRAG-PAGE', source)


if __name__ == "__main__":
    unittest.main()
