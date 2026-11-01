#!/usr/bin/env python3
"""Unit tests for tuna-wellbeing-action-proof.py (synthetic dumps only).

Runtime gates press actual banners in the nested harness; these tests
pin the predicates, the NARROWED stale-press rule (retired-UI-absence
only on an alive owned tree with zero transport errors; dead bus and
killed trees fail), and the Delay/Skip due-schedule checks. No cargo,
no compositor required.
"""
import hashlib
import importlib.util
import json
import os
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

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
        self.assertEqual(mod.SKIP_SCHEDULE_TOLERANCE_SECS, 8.0)


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


GATE = 'G-TEST'
NONCE = 'nonce-test-1'


def owner_identity():
    pid = os.getpid()
    return pid, mod.proc_start_ticks(pid, 'test')


def write_stamp(path, pid, start, gate=GATE, nonce=NONCE, epoch=None,
                seq=1, dump_sha256=None):
    if dump_sha256 is None:
        raise TypeError('a test stamp must bind exact dump bytes (dump_sha256)')
    path.write_text(json.dumps({
        'gate': gate, 'nonce': nonce, 'seq': seq, 'pid': pid,
        'start_ticks': start,
        'epoch': time.time() if epoch is None else epoch,
        'dump_sha256': dump_sha256,
    }))
    return path


def bound_stamp(path, dump_path, pid, start, **over):
    """A stamp bound to the actual bytes on disk, like wb_stamp_dump."""
    sha = hashlib.sha256(Path(dump_path).read_bytes()).hexdigest()
    return write_stamp(path, pid, start, dump_sha256=sha, **over)


def bytes_sha(nodes):
    """The dump_sha256 check_dump will bind for these exact nodes."""
    return hashlib.sha256(json.dumps(nodes).encode()).hexdigest()


def owned_nodes(pid, banner_showing=False, anchor=True):
    """Actual producer shapes: full typed records, app root + buttons."""
    nodes = [{'role': 'application', 'name': 'tuna-shell-gtk',
              'description': '', 'depth': 0, 'showing': True,
              'checked': False, 'selected': False, 'focused': False,
              'bounds': None, 'process_id': pid},
             {'role': 'button', 'name': DUE, 'description': '',
              'depth': 1, 'showing': banner_showing,
              'checked': False, 'selected': False, 'focused': False,
              'bounds': [10, 10, 220, 48]},
             {'role': 'button', 'name': 'Take', 'description': '',
              'depth': 1, 'showing': anchor,
              'checked': False, 'selected': False, 'focused': False,
              'bounds': [10, 70, 220, 48]}]
    return nodes


