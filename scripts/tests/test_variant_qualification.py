#!/usr/bin/env python3
"""Regression coverage for immutable refs and fail-closed matrix inventory."""
import base64
import contextlib
import importlib.machinery
import importlib.util
import io
import json
from pathlib import Path
import tempfile
import sys
import unittest
from unittest.mock import Mock, patch

SCRIPT = Path(__file__).resolve().parents[1] / 'tuna-variant-qualification'
loader = importlib.machinery.SourceFileLoader('variant_qualification', str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
module = importlib.util.module_from_spec(spec)
loader.exec_module(module)

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_corral_identity import FakeCorral


class CommandCancellationTests(unittest.TestCase):
    def test_timeout_retires_entire_journey_group_before_cleanup_can_run(self):
        process = Mock(pid=123)
        process.communicate.side_effect = [
            module.subprocess.TimeoutExpired(['journey'], 5), ('full stdout', 'full stderr')]
        with patch.object(module.subprocess, 'Popen', return_value=process) as spawn, \
                patch.object(module.os, 'killpg') as kill:
            with self.assertRaises(module.subprocess.TimeoutExpired) as caught:
                module.command(['journey'], timeout=5)
        self.assertTrue(spawn.call_args.kwargs['start_new_session'])
        kill.assert_called_once_with(123, module.signal.SIGKILL)
        self.assertEqual(caught.exception.stdout, 'full stdout')
        self.assertEqual(caught.exception.stderr, 'full stderr')


class MatrixTests(unittest.TestCase):
    def inventory(self, config, registry_exit=0):
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / 'evidence'
            source = {'content': base64.b64encode(config.encode()).decode()}
            def github(path):
                return {'sha': 'a' * 40} if path.endswith('commits/main') else source
            digest = 'sha256:' + 'b' * 64
            result = {'exit': registry_exit, 'stdout': json.dumps({'Digest': digest}), 'stderr': 'registry unavailable' if registry_exit else ''}
            with patch.object(module, 'github', side_effect=github), patch.object(module, 'command', return_value=result) as command, patch('sys.argv', [str(SCRIPT), '--out', str(out)]), contextlib.redirect_stdout(io.StringIO()):
                code = module.main()
            return code, json.loads((out / 'matrix.json').read_text()), command.call_args_list

    def test_alias_publication_is_pinned_without_changing_internal_variant(self):
        code, report, calls = self.inventory('''
[variants.flounder-sid]
description = "Debian Sid"
publish_name = "flounder"
tag_suffix = "sid"
[[variants.flounder-sid.flavors]]
id = "tuna"
build_image = true
platforms = ["linux/amd64"]
[tuna_rollout.flounder-sid]
status = "admitted"
package_target = "debian"
''')
        row = report['variants'][0]
        self.assertEqual(code, 0)
        self.assertEqual(row['variant'], 'flounder-sid')
        self.assertEqual(row['published_tag'], 'ghcr.io/tuna-os/flounder:tuna-sid')
        self.assertEqual(row['image'], 'ghcr.io/tuna-os/flounder@sha256:' + 'b' * 64)
        self.assertEqual(row['qualification'], 'not-qualified')
        self.assertEqual(row['runtime'], 'not-run')
        self.assertIn('--no-tags', calls[0].args[0])
        self.assertIn('lib/corral_identity.py', report['harness_sha256'])

    def test_registry_failure_is_not_an_unsupported_waiver(self):
        code, report, _ = self.inventory('''
[variants.marlin]
description = "Arch"
[[variants.marlin.flavors]]
id = "tuna"
build_image = true
[tuna_rollout.marlin]
status = "admitted"
package_target = "arch"
''', registry_exit=1)
        self.assertEqual(code, 1)
        self.assertEqual(report['variants'][0]['qualification'], 'not-qualified')
        self.assertEqual(report['variants'][0]['runtime'], 'blocked-registry')

    def test_unsupported_requires_upstream_reason(self):
        config = '''
[variants.guppy]
description = "Gentoo"
flavors = []
[tuna_rollout.guppy]
status = "unsupported"
package_target = "gentoo"
'''
        with self.assertRaisesRegex(RuntimeError, 'inconsistent unsupported declaration'):
            self.inventory(config)
        code, report, calls = self.inventory(config + 'reason = "upstream package target unavailable"\n')
        self.assertEqual(code, 0)
        self.assertEqual(report['variants'][0]['qualification'], 'source-declared-unsupported')
        self.assertEqual(calls, [])

    def test_missing_rollout_cannot_drop_a_variant(self):
        with self.assertRaisesRegex(RuntimeError, 'inventory and Tuna rollout keys differ'):
            self.inventory('[variants.marlin]\ndescription = "Arch"\nflavors = []\n[tuna_rollout]\n')

    def test_unproven_cleanup_stops_before_second_vm(self):
        config = ''.join(f'''
[variants.{name}]
description = "test image"
[[variants.{name}.flavors]]
id = "tuna"
build_image = true
[tuna_rollout.{name}]
status = "admitted"
package_target = "el10"
''' for name in ('yellowfin', 'albacore'))
        source = {'content': base64.b64encode(config.encode()).decode()}
        journeys = []
        def github(path):
            return {'sha': 'a' * 40} if path.endswith('commits/main') else source
        def command(argv, timeout=120):
            if argv[0] == 'skopeo':
                return {'exit': 0, 'stdout': json.dumps({'Digest': 'sha256:' + 'b' * 64}), 'stderr': ''}
            journeys.append(argv)
            directory = Path(argv[argv.index('--out') + 1])
            directory.mkdir()
            (directory / 'manifest.json').write_text(json.dumps({
                'steps': [], 'cleanup': {'deleted': False}, 'image': {}
            }))
            return {'exit': 1, 'stdout': '', 'stderr': 'cleanup failed'}
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / 'evidence'
            with patch.object(module, 'github', side_effect=github), patch.object(module, 'command', side_effect=command), patch('sys.argv', [str(SCRIPT), '--out', str(out), '--run']):
                with self.assertRaisesRegex(RuntimeError, 'refusing to create another VM'):
                    module.main()
            report = json.loads((out / 'matrix.json').read_text())
        self.assertEqual(len(journeys), 1)
        self.assertEqual(report['variants'][0]['runtime'], 'failed')
        self.assertEqual(report['variants'][1]['runtime'], 'not-run')

    def interrupted_journey(self, name, builder=False, unrelated=False):
        config = f'''
[variants.{name}]
description = "test image"
[[variants.{name}.flavors]]
id = "tuna"
build_image = true
[tuna_rollout.{name}]
status = "admitted"
package_target = "el10"
'''
        source = {'content': base64.b64encode(config.encode()).decode()}
        calls = []
        fake = FakeCorral()
        fake.node = 'karnataka'
        def github(path):
            return {'sha': 'a' * 40} if path.endswith('commits/main') else source
        def command(argv, timeout=120, input=None):
            calls.append(argv)
            if argv[0] == 'skopeo':
                return {'exit': 0, 'stdout': json.dumps({'Digest': 'sha256:' + 'b' * 64}), 'stderr': ''}
            if argv[0] in ('corral', 'kubectl'):
                return fake.command(argv, timeout=timeout, input=input)
            fake.name = argv[argv.index('--vm') + 1]
            identity = fake.prepare()
            fake.add_vm(alternatives=unrelated)
            if builder:
                fake.add_vm(builder=True, alternatives=unrelated)
            directory = Path(argv[argv.index('--out') + 1])
            directory.mkdir()
            # This is the early durable receipt, not a completed manifest.
            (directory / 'identity.json').write_text(json.dumps(identity.state))
            raise module.subprocess.TimeoutExpired(argv, timeout)
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / 'evidence'
            with patch.object(module, 'github', side_effect=github), patch.object(module, 'command', side_effect=command), patch('sys.argv', [str(SCRIPT), '--out', str(out), '--run']):
                module.main()
            report = json.loads((out / 'matrix.json').read_text())
            frozen = out / 'harness/lib/corral_identity.py'
            self.assertEqual(frozen.read_bytes(), (SCRIPT.parent / 'lib/corral_identity.py').read_bytes())
            self.assertTrue((out / name / 'identity.json').exists())
        return calls, report

    def test_longest_variant_vm_fits_corral_builder_label(self):
        calls, _ = self.interrupted_journey('bonito-rawhide')
        journey = next(argv for argv in calls if argv[0] != 'skopeo' and argv[0] != 'corral')
        vm = journey[journey.index('--vm') + 1]
        self.assertLessEqual(len(vm + '-bootc-builder-cloudinit'), 63)

    def test_interrupted_build_deletes_owned_builder(self):
        calls, report = self.interrupted_journey('grouper', builder=True)
        journey = next(argv for argv in calls if argv[0] != 'skopeo' and argv[0] != 'corral')
        vm = journey[journey.index('--vm') + 1]
        deleted = [operation['canonical_identity']['id']
                   for operation in report['variants'][0]['emergency_cleanup']['result']]
        self.assertEqual(deleted, [FakeCorral.builder_target, FakeCorral.target])
        self.assertEqual(report['variants'][0]['runtime'], 'failed')

    def test_cleanup_does_not_delete_prefix_collision(self):
        calls, report = self.interrupted_journey('grouper', builder=True, unrelated=True)
        journey = next(argv for argv in calls if argv[0] != 'skopeo' and argv[0] != 'corral')
        vm = journey[journey.index('--vm') + 1]
        deleted = [operation['canonical_identity']['id']
                   for operation in report['variants'][0]['emergency_cleanup']['result']]
        self.assertEqual(deleted, [FakeCorral.builder_target, FakeCorral.target])
        self.assertEqual(report['variants'][0]['runtime'], 'failed')
        self.assertTrue(report['variants'][0]['emergency_cleanup']['deleted'])

    def test_emergency_cleanup_without_early_identity_never_guesses_a_vm_id(self):
        with tempfile.TemporaryDirectory() as tmp, patch.object(module, 'command') as command:
            result = module.emergency_cleanup(Path(tmp), Path(tmp) / 'harness',
                                              FakeCorral.name, 'karnataka')
        self.assertFalse(result['deleted'])
        self.assertIn('no retained identity', result['failures'][0])
        command.assert_not_called()

    def test_deferred_frame_validation_is_required_for_functional_capture(self):
        config = '''
[variants.yellowfin]
description = "test image"
[[variants.yellowfin.flavors]]
id = "tuna"
build_image = true
[tuna_rollout.yellowfin]
status = "admitted"
package_target = "el10"
'''
        source = {'content': base64.b64encode(config.encode()).decode()}
        for validated in (None, False, True):
            with self.subTest(frame_validated=validated), tempfile.TemporaryDirectory() as tmp:
                def github(path):
                    return {'sha': 'a' * 40} if path.endswith('commits/main') else source
                def command(argv, timeout=120):
                    if argv[0] == 'skopeo':
                        return {'exit': 0, 'stdout': json.dumps({'Digest': 'sha256:' + 'b' * 64}), 'stderr': ''}
                    directory = Path(argv[argv.index('--out') + 1])
                    directory.mkdir()
                    capture = {'lock_withdrawal_verified': True}
                    if validated is not None:
                        capture['frame_validated'] = validated
                    (directory / 'manifest.json').write_text(json.dumps({
                        'steps': [{'id': sid, 'result': 'pass'} for sid in
                                  ('portal-availability', 'portal-screenshot', 'portal-screencast')],
                        'portal_screencast': capture, 'cleanup': {'deleted': True}, 'image': {}
                    }))
                    return {'exit': 0, 'stdout': '', 'stderr': ''}
                out = Path(tmp) / 'evidence'
                with patch.object(module, 'github', side_effect=github), patch.object(module, 'command', side_effect=command), patch('sys.argv', [str(SCRIPT), '--out', str(out), '--run']), contextlib.redirect_stdout(io.StringIO()):
                    module.main()
                row = json.loads((out / 'matrix.json').read_text())['variants'][0]
                expected = ('functional-capture-and-lock-revoke-proved; visual-review-required'
                            if validated else 'screenshot-functional; screencast-unproven')
                self.assertEqual(row['gates']['portals'], expected)


if __name__ == '__main__':
    unittest.main()
