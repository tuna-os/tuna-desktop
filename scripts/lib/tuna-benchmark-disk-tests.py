#!/usr/bin/env python3
"""Pure disk-export admission/provenance regressions; no image or VM builds."""
import copy
from pathlib import Path
import unittest
from tuna_benchmark_disk import allocated_extents, provenance, storage_receipt, VIRTUAL_BYTES, GIB


class DiskTests(unittest.TestCase):
    def setUp(self):
        self.info = {'format': 'qcow2', 'virtual-size': VIRTUAL_BYTES, 'actual-size': GIB}
        self.mapping = [{'start': 0, 'length': 4*GIB, 'data': True, 'zero': False},
                        {'start': 4*GIB, 'length': 14*GIB, 'data': False, 'zero': True}]
        self.reference = {'schema': 1, 'profile': 'stock', 'mutter_package': 'mutter 51.0-1',
                          'shell_package': 'gnome-shell 51.0-1', 'packages_sha256': 'a'*64,
                          'critical_sha256': 'b'*64}
        self.payload = {'package': {'sha256': 'c'*64},
                        'required_members': {'usr/bin/tuna-compositor': {'sha256': 'd'*64}}}
        self.selection = {'head_sha': 'e'*40, 'run_id': 123}

    def test_finite_storage_estimate_never_claims_node_qualification(self):
        result = storage_receipt(self.info, self.mapping, GIB)
        self.assertEqual(result['estimated_peak_bytes'], 5*GIB + GIB//2)
        self.assertFalse(result['node_peak_qualified'])

    def test_backing_snapshots_wrong_format_or_virtual_size_refused(self):
        for patch in [{'backing-filename': 'other'}, {'snapshots': [{}]}, {'format': 'raw'},
                      {'virtual-size': 21*GIB}, {'actual-size': True}]:
            with self.subTest(patch=patch), self.assertRaises(ValueError):
                storage_receipt(self.info | patch, self.mapping, GIB)

    def test_compressed_and_expanded_bounds_independent(self):
        with self.assertRaises(ValueError): storage_receipt(self.info, self.mapping, 2*GIB+1)
        large = [{'start': 0, 'length': VIRTUAL_BYTES, 'data': True, 'zero': False}]
        with self.assertRaises(ValueError): storage_receipt(self.info, large, GIB)

    def test_map_gap_overlap_bool_and_unclassified_data_refused(self):
        for field, value in [('start', 1), ('length', True), ('data', 1), ('zero', None)]:
            broken = copy.deepcopy(self.mapping); broken[0][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                allocated_extents(broken, VIRTUAL_BYTES)
        with self.assertRaises(ValueError): allocated_extents(self.mapping[:1], VIRTUAL_BYTES)

    def test_reference_and_package_identity_remain_separate(self):
        result = provenance('f'*40, self.selection, self.payload, self.reference, 'stock')
        self.assertEqual(result['candidate_source_commit'], 'e'*40)
        self.assertEqual(result['observer_source_commit'], 'f'*40)
        self.assertFalse(result['signed_release'])
        self.assertFalse(result['performance_qualified'])

    def test_profile_and_missing_native_hash_refused(self):
        with self.assertRaises(ValueError):
            provenance('f'*40, self.selection, self.payload, self.reference, 'diagnostic')
        bad = copy.deepcopy(self.payload); bad['required_members']['usr/bin/tuna-compositor'] = {}
        with self.assertRaises(ValueError): provenance('f'*40, self.selection, bad, self.reference, 'stock')

    def test_candidate_requires_dispatched_exact_source_same_repository_digest(self):
        import importlib.util
        spec = importlib.util.spec_from_file_location('disk_export_payload', Path(__file__).with_name('tuna-perf-payload.py'))
        payload = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(payload)
        run = {'id': 12, 'event': 'workflow_dispatch', 'status': 'completed', 'conclusion': 'success',
               'head_sha': 'a'*40, 'head_branch': 'codex/candidate', 'path': '.github/workflows/ci.yml',
               'repository': {'id': 3, 'full_name': 'tuna-os/tuna-desktop'},
               'head_repository': {'id': 3, 'full_name': 'tuna-os/tuna-desktop'}}
        artifact = {'id': 4, 'name': 'tuna-arch', 'expired': False, 'size_in_bytes': 4096,
                    'digest': 'sha256:'+'b'*64, 'workflow_run': {'id': 12, 'head_sha': 'a'*40,
                    'head_branch': 'codex/candidate', 'repository_id': 3}}
        good = payload.metadata(run, artifact, 12, 'tuna-os/tuna-desktop', 'a'*40)
        self.assertFalse(good['release_qualified'])
        for field, value in [('event', 'pull_request'), ('status', 'in_progress'),
                             ('head_sha', 'c'*40), ('path', '.github/workflows/foreign.yml'),
                             ('head_repository', {'id': 8, 'full_name': 'fork/tuna-desktop'})]:
            with self.subTest(field=field), self.assertRaises(ValueError):
                payload.metadata(run | {field: value}, artifact, 12, 'tuna-os/tuna-desktop', 'a'*40)
        for patch in [{'digest': None}, {'name': 'marlin-performance'},
                      {'workflow_run': artifact['workflow_run'] | {'head_branch': 'main'}},
                      {'workflow_run': artifact['workflow_run'] | {'repository_id': 9}}]:
            with self.subTest(patch=patch), self.assertRaises(ValueError):
                payload.metadata(run, artifact | patch, 12, 'tuna-os/tuna-desktop', 'a'*40)

    def test_workflow_export_only_guard_and_candidate_identity(self):
        source = (Path(__file__).resolve().parents[2]/'.github/workflows/performance-baseline.yml').read_text()
        self.assertIn('[[ "$EXPORT_ONLY" == true &&', source)
        self.assertIn("selected package does not match requested candidate source", source)
        self.assertIn("!inputs.export_only && steps.shared_payload.outcome", source)
        self.assertIn('compression-level: 0', source)

    def test_export_probes_and_install_use_inspected_image_id(self):
        import ast
        source = (Path(__file__).resolve().parents[2]/'scripts/tuna-benchmark-disk-export').read_text()
        tree = ast.parse(source)
        main = next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == 'main')
        assignments = [n for n in ast.walk(main) if isinstance(n, ast.Assign)
                       and any(isinstance(t, ast.Name) and t.id == 'image' for t in n.targets)]
        self.assertEqual(len(assignments), 2)
        pinned = assignments[1]
        self.assertEqual(ast.unparse(pinned.value), "receipt['fixture_image_id']")
        for command in ast.walk(main):
            if isinstance(command, ast.List) and command.lineno > pinned.lineno:
                self.assertNotIn('localhost/marlin-corral-benchmark:export',
                                 [e.value for e in command.elts if isinstance(e, ast.Constant)])


if __name__ == '__main__':
    unittest.main()
