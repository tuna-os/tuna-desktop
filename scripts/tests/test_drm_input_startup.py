#!/usr/bin/env python3
"""Unit tests of D-INPUT-AT-STARTUP's check (scripts/lib/drm_input_startup.py).

The old-behaviour case is the state main's drm-smoke main session wrote
(run 44856969073): `input_devices` empty although the runner had a mouse
and a keyboard, which its tablet session listed once a hotplug woke
libinput. These tests do not substitute for the drm-smoke lane.
"""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / "scripts/lib/drm_input_startup.py"
sys.path.insert(0, str(ROOT / "scripts/lib"))
import drm_input_startup as startup  # noqa: E402

NAME = "Tuna startup mouse"
HOST = [{"name": "Microsoft Vmbus HID-compliant Mouse", "kind": "pointer"},
        {"name": "AT Translated Set 2 keyboard", "kind": "other"}]


def run_check(state):
    with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as stream:
        json.dump(state, stream)
    try:
        return subprocess.run([sys.executable, str(SCRIPT), "check", stream.name, NAME],
                              capture_output=True, text=True)
    finally:
        Path(stream.name).unlink()


class Check(unittest.TestCase):
    def test_lists_the_device_present_before_startup(self):
        state = {"input_devices": HOST + [{"name": NAME, "kind": "pointer"}]}
        self.assertIsNone(startup.problem(state, NAME))
        result = run_check(state)
        self.assertEqual(result.returncode, 0, result.stdout)
        self.assertIn("listed as pointer among 3 input devices", result.stdout)

    def test_old_behaviour_lists_no_device_at_all(self):
        # main's run 44856969073, compositor-state.json: input_devices [].
        state = {"input_devices": []}
        self.assertIn("is not among the input devices []", startup.problem(state, NAME))
        self.assertEqual(run_check(state).returncode, 1)

    def test_host_devices_alone_do_not_pass(self):
        self.assertIsNotNone(startup.problem({"input_devices": HOST}, NAME))

    def test_a_missing_or_partial_state_fails(self):
        self.assertEqual(startup.problem({}, NAME), "the state has no input_devices")
        self.assertEqual(run_check({}).returncode, 1)
        result = subprocess.run([sys.executable, str(SCRIPT), "check", "/nonexistent.json", NAME],
                                capture_output=True, text=True)
        self.assertEqual(result.returncode, 1)
        self.assertIn("no compositor state", result.stdout)

    def test_usage(self):
        result = subprocess.run([sys.executable, str(SCRIPT)], capture_output=True, text=True)
        self.assertEqual(result.returncode, 2)


if __name__ == "__main__":
    unittest.main()
