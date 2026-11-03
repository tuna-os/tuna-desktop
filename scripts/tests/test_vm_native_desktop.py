"""Native desktop evidence retention; no VM or installed-session qualification."""
import json
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
from vm_native_desktop import NativeDesktop


class FailureMessage(unittest.TestCase):
    def desktop(self, out):
        metadata = {'schema': 1, 'installed_signed_release_qualified': False,
                    'commit': 'a' * 40, 'package_sha256': 'b' * 64,
                    'image_id': 'c' * 64, 'base_image': 'example@sha256:' + 'd' * 64,
                    'member_sha256': {}}
        return NativeDesktop(qmp=None, agent_factory=None, out=out, metadata=metadata,
                             action='autotz', source_members={})

    def test_failed_guest_record_carries_bounded_message(self):
        with tempfile.TemporaryDirectory() as tmp:
            desktop = self.desktop(tmp)
            agent = mock.Mock()
            agent.run.side_effect = RuntimeError('guest lifecycle autotz failed (exit=1)')
            agent.guest_exec_state = 'none'
            agent.pending_guest_pid = None
            desktop._agent_factory = lambda deadline: agent
            with self.assertRaises(RuntimeError):
                desktop.guest('autotz', 'watch-start')
            retained = json.loads(Path(tmp, '00001-failed-guest-autotz-watch-start.json').read_text())
            self.assertEqual(retained['error_type'], 'RuntimeError')
            self.assertEqual(retained['message'], 'guest lifecycle autotz failed (exit=1)')
            self.assertTrue(retained['not_replayed'])

    def test_failed_message_is_bounded_not_dropped(self):
        with tempfile.TemporaryDirectory() as tmp:
            desktop = self.desktop(tmp)
            agent = mock.Mock()
            agent.run.side_effect = RuntimeError('y' * 5000)
            agent.guest_exec_state = 'none'
            agent.pending_guest_pid = None
            desktop._agent_factory = lambda deadline: agent
            with self.assertRaises(RuntimeError):
                desktop.guest('autotz', 'snapshot')
            retained = json.loads(Path(tmp, '00001-failed-guest-autotz-snapshot.json').read_text())
            self.assertEqual(retained['message'], 'y' * 2000)


if __name__ == '__main__':
    unittest.main()
