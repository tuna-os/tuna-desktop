#!/usr/bin/env python3
"""Unit tests for tuna-wellbeing-probe-bind.py (synthetic scenes only).

Runtime gates bind the live owned probe in the nested harness; these
tests pin the fail-closed rules: unique new-map identity, native-only,
decoy rejection, process-lifetime (PID reuse) detection and stable
geometry. No cargo, no compositor required.
"""
import importlib.util
import io
import json
import os
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

sys.dont_write_bytecode = True

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location(
    'wb_bind', HERE / 'tuna-wellbeing-probe-bind.py')
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

APP = 'tuna-cursor-probe'


def window(wid, app_id=APP, x11=None, rect=None, app_window=None):
    return {'id': wid, 'app_id': app_id, 'x11_window_id': x11,
            'application_window_id': app_window if app_window is not None else wid * 10,
            'workspace': 1, 'rect': rect or [100, 100, 320, 200]}


def scene(*windows):
    return {'windows': list(windows)}


def identity(pid=None, start=12345, uid=1000, exe='/bin/probe'):
    return {'pid': pid if pid is not None else os.getpid(), 'uid': uid,
            'start_ticks': start, 'exe': exe}


class ProbeBind(unittest.TestCase):
    def test_bind_unique_new_native_map(self):
        before = scene(window(1, 'tuna-shell-gtk'))
        after = scene(window(1, 'tuna-shell-gtk'), window(7))
        owner = mod.bind(before, after, identity(), APP)
        self.assertEqual(owner['id'], 7)
        self.assertEqual(owner['application_window_id'], 70)
        self.assertEqual(owner['rect'], [100, 100, 320, 200])

    def test_bind_rejects_decoy_same_app_id(self):
        # Pre-existing same-app window plus our new map: ambiguous.
        before = scene(window(1, 'tuna-shell-gtk'), window(5))
        after = scene(window(1, 'tuna-shell-gtk'), window(5), window(7))
        with self.assertRaises(RuntimeError):
            mod.bind(before, after, identity(), APP)

    def test_bind_rejects_two_new_windows(self):
        before = scene(window(1, 'tuna-shell-gtk'))
        after = scene(window(1, 'tuna-shell-gtk'), window(7), window(8, 'other'))
        with self.assertRaises(RuntimeError):
            mod.bind(before, after, identity(), APP)

    def test_bind_rejects_missing_map(self):
        before = scene(window(1, 'tuna-shell-gtk'))
        with self.assertRaises(RuntimeError):
            mod.bind(before, scene(window(1, 'tuna-shell-gtk')), identity(), APP)

    def test_bind_rejects_wrong_app_id(self):
        before = scene(window(1, 'tuna-shell-gtk'))
        after = scene(window(1, 'tuna-shell-gtk'), window(7, 'impostor'))
        with self.assertRaises(RuntimeError):
            mod.bind(before, after, identity(), APP)

    def test_bind_rejects_x11_window(self):
        before = scene(window(1, 'tuna-shell-gtk'))
        after = scene(window(1, 'tuna-shell-gtk'), window(7, x11=4242))
        with self.assertRaises(RuntimeError):
            mod.bind(before, after, identity(), APP)


class ProbeCheck(unittest.TestCase):
    def bound(self, **over):
        owner = {**identity(), 'id': 7, 'application_window_id': 70,
                 'app_id': APP, 'rect': [100, 100, 320, 200]}
        owner.update(over)
        return owner

    def test_check_accepts_stable_owner(self):
        live = scene(window(1, 'tuna-shell-gtk'), window(7))
        self.assertEqual(mod.check(live, self.bound(), identity()), [100, 100, 320, 200])

    def test_check_rejects_pid_reuse(self):
        # Same pid, new start ticks: a recycled process is not our client.
        live = scene(window(1, 'tuna-shell-gtk'), window(7))
        with self.assertRaises(RuntimeError):
            mod.check(live, self.bound(), identity(start=99999))

    def test_check_rejects_exe_swap(self):
        live = scene(window(1, 'tuna-shell-gtk'), window(7))
        with self.assertRaises(RuntimeError):
            mod.check(live, self.bound(), identity(exe='/tmp/evil'))

    def test_check_rejects_geometry_change(self):
        live = scene(window(1, 'tuna-shell-gtk'),
                     window(7, rect=[110, 100, 320, 200]))
        with self.assertRaises(RuntimeError):
            mod.check(live, self.bound(), identity())

    def test_check_rejects_identity_change(self):
        live = scene(window(1, 'tuna-shell-gtk'),
                     window(9, app_window=90))
        with self.assertRaises(RuntimeError):
            mod.check(live, self.bound(), identity())

    def test_check_rejects_decoy_at_check_time(self):
        live = scene(window(1, 'tuna-shell-gtk'), window(7),
                     window(9, app_window=91))
        with self.assertRaises(RuntimeError):
            mod.check(live, self.bound(), identity())

    def test_check_rejects_closed_probe(self):
        with self.assertRaises(RuntimeError):
            mod.check(scene(window(1, 'tuna-shell-gtk')), self.bound(), identity())

    def test_process_identity_reads_live_proc(self):
        me = mod.process_identity(os.getpid())
        self.assertEqual(me['pid'], os.getpid())
        self.assertEqual(me['uid'], os.getuid())
        self.assertGreater(me['start_ticks'], 0)
        self.assertTrue(me['exe'])

    def test_process_identity_rejects_dead_pid(self):
        with self.assertRaises(RuntimeError):
            mod.process_identity(2**31 - 1)


