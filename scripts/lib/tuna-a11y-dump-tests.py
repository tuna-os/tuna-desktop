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


class _FakeAction:
    def __init__(self, node):
        self._node = node

    @property
    def nActions(self):
        if self._node._action_count_error is not None:
            raise self._node._action_count_error
        return self._node._actions

    def getName(self, index):
        assert index == 0
        if self._node._getname_error is not None:
            raise self._node._getname_error
        return self._node._action_name

    def doAction(self, index):
        self._node.do_calls.append(index)
        if self._node._do_raise is not None:
            raise self._node._do_raise
        return self._node._do_return


class _NoNameAction(_FakeAction):
    def __getattribute__(self, name):
        if name == 'getName':
            raise AttributeError('getName')
        return super().__getattribute__(name)


class _GoneError(Exception):
    """Fake retired-object error with the 2bc triple shape."""

    def __init__(self, domain='atspi_error', code=1,
                 message='Object does not exist at path "/org/foo"'):
        super().__init__(message)
        self.domain = domain
        self.code = code
        self.message = message


def _make_gone(domain='atspi_error', code=1,
               message='Object does not exist at path "/org/foo"'):
    """Real GLib.Error retired signal when gi exists (exercises the exact
    production strict path), else the duck-typed triple fallback."""
    try:
        from gi.repository import GLib
        return GLib.Error.new_literal(GLib.quark_from_string(domain),
                                      message, code)
    except Exception:
        return _GoneError(domain, code, message)


class _NoClearApp:
    """App-shaped object genuinely lacking clear_cache."""

    name = 'tuna-shell-gtk'

    def __init__(self):
        self._pid = 4242

    def get_process_id(self):
        return self._pid


class FakeAccessible:
    """Scriptable accessible: children, states, errors, drifting counts."""

    def __init__(self, name, role='button', children=(), states=(),
                 description='', rect=(1, 2, 3, 4), pid=4242, counts=None,
                 child_errors=None, role_error=None, state_error=None,
                 component_error=None, actions=0, no_action=False,
                 action_count_error=None, do_return=True, do_raise=None,
                 action_name='click', no_getname=False, getname_error=None,
                 clear_error=None, pid_error=None, pids=None):
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
        self._actions = actions
        self._no_action = no_action
        self._action_count_error = action_count_error
        self._do_return = do_return
        self._do_raise = do_raise
        self._action_name = action_name
        self._no_getname = no_getname
        self._getname_error = getname_error
        self._clear_error = clear_error
        self._pid_error = pid_error
        self._pids = list(pids) if pids else None
        self.do_calls = []
        self.clear_calls = 0

    def clear_cache(self):
        if self._clear_error is not None:
            raise self._clear_error
        self.clear_calls += 1

    def queryAction(self):
        if self._no_action:
            raise NotImplementedError('no Action')
        if self._no_getname:
            return _NoNameAction(self)
        return _FakeAction(self)

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
        if self._pid_error is not None:
            raise self._pid_error
        if self._pids is not None:
            if len(self._pids) > 1:
                return self._pids.pop(0)
            return self._pids[0]
        return self._pid


class _FakeRegistry:
    def __init__(self):
        self.desktop = FakeAccessible('desktop', role='desktop')

    def getDesktop(self, index):
        assert index == 0
        return self.desktop


class _FakeTime:
    """Scripted clock with recording no-op sleep: bounded, instant."""

    def __init__(self, ticks, mono=None):
        self._ticks = list(ticks)
        self._mono = list(mono) if mono is not None else None
        self.sleeps = []

    def time(self):
        if len(self._ticks) > 1:
            return self._ticks.pop(0)
        return self._ticks[0]

    def monotonic(self):
        assert self._mono is not None, 'monotonic not scripted'
        if len(self._mono) > 1:
            return self._mono.pop(0)
        return self._mono[0]

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


class _ClearGatedApp(FakeAccessible):
    """App whose name is unreadable until clear_cache ran (order probe)."""

    def __init__(self, *args, **kwargs):
        super().__init__(*args, **kwargs)
        self._gated_cleared = False

    def clear_cache(self):
        super().clear_cache()
        self._gated_cleared = True

    @property
    def name(self):
        if not self._gated_cleared:
            raise RuntimeError('name read before clear_cache')
        return self._name


