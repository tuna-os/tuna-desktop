#!/usr/bin/env python3
"""Actual CI admission code, synthetic file fixtures; no runtime qualification."""
import json
from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = (ROOT / 'scripts/tuna-auth-owner-ci-proof').read_text()
BLOCKS = [part.split('\nPY\n', 1)[0] for part in SCRIPT.split("<<'PY'\n")[1:]]


class Admission(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.out = Path(self.temp.name)
        self.test = self.out / 'test-bin'
        self.bin = self.out / 'compositor-bin'
        self.test.write_bytes(b'admitted-test')
        self.bin.write_bytes(b'admitted-compositor')
        (self.out / 'source.txt').write_text('candidate-sha\n')
        self.rows = [self.artifact('auth_owner_runtime', 'test', self.test, True),
                     self.artifact('tuna-compositor', 'bin', self.bin, False)]

    @staticmethod
    def artifact(name, kind, path, test):
        return {'reason': 'compiler-artifact', 'target': {'name': name, 'kind': [kind]},
                'profile': {'test': test}, 'executable': str(path)}

    def execute(self, block):
        before = sys.argv
        try:
            sys.argv = ['proof-admission', str(self.out)]
            exec(compile(block, '<actual-auth-admission>', 'exec'), {})
        finally:
            sys.argv = before

    def admit(self):
        (self.out / 'build.json').write_text('\n'.join(map(json.dumps, self.rows)) + '\n')
        self.execute(BLOCKS[0])
        r = json.loads((self.out / 'binary-receipt.json').read_text())
        (self.out / 'runtime').mkdir(exist_ok=True)
        (self.out / 'runtime/success.json').write_text(json.dumps({
            'late_closed_origin_success_denied': True, 'fresh_origin_success': True}))
        (self.out / 'test.log').write_text('auth owner compositor sha256: ' + r['executables'][str(self.bin)] + '\n')
        return r

    def test_exact_roles_and_unchanged_files_admitted(self):
        r = self.admit()
        self.assertEqual(r['compositor'], str(self.bin))
        self.assertEqual(r['test'], str(self.test))
        self.execute(BLOCKS[1])

    def test_missing_test_is_refused(self):
        self.rows.pop(0)
        with self.assertRaises(AssertionError): self.admit()

    def test_two_distinct_compositor_paths_refused(self):
        self.rows.append(self.artifact('tuna-compositor', 'bin', self.out / 'other', False))
        with self.assertRaises(AssertionError): self.admit()

    def test_test_profile_binary_cannot_replace_normal_compositor(self):
        self.rows[1]['profile']['test'] = True
        with self.assertRaises(AssertionError): self.admit()

    def test_binary_mutation_after_admission_refused(self):
        self.admit()
        self.bin.write_bytes(b'changed')
        with self.assertRaises(AssertionError): self.execute(BLOCKS[1])

    def test_actual_compositor_hash_cannot_be_test_hash(self):
        r = self.admit()
        (self.out / 'test.log').write_text('auth owner compositor sha256: ' + r['executables'][str(self.test)] + '\n')
        with self.assertRaises(AssertionError): self.execute(BLOCKS[1])

    def test_runtime_requirement_and_negative_receipt_remain_strict(self):
        self.admit()
        (self.out / 'runtime/success.json').write_text(json.dumps({
            'late_closed_origin_success_denied': False, 'fresh_origin_success': True}))
        with self.assertRaises(AssertionError): self.execute(BLOCKS[1])
        self.assertIn('TUNA_REQUIRE_AUTH_OWNER_RUNTIME=1', SCRIPT)
        self.assertIn('timeout --kill-after=3s 45s', SCRIPT)
        self.assertIn('1 passed; 0 failed; 0 ignored;', SCRIPT)


if __name__ == '__main__':
    unittest.main()
