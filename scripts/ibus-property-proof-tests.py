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
             and n.name in {"validate_traffic", "walk"}]
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
