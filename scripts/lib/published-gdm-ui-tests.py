#!/usr/bin/env python3
"""Fake-only tests: no GI imports, GUI, bus, input or authentication."""
import contextlib
import importlib.machinery
import importlib.util
import io
import json
from pathlib import Path
import unittest
from unittest.mock import patch


PATH = Path(__file__).resolve().parents[2] / "packaging/marlin/vm-lane/tuna-vm-gdm-ui"
LOADER = importlib.machinery.SourceFileLoader("published_gdm_ui", str(PATH))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
ui = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(ui)

PRINCIPAL = {"pid": 123, "uid": 120, "start_ticks": 456,
             "executable": "/usr/bin/gnome-shell", "exe_device": 1,
             "exe_inode": 2, "session_id": "c1", "session_id_source": "trusted-root-collector"}


class Node:
    def __init__(self, role="other", name="", children=(), **states):
        self.role, self.name, self.children = role, name, list(children)
        self.states = dict(showing=True, enabled=True, focused=False, selected=False, editable=False)
        self.states.update(states)
        self.rect = [10, 20, 100, 40]
        self.pid = 123
        self.owner = ":1.42"


class Fake:
    def __init__(self, nodes):
        self.app = Node(children=nodes)
        self.root = Node(children=[self.app])
        self.calls = []
        self.creds = 0
        self.wrong_after = False
        self.available = True

    def existing_bus(self):
        if not self.available:
            raise RuntimeError("private arbitrary error must never be serialized")

    def bind_owner(self, owner): self.bound_owner = owner
    def desktop(self): return self.root
    def child_count(self, node):
        if node.role in ui.INPUT_ROLES:
            raise AssertionError("queried input children")
        return len(node.children)
    def child(self, node, i): return node.children[i]
    def pid(self, node): return node.pid
    def app_owner(self, node): return node.owner
    def credentials(self, owner):
        self.creds += 1
        return {"GetNameOwner": owner,
                "GetConnectionUnixProcessID": 124 if self.wrong_after and self.creds > 1 else 123,
                "GetConnectionUnixUser": 120}
    def inspect(self, node): return node.role, node.states
    def public_name(self, node):
        self.calls.append(node)
        if node.role in ui.INPUT_ROLES:
            raise AssertionError("queried input name")
        return node.name
    def rect(self, node): return node.rect


