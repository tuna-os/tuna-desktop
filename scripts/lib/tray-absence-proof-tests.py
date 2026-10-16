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
