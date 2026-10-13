#!/usr/bin/env python3
"""Unit tests for tuna-wellbeing-probe-bind.py (synthetic scenes only).

Runtime gates bind the live owned probe in the nested harness; these
tests pin the fail-closed rules: unique new-map identity, native-only,
decoy rejection, process-lifetime (PID reuse) detection and stable
geometry. No cargo, no compositor required.
"""
import importlib.util
import os
import sys
import unittest
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


if __name__ == '__main__':
    unittest.main()
