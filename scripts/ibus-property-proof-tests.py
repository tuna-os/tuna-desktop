#!/usr/bin/python3
"""Source/helper controls only; these tests do not execute GTK or IBus."""
import ast
from pathlib import Path
import sys
import unittest
sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "scripts/lib/tuna-ibus-properties-proof.py"
module = ast.parse(SOURCE.read_text(), filename=str(SOURCE))
functions = [n for n in module.body if isinstance(n, ast.FunctionDef)
             and n.name in {"validate_traffic", "walk", "walk_with_ancestors",
                            "collapse_known_menu_pair", "wait_until"}]
namespace = {}
exec(compile(ast.Module(body=functions, type_ignores=[]), str(SOURCE), "exec"), namespace)


class Controls(unittest.TestCase):
    def test_exact_ordered_fresh_callback_passes(self):
        namespace["validate_traffic"](["old-positive"], ["old-positive", "fresh-positive"], "fresh-positive")

    def test_missing_duplicate_reordered_and_old_action_fail(self):
        before = ["old-positive"]
        for after in [before, before + ["old-action", "fresh-positive"],
                      before + ["fresh-positive", "old-action"],
                      before + ["fresh-positive", "fresh-positive"],
                      ["fresh-positive", "old-positive"], before + ["wrong-engine"]]:
            with self.subTest(after=after), self.assertRaises(RuntimeError):
                namespace["validate_traffic"](before, after, "fresh-positive")

    def test_accessible_walk_node_and_depth_limits(self):
        class Node:
            def __init__(self, children=()): self.children = children
            def get_child_count(self): return len(self.children)
            def get_child_at_index(self, i): return self.children[i]
        self.assertEqual(len(list(namespace["walk"](Node([Node()])))), 2)
        with self.assertRaises(RuntimeError):
            list(namespace["walk"](Node([Node() for _ in range(4096)])))
        node = Node()
        for _ in range(33): node = Node([node])
        with self.assertRaises(RuntimeError): list(namespace["walk"](node))

    @staticmethod
    def _pair(parent_role="button", child_role="toggle button",
              bounds=(1095, 0, 49, 34), parent_actions=0, child_actions=1,
              related=True, order="parent-first"):
        parent = object()
        child = object()
        records = [(parent, bounds, parent_role, parent_actions, ()),
                   (child, bounds, child_role, child_actions,
                    (parent,) if related else ())]
        if order == "child-first":
            records.reverse()
        return records, parent, child

    def test_known_menu_pair_resolves_to_actionable_descendant(self):
        collapse = namespace["collapse_known_menu_pair"]
        for order in ("parent-first", "child-first"):
            records, _parent, child = self._pair(order=order)
            self.assertEqual(collapse(records), [child])
        records, _parent, child = self._pair(parent_role="push button")
        self.assertEqual(collapse(records), [child])

    def test_unrelated_equal_bounds_nodes_stay_ambiguous(self):
        collapse = namespace["collapse_known_menu_pair"]
        records, parent, child = self._pair(related=False)
        self.assertEqual(collapse(records), [parent, child])
        distinct = [(parent, (0, 0, 10, 10), "button", 0, ()),
                    (child, (20, 0, 10, 10), "toggle button", 1, (parent,))]
        self.assertEqual(collapse(distinct), [parent, child])
        self.assertEqual(collapse([]), [])
        self.assertEqual(collapse([(parent, (0, 0, 10, 10), "button", 0, ())]), [parent])

    def test_unknown_or_nonpositive_bounds_never_resolve(self):
        collapse = namespace["collapse_known_menu_pair"]
        for bounds in (None, (1095, 0, 0, 34), (1095, 0, 49, 0)):
            records, parent, child = self._pair(bounds=bounds)
            with self.subTest(bounds=bounds):
                self.assertEqual(collapse(records), [parent, child])
        records, parent, child = self._pair()
        records[0] = (parent, None, "button", 0, ())
        self.assertEqual(collapse(records), [parent, child])

    def test_nested_extra_match_stays_ambiguous(self):
        collapse = namespace["collapse_known_menu_pair"]
        records, parent, child = self._pair()
        extra = object()
        records.append((extra, (1095, 0, 49, 34), "toggle button", 1, (parent, child)))
        self.assertEqual(collapse(records), [parent, child, extra])

    def test_duplicate_zero_or_unknown_actions_stay_ambiguous(self):
        collapse = namespace["collapse_known_menu_pair"]
        for parent_actions, child_actions in ((1, 1), (0, 0), (1, 0), (None, 1), (0, None)):
            records, parent, child = self._pair(parent_actions=parent_actions,
                                                child_actions=child_actions)
            with self.subTest(parent=parent_actions, child=child_actions):
                self.assertEqual(collapse(records), [parent, child])

    def test_wrong_roles_or_reversed_ancestry_stay_ambiguous(self):
        collapse = namespace["collapse_known_menu_pair"]
        records, parent, child = self._pair(parent_role="menu", child_role="menu item")
        self.assertEqual(collapse(records), [parent, child])
        records, parent, child = self._pair(parent_role="toggle button", child_role="button")
        self.assertEqual(collapse(records), [parent, child])
        toggle = object()
        button = object()
        records = [(toggle, (1095, 0, 49, 34), "toggle button", 1, ()),
                   (button, (1095, 0, 49, 34), "button", 0, (toggle,))]
        self.assertEqual(collapse(records), [toggle, button])

    def test_walk_with_ancestors_tracks_identity_chain_and_limits(self):
        walk = namespace["walk_with_ancestors"]
        class Node:
            def __init__(self, children=()): self.children = children
            def get_child_count(self): return len(self.children)
            def get_child_at_index(self, i): return self.children[i]
        leaf = Node()
        mid = Node([leaf])
        root = Node([mid])
        seen = list(walk(root))
        self.assertEqual([(node is root, len(ancestors)) for node, ancestors in seen],
                         [(True, 0), (False, 1), (False, 2)])
        _leaf_node, ancestors = seen[2]
        self.assertTrue(ancestors[0] is root and ancestors[1] is mid)
        with self.assertRaises(RuntimeError):
            list(walk(Node([Node() for _ in range(4096)])))
        node = Node()
        for _ in range(33): node = Node([node])
        with self.assertRaises(RuntimeError): list(walk(node))

    def test_wait_rechecks_deadline_after_late_positive(self):
        wait_until = namespace["wait_until"]
        wait_until(lambda: True, 200.0, clock=lambda: 100.0, sleep=lambda s: None)
        now = [100.0]
        def slow_predicate():
            now[0] = 300.0
            return True
        with self.assertRaises(RuntimeError):
            wait_until(slow_predicate, 200.0, clock=lambda: now[0], sleep=lambda s: None)
        ticking = [100.0]
        def tick(seconds):
            ticking[0] += seconds
        with self.assertRaises(RuntimeError):
            wait_until(lambda: False, 102.0, clock=lambda: ticking[0], sleep=tick)

    def test_global_collapse_removed_and_wait_delegates_with_guards(self):
        helper = SOURCE.read_text()
        self.assertNotIn("collapse_same_widget", helper)
        self.assertIn("collapse_known_menu_pair(records)", helper)
        self.assertIn("wait_until(predicate, deadline, time.monotonic, time.sleep)", helper)
        self.assertIn("if clock() >= deadline:", helper)
        self.assertIn("GTK process changed", helper)
        self.assertIn("GTK accessible application not unique", helper)

    def test_harness_has_external_blocking_call_bound_and_expected_executable(self):
        harness = (ROOT / "scripts/tuna-gtk-shell-proof").read_text()
        self.assertIn('timeout --kill-after=2s 40s', harness)
        self.assertIn('"$privacy_expected_shell" "${IME_PIDS%% *}"', harness)
        self.assertIn('fail G-IME-PROPERTIES "live properties proof failed"', harness)
        self.assertIn('file_hash(expected_shell) != expected_hash', SOURCE.read_text())

    def test_owner_refresh_and_lifecycle_admission_are_explicit(self):
        gtk = (ROOT / "crates/shell-gtk/src/ibus_property_menu.rs").read_text()
        workflow = (ROOT / ".github/workflows/ci.yml").read_text()
        self.assertIn('python3 scripts/ibus-property-proof-tests.py', workflow)
        self.assertIn('Some(&destination)', gtk)
        self.assertIn('name == IBUS', gtk)
        self.assertIn('menu.owner_epoch.get() != epoch', gtk)
        self.assertIn('1u32 | 4u32', gtk)
        self.assertNotIn('1u32 | 2u32', gtk)
        self.assertIn('another panel owns the name', gtk)
        helper = SOURCE.read_text()
        self.assertIn('phase_file.write_text', helper)
        self.assertIn('ss_pid != pid', helper)
        self.assertIn('instance() != original_instance', helper)
        self.assertIn('validate_traffic(before, traffic(), positive)', helper)

    def test_production_callback_keeps_epoch_and_peer_lock_guards(self):
        gtk = (ROOT / "crates/shell-gtk/src/ibus_property_menu.rs").read_text()
        host = (ROOT / "crates/shell-host/src/ibus_properties.rs").read_text()
        self.assertIn('let token = self.model.borrow().token(&p.key);', gtk)
        self.assertIn('menu.activate(&token)', gtk)
        self.assertIn('token.epoch != self.epoch', host)
        self.assertIn('trusted_sender(self.peer.borrow().as_ref(), sender)', gtk)
        self.assertRegex(gtk, r'conn\s*\.stream\(\)')
        self.assertIn('credentials.unix_user()', gtk)
        self.assertIn('same_file(&self.expected, &actual)', gtk)
        self.assertIn('!self.owned.get()', gtk)
        self.assertIn('(self.locked)()', gtk)


if __name__ == "__main__":
    unittest.main()
