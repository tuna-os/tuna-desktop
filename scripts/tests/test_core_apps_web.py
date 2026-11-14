"""unique() control-matching regressions: a container proxy and its leaf widget
may share one name and role, which must resolve to the actionable leaf;
genuinely distinct duplicates must still fail. Helper-validator controls only.
"""
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts' / 'lib'))
from core_apps_web import unique


def node(name, role, path, showing=True):
    return {'name': name, 'role': role, 'path': list(path), 'showing': showing,
            'sensitive': True}


DIALOG = {'name': 'Preferences', 'role': 'dialog', 'path': [0], 'showing': True,
          'sensitive': True, 'bounds': [0, 0, 400, 300]}


class NestedProxyResolution(unittest.TestCase):
    def tree(self, controls):
        return {'nodes': [DIALOG] + controls}

    def test_nested_proxy_resolves_to_leaf(self):
        row = node('Group Digits', 'switch', [0, 1])
        widget = node('Group Digits', 'switch', [0, 1, 0])
        found = unique(self.tree([row, widget]),
                       lambda n: n.get('name') == 'Group Digits' and n.get('role') == 'switch',
                       'preference switch')
        self.assertEqual(found['path'], [0, 1, 0])

    def test_distinct_duplicates_still_fail(self):
        first = node('Group Digits', 'switch', [0, 1])
        second = node('Group Digits', 'switch', [0, 2])
        with self.assertRaises(RuntimeError):
            unique(self.tree([first, second]),
                   lambda n: n.get('name') == 'Group Digits' and n.get('role') == 'switch',
                   'preference switch')

    def test_absent_control_still_fails(self):
        with self.assertRaises(RuntimeError):
            unique(self.tree([]), lambda n: n.get('role') == 'switch', 'preference switch')

    def test_single_match_unchanged(self):
        only = node('Group Digits', 'switch', [0, 1, 0])
        found = unique(self.tree([only]),
                       lambda n: n.get('name') == 'Group Digits' and n.get('role') == 'switch',
                       'preference switch')
        self.assertEqual(found['path'], [0, 1, 0])

    def test_outermost_prefers_proxy_row(self):
        row = node('Zoom to Specific Level', 'button', [0, 2, 2])
        widget = node('Zoom to Specific Level', 'toggle button', [0, 2, 2, 0])
        found = unique(self.tree([row, widget]),
                       lambda n: n.get('name') == 'Zoom to Specific Level',
                       'zoom control', outermost=True)
        self.assertEqual(found['path'], [0, 2, 2])

    def test_press_targets_proxy_row(self):
        from core_apps_web import press
        row = node('Zoom to Specific Level', 'button', [0, 2, 2])
        widget = node('Zoom to Specific Level', 'toggle button', [0, 2, 2, 0])
        calls = {}

        class Desktop:
            def tree(self, actor):
                return {'nodes': [row, widget]}
            def press(self, actor, name, role=None):
                calls['press'] = (name, role)
        press(Desktop(), {'id': 'org.gnome.Loupe'}, 'Zoom to Specific Level')
        self.assertEqual(calls['press'], ('Zoom to Specific Level', 'button'))


if __name__ == '__main__':
    unittest.main()