class _ClearGatedDesktop(FakeAccessible):
    """Desktop whose childCount is unreadable until cleared (order probe)."""

    def __init__(self, children):
        super().__init__('desktop', role='desktop', children=children)
        self._gated_cleared = False

    def clear_cache(self):
        super().clear_cache()
        self._gated_cleared = True

    @property
    def childCount(self):
        if not self._gated_cleared:
            raise RuntimeError('childCount read before clear_cache')
        return len(self._children)


class _FlipButton(FakeAccessible):
    """Take button whose showing state advances per getState call."""

    def __init__(self, name, states_seq, **kw):
        super().__init__(name, states=(), **kw)
        self._seq = [frozenset(s) for s in states_seq]

    def getState(self):
        current = self._seq[0]
        if len(self._seq) > 1:
            self._seq.pop(0)
        return _State(current)


class _FlakyTake(FakeAccessible):
    """Take button raising once, then behaving."""

    def __init__(self, name, first_error, **kw):
        super().__init__(name, **kw)
        self._first = first_error
        self._flaked = False

    def getState(self):
        if not self._flaked:
            self._flaked = True
            raise self._first
        return super().getState()


class _FlipDesktop(FakeAccessible):
    """Desktop whose top-level children change per getChildAtIndex call."""

    def __init__(self, seq):
        super().__init__('desktop', role='desktop')
        self._seq = [list(s) for s in seq]

    @property
    def childCount(self):
        return len(self._seq[0])

    def getChildAtIndex(self, i):
        kids = self._seq[0]
        if len(self._seq) > 1:
            self._seq.pop(0)
        if 0 <= i < len(kids):
            return kids[i]
        return None


class _FlakyDesktop(FakeAccessible):
    """Desktop whose childCount works once, then raises."""

    def __init__(self, children, error):
        super().__init__('desktop', role='desktop', children=children)
        self._error = error
        self._calls = 0

    @property
    def childCount(self):
        self._calls += 1
        if self._calls == 1:
            return len(self._children)
        raise self._error


