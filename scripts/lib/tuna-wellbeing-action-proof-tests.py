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
    nodes = [{'role': 'application', 'name': 'tuna-shell-gtk',
              'showing': anchor, 'process_id': pid},
             {'role': 'button', 'name': DUE, 'showing': banner_showing},
             {'role': 'button', 'name': 'Take', 'showing': False}]
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
                {'role': 'button', 'name': 'Extra', 'showing': False}]
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
                    {'role': 'button', 'name': 'Extra', 'showing': False}]))
            with self.assertRaisesRegex(mod.HarnessError, 'actual current'):
                mod.check_retired_absent(
                    retired, DUE, gate_start, expected_pid=pid,
                    expected_start_ticks=start, stamp_path=stamp,
                    expected_gate=GATE, expected_nonce=NONCE,
                    expected_seq=1)

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


if __name__ == '__main__':
    unittest.main()
