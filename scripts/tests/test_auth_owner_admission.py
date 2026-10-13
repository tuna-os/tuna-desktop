#!/usr/bin/env python3
"""Actual CI admission code, synthetic file fixtures; no runtime qualification."""
import json
from pathlib import Path
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
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
        self.probe = self.out / 'probe-bin'
        self.probe.write_bytes(b'admitted-probe')
        self.probe_rows = [self.artifact('native-auth-owner-probe', 'example', self.probe, False)]
        (self.out / 'source.txt').write_text('candidate-sha\n')
        (self.out / 'root.txt').write_text(str(ROOT)+'\n')
        (self.out / 'metadata.json').write_text(json.dumps({'packages': [{
            'name': 'tuna-compositor', 'id': 'exact-compositor-package',
            'manifest_path': str(ROOT / 'crates/compositor/Cargo.toml')}]}))
        self.rows = [self.artifact('auth_owner_runtime', 'test', self.test, True),
                     self.artifact('tuna-compositor', 'bin', self.bin, False)]

    @staticmethod
    def artifact(name, kind, path, test):
        return {'reason': 'compiler-artifact', 'package_id': 'exact-compositor-package',
                'target': {'name': name, 'kind': [kind], 'src_path': str(ROOT / (
                    'crates/compositor/tests/auth_owner_runtime.rs' if name == 'auth_owner_runtime'
                    else 'crates/compositor/examples/native-auth-owner-probe.rs' if name == 'native-auth-owner-probe'
                    else 'crates/compositor/src/main.rs'))},
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
        (self.out / 'probe-build.json').write_text('\n'.join(map(json.dumps, self.probe_rows)))
        self.execute(BLOCKS[0])
        r = json.loads((self.out / 'binary-receipt.json').read_text())
        (self.out / 'runtime').mkdir(exist_ok=True)
        (self.out / 'runtime/success.json').write_text(json.dumps({
            'late_closed_origin_success_denied': True, 'fresh_origin_success': True}))
        (self.out / 'test.log').write_text('auth owner compositor sha256: ' + r['executables'][str(self.bin)] + '\n')
        (self.out/'runtime-epoch').mkdir(exist_ok=True)
        (self.out/'runtime-epoch/success.json').write_text(json.dumps({k:True for k in ['late_epoch_success_denied','busy_public_sync_denied','fresh_origin_success','public_sync_success']}))
        (self.out/'test-epoch.log').write_text('auth owner probe sha256: '+r['executables'][str(self.probe)]+'\n')
        (self.out/'runtime-pam').mkdir(exist_ok=True)
        (self.out/'runtime-pam/success.json').write_text(json.dumps({
            'closed_origin_canceled_retired': True, 'origin_loss_wakes_private_pam_without_fixture_release': True, 'fresh_origin_success': True,
            'held_self_path_replaced': True, 'compositor_sha256': r['executables'][str(self.bin)]}))
        (self.out/'test-pam.log').write_text('auth owner pam sha256: '+r['executables'][str(self.bin)]+'\n')
        return r

    def test_exact_roles_and_unchanged_files_admitted(self):
        r = self.admit()
        self.assertEqual(r['compositor'], str(self.bin))
        self.assertEqual(r['test'], str(self.test))
        self.execute(BLOCKS[1])

    def test_false_test_kind_profile_package_and_source_are_refused(self):
        import copy
        original = copy.deepcopy(self.rows)
        for target in [0, 1]:
            for field, wrong in [('kind', ['example']), ('profile', not original[target]['profile']['test']),
                                 ('package', 'wrong-package'), ('source', str(ROOT/'crates/greeter/src/main.rs'))]:
                self.rows = copy.deepcopy(original)
                if field == 'kind': self.rows[target]['target']['kind'] = wrong
                elif field == 'profile': self.rows[target]['profile']['test'] = wrong
                elif field == 'package': self.rows[target]['package_id'] = wrong
                else: self.rows[target]['target']['src_path'] = wrong
                with self.subTest(role=target, field=field), self.assertRaises(AssertionError): self.admit()

    def test_duplicate_identical_matching_artifact_is_refused(self):
        self.rows.append(dict(self.rows[0]))
        with self.assertRaises(AssertionError): self.admit()

    def test_missing_compositor_is_refused(self):
        self.rows.pop(1)
        with self.assertRaises(AssertionError): self.admit()

    def test_final_admission_reuses_exact_matcher(self):
        self.admit()
        rows = self.rows
        rows[0]['target']['src_path'] = str(ROOT/'crates/greeter/src/main.rs')
        (self.out/'build.json').write_text('\n'.join(map(json.dumps, rows))+'\n')
        with self.assertRaises(AssertionError): self.execute(BLOCKS[1])

    def test_probe_wrong_kind_package_profile_source_or_missing_refused(self):
        import copy
        original = copy.deepcopy(self.probe_rows)
        for field in ['kind', 'package_id', 'profile', 'src_path', 'missing', 'duplicate']:
            self.probe_rows = copy.deepcopy(original)
            if field == 'kind': self.probe_rows[0]['target']['kind'] = ['bin']
            elif field == 'package_id': self.probe_rows[0]['package_id'] = 'wrong'
            elif field == 'profile': self.probe_rows[0]['profile']['test'] = True
            elif field == 'src_path': self.probe_rows[0]['target']['src_path'] = str(ROOT/'crates/compositor/src/main.rs')
            elif field == 'missing': self.probe_rows = []
            else: self.probe_rows.append(copy.deepcopy(original[0]))
            with self.subTest(field=field), self.assertRaises(AssertionError): self.admit()

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


    def test_actual_pam_receipt_false_or_wrong_image_refused(self):
        for field, wrong in [('closed_origin_canceled_retired', False), ('origin_loss_wakes_private_pam_without_fixture_release', False), ('fresh_origin_success', False),
                             ('held_self_path_replaced', False), ('compositor_sha256', '0'*64)]:
            self.admit()
            path = self.out/'runtime-pam/success.json'
            receipt = json.loads(path.read_text()); receipt[field] = wrong
            path.write_text(json.dumps(receipt))
            with self.subTest(field=field), self.assertRaises(AssertionError): self.execute(BLOCKS[1])

    def test_actual_pam_receipt_missing_or_duplicate_source_refused(self):
        self.admit()
        (self.out/'runtime-pam/success.json').unlink()
        with self.assertRaises(FileNotFoundError): self.execute(BLOCKS[1])
        self.admit()
        path = self.out/'test-pam.log'; path.write_text(path.read_text()*2)
        with self.assertRaises(AssertionError): self.execute(BLOCKS[1])

class DisplayReadiness(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        import importlib.util
        spec = importlib.util.spec_from_file_location(
            'auth_display_readiness', ROOT / 'scripts/lib/auth-display-readiness.py')
        cls.module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(cls.module)

    def observe(self, rows, *, read_cost=0, live=True):
        now = [0.0]
        remaining = iter(rows)
        last = [b'']
        def read():
            now[0] += read_cost
            last[0] = next(remaining, last[0])
            return last[0]
        def pause(duration):
            now[0] += duration
        return self.module.wait_display(
            'unused-owned-file', 123, clock=lambda: now[0], pause=pause,
            alive=lambda pid: live, read=read, budget=5.0)

    def test_partial_token_waits_for_complete_newline(self):
        receipt = self.observe([b'', b'1', b'12', b'12\n'])
        self.assertTrue(receipt['admitted'])
        self.assertEqual(receipt['display'], '12')

    def test_complete_zero_is_valid(self):
        self.assertEqual(self.observe([b'0\n'])['display'], '0')

    def test_unterminated_and_empty_tokens_expire(self):
        for token in [b'', b'0']:
            with self.subTest(token=token):
                receipt = self.observe([token])
                self.assertFalse(receipt['admitted'])
                self.assertEqual(receipt['reason'], 'deadline')

    def test_invalid_complete_and_oversized_tokens_refused(self):
        for token in [b'\n', b'-1\n', b'0\n1\n', b'x\n', b'1' * 65]:
            with self.subTest(token=token):
                self.assertFalse(self.observe([token])['admitted'])

    def test_read_crossing_deadline_cannot_admit(self):
        receipt = self.observe([b'0\n'], read_cost=5)
        self.assertFalse(receipt['admitted'])
        self.assertEqual(receipt['reason'], 'deadline')

    def test_dead_owned_process_cannot_admit(self):
        receipt = self.observe([b'0\n'], live=False)
        self.assertFalse(receipt['admitted'])
        self.assertEqual(receipt['reason'], 'xvfb_exited')

    def test_process_dying_after_read_cannot_admit(self):
        calls = iter([True, False])
        receipt = self.module.wait_display(
            'unused', 123, clock=lambda: 0, pause=lambda duration: None,
            alive=lambda pid: next(calls), read=lambda: b'0\n')
        self.assertFalse(receipt['admitted'])

    def test_actual_pretest_error_trap_records_phase_line_status(self):
        import subprocess
        start = SCRIPT.index('pretest_failure() {')
        end = SCRIPT.index('git rev-parse HEAD', start)
        with tempfile.TemporaryDirectory() as directory:
            owned = Path(directory)
            script = ('set -Eeuo pipefail\n'
                      + 'out="$1"; pretest_phase=display_admission\n'
                      + SCRIPT[start:end] + 'false\n')
            result = subprocess.run(['bash', '-c', script, 'owned-test', str(owned)],
                                    capture_output=True, text=True)
            self.assertEqual(result.returncode, 1)
            receipt = json.loads((owned / 'pretest-failure.json').read_text())
            self.assertEqual(receipt['phase'], 'display_admission')
            self.assertEqual(receipt['status'], 1)
            self.assertIs(type(receipt['line']), int)
            self.assertGreater(receipt['line'], 0)

    def test_real_file_read_closes_stream(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'display'
            path.write_bytes(b'3\n')
            receipt = self.module.wait_display(path, 123, alive=lambda pid: True)
            self.assertEqual(receipt['display'], '3')

if __name__ == '__main__':
    unittest.main()