class PressReadiness(unittest.TestCase):
    def run_press(self, argv, ticks=(1000.0, 1000.0),
                    mono=(1000.0, 1000.0, 1000.0)):
        import contextlib
        import io
        clock = _FakeTime(ticks, mono=mono)
        err = io.StringIO()
        with mock.patch.object(sys, 'argv',
                               ['tuna-a11y-dump.py'] + argv), \
                mock.patch.object(mod, 'time', clock), \
                contextlib.redirect_stderr(err):
            return mod.main(), clock, err.getvalue()

    def set_desktop(self, *children):
        _FAKE.Registry.desktop = FakeAccessible(
            'desktop', role='desktop', children=list(children))

    @staticmethod
    def record(stderr_text):
        line = [ln for ln in stderr_text.splitlines()
                if 'press-failed' in ln]
        assert len(line) == 1, stderr_text
        return json.loads(line[0].split('tuna-a11y-dump: ', 1)[1])

    def test_ready_immediately_invokes_once(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1)
            self.set_desktop(app_node(take, pid=4242))
            out = str(Path(td) / 'press.json')
            code, _, _ = self.run_press(
                [APP, out, '5', '--press', 'Take', '--pid', '4242'])
            self.assertEqual(code, 0)
            self.assertEqual(take.do_calls, [0])
            nodes = json.loads(Path(out).read_text())
            self.assertEqual(nodes[0]['process_id'], 4242)
            self.assertIn('Take', [n['name'] for n in nodes])

    def test_late_candidate_found_within_budget(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = _FlipButton('Take', [(), (SHOWING,)], role='button',
                               actions=1)
            self.set_desktop(app_node(take))
            out = str(Path(td) / 'press.json')
            code, _, _ = self.run_press([APP, out, '5', '--press', 'Take'])
            self.assertEqual(code, 0)
            self.assertEqual(take.do_calls, [0])

    def test_doaction_false_honored_no_second_call(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1, do_return=False)
            self.set_desktop(app_node(take))
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '5', '--press', 'Take'])
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [0])
            rec = self.record(err)
            self.assertIs(rec['returned'], False)
            self.assertIsNone(rec['error'])
            self.assertEqual(rec['invocations'], 1)
            self.assertEqual(rec['actions'], 1)
            self.assertEqual(rec['action'], 'click')
            self.assertEqual(rec['pid'], 4242)
            nodes = json.loads(out.read_text())
            self.assertIn('Take', [n['name'] for n in nodes])

    def test_doaction_none_counts_one_invocation(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1, do_return=None)
            self.set_desktop(app_node(take))
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '5', '--press', 'Take'])
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [0])
            rec = self.record(err)
            self.assertIsNone(rec['returned'])
            self.assertEqual(rec['invocations'], 1)

    def test_role_error_refuses_immediately(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1,
                          role_error=RuntimeError('role gone'))
            self.set_desktop(app_node(take))
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '5', '--press', 'Take'])
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [])
            rec = self.record(err)
            self.assertEqual(rec['observations'], 1)
            self.assertEqual(rec['invocations'], 0)
            self.assertTrue(any('role err=' in c and 'role gone' in c
                                for c in rec['conditions']),
                            rec['conditions'])

    def test_arbitrary_error_fails_without_retry(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1,
                          state_error=RuntimeError('bus boom'))
            self.set_desktop(app_node(take))
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '5', '--press', 'Take'])
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [])
            rec = self.record(err)
            self.assertEqual(rec['observations'], 1)
            self.assertTrue(any('bus boom' in c for c in rec['conditions']),
                            rec['conditions'])

    def test_retired_signal_retries_then_succeeds(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = _FlakyTake('Take', _make_gone(), role='button',
                              states=(SHOWING,), actions=1)
            self.set_desktop(app_node(take))
            out = str(Path(td) / 'press.json')
            code, _, _ = self.run_press([APP, out, '5', '--press', 'Take'])
            self.assertEqual(code, 0)
            self.assertEqual(take.do_calls, [0])

    def test_retired_nearmiss_is_fatal(self):
        import tempfile
        for label, gone in (
                ('code', _make_gone(code=2)),
                ('message', _make_gone(message='gone')),
                ('domain', _make_gone(domain='other'))):
            with tempfile.TemporaryDirectory() as td:
                take = button('Take', actions=1, state_error=gone)
                self.set_desktop(app_node(take))
                out = Path(td) / 'press.json'
                code, _, err = self.run_press(
                    [APP, str(out), '5', '--press', 'Take'])
                self.assertEqual(code, 1, msg=label)
                self.assertEqual(take.do_calls, [], msg=label)
                rec = self.record(err)
                self.assertEqual(rec['observations'], 1, msg=label)

    def test_non_glib_triple_rejected_when_gi_present(self):
        import tempfile
        try:
            from gi.repository import GLib  # noqa: F401
        except Exception:
            self.skipTest('gi absent: duck path accepts the triple')
            return
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1, state_error=_GoneError())
            self.set_desktop(app_node(take))
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '5', '--press', 'Take'])
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [])
            rec = self.record(err)
            self.assertEqual(rec['observations'], 1)

    def test_none_child_retries_then_succeeds(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1)
            fake_app = app_node(take)
            desktop = _FlipDesktop([[fake_app], [None], [fake_app]])
            _FAKE.Registry.desktop = desktop
            out = str(Path(td) / 'press.json')
            code, _, _ = self.run_press([APP, out, '5', '--press', 'Take'])
            self.assertEqual(code, 0)
            self.assertEqual(take.do_calls, [0])

    def test_clear_cache_error_is_fatal(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1,
                          clear_error=RuntimeError('cache gone'))
            self.set_desktop(app_node(take))
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '5', '--press', 'Take'])
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [])
            rec = self.record(err)
            self.assertEqual(rec['observations'], 1)
            self.assertTrue(any('clear_cache' in c for c in rec['conditions']),
                            rec['conditions'])

    def test_clear_cache_missing_is_fatal(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            _FAKE.Registry.desktop = FakeAccessible(
                'desktop', role='desktop', children=[_NoClearApp()])
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '5', '--press', 'Take'])
            self.assertEqual(code, 1)
            rec = self.record(err)
            self.assertEqual(rec['observations'], 1)
            self.assertTrue(any('clear_cache missing' in c
                                for c in rec['conditions']),
                            rec['conditions'])

    def test_node_budget_trip_is_fatal(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', showing=False)
            self.set_desktop(app_node(take, button('B')))
            out = Path(td) / 'press.json'
            with mock.patch.object(mod, 'PRESS_NODES_MAX', 2):
                code, _, err = self.run_press(
                    [APP, str(out), '5', '--press', 'Take'])
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [])
            rec = self.record(err)
            self.assertTrue(any('node budget' in c for c in rec['conditions']),
                            rec['conditions'])

    def test_children_cap_trip_is_fatal(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', showing=False)
            self.set_desktop(app_node(take, button('B')))
            out = Path(td) / 'press.json'
            with mock.patch.object(mod, 'PRESS_CHILDREN_MAX', 1):
                code, _, err = self.run_press(
                    [APP, str(out), '5', '--press', 'Take'])
            self.assertEqual(code, 1)
            rec = self.record(err)
            self.assertTrue(any('exceeds' in c for c in rec['conditions']),
                            rec['conditions'])

    def test_depth_cap_trip_is_fatal(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', showing=False)
            self.set_desktop(app_node(button('Mid', children=[take])))
            out = Path(td) / 'press.json'
            with mock.patch.object(mod, 'PRESS_DEPTH_MAX', 1):
                code, _, err = self.run_press(
                    [APP, str(out), '5', '--press', 'Take'])
            self.assertEqual(code, 1)
            rec = self.record(err)
            self.assertTrue(any('max depth' in c for c in rec['conditions']),
                            rec['conditions'])

    def test_late_ready_invokes_zero_times(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1, do_return=True)
            self.set_desktop(app_node(take))
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '2', '--press', 'Take'],
                ticks=(1000.0, 1000.0),
                mono=(1000.0, 1000.0, 1005.0))
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [])
            rec = self.record(err)
            self.assertEqual(rec['invocations'], 0)
            self.assertIn('late-ready', rec['error'])

    def test_post_owner_expiry_refuses_true(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1, do_return=True)
            self.set_desktop(app_node(take))
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '5', '--press', 'Take'],
                ticks=(1000.0, 1000.0),
                mono=(1000.0, 1000.0, 1000.0, 1000.0, 1000.0, 1006.0))
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [0])
            rec = self.record(err)
            self.assertIs(rec['returned'], True)
            self.assertEqual(rec['invocations'], 1)
            self.assertIn('post-owner past deadline', rec['error'])

    def test_final_dump_expiry_refuses_true(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1, do_return=True)
            self.set_desktop(app_node(take))
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '5', '--press', 'Take'],
                ticks=(1000.0, 1000.0),
                mono=(1000.0, 1000.0, 1000.0, 1000.0, 1000.0, 1000.0,
                      1000.0, 1000.0, 1009.0))
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [0])
            rec = self.record(err)
            self.assertIs(rec['returned'], True)
            self.assertIn('late-success refused', rec['error'])

    def test_action_name_unsupported_keeps_candidate(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1, no_getname=True)
            self.set_desktop(app_node(take))
            out = str(Path(td) / 'press.json')
            code, _, _ = self.run_press([APP, out, '5', '--press', 'Take'])
            self.assertEqual(code, 0)
            self.assertEqual(take.do_calls, [0])

    def test_action_name_error_is_fatal(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1,
                          getname_error=RuntimeError('name boom'))
            self.set_desktop(app_node(take))
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '5', '--press', 'Take'])
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [])
            rec = self.record(err)
            self.assertTrue(any('name boom' in c for c in rec['conditions']),
                            rec['conditions'])

    def test_resolve_desktop_error_is_fatal(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1)
            fake_app = app_node(take)
            desktop = _FlakyDesktop([fake_app], RuntimeError('bus timeout'))
            _FAKE.Registry.desktop = desktop
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '5', '--press', 'Take'])
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [])
            rec = self.record(err)
            self.assertEqual(rec['observations'], 1)
            self.assertTrue(any('bus timeout' in c for c in rec['conditions']),
                            rec['conditions'])

    def test_ambiguous_owner_is_fatal(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1)
            self.set_desktop(app_node(take, pid=1),
                             app_node(button('Other'), pid=2))
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '5', '--press', 'Take'])
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [])
            rec = self.record(err)
            self.assertTrue(any('unique' in c for c in rec['conditions']),
                            rec['conditions'])

    def test_wall_jump_cannot_extend_press_budget(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', showing=False)
            self.set_desktop(app_node(take))
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '5', '--press', 'Take'],
                ticks=(1000.0, 0.0),
                mono=(1000.0, 1000.0, 1006.0))
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [])
            rec = self.record(err)
            self.assertEqual(rec['observations'], 1)

    def test_pre_pid_error_invokes_zero_times(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1, do_return=True)
            fake_app = app_node(take, pid_error=RuntimeError('pid gone'))
            self.set_desktop(fake_app)
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '5', '--press', 'Take'])
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [])
            rec = self.record(err)
            self.assertEqual(rec['invocations'], 0)
            self.assertIn('pre-pid', rec['error'])

    def test_pre_pid_nonint_invokes_zero_times(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1, do_return=True)
            self.set_desktop(app_node(take, pid='4242'))
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '5', '--press', 'Take'])
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [])
            rec = self.record(err)
            self.assertEqual(rec['invocations'], 0)
            self.assertIn('pre-pid non-positive', rec['error'])

    def test_invocation_gate_expiry_invokes_zero_times(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1, do_return=True)
            self.set_desktop(app_node(take))
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '2', '--press', 'Take'],
                ticks=(1000.0, 1000.0),
                mono=(1000.0, 1000.0, 1000.0, 1005.0))
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [])
            rec = self.record(err)
            self.assertEqual(rec['invocations'], 0)
            self.assertIn('invocation gate', rec['error'])

    def test_resolve_clears_before_reads(self):
        take = button('Take', actions=1)
        gated_app = _ClearGatedApp(APP, role='application',
                                   children=[take], pid=4242)
        gated = _ClearGatedDesktop([gated_app])
        _FAKE.Registry.desktop = gated
        matches = mod.resolve_named_app(APP, None, [])
        self.assertEqual(matches, [gated_app])
        self.assertGreater(gated.clear_calls, 0)
        self.assertGreater(gated_app.clear_calls, 0)

    def test_probe_clears_before_reads(self):
        take = button('Take', actions=1)
        gated_app = _ClearGatedApp(APP, role='application',
                                   children=[take], pid=4242)
        found = mod.press_candidate(gated_app, 'Take', [], 0, {'nodes': 0})
        self.assertIsNotNone(found)
        self.assertEqual(found[2], 1)
        self.assertEqual(found[3], 'click')
        self.assertGreater(gated_app.clear_calls, 0)
        self.assertGreater(take.clear_calls, 0)

    def test_capped_postmortem_noted_and_bounded(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', showing=False)
            self.set_desktop(app_node(take, button('B')))
            out = Path(td) / 'press.json'
            with mock.patch.object(mod, 'PRESS_NODES_MAX', 2):
                code, _, err = self.run_press(
                    [APP, str(out), '2', '--press', 'Take'],
                    ticks=(1000.0, 1000.0),
                    mono=(1000.0, 1000.0, 1003.0))
            self.assertEqual(code, 1)
            rec = self.record(err)
            nodes = json.loads(out.read_text())
            self.assertLessEqual(len(nodes), 2)
            self.assertTrue(any('postmortem tree capped' in c
                                for c in rec['conditions']),
                            rec['conditions'])

    def test_capped_success_dump_fails_closed(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1, do_return=True)
            sibs = [button('S%d' % i) for i in range(3)]
            self.set_desktop(app_node(take, *sibs))
            out = Path(td) / 'press.json'
            with mock.patch.object(mod, 'PRESS_NODES_MAX', 2):
                code, _, err = self.run_press(
                    [APP, str(out), '5', '--press', 'Take'])
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [0])
            rec = self.record(err)
            self.assertIs(rec['returned'], True)
            self.assertIn('success dump capped', rec['error'])

    def test_walk_limit_bounds_records_and_traversal(self):
        kids = [button('K%d' % i) for i in range(5)]
        fake_app = app_node(*kids)
        out, stats = [], {}
        mod.walk(fake_app, 0, out, False, 3, stats)
        self.assertLessEqual(len(out), 3)
        self.assertTrue(stats.get('capped'))
        out2, stats2 = [], {}
        mod.walk(fake_app, 0, out2, False, None, stats2)
        self.assertEqual(len(out2), 6)
        self.assertNotIn('capped', stats2)

    def test_doaction_exception_honored_no_second_call(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=2,
                          do_raise=RuntimeError('bus gone'))
            self.set_desktop(app_node(take))
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '5', '--press', 'Take'])
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [0])
            rec = self.record(err)
            self.assertIsNone(rec['returned'])
            self.assertIn('RuntimeError', rec['error'])
            self.assertIn('bus gone', rec['error'])
            self.assertEqual(rec['invocations'], 1)
            self.assertTrue(out.exists())

    def test_budget_expiry_retains_conditions_never_invokes(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', showing=False, actions=1)
            zero = button('Take', actions=0)
            self.set_desktop(app_node(take, zero))
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '2', '--press', 'Take'],
                ticks=(1000.0, 1000.0, 1000.0),
                mono=(1000.0, 1000.0, 1003.0))
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [])
            self.assertEqual(zero.do_calls, [])
            rec = self.record(err)
            self.assertEqual(rec['invocations'], 0)
            self.assertIsNone(rec['returned'])
            self.assertTrue(any('unshowing' in c for c in rec['conditions']),
                            rec['conditions'])
            self.assertTrue(any('actions=0' in c for c in rec['conditions']),
                            rec['conditions'])
            self.assertTrue(out.exists())

    def test_label_only_never_pressed(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            lbl = FakeAccessible('Take', role='label', states=(SHOWING,),
                                 actions=1)
            self.set_desktop(app_node(lbl))
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '2', '--press', 'Take'],
                ticks=(1000.0, 1000.0, 1000.0),
                mono=(1000.0, 1000.0, 1003.0))
            self.assertEqual(code, 1)
            self.assertEqual(lbl.do_calls, [])
            rec = self.record(err)
            self.assertTrue(any('label' in c for c in rec['conditions']),
                            rec['conditions'])

    def test_none_branch_recorded_explicitly(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            self.set_desktop(app_node(button('A'), None))
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '2', '--press', 'Take'],
                ticks=(1000.0, 1000.0, 1000.0),
                mono=(1000.0, 1000.0, 1003.0))
            self.assertEqual(code, 1)
            rec = self.record(err)
            self.assertTrue(any('None' in c for c in rec['conditions']),
                            rec['conditions'])

    def test_late_true_refused_never_positive(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1, do_return=True)
            self.set_desktop(app_node(take))
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '2', '--press', 'Take'],
                ticks=(1000.0, 1000.0),
                mono=(1000.0, 1000.0, 1000.0, 1000.0, 1005.0))
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [0])
            rec = self.record(err)
            self.assertIs(rec['returned'], True)
            self.assertIn('late-positive refused', rec['error'])

    def test_owner_change_refused_after_true(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1, do_return=True)
            fake_app = app_node(take, pid=4242)
            pids = [4242, 9999]

            def drifting():
                if len(pids) > 1:
                    return pids.pop(0)
                return pids[0]
            fake_app.get_process_id = drifting
            self.set_desktop(fake_app)
            out = Path(td) / 'press.json'
            code, _, err = self.run_press([APP, str(out), '5', '--press',
                                           'Take'])
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [0])
            rec = self.record(err)
            self.assertIs(rec['returned'], True)
            self.assertIn('owner identity', rec['error'])
            self.assertIn('4242', rec['error'])
            self.assertIn('9999', rec['error'])


