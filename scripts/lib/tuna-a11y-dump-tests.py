#!/usr/bin/env python3
"""Unit tests for tuna-a11y-dump.py driven through fake pyatspi.

The production dumper module is imported with a hermetic fake
`pyatspi` (no accessibility bus required): strict traversal must
surface every inconsistency as an in-band error node -- a None child
for an advertised index and a drifting childCount are exposed, never
silently skipped -- while a legitimately unsupported Component keeps
null bounds. PID binding is exercised through the real main().
"""
import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest import mock

sys.dont_write_bytecode = True


class _State:
    def __init__(self, states):
        self._states = states

    def contains(self, want):
        return want in self._states


class _Rect:
    def __init__(self, rect):
        self.x, self.y, self.width, self.height = rect


class _Component:
    def __init__(self, rect):
        self._rect = rect

    def getExtents(self, coords):
        return _Rect(self._rect)


class FakeAccessible:
    """Scriptable accessible: children, states, errors, drifting counts."""

    def __init__(self, name, role='button', children=(), states=(),
                 description='', rect=(1, 2, 3, 4), pid=4242, counts=None,
                 child_errors=None, role_error=None, state_error=None,
                 component_error=None):
        self._name = name
        self._role = role
        self._children = list(children)
        self._states = frozenset(states)
        self.description = description
        self._rect = rect
        self._pid = pid
        self._counts = list(counts) if counts else None
        self._child_errors = dict(child_errors or {})
        self._role_error = role_error
        self._state_error = state_error
        self._component_error = component_error

    @property
    def name(self):
        return self._name

    def getRoleName(self):
        if self._role_error is not None:
            raise self._role_error
        return self._role

    def getState(self):
        if self._state_error is not None:
            raise self._state_error
        return _State(self._states)

    @property
    def childCount(self):
        if self._counts is not None:
            if len(self._counts) > 1:
                return self._counts.pop(0)
            return self._counts[0]
        return len(self._children)

    def getChildAtIndex(self, i):
        if i in self._child_errors:
            raise self._child_errors[i]
        if 0 <= i < len(self._children):
            return self._children[i]
        return None

    def queryComponent(self):
        if self._component_error is not None:
            raise self._component_error
        return _Component(self._rect)

    def get_process_id(self):
        return self._pid


class _FakeRegistry:
    def __init__(self):
        self.desktop = FakeAccessible('desktop', role='desktop')

    def getDesktop(self, index):
        assert index == 0
        return self.desktop


class _FakeTime:
    """Scripted clock with recording no-op sleep: bounded, instant."""

    def __init__(self, ticks):
        self._ticks = list(ticks)
        self.sleeps = []

    def time(self):
        if len(self._ticks) > 1:
            return self._ticks.pop(0)
        return self._ticks[0]

    def sleep(self, seconds):
        self.sleeps.append(seconds)


class _FakePyatspi:
    STATE_SHOWING = 'showing'
    STATE_CHECKED = 'checked'
    STATE_PRESSED = 'pressed'
    STATE_SELECTED = 'selected'
    STATE_FOCUSED = 'focused'
    WINDOW_COORDS = 'window'

    def __init__(self):
        self.Registry = _FakeRegistry()


HERE = Path(__file__).resolve().parent
_FAKE = _FakePyatspi()
_real_pyatspi = sys.modules.get('pyatspi')
sys.modules['pyatspi'] = _FAKE
try:
    spec = importlib.util.spec_from_file_location(
        'a11y_dump', HERE / 'tuna-a11y-dump.py')
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
finally:
    if _real_pyatspi is None:
        sys.modules.pop('pyatspi', None)
    else:
        sys.modules['pyatspi'] = _real_pyatspi

APP = 'tuna-shell-gtk'
SHOWING = mod.pyatspi.STATE_SHOWING


def app_node(*children, pid=4242, **kw):
    return FakeAccessible(APP, role='application', children=children,
                          states=(SHOWING,), pid=pid, **kw)


def button(name, showing=True, **kw):
    states = (SHOWING,) if showing else ()
    return FakeAccessible(name, role='button', states=states, **kw)


def errors(out):
    return [n for n in out if n['role'] == 'error']


