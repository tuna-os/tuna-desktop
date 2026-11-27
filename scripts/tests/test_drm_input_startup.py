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


SMOKE = ROOT / "scripts/tuna-drm-smoke"
OPEN_PIPE = 'exec 7>"$ARTIFACTS/startup-device.fifo"'


def background_jobs_while_the_pipe_is_open(text):
    """Every command tuna-drm-smoke starts in the background between
    opening the startup mouse's pipe (fd 7) and closing it, as one logical
    line (backslash continuations joined)."""
    logical, current = [], ""
    for line in text.splitlines():
        if line.endswith("\\"):
            current += line[:-1].rstrip() + " "
            continue
        logical.append((current + line).strip())
        current = ""
    start = logical.index(OPEN_PIPE)
    end = next(i for i in range(start + 1, len(logical)) if logical[i] == "exec 7>&-")
    return [line for line in logical[start + 1:end]
            if line.endswith("&") and not line.endswith("&&")]


class PipeHolders(unittest.TestCase):
    """The holder unplugs the mouse when the pipe reaches end-of-file, so a
    background process started while fd 7 is open must not inherit it: on
    run 44875566230 the compositor and its shell kept it open, the mouse
    never unplugged and the job hung until it was cancelled."""

    def test_background_jobs_close_the_startup_pipe(self):
        jobs = background_jobs_while_the_pipe_is_open(SMOKE.read_text())
        self.assertTrue(any("tuna-compositor" in job for job in jobs), jobs)
        for job in jobs:
            with self.subTest(job=job[:80]):
                self.assertIn("7>&-", job)

    def test_the_check_catches_an_inherited_pipe(self):
        hung = "\n".join([
            OPEN_PIPE,
            'env A=1 \\',
            '    "$ROOT/target/debug/tuna-compositor" --backend drm >"$LOG" 2>&1 &',
            'exec 7>&-',
        ])
        jobs = background_jobs_while_the_pipe_is_open(hung)
        self.assertEqual(len(jobs), 1)
        self.assertNotIn("7>&-", jobs[0])

    def test_the_unplug_wait_is_bounded(self):
        text = SMOKE.read_text()
        unplug = text[text.index("# Unplug it now"):text.index('sleep 2\n[ -s "$CSTATE" ]')]
        self.assertIn("not unplugged within 10 s", unplug)
        self.assertLess(unplug.index('-gt "$end"'), unplug.index('wait "$STARTUP_DEVICE_PID"'))


if __name__ == "__main__":
    unittest.main()
