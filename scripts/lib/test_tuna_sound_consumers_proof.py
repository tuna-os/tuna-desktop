import hashlib
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

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

if __name__ == '__main__':
    unittest.main()
