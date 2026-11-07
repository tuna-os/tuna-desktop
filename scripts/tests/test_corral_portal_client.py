#!/usr/bin/env python3
"""Bookkeeping batching preserves native request identity and withdrawal bounds."""
import base64
import hashlib
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
from types import SimpleNamespace
import tempfile
import unittest
from unittest.mock import Mock, patch

SOURCE = Path(__file__).resolve().parents[1] / 'lib/tuna-portal-journey-client.py'
loader = importlib.machinery.SourceFileLoader('portal_journey_client', str(SOURCE))
spec = importlib.util.spec_from_loader(loader.name, loader)
client = importlib.util.module_from_spec(spec)
loader.exec_module(client)


def state(phase, locked=False):
    return {'phase': phase, 'at': 'observed', 'exit': 0,
            'stdout': '(true,)' if locked else '(false,)', 'stderr': ''}


class NativeCoordinatorTests(unittest.TestCase):
    def test_locked_launch_never_starts_a_native_requester(self):
        with patch.object(client, 'session_state', return_value=state('before-request', True)), \
                patch.object(client.subprocess, 'Popen') as spawn:
            report = client.launch({'kind': 'screencast'})
        self.assertIn('requires an unlocked session', report['error'])
        self.assertFalse(report['ready'])
        spawn.assert_not_called()

    def test_launch_bundles_ready_status_native_argv_and_exact_source_hashes(self):
        for kind, script, mode, marker in (
                ('screenshot', 'tuna-portal-screenshot-client.py', 'allow', 'frame.waiting'),
                ('screencast', 'tuna-portal-capture-client.py', 'revoke', 'frame.ready')):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as tmp:
                data = b'native source bytes unchanged'
                values = {'kind': kind, 'directory': 'owned',
                          'sources': {script: base64.b64encode(data).decode()}}
                root = Path(tmp) / 'owned'
                def ready(paths, seconds):
                    self.assertEqual(seconds, 45 if kind == 'screenshot' else 200)
                    (root / marker).write_text(mode)
                    return True
                with patch.dict(os.environ, XDG_RUNTIME_DIR=tmp), \
                        patch.object(client, 'session_state', side_effect=state), \
                        patch.object(client, 'process_start', return_value='own-start'), \
                        patch.object(client, 'wait_for_files', side_effect=ready), \
                        patch.object(client.subprocess, 'Popen', return_value=SimpleNamespace(pid=123)) as spawn:
                    report = client.launch(values)
                self.assertTrue(report['ready'])
                self.assertFalse(report['finished'])
                self.assertEqual(report['argv'][1:], [str(root / script), str(root / 'frame.png'), mode])
                self.assertEqual((root / script).read_bytes(), data)
                self.assertEqual(report['source_sha256'][script], hashlib.sha256(data).hexdigest())
                self.assertTrue(spawn.call_args.kwargs['start_new_session'])
                self.assertEqual(json.loads((root / 'worker.json').read_text())['start'], 'own-start')

    def test_exited_requester_prevents_any_picker_subprocess(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'requester.exit').write_text('1')
            (root / 'requester.log').write_text('native Start timed out')
            with patch.object(client.subprocess, 'run') as action:
                report = client.consent({'kind': 'screencast', 'root': tmp})
            action.assert_not_called()
            self.assertEqual(report['requester_exit'], '1')
            self.assertEqual(report['log'], 'native Start timed out')
            self.assertIn('no picker action attempted', report['error'])

    def test_share_active_state_and_observer_arm_are_one_guest_operation(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            def share(argv, **kwargs):
                self.assertEqual(kwargs['timeout'], 60)
                self.assertEqual(argv[argv.index('--kind') + 1], 'screencast')
                self.assertEqual(argv[argv.index('--request-exit') + 1], str(root / 'requester.exit'))
                (root / 'frame.active').write_text('58')
                (root / 'frame.response.json').write_text(json.dumps({'response': 0, 'results': {'streams': [[58, {}]]}}))
                (root / 'consent.json').write_text('[{"name":"Share"}]')
                return SimpleNamespace(returncode=0, stdout='actual portal picker: Share', stderr='')
            with patch.object(client.subprocess, 'run', side_effect=share), \
                    patch.object(client, 'wait_for_files', return_value=True) as wait, \
                    patch.object(client, 'session_state', side_effect=state):
                report = client.consent({'kind': 'screencast', 'root': tmp})
            self.assertTrue(report['active'])
            self.assertTrue(report['observer_armed'])
            self.assertTrue((root / 'frame.revoke').exists())
            self.assertNotIn('frame', report, 'PNG transfer must wait until after the host lock key')
            self.assertEqual(report['response']['results']['streams'][0][0], 58)
            self.assertEqual(report['input']['accessible_tree'], [{'name': 'Share'}])
            wait.assert_called_once()
            self.assertEqual(wait.call_args.args[1], 100)

    def test_lock_between_share_and_active_check_never_arms_a_grant(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'frame.active').write_text('58')
            with patch.object(client.subprocess, 'run', return_value=SimpleNamespace(
                    returncode=0, stdout='actual portal picker: Share', stderr='')), \
                    patch.object(client, 'wait_for_files', return_value=True), \
                    patch.object(client, 'session_state', return_value=state('after-response', True)), \
                    patch.object(client, 'stop_requester', return_value=True) as retire:
                report = client.consent({'kind': 'screencast', 'root': tmp})
            self.assertFalse(report['observer_armed'])
            self.assertFalse((root / 'frame.revoke').exists())
            self.assertIn('unlocked session', report['error'])
            retire.assert_called_once_with(root)

    def test_wrong_or_failed_picker_retires_only_its_requester_and_keeps_diagnostics(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / 'requester.log').write_text('native requester is still waiting')
            with patch.object(client.subprocess, 'run', return_value=SimpleNamespace(
                    returncode=1, stdout='helper diagnostics', stderr='matching Share missing')), \
                    patch.object(client, 'stop_requester', return_value=True) as retire:
                report = client.consent({'kind': 'screencast', 'root': tmp})
            self.assertEqual(report['input']['exit'], 1)
            self.assertEqual(report['input']['stderr'], 'matching Share missing')
            self.assertEqual(report['log'], 'native requester is still waiting')
            self.assertTrue(report['requester_terminated'])
            retire.assert_called_once_with(root)
            self.assertFalse((root / 'frame.revoke').exists())

    def test_collection_preserves_exact_native_image_response_and_revocation(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            data = b'actual native frame transport bytes'
            (root / 'frame.png').write_bytes(data)
            (root / 'requester.exit').write_text('0')
            (root / 'requester.log').write_text('lock removed node in 0.19s')
            (root / 'frame.response.json').write_text('{"response":0,"results":{}}')
            (root / 'frame.revoked.json').write_text('[]')
            with patch.object(client, 'wait_for_files', return_value=True):
                report = client.collect({'root': tmp})
            self.assertEqual(report['requester_exit'], '0')
            self.assertEqual(base64.b64decode(report['frame']['base64']), data)
            self.assertEqual(report['frame']['sha256'], hashlib.sha256(data).hexdigest())
            self.assertEqual(report['revoked'], [])
            self.assertEqual(report['response'], {'response': 0, 'results': {}})

    def test_cleanup_does_not_signal_a_reused_or_unpinned_pid(self):
        for initial, current in (('old', 'new'), (None, None)):
            with self.subTest(initial=initial), tempfile.TemporaryDirectory() as tmp:
                root = Path(tmp)
                (root / 'worker.json').write_text(json.dumps({'pid': 123, 'start': initial}))
                with patch.object(client, 'process_start', return_value=current), \
                        patch.object(client.os, 'killpg') as kill:
                    self.assertFalse(client.stop_requester(root))
                kill.assert_not_called()


if __name__ == '__main__':
    unittest.main()
