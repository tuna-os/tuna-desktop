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
            and n.name in {"validate_traffic", "live", "walk", "matching",
                           "application_gone", "owned_application", "child_failure"})
            or (isinstance(n, ast.ClassDef) and n.name == "Transient")
            or (isinstance(n, ast.Assign) and any(getattr(t, "id", None) == "INDICATOR" for t in n.targets))]


class Atspi:
    class StateType:
        SHOWING = "showing"


class GLib:
    # As PyGObject's: GLib.Error is a RuntimeError.
    class Error(RuntimeError):
        def __init__(self, message="", domain="", code=-1):
            super().__init__(message)
            self.message, self.domain, self.code = message, domain, code


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

    def test_only_libatspis_own_disposal_of_the_bus_bound_shell_is_transient(self):
        # GTK < 4.17.1 names its own application root as the child removed
        # when one of its windows hides; libatspi 2.52 then disposes that
        # application in the reading process although it never left the bus
        # (gtk-shell 123281544604: 'not unique (0 live)' after the press).
        gone = GLib.Error("The application no longer exists", "atspi_error", 0)
        class Root:
            def __init__(self, pid, error=None): self.pid, self.error = pid, error
            def get_role(self):
                if self.error:
                    raise self.error
                return "application"
            def get_child_count(self): return 1
            def get_process_id(self): return self.pid
        owned, transient = namespace["owned_application"], namespace["Transient"]
        shell, other, disposed = Root(42), Root(7), Root(42, gone)
        self.assertTrue(namespace["application_gone"](gone))
        self.assertEqual(owned([(":1.0", shell), (":1.3", other)], 42, {}.get), (":1.0", shell))
        self.assertEqual(owned([(":1.3", Root(7, gone)), (":1.0", shell)], 42, {":1.3": 7}.get),
                         (":1.0", shell))
        with self.assertRaises(transient):
            owned([(":1.0", disposed), (":1.3", other)], 42, {":1.0": 42}.get)
        # The bus no longer binds the name to the shell: it really left.
        for bus_pid in ({}.get, {":1.0": 7}.get):
            with self.assertRaisesRegex(RuntimeError, r"not unique \(0 live\)"):
                owned([(":1.0", disposed)], 42, bus_pid)
        # Errors the application or the bus sent back are no local disposal.
        for error in (GLib.Error("No such interface", "atspi_error", 1),
                      GLib.Error("Disconnected", "g-dbus-error-quark", 0)):
            self.assertFalse(namespace["application_gone"](error))
            with self.assertRaisesRegex(RuntimeError, r"not unique \(0 live\)"):
                owned([(":1.0", Root(42, error))], 42, {":1.0": 42}.get)
        # Absence and duplicates still fail.
        with self.assertRaisesRegex(RuntimeError, r"not unique \(0 live\)"):
            owned([(":1.3", other)], 42, {}.get)
        with self.assertRaisesRegex(RuntimeError, r"not unique \(2 live\)"):
            owned([(":1.0", shell), (":1.9", Root(42))], 42, {}.get)

    def test_a_disposal_after_the_liveness_reads_is_the_same_disposal(self):
        # gtk-shell 123521641884: get_role and get_child_count succeeded, then
        # get_process_id failed "Process is defunct" (atspi_error 1): libatspi
        # had disposed the application, clearing the root's app, in between.
        defunct = GLib.Error("Process is defunct", "atspi_error", 1)
        class Root:
            def __init__(self, pid, app=":1.0", error=None):
                self.pid, self.app, self.error = pid, app, error
            def get_role(self): return "application"
            def get_child_count(self): return 1
            def get_process_id(self):
                if self.error:
                    raise self.error
                return self.pid
        owned, transient = namespace["owned_application"], namespace["Transient"]
        shell, disposed = Root(42), Root(42, None, defunct)
        # Another application's disposal or departure is not the shell's.
        for other in (Root(7, None, defunct), Root(7, ":1.3", defunct)):
            self.assertEqual(owned([(":1.3", other), (":1.0", shell)], 42, {":1.3": 7}.get),
                             (":1.0", shell))
        # The shell's own disposal rereads only while the bus binds it to the shell.
        with self.assertRaises(transient):
            owned([(":1.0", disposed)], 42, {":1.0": 42}.get)
        for bus_pid in ({}.get, {":1.0": 7}.get):
            with self.assertRaisesRegex(RuntimeError, r"not unique \(0 live\)"):
                owned([(":1.0", disposed)], 42, bus_pid)
        # The bus no longer resolving a still-held application is no disposal.
        with self.assertRaisesRegex(RuntimeError, r"not unique \(0 live\)"):
            owned([(":1.0", Root(42, ":1.0", defunct))], 42, {":1.0": 42}.get)

    def test_a_disposal_under_the_walk_is_transient_only_while_the_bus_binds_the_shell(self):
        # gtk-shell 123371830361: libatspi disposed the shell's application
        # while matching walked it after a press; the GLib.Error fell into
        # `except RuntimeError` and failed the proof instead of rereading.
        self.assertTrue(issubclass(GLib.Error, RuntimeError))
        failure = namespace["child_failure"]
        gone = GLib.Error("The application no longer exists", "atspi_error", 0)
        reply = failure(gone, ":1.0", 42, {":1.0": 42}.get)
        self.assertEqual(list(reply), ["transient"])
        # The bus no longer binds the name to the shell, or no application
        # was resolved yet: a real failure.
        for app_bus, bus_pid in ((":1.0", {}.get), (":1.0", {":1.0": 7}.get), (None, {":1.0": 42}.get)):
            with self.subTest(app_bus=app_bus):
                self.assertEqual(failure(gone, app_bus, 42, bus_pid),
                                 {"error": "The application no longer exists"})
        # Errors the application or the bus sent back, and every other
        # RuntimeError, still fail.
        for error in (GLib.Error("No such interface", "atspi_error", 1),
                      GLib.Error("Disconnected", "g-dbus-error-quark", 0),
                      RuntimeError("GTK accessible application not unique (0 live)"),
                      RuntimeError("accessible depth limit")):
            with self.subTest(error=str(error)):
                self.assertEqual(list(failure(error, ":1.0", 42, {":1.0": 42}.get)), ["error"])

    def test_the_child_classifies_glib_errors_through_child_failure(self):
        functions = {n.name: n for n in module.body if isinstance(n, ast.FunctionDef)}
        # A `GLib.Error` handler after a `RuntimeError` one of the same try
        # is unreachable.
        for statement in ast.walk(functions["atspi_child"]):
            if isinstance(statement, ast.Try):
                caught = [ast.unparse(h.type) for h in statement.handlers if h.type is not None]
                if "RuntimeError" in caught:
                    self.assertNotIn("GLib.Error", caught[caught.index("RuntimeError"):])
        source = SOURCE.read_text()
        self.assertIn("return child_failure(error, app_bus, pid, bus_pid)",
                      ast.get_source_segment(source, functions["atspi_child"]))

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
        # Open-only: the lifecycle's final close is real Escape in the harness.
        self.assertEqual(helper.count('press(*INDICATOR)'), 2)
        harness = (ROOT / "scripts/tuna-gtk-shell-proof").read_text()
        self.assertIn('fail G-IME-PROPERTIES-LIFECYCLE "Escape did not close the property menu"', harness)

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

    def test_every_atspi_query_and_press_runs_in_a_fresh_process(self):
        # A long-lived driver's libatspi keeps a disposed application object
        # (gtk-shell 116769499648: 'not unique (0 live)' right after the
        # popover opened). Only the per-call child may touch libatspi.
        functions = {n.name: n for n in module.body if isinstance(n, ast.FunctionDef)}
        def atspi_uses(fn):
            return [n for n in ast.walk(fn) if isinstance(n, ast.Name) and n.id == "Atspi"]
        self.assertEqual(atspi_uses(functions["run"]), [])
        self.assertEqual(atspi_uses(functions["atspi"]), [])
        self.assertTrue(atspi_uses(functions["atspi_child"]))
        source = SOURCE.read_text()
        self.assertIn('[sys.executable, __file__, "--atspi", str(pid), op, name, role or ""]', source)
        read = ast.get_source_segment(source, functions["run"])
        self.assertIn('return atspi(pid, op, name, role)', read)
        self.assertIn('reads.append(read("query", name, role))', read)
        self.assertIn('wait(lambda: bool(read("press", name, role)))', read)
        # Assertions the child must still enforce, before and after its walk.
        child = ast.get_source_segment(source, functions["atspi_child"])
        for message in ("ambiguous property action", "accessible action rejected"):
            self.assertIn(message, child)
        self.assertEqual(child.count("owned_application("), 2)
        self.assertIn("GTK accessible application not unique",
                      ast.get_source_segment(source, functions["owned_application"]))

    def test_a_press_is_repeated_only_when_no_action_was_sent(self):
        # press() retries a Transient press; only libatspi's own refusal
        # before DoAction may be one. A timeout or a DoAction error fails.
        functions = {n.name: n for n in module.body if isinstance(n, ast.FunctionDef)}
        source = SOURCE.read_text()
        child = ast.get_source_segment(source, functions["atspi_child"])
        action = child[child.index("action.do_action(0)"):]
        self.assertLess(action.index("if application_gone(error):"), action.index("raise Transient("))
        self.assertIn('return {"error": f"accessible action failed: {error.message}"}', action)
        parent = ast.get_source_segment(source, functions["atspi"])
        self.assertLess(parent.index('if op == "press":'), parent.index('raise RuntimeError(f"AT-SPI press'))
        self.assertLess(parent.index('raise RuntimeError(f"AT-SPI press'), parent.index("raise Transient("))


if __name__ == "__main__":
    unittest.main()