class Tests(unittest.TestCase):
    def run_fake(self, fake, **kwargs):
        return ui.observe(123, 456, "c1", 120, backend_factory=lambda remaining: fake,
                          identity=kwargs.pop("identity", lambda *a: dict(PRINCIPAL)), **kwargs)

    def test_fixture_user_public_only(self):
        f = Fake([Node("button", "tuna-test"), Node("button", "Not listed?"),
                  Node("label", "an unrelated private-looking label")])
        r = self.run_fake(f)
        self.assertEqual([n["kind"] for n in r["controls"]], ["fixture-user", "not-listed"])
        self.assertTrue(r["bus_owner_verified"])
        self.assertFalse(r["input_value_observed"])
        self.assertEqual(r["principal"], PRINCIPAL)
        self.assertNotIn("private-looking", json.dumps(r))
        self.assertEqual(f.creds, 2)

    def test_password_never_reads_name_value_or_children(self):
        secret = Node("password-entry", "never read me", [Node("label", "secret child")], focused=True)
        f = Fake([secret, Node("button", "Submit"), Node("button", "Back")])
        r = self.run_fake(f)
        self.assertNotIn(secret, f.calls)
        self.assertEqual(r["controls"][0]["kind"], "password-entry")
        self.assertNotIn("secret", json.dumps(r))
        self.assertTrue(r["controls"][0]["focused"])

    def test_username_requires_public_prompt_without_reading_entry(self):
        entry = Node("entry", "do not read", [Node("label", "entered content")])
        f = Fake([Node("label", "Username:"), entry, Node("button", "Submit")])
        r = self.run_fake(f)
        self.assertNotIn(entry, f.calls)
        self.assertIn("username-entry", [c["kind"] for c in r["controls"]])
        with self.assertRaises(ui.Unavailable):
            self.run_fake(Fake([entry, Node("button", "Submit")]))

    def test_text_role_is_never_named(self):
        text = Node("text", "private input", editable=True)
        f = Fake([Node("label", "Username:"), text, Node("button", "Submit")])
        self.run_fake(f)
        self.assertNotIn(text, f.calls)

    def test_unknown_editable_role_is_never_named(self):
        node = Node("label", "must not read", editable=True)
        f = Fake([node, Node("button", "Submit")])
        with self.assertRaises(ui.Unavailable): self.run_fake(f)
        self.assertNotIn(node, f.calls)

    def test_tuna_selection_requires_real_state(self):
        item = Node("radio-menu-item", "Tuna Desktop", selected=False)
        r = self.run_fake(Fake([item]))
        self.assertFalse(r["controls"][0]["selected"])
        item.states["selected"] = True
        self.assertTrue(self.run_fake(Fake([item]))["controls"][0]["selected"])

    def test_exact_shell_51_public_controls(self):
        r = self.run_fake(Fake([Node("menu", "Login Options"),
                                Node("button", "Submit"), Node("button", "Back")]))
        self.assertEqual([c["kind"] for c in r["controls"]],
                         ["session-menu", "sign-in", "cancel"])
        for role, name in (("button", "Login Options"), ("button", "Select a session"),
                           ("button", "Sign In"), ("button", "Cancel")):
            with self.subTest(name=name), self.assertRaises(ui.Unavailable):
                self.run_fake(Fake([Node(role, name)]))

    def test_unknown_localized_and_label_impostors_fail(self):
        for node in (Node("button", "Anmelden"), Node("label", "Tuna Desktop"),
                     Node("label", "tuna-test"), Node("label", "Back")):
            with self.subTest(node=node.name), self.assertRaises(ui.Unavailable):
                self.run_fake(Fake([node]))

    def test_auth_error_public_and_no_success_on_error_alone(self):
        error = Node("label", "Sorry, that didn’t work. Please try again.")
        r = self.run_fake(Fake([error, Node("password-entry"), Node("button", "Submit")]))
        self.assertIn("auth-error", [c["kind"] for c in r["controls"]])
        with self.assertRaises(ui.Unavailable): self.run_fake(Fake([error]))

    def test_duplicate_controls_and_entries_fail(self):
        cases = [[Node("button", "Submit"), Node("button", "Submit")],
                 [Node("password-entry"), Node("password-entry")],
                 [Node("label", "Username:"), Node("entry"), Node("entry")]]
        for nodes in cases:
            with self.subTest(nodes=nodes), self.assertRaises(ui.Unavailable): self.run_fake(Fake(nodes))

    def test_wrong_bus_owner_before_or_after_fails(self):
        f = Fake([Node("button", "tuna-test")])
        f.wrong_after = True
        with self.assertRaises(ui.Unavailable): self.run_fake(f)
        f = Fake([Node("button", "tuna-test")])
        f.credentials = lambda owner: {"GetNameOwner": owner, "GetConnectionUnixProcessID": 124,
                                        "GetConnectionUnixUser": 120}
        with self.assertRaises(ui.Unavailable): self.run_fake(f)

    def test_missing_or_ambiguous_original_app_fails(self):
        for count in (0, 2):
            f = Fake([Node("button", "tuna-test")])
            f.root.children = [f.app] * count
            with self.assertRaises(ui.Unavailable): self.run_fake(f)

    def test_principal_change_fails(self):
        calls = []
        def identity(*args):
            calls.append(1)
            return {**PRINCIPAL, "start_ticks": 456 if len(calls) == 1 else 457}
        with self.assertRaises(ui.Unavailable):
            self.run_fake(Fake([Node("button", "tuna-test")]), identity=identity)

    def test_invalid_rect_and_hidden_controls(self):
        for rect in ([1, 2, 0, 4], [1, 2, 3, -4], [True, 2, 3, 4], [-2**31, 0, 3, 4]):
            node = Node("button", "tuna-test"); node.rect = rect
            with self.subTest(rect=rect), self.assertRaises(ui.Unavailable): self.run_fake(Fake([node]))
        with self.assertRaises(ui.Unavailable):
            self.run_fake(Fake([Node("button", "tuna-test", showing=False)]))

    def test_tree_and_time_bounds(self):
        root = Node("button", "tuna-test")
        for _ in range(ui.MAX_DEPTH + 1): root = Node(children=[root])
        with self.assertRaises(ui.Unavailable): self.run_fake(Fake([root]))
        with self.assertRaises(ui.Unavailable):
            self.run_fake(Fake([Node()] * (ui.MAX_CHILDREN + 1)))
        times = iter([0, 9])
        with self.assertRaises(ui.Unavailable):
            self.run_fake(Fake([Node("button", "tuna-test")]), clock=lambda: next(times))
        with patch.object(ui, "MAX_OUTPUT", 8), self.assertRaises(ui.Unavailable):
            self.run_fake(Fake([Node("button", "tuna-test")]))

    def test_no_existing_tree_never_passes(self):
        f = Fake([Node("button", "tuna-test")]); f.available = False
        with self.assertRaises(RuntimeError): self.run_fake(f)

    def test_expected_dynamic_uid_must_match_before_bus_reads(self):
        f = Fake([Node("button", "tuna-test")])
        with self.assertRaises(ui.Unavailable):
            self.run_fake(f, identity=lambda *args: {**PRINCIPAL, "uid": 121})
        self.assertEqual(f.creds, 0)

    def test_process_identity_refuses_wrong_uid_or_non_greeter_account(self):
        from types import SimpleNamespace
        for real_uid, name, mapped_uid in ((0, "gdm-greeter", 120), (121, "gdm-greeter", 120),
                                          (120, "gdm", 120), (120, "tuna-test", 120),
                                          (120, "gdm-greeter-1", 120), (120, "gdm-greeter", 121)):
            record = SimpleNamespace(pw_name=name, pw_uid=mapped_uid)
            with self.subTest(uid=real_uid, name=name), \
                 patch.object(ui.os, "getuid", return_value=real_uid), \
                 patch.object(ui.os, "geteuid", return_value=real_uid), \
                 patch.object(ui.pwd, "getpwuid", return_value=record), \
                 patch.object(ui.pwd, "getpwnam", return_value=record):
                with self.assertRaises(ui.Unavailable): ui.process_identity(123, 456, "c1", 120)

    def test_invalid_arguments(self):
        for args in ((True, 456, "c1", 120), (123, 0, "c1", 120), (123, 456, "../bad", 120),
                     (123, 456, "c1", 0), (123, 456, "c1", True)):
            with self.subTest(args=args), self.assertRaises(ui.Unavailable): ui.observe(*args)

    def test_cli_fixed_unavailable_without_exception_or_argument_echo(self):
        out = io.StringIO()
        with patch.object(ui, "observe", side_effect=RuntimeError("secret failure")), contextlib.redirect_stdout(out):
            status = ui.main(["--pid", "123", "--start-ticks", "456", "--session-id", "c1", "--uid", "120"])
        self.assertEqual(status, 1)
        self.assertEqual(json.loads(out.getvalue()), ui.unavailable())
        self.assertNotIn("secret", out.getvalue())
        out = io.StringIO()
        with contextlib.redirect_stdout(out): self.assertEqual(ui.main(["--secret-input"]), 1)
        self.assertEqual(json.loads(out.getvalue()), ui.unavailable())


