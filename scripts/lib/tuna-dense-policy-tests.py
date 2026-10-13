#!/usr/bin/env python3
"""Dense full-paired policy binding regression; no image, VM, build, or rerun.

Measured fixture (digest-verified f58 diagnostic ZIP
1b1871532403e0e8f91bd3a93b18a3b49a76ee5b40279ab8274f97ea54f3f489,
source f58e30c51ae599d18bdad51a9a75e3dd2279fa05):
allocated 4074299392, transfer 4105446912, virtual 18Gi, mapped
nonzero 17074094080, estimate 21716411904, raw allocated 8055300096
(formula-integrity witness only, NOT an estimate input), qemu RSS
33853440 (memory, NOT a storage-bound input).
"""
import copy
import inspect
from pathlib import Path
import unittest
from unittest.mock import patch
from tuna_benchmark_disk import (
    allocated_extents, provenance, storage_receipt,
    dense_full_paired_storage_receipt, VIRTUAL_BYTES, GIB,
    GUEST_WRITE_ALLOWANCE, MAX_EXPORT_BYTES, MAX_ESTIMATED_PEAK,
    DENSE_FULL_PAIRED_MAX_EXPORT_BYTES, DENSE_FULL_PAIRED_MAX_ESTIMATED_PEAK,
    DENSE_FULL_PAIRED_REQUESTED_PVC, DENSE_FULL_PAIRED_REQUESTED_PVC_BYTES,
)
import tuna_benchmark_disk as diskmod

ALLOCATED = 4074299392
TRANSFER = 4105446912
NONZERO = 17074094080
ALLOWANCE = 536870912
ESTIMATE = 21716411904
RAW_ALLOC = 8055300096
QEMU_RSS = 33853440
HOLE = VIRTUAL_BYTES - NONZERO
# Compact totals-equivalent of the verified 15571-extent qemu map (11120
# data+nonzero summing to NONZERO, 0 data+zero, remainder non-data).
MEASURED_MAPPING = [
    {'start': 0, 'length': NONZERO, 'data': True, 'zero': False},
    {'start': NONZERO, 'length': HOLE, 'data': False, 'zero': True},
]


