#!/usr/bin/env python3
"""Row logic of gsd-media-keys-bus.py; no bus, gsd or compositor needed."""
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('gsd_bus', HERE / 'gsd-media-keys-bus.py')
bus = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bus)

GSD, SHELL, OTHER = ':1.40', ':1.7', ':1.9'


def call(serial, member, body, sender=GSD, ns=10):
    return {'ns': ns, 'type': 'call', 'serial': serial, 'reply_serial': 0, 'sender': sender,
            'destination': SHELL, 'interface': 'org.gnome.Shell', 'member': member, 'body': body}


def reply(serial, body, to=GSD, kind='return'):
    return {'ns': 11, 'type': kind, 'serial': 900 + serial, 'reply_serial': serial,
            'sender': SHELL, 'destination': to, 'interface': None, 'member': None, 'body': body}


def activated(action, ns):
    return {'ns': ns, 'type': 'signal', 'sender': SHELL, 'interface': 'org.gnome.Shell',
            'member': 'AcceleratorActivated', 'body': [action, {'action-mode': 1}]}


def grab_rows():
    return [
        call(5, 'GrabAccelerators', [[['XF86AudioMicMute', 15, 0], ['XF86AudioPlay', 15, 0],
                                      ['XF86MonBrightnessUp', 15, 0], ['XF86WWW', 1, 0]]]),
        reply(5, [[3, 4, 0, 6]]),
    ]


class GrabTests(unittest.TestCase):
    def test_nonzero_replies_are_live_grabs(self):
        self.assertEqual(bus.live_grabs(grab_rows(), GSD),
                         {'XF86AudioMicMute': 3, 'XF86AudioPlay': 4, 'XF86WWW': 6})

    def test_other_callers_and_unanswered_calls_do_not_count(self):
        rows = grab_rows() + [call(7, 'GrabAccelerators', [[['XF86AudioMute', 1, 0]]])]
        self.assertNotIn('XF86AudioMute', bus.live_grabs(rows, GSD))
        self.assertEqual(bus.live_grabs(grab_rows(), OTHER), {})
        # A reply to another connection's identical serial is not GSD's.
        rows = [grab_rows()[0], reply(5, [[3, 4, 0, 6]], to=OTHER)]
        self.assertEqual(bus.live_grabs(rows, GSD), {})

    def test_error_reply_is_no_grab(self):
        rows = [grab_rows()[0], reply(5, [], kind='error')]
        self.assertEqual(bus.live_grabs(rows, GSD), {})

    def test_ungrab_releases(self):
        rows = grab_rows() + [call(8, 'UngrabAccelerators', [[3, 6]]), reply(8, [True])]
        self.assertEqual(bus.live_grabs(rows, GSD), {'XF86AudioPlay': 4})
        rows = grab_rows() + [call(9, 'UngrabAccelerator', [4])]
        self.assertNotIn('XF86AudioPlay', bus.live_grabs(rows, GSD))

    def test_single_grab(self):
        rows = [call(3, 'GrabAccelerator', ['XF86AudioNext', 1, 0]), reply(3, [12])]
        self.assertEqual(bus.live_grabs(rows, GSD), {'XF86AudioNext': 12})


class SignalTests(unittest.TestCase):
    def test_osd_icons_since_from_owner_only(self):
        rows = [
            call(20, 'ShowOSD', [{'icon': 'audio-volume-high-symbolic'}], ns=5),
            call(21, 'ShowOSD', [{'icon': 'microphone-sensitivity-muted-symbolic', 'level': 0.0}], ns=50),
            call(22, 'ShowOSD', [{'icon': 'microphone-sensitivity-high-symbolic'}], sender=OTHER, ns=60),
        ]
        self.assertEqual(bus.osd_icons(rows, GSD, 40), ['microphone-sensitivity-muted-symbolic'])
        self.assertEqual(bus.osd_icons(rows, GSD, 0),
                         ['audio-volume-high-symbolic', 'microphone-sensitivity-muted-symbolic'])

    def test_activations_count_one_action_after_since(self):
        rows = [activated(3, 5), activated(3, 50), activated(4, 51), activated(3, 52)]
        self.assertEqual(bus.activations(rows, 3, 40), 2)
        self.assertEqual(bus.activations(rows, 4, 0), 1)
        self.assertEqual(bus.activations(rows, 6, 0), 0)


class FileTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.dir = Path(self.temp.name)

    def test_torn_jsonl_line_is_skipped(self):
        path = self.dir / 'bus.jsonl'
        path.write_text(json.dumps(grab_rows()[0]) + '\n' + json.dumps(grab_rows()[1]) + '\n{"ns": 1')
        self.assertEqual(len(bus.load_rows(path)), 2)
        self.assertEqual(bus.load_rows(self.dir / 'missing.jsonl'), [])

    def test_cli_grabs_and_osd(self):
        path = self.dir / 'bus.jsonl'
        rows = grab_rows() + [call(30, 'ShowOSD', [{'icon': 'microphone-sensitivity-muted-symbolic'}], ns=70)]
        path.write_text(''.join(json.dumps(r) + '\n' for r in rows))
        self.assertEqual(bus.main(['grabs', str(path), GSD, 'XF86AudioMicMute', 'XF86WWW']), 0)
        self.assertEqual(bus.main(['grabs', str(path), GSD, 'XF86MonBrightnessUp', '--timeout', '0']), 1)
        self.assertEqual(bus.main(['osd', str(path), GSD, '60', 'microphone-sensitivity-muted-symbolic']), 0)
        self.assertEqual(bus.main(['osd', str(path), GSD, '80', 'microphone-sensitivity-muted-symbolic']), 1)

    def nested_log(self, offset_text, rows):
        path = self.dir / 'nested.log'
        lines = [offset_text]
        for row in rows:
            lines.append(bus.DIAGNOSTIC + json.dumps(row))
            lines.append('tuna-shell-gtk: unrelated line')
        path.write_text('\n'.join(lines) + '\n')
        return path, len(offset_text.encode()) + 1

    def test_diagnostics_after_offset(self):
        before = {'actor': 'shell', 'stage': 'media-action', 'details': {'action': 'VolumeUp'}}
        path = self.dir / 'nested.log'
        text = bus.DIAGNOSTIC + json.dumps(before) + '\n'
        path.write_text(text + bus.DIAGNOSTIC + json.dumps(before) + '\n')
        self.assertEqual(bus.native(bus.diagnostics(path, 0)), 2)
        self.assertEqual(bus.native(bus.diagnostics(path, len(text.encode()))), 1)

    def test_forwarded_fired_native(self):
        rows = [
            {'actor': 'shell', 'stage': 'accelerators-send',
             'details': {'write_ok': True, 'media': [[3, 269025202, 0, 15], [1073741825, 269025043, 0, 15]]}},
            {'actor': 'compositor', 'stage': 'media-accelerator-fired',
             'details': {'action': 3, 'keysym': 269025202, 'mods': 0, 'mode': 1}},
            {'actor': 'shell', 'stage': 'accelerators-send',
             'details': {'write_ok': False, 'media': [[7, 269025043, 0, 15]]}},
        ]
        path, offset = self.nested_log('tuna-compositor: earlier', rows)
        diag = bus.diagnostics(path, offset)
        self.assertTrue(bus.forwarded(diag, 3, 0x1008ffb2))
        self.assertFalse(bus.forwarded(diag, 3, 0x1008ff13))
        self.assertFalse(bus.forwarded(diag, 7, 0x1008ff13))
        self.assertEqual(bus.fired(diag, 3), 1)
        self.assertEqual(bus.fired(diag, 4), 0)
        self.assertEqual(bus.native(diag), 0)
        self.assertEqual(bus.main(['forwarded', str(path), str(offset), '3', str(0x1008ffb2)]), 0)
        self.assertEqual(bus.main(['forwarded', str(path), str(offset), '9', str(0x1008ffb2)]), 1)


if __name__ == '__main__':
    unittest.main()
