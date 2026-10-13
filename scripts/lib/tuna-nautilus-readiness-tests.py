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
        names = {'removed_accessible', 'wait', 'controls', 'cell', 'select'}
        functions = [node for node in ast.parse(SOURCE.read_text()).body
                     if isinstance(node, ast.FunctionDef) and node.name in names]
        self.assertEqual({node.name for node in functions}, names)
        exec(compile(ast.Module(body=functions, type_ignores=[]), str(SOURCE), 'exec'), self.ns)

    def wait(self, predicate):
        return self.ns['wait'](predicate, 'retain strict actual contents')

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