class PressV4Bounds(unittest.TestCase):
    """v4: record-independent walk bounds, gated pre-PID, malformed
    action counts fail, honest failure scope. Standalone (no parent
    tests re-run); helpers mirror PressReadiness exactly."""

    def run_press(self, argv, ticks=(1000.0, 1000.0),
                    mono=(1000.0, 1000.0, 1000.0)):
        import contextlib
        import io
        clock = _FakeTime(ticks, mono=mono)
        err = io.StringIO()
        with mock.patch.object(sys, 'argv',
                               ['tuna-a11y-dump.py'] + argv), \
                mock.patch.object(mod, 'time', clock), \
                contextlib.redirect_stderr(err):
            return mod.main(), clock, err.getvalue()

    def set_desktop(self, *children):
        _FAKE.Registry.desktop = FakeAccessible(
            'desktop', role='desktop', children=list(children))

    @staticmethod
    def record(stderr_text):
        line = [ln for ln in stderr_text.splitlines()
                if 'press-failed' in ln]
        assert len(line) == 1, stderr_text
        return json.loads(line[0].split('tuna-a11y-dump: ', 1)[1])

    def test_walk_huge_none_flood_trips_before_any_read(self):
        fake_app = app_node(counts=[2000])
        reads = []
        orig = fake_app.getChildAtIndex

        def counted(i):
            reads.append(i)
            return orig(i)

        fake_app.getChildAtIndex = counted
        out, stats = [], {}
        mod.walk(fake_app, 0, out, False, mod.PRESS_NODES_MAX, stats)
        self.assertTrue(stats.get('capped'))
        self.assertEqual(reads, [])
        self.assertEqual(len(out), 1)

    def test_walk_none_run_counts_visits_not_records(self):
        fake_app = app_node(counts=[3])
        out, stats = [], {}
        mod.walk(fake_app, 0, out, False, mod.PRESS_NODES_MAX, stats)
        self.assertNotIn('capped', stats)
        self.assertEqual(stats.get('visits'), 4)
        self.assertEqual(len(out), 1)

    def test_walk_malformed_count_trips_bounded_dump(self):
        for bad in (True, -1, 1.5, '3'):
            with self.subTest(count=bad):
                fake_app = app_node(button('K'), counts=[bad])
                out, stats = [], {}
                mod.walk(fake_app, 0, out, False, mod.PRESS_NODES_MAX,
                         stats)
                self.assertTrue(stats.get('capped'), bad)
                self.assertEqual(len(out), 1)

    def test_walk_deep_chain_trips_bounded_dump(self):
        node = button('Leaf')
        for i in range(70):
            node = FakeAccessible('N%d' % i, role='panel',
                                  children=[node])
        fake_app = app_node(node)
        out, stats = [], {}
        mod.walk(fake_app, 0, out, False, mod.PRESS_NODES_MAX, stats)
        self.assertTrue(stats.get('capped'))
        self.assertLess(len(out), 72)

    def test_walk_deadline_trip_flags_distinctly(self):
        clock = _FakeTime((1000.0,), mono=(1000.0, 1009.0))
        fake_app = app_node(button('K'))
        out, stats = [], {}
        with mock.patch.object(mod, 'time', clock):
            mod.walk(fake_app, 0, out, False, 100, stats,
                     deadline=1005.0)
        self.assertTrue(stats.get('deadline_tripped'))
        self.assertNotIn('capped', stats)
        self.assertEqual(len(out), 1)

    def test_walk_unlimited_legacy_ignores_new_bounds(self):
        kid = button('K')
        legacy = app_node(kid, counts=[True])
        out = []
        mod.walk(legacy, 0, out, False, None, None, deadline=0.0)
        self.assertEqual([n['name'] for n in out], [APP, 'K'])
        negative = app_node(kid, counts=[-1])
        out2 = []
        mod.walk(negative, 0, out2, False, None, None, deadline=0.0)
        self.assertEqual([n['name'] for n in out2], [APP])

    def test_malformed_action_count_fails_whole_walk(self):
        import tempfile
        for bad in (True, 1.5, -1, '1'):
            with self.subTest(count=bad):
                with tempfile.TemporaryDirectory() as td:
                    take = button('Take', actions=bad)
                    self.set_desktop(app_node(take))
                    out = Path(td) / 'press.json'
                    code, _, err = self.run_press(
                        [APP, str(out), '5', '--press', 'Take'])
                    self.assertEqual(code, 1)
                    self.assertEqual(take.do_calls, [])
                    rec = self.record(err)
                    self.assertEqual(rec['observations'], 1)
                    self.assertTrue(any('malformed actions' in c
                                       for c in rec['conditions']),
                                    rec['conditions'])

    def test_zero_actions_sibling_still_pressed(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            zero = button('Take', actions=0)
            ready = button('Take', actions=1, do_return=True)
            self.set_desktop(app_node(zero, ready))
            out = str(Path(td) / 'press.json')
            code, _, _ = self.run_press(
                [APP, out, '5', '--press', 'Take'])
            self.assertEqual(code, 0)
            self.assertEqual(zero.do_calls, [])
            self.assertEqual(ready.do_calls, [0])

    def test_malformed_sibling_blocks_ready_candidate(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            bad = button('Take', actions=True)
            ready = button('Take', actions=1, do_return=True)
            self.set_desktop(app_node(bad, ready))
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '5', '--press', 'Take'])
            self.assertEqual(code, 1)
            self.assertEqual(bad.do_calls, [])
            self.assertEqual(ready.do_calls, [])
            rec = self.record(err)
            self.assertTrue(any('malformed actions' in c
                               for c in rec['conditions']),
                            rec['conditions'])

    def test_pre_pid_nonpositive_refused_without_invocation(self):
        import tempfile
        for bad in (0, -5):
            with self.subTest(pid=bad):
                with tempfile.TemporaryDirectory() as td:
                    take = button('Take', actions=1, do_return=True)
                    self.set_desktop(app_node(take, pid=bad))
                    out = Path(td) / 'press.json'
                    code, _, err = self.run_press(
                        [APP, str(out), '5', '--press', 'Take'])
                    self.assertEqual(code, 1)
                    self.assertEqual(take.do_calls, [])
                    rec = self.record(err)
                    self.assertEqual(rec['invocations'], 0)
                    self.assertIn('pre-pid non-positive', rec['error'])

    def test_pre_pid_mismatch_refused_without_invocation(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1, do_return=True)
            fake_app = app_node(take, pids=[4242, 4242, 9999])
            self.set_desktop(fake_app)
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '5', '--press', 'Take',
                 '--pid', '4242'])
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [])
            rec = self.record(err)
            self.assertEqual(rec['invocations'], 0)
            self.assertIn('pre-pid owner mismatch', rec['error'])
            self.assertIn('9999', rec['error'])

    def test_success_dump_deadline_trip_fails_closed(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1, do_return=True)
            self.set_desktop(app_node(take))
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '5', '--press', 'Take'],
                ticks=(1000.0, 1000.0),
                mono=(1000.0, 1000.0, 1000.0, 1000.0, 1000.0, 1000.0,
                      1000.0, 1009.0))
            self.assertEqual(code, 1)
            self.assertEqual(take.do_calls, [0])
            rec = self.record(err)
            self.assertIs(rec['returned'], True)
            self.assertEqual(rec['invocations'], 1)
            self.assertIn('success dump deadline tripped', rec['error'])

    def test_failure_scope_names_failed_check(self):
        import tempfile
        with tempfile.TemporaryDirectory() as td:
            take = button('Take', actions=1, do_return=True)
            self.set_desktop(app_node(take))
            out = Path(td) / 'press.json'
            code, _, err = self.run_press(
                [APP, str(out), '5', '--press', 'Take'],
                ticks=(1000.0, 1000.0),
                mono=(1000.0, 1000.0, 1000.0, 1000.0, 1000.0, 1000.0,
                      1000.0, 1009.0))
            self.assertEqual(code, 1)
            rec = self.record(err)
            self.assertIn('failed at:', rec['scope'])
            self.assertIn('success dump deadline tripped', rec['scope'])
            self.assertNotIn('every readout inside', rec['scope'])


if __name__ == '__main__':
    unittest.main()
