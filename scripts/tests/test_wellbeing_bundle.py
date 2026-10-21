"""Focused proof admission negatives; fake bytes never count as native runtime evidence."""
import sys
# Admission must observe a clean source tree; imports must not create pyc
# files in this checkout (including before the admission check itself).
sys.dont_write_bytecode = True

import hashlib
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('wellbeing_bundle', ROOT / 'scripts/lib/wellbeing-bundle.py')
module = importlib.util.module_from_spec(spec)
spec.loader.exec_module(module)


class BundleAdmission(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name)
        self.root = self.base / 'root'
        self.root.mkdir()
        (self.root / 'Cargo.lock').write_text('fixture lock\n')
        subprocess.run(['git', 'init', '--quiet', str(self.root)], check=True)
        subprocess.run(['git', 'add', 'Cargo.lock'], cwd=self.root, check=True)
        subprocess.run(['git', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '--quiet', '-m', 'fixture'], cwd=self.root, check=True)
        self.revision = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=self.root, text=True).strip()
        self.frozen = self.base / 'frozen'
        self.frozen.mkdir()
        (self.frozen / 'Cargo.lock').write_bytes((self.root / 'Cargo.lock').read_bytes())
        (self.frozen / 'SOURCE_COMMIT').write_text(self.revision + '\n')
        self.messages = []
        self.sha = {}
        for name in ('tuna-compositor', 'tuna-shell-gtk', 'tuna-modal-dim-client', 'tuna-wellbeing-proof-shell'):
            path = self.base / name
            path.write_bytes(b'fake fixture bytes, not runtime: ' + name.encode())
            self.messages.append({'reason': 'compiler-artifact', 'target': {'name': name}, 'executable': str(path)})
            self.sha[name] = module.digest(path)
            if name != 'tuna-wellbeing-proof-shell':
                (self.frozen / name).write_bytes(path.read_bytes())
        self.manifest = {'source_commit': self.revision, 'cargo_lock_sha256': module.digest(self.frozen / 'Cargo.lock'), 'sha256': {k: v for k, v in self.sha.items() if k != 'tuna-wellbeing-proof-shell'}}
        (self.frozen / 'manifest.json').write_text(json.dumps(self.manifest))
        self.original_receipt = (self.frozen / 'manifest.json').read_bytes()
        self.build = self.base / 'build.json'
        self.dest = self.base / 'proof/bin'

    def admit(self):
        self.build.write_text('\n'.join(json.dumps(m) for m in self.messages))
        return module.admit(self.root, self.frozen, self.build, self.dest)

    def test_exact_members_admitted_original_receipt_immutable(self):
        result = self.admit()
        self.assertEqual(result['source_commit'], self.revision)
        self.assertEqual(result['sha256'], self.sha)
        self.assertEqual(result['parent_manifest_sha256'], hashlib.sha256(self.original_receipt).hexdigest())
        self.assertEqual((self.frozen / 'manifest.json').read_bytes(), self.original_receipt)
        self.assertFalse((self.frozen / 'tuna-wellbeing-proof-shell').exists())
        self.assertEqual(module.digest(self.dest / 'tuna-wellbeing-proof-shell'), self.sha['tuna-wellbeing-proof-shell'])
        with self.assertRaises(AssertionError):
            self.admit()

    def test_missing_or_ambiguous_example_rejected_before_creation(self):
        original = self.messages.copy()
        self.messages.pop()
        with self.assertRaises(AssertionError):
            self.admit()
        self.assertFalse(self.dest.exists())
        self.messages = original + [original[-1]]
        with self.assertRaises(AssertionError):
            self.admit()
        self.assertFalse(self.dest.exists())

    def test_mismatched_build_member_rejected(self):
        (self.base / 'tuna-compositor').write_bytes(b'stale source binary')
        with self.assertRaises(AssertionError):
            self.admit()
        self.assertFalse(self.dest.exists())

    def test_dirty_source_and_wrong_lock_rejected(self):
        (self.root / 'Cargo.lock').write_text('different lock')
        with self.assertRaises(AssertionError):
            self.admit()
        self.assertFalse(self.dest.exists())

    def test_wrong_revision_rejected(self):
        (self.frozen / 'SOURCE_COMMIT').write_text('0' * 40)
        with self.assertRaises(AssertionError):
            self.admit()
        self.assertFalse(self.dest.exists())

    def test_checkout_artifact_path_rejected(self):
        self.dest = self.root / 'artifacts/bin'
        with self.assertRaises(AssertionError):
            self.admit()
        self.assertFalse(self.dest.exists())

    def test_live_proof_invokes_pixels_and_real_capture(self):
        source = (ROOT / 'scripts/tuna-wellbeing-proof').read_text()
        self.assertIn("subprocess.run(['scrot'", source)
        self.assertIn("action('capture', filename=str(saved))", source)
        self.assertIn('PIXELS.verify(', source)
        self.assertIn('validate_saved_pixels(', source)
        self.assertIn("check_pair(neutral, capture('overview-affected')", source)
        self.assertIn("check_pair(tile_neutral, capture('tile-affected')", source)
        peer = (ROOT / 'crates/compositor/examples/tuna-wellbeing-proof-shell.rs').read_text()
        self.assertIn('assert_eq!(status, CommandStatus::Applied)', peer)
        self.assertIn('SetWellbeingEffects', peer)
        self.assertIn('"Screenshot"', peer)
        self.assertIn('libc::getppid()', peer)


class NativeMapBarrier(unittest.TestCase):
    def setUp(self):
        import runpy
        self.settle = runpy.run_path(str(ROOT / 'scripts/tuna-wellbeing-proof'))['settle_native_map']

    @staticmethod
    def state(window=1, opening=()):
        return {'windows': [] if window is None else [{'id': window}],
                'window_lifecycle': {'opening': [{'window': n} for n in opening]}}

    def test_model_admission_does_not_advance_clock_before_first_buffer(self):
        events = []
        def wait(check, label):
            events.append(label)
            if label == 'native-map':
                doc = self.state()
            elif label == 'native-map-started':
                self.assertNotIn('advance', events)
                self.assertFalse(check(self.state()))
                self.assertFalse(check(self.state(opening=(2,))))
                doc = self.state(opening=(1,))
            else:
                self.assertFalse(check(self.state(opening=(1,))))
                self.assertFalse(check(self.state(window=None)))
                self.assertFalse(check(self.state(window=2)))
                doc = self.state()
            self.assertTrue(check(doc))
            return doc
        def advance(delta):
            self.assertEqual(delta, 1000)
            events.append('advance')
        self.assertEqual(self.settle(wait, advance), self.state())
        self.assertEqual(events, ['native-map', 'native-map-started', 'advance', 'native-map-settled'])

    def test_missing_map_start_fails_without_advancing(self):
        def wait(check, label):
            if label == 'native-map':
                return self.state()
            self.assertFalse(check(self.state()))
            raise AssertionError('timed out: ' + label)
        def advance(_):
            self.fail('clock advanced before native map started')
        with self.assertRaisesRegex(AssertionError, 'native-map-started'):
            self.settle(wait, advance)


if __name__ == '__main__':
    unittest.main()
