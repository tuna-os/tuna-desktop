#!/usr/bin/python3
"""Finite native action/unit security boundaries, NOT installed/native proof."""
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'scripts/lib'))


def module(name, path):
    loader = importlib.machinery.SourceFileLoader(name, str(path))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    result = importlib.util.module_from_spec(spec)
    loader.exec_module(result)
    return result


guest = module('explicit_sync_guest_tests', ROOT / 'scripts/lib/tuna-explicit-sync-guest.py')
host = module('explicit_sync_host_tests', ROOT / 'scripts/lib/explicit_sync_vm.py')
NONCE = 'a' * 32
ATTEMPT = 'b' * 32


class FiniteActionSchema(unittest.TestCase):
    def parse(self, action, **value):
        return guest.request(action, json.dumps(dict(nonce=NONCE, **value)))

    def test_exact_operation_and_keys(self):
        self.assertEqual(self.parse('state'), {'nonce': NONCE})
        for action in ('run', 'stop', 'exec', '../state'):
            with self.subTest(action=action), self.assertRaises(ValueError):
                self.parse(action)
        with self.assertRaises(ValueError):
            self.parse('state', argv=['/bin/sh'])
        with self.assertRaises(ValueError):
            guest.request('state', '{"nonce":"' + NONCE + '","nonce":"' + NONCE + '"}')
        with self.assertRaises(ValueError):
            guest.request('state', ' ' * 4097)

    def test_no_path_or_principal_injection(self):
        for value in ('../../tmp/owner', '', '/' + NONCE):
            with self.subTest(value=value), self.assertRaises(ValueError):
                guest.require_nonce(value)
        for case in ('../../etc', 'ordering/../../etc', 'unknown'):
            with self.subTest(case=case), self.assertRaises(ValueError):
                self.parse('start', case=case)
        for uid in (0, True, -1, '1000'):
            with self.subTest(uid=uid), self.assertRaises(ValueError):
                self.parse('init', pid=123, start_ticks=12, uid=uid, sha256='c' * 64)
        for name, offset in (('../../etc/shadow', 0), ('stderr.log', -1),
                             ('stderr.log', True), ('stderr.log', 1048577)):
            with self.subTest(name=name, offset=offset), self.assertRaises(ValueError):
                self.parse('file', case='ordering', name=name, offset=offset)

    def test_legacy_producer_is_a_real_named_artifact(self):
        value = self.parse('file', case='legacy-v3', name='producer-legacy-v3-red.ppm', offset=0)
        self.assertEqual(value['name'], 'producer-legacy-v3-red.ppm')

    def test_capture_acknowledgements_do_not_cross_consumer_boundaries(self):
        for case, name in (('ordering', 'on'), ('background-effect', 'red-presented'),
                           ('disconnect', 'on')):
            with self.subTest(case=case, name=name), self.assertRaises(ValueError):
                self.parse('ack', case=case, name=name, attempt=ATTEMPT, png_sha256='c' * 64)

    def test_completed_or_active_case_cannot_be_started_again(self):
        state = {'active': {'case': 'ordering'}, 'cases': {}, 'deadline': time.monotonic() + 10}
        with self.assertRaises(RuntimeError):
            guest.start(Path('/unused'), state, 'ordering')
        state.update(active=None, cases={'ordering': {'finished': True}})
        with self.assertRaises(RuntimeError):
            guest.start(Path('/unused'), state, 'ordering')
        with self.assertRaises(RuntimeError):
            guest.start(Path('/unused'), state, 'disconnect')