class StrictTraversal(unittest.TestCase):
    def test_full_tree_strict_has_no_errors(self):
        leaf = button('Leaf')
        mid = button('Mid', children=[leaf])
        root = app_node(mid, button('Take'))
        out = []
        mod.walk(root, 0, out, True)
        self.assertEqual([n['name'] for n in out],
                         [APP, 'Mid', 'Leaf', 'Take'])
        self.assertEqual([n['depth'] for n in out], [0, 1, 2, 1])
        self.assertEqual(errors(out), [])
        for key in ('role', 'name', 'description', 'depth', 'showing',
                    'checked', 'selected', 'focused', 'bounds'):
            self.assertIn(key, out[1])
        self.assertEqual(out[0]['bounds'], [1, 2, 3, 4])
        self.assertIs(out[3]['showing'], True)

    def test_none_advertised_child_exposed_strict(self):
        root = app_node(button('A'), None, button('B'))
        out = []
        mod.walk(root, 0, out, True)
        found = errors(out)
        self.assertEqual(len(found), 1)
        self.assertIn('missing child 1 of 3', found[0]['name'])
        self.assertEqual(found[0]['depth'], 1)
        self.assertIs(found[0]['showing'], False)
        names = [n['name'] for n in out if n['role'] != 'error']
        self.assertEqual(names, [APP, 'A', 'B'])

    def test_none_child_skipped_non_strict(self):
        root = app_node(button('A'), None, button('B'))
        out = []
        mod.walk(root, 0, out, False)
        self.assertEqual(errors(out), [])
        self.assertEqual([n['name'] for n in out], [APP, 'A', 'B'])

    def test_childcount_drift_exposed(self):
        root = app_node(button('A'), button('B'), counts=[2, 5])
        out = []
        mod.walk(root, 0, out, True)
        found = errors(out)
        self.assertEqual(len(found), 1)
        self.assertIn('childCount drifted 2 -> 5', found[0]['name'])
        self.assertEqual(found[0]['depth'], 0)
        names = [n['name'] for n in out if n['role'] != 'error']
        self.assertEqual(names, [APP, 'A', 'B'])
        root = app_node(button('A'), button('B'), counts=[2, 'x'])
        out = []
        mod.walk(root, 0, out, True)
        self.assertEqual(len(errors(out)), 1)

    def test_malformed_end_count_fail_closed(self):
        # True == 1 and 1.0 == 1: equality alone silently accepts these.
        for start, end in ((1, True), (0, False), (1, 1.0)):
            kids = [button('A')] if start == 1 else []
            root = app_node(*kids, counts=[start, end])
            out = []
            mod.walk(root, 0, out, True)
            found = errors(out)
            self.assertEqual(len(found), 1,
                             msg=f'counts=[{start}, {end!r}]')
            self.assertIn('malformed childCount', found[0]['name'])

    def test_malformed_childcount_fail_closed(self):
        for bad in (-1, -100, True, False, '3', None):
            root = app_node(button('A'), counts=[bad])
            out = []
            mod.walk(root, 0, out, True)
            found = errors(out)
            self.assertEqual(len(found), 1, msg=f'count={bad!r}')
            self.assertIn('malformed childCount', found[0]['name'])
            self.assertEqual(found[0]['depth'], 0)
            names = [n['name'] for n in out if n['role'] != 'error']
            self.assertEqual(names, [APP], msg=f'count={bad!r}')
        out = []
        mod.walk(app_node(button('A'), counts=[-1]), 0, out, False)
        self.assertEqual(errors(out), [])

    def test_query_errors_fail_closed(self):
        cases = [
            ('role', {'role_error': RuntimeError('role gone')}),
            ('state', {'state_error': RuntimeError('state gone')}),
            ('component',
             {'component_error': RuntimeError('component exploded')}),
        ]
        for label, kw in cases:
            root = app_node(button('Bad', **kw))
            out = []
            mod.walk(root, 0, out, True)
            found = errors(out)
            self.assertTrue(found, msg=label)
            self.assertTrue(
                any('gone' in e['name'] or 'exploded' in e['name']
                    for e in found), msg=label)

    def test_unsupported_component_keeps_null_bounds(self):
        for strict in (True, False):
            root = app_node(button(
                'Text', component_error=NotImplementedError('no Component')))
            out = []
            mod.walk(root, 0, out, strict)
            self.assertEqual(errors(out), [], msg=f'strict={strict}')
            self.assertIsNone(out[1]['bounds'])
            self.assertEqual(out[1]['name'], 'Text')


