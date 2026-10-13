#!/usr/bin/env python3
"""Safe controls for the actual Nautilus waiter/walk; no GI or display launch."""
import ast
from pathlib import Path
import re
from types import SimpleNamespace
import unittest

SOURCE = Path(__file__).with_name('tuna-nautilus-file-operations.py')


class Error(Exception):
    def __init__(self, message, domain='atspi_error', code=1):
        super().__init__(message)
        self.message, self.domain, self.code = message, domain, code


def removed():
    return Error('Object does not exist at path “/org/gnome/Nautilus/a11y/retired”')


class Clock:
    now = 0.0

    def monotonic(self):
        return self.now

    def sleep(self, value):
        self.now += value


class Tests(unittest.TestCase):
    def setUp(self):
        self.clock = Clock()
        self.guards = []
        self.ns = {'re': re, 'time': self.clock, 'GLib': SimpleNamespace(Error=Error),
                   'focused': lambda: self.guards.append(1)}
        names = {'removed_accessible', 'wait', 'controls', 'cell', 'select',
                 'owned_unlocked', 'focused', 'wait_for_initial_map', 'snapshot',
                 'wait_for_browser_close'}
        classes = {'IncompleteTree'}
        body = ast.parse(SOURCE.read_text()).body
        functions = [node for node in body
                     if isinstance(node, ast.FunctionDef) and node.name in names]
        defined = [node for node in body
                   if isinstance(node, ast.ClassDef) and node.name in classes]
        self.assertEqual({node.name for node in functions}, names)
        self.assertEqual({node.name for node in defined}, classes)
        exec(compile(ast.Module(body=defined + functions, type_ignores=[]), str(SOURCE), 'exec'), self.ns)
        self.actual_focused = self.ns['focused']
        self.ns['focused'] = lambda: self.guards.append(1)

    def wait(self, predicate):
        return self.ns['wait'](predicate, 'retain strict actual contents')

    def test_closed_original_window_allows_resulting_focus_loss_or_app_exit(self):
        self.ns['scene'] = lambda: {'windows': [], 'focused_app_id': 'another-app'}
        self.ns['focused'] = lambda: self.fail('closed window cannot retain app focus')
        self.ns['wait_for_browser_close'](42)

    def test_still_mapped_window_retains_strict_focus_guard(self):
        self.ns['scene'] = lambda: {'windows': [{'id': 42}]}
        def lost():
            raise RuntimeError('Files lost actual compositor keyboard focus')
        self.ns['focused'] = lost
        with self.assertRaisesRegex(RuntimeError, 'keyboard focus'):
            self.ns['wait_for_browser_close'](42)

    def test_close_between_scene_and_guard_requires_two_actual_absent_reads(self):
        values = iter([{'windows': [{'id': 42}]}, {'windows': []}, {'windows': []}])
        self.ns['scene'] = lambda: next(values)
        def lost():
            raise RuntimeError('Files lost actual compositor keyboard focus')
        self.ns['focused'] = lost
        self.ns['wait_for_browser_close'](42)
        with self.assertRaises(StopIteration):
            next(values)

    def test_owner_failure_cannot_be_hidden_by_later_window_removal(self):
        self.ns['scene'] = lambda: {'windows': [{'id': 42}]}
        def replaced():
            raise RuntimeError('Files original bus owner or installed process changed')
        self.ns['focused'] = replaced
        with self.assertRaisesRegex(RuntimeError, 'owner or installed process changed'):
            self.ns['wait_for_browser_close'](42)

    def test_locking_after_absent_read_is_terminal(self):
        reads = []
        def scene():
            reads.append(1)
            if len(reads) == 2:
                raise RuntimeError('Files operation attempted while the desktop is locked')
            return {'windows': []}
        self.ns['scene'] = scene
        with self.assertRaisesRegex(RuntimeError, 'locked'):
            self.ns['wait_for_browser_close'](42)

    def test_original_window_reappearance_cannot_admit(self):
        values = iter([{'windows': []}, {'windows': [{'id': 42}]}])
        self.ns['scene'] = lambda: next(values)
        with self.assertRaisesRegex(RuntimeError, 'reappeared'):
            self.ns['wait_for_browser_close'](42)

    def test_late_final_absence_is_refused(self):
        reads = []
        def scene():
            reads.append(1)
            if len(reads) == 2:
                self.clock.now = 10
            return {'windows': []}
        self.ns['scene'] = scene
        with self.assertRaisesRegex(RuntimeError, 'did not close'):
            self.ns['wait_for_browser_close'](42)

    def test_close_poll_expires_without_replaying_input(self):
        self.ns['scene'] = lambda: {'windows': [{'id': 42}]}
        self.ns['key'] = lambda _: self.fail('close action replayed')
        with self.assertRaisesRegex(RuntimeError, 'did not close'):
            self.ns['wait_for_browser_close'](42)
        self.assertEqual(self.clock.now, 10)

    def test_invalid_original_window_ids_are_refused_before_observation(self):
        self.ns['scene'] = lambda: self.fail('invalid original window observed')
        for bad in (True, 0, -1, '42'):
            with self.subTest(value=bad), self.assertRaises(RuntimeError):
                self.ns['wait_for_browser_close'](bad)

    def startup(self, *, locked=False, changed=False, focus_at=.1):
        original = {'owner': ':1.42', 'pid': 10}
        def scene():
            if locked:
                raise RuntimeError('locked')
            return {'focused_app_id': ('org.gnome.Nautilus'
                                      if self.clock.now >= focus_at else None)}
        self.ns.update(identity=original, scene=scene,
                       nautilus_identity=lambda: ({'owner': ':1.43', 'pid': 11}
                                                  if changed else original),
                       focused=self.actual_focused, cell=lambda _: object())

    def test_initial_map_waits_for_genuine_focus_then_admits(self):
        self.startup()
        self.ns['wait_for_initial_map']('seed.txt')
        self.assertGreaterEqual(self.clock.now, .1)
        self.assertLess(self.clock.now, 10)

    def test_initial_map_locked_or_changed_owner_never_admits(self):
        for options, message in [({'locked': True}, 'locked'),
                                 ({'changed': True}, 'owner or installed process changed')]:
            self.startup(**options)
            with self.assertRaisesRegex(RuntimeError, message):
                self.ns['wait_for_initial_map']('seed.txt')

    def test_initial_owner_or_lock_change_during_cell_read_never_admits(self):
        for failure in ('owner', 'lock'):
            self.startup(focus_at=0)
            original_scene, original_owner = self.ns['scene'], self.ns['nautilus_identity']
            changed = [False]
            def cell(_):
                changed[0] = True
                return object()
            def scene():
                if failure == 'lock' and changed[0]:
                    raise RuntimeError('locked')
                return original_scene()
            self.ns.update(cell=cell, scene=scene,
                nautilus_identity=lambda: ({'owner': ':1.99', 'pid': 99}
                    if failure == 'owner' and changed[0] else original_owner()))
            with self.assertRaises(RuntimeError):
                self.ns['wait_for_initial_map']('seed.txt')

    def test_initial_strict_final_focus_guard_cannot_finish_late(self):
        self.startup(focus_at=0)
        def late_focus():
            current = self.actual_focused()
            self.clock.now = 10.0
            return current
        self.ns['focused'] = late_focus
        with self.assertRaisesRegex(RuntimeError, 'Files did not'):
            self.ns['wait_for_initial_map']('seed.txt')

    def test_initial_focus_never_arrives_keeps_original_deadline(self):
        self.startup(focus_at=20)
        with self.assertRaisesRegex(RuntimeError, 'Files did not'):
            self.ns['wait_for_initial_map']('seed.txt')
        self.assertEqual(self.clock.now, 10)

    def test_initial_late_mapped_cell_never_admits(self):
        self.startup(focus_at=0)
        def cell(_):
            self.clock.now = 10
            return object()
        self.ns['cell'] = cell
        with self.assertRaisesRegex(RuntimeError, 'Files did not'):
            self.ns['wait_for_initial_map']('seed.txt')

    def test_initial_accepted_predicate_still_requires_strict_focus(self):
        self.startup(focus_at=20)
        with self.assertRaisesRegex(RuntimeError, 'keyboard focus'):
            self.ns['wait'](lambda: True, 'map', startup=True)

    def test_later_focus_loss_never_becomes_startup_readiness(self):
        self.startup(focus_at=20)
        with self.assertRaisesRegex(RuntimeError, 'keyboard focus'):
            self.wait(lambda: True)

    def test_classifier_is_exact_domain_code_message_and_error_type(self):
        self.assertTrue(self.ns['removed_accessible'](removed()))
        for error in [Error('connection closed'), Error(removed().message, code=2),
                      Error(removed().message, domain='other'), RuntimeError(removed().message),
                      Error(removed().message + ' and another failure')]:
            self.assertFalse(self.ns['removed_accessible'](error))

    def test_known_removed_role_rewalks_and_revalidates_owner(self):
        calls = []
        class Node:
            name = 'seed.txt'
            def getRoleName(node):
                if len(calls) == 1:
                    raise removed()
                return 'table cell'
        def controls():
            calls.append(1)
            return [Node()]
        self.ns['controls'] = controls
        found = self.wait(lambda: self.ns['cell']('seed.txt'))
        self.assertEqual(found.name, 'seed.txt')
        self.assertEqual(len(calls), 2)
        self.assertEqual(len(self.guards), 4)

    def test_original_owner_loss_after_removed_error_is_not_retried(self):
        calls = []
        def guard():
            self.guards.append(1)
            if len(self.guards) == 2:
                raise RuntimeError('original owner changed')
        self.ns['focused'] = guard
        def predicate():
            calls.append(1)
            raise removed()
        with self.assertRaisesRegex(RuntimeError, 'owner changed'):
            self.wait(predicate)
        self.assertEqual(calls, [1])

    def test_initial_owner_denial_never_reads_or_acts(self):
        calls = []
        self.ns['focused'] = lambda: (_ for _ in ()).throw(RuntimeError('locked'))
        with self.assertRaisesRegex(RuntimeError, 'locked'):
            self.wait(lambda: calls.append(1))
        self.assertEqual(calls, [])

    def test_other_glib_and_identity_errors_propagate(self):
        for error in [Error('transport unavailable'), Error(removed().message, code=2),
                      RuntimeError('identity changed')]:
            calls = []
            def predicate():
                calls.append(1)
                raise error
            with self.assertRaises(type(error)):
                self.wait(predicate)
            self.assertEqual(calls, [1])

    def test_late_positive_predicate_cannot_qualify(self):
        def predicate():
            self.clock.now = 10.0
            return True
        with self.assertRaisesRegex(RuntimeError, 'Files did not'):
            self.wait(predicate)

    def test_late_post_owner_guard_cannot_qualify(self):
        def guard():
            self.guards.append(1)
            if len(self.guards) == 2:
                self.clock.now = 11.0
        self.ns['focused'] = guard
        with self.assertRaisesRegex(RuntimeError, 'Files did not'):
            self.wait(lambda: True)

    def test_removed_retry_keeps_original_deadline(self):
        calls = []
        def predicate():
            calls.append(1)
            self.clock.now += 6.0
            raise removed()
        with self.assertRaisesRegex(RuntimeError, 'Files did not'):
            self.wait(predicate)
        self.assertEqual(len(calls), 2)
        self.assertLess(self.clock.now, 20)

    def test_duplicate_cells_never_satisfy_uniqueness(self):
        class Node:
            name = 'seed.txt'
            def getRoleName(self):
                return 'table cell'
        self.ns['controls'] = lambda: [Node(), Node()]
        with self.assertRaisesRegex(RuntimeError, 'Files did not'):
            self.wait(lambda: self.ns['cell']('seed.txt'))

    def walker(self, node):
        app = SimpleNamespace(get_process_id=lambda: 10, clear_cache=lambda: None,
                              getState=lambda: SimpleNamespace(contains=lambda _: True),
                              childCount=1, getChildAtIndex=lambda _: node)
        desktop = SimpleNamespace(childCount=1, getChildAtIndex=lambda _: app)
        self.ns.update(identity={'pid': 10},
                       GLib=SimpleNamespace(Error=Error, MainContext=SimpleNamespace(
                           default=lambda: SimpleNamespace(pending=lambda: False))),
                       pyatspi=SimpleNamespace(STATE_SHOWING=1, Registry=SimpleNamespace(
                           getDesktop=lambda _: desktop)))

    def test_actual_owner_walk_propagates_removed_and_arbitrary_error(self):
        for error in [removed(), Error('transport broken'), RuntimeError('bad tree')]:
            self.walker(SimpleNamespace(clear_cache=lambda: (_ for _ in ()).throw(error)))
            with self.assertRaises(type(error)):
                self.ns['controls']()

    def test_unrelated_removed_owner_is_skipped_but_transport_is_not(self):
        for error, should_raise in [(removed(), False), (Error('transport broken'), True)]:
            app = SimpleNamespace(get_process_id=lambda: (_ for _ in ()).throw(error))
            self.walker(None)
            self.ns['pyatspi'].Registry.getDesktop = lambda _: SimpleNamespace(
                childCount=1, getChildAtIndex=lambda _: app)
            if should_raise:
                with self.assertRaises(Error):
                    self.ns['controls']()
            else:
                self.assertEqual(self.ns['controls'](), [])

    def test_none_child_invalidates_entire_walk_without_partial_result(self):
        Incomplete = self.ns['IncompleteTree']
        self.walker(None)
        with self.assertRaises(Incomplete):
            self.ns['controls']()
        self.walker(SimpleNamespace(clear_cache=lambda: None,
                                    getState=lambda: SimpleNamespace(contains=lambda _: True),
                                    childCount=0, getChildAtIndex=lambda _: None))
        self.ns['pyatspi'].Registry.getDesktop = lambda _: SimpleNamespace(
            childCount=1, getChildAtIndex=lambda _: None)
        with self.assertRaises(Incomplete):
            self.ns['controls']()
        leaf = SimpleNamespace(clear_cache=lambda: None,
                               getState=lambda: SimpleNamespace(contains=lambda _: True),
                               childCount=0, getChildAtIndex=lambda _: None)
        self.walker(leaf)
        found = self.ns['controls']()
        self.assertEqual(len(found), 2)

    def test_transient_none_rewalks_to_positive_within_original_budget(self):
        Incomplete = self.ns['IncompleteTree']
        calls = []
        class Node:
            name = 'seed.txt'
            def getRoleName(self):
                return 'table cell'
        def controls():
            calls.append(1)
            if len(calls) == 1:
                raise Incomplete('transient incomplete accessibility tree: missing child')
            return [Node()]
        self.ns['controls'] = controls
        found = self.wait(lambda: self.ns['cell']('seed.txt'))
        self.assertEqual(found.name, 'seed.txt')
        self.assertEqual(len(calls), 2)
        self.assertEqual(len(self.guards), 4)
        self.assertLess(self.clock.now, 10)

    def test_persistent_none_never_admits_beyond_original_deadline(self):
        Incomplete = self.ns['IncompleteTree']
        calls = []
        def predicate():
            calls.append(1)
            raise Incomplete('transient incomplete accessibility tree: missing child')
        with self.assertRaisesRegex(RuntimeError, 'Files did not'):
            self.wait(predicate)
        self.assertEqual(self.clock.now, 10)
        self.assertGreater(len(calls), 1)
        self.assertEqual(len(self.guards), 2 * len(calls))

    def test_non_none_errors_propagate_without_becoming_readiness(self):
        Incomplete = self.ns['IncompleteTree']
        self.assertFalse(self.ns['removed_accessible'](Incomplete('x')))
        for error in [Error('transport broken'), AttributeError('clear_cache boom'),
                      RuntimeError('bad tree')]:
            def predicate(err=error):
                raise err
            with self.assertRaises(type(error)):
                self.wait(predicate)
            self.walker(SimpleNamespace(
                clear_cache=lambda err=error: (_ for _ in ()).throw(err)))
            with self.assertRaises(type(error)):
                self.ns['controls']()

    def test_snapshot_retries_transient_none_then_captures_frame(self):
        import json
        import tempfile
        Incomplete = self.ns['IncompleteTree']
        tmp = Path(tempfile.mkdtemp())
        calls = []
        focus_calls = []
        class Frame:
            name = 'Files'
            def getRoleName(self):
                return 'frame'
            def getState(self):
                return SimpleNamespace(contains=lambda _: False)
        def controls():
            calls.append(1)
            if len(calls) == 1:
                raise Incomplete('transient incomplete accessibility tree: missing child')
            return [Frame()]
        def focused():
            focus_calls.append(1)
            return {'focused': 44}
        scrots = []
        self.ns.update(controls=controls, focused=focused, fixture_files=lambda: [],
                       OUT=tmp, json=json,
                       pyatspi=SimpleNamespace(STATE_SELECTED=2, STATE_FOCUSED=3),
                       subprocess=SimpleNamespace(run=lambda *a, **k: scrots.append(a)))
        self.ns['snapshot']('folder-created')
        self.assertEqual(len(calls), 2)
        self.assertEqual(len(focus_calls), 3)
        payload = json.loads((tmp / 'nautilus-folder-created.json').read_text())
        self.assertTrue(any(node['role'] == 'frame' for node in payload['a11y']))
        self.assertEqual(len(scrots), 1)

    def test_snapshot_persistent_none_fails_clearly_after_bounded_attempts(self):
        import json
        import tempfile
        Incomplete = self.ns['IncompleteTree']
        tmp = Path(tempfile.mkdtemp())
        calls = []
        def controls():
            calls.append(1)
            raise Incomplete('transient incomplete accessibility tree: missing child')
        focus_calls = []
        def focused():
            focus_calls.append(1)
            return {'focused': 44}
        self.ns.update(controls=controls, focused=focused, fixture_files=lambda: [],
                       OUT=tmp, json=json,
                       pyatspi=SimpleNamespace(STATE_SELECTED=2, STATE_FOCUSED=3),
                       subprocess=SimpleNamespace(run=lambda *a, **k: None))
        with self.assertRaisesRegex(RuntimeError, 'transient incomplete accessibility tree'):
            self.ns['snapshot']('folder-created')
        self.assertEqual(len(calls), 3)
        self.assertEqual(len(focus_calls), 4)
        transport_calls = []
        def bad_controls():
            transport_calls.append(1)
            raise Error('transport broken')
        self.ns['controls'] = bad_controls
        with self.assertRaises(Error):
            self.ns['snapshot']('folder-created')
        self.assertEqual(len(transport_calls), 1)

    def test_select_reacquires_extents_before_single_input_never_replays(self):
        reads, inputs = [], []
        class Node:
            name = 'seed.txt'
            def getRoleName(self):
                return 'table cell'
            def queryComponent(self):
                def bounds(_):
                    reads.append(1)
                    if len(reads) == 1:
                        raise removed()
                    return SimpleNamespace(x=5, y=5, width=30, height=20)
                return SimpleNamespace(getExtents=bounds)
            def getState(self):
                return SimpleNamespace(contains=lambda _: True)
        self.ns.update(controls=lambda: [Node()], host='owned',
                       focused=lambda: {'focused_rect': [10, 20, 400, 300]},
                       run=lambda *args: inputs.append(args),
                       pyatspi=SimpleNamespace(WINDOW_COORDS=1, STATE_SELECTED=2))
        self.ns['select']('seed.txt')
        self.assertEqual(len(reads), 2)
        self.assertEqual(len(inputs), 1)
        self.assertEqual(inputs[0][-2:], ('click', '1'))


if __name__ == '__main__':
    unittest.main()