class FilesystemBoundaries(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name) / 'proof'
        self.root.mkdir(mode=0o711)
        self.directory = self.root / NONCE
        self.directory.mkdir(mode=0o711)
        self.root_patch = patch.object(guest, 'ROOT', self.root)
        self.root_patch.start()
        self.addCleanup(self.root_patch.stop)

    def state(self):
        return {'active': {'case': 'ordering', 'attempt': ATTEMPT, 'deadline': time.monotonic() + 30},
                'checkpoint': {'case': 'ordering', 'attempt': ATTEMPT, 'name': 'red-presented'},
                'cases': {'ordering': {'finished': False, 'client_reaped': False}}}

    def dispatch(self, operation, value):
        # Unit tests may run unprivileged. Only the root-UID check is bypassed;
        # actual flock, atomic publication and filesystem semantics are exercised.
        with patch.object(guest, 'secure_directory'), patch.object(guest, 'check_owner'):
            return guest.dispatch(operation, value)

    def test_ack_is_attempt_owned_atomic_and_never_overwritten(self):
        guest.save(self.directory / 'state.json', self.state())
        value = guest.request('ack', json.dumps({'nonce': NONCE, 'case': 'ordering', 'attempt': ATTEMPT,
                                               'name': 'red-presented', 'png_sha256': 'c' * 64}))
        stale = dict(value, attempt='d' * 32)
        with self.assertRaises(RuntimeError):
            self.dispatch('ack', stale)
        self.dispatch('ack', value)
        path = self.directory / (ATTEMPT + '-red-presented.ack')
        self.assertEqual(json.loads(path.read_text()), value)
        with self.assertRaises(FileExistsError):
            self.dispatch('ack', dict(value, png_sha256='e' * 64))
        self.assertEqual(json.loads(path.read_text()), value)
        background_state = self.state()
        background_state['active']['case'] = 'background-effect'
        background_state['checkpoint'].update(case='background-effect', name='null-committed')
        guest.save(self.directory / 'state.json', background_state)
        background_ack = guest.request('ack', json.dumps(dict(
            value, case='background-effect', name='null-committed')))
        with self.assertRaises(RuntimeError):
            self.dispatch('ack', value)
        self.dispatch('ack', background_ack)
        background_path = self.directory / (ATTEMPT + '-null-committed.ack')
        self.assertEqual(json.loads(background_path.read_text()), background_ack)
        self.assertFalse(path.with_suffix('.pending').exists())

    def test_artifact_symlinks_and_nonregular_files_are_rejected(self):
        target = self.directory / 'ordering'
        target.mkdir()
        source = Path(self.temporary.name) / 'private'
        source.write_bytes(b'not an artifact')
        (target / 'stderr.log').symlink_to(source)
        value = {'case': 'ordering', 'name': 'stderr.log', 'offset': 0}
        with self.assertRaises(OSError):
            guest.artifact(self.directory, value)
        (target / 'stderr.log').unlink()
        os.mkfifo(target / 'stderr.log')
        with self.assertRaises(RuntimeError):
            guest.artifact(self.directory, value)
        (target / 'stderr.log').unlink()
        target.rmdir()
        target.symlink_to(source.parent, target_is_directory=True)
        with self.assertRaises(OSError):
            guest.artifact(self.directory, value)

    def test_root_directory_rejects_broad_permissions(self):
        os.chmod(self.root, 0o777)
        with self.assertRaises(RuntimeError):
            guest.secure_directory(self.root, 0o711)

    def test_cleanup_kills_only_owned_worker_and_requires_client_reap_proof(self):
        owned = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'])
        unrelated = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'])
        self.addCleanup(unrelated.wait)
        self.addCleanup(lambda: unrelated.poll() is None and unrelated.kill())
        self.addCleanup(owned.wait)
        self.addCleanup(lambda: owned.poll() is None and owned.kill())
        state = self.state()
        state['active']['worker'] = guest.process(owned.pid)
        guest.save(self.directory / 'state.json', state)
        with patch.object(guest, 'secure_directory'):
            with self.assertRaisesRegex(RuntimeError, 'reap proof absent'):
                guest.retire(self.directory, state, True)
        self.assertEqual(owned.wait(timeout=2), -signal.SIGTERM)
        self.assertIsNone(unrelated.poll())
        self.assertIsNotNone(guest.load(self.directory)['active'])

    def test_worker_identity_mismatch_never_signals_a_process(self):
        owned = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'])
        try:
            state = self.state()
            identity = guest.process(owned.pid)
            identity['start_ticks'] += 1
            state['active']['worker'] = identity
            with self.assertRaisesRegex(RuntimeError, 'identity changed'):
                guest.retire(self.directory, state, True)
            self.assertIsNone(owned.poll())
        finally:
            owned.kill()
            owned.wait()


class HostTransportBoundaries(unittest.TestCase):
    def exercise(self, uncertain):
        with tempfile.TemporaryDirectory() as directory:
            calls = []
            acquisitions = []
            class Agent:
                guest_exec_state = 'none'
                closed = False

                def run(self, action, operation, raw, *, timeout):
                    calls.append((action, operation, json.loads(raw), timeout))
                    if operation == 'init':
                        self.guest_exec_state = 'unknown' if uncertain else 'none'
                        raise TimeoutError('test finite request failure')
                    return {'active': None, 'cases': {}}

                def close(self):
                    self.closed = True

            agents = []
            def factory(*, deadline):
                acquisitions.append(deadline)
                agent = Agent()
                agents.append(agent)
                return agent

            class Vm:
                def poll(self):
                    return None

            image = {'schema': 1, 'commit': 'a' * 40, 'image_id': 'sha256:' + 'b' * 64,
                     'package_sha256': 'c' * 64,
                     'member_sha256': {'/usr/bin/tuna-compositor': 'd' * 64}}
            inventory = {'session_uid': 1000, 'processes': [
                {'pid': 123, 'uid': 1000, 'start_ticks': 456, 'executable': 'tuna-compositor'}]}
            before = time.monotonic()
            with self.assertRaises((RuntimeError, TimeoutError)):
                host.run(None, factory, directory, image, inventory, Vm())
            receipt = json.loads((Path(directory) / 'explicit-sync/receipt.json').read_text())
            self.assertFalse(receipt['complete'])
            self.assertEqual(receipt['image_admission'], image)
            self.assertTrue(all(agent.closed for agent in agents))
            self.assertTrue(all(0 < call[3] <= 7 for call in calls))
            self.assertTrue(all(before < end <= time.monotonic() + 7 for end in acquisitions))
            self.assertTrue(all(call[0] == 'explicit-sync' for call in calls))
            self.assertEqual(set(calls[0][2]), {'nonce', 'pid', 'uid', 'start_ticks', 'sha256'})
            return calls, receipt

    def test_unknown_action_departure_never_opens_a_cleanup_stream(self):
        calls, receipt = self.exercise(True)
        self.assertEqual([call[1] for call in calls], ['init'])
        self.assertIn('departure unconfirmed', receipt['cleanup_failure'])

    def test_known_terminal_failure_still_runs_finite_cleanup(self):
        calls, receipt = self.exercise(False)
        self.assertEqual([call[1] for call in calls], ['init', 'cleanup'])
        self.assertNotIn('cleanup_failure', receipt)


if __name__ == '__main__':
    unittest.main()