class FreshEnter(unittest.TestCase):
    def write(self, tmp, *lines):
        log = tmp / 'probe.log'
        log.write_bytes((''.join(lines)).encode())
        return log

    def test_fresh_enter_after_offset_passes(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            old = '{"enter":11,"surface":32}\n'
            log = self.write(tmp, old, '{"enter":12,"surface":32}\n')
            entered = mod.fresh_enter(log, len(old.encode()))
            self.assertEqual(entered['enter'], 12)

    def test_preseeded_old_enter_with_no_fresh_one_fails(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            old = '{"enter":11,"surface":32}\n'
            log = self.write(tmp, old)
            with self.assertRaises(RuntimeError):
                mod.fresh_enter(log, len(old.encode()))

    def test_offset_zero_sees_the_first_enter(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            log = self.write(tmp, '{"enter":11,"surface":32}\n')
            self.assertEqual(mod.fresh_enter(log, 0)['enter'], 11)

    def test_offset_past_end_fails(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            log = self.write(tmp, '{"enter":11,"surface":32}\n')
            with self.assertRaises(RuntimeError):
                mod.fresh_enter(log, 10**6)

    def test_missing_log_fails(self):
        with tempfile.TemporaryDirectory() as td:
            with self.assertRaises(RuntimeError):
                mod.fresh_enter(Path(td) / 'gone.log', 0)

    def test_negative_or_bool_offset_fails(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            log = self.write(tmp, '{"enter":11,"surface":32}\n')
            with self.assertRaises(RuntimeError):
                mod.fresh_enter(log, -1)
            with self.assertRaises(RuntimeError):
                mod.fresh_enter(log, True)

    def test_torn_line_skipped_fresh_still_counts(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            log = self.write(tmp, '{"enter":11,"surfac\n',
                             '{"enter":12,"surface":32}\n')
            self.assertEqual(mod.fresh_enter(log, 0)['enter'], 12)

    def test_non_integer_enter_never_counts(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            log = self.write(tmp, '{"enter":"soon"}\n')
            with self.assertRaises(RuntimeError):
                mod.fresh_enter(log, 0)

    def test_other_json_lines_do_not_count(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            log = self.write(tmp, '{"cursor_callback":3,"time":9}\n')
            with self.assertRaises(RuntimeError):
                mod.fresh_enter(log, 0)

    def test_fresh_enter_cli_reports_the_new_enter(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            old = '{"enter":11,"surface":32}\n'
            log = self.write(tmp, old, '{"enter":12,"surface":32}\n')
            buf = io.StringIO()
            with redirect_stdout(buf):
                mod.main(['fresh-enter', '--log', str(log),
                          '--offset', str(len(old.encode()))])
            self.assertEqual(json.loads(buf.getvalue())['enter'], 12)


class HandshakeBeforeAfter(unittest.TestCase):
    """The handshake pins geometry + lifetime before AND after the enter."""

    def bound(self, **over):
        owner = {**identity(), 'id': 7, 'application_window_id': 70,
                 'app_id': APP, 'rect': [100, 100, 320, 200]}
        owner.update(over)
        return owner

    def live(self, rect=None):
        return scene(window(1, 'tuna-shell-gtk'),
                     window(7, rect=rect or [100, 100, 320, 200]))

    def test_geometry_held_across_handshake(self):
        expected = self.bound()
        mod.check(self.live(), expected, identity())
        mod.check(self.live(), expected, identity())

    def test_geometry_moved_after_enter_fails(self):
        expected = self.bound()
        mod.check(self.live(), expected, identity())
        with self.assertRaises(RuntimeError):
            mod.check(self.live(rect=[101, 100, 320, 200]), expected,
                      identity())

    def test_lifetime_changed_after_enter_fails(self):
        expected = self.bound()
        mod.check(self.live(), expected, identity())
        with self.assertRaises(RuntimeError):
            mod.check(self.live(), expected, identity(start=77777))


if __name__ == '__main__':
    unittest.main()