class PidBinding(unittest.TestCase):
    def run_main(self, argv, ticks=(1000.0, 1000.0)):
        clock = _FakeTime(ticks)
        with mock.patch.object(sys, 'argv', ['tuna-a11y-dump.py'] + argv), \
                mock.patch.object(mod, 'time', clock):
            return mod.main(), clock

    @staticmethod
    def counting_pid(app, queried):
        real_pid = app.get_process_id

        def counted():
            queried.append(real_pid())
            return queried[-1]

        app.get_process_id = counted

    def test_pid_match_dumps_and_stamps(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            leaf = button('Leaf')
            fake_app = app_node(button('Mid', children=[leaf]),
                                button('Take'), pid=4242)
            _FAKE.Registry.desktop = FakeAccessible(
                'desktop', role='desktop', children=[fake_app])
            out = tmp / 'dump.json'
            code, _ = self.run_main(
                [APP, str(out), '5', '--pid', '4242', '--strict'])
            self.assertEqual(code, 0)
            nodes = json.loads(out.read_text())
            self.assertEqual(nodes[0]['process_id'], 4242)
            self.assertEqual([n['name'] for n in nodes],
                             [APP, 'Mid', 'Leaf', 'Take'])
            self.assertEqual(errors(nodes), [])

    def test_pid_mismatch_queries_then_fails_closed(self):
        # The mocked clock forces loop iterations: the wrong PID must
        # actually be queried (not skipped by a never-entered loop).
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            fake_app = app_node(button('Take'), pid=9999)
            queried = []
            self.counting_pid(fake_app, queried)
            _FAKE.Registry.desktop = FakeAccessible(
                'desktop', role='desktop', children=[fake_app])
            out = tmp / 'dump.json'
            code, _ = self.run_main([APP, str(out), '5', '--pid', '4242'],
                                    ticks=(1000.0, 1000.0, 1006.0))
            self.assertEqual(code, 1)
            self.assertFalse(out.exists())
            self.assertEqual(queried, [9999])

    def test_pid_later_match_is_selected(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            wrong = app_node(button('Decoy'), pid=7)
            owner = app_node(button('Owned'), pid=8)
            queried = []
            self.counting_pid(wrong, queried)
            self.counting_pid(owner, queried)
            _FAKE.Registry.desktop = FakeAccessible(
                'desktop', role='desktop', children=[wrong, owner])
            out = tmp / 'dump.json'
            code, _ = self.run_main(
                [APP, str(out), '5', '--pid', '8', '--strict'])
            self.assertEqual(code, 0)
            nodes = json.loads(out.read_text())
            # Wrong PID queried and skipped, owner matched, then stamped.
            self.assertEqual(queried, [7, 8, 8])
            self.assertEqual(nodes[0]['process_id'], 8)
            self.assertEqual([n['name'] for n in nodes],
                             [APP, 'Owned'])

    def test_absent_app_fails_closed(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            _FAKE.Registry.desktop = FakeAccessible(
                'desktop', role='desktop', children=[])
            out = tmp / 'dump.json'
            code, _ = self.run_main([APP, str(out), '0'])
            self.assertEqual(code, 1)
            self.assertFalse(out.exists())


class _Action:
    def __init__(self, presses, name):
        self.nActions = 1
        self._presses, self._name = presses, name

    def doAction(self, index):
        self._presses.append((self._name, index))


class WhenShowing(unittest.TestCase):
    run_main = PidBinding.run_main

    def tree(self, title_showing, presses):
        take = button('Take')
        take.queryAction = lambda: _Action(presses, 'Take')
        title = FakeAccessible('Time for a Break', role='label',
                               states=(SHOWING,) if title_showing else ())
        _FAKE.Registry.desktop = FakeAccessible(
            'desktop', role='desktop', children=[app_node(title, take)])

    def test_presses_in_the_pass_that_saw_the_title(self):
        with tempfile.TemporaryDirectory() as td:
            out, presses = Path(td) / 'dump.json', []
            self.tree(True, presses)
            code, clock = self.run_main([APP, str(out), '5', '--pid', '4242',
                                         '--when-showing', 'Time for a Break',
                                         '--press', 'Take'])
            self.assertEqual(code, 0)
            self.assertEqual(presses, [('Take', 0)])
            # The dump is the pre-press tree; no post-press wait.
            self.assertIn('Time for a Break', [n['name'] for n in json.loads(out.read_text())])
            self.assertEqual(clock.sleeps, [])

    def test_hidden_title_dumps_without_pressing(self):
        with tempfile.TemporaryDirectory() as td:
            out, presses = Path(td) / 'dump.json', []
            self.tree(False, presses)
            code, _ = self.run_main([APP, str(out), '5', '--pid', '4242',
                                     '--when-showing', 'Time for a Break',
                                     '--press', 'Take'])
            self.assertEqual(code, 3)
            self.assertEqual(presses, [])
            self.assertTrue(out.exists())

    def test_requires_press(self):
        code, _ = self.run_main([APP, '/nonexistent', '5', '--when-showing', 'X'])
        self.assertEqual(code, 2)


if __name__ == '__main__':
    unittest.main()