class DirectBusTests(unittest.TestCase):
    def backend(self):
        from types import SimpleNamespace
        backend = ui.NativeBackend.__new__(ui.NativeBackend)
        backend.bound_owner = ":1.42"
        backend.registry_owner = ":1.2"
        backend.roles = {}
        backend.bus = object()
        backend.GLib = SimpleNamespace(Variant=lambda signature, value: (signature, value))
        backend.requests = []
        def dbus(connection, destination, path, interface, method, args=None, signature=None):
            backend.requests.append((destination, path, interface, method, args, signature))
            if method == "GetRole": return backend.role
            if method == "GetState": return [backend.bits, 0]
            if method == "GetChildAtIndex": return backend.child_ref
            if method == "GetExtents": return (1, 2, 3, 4)
            if method == "Get": return "Tuna Desktop"
            raise AssertionError("unapproved query")
        backend.dbus = dbus
        backend.role = 45
        backend.bits = (1 << 25) | (1 << 8) | (1 << 4)
        backend.child_ref = (":1.42", "/child")
        return backend

    def test_role_state_first_and_real_checked_bit(self):
        b = self.backend(); node = (":1.42", "/item")
        role, state = b.inspect(node)
        self.assertEqual(role, "radio-menu-item")
        self.assertTrue(state["selected"])
        self.assertEqual([r[3] for r in b.requests], ["GetRole", "GetState"])
        self.assertEqual(b.public_name(node), "Tuna Desktop")
        self.assertEqual(b.requests[-1][4], ("(ss)", (b.ACCESSIBLE, "Name")))

    def test_password_name_children_and_generic_metadata_are_refused(self):
        b = self.backend(); node = (":1.42", "/entry"); b.role = 40
        self.assertEqual(b.inspect(node)[0], "password-entry")
        for fn in (lambda: b.public_name(node), lambda: b.child_count(node), lambda: b.child(node, 0)):
            with self.assertRaises(ui.Unavailable): fn()
        self.assertEqual([r[3] for r in b.requests], ["GetRole", "GetState"])
        self.assertEqual(b.rect(node), [1, 2, 3, 4])

    def test_name_before_role_is_refused(self):
        b = self.backend()
        with self.assertRaises(ui.Unavailable): b.public_name((":1.42", "/item"))
        self.assertEqual(b.requests, [])

    def test_foreign_owner_refused_before_any_metadata(self):
        b = self.backend()
        with self.assertRaises(ui.Unavailable): b.inspect((":1.99", "/entry"))
        self.assertEqual(b.requests, [])

    def test_foreign_child_reference_refused(self):
        b = self.backend(); b.child_ref = (":1.99", "/child")
        with self.assertRaises(ui.Unavailable): b.child((":1.42", "/parent"), 0)
        self.assertEqual(len(b.requests), 1)

    def test_no_owner_binding_allows_only_registry_root(self):
        b = self.backend(); b.bound_owner = None
        with self.assertRaises(ui.Unavailable): b.inspect((":1.42", "/item"))
        self.assertEqual(b.requests, [])
        b.child_count(b.desktop())
        self.assertEqual(b.requests[0][0], b.registry_owner)

    def test_malformed_state_or_role_is_refused(self):
        for role, bits in ((True, 0), (256, 0), (40, -1), (40, 1 << 32)):
            b = self.backend(); b.role, b.bits = role, bits
            with self.subTest(role=role, bits=bits), self.assertRaises(ui.Unavailable):
                b.inspect((":1.42", "/item"))

    def test_source_has_no_bulk_cache_p2p_or_signal_subscription(self):
        import ast
        source = PATH.read_text()
        tree = ast.parse(source)
        imports = [n for n in ast.walk(tree) if isinstance(n, (ast.Import, ast.ImportFrom))]
        self.assertNotIn("Atspi", " ".join(ast.unparse(n) for n in imports))
        calls = [n for n in ast.walk(tree) if isinstance(n, ast.Call)]
        names = {n.func.attr for n in calls if isinstance(n.func, ast.Attribute)}
        self.assertFalse(names & {"signal_subscribe", "get_desktop", "get_name", "get_application"})
        for prohibited in ("GetAll", "GetItems", "GetApplicationBusAddress"):
            self.assertNotIn('"' + prohibited + '"', source)



if __name__ == "__main__":
    unittest.main()