class DensePolicyTests(unittest.TestCase):
    def setUp(self):
        self.info = {'format': 'qcow2', 'virtual-size': VIRTUAL_BYTES,
                     'actual-size': ALLOCATED}
        self.mapping = copy.deepcopy(MEASURED_MAPPING)
        self.reference = {'schema': 1, 'profile': 'stock',
                          'mutter_package': 'mutter 51.0-1',
                          'shell_package': 'gnome-shell 51.0-1',
                          'packages_sha256': 'a' * 64,
                          'critical_sha256': 'b' * 64}
        self.payload = {'package': {'sha256': 'c' * 64},
                        'required_members': {
                            'usr/bin/tuna-compositor': {'sha256': 'd' * 64}}}
        self.selection = {'head_sha': 'e' * 40, 'run_id': 123}

    def test_original_2gi_7gi_still_fails_measured_fixture(self):
        self.assertEqual(MAX_EXPORT_BYTES, 2 * GIB)
        self.assertEqual(MAX_ESTIMATED_PEAK, 7 * GIB)
        with self.assertRaisesRegex(ValueError, 'allocation exceeds export bound'):
            storage_receipt(self.info, self.mapping, TRANSFER)
        small = {'format': 'qcow2', 'virtual-size': VIRTUAL_BYTES,
                 'actual-size': GIB}
        with self.assertRaisesRegex(ValueError, 'transfer exceeds 2Gi'):
            storage_receipt(small, self.mapping, TRANSFER)
        with self.assertRaisesRegex(ValueError, 'exceeds 7Gi bound'):
            storage_receipt(small, self.mapping, GIB)

    def test_dense_leaf_admits_measured_fixture_exact_receipt(self):
        self.assertEqual(VIRTUAL_BYTES, 18 * GIB)
        self.assertEqual(HOLE, 2253258752)
        self.assertEqual(DENSE_FULL_PAIRED_MAX_EXPORT_BYTES, 5 * GIB)
        self.assertEqual(DENSE_FULL_PAIRED_MAX_ESTIMATED_PEAK, 24 * GIB)
        self.assertEqual(NONZERO + TRANSFER + ALLOWANCE, ESTIMATE)
        result = dense_full_paired_storage_receipt(
            self.info, self.mapping, TRANSFER)
        self.assertEqual(result['policy'], 'dense-full-paired-5gi-24gi')
        self.assertEqual(result['virtual_bytes'], VIRTUAL_BYTES)
        self.assertEqual(result['qcow2_allocated_bytes'], ALLOCATED)
        self.assertEqual(result['transfer_bytes'], TRANSFER)
        self.assertEqual(result['mapped_nonzero_bytes'], NONZERO)
        self.assertEqual(result['guest_write_allowance_bytes'], ALLOWANCE)
        self.assertEqual(result['estimated_peak_bytes'], ESTIMATE)
        self.assertEqual(result['estimated_peak_limit_bytes'], 24 * GIB)
        self.assertEqual(result['requested_pvc'], '28Gi')
        self.assertEqual(result['requested_pvc_bytes'], 28 * GIB)
        self.assertEqual(result['requested_pvc'],
                         DENSE_FULL_PAIRED_REQUESTED_PVC)
        self.assertEqual(result['requested_pvc_bytes'],
                         DENSE_FULL_PAIRED_REQUESTED_PVC_BYTES)
        self.assertFalse(result['node_peak_qualified'])
        self.assertIn('CDI scratch/preallocation', result['limitation'])
        self.assertLess(ALLOCATED, 5 * GIB)
        self.assertLess(TRANSFER, 5 * GIB)
        self.assertLess(ESTIMATE, 24 * GIB)
        self.assertLess(ESTIMATE, 28 * GIB)

    def test_formula_keeps_allowance_not_rss_or_raw(self):
        self.assertEqual(GUEST_WRITE_ALLOWANCE, GIB // 2)
        self.assertEqual(ALLOWANCE, GUEST_WRITE_ALLOWANCE)
        self.assertEqual(ESTIMATE, NONZERO + TRANSFER + GUEST_WRITE_ALLOWANCE)
        self.assertNotEqual(ESTIMATE, RAW_ALLOC + TRANSFER + ALLOWANCE)
        self.assertNotEqual(ESTIMATE, NONZERO + QEMU_RSS + ALLOWANCE)
        self.assertNotEqual(ESTIMATE, RAW_ALLOC + QEMU_RSS + ALLOWANCE)
        source = inspect.getsource(dense_full_paired_storage_receipt)
        self.assertIn('nonzero + file_bytes + GUEST_WRITE_ALLOWANCE', source)
        code = source.split('"""', 2)[-1]
        self.assertNotIn('RSS', code)
        self.assertNotIn('rss', code)
        self.assertNotIn('raw', code.lower())

    def test_dense_leaf_5gi_boundaries(self):
        small_nonzero = [{'start': 0, 'length': GIB, 'data': True, 'zero': False},
                         {'start': GIB, 'length': VIRTUAL_BYTES - GIB,
                          'data': False, 'zero': True}]
        at_cap = {'format': 'qcow2', 'virtual-size': VIRTUAL_BYTES,
                  'actual-size': 5 * GIB}
        result = dense_full_paired_storage_receipt(
            at_cap, small_nonzero, 5 * GIB)
        self.assertEqual(result['estimated_peak_bytes'], 5 * GIB + GIB + GIB // 2)
        over = {'format': 'qcow2', 'virtual-size': VIRTUAL_BYTES,
                'actual-size': 5 * GIB + 1}
        with self.assertRaisesRegex(ValueError, 'dense full-paired 5Gi bound'):
            dense_full_paired_storage_receipt(over, small_nonzero, GIB)
        with self.assertRaisesRegex(ValueError, 'dense full-paired 5Gi bound'):
            dense_full_paired_storage_receipt(at_cap, small_nonzero, 5 * GIB + 1)
        with self.assertRaises(ValueError):
            dense_full_paired_storage_receipt(at_cap, small_nonzero, 0)

    def test_dense_leaf_24gi_is_conservative_headroom(self):
        full = [{'start': 0, 'length': VIRTUAL_BYTES, 'data': True, 'zero': False}]
        info = {'format': 'qcow2', 'virtual-size': VIRTUAL_BYTES,
                'actual-size': 5 * GIB}
        result = dense_full_paired_storage_receipt(info, full, 5 * GIB)
        max_real = VIRTUAL_BYTES + 5 * GIB + GIB // 2
        self.assertEqual(max_real, 25232932864)
        self.assertEqual(result['estimated_peak_bytes'], max_real)
        self.assertLess(max_real, 24 * GIB)
        with patch.object(diskmod, 'allocated_extents', return_value=19 * GIB):
            with self.assertRaisesRegex(ValueError, 'dense full-paired 24Gi bound'):
                dense_full_paired_storage_receipt(info, full, 5 * GIB)

    def test_dense_leaf_rejects_invalid_map_backing_format(self):
        good = {'format': 'qcow2', 'virtual-size': VIRTUAL_BYTES,
                'actual-size': GIB}
        small = [{'start': 0, 'length': GIB, 'data': True, 'zero': False},
                 {'start': GIB, 'length': VIRTUAL_BYTES - GIB,
                  'data': False, 'zero': True}]
        for patch in [{'backing-filename': 'other'},
                      {'full-backing-filename': 'other'},
                      {'snapshots': [{}]}, {'format': 'raw'},
                      {'virtual-size': 21 * GIB}, {'actual-size': True}]:
            with self.subTest(patch=patch), self.assertRaises(ValueError):
                dense_full_paired_storage_receipt(good | patch, small, GIB)
        for field, value in [('start', 1), ('length', True), ('data', 1),
                             ('zero', None)]:
            broken = copy.deepcopy(small)
            broken[0][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                dense_full_paired_storage_receipt(good, broken, GIB)
        with self.assertRaises(ValueError):
            dense_full_paired_storage_receipt(good, small[:1], GIB)
        with self.assertRaises(ValueError):
            allocated_extents(small[:1], VIRTUAL_BYTES)

    def test_hash_and_provenance_negatives(self):
        good = provenance('f' * 40, self.selection, self.payload,
                          self.reference, 'stock')
        self.assertEqual(good['paired_desktops'], ['gnome', 'tuna'])
        self.assertFalse(good['signed_release'])
        self.assertFalse(good['performance_qualified'])
        with self.assertRaises(ValueError):
            provenance('xyz', self.selection, self.payload,
                       self.reference, 'stock')
        with self.assertRaises(ValueError):
            provenance('f' * 40, self.selection | {'head_sha': 'zz'},
                       self.payload, self.reference, 'stock')
        with self.assertRaises(ValueError):
            provenance('f' * 40, self.selection | {'run_id': 0},
                       self.payload, self.reference, 'stock')
        bad_pkg = copy.deepcopy(self.payload)
        bad_pkg['package'] = {'sha256': 'not-a-hash'}
        with self.assertRaises(ValueError):
            provenance('f' * 40, self.selection, bad_pkg,
                       self.reference, 'stock')
        bad_member = copy.deepcopy(self.payload)
        bad_member['required_members']['usr/bin/tuna-compositor'] = {}
        with self.assertRaises(ValueError):
            provenance('f' * 40, self.selection, bad_member,
                       self.reference, 'stock')
        with self.assertRaises(ValueError):
            provenance('f' * 40, self.selection, self.payload,
                       self.reference, 'diagnostic')

    def test_exporter_binding_is_reviewed_policy_with_historical_diagnostic(self):
        root = Path(__file__).resolve().parents[2]
        text = (root / 'scripts/tuna-benchmark-disk-export').read_text()
        compile(text, 'actual-exporter', 'exec')
        self.assertIn(
            "receipt['storage'] = dense_full_paired_storage_receipt(info, mapping, disk.stat().st_size)",
            text)
        self.assertLess(
            text.index("receipt['dense_decode'] = dense_decode"),
            text.index("receipt['historical_compact_storage_error']"))
        self.assertLess(
            text.index("receipt['historical_compact_storage_error']"),
            text.index("receipt['storage'] = dense_full_paired_storage_receipt"))
        self.assertIn(
            "receipt['historical_compact_storage'] = storage_receipt(info, mapping, disk.stat().st_size)",
            text)
        self.assertNotIn('except NameError', text)
        self.assertNotIn("receipt['dense_full_paired_storage']", text)
        self.assertIn('dense_full_paired_storage_receipt', text)
        self.assertIn('DENSE_FULL_PAIRED_REQUESTED_PVC', text)
        self.assertIn("requested_pvc=DENSE_FULL_PAIRED_REQUESTED_PVC", text)
        self.assertNotIn("requested_pvc='20Gi'", text)
        self.assertIn('node_storage_qualified=False', text)
        self.assertEqual(DENSE_FULL_PAIRED_REQUESTED_PVC, '28Gi')
        self.assertEqual(DENSE_FULL_PAIRED_REQUESTED_PVC_BYTES, 28 * GIB)
        self.assertIn('/usr/share/tuna-perf-reference.json', text)
        self.assertIn('Exec=/usr/libexec/tuna-perf-session', text)
        self.assertIn("pacman', '-Q', 'linux'", text)


if __name__ == '__main__':
    unittest.main()
