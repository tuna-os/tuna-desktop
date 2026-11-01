#!/usr/bin/env python3
"""Exercise the real focus proof predicates with captured/boundary timelines.

The GTK/input plumbing is replaced only in this predicate unit test. This
never emits a live proof receipt and does not substitute for the real-key
pixel journey. Late onset must fail as onset, not masquerade as expiration.
"""
import contextlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
LIBRARY = ROOT / "scripts/lib/tuna-a11y-focus-proof.sh"
SPEC = importlib.util.spec_from_file_location(
    "focus_timeline", ROOT / "scripts/lib/tuna-focus-ring-timeline.py")
TIMELINE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TIMELINE)


def sample(begin, end, is_lit):
    return {"capture_begin_s": begin, "capture_end_s": end,
            "pills": [[0, 100]] if is_lit else [], "lit": {"target": is_lit}}


def control(onset=None, expiry=None, *, onset_begin=None, last_lit_begin=None,
            expiry_begin=None):
    frames = []
    if onset is not None:
        frames.append(sample(onset - .005 if onset_begin is None else onset_begin,
                             onset, True))
        if expiry is not None:
            lower = expiry - .02 if last_lit_begin is None else last_lit_begin
            frames.append(sample(lower, lower + .001, True))
            frames.append(sample(expiry - .005 if expiry_begin is None else expiry_begin,
                                 expiry, False))
    return TIMELINE.summarize_control(frames, "target", [0, 0, 100, 28], False)


class FocusPredicates(unittest.TestCase):
    def run_gate(self, label=None, name=None, onset=None, expiry=None, **bounds):
        fixtures = {
            "default": {"act": control(.09, 3.06), "clock": control()},
            "two": {"act": control(), "clock": control(.136, 2.142)},
            "five": {"act": control(.136, 5.085), "clock": control()},
            "forever": {"act": control(), "clock": control(.136)},
        }
        if label is not None:
            fixtures[label][name] = control(onset, expiry, **bounds)
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
        self.assertIn("G-FOCUS-RING: could not bound clock focus onset within 0.8 s", result.stdout)
        self.assertNotIn("let the indication clear", result.stdout)
        self.assertIn('"capture_end_s": 0.869', result.stderr)
        self.assertIn('"first_unlit": null', result.stderr)

    def test_captured_867_five_is_late_not_early_expiry(self):
        result = self.run_gate("five", "act", 1.077, 5.411)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("G-FOCUS-RING: could not bound Activities focus onset within 0.8 s", result.stdout)
        self.assertNotIn("G-FOCUS-RING-TIMEOUT:", result.stdout)

    def test_forever_expiring_still_fails_timeout(self):
        result = self.run_gate("forever", "clock", .136, 3.1)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("G-FOCUS-RING-TIMEOUT: a zero timeout let the indication clear", result.stdout)

    def test_five_expiring_at_toolkit_default_still_fails_timeout(self):
        result = self.run_gate("five", "act", .136, 3.1)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("G-FOCUS-RING-TIMEOUT: could not bound 5 s focus expiry", result.stdout)

    def test_five_never_expiring_still_fails_timeout(self):
        result = self.run_gate("five", "act", .136, None)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("G-FOCUS-RING-TIMEOUT: could not bound 5 s focus expiry", result.stdout)

    def test_missing_onset_still_fails(self):
        result = self.run_gate("forever", "clock", None, None)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("G-FOCUS-RING: could not bound clock focus onset within 0.8 s", result.stdout)

    def test_exact_onset_limit_passes_and_later_fails(self):
        for label, name, expiry in (("default", "act", 3.06), ("two", "clock", 2.142),
                                    ("five", "act", 5.085), ("forever", "clock", None)):
            with self.subTest(label=label):
                result = self.run_gate(label, name, .8, expiry)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                result = self.run_gate(label, name, .8000001, expiry)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("G-FOCUS-RING:", result.stdout)
                self.assertNotIn("G-FOCUS-RING-TIMEOUT:", result.stdout)

    def test_exact_expiry_limits_pass(self):
        for label, name, lower, upper in (
                ("default", "act", 2.8, 3.9),
                ("two", "clock", 1.8, 2.9),
                ("five", "act", 4.8, 5.9)):
            with self.subTest(label=label):
                result = self.run_gate(label, name, .136, upper, last_lit_begin=lower)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_outside_expiry_limits_fail(self):
        for label, name, lower, upper in (
                ("default", "act", 2.8, 3.9),
                ("two", "clock", 1.8, 2.9),
                ("five", "act", 4.8, 5.9)):
            for last_lit, first_unlit in ((lower - .001, lower + .02),
                                          (upper - .02, upper + .001)):
                with self.subTest(label=label, bracket=(last_lit, first_unlit)):
                    result = self.run_gate(label, name, .136, first_unlit,
                                           last_lit_begin=last_lit)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn("G-FOCUS-RING-TIMEOUT:", result.stdout)

    def test_installed_cad_boundary_crossing_is_not_a_pass(self):
        # Installed CAD source 1520/run 41251798448: the original sampled
        # transition (2.875, 2.919] crosses 2.9; 2.919 is not actual expiry.
        result = self.run_gate("two", "clock", .529, 2.919,
                               last_lit_begin=2.875, expiry_begin=2.909)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("G-FOCUS-RING-TIMEOUT:", result.stdout)
        self.assertIn('"capture_begin_s": 2.875', result.stderr)
        self.assertIn('"capture_end_s": 2.919', result.stderr)

    def test_unlit_capture_start_inside_end_outside_rejects(self):
        result = self.run_gate("two", "clock", .529, 2.901,
                               last_lit_begin=2.88, expiry_begin=2.899)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("G-FOCUS-RING-TIMEOUT:", result.stdout)

    def test_lit_capture_begin_below_expiry_floor_rejects(self):
        result = self.run_gate("two", "clock", .136, 1.82, last_lit_begin=1.799)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("G-FOCUS-RING-TIMEOUT:", result.stdout)

    def test_onset_capture_start_inside_end_outside_rejects(self):
        for label, name in (("default", "act"), ("two", "clock"),
                            ("five", "act"), ("forever", "clock")):
            with self.subTest(label=label):
                result = self.run_gate(label, name, .801, onset_begin=.799)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("G-FOCUS-RING:", result.stdout)
                self.assertNotIn("G-FOCUS-RING-TIMEOUT:", result.stdout)

    def test_sampled_valid_transition_bracket_passes(self):
        result = self.run_gate("two", "clock", .529, 2.899,
                               last_lit_begin=2.875, expiry_begin=2.895)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_default_cad_bracket_still_passes(self):
        result = self.run_gate("default", "act", .698, 3.509, last_lit_begin=3.495)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_last_lit_is_before_first_expiry_not_a_later_relight(self):
        frames = [sample(.1, .11, True), sample(2.875, 2.88, True),
                  sample(2.91, 2.919, False), sample(3.0, 3.01, True)]
        result = TIMELINE.summarize_control(frames, "target", [0, 0, 100, 28], False)
        self.assertEqual(result["first_lit"]["capture_end_s"], .11)
        self.assertEqual(result["last_lit"]["capture_begin_s"], 2.875)
        self.assertEqual(result["first_unlit"]["capture_end_s"], 2.919)


