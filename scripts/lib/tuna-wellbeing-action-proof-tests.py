#!/usr/bin/env python3
"""Unit tests for tuna-wellbeing-action-proof.py (synthetic dumps only).

Runtime gates press actual banners in the nested harness; these tests
pin the predicates, the NARROWED stale-press rule (retired-UI-absence
only on an alive owned tree with zero transport errors; dead bus and
killed trees fail), and the Delay/Skip due-schedule checks. No cargo,
no compositor required.
"""
import importlib.util
import json
import sys
import tempfile
import time
import unittest
from pathlib import Path

sys.dont_write_bytecode = True

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location(
    'wb_action', HERE / 'tuna-wellbeing-action-proof.py')
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

DUE = 'Time for an Eyesight Break'


def dump(path, *entries):
    path.write_text(json.dumps([
        {'role': 'button', 'name': name, 'showing': show}
        for name, show in entries
    ]))


def cstate(path, locked=False, brightness=1.0):
    path.write_text(json.dumps({
        'locked': locked,
        'wellbeing_effects': {'desaturation': 0.0, 'brightness': brightness,
                              'animating': brightness < 1.0},
    }))


class PinnedBudgets(unittest.TestCase):
    def test_tick_tolerance_not_widened(self):
        self.assertEqual(mod.TICK_TOLERANCE_SECS, 5.0)
        self.assertEqual(mod.PRESS_SETTLE_SECS, 1.5)


