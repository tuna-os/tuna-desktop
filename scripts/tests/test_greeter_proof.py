#!/usr/bin/env python3
"""Source-owned wiring and fixture admission regressions; not a runtime claim."""
import pathlib
import subprocess
import uuid
import unittest
ROOT = pathlib.Path(__file__).resolve().parents[2]

class GreeterProof(unittest.TestCase):
    def test_main_owns_controller_until_application_shutdown(self):
        main = (ROOT / 'crates/greeter/src/main.rs').read_text()
        self.assertIn('Vec::<login::LoginWindow>', main)
        self.assertIn('login::build_login(app, sessions.entries, socket)', main)
        self.assertIn('connect_shutdown', main)
        self.assertNotIn('ui::build_ui', main)

    def test_shutdown_takes_stream_before_tail_expression_or_socket_shutdown(self):
        login = (ROOT / 'crates/greeter/src/login.rs').read_text()
        self.assertNotIn('if let Some(stream) = state.interrupt.lock()', login)
        self.assertEqual(login.count('let interrupt = state.interrupt.lock().expect("interrupt lock").take();'), 2)
        self.assertEqual(login.count('if let Some(stream) = interrupt {'), 2)

    def test_standalone_greeter_declares_used_accessibility_api(self):
        import tomllib
        manifest = tomllib.loads((ROOT / 'crates/greeter/Cargo.toml').read_text())
        gtk = manifest['dependencies']['gtk4']
        self.assertIn('v4_10', gtk['features'])
        self.assertTrue(gtk['optional'])
        self.assertIn('dep:gtk4', manifest['features']['gtk-ui'])
        self.assertIn('cargo test -p tuna-greeter --test gtk_login',
                      (ROOT / 'scripts/tuna-greeter-proof').read_text())


    def test_admission_rejects_checkout_artifacts_and_missing_execution(self):
        proof = (ROOT / 'scripts/tuna-greeter-proof').read_text()
        for marker in ['realpath -m', 'outside source checkout', "'crates/greeter/src/lib.rs'", "'crates/greeter/src/ui.rs'", "'1 passed; 0 failed; 0 ignored;'", "['git','diff','--quiet','HEAD']"]:
            self.assertIn(marker, proof)
        forbidden = ROOT / ('.greeter-proof-' + uuid.uuid4().hex)
        result = subprocess.run([str(ROOT / 'scripts/tuna-greeter-proof'), str(forbidden)], cwd=ROOT, capture_output=True, text=True, check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('outside source checkout', result.stderr)
        self.assertFalse(forbidden.exists())

    def test_real_widgets_and_wire_negative_cases_remain(self):
        test = (ROOT / 'crates/greeter/tests/gtk_login.rs').read_text()
        for marker in ['UnixListener::bind', 'Request::read_from', 'AuthMessageType::Secret', 'AuthMessageType::Visible', 'AuthMessageType::Info', 'AuthMessageType::Error', '"cancel"', '"start"', 'start-rejected', 'emit_clicked', 'scrot', 'gtk4::prelude::EntryExt::is_visible', 'app.run_with_args']:
            self.assertIn(marker, test)

if __name__ == '__main__':
    unittest.main()
