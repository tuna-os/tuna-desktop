#!/usr/bin/env python3
"""Exercise the real focus proof predicates with captured/boundary timelines.

The GTK/input plumbing is replaced only in this predicate unit test. This
never emits a live proof receipt and does not substitute for the real-key
pixel journey. Late onset must fail as onset, not masquerade as expiration.
"""
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
LIBRARY = ROOT / "scripts/lib/tuna-a11y-focus-proof.sh"


def control(onset=None, expiry=None):
    return {"lit_before": False, "lit_from": onset, "lit_until": expiry}


class FocusPredicates(unittest.TestCase):
    def run_gate(self, label=None, name=None, onset=None, expiry=None):
        fixtures = {
            "default": {"act": control(.09, 3.06), "clock": control()},
            "two": {"act": control(), "clock": control(.136, 2.142)},
            "five": {"act": control(.136, 5.085), "clock": control()},
            "forever": {"act": control(), "clock": control(.136)},
        }
        if label is not None:
            fixtures[label][name] = control(onset, expiry)
        with tempfile.TemporaryDirectory() as directory:
            for key, values in fixtures.items():
                Path(directory, f"focus-ring-{key}.json").write_text(
                    json.dumps({"controls": values}))
            return subprocess.run([
                "/bin/sh", "-c", '''
set -eu
. "$1"
ARTIFACTS="$2" WIDTH=1280 HEIGHT=800 WID=1
xdotool() { :; }
gsettings() { :; }
sleep() { :; }
cstate() { :; }
a11y_focus_timeline() { :; }
a11y_focus_dump() { :; }
a11y_focus_rect() { echo 0,0,100,28; }
focused() {
    case "$1:$2" in *-two.json:Activities) return 1 ;; esac
}
pass() { :; }
fail() { printf '%s: %s\\n' "$1" "$2"; exit 1; }
a11y_focus_ring_proof
''', "focus-predicate-test", str(LIBRARY), directory],
                text=True, capture_output=True, timeout=10)

    def test_all_original_windows_pass(self):
        result = self.run_gate()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_captured_970_forever_is_late_not_expired(self):
        result = self.run_gate("forever", "clock", .869, None)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("G-FOCUS-RING: Tab did not light the clock within 0.8 s", result.stdout)
        self.assertNotIn("let the indication clear", result.stdout)
        self.assertIn('"lit_from": 0.869', result.stderr)
        self.assertIn('"lit_until": null', result.stderr)

    def test_captured_867_five_is_late_not_early_expiry(self):
        result = self.run_gate("five", "act", 1.077, 5.411)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("G-FOCUS-RING: Shift+Tab did not light Activities within 0.8 s", result.stdout)
        self.assertNotIn("past the toolkit", result.stdout)

    def test_forever_expiring_still_fails_timeout(self):
        result = self.run_gate("forever", "clock", .136, 3.1)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("G-FOCUS-RING-TIMEOUT: a zero timeout let the indication clear", result.stdout)

    def test_five_expiring_at_toolkit_default_still_fails_timeout(self):
        result = self.run_gate("five", "act", .136, 3.1)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("G-FOCUS-RING-TIMEOUT: a 5 s timeout", result.stdout)

    def test_five_never_expiring_still_fails_timeout(self):
        result = self.run_gate("five", "act", .136, None)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("G-FOCUS-RING-TIMEOUT: a 5 s timeout", result.stdout)

    def test_missing_onset_still_fails(self):
        result = self.run_gate("forever", "clock", None, None)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("G-FOCUS-RING: Tab did not light the clock within 0.8 s", result.stdout)

    def test_exact_onset_limit_passes_and_later_fails(self):
        self.assertEqual(self.run_gate("forever", "clock", .8, None).returncode, 0)
        self.assertNotEqual(self.run_gate("forever", "clock", .801, None).returncode, 0)

    def test_exact_expiry_limits_pass(self):
        for boundary in (4.8, 5.9):
            with self.subTest(boundary=boundary):
                result = self.run_gate("five", "act", .136, boundary)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_outside_expiry_limits_fail(self):
        for boundary in (4.799, 5.901):
            with self.subTest(boundary=boundary):
                self.assertNotEqual(self.run_gate("five", "act", .136, boundary).returncode, 0)


if __name__ == "__main__":
    unittest.main()
