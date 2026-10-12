#!/usr/bin/env python3
"""Source-owned wiring and fixture admission regressions; not a runtime claim."""
import pathlib
import unittest
ROOT = pathlib.Path(__file__).resolve().parents[2]

class GreeterProof(unittest.TestCase):
    def test_main_owns_controller_until_application_shutdown(self):
        main = (ROOT / 'crates/greeter/src/main.rs').read_text()
        self.assertIn('Vec::<login::LoginWindow>', main)
        self.assertIn('login::build_login(app, sessions.entries, socket)', main)
        self.assertIn('connect_shutdown', main)
        self.assertNotIn('ui::build_ui', main)

    def test_ci_captures_failure_receipts_after_source_clean_freeze(self):
        ci = (ROOT / '.github/workflows/ci.yml').read_text()
        self.assertLess(ci.index('scripts/tuna-minimize-ci-proof'), ci.index('scripts/tuna-greeter-proof'))
        self.assertLess(ci.index('scripts/tuna-greeter-proof'), ci.index('Prove live accessibility'))
        self.assertIn('name: native-greeter-proof', ci)
        proof = (ROOT / 'scripts/tuna-greeter-proof').read_text()
        self.assertIn('git status --porcelain', proof)
        self.assertIn("m.get('executable')", proof)
        self.assertIn('timeout --kill-after=5s 90s', proof)
        self.assertIn("r['runtime_passed']", proof)

    def test_real_widgets_and_wire_negative_cases_remain(self):
        test = (ROOT / 'crates/greeter/tests/gtk_login.rs').read_text()
        for marker in ['UnixListener::bind', 'Request::read_from', 'AuthMessageType::Secret', 'AuthMessageType::Visible', 'AuthMessageType::Info', 'AuthMessageType::Error', '"cancel"', '"start"', 'start-rejected', 'emit_clicked', 'scrot', 'gtk4::prelude::EntryExt::is_visible', 'app.run_with_args']:
            self.assertIn(marker, test)

if __name__ == '__main__':
    unittest.main()
