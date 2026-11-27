#!/usr/bin/env python3
"""Functional negatives execute the production tray absence predicate."""
import importlib.util
import json
from pathlib import Path
import tempfile
import sys
from types import SimpleNamespace
import unittest
from unittest.mock import patch

SPEC = importlib.util.spec_from_file_location('tray_absence', Path(__file__).with_name('tuna_tray_absence.py'))
M = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(M)
OWNER = {'pid': 7, 'start_ticks': '10', 'boot_id': 'fixture-boot', 'sha256': 'f' * 64}
TREE = [{'role': 'application', 'name': 'tuna-shell-gtk', 'process_id': 7},
        {'role': 'toggle button', 'name': 'Activities', 'showing': True}]

class Absence(unittest.TestCase):
    def observe(self, tree=TREE, status=0, write=True, identities=None):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory) / 'tree.json'
            output.write_text(json.dumps(TREE))  # stale success must be discarded
            def dump():
                self.assertFalse(output.exists(), 'stale output survived before dump')
                if write:
                    output.write_text(tree if isinstance(tree, str) else json.dumps(tree))
                return status
            with patch.object(M, 'process_identity', side_effect=identities or [OWNER, OWNER]):
                return M.observe_absence(output, OWNER, dump)

    def test_positive_fresh_complete_absence(self):
        self.assertTrue(self.observe())

    def test_live_row_is_not_absent(self):
        self.assertFalse(self.observe(TREE + [{'name': 'Tuna Desktop Tray Probe', 'showing': True}]))

    def test_failed_dump_cannot_qualify_stale_or_fresh_output(self):
        for write in (False, True):
            with self.subTest(write=write), self.assertRaisesRegex(ValueError, 'dump failed'):
                self.observe(status=1, write=write)

    def test_missing_output_cannot_qualify_stale_output(self):
        with self.assertRaises(FileNotFoundError):
            self.observe(write=False)

    def test_invalid_empty_partial_or_wrong_owner_tree_refused(self):
        for tree in ['invalid', [], {}, TREE + [{'role': 'error'}],
                     [dict(TREE[0], process_id=8), TREE[1]], [TREE[0]],
                     [dict(TREE[0], name='different-process'), TREE[1]]]:
            with self.subTest(tree=tree), self.assertRaises(ValueError):
                self.observe(tree=tree)

    def test_replaced_process_before_or_during_dump_refused(self):
        changed = dict(OWNER, start_ticks='11')
        for identities in [[changed], [OWNER, changed]]:
            with self.subTest(identities=identities), self.assertRaisesRegex(ValueError, 'lifetime changed'):
                self.observe(identities=identities)

# The tree the grid-cancel comparison's G-TRAY refused (run 44793641609):
# the killed item still showing while the walk met its unexported child.
RACE = 'atspi_error: No such interface “org.a11y.atspi.Accessible” on object at path /org/tuna/Shell/a11y/88cef3b2 (1)'
ITEM_TREE = [dict(TREE[0], depth=0), dict(TREE[1], depth=3),
             {'role': 'push button', 'name': 'Tuna Desktop Tray Probe', 'depth': 5, 'showing': True}]

class Teardown(unittest.TestCase):
    observe = Absence.observe

    def test_unexported_child_of_departing_item_is_a_retry(self):
        with self.assertRaisesRegex(M.TearingDown, 'No such interface'):
            self.observe(ITEM_TREE + [{'role': 'error', 'name': RACE, 'depth': 6, 'showing': False}])

    def test_other_errors_stay_refusals(self):
        trees = {
            'outside the item': ITEM_TREE[:2] + [{'role': 'error', 'name': RACE, 'depth': 4, 'showing': False}] + ITEM_TREE[2:],
            'after the item subtree': ITEM_TREE + [{'role': 'error', 'name': RACE, 'depth': 5, 'showing': False}],
            'another kind inside the item': ITEM_TREE + [{'role': 'error', 'name': 'childCount drifted 2 -> 1 at depth 6', 'depth': 6}],
            'no depth': ITEM_TREE + [{'role': 'error', 'name': RACE}],
        }
        for case, tree in trees.items():
            with self.subTest(case), self.assertRaisesRegex(ValueError, 'partial accessibility tree'):
                self.observe(tree)

    def test_race_still_requires_owner_and_panel_anchor(self):
        tree = [dict(ITEM_TREE[0], process_id=8)] + ITEM_TREE[1:] + [
            {'role': 'error', 'name': RACE, 'depth': 6, 'showing': False}]
        with self.assertRaisesRegex(ValueError, 'admitted shell identity'):
            self.observe(tree)

