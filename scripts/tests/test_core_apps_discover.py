"""Discover fallback regressions: a mapped surface whose client never owned the
well-known bus name must bind through the compositor-attested live peer;
departed peers and missing windows must stay not-ready, never raise.
Helper-validator controls only; no application or native CI qualification.
"""
import copy
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import unittest
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'packaging/marlin/vm-lane/tuna-native-session'))
spec = importlib.util.spec_from_file_location(
    "core_apps_discover_guest", ROOT / "packaging/marlin/vm-lane/core-apps/guest.py")
guest = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guest)

APP_ID = 'org.gnome.Calculator'
PACKAGE = 'gnome-calculator'
WINDOW_ID = 8


class FakeContext:
    def __init__(self, bus, state):
        self.uid = os.getuid()
        self.owner = SimpleNamespace(pw_uid=self.uid)
        self._bus = bus
        self._state = state
        self.actor_calls = []
        inventory = {'apps': {APP_ID: {'package': PACKAGE, 'version': '51.1-1',
                                       'desktop_sha256': '0' * 64}}}
        self.controls = {'inventory': inventory}

    def actor(self, arguments, operation='tree'):
        self.actor_calls.append((dict(arguments), operation))
        if operation == 'discover':
            return copy.deepcopy(self._bus)
        actor = arguments['actor']
        assert actor['pid'] == self._state['native_keyboard_windows'][0]['client_pid'], \
            'tree must bind the attested peer PID'
        return {'a11y_bus': ':1.7',
                'nodes': [{'role': 'frame', 'showing': True, 'active': True,
                           'object_path': '/org/app/frame'}]}

    def state(self):
        return copy.deepcopy(self._state)

    def command(self, argv, **kwargs):
        assert argv[:2] == ['pacman', '-Qoq'], 'only exe ownership lookup expected'
        return SimpleNamespace(stdout=PACKAGE + '\n')

    def read_control(self, name):
        return copy.deepcopy(self.controls[name])


def live_state(pid):
    return {'locked': False, 'focused': WINDOW_ID,
            'windows': [{'id': WINDOW_ID, 'app_id': APP_ID}],
            'native_keyboard_windows': [{'window_id': WINDOW_ID, 'client_pid': pid,
                                         'client_uid': os.getuid()}]}


class DiscoverBinding(unittest.TestCase):
    def peer(self):
        # A same-user process behind a root-owned installed executable, which
        # is what the guest identity check admits as the application peer.
        peer = subprocess.Popen(['/usr/bin/sleep', '300'])
        self.addCleanup(peer.wait)
        self.addCleanup(peer.kill)
        return peer.pid

    def test_bus_owned_binds_bus_name(self):
        pid = self.peer()
        context = FakeContext({'pid': pid, 'uid': os.getuid(),
                               'session_bus': ':1.9'}, live_state(pid))
        actor = guest.desktop('discover', {'app_id': APP_ID, 'window_id': WINDOW_ID},
                              context)
        self.assertEqual(actor['pid'], pid)
        self.assertEqual(actor['session_bus'], ':1.9')
        self.assertEqual(actor['binding'], 'well-known-bus-name')
        self.assertEqual(actor['window_id'], WINDOW_ID)
        self.assertEqual(actor['a11y_bus'], ':1.7')

    def test_nameless_surface_binds_attested_peer(self):
        pid = self.peer()
        context = FakeContext({'ready': False, 'reason': 'application-bus-not-owned'},
                              live_state(pid))
        actor = guest.desktop('discover', {'app_id': APP_ID, 'window_id': WINDOW_ID},
                              context)
        self.assertEqual(actor['pid'], pid)
        self.assertIsNone(actor['session_bus'])
        self.assertEqual(actor['binding'], 'compositor-attested-client-pid')
        self.assertEqual(actor['executable_package'], PACKAGE)
        self.assertEqual(actor['window_id'], WINDOW_ID)
        self.assertEqual(actor['a11y_bus'], ':1.7')

    def test_departed_peer_stays_not_ready(self):
        context = FakeContext({'ready': False, 'reason': 'application-bus-not-owned'},
                              live_state(2 ** 22))
        value = guest.desktop('discover', {'app_id': APP_ID, 'window_id': WINDOW_ID},
                              context)
        self.assertFalse(value.get('ready'))
        self.assertIsNone(value.get('pid'))
        self.assertEqual(value['surface']['peer'], 'departed')

    def test_no_window_id_stays_not_ready(self):
        context = FakeContext({'ready': False, 'reason': 'application-bus-not-owned'},
                              live_state(os.getpid()))
        value = guest.desktop('discover', {'app_id': APP_ID}, context)
        self.assertFalse(value.get('ready'))
        self.assertNotIn('surface', value)


if __name__ == '__main__':
    unittest.main()
