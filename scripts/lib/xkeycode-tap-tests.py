#!/usr/bin/python3
"""Bounded XTEST driver tests; no display, compositor or shared keymap is touched."""
import importlib.util
from pathlib import Path
from types import SimpleNamespace
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('tap', Path(__file__).with_name('xkeycode-tap.py'))
tap = importlib.util.module_from_spec(spec)
spec.loader.exec_module(tap)


class Function:
    def __init__(self, callback):
        self.callback = callback

    def __call__(self, *args):
        return self.callback(*args)


class PhysicalTap(unittest.TestCase):
    def run_driver(self, args, mapping=None, reject=None):
        events, queries, closes = [], [], []

        def lookup(_display, code, level):
            queries.append((code, level))
            if mapping is None:
                raise AssertionError('physical mode queried a keysym')
            return mapping.get(code, 0)

        def event(_display, code, pressed, delay):
            events.append((code, pressed, delay))
            return int(pressed != reject)

        x11 = SimpleNamespace(XOpenDisplay=Function(lambda _: 1),
                              XCloseDisplay=Function(lambda _: closes.append(1)),
                              XKeycodeToKeysym=Function(lookup),
                              XSync=Function(lambda *_: None))
        xtst = SimpleNamespace(XTestFakeKeyEvent=Function(event))
        with patch.object(tap.ctypes, 'CDLL', side_effect=[x11, xtst]), \
                patch.object(tap.sys, 'argv', ['tap', *args]), \
                patch.object(tap.time, 'sleep', lambda _: None):
            result = tap.main()
        self.assertEqual(closes, [1])
        return result, events, queries

    def test_physical_right_alt_sends_only_requested_press_and_release(self):
        result, events, queries = self.run_driver([':96', '--keycode', '108'])
        self.assertEqual(result, 0)
        self.assertEqual(events, [(108, 1, 0), (108, 0, 0)])
        self.assertEqual(queries, [])
        self.assertNotIn(64, [event[0] for event in events])

    def test_physical_bounds_and_non_numeric_arguments_are_rejected(self):
        for value in ['7', '256', '-1', '108;echo bad', '0x6c', '１０８', '']:
            with self.subTest(value=value), self.assertRaises(ValueError):
                tap.parse_request([':96', '--keycode', value])
        for value in ['8', '255']:
            self.assertEqual(tap.parse_request([':96', '--keycode', value])[2], int(value))

    def test_existing_keysym_mode_uses_only_level_zero(self):
        result, events, queries = self.run_driver([':96', 'ffbe'], {67: 0xffbe})
        self.assertEqual(result, 0)
        self.assertEqual(events, [(67, 1, 0), (67, 0, 0)])
        self.assertTrue(all(level == 0 for _, level in queries))

    def test_missing_level_zero_keysym_does_not_inject(self):
        result, events, _ = self.run_driver([':96', 'ffbe'], {})
        self.assertEqual(result, 1)
        self.assertEqual(events, [])

    def test_press_failure_is_not_a_pass(self):
        result, events, _ = self.run_driver([':96', '--keycode', '108'], reject=1)
        self.assertEqual(result, 1)
        self.assertEqual(events, [(108, 1, 0)])

    def test_release_failure_is_not_a_pass(self):
        result, events, _ = self.run_driver([':96', '--keycode', '108'], reject=0)
        self.assertEqual(result, 1)
        self.assertEqual(events, [(108, 1, 0), (108, 0, 0)])


if __name__ == '__main__':
    unittest.main()
