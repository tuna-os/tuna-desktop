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
selected = [n for n in module.body if (isinstance(n, ast.FunctionDef)
            and n.name in {"validate_traffic", "live", "walk", "matching"})
            or (isinstance(n, ast.Assign) and any(getattr(t, "id", None) == "INDICATOR" for t in n.targets))]


class Atspi:
    class StateType:
        SHOWING = "showing"


class GLib:
    class Error(Exception):
        pass


namespace = {"Atspi": Atspi, "GLib": GLib}
exec(compile(ast.Module(body=selected, type_ignores=[]), str(SOURCE), "exec"), namespace)


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
            def get_role(self): return "panel"
            def get_child_count(self): return len(self.children)
            def get_child_at_index(self, i): return self.children[i]
        self.assertEqual(len(list(namespace["walk"](Node([Node()])))), 2)
        with self.assertRaises(RuntimeError):
            list(namespace["walk"](Node([Node() for _ in range(4096)])))
        node = Node()
        for _ in range(33): node = Node([node])
        with self.assertRaises(RuntimeError): list(namespace["walk"](node))

    def test_defunct_reference_is_skipped_not_retried_forever(self):
        # libatspi keeps a disposed application: every query through it fails
        # ("The application no longer exists") for the process's life.
        class Node:
            def __init__(self, children=()): self.children = children
            def get_role(self): return "panel"
            def get_child_count(self): return len(self.children)
            def get_child_at_index(self, i): return self.children[i]
        class Defunct(Node):
            def get_role(self): raise GLib.Error("The application no longer exists")
        live = Node()
        root = Node([Defunct([Node()]), live])
        self.assertEqual(list(namespace["walk"](root)), [root, live])
        self.assertFalse(namespace["live"](Defunct()))
        self.assertTrue(namespace["live"](live))

    def test_indicator_selects_the_showing_inner_toggle_of_the_menu_button(self):
        class Node:
            def __init__(self, role, name, showing=True):
                self.role, self.name, self.showing = role, name, showing
            def get_name(self): return self.name
            def get_role_name(self): return self.role
            def get_state_set(self):
                showing = self.showing
                class States:
                    def contains(self, state): return state == "showing" and showing
                return States()
        # Shape of a gtk::MenuButton in the shell's a11y dumps (Date and Time,
        # System): an outer "button" and an inner "toggle button", same name.
        outer, inner = Node("button", "中"), Node("toggle button", "中")
        tree = [outer, inner, Node("label", "中"), Node("toggle button", "中", showing=False),
                Node("toggle button", "Date and Time")]
        self.assertEqual(namespace["matching"](tree, "中"), [outer, inner])
        self.assertEqual(namespace["matching"](tree, *namespace["INDICATOR"]), [inner])
        helper = SOURCE.read_text()
        self.assertNotIn('press("中")', helper)
        self.assertEqual(helper.count('press(*INDICATOR)'), 3)

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

    def test_property_toggles_expose_an_accessible_action(self):
        # GTK's AT-SPI bridge exports no Action for GtkCheckButton, so the
        # proof's press("Input Mode") needs the widget-local action group.
        gtk = (ROOT / "crates/shell-gtk/src/ibus_property_menu.rs").read_text()
        self.assertIn('expose_toggle_action(&button);', gtk)
        self.assertIn('button.insert_action_group("property", Some(&actions));', gtk)

    def test_property_activate_state_is_unsigned(self):
        # ibus-daemon reads PropertyActivate as (&su); an (si) body reaches
        # the engine as a NULL key (journey 38726161191).
        host = (ROOT / "crates/shell-host/src/ibus_properties.rs").read_text()
        panel = (ROOT / "crates/shell-gtk/src/ibus_panel.rs").read_text()
        self.assertIn('pub fn activate(&mut self, token: &Token) -> Vec<(String, u32)>', host)
        self.assertIn('<signal name="PropertyActivate"><arg type="s"/><arg type="u"/></signal>', panel)

    def test_every_loaded_name_resolves(self):
        # These controls never execute the helper end to end, so a dropped
        # import only showed on CI as a NameError (journey 116756962210:
        # 'Path' undefined after the faulthandler edit).
        import builtins
        bound = set(dir(builtins)) | {"__file__"}
        for n in ast.walk(module):
            if isinstance(n, (ast.Import, ast.ImportFrom)):
                bound |= {(a.asname or a.name).split(".")[0] for a in n.names}
            elif isinstance(n, (ast.FunctionDef, ast.ClassDef)):
                bound.add(n.name)
            elif isinstance(n, ast.Name) and isinstance(n.ctx, (ast.Store, ast.Del)):
                bound.add(n.id)
            elif isinstance(n, ast.arg):
                bound.add(n.arg)
            elif isinstance(n, ast.ExceptHandler) and n.name:
                bound.add(n.name)
        loaded = {n.id for n in ast.walk(module)
                  if isinstance(n, ast.Name) and isinstance(n.ctx, ast.Load)}
        self.assertEqual(loaded - bound, set())


if __name__ == "__main__":
    unittest.main()
