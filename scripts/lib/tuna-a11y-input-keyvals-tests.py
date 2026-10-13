#!/usr/bin/python3
"""Focused proof-recorder regression; no display or GTK application is started."""
import ast
from pathlib import Path
import unittest

import gi

gi.require_version('Gdk', '4.0')
from gi.repository import Gdk


class RecorderKeyvals(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Execute only recorder callbacks: importing the fixture would start its
        # application. Use actual GDK conversion, not a guessed keysym table.
        source = Path(__file__).with_name('tuna-a11y-input-window.py')
        tree = ast.parse(source.read_text())
        callbacks = [node for node in tree.body if isinstance(node, ast.FunctionDef)
                     and node.name in {'printable_keyval', 'pressed'}]
        cls.namespace = {'Gdk': Gdk, 'save': lambda: None}
        exec(compile(ast.Module(body=callbacks, type_ignores=[]), str(source), 'exec'),
             cls.namespace)

    def setUp(self):
        self.namespace['state'] = {'presses': []}

    def record(self, *keyvals):
        for keyval in keyvals:
            self.namespace['pressed'](None, keyval, 108, Gdk.ModifierType(0))
        return self.namespace['state']['presses']

    def test_plain_altgr_retains_modifier_but_counts_only_q(self):
        events = self.record(Gdk.KEY_ISO_Level3_Shift, Gdk.KEY_q)
        self.assertEqual([event['keyval'] for event in events], [65027, 113])
        self.assertEqual([event['keyval'] for event in events if event['printable']], [113])
        self.assertFalse(events[-1]['shift'])
        self.assertFalse(events[-1]['ctrl'])

    def test_level3_latch_counts_at_then_q(self):
        events = self.record(Gdk.KEY_ISO_Level3_Shift, Gdk.KEY_at, Gdk.KEY_q)
        self.assertEqual([event['keyval'] for event in events if event['printable']], [64, 113])

    def test_wrong_character_is_not_filtered_into_a_pass(self):
        events = self.record(Gdk.KEY_ISO_Level3_Shift, Gdk.KEY_at)
        self.assertNotEqual([event['keyval'] for event in events if event['printable']], [113])

    def test_modifiers_and_control_characters_are_not_printable(self):
        events = self.record(Gdk.KEY_Shift_L, Gdk.KEY_Control_L, Gdk.KEY_Alt_R,
                             Gdk.KEY_ISO_Level5_Shift, Gdk.KEY_Return, Gdk.KEY_Tab)
        self.assertFalse(any(event['printable'] for event in events))

    def test_alt_state_is_retained_for_opposite_modifier_control(self):
        self.namespace['pressed'](None, Gdk.KEY_a, 38, Gdk.ModifierType.ALT_MASK)
        event = self.namespace['state']['presses'][-1]
        self.assertTrue(event['printable'])
        self.assertTrue(event['alt'])
        self.assertFalse(event['ctrl'])
        self.assertFalse(event['shift'])

    def test_unicode_characters_above_old_cutoff_are_retained(self):
        unicode_euro = 0x010020ac
        self.assertGreater(unicode_euro, 65280)
        events = self.record(unicode_euro)
        self.assertTrue(events[0]['printable'])


if __name__ == '__main__':
    unittest.main()
