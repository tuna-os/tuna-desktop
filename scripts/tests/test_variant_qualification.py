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
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().parents[1] / 'tuna-variant-qualification'
loader = importlib.machinery.SourceFileLoader('variant_qualification', str(SCRIPT))
spec = importlib.util.spec_from_loader(loader.name, loader)
module = importlib.util.module_from_spec(spec)
loader.exec_module(module)


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
        calls, listings = [], []
        def github(path):
            return {'sha': 'a' * 40} if path.endswith('commits/main') else source
        def command(argv, timeout=120):
            calls.append(argv)
            if argv[0] == 'skopeo':
                return {'exit': 0, 'stdout': json.dumps({'Digest': 'sha256:' + 'b' * 64}), 'stderr': ''}
            if argv[:3] == ['corral', '--json', 'list']:
                # The first listing sees the interrupted builder; the final one proves absence.
                listed = [{'name': argv[-1] + '-bootc-builder'}] if builder and not listings else []
                if unrelated:
                    listed.append({'name': argv[-1] + '-another-run'})
                listings.append(listed)
                return {'exit': 0, 'stdout': json.dumps(listed), 'stderr': ''}
            if argv[:3] == ['corral', '--json', 'delete']:
                return {'exit': 0, 'stdout': json.dumps([{'name': argv[3], 'deleted': True}]), 'stderr': ''}
            raise module.subprocess.TimeoutExpired(argv, timeout)
        with tempfile.TemporaryDirectory() as tmp:
            out = Path(tmp) / 'evidence'
            with patch.object(module, 'github', side_effect=github), patch.object(module, 'command', side_effect=command), patch('sys.argv', [str(SCRIPT), '--out', str(out), '--run']):
                if unrelated:
                    with self.assertRaisesRegex(RuntimeError, 'cleanup unproven'):
                        module.main()
                else:
                    module.main()
            report = json.loads((out / 'matrix.json').read_text())
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
        deleted = [argv[3] for argv in calls if argv[:3] == ['corral', '--json', 'delete']]
        self.assertEqual(deleted, [vm, vm + '-bootc-builder'])
        self.assertEqual(report['variants'][0]['runtime'], 'failed')

    def test_cleanup_does_not_delete_prefix_collision(self):
        calls, report = self.interrupted_journey('grouper', builder=True, unrelated=True)
        journey = next(argv for argv in calls if argv[0] != 'skopeo' and argv[0] != 'corral')
        vm = journey[journey.index('--vm') + 1]
        deleted = [argv[3] for argv in calls if argv[:3] == ['corral', '--json', 'delete']]
        self.assertEqual(deleted, [vm, vm + '-bootc-builder'])
        self.assertEqual(report['variants'][0]['runtime'], 'failed')


if __name__ == '__main__':
    unittest.main()