class Deadline(unittest.TestCase):
    def run_await(self, outcomes):
        calls, sleeps = [], []
        def observe():
            outcome = outcomes[min(len(calls), len(outcomes) - 1)]
            calls.append(outcome)
            if isinstance(outcome, Exception):
                raise outcome
            return outcome
        return M.await_absence(observe, sleep=sleeps.append), calls, sleeps

    def test_deadline_is_unchanged(self):
        self.assertEqual((M.ATTEMPTS, M.INTERVAL_S), (30, 0.5))

    def test_teardown_then_absence_passes(self):
        reason, calls, sleeps = self.run_await([M.TearingDown(RACE), False, True])
        self.assertIsNone(reason)
        self.assertEqual((len(calls), sleeps), (3, [0.5, 0.5]))

    def test_persistent_teardown_fails_at_the_deadline(self):
        reason, calls, sleeps = self.run_await([M.TearingDown(RACE)])
        self.assertRegex(reason, 'still being torn down: .*No such interface')
        self.assertEqual((len(calls), sleeps), (30, [0.5] * 30))

    def test_item_still_showing_fails_at_the_deadline(self):
        reason, calls, _ = self.run_await([False])
        self.assertEqual((reason, len(calls)), ('the item is still showing', 30))

    def test_refusal_is_not_retried(self):
        calls = []
        def observe():
            calls.append(1)
            raise ValueError('missing, empty or partial accessibility tree')
        with self.assertRaises(ValueError):
            M.await_absence(observe, sleep=lambda _: None)
        self.assertEqual(len(calls), 1)

    def test_exit_codes(self):
        with tempfile.TemporaryDirectory() as directory:
            receipt = Path(directory) / 'receipt.json'
            receipt.write_text(json.dumps({'complete': True, 'publisher': OWNER}))
            argv = ['tuna_tray_absence.py', str(receipt), str(Path(directory) / 'tree.json'), 'dump.py']
            for outcome, code in ((True, 0), (M.TearingDown(RACE), 2)):
                def observe(*_, outcome=outcome):
                    if isinstance(outcome, Exception):
                        raise outcome
                    return outcome
                with self.subTest(outcome=outcome), patch.object(sys, 'argv', argv), \
                        patch.object(M, 'observe_absence', side_effect=observe), \
                        patch.object(M.time, 'sleep'), patch('sys.stderr'):
                    self.assertEqual(M.main(), code)

class OwnerBoundDump(unittest.TestCase):
    def run_dump(self, include_owner):
        class App:
            def __init__(self, pid):
                self.pid, self.name, self.description = pid, 'tuna-shell-gtk', ''
                self.childCount = 0
            def get_process_id(self): return self.pid
            def getRoleName(self): return 'application'
            def getState(self): return SimpleNamespace(contains=lambda flag: True)
        apps = [App(8)] + ([App(7)] if include_owner else [])
        desktop = SimpleNamespace(childCount=len(apps), getChildAtIndex=lambda index: apps[index])
        spi = SimpleNamespace(Registry=SimpleNamespace(getDesktop=lambda index: desktop),
                              STATE_SHOWING=1, STATE_CHECKED=2, STATE_PRESSED=3,
                              STATE_SELECTED=4, STATE_FOCUSED=5, WINDOW_COORDS=0)
        helper = Path(__file__).with_name('tuna-a11y-dump.py')
        spec = importlib.util.spec_from_file_location('owned_dump_fixture', helper)
        with tempfile.TemporaryDirectory() as directory, patch.dict(sys.modules, pyatspi=spi):
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            output = Path(directory) / 'tree.json'
            with patch.object(sys, 'argv', [str(helper), 'tuna-shell-gtk', str(output), '0.01', '--pid', '7']):
                status = module.main()
            return status, json.loads(output.read_text()) if output.exists() else None

    def test_same_name_wrong_pid_is_skipped_before_owner(self):
        status, nodes = self.run_dump(True)
        self.assertEqual(status, 0)
        self.assertEqual(nodes[0]['process_id'], 7)

    def test_same_name_only_wrong_pid_cannot_create_tree(self):
        status, nodes = self.run_dump(False)
        self.assertEqual(status, 1)
        self.assertIsNone(nodes)

if __name__ == '__main__':
    unittest.main()
