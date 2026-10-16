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

    def test_unterminated_valid_json_is_not_a_completed_record(self):
        with tempfile.TemporaryDirectory() as td:
            log = self.write(Path(td), '{"enter":12,"surface":32}')
            with self.assertRaises(RuntimeError):
                mod.fresh_enter(log, 0)
            with log.open('ab') as stream:
                stream.write(b'\n')
            self.assertEqual(mod.fresh_enter(log, 0)['enter'], 12)

    def test_valid_json_suffix_of_old_record_is_not_fresh(self):
        with tempfile.TemporaryDirectory() as td:
            # The offset was taken while the old prefix was written.
            prefix = 'old-record-prefix '
            log = self.write(Path(td), prefix, '{"enter":11}\n')
            with self.assertRaises(RuntimeError):
                mod.fresh_enter(log, len(prefix.encode()))
            with log.open('ab') as stream:
                stream.write(b'{"enter":12}\n')
            self.assertEqual(mod.fresh_enter(log, len(prefix.encode()))['enter'], 12)

    def test_incomplete_tail_does_not_hide_completed_enter(self):
        with tempfile.TemporaryDirectory() as td:
            log = self.write(Path(td), '{"enter":12}\n', '{"enter":13}')
            self.assertEqual(mod.fresh_enter(log, 0)['enter'], 12)

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


class PointerZone(unittest.TestCase):
    """Outside/inside verdicts need a well-formed finite [x, y] pair."""

    RECT = [100, 100, 320, 200]

    def state(self, tmp, body):
        path = tmp / 'cstate.json'
        path.write_text(body)
        return path

    def test_inside_outside_and_edges(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            cases = [([20, 580], 'outside'), ([99, 150], 'outside'),
                     ([100, 100], 'inside'), ([150.5, 150], 'inside'),
                     ([419, 299], 'inside'), ([420, 150], 'outside'),
                     ([150, 300], 'outside'), ([150, 99], 'outside')]
            for pos, zone in cases:
                path = self.state(tmp, json.dumps({'pointer_position': pos}))
                self.assertEqual(mod.pointer_zone(path, list(self.RECT)),
                                 zone, msg=f'pos={pos!r}')

    def test_malformed_shape_rejected(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            for bad in ([], [1], [1, 2, 3], 'x', 5, {'x': 1}):
                path = self.state(
                    tmp, json.dumps({'pointer_position': bad}))
                with self.assertRaises(RuntimeError, msg=f'pos={bad!r}'):
                    mod.pointer_zone(path, list(self.RECT))

    def test_missing_or_null_rejected(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            path = self.state(tmp, json.dumps({'pointer_position': None}))
            with self.assertRaises(RuntimeError):
                mod.pointer_zone(path, list(self.RECT))
            path = self.state(tmp, json.dumps({'locked': False}))
            with self.assertRaises(RuntimeError):
                mod.pointer_zone(path, list(self.RECT))

    def test_mistyped_coordinates_rejected(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            for bad in (['a', 2], [True, 1], [1, False], [None, 2],
                        [[1], 2], ['100', '100']):
                path = self.state(
                    tmp, json.dumps({'pointer_position': bad}))
                with self.assertRaises(RuntimeError, msg=f'pos={bad!r}'):
                    mod.pointer_zone(path, list(self.RECT))

    def test_nonfinite_coordinates_rejected(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            for body in ('{"pointer_position": [1e999, 100]}',
                         '{"pointer_position": [100, NaN]}',
                         '{"pointer_position": [Infinity, 100]}'):
                path = self.state(tmp, body)
                with self.assertRaises(RuntimeError, msg=body):
                    mod.pointer_zone(path, list(self.RECT))

    def test_unreadable_state_rejected(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            with self.assertRaises(RuntimeError):
                mod.pointer_zone(tmp / 'gone.json', list(self.RECT))
            path = self.state(tmp, '{oops')
            with self.assertRaises(RuntimeError):
                mod.pointer_zone(path, list(self.RECT))
            path = self.state(tmp, '[]')
            with self.assertRaises(RuntimeError):
                mod.pointer_zone(path, list(self.RECT))

    def test_bad_rect_rejected(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            path = self.state(tmp, json.dumps({'pointer_position': [20, 580]}))
            for bad in ([1, 2, 3], [1.5, 2, 3, 4], [True, 2, 3, 4],
                        'x', None, [100, 100, 0, 200],
                        [100, 100, 320, 0], [100, 100, -5, 200],
                        [100, 100, 320, -5]):
                with self.assertRaises(RuntimeError, msg=f'rect={bad!r}'):
                    mod.pointer_zone(path, bad)

    def test_negative_origin_rect_legal(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            rect = [-50, 500, 320, 200]
            path = self.state(tmp, json.dumps({'pointer_position': [20, 580]}))
            self.assertEqual(mod.pointer_zone(path, list(rect)), 'inside')
            path = self.state(tmp, json.dumps({'pointer_position': [300, 580]}))
            self.assertEqual(mod.pointer_zone(path, list(rect)), 'outside')

    def test_pointer_zone_cli_reports_zone(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            path = self.state(tmp, json.dumps({'pointer_position': [20, 580]}))
            buf = io.StringIO()
            with redirect_stdout(buf):
                mod.main(['pointer-zone', '--cstate', str(path),
                          '--rect', '100,100,320,200'])
            self.assertEqual(buf.getvalue().strip(), 'outside')
            path = self.state(tmp, json.dumps({'pointer_position': [150, 150]}))
            buf = io.StringIO()
            with redirect_stdout(buf):
                mod.main(['pointer-zone', '--cstate', str(path),
                          '--rect', '100,100,320,200'])
            self.assertEqual(buf.getvalue().strip(), 'inside')
            with self.assertRaises(RuntimeError):
                mod.main(['pointer-zone', '--cstate', str(path),
                          '--rect', '100,100,320'])


if __name__ == '__main__':
    unittest.main()
