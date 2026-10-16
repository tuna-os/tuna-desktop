#!/usr/bin/env python3
"""Workspace menu selection rejects arbitrary/replaced/X11 focus."""
import copy
import sys
import unittest

sys.dont_write_bytecode = True
from tuna_window_menu_fixture import owner, target


class Fixture(unittest.TestCase):
    def setUp(self):
        self.process = {'pid': 100, 'uid': 1000, 'start_ticks': 123}
        self.scene = {'windows': [{'app_id': 'tuna-test-alpha', 'id': 1, 'application_window_id': 1,
                                  'x11_window_id': None, 'rect': [320,206,640,420], 'workspace': 0},
                                 {'app_id': 'XEyes', 'id': 7, 'application_window_id': 7,
                                  'x11_window_id': 6291466, 'rect': [565,366,150,100], 'workspace': 0}],
                      'focused': 1, 'focused_app_id': 'tuna-test-alpha', 'focused_rect': [320,206,640,420],
                      'active_workspace': 0, 'overview_open': False, 'overview_progress': 0}
        self.expected = owner(self.scene, self.process)

    def test_record_rejects_ambiguous_or_x11_alpha(self):
        scene = copy.deepcopy(self.scene)
        scene['windows'][0]['x11_window_id'] = 10
        with self.assertRaisesRegex(RuntimeError, 'unique native'):
            owner(scene, self.process)
        scene = copy.deepcopy(self.scene)
        scene['windows'].append(copy.deepcopy(scene['windows'][0]))
        with self.assertRaisesRegex(RuntimeError, 'unique native'):
            owner(scene, self.process)

    def test_original_native_alpha_accepted(self):
        self.assertEqual(target(self.scene, self.expected, self.process), (1,0,380,226))

    def test_observed_closed_but_unsettled_alpha_refused_until_zero_progress(self):
        # Exact18bf installed snapshot: original process/native IDs, focus,
        # rect and workspace all matched; close was still104.127371ms in.
        process = {'pid': 38552, 'uid': 1001, 'start_ticks': 161862}
        expected = owner(self.scene, process)
        self.scene.update(overview_open=False, overview_progress=0.3404611822619463)
        with self.assertRaisesRegex(RuntimeError, 'not focused and mapped'):
            target(self.scene, expected, process)
        self.scene['overview_progress'] = 0
        self.assertEqual(target(self.scene, expected, process), (1, 0, 380, 226))

    def test_observed_xeyes_focus_rejected(self):
        self.scene.update(focused=7, focused_app_id='XEyes', focused_rect=[565,366,150,100])
        with self.assertRaisesRegex(RuntimeError, 'not focused'):
            target(self.scene, self.expected, self.process)

    def test_reused_pid_and_replaced_native_ids_rejected(self):
        for key in ('pid', 'uid', 'start_ticks'):
            live = {**self.process, key: self.process[key]+1}
            with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, 'process lifetime'):
                target(self.scene, self.expected, live)
        for key in ('id', 'application_window_id', 'x11_window_id'):
            scene = copy.deepcopy(self.scene); scene['windows'][0][key] = 99
            with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, 'native identity changed'):
                target(scene, self.expected, self.process)

    def test_wrong_workspace_geometry_overview_or_ambiguous_alpha_rejected(self):
        for key, value in [('active_workspace',1), ('focused_rect',[0,0,800,600]),
                           ('overview_open',True), ('overview_progress',0.5)]:
            scene = {**self.scene, key:value}
            with self.subTest(key=key), self.assertRaisesRegex(RuntimeError, 'not focused'):
                target(scene, self.expected, self.process)
        self.scene['windows'].append(copy.deepcopy(self.scene['windows'][0]))
        with self.assertRaisesRegex(RuntimeError, 'ambiguous'):
            target(self.scene, self.expected, self.process)


if __name__ == '__main__':
    unittest.main()