class NarrowedStalePress(unittest.TestCase):
    def stale(self, tmp, exit_code=1, locked=False, brightness=1.0,
              moved_locked=False, moved_bright=None, banner_showing=False,
              transport_error=False, empty_tree=False):
        pid, start = owner_identity()
        gate_start = time.time() - 1
        before, after = tmp / 'before.json', tmp / 'after.json'
        cstate(before, locked=locked, brightness=brightness)
        cstate(after, locked=moved_locked if moved_locked else locked,
               brightness=moved_bright if moved_bright is not None else brightness)
        retired = tmp / 'retired.json'
        if empty_tree:
            retired.write_text('[]')
        else:
            nodes = owned_nodes(pid, banner_showing=banner_showing)
            if transport_error:
                nodes.append({'role': 'error', 'name': 'bus died', 'depth': 0,
                              'showing': False})
            retired.write_text(json.dumps(nodes))
        stamp = bound_stamp(tmp / 'retired.json.stamp', retired, pid, start)
        owned = {'expected_pid': pid, 'expected_start_ticks': start,
                 'stamp_path': stamp, 'expected_gate': GATE,
                 'expected_nonce': NONCE, 'expected_seq': 1}
        return before, after, retired, gate_start, owned

    def check(self, exit_code, before, after, retired, gate_start, owned,
              action='Take', due=None):
        return mod.check_stale_press_fails(
            exit_code, before, after, action, retired, DUE, gate_start, due,
            **owned)

    def test_stale_press_must_fail_without_state_change(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after, retired, gate_start, owned = self.stale(tmp)
            self.assertTrue(self.check(1, before, after, retired,
                                       gate_start, owned))
            with self.assertRaises(AssertionError):
                self.check(0, before, after, retired, gate_start, owned)

    def test_stale_press_must_not_move_lock_or_fade(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after, retired, gate_start, owned = self.stale(
                tmp, moved_locked=True)
            with self.assertRaises(AssertionError):
                self.check(1, before, after, retired, gate_start, owned)
            before, after, retired, gate_start, owned = self.stale(
                tmp, moved_bright=0.6)
            with self.assertRaises(AssertionError):
                self.check(1, before, after, retired, gate_start, owned)

    def test_retired_banner_still_showing_fails(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after, retired, gate_start, owned = self.stale(
                tmp, banner_showing=True)
            with self.assertRaises(AssertionError):
                self.check(1, before, after, retired, gate_start, owned)

    def test_dead_bus_fails_the_gate(self):
        # No dump file: the bus is dead or the dump timed out. Absence
        # must never be inferred from a missing tree.
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after, _, gate_start, owned = self.stale(tmp)
            with self.assertRaises(mod.HarnessError):
                self.check(1, before, after, tmp / 'gone.json',
                           gate_start, owned)

    def test_killed_tree_fails_the_gate(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after, retired, gate_start, owned = self.stale(
                tmp, empty_tree=True)
            with self.assertRaises(mod.HarnessError):
                self.check(1, before, after, retired, gate_start, owned)

    def test_transport_errors_fail_the_gate(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after, retired, gate_start, owned = self.stale(
                tmp, transport_error=True)
            with self.assertRaises(mod.HarnessError):
                self.check(1, before, after, retired, gate_start, owned)

    def test_stale_observation_fails(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after, retired, _, owned = self.stale(tmp)
            with self.assertRaises(mod.HarnessError):
                self.check(1, before, after, retired,
                           time.time() + 100, owned)

    def test_stale_dump_mtime_fails(self):
        # A fresh stamp with a backdated dump file still fails on mtime.
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after, retired, gate_start, owned = self.stale(tmp)
            old = time.time() - 100
            os.utime(retired, (old, old))
            with self.assertRaises(AssertionError):
                self.check(1, before, after, retired, gate_start, owned)

    def test_delay_stale_leaves_due_schedule_undisturbed(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after, retired, gate_start, owned = self.stale(tmp)
            self.assertTrue(self.check(
                1, before, after, retired, gate_start, owned,
                action='Delay', due=('delay', 1000.0, 1030.0, 30)))
            with self.assertRaises(AssertionError):
                self.check(1, before, after, retired, gate_start, owned,
                           action='Delay',
                           due=('delay', 1000.0, 1012.0, 30))

    def test_skip_stale_leaves_interval_restart_undisturbed(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after, retired, gate_start, owned = self.stale(tmp)
            self.assertTrue(self.check(
                1, before, after, retired, gate_start, owned,
                action='Skip', due=('skip', 1002.0, 1012.0, 10)))
            with self.assertRaises(AssertionError):
                self.check(1, before, after, retired, gate_start, owned,
                           action='Skip', due=('skip', 1002.0, 1030.0, 10))

    def test_harness_error_is_not_a_pixel_assertion(self):
        self.assertFalse(issubclass(mod.HarnessError, AssertionError))


class PresenceFirstStaleState(unittest.TestCase):
    """Both snapshots must carry typed state before any equality (step 5)."""

    def owned(self, tmp, stamp_epoch=None):
        pid, start = owner_identity()
        gate_start = time.time() - 1
        retired = tmp / 'retired.json'
        retired.write_text(json.dumps(owned_nodes(pid)))
        stamp = bound_stamp(tmp / 'retired.json.stamp', retired, pid, start,
                            epoch=stamp_epoch)
        owned = {'expected_pid': pid, 'expected_start_ticks': start,
                 'stamp_path': stamp, 'expected_gate': GATE,
                 'expected_nonce': NONCE, 'expected_seq': 1}
        return retired, gate_start, owned

    def docs(self, tmp, before_doc, after_doc):
        before, after = tmp / 'before.json', tmp / 'after.json'
        before.write_text(json.dumps(before_doc))
        after.write_text(json.dumps(after_doc))
        return before, after

    def good_doc(self, locked=False, brightness=1.0):
        return {'locked': locked,
                'wellbeing_effects': {'desaturation': 0.0,
                                      'brightness': brightness,
                                      'animating': brightness < 1.0}}

    def check_docs(self, tmp, before_doc, after_doc):
        before, after = self.docs(tmp, before_doc, after_doc)
        retired, gate_start, owned = self.owned(tmp)
        return mod.check_stale_press_fails(
            1, before, after, 'Take', retired, DUE, gate_start, None, **owned)

    def test_empty_docs_error(self):
        with tempfile.TemporaryDirectory() as td:
            with self.assertRaises(mod.HarnessError):
                self.check_docs(Path(td), {}, {})

    def test_missing_locked_errors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            good = self.good_doc()
            bad = dict(good)
            del bad['locked']
            with self.assertRaises(mod.HarnessError):
                self.check_docs(tmp, bad, good)
            with self.assertRaises(mod.HarnessError):
                self.check_docs(tmp, good, bad)

    def test_locked_must_be_actual_bool(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            for not_bool in (1, 0, 'true', 'false', None, 1.0):
                bad = self.good_doc()
                bad['locked'] = not_bool
                with self.assertRaises(mod.HarnessError,
                                       msg=f'locked={not_bool!r}'):
                    self.check_docs(tmp, bad, self.good_doc())
                with self.assertRaises(mod.HarnessError,
                                       msg=f'locked={not_bool!r}'):
                    self.check_docs(tmp, self.good_doc(), bad)

    def test_missing_or_mistyped_effects_error(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            good = self.good_doc()
            for bad_wb in (None, [], 'x', 1.0, True):
                bad = dict(good, wellbeing_effects=bad_wb)
                with self.assertRaises(mod.HarnessError,
                                       msg=f'wb={bad_wb!r}'):
                    self.check_docs(tmp, bad, good)
            bad = dict(good)
            del bad['wellbeing_effects']
            with self.assertRaises(mod.HarnessError):
                self.check_docs(tmp, bad, good)

    def test_brightness_must_be_numeric_finite(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            for bad_bright in ('0.6', None, True, False,
                               float('nan'), float('inf'), [1.0]):
                bad = self.good_doc()
                bad['wellbeing_effects']['brightness'] = bad_bright
                with self.assertRaises(mod.HarnessError,
                                       msg=f'brightness={bad_bright!r}'):
                    self.check_docs(tmp, bad, self.good_doc())
            bad = self.good_doc()
            del bad['wellbeing_effects']['brightness']
            with self.assertRaises(mod.HarnessError):
                self.check_docs(tmp, bad, self.good_doc())

    def test_desaturation_must_be_numeric_finite(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            bad = self.good_doc()
            bad['wellbeing_effects']['desaturation'] = 'none'
            with self.assertRaises(mod.HarnessError):
                self.check_docs(tmp, bad, self.good_doc())
            bad = self.good_doc()
            del bad['wellbeing_effects']['desaturation']
            with self.assertRaises(mod.HarnessError):
                self.check_docs(tmp, bad, self.good_doc())

    def test_animating_must_be_actual_bool(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            bad = self.good_doc()
            bad['wellbeing_effects']['animating'] = 1
            with self.assertRaises(mod.HarnessError):
                self.check_docs(tmp, bad, self.good_doc())

    def test_changed_state_fails_after_presence(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            with self.assertRaises(AssertionError):
                self.check_docs(tmp, self.good_doc(locked=False),
                                self.good_doc(locked=True))
            with self.assertRaises(AssertionError):
                self.check_docs(tmp, self.good_doc(brightness=1.0),
                                self.good_doc(brightness=0.6))

    def test_unchanged_present_state_passes(self):
        with tempfile.TemporaryDirectory() as td:
            self.assertTrue(self.check_docs(
                Path(td), self.good_doc(), self.good_doc()))


class StampedRetiredAnchor(unittest.TestCase):
    """Retired dumps need owner stamp + live PID + fresh stamp + anchor."""

    def setup(self, tmp):
        pid, start = owner_identity()
        gate_start = time.time() - 1
        return pid, start, gate_start

    def check_dump(self, tmp, nodes, pid=None, start=None, gate_start=None,
                   stamp=None, gate=GATE, nonce=NONCE, seq=1):
        real_pid, real_start = owner_identity()
        pid = real_pid if pid is None else pid
        start = real_start if start is None else start
        gate_start = time.time() - 1 if gate_start is None else gate_start
        retired = tmp / 'retired.json'
        retired.write_text(json.dumps(nodes))
        if stamp is None:
            stamp = bound_stamp(tmp / 'retired.json.stamp', retired,
                                pid, start)
        return mod.check_retired_absent(
            retired, DUE, gate_start, expected_pid=pid,
            expected_start_ticks=start, stamp_path=stamp,
            expected_gate=gate, expected_nonce=nonce, expected_seq=seq)

    def test_owned_stamped_anchored_dump_passes(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _, _ = self.setup(tmp)
            self.assertTrue(self.check_dump(tmp, owned_nodes(pid)))

    def test_bare_object_errors(self):
        with tempfile.TemporaryDirectory() as td:
            with self.assertRaises(mod.HarnessError):
                self.check_dump(Path(td), [{}])

    def test_missing_process_id_errors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _, _ = self.setup(tmp)
            nodes = owned_nodes(pid)
            del nodes[0]['process_id']
            with self.assertRaises(mod.HarnessError):
                self.check_dump(tmp, nodes)

    def test_wrong_process_id_errors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _, _ = self.setup(tmp)
            nodes = owned_nodes(pid + 1000000)
            with self.assertRaises(mod.HarnessError):
                self.check_dump(tmp, nodes)

    def test_bool_process_id_errors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _, _ = self.setup(tmp)
            nodes = owned_nodes(pid)
            nodes[0]['process_id'] = True
            with self.assertRaises(mod.HarnessError):
                self.check_dump(tmp, nodes)

    def test_dead_pid_dump_errors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            dead = 2**31 - 1
            with self.assertRaises(mod.HarnessError):
                self.check_dump(tmp, owned_nodes(dead), pid=dead, start=1)

    def test_replaced_pid_errors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, start, _ = self.setup(tmp)
            with self.assertRaises(mod.HarnessError):
                self.check_dump(tmp, owned_nodes(pid),
                                start=start + 999999999)

    def test_missing_stamp_errors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _, _ = self.setup(tmp)
            retired = tmp / 'retired.json'
            retired.write_text(json.dumps(owned_nodes(pid)))
            real_pid, real_start = owner_identity()
            with self.assertRaises(mod.HarnessError):
                mod.check_retired_absent(
                    retired, DUE, time.time() - 1,
                    expected_pid=real_pid,
                    expected_start_ticks=real_start,
                    stamp_path=tmp / 'gone.stamp',
                    expected_gate=GATE, expected_nonce=NONCE,
                    expected_seq=1)

    def test_replayed_stamp_errors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, start, _ = self.setup(tmp)
            nodes = owned_nodes(pid)
            other = write_stamp(tmp / 'other.stamp', pid, start,
                                nonce='nonce-other-gate',
                                dump_sha256=bytes_sha(nodes))
            with self.assertRaises(mod.HarnessError):
                self.check_dump(tmp, nodes, stamp=other)

    def test_wrong_gate_stamp_errors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, start, _ = self.setup(tmp)
            nodes = owned_nodes(pid)
            other = write_stamp(tmp / 'other.stamp', pid, start,
                                gate='G-OTHER',
                                dump_sha256=bytes_sha(nodes))
            with self.assertRaises(mod.HarnessError):
                self.check_dump(tmp, nodes, stamp=other)

    def test_stamp_for_other_process_errors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, start, _ = self.setup(tmp)
            nodes = owned_nodes(pid)
            other = write_stamp(tmp / 'other.stamp', pid + 1000000, start,
                                dump_sha256=bytes_sha(nodes))
            with self.assertRaises(mod.HarnessError):
                self.check_dump(tmp, nodes, stamp=other)

    def test_stamp_missing_key_errors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, start, _ = self.setup(tmp)
            stamp = tmp / 'short.stamp'
            stamp.write_text(json.dumps({'gate': GATE, 'nonce': NONCE}))
            with self.assertRaises(mod.HarnessError):
                self.check_dump(tmp, owned_nodes(pid), stamp=stamp)

    def test_stale_stamp_errors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, start, _ = self.setup(tmp)
            nodes = owned_nodes(pid)
            old = write_stamp(tmp / 'old.stamp', pid, start,
                              epoch=time.time() - 100,
                              dump_sha256=bytes_sha(nodes))
            with self.assertRaises(mod.HarnessError):
                self.check_dump(tmp, nodes, stamp=old,
                                gate_start=time.time() - 1)

    def test_missing_showing_anchor_errors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _, _ = self.setup(tmp)
            with self.assertRaises(mod.HarnessError):
                self.check_dump(tmp, owned_nodes(pid, anchor=False))

    def test_showing_title_still_fails(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _, _ = self.setup(tmp)
            with self.assertRaises(AssertionError):
                self.check_dump(tmp, owned_nodes(pid, banner_showing=True))


class StampedObservationBinding(unittest.TestCase):
    """The stamp binds the exact observation seq and the exact dump bytes."""

    def setup(self, tmp):
        pid, start = owner_identity()
        return pid, start, time.time() - 1

    def test_bound_seq_and_bytes_pass(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, start, gate_start = self.setup(tmp)
            retired = tmp / 'retired.json'
            retired.write_text(json.dumps(owned_nodes(pid)))
            stamp = bound_stamp(tmp / 'retired.json.stamp', retired,
                                pid, start, seq=7)
            self.assertTrue(mod.check_retired_absent(
                retired, DUE, gate_start, expected_pid=pid,
                expected_start_ticks=start, stamp_path=stamp,
                expected_gate=GATE, expected_nonce=NONCE, expected_seq=7))

    def test_wrong_seq_errors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, start, gate_start = self.setup(tmp)
            retired = tmp / 'retired.json'
            retired.write_text(json.dumps(owned_nodes(pid)))
            stamp = bound_stamp(tmp / 'retired.json.stamp', retired,
                                pid, start, seq=7)
            with self.assertRaisesRegex(mod.HarnessError, 'observation 1'):
                mod.check_retired_absent(
                    retired, DUE, gate_start, expected_pid=pid,
                    expected_start_ticks=start, stamp_path=stamp,
                    expected_gate=GATE, expected_nonce=NONCE,
                    expected_seq=1)

    def test_bool_seq_errors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, start, gate_start = self.setup(tmp)
            retired = tmp / 'retired.json'
            retired.write_text(json.dumps(owned_nodes(pid)))
            stamp = bound_stamp(tmp / 'retired.json.stamp', retired,
                                pid, start, seq=True)
            with self.assertRaises(mod.HarnessError):
                mod.check_retired_absent(
                    retired, DUE, gate_start, expected_pid=pid,
                    expected_start_ticks=start, stamp_path=stamp,
                    expected_gate=GATE, expected_nonce=NONCE,
                    expected_seq=1)

    def test_bad_expected_seq_errors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, start, gate_start = self.setup(tmp)
            retired = tmp / 'retired.json'
            retired.write_text(json.dumps(owned_nodes(pid)))
            stamp = bound_stamp(tmp / 'retired.json.stamp', retired,
                                pid, start)
            for bad in (0, -1, True, '1', None):
                with self.assertRaises(mod.HarnessError, msg=f'seq={bad!r}'):
                    mod.check_retired_absent(
                        retired, DUE, gate_start, expected_pid=pid,
                        expected_start_ticks=start, stamp_path=stamp,
                        expected_gate=GATE, expected_nonce=NONCE,
                        expected_seq=bad)

    def test_replayed_dump_bytes_error(self):
        # A valid owned tree paired with a stamp bound to DIFFERENT
        # bytes: the dump was swapped under a fresh stamp.
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, start, gate_start = self.setup(tmp)
            retired = tmp / 'retired.json'
            retired.write_text(json.dumps(owned_nodes(pid)))
            swapped = owned_nodes(pid) + [
                {'role': 'button', 'name': 'Extra', 'description': '',
                 'depth': 1, 'showing': False, 'checked': False,
                 'selected': False, 'focused': False,
                 'bounds': [10, 130, 220, 48]}]
            stamp = write_stamp(tmp / 'retired.json.stamp', pid, start,
                                dump_sha256=bytes_sha(swapped))
            with self.assertRaisesRegex(mod.HarnessError, 'actual current'):
                mod.check_retired_absent(
                    retired, DUE, gate_start, expected_pid=pid,
                    expected_start_ticks=start, stamp_path=stamp,
                    expected_gate=GATE, expected_nonce=NONCE,
                    expected_seq=1)

    def test_tampered_dump_after_stamp_errors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, start, gate_start = self.setup(tmp)
            retired = tmp / 'retired.json'
            retired.write_text(json.dumps(owned_nodes(pid)))
            stamp = bound_stamp(tmp / 'retired.json.stamp', retired,
                                pid, start)
            retired.write_text(json.dumps(
                owned_nodes(pid) + [
                    {'role': 'button', 'name': 'Extra', 'description': '',
                     'depth': 1, 'showing': False, 'checked': False,
                     'selected': False, 'focused': False,
                     'bounds': [10, 130, 220, 48]}]))
            with self.assertRaisesRegex(mod.HarnessError, 'actual current'):
                mod.check_retired_absent(
                    retired, DUE, gate_start, expected_pid=pid,
                    expected_start_ticks=start, stamp_path=stamp,
                    expected_gate=GATE, expected_nonce=NONCE,
                    expected_seq=1)

    def test_dump_read_once_parse_and_hash_same_buffer(self):
        # The dump file is read exactly ONCE: parse and hash see the
        # same bytes, so a file replaced between two reads cannot pair
        # a valid parsed tree with a stamped hash of different bytes.
        # Any second read of the dump serves hostile replacement bytes.
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, start, gate_start = self.setup(tmp)
            retired = tmp / 'retired.json'
            retired.write_text(json.dumps(owned_nodes(pid)))
            stamp = bound_stamp(tmp / 'retired.json.stamp', retired,
                                pid, start)
            dump_reads = []
            real_read_text = Path.read_text
            real_read_bytes = Path.read_bytes

            def counting_text(path, *args, **kwargs):
                if path == retired:
                    dump_reads.append('text')
                    if len(dump_reads) > 1:
                        return '["replaced-between-reads"]'
                return real_read_text(path, *args, **kwargs)

            def counting_bytes(path, *args, **kwargs):
                if path == retired:
                    dump_reads.append('bytes')
                    if len(dump_reads) > 1:
                        return b'["replaced-between-reads"]'
                return real_read_bytes(path, *args, **kwargs)

            with mock.patch.object(Path, 'read_text', new=counting_text), \
                    mock.patch.object(Path, 'read_bytes', new=counting_bytes):
                self.assertTrue(mod.check_retired_absent(
                    retired, DUE, gate_start, expected_pid=pid,
                    expected_start_ticks=start, stamp_path=stamp,
                    expected_gate=GATE, expected_nonce=NONCE,
                    expected_seq=1))
            self.assertEqual(dump_reads, ['bytes'])

    def test_missing_seq_or_sha_key_errors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, start, gate_start = self.setup(tmp)
            retired = tmp / 'retired.json'
            retired.write_text(json.dumps(owned_nodes(pid)))
            stamp = bound_stamp(tmp / 'retired.json.stamp', retired,
                                pid, start)
            doc = json.loads(stamp.read_text())
            for key in ('seq', 'dump_sha256'):
                short = tmp / f'short-{key}.stamp'
                short.write_text(json.dumps(
                    {k: v for k, v in doc.items() if k != key}))
                with self.assertRaises(mod.HarnessError, msg=key):
                    mod.check_retired_absent(
                        retired, DUE, gate_start, expected_pid=pid,
                        expected_start_ticks=start, stamp_path=short,
                        expected_gate=GATE, expected_nonce=NONCE,
                        expected_seq=1)


class StrictProducerTree(unittest.TestCase):
    """Absence needs a strictly typed tree, a non-app anchor, stable life."""

    def check_nodes(self, tmp, nodes, patch_start=None):
        pid, start = owner_identity()
        gate_start = time.time() - 1
        retired = tmp / 'retired.json'
        retired.write_text(json.dumps(nodes))
        stamp = bound_stamp(tmp / 'retired.json.stamp', retired,
                            pid, start)

        def call():
            return mod.check_retired_absent(
                retired, DUE, gate_start, expected_pid=pid,
                expected_start_ticks=start, stamp_path=stamp,
                expected_gate=GATE, expected_nonce=NONCE, expected_seq=1)

        if patch_start is None:
            return call()
        with mock.patch.object(mod, 'proc_start_ticks',
                               side_effect=patch_start):
            return call()

    def test_showing_string_never_anchors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _ = owner_identity()
            for bad in ('true', 'false', 1, 0):
                nodes = owned_nodes(pid, anchor=False)
                nodes[0]['showing'] = False
                nodes[2]['showing'] = bad
                with self.assertRaises(mod.HarnessError,
                                       msg=f'showing={bad!r}'):
                    self.check_nodes(tmp, nodes)

    def test_mistyped_nodes_rejected(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _ = owner_identity()
            cases = [
                ('missing bounds', lambda n: n[2].pop('bounds')),
                ('short bounds', lambda n: n[2].update(bounds=[1, 2])),
                ('bool bounds',
                 lambda n: n[2].update(bounds=[1, 2, 3, True])),
                ('string bounds', lambda n: n[2].update(bounds='x')),
                ('string depth', lambda n: n[2].update(depth='1')),
                ('bool depth', lambda n: n[2].update(depth=True)),
                ('int checked', lambda n: n[2].update(checked=1)),
                ('null name', lambda n: n[2].update(name=None)),
                ('missing description', lambda n: n[1].pop('description')),
                ('int role', lambda n: n[1].update(role=5)),
                ('string focused', lambda n: n[1].update(focused='yes')),
                ('int selected', lambda n: n[1].update(selected=0)),
                ('non-dict node', lambda n: n.__setitem__(2, ['bogus'])),
            ]
            for label, apply in cases:
                nodes = owned_nodes(pid)
                apply(nodes)
                with self.assertRaises(mod.HarnessError, msg=label):
                    self.check_nodes(tmp, nodes)

    def test_root_only_tree_cannot_establish_absence(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _ = owner_identity()
            with self.assertRaisesRegex(mod.HarnessError, 'root-only'):
                self.check_nodes(tmp, owned_nodes(pid)[:1])

    def test_process_replacement_during_validation_errors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, start = owner_identity()
            with self.assertRaisesRegex(mod.HarnessError,
                                        'during validation'):
                self.check_nodes(tmp, owned_nodes(pid),
                                 patch_start=[start, start + 999999])

    def test_zero_area_anchor_never_anchors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _ = owner_identity()
            for bounds in ([10, 70, 0, 48], [10, 70, 220, 0],
                           [10, 70, -5, 48], [10, 70, 220, -5]):
                nodes = owned_nodes(pid)
                nodes[2]['bounds'] = bounds
                with self.assertRaisesRegex(mod.HarnessError, 'allocated',
                                            msg=f'bounds={bounds!r}'):
                    self.check_nodes(tmp, nodes)

    def test_zero_area_hidden_node_keeps_valid_anchor(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _ = owner_identity()
            nodes = owned_nodes(pid)
            nodes[1]['bounds'] = [10, 10, 0, 48]
            self.assertTrue(self.check_nodes(tmp, nodes))

    def test_negative_origin_anchor_bounds_legal(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _ = owner_identity()
            nodes = owned_nodes(pid)
            nodes[2]['bounds'] = [-5, -5, 220, 48]
            self.assertTrue(self.check_nodes(tmp, nodes))

    def test_empty_role_rejected(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _ = owner_identity()
            nodes = owned_nodes(pid)
            nodes[2]['role'] = ''
            with self.assertRaises(mod.HarnessError):
                self.check_nodes(tmp, nodes)

    def test_bad_depth_rejected(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _ = owner_identity()
            nodes = owned_nodes(pid)
            nodes[2]['depth'] = -1
            with self.assertRaises(mod.HarnessError):
                self.check_nodes(tmp, nodes)
            nodes = owned_nodes(pid)
            nodes[0]['depth'] = 1
            with self.assertRaises(mod.HarnessError):
                self.check_nodes(tmp, nodes)
            nodes = owned_nodes(pid)
            nodes[2]['depth'] = 5
            with self.assertRaisesRegex(mod.HarnessError, 'depth jump'):
                self.check_nodes(tmp, nodes)

    def test_valid_deeper_nesting_passes(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _ = owner_identity()
            nodes = owned_nodes(pid)
            nodes[2]['depth'] = 2
            self.assertTrue(self.check_nodes(tmp, nodes))

    def test_non_application_root_rejected(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _ = owner_identity()
            nodes = owned_nodes(pid)
            nodes[0]['role'] = 'window'
            with self.assertRaisesRegex(mod.HarnessError,
                                        'must be the application node'):
                self.check_nodes(tmp, nodes)


UPCOMING = 'Eyesight Break Soon'


def banner_nodes(pid, title, buttons, title_showing=True):
    """Producer-shape banner: app root, title label, push buttons.

    Each button carries its inner text label (as real dumps do), so a
    label named like a button must never satisfy an expectation: only
    the push-button control qualifies, exactly as in press().
    """
    nodes = [{'role': 'application', 'name': 'tuna-shell-gtk',
              'description': '', 'depth': 0, 'showing': True,
              'checked': False, 'selected': False, 'focused': False,
              'bounds': None, 'process_id': pid},
             {'role': 'label', 'name': title, 'description': '',
              'depth': 1, 'showing': title_showing,
              'checked': False, 'selected': False, 'focused': False,
              'bounds': [10, 10, 220, 48]}]
    for name, showing in buttons:
        nodes.append({'role': 'push button', 'name': name,
                      'description': '', 'depth': 1, 'showing': showing,
                      'checked': False, 'selected': False, 'focused': False,
                      'bounds': [10, 70, 100, 40]})
        nodes.append({'role': 'label', 'name': name, 'description': '',
                      'depth': 2, 'showing': showing,
                      'checked': False, 'selected': False, 'focused': False,
                      'bounds': [12, 72, 96, 36]})
    return nodes


class NoticeButtons(unittest.TestCase):
    def check_file(self, tmp, nodes):
        path = tmp / 'banner.json'
        path.write_text(json.dumps(nodes))
        return path

    def test_upcoming_offers_delay_and_skip_without_take(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _ = owner_identity()
            path = self.check_file(
                tmp, banner_nodes(pid, UPCOMING,
                                  [('Delay', True), ('Skip', True)]))
            self.assertTrue(mod.notice_buttons(
                path, UPCOMING, ('Delay', 'Skip'), ('Take',)))

    def test_due_offers_take_and_delay_without_skip(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _ = owner_identity()
            path = self.check_file(
                tmp, banner_nodes(pid, DUE,
                                  [('Take', True), ('Delay', True)]))
            self.assertTrue(mod.notice_buttons(
                path, DUE, ('Take', 'Delay'), ('Skip',)))

    def test_missing_expected_button_fails(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _ = owner_identity()
            path = self.check_file(
                tmp, banner_nodes(pid, UPCOMING, [('Delay', True)]))
            with self.assertRaises(AssertionError):
                mod.notice_buttons(path, UPCOMING, ('Delay', 'Skip'), ())

    def test_rejected_button_present_fails(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _ = owner_identity()
            path = self.check_file(
                tmp, banner_nodes(pid, DUE,
                                  [('Take', True), ('Delay', True),
                                   ('Skip', True)]))
            with self.assertRaises(AssertionError):
                mod.notice_buttons(path, DUE, ('Take', 'Delay'), ('Skip',))

    def test_inner_label_never_satisfies_expectation(self):
        # A label named Skip (only) must not count as an offered Skip.
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _ = owner_identity()
            nodes = banner_nodes(pid, UPCOMING, [('Delay', True)])
            nodes.append({'role': 'label', 'name': 'Skip', 'description': '',
                          'depth': 1, 'showing': True,
                          'checked': False, 'selected': False,
                          'focused': False, 'bounds': [10, 110, 100, 40]})
            path = self.check_file(tmp, nodes)
            with self.assertRaises(AssertionError):
                mod.notice_buttons(path, UPCOMING, ('Skip',), ())

    def test_hidden_button_never_satisfies_expectation(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _ = owner_identity()
            path = self.check_file(
                tmp, banner_nodes(pid, UPCOMING,
                                  [('Delay', True), ('Skip', False)]))
            with self.assertRaises(AssertionError):
                mod.notice_buttons(path, UPCOMING, ('Skip',), ())

    def test_matching_is_exact_not_substring_or_folded(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _ = owner_identity()
            path = self.check_file(
                tmp, banner_nodes(pid, UPCOMING,
                                  [('Delay', True), ('Skip', True)]))
            with self.assertRaises(AssertionError):
                mod.notice_buttons(path, UPCOMING, ('skip',), ())
            with self.assertRaises(AssertionError):
                mod.notice_buttons(path, UPCOMING, ('Ski',), ())

    def test_missing_banner_fails(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, _ = owner_identity()
            path = self.check_file(
                tmp, banner_nodes(pid, UPCOMING, [('Delay', True)],
                                  title_showing=False))
            with self.assertRaises(AssertionError):
                mod.notice_buttons(path, UPCOMING, ('Delay',), ())

    def test_malformed_tree_is_harness_error(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            path = tmp / 'banner.json'
            path.write_text('{"not": "a list"}')
            with self.assertRaises(mod.HarnessError):
                mod.notice_buttons(path, UPCOMING, ('Skip',), ())


class SkipUndisturbed(unittest.TestCase):
    def test_zero_drift_passes_and_returns_delta(self):
        self.assertAlmostEqual(mod.check_skip_undisturbed(1000.0, 1000.0), 0.0)
        self.assertAlmostEqual(mod.check_skip_undisturbed(1000.0, 1003.0), 3.0)
        self.assertAlmostEqual(mod.check_skip_undisturbed(1003.0, 1000.0), -3.0)

    def test_tolerance_boundary(self):
        self.assertAlmostEqual(mod.check_skip_undisturbed(1000.0, 1008.0), 8.0)
        self.assertAlmostEqual(mod.check_skip_undisturbed(1000.0, 992.0), -8.0)
        with self.assertRaises(AssertionError):
            mod.check_skip_undisturbed(1000.0, 1008.1)
        with self.assertRaises(AssertionError):
            mod.check_skip_undisturbed(1000.0, 991.9)

    def test_restart_sized_drift_fails(self):
        # An interval restart (the OLD Skip expectation) is a schedule
        # move, not an undisturbed schedule: it must fail here.
        with self.assertRaises(AssertionError):
            mod.check_skip_undisturbed(1000.0, 1065.0)

    def test_stale_press_due_kind_routes_to_undisturbed(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            pid, start = owner_identity()
            before, after = tmp / 'before.json', tmp / 'after.json'
            cstate(before)
            cstate(after)
            retired = tmp / 'retired.json'
            retired.write_text(json.dumps(owned_nodes(pid)))
            stamp = bound_stamp(tmp / 'retired.json.stamp', retired,
                                pid, start)
            owned = {'expected_pid': pid, 'expected_start_ticks': start,
                     'stamp_path': stamp, 'expected_gate': GATE,
                     'expected_nonce': NONCE, 'expected_seq': 1}
            self.assertTrue(mod.check_stale_press_fails(
                1, before, after, 'Skip', retired, DUE, time.time() - 1,
                ('skip-undisturbed', 1000.0, 1002.0, 0), **owned))
            with self.assertRaises(AssertionError):
                mod.check_stale_press_fails(
                    1, before, after, 'Skip', retired, DUE,
                    time.time() - 1,
                    ('skip-undisturbed', 1000.0, 1065.0, 0), **owned)
            with self.assertRaises(AssertionError):
                mod.check_stale_press_fails(
                    1, before, after, 'Skip', retired, DUE,
                    time.time() - 1,
                    ('skip-undisturbed', 1000.0, 1002.0, 30), **owned)


class SkipScheduleEvidence(unittest.TestCase):
    """Synthetic source observations; never claimed as live CI evidence."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.path = Path(self.tmp.name) / 'nested.log'
        self.pid, self.start = owner_identity()

    def skip(self, *, now=911, before=1000, after=1000, pid=None, idle=0):
        return (f'tuna-shell-gtk: wellbeing: skip schedule pid={self.pid if pid is None else pid} '
                f'now={now} idle={idle} before={before} after={after}\n')

    def due(self, *, now=1001, due=1000, pid=None, idle=0):
        return (f'tuna-shell-gtk: wellbeing: due schedule pid={self.pid if pid is None else pid} '
                f'now={now} idle={idle} due={due}\n')

    def check(self, text=None, *, offset=0, orig=1000, arrival=1003, gate=900):
        if text is not None:
            self.path.write_text(text)
        return mod.check_skip_schedule(
            self.path, offset, orig, arrival, gate,
            expected_pid=self.pid, expected_start_ticks=self.start)

    def test_deadline_and_within_budget_presentation_are_recorded_separately(self):
        receipt = self.check(self.skip() + self.due())
        self.assertEqual(receipt['scheduled_due'], 1000)
        self.assertEqual(receipt['schedule_drift_secs'], 0)
        self.assertEqual(receipt['arrival_lag_secs'], 3)
        self.assertEqual(receipt['skip']['before'], receipt['skip']['after'])
        self.assertEqual(receipt['skip']['after'], receipt['due_notice']['due'])

    def test_eleven_second_arrival_still_fails_with_unchanged_actual_deadline(self):
        with self.assertRaisesRegex(AssertionError, 'got 11.0s'):
            self.check(self.skip() + self.due(), arrival=1011)

    def test_eleven_second_actual_schedule_drift_still_fails(self):
        with self.assertRaisesRegex(AssertionError, 'got 11.0s'):
            self.check(self.skip(before=1011, after=1011)
                       + self.due(now=1011, due=1011), arrival=1011)

    def test_even_one_second_consumer_or_later_deadline_change_fails(self):
        for text in (self.skip(after=1001) + self.due(due=1001),
                     self.skip() + self.due(due=1001)):
            with self.subTest(text=text):
                with self.assertRaisesRegex(AssertionError, 'deadline'):
                    self.check(text)

    def test_arrival_without_actual_source_evidence_never_qualifies(self):
        for text in ('', self.skip(), self.due()):
            with self.subTest(text=text):
                with self.assertRaisesRegex(mod.HarnessError, 'exactly one'):
                    self.check(text)

    def test_duplicate_actions_including_stale_invocations_fail(self):
        with self.assertRaisesRegex(mod.HarnessError, 'exactly one'):
            self.check(self.skip() + self.skip(now=912) + self.due())

    def test_ambiguous_due_notices_fail(self):
        with self.assertRaisesRegex(mod.HarnessError, 'exactly one'):
            self.check(self.skip() + self.due() + self.due(now=1002))

    def test_zero_missing_nonfinite_and_malformed_deadlines_fail_closed(self):
        for value in (0, None, 'NaN', 'inf', -1):
            for text in (self.skip(before=value, after=value) + self.due(),
                         self.skip() + self.due(due=value)):
                with self.subTest(value=value, text=text):
                    with self.assertRaises(mod.HarnessError):
                        self.check(text)
        with self.assertRaises(mod.HarnessError):
            self.check(self.skip().replace(' after=1000', '') + self.due())

    def test_invalid_source_clock_or_idle_fails_closed(self):
        for text in (self.skip(now='NaN') + self.due(),
                     self.skip(idle=-1) + self.due(),
                     self.skip() + self.due(now=0)):
            with self.subTest(text=text):
                with self.assertRaises(mod.HarnessError):
                    self.check(text)

    def test_skip_must_act_on_an_actual_future_deadline(self):
        with self.assertRaisesRegex(AssertionError, 'upcoming'):
            self.check(self.skip(now=1000) + self.due())

    def test_records_must_follow_action_gate_and_due_time(self):
        for text in (self.due() + self.skip(),
                     self.skip(now=899) + self.due(),
                     self.skip() + self.due(now=999),
                     self.skip() + self.due(now=1012)):
            with self.subTest(text=text):
                with self.assertRaises(AssertionError):
                    self.check(text)

    def test_foreign_or_replaced_shell_cannot_supply_deadlines(self):
        for text in (self.skip(pid=self.pid + 1) + self.due(),
                     self.skip() + self.due(pid=self.pid + 1)):
            with self.subTest(text=text):
                with self.assertRaisesRegex(mod.HarnessError, 'owner'):
                    self.check(text)
        for starts in ([self.start + 1], [self.start, self.start + 1]):
            self.path.write_text(self.skip() + self.due())
            with mock.patch.object(mod, 'proc_start_ticks', side_effect=starts):
                with self.assertRaisesRegex(mod.HarnessError, 'replaced'):
                    self.check()

    def test_fresh_log_offset_excludes_old_actions(self):
        old = self.skip(now=811, before=850, after=850) + self.due(now=851, due=850)
        receipt = self.check(old + self.skip() + self.due(),
                             offset=len(old.encode()))
        self.assertEqual(receipt['scheduled_due'], 1000)
        with self.assertRaisesRegex(mod.HarnessError, 'exactly one'):
            self.check(old, offset=len(old.encode()))

    def test_truncated_or_mid_line_log_mark_fails_closed(self):
        text = self.skip() + self.due()
        for offset in (-1, True, 1, len(text.encode()) + 1):
            with self.subTest(offset=offset):
                with self.assertRaises(mod.HarnessError):
                    self.check(text, offset=offset)

    def test_missing_log_and_untyped_epochs_are_harness_errors(self):
        with self.assertRaises(mod.HarnessError):
            self.check()
        for value in (None, True, float('nan'), float('inf'), 0, -1):
            for key in ('orig', 'arrival', 'gate'):
                with self.subTest(key=key, value=value):
                    with self.assertRaises(mod.HarnessError):
                        self.check(self.skip() + self.due(), **{key: value})

    def test_cli_emits_actual_schedule_receipt(self):
        self.path.write_text(self.skip() + self.due())
        with mock.patch('builtins.print') as printed:
            self.assertEqual(mod.main([
                'skip-schedule', '--log', str(self.path), '--log-offset', '0',
                '--orig-due', '1000', '--due-arrival', '1003', '--gate-start', '900',
                '--expect-pid', str(self.pid), '--expect-start', str(self.start),
            ]), 0)
        self.assertEqual(json.loads(printed.call_args.args[0])['scheduled_due'], 1000)

    def test_shell_uses_authoritative_due_and_keeps_existing_wait_budget(self):
        proof = (HERE.parent / 'tuna-gtk-shell-proof').read_text()
        gate = proof.split('wb_gate_begin G-WELLBEING-ACTION-SKIP\n', 1)[1]
        gate = gate.split('\nwb_break_cleanup', 1)[0]
        self.assertLess(gate.index('skip_schedule_log_offset='),
                        gate.index('gsettings set org.gnome.desktop.break-reminders selected-breaks'))
        self.assertIn('skip-schedule \\\n', gate)
        self.assertIn('--expect-pid "$WB_SHELL_PID" --expect-start "$WB_SHELL_START"', gate)
        self.assertIn('--due-arrival "$skip_due_epoch"', gate)
        self.assertIn('--due-new "$skip_due_epoch"', gate)
        self.assertIn('--log "$LOG" --log-offset "$skip_schedule_log_offset"', gate)
        self.assertIn('>"$ARTIFACTS/wb-skip-schedule.json"', gate)
        self.assertIn('end=$(($(date +%s) + 150))', gate)


if __name__ == '__main__':
    unittest.main()
