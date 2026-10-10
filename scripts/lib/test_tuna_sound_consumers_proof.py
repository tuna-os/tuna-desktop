import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch
from contextlib import redirect_stderr
import io

spec = importlib.util.spec_from_file_location('sound_proof', Path(__file__).with_name('tuna-sound-consumers-proof.py'))
proof = importlib.util.module_from_spec(spec)
spec.loader.exec_module(proof)

class FrozenSoundIdentityTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.volume = self.root / 'volume'
        self.bell = self.root / 'bell'
        self.volume.write_bytes(b'volume-client')
        self.bell.write_bytes(b'bell-client')
        self.identity = self.root / 'identity.json'
        self.identity.write_text(json.dumps({'source_revision': 'revision', 'binary_sha256': {
            p.name: hashlib.sha256(p.read_bytes()).hexdigest() for p in (self.volume, self.bell)}}))
    def validate(self, **kwargs):
        return proof.validate_identity(self.identity, kwargs.get('revision', 'revision'),
                                       kwargs.get('volume', self.volume), self.bell,
                                       kwargs.get('delivered', False))
    def test_matched_source_and_bytes(self):
        self.assertEqual(len(self.validate()[1]), 2)
    def test_wrong_source_rejected(self):
        with self.assertRaisesRegex(AssertionError, 'source identity'):
            self.validate(revision='other')
    def test_changed_bytes_rejected(self):
        self.volume.write_bytes(b'other')
        with self.assertRaisesRegex(AssertionError, 'client byte'):
            self.validate()
    def test_client_outside_bundle_rejected(self):
        outside = self.root / 'outside'
        outside.mkdir()
        copy = outside / self.volume.name
        copy.write_bytes(self.volume.read_bytes())
        with self.assertRaisesRegex(AssertionError, 'outside frozen'):
            self.validate(volume=copy)
    def test_delivered_requires_core_binaries(self):
        with self.assertRaises(FileNotFoundError):
            self.validate(delivered=True)

class PipeWireGraphTests(unittest.TestCase):
    def test_consecutive_arrays_replace_complete_node_state(self):
        events = '[{"id": 7, "info": {"state": "idle"}}]\n[{"id": 7, "info": {"state": "running"}}]'
        self.assertEqual(proof.pipewire_graph(events), [{'id': 7, 'info': {'state': 'running'}}])

    def test_removal_and_unrelated_nodes(self):
        events = '[{"id": 7}, {"id": 8}] [{"id": 7, "info": null}]'
        self.assertEqual(proof.pipewire_graph(events), [{'id': 8}])

    def test_whitespace_and_empty_batches(self):
        self.assertEqual(proof.pipewire_graph(' \n[]\n[] '), [])

    def test_malformed_or_incomplete_json_rejected(self):
        for events in ('[] broken', '[{"id": 7}', '[{"id": 7}] ['):
            with self.subTest(events=events), self.assertRaises(json.JSONDecodeError):
                proof.pipewire_graph(events)

    def test_invalid_batch_and_identity_rejected(self):
        for events in ('{}', '[null]', '[{"id": true}]', '[{"id": "7"}]'):
            with self.subTest(events=events), self.assertRaises(AssertionError):
                proof.pipewire_graph(events)


class ProofOptionsTests(unittest.TestCase):
    def reject(self, extra):
        argv = ['sound-proof', '--volume-client', 'unused', '--bell-client', 'unused',
                '--artifacts', 'unused', '--source-revision', 'unused', '--identity', 'unused'] + extra
        with patch('sys.argv', argv), redirect_stderr(io.StringIO()), self.assertRaises(SystemExit) as raised:
            proof.main()
        self.assertEqual(raised.exception.code, 2)

    def test_overlap_requires_delivery_and_player_diagnostics(self):
        for flags in ([], ['--delivered'], ['--diagnose-player']):
            with self.subTest(flags=flags):
                self.reject(['--owned-overlap-only'] + flags)

    def test_capture_latency_is_bounded_before_starting_services(self):
        for latency in ('-1', '0', '8193'):
            with self.subTest(latency=latency):
                self.reject(['--recorder-latency-samples', latency])


if __name__ == '__main__':
    unittest.main()