class ActionPredicates(unittest.TestCase):
    def test_banner_present_and_absent(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            shown = tmp / 'shown.json'
            dump(shown, (DUE, True), ('Take', True))
            self.assertTrue(mod.banner_present(shown, DUE))
            with self.assertRaises(AssertionError):
                mod.banner_present(shown, 'Nope')
            hidden = tmp / 'hidden.json'
            dump(hidden, (DUE, False))
            self.assertTrue(mod.banner_absent(hidden, DUE))
            with self.assertRaises(AssertionError):
                mod.banner_absent(shown, DUE)

    def test_lock_and_fade_samples(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            locked = tmp / 'locked.json'
            cstate(locked, locked=True)
            mod.check_locked(locked, True)
            with self.assertRaises(AssertionError):
                mod.check_locked(locked, False)
            faded = tmp / 'faded.json'
            cstate(faded, brightness=0.6)
            mod.check_fade_sample(faded, True)
            with self.assertRaises(AssertionError):
                mod.check_fade_sample(faded, False)
            neutral = tmp / 'neutral.json'
            cstate(neutral, brightness=1.0)
            mod.check_fade_sample(neutral, False)
            with self.assertRaises(AssertionError):
                mod.check_fade_sample(neutral, True)

    def test_malformed_cstate_is_harness_error(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            with self.assertRaises(mod.HarnessError):
                mod.check_locked(tmp / 'gone.json', True)
            broken = tmp / 'broken.json'
            broken.write_text('{oops')
            with self.assertRaises(mod.HarnessError):
                mod.check_fade_sample(broken, True)

    def test_delay_moves_live_due_by_delay_secs(self):
        self.assertAlmostEqual(mod.check_delay(1000.0, 1030.0, 30), 30.0)
        with self.assertRaises(AssertionError):
            mod.check_delay(1000.0, 1010.0, 30)
        with self.assertRaises(AssertionError):
            mod.check_delay(1000.0, 1060.0, 30)

    def test_skip_restarts_interval_at_press(self):
        self.assertAlmostEqual(mod.check_skip(1002.0, 1012.0, 10), 10.0)
        with self.assertRaises(AssertionError):
            mod.check_skip(1002.0, 1032.0, 10)

    def test_delay_and_skip_are_distinguishable(self):
        # delay 30 vs interval 10 cannot both accept the same redue time.
        with self.assertRaises(AssertionError):
            mod.check_delay(1000.0, 1012.0, 30)
        with self.assertRaises(AssertionError):
            mod.check_skip(1002.0, 1030.0, 10)


class NarrowedStalePress(unittest.TestCase):
    def stale(self, tmp, exit_code=1, locked=False, brightness=1.0,
              moved_locked=False, moved_bright=None, banner_showing=False,
              transport_error=False, empty_tree=False):
        before, after = tmp / 'before.json', tmp / 'after.json'
        cstate(before, locked=locked, brightness=brightness)
        cstate(after, locked=moved_locked if moved_locked else locked,
               brightness=moved_bright if moved_bright is not None else brightness)
        retired = tmp / 'retired.json'
        if empty_tree:
            retired.write_text('[]')
        else:
            nodes = [{'role': 'button', 'name': DUE, 'showing': banner_showing},
                     {'role': 'button', 'name': 'Take', 'showing': False}]
            if transport_error:
                nodes.append({'role': 'error', 'name': 'bus died', 'depth': 0,
                              'showing': False})
            retired.write_text(json.dumps(nodes))
        return before, after, retired

    def test_stale_press_must_fail_without_state_change(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            gate_start = time.time() - 1
            before, after, retired = self.stale(tmp)
            self.assertTrue(mod.check_stale_press_fails(
                1, before, after, 'Take', retired, DUE, gate_start))
            with self.assertRaises(AssertionError):
                mod.check_stale_press_fails(
                    0, before, after, 'Take', retired, DUE, gate_start)

    def test_stale_press_must_not_move_lock_or_fade(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            gate_start = time.time() - 1
            before, after, retired = self.stale(tmp, moved_locked=True)
            with self.assertRaises(AssertionError):
                mod.check_stale_press_fails(
                    1, before, after, 'Take', retired, DUE, gate_start)
            before, after, retired = self.stale(tmp, moved_bright=0.6)
            with self.assertRaises(AssertionError):
                mod.check_stale_press_fails(
                    1, before, after, 'Take', retired, DUE, gate_start)

    def test_retired_banner_still_showing_fails(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            gate_start = time.time() - 1
            before, after, retired = self.stale(tmp, banner_showing=True)
            with self.assertRaises(AssertionError):
                mod.check_stale_press_fails(
                    1, before, after, 'Take', retired, DUE, gate_start)

    def test_dead_bus_fails_the_gate(self):
        # No dump file: the bus is dead or the dump timed out. Absence
        # must never be inferred from a missing tree.
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            gate_start = time.time() - 1
            before, after, _ = self.stale(tmp)
            with self.assertRaises(mod.HarnessError):
                mod.check_stale_press_fails(
                    1, before, after, 'Take', tmp / 'gone.json', DUE, gate_start)

    def test_killed_tree_fails_the_gate(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            gate_start = time.time() - 1
            before, after, retired = self.stale(tmp, empty_tree=True)
            with self.assertRaises(mod.HarnessError):
                mod.check_stale_press_fails(
                    1, before, after, 'Take', retired, DUE, gate_start)

    def test_transport_errors_fail_the_gate(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            gate_start = time.time() - 1
            before, after, retired = self.stale(tmp, transport_error=True)
            with self.assertRaises(mod.HarnessError):
                mod.check_stale_press_fails(
                    1, before, after, 'Take', retired, DUE, gate_start)

    def test_stale_dump_fails(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after, retired = self.stale(tmp)
            with self.assertRaises(AssertionError):
                mod.check_stale_press_fails(
                    1, before, after, 'Take', retired, DUE,
                    time.time() + 100)

    def test_delay_stale_leaves_due_schedule_undisturbed(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            gate_start = time.time() - 1
            before, after, retired = self.stale(tmp)
            self.assertTrue(mod.check_stale_press_fails(
                1, before, after, 'Delay', retired, DUE, gate_start,
                due=('delay', 1000.0, 1030.0, 30)))
            with self.assertRaises(AssertionError):
                mod.check_stale_press_fails(
                    1, before, after, 'Delay', retired, DUE, gate_start,
                    due=('delay', 1000.0, 1012.0, 30))

    def test_skip_stale_leaves_interval_restart_undisturbed(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            gate_start = time.time() - 1
            before, after, retired = self.stale(tmp)
            self.assertTrue(mod.check_stale_press_fails(
                1, before, after, 'Skip', retired, DUE, gate_start,
                due=('skip', 1002.0, 1012.0, 10)))
            with self.assertRaises(AssertionError):
                mod.check_stale_press_fails(
                    1, before, after, 'Skip', retired, DUE, gate_start,
                    due=('skip', 1002.0, 1030.0, 10))

    def test_harness_error_is_not_a_pixel_assertion(self):
        self.assertFalse(issubclass(mod.HarnessError, AssertionError))


if __name__ == '__main__':
    unittest.main()