class TimelineCaptureBounds(unittest.TestCase):
    def capture(self, seconds):
        clock = SimpleNamespace(now=100.0, pauses=[], captures=[])

        def monotonic():
            return clock.now

        def sleep(delay):
            clock.pauses.append(delay)
            clock.now += delay

        class Frame:
            def convert(self, mode):
                clock.now += .001
                return self

            def save(self, path):
                pass

        def grab(**kwargs):
            begin = clock.now
            clock.now += .002
            clock.captures.append((begin, clock.now))
            return Frame()

        def key_command(*args, **kwargs):
            clock.now += .125

        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory, "timeline.json")
            with patch.dict(sys.modules, {"PIL": SimpleNamespace(
                    ImageGrab=SimpleNamespace(grab=grab))}), \
                    patch.object(TIMELINE.sys, "argv", [
                        "timeline", ":99", str(out), str(seconds), "Tab", "act=0,0,10,28"]), \
                    patch.object(TIMELINE.time, "monotonic", monotonic), \
                    patch.object(TIMELINE.time, "sleep", sleep), \
                    patch.object(TIMELINE.subprocess, "run", key_command), \
                    patch.object(TIMELINE, "lit_pills", return_value=[[0, 10]]), \
                    contextlib.redirect_stdout(io.StringIO()):
                TIMELINE.main()
            return json.loads(out.read_text()), clock

    def test_command_return_origin_and_unrounded_capture_bounds(self):
        receipt, clock = self.capture(.04)
        command = receipt["command"]
        self.assertEqual(receipt["clock"], "monotonic")
        self.assertEqual(receipt["origin"], "xdotool-command-return")
        self.assertEqual(receipt["origin_monotonic_s"], command["end_monotonic_s"])
        self.assertAlmostEqual(command["end_monotonic_s"] - command["begin_monotonic_s"], .125)
        self.assertAlmostEqual(command["begin_monotonic_s"], 100.003)
        self.assertAlmostEqual(command["end_monotonic_s"], 100.128)
        self.assertLess(receipt["before_capture"]["capture_end_s"], 0)
        first = receipt["frames"][0]
        origin = receipt["origin_monotonic_s"]
        self.assertEqual(first["capture_begin_s"], clock.captures[1][0] - origin)
        self.assertEqual(first["capture_end_s"], clock.captures[1][1] - origin)
        self.assertEqual(receipt["controls"]["act"]["first_lit"]["capture_end_s"],
                         first["capture_end_s"])
        self.assertNotEqual(first["capture_end_s"], round(first["capture_end_s"], 3))
        self.assertEqual(receipt["sample_pause_s"], .01)
        self.assertTrue(all(0 < pause <= .01 for pause in clock.pauses))

    def test_original_total_windows_remain_bounded(self):
        for seconds in (5, 4, 7, 8):
            with self.subTest(seconds=seconds):
                receipt, clock = self.capture(seconds)
                self.assertEqual(receipt["seconds"], seconds)
                self.assertTrue(receipt["frames"])
                self.assertTrue(all(0 <= frame["capture_begin_s"] <=
                                    frame["capture_end_s"] <= seconds
                                    for frame in receipt["frames"]))
                self.assertAlmostEqual(clock.now - receipt["origin_monotonic_s"],
                                       seconds, delta=.003)

    def test_capture_finishing_outside_window_is_excluded(self):
        receipt, clock = self.capture(.001)
        self.assertEqual(len(clock.captures), 2)  # Before-key plus overrun capture.
        self.assertEqual(receipt["frames"], [])
        self.assertIsNone(receipt["controls"]["act"]["first_lit"])


if __name__ == "__main__":
    unittest.main()
