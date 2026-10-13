"""Dense consumer 28Gi/5Gi regression; no import, VM, build, or rerun.

Covers the reviewed consumer handoff for the dense full-paired 5Gi export:
28Gi PVC admission, 5Gi transfer caps, 5Gi/6Gi archive bounds, headroom math,
synthetic-fixture refusal, raw >5Gi non-admission, and byte-identical guards.
Node-capacity inputs are either explicitly SYNTHETIC fixtures or a labeled
MIXED fixture (actual observed fs bytes inside a synthetic envelope);
nothing here claims any current node state.
"""
import pathlib
import sys
import unittest

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / 'lib'))
import corral_artifact as artifact
import corral_raw as rawmod
from corral_perf import GIB, capacity_receipt

ROOT = pathlib.Path(__file__).resolve().parents[2]
# Measured f58 binding fixture (producer packet, read-only reference).
ALLOCATED = 4074299392
TRANSFER = 4105446912
NONZERO = 17074094080
ESTIMATE = 21716411904
# Consumer plan math: 56Gi + transfer + 1Gi + 0.5Gi.
DENSE_ESTIMATE = 56 * GIB + TRANSFER + GIB + GIB // 2


def dense_manifest(transfer=TRANSFER):
    return {'storage': {'virtual_bytes': 18 * GIB, 'transfer_bytes': transfer,
                        'mapped_nonzero_bytes': NONZERO},
            'requested_pvc': '28Gi', 'firmware': 'uefi'}


def compact_manifest(transfer=2 * GIB):
    return {'storage': {'virtual_bytes': 18 * GIB, 'transfer_bytes': transfer},
            'requested_pvc': '20Gi', 'firmware': 'uefi'}


class FakeEntry:
    def __init__(self, filename, file_size):
        self.filename = filename
        self.file_size = file_size
        self.external_attr = 0


class FakeArchive:
    def __init__(self, entries):
        self._entries = entries

    def infolist(self):
        return self._entries


class DenseConsumer(unittest.TestCase):
    def test_dense_28gi_admits_measured_fixture_exact_plan(self):
        plan = artifact.disk_plan(dense_manifest())
        self.assertEqual(plan['target_pvc_bytes'], 28 * GIB)
        self.assertEqual(plan['scratch_pvc_bytes'], 28 * GIB)
        self.assertEqual(plan['target_pvc_bytes'] + plan['scratch_pvc_bytes'], 56 * GIB)
        self.assertEqual(plan['transfer_temporary_allowance_bytes'], TRANSFER)
        self.assertEqual(plan['estimated_peak_bytes'], DENSE_ESTIMATE)
        self.assertEqual(DENSE_ESTIMATE, 65845601792)
        self.assertFalse(plan['performance_qualified'])
        self.assertFalse(plan['node_peak_qualified'])
        self.assertIn('node admission pending measurement', plan['limitation'])
        self.assertIn('no node is admitted by this plan', plan['limitation'])
        # Producer percentages, re-derived: transfer/alloc within 5Gi.
        self.assertLess(TRANSFER, 5 * GIB)
        self.assertLess(ALLOCATED, 5 * GIB)

    def test_compact_20gi_frozen_no_silent_widening(self):
        plan = artifact.disk_plan(compact_manifest())
        self.assertEqual(plan['target_pvc_bytes'], 20 * GIB)
        self.assertEqual(plan['scratch_pvc_bytes'], 20 * GIB)
        self.assertEqual(plan['estimated_peak_bytes'], 40 * GIB + 2 * GIB + GIB + GIB // 2)
        # Old path refuses anything over 2Gi, including the dense 5Gi transfer.
        for bad in (2 * GIB + 1, 5 * GIB, TRANSFER):
            with self.assertRaises(ValueError):
                artifact.disk_plan(compact_manifest(transfer=bad))

    def test_dense_5gi_boundaries_and_negatives(self):
        artifact.disk_plan(dense_manifest(transfer=5 * GIB))
        for bad in (0, -1, 5 * GIB + 1):
            with self.assertRaises(ValueError):
                artifact.disk_plan(dense_manifest(transfer=bad))
        for bad_pvc in ('20Gib', '30Gi', '', None, '28gi'):
            bad = dense_manifest()
            bad['requested_pvc'] = bad_pvc
            with self.assertRaises(ValueError):
                artifact.disk_plan(bad)
        bad = dense_manifest()
        bad['storage']['virtual_bytes'] = 20 * GIB
        with self.assertRaises(ValueError):
            artifact.disk_plan(bad)
        bad = dense_manifest()
        bad['firmware'] = 'bios'
        with self.assertRaises(ValueError):
            artifact.disk_plan(bad)
        with self.assertRaises(ValueError):
            artifact.disk_plan(dense_manifest(), method='unproven-raw')

    def test_storage_schema_typed_validation(self):
        # transfer_bytes must be a genuine int: bool/float/str/None/missing
        # refuse (bool True previously admitted as 1 byte; str/None raised
        # bare TypeError). Genuine 0/negative still refuse via range bounds.
        for bad in (True, False, 4105446912.0, '4105446912', None):
            bad_manifest = dense_manifest(transfer=bad)
            with self.subTest(transfer=bad):
                with self.assertRaisesRegex(ValueError, 'bounded integer transfer bytes required'):
                    artifact.disk_plan(bad_manifest)
        missing = dense_manifest()
        del missing['storage']['transfer_bytes']
        with self.assertRaisesRegex(ValueError, 'bounded integer transfer bytes required'):
            artifact.disk_plan(missing)
        no_storage = {'requested_pvc': '28Gi', 'firmware': 'uefi'}
        with self.assertRaisesRegex(ValueError, 'bounded storage manifest required'):
            artifact.disk_plan(no_storage)
        listed = dense_manifest()
        listed['storage'] = ['transfer_bytes']
        with self.assertRaisesRegex(ValueError, 'bounded storage manifest required'):
            artifact.disk_plan(listed)
        # Compact lane enforces the same schema.
        with self.assertRaisesRegex(ValueError, 'bounded integer transfer bytes required'):
            artifact.disk_plan(compact_manifest(transfer=True))

    def test_virtual_bytes_typed_both_lanes(self):
        # virtual_bytes must be a genuine int in BOTH lanes: float
        # 19327352832.0 numerically equals 18Gi and previously admitted
        # via !=; bool/str/missing refuse as well.
        for make in (dense_manifest, compact_manifest):
            for bad in (19327352832.0, True, '19327352832'):
                bad_manifest = make()
                bad_manifest['storage']['virtual_bytes'] = bad
                with self.subTest(lane=make.__name__, virtual=bad):
                    with self.assertRaisesRegex(ValueError, 'bounded integer virtual bytes required'):
                        artifact.disk_plan(bad_manifest)
            missing = make()
            del missing['storage']['virtual_bytes']
            with self.subTest(lane=make.__name__, virtual='missing'):
                with self.assertRaisesRegex(ValueError, 'bounded integer virtual bytes required'):
                    artifact.disk_plan(missing)

    def test_archive_member_total_bounds_cover_5gi(self):
        self.assertEqual(artifact.ARCHIVE_MAX_MEMBER_BYTES, 5 * GIB)
        self.assertEqual(artifact.ARCHIVE_MAX_TOTAL_BYTES, 6 * GIB)
        self.assertEqual(artifact.ARCHIVE_MAX_ZIP_BYTES, 6 * GIB)
        # 5Gi member + small manifest admits; over-member refuses.
        ok = FakeArchive([FakeEntry('manifest.json', 1024), FakeEntry('disk.qcow2', 5 * GIB)])
        _, total = artifact.safe_members(ok)
        self.assertEqual(total, 5 * GIB + 1024)
        over_member = FakeArchive([FakeEntry('manifest.json', 1024), FakeEntry('disk.qcow2', 5 * GIB + 1)])
        with self.assertRaisesRegex(ValueError, 'expansion exceeds'):
            artifact.safe_members(over_member)
        # Total 6Gi admits; 6Gi+1 refuses.
        at_total = FakeArchive([FakeEntry('manifest.json', GIB), FakeEntry('disk.qcow2', 5 * GIB)])
        _, total = artifact.safe_members(at_total)
        self.assertEqual(total, 6 * GIB)
        over_total = FakeArchive([FakeEntry('manifest.json', GIB + 1), FakeEntry('disk.qcow2', 5 * GIB)])
        with self.assertRaisesRegex(ValueError, 'expansion exceeds'):
            artifact.safe_members(over_total)
        # Traversal/link/missing guards still refuse.
        for name in ('../disk.qcow2', '/disk.qcow2'):
            with self.assertRaises(ValueError):
                artifact.safe_members(FakeArchive([FakeEntry('manifest.json', 1), FakeEntry(name, 1)]))
        with self.assertRaises(ValueError):
            artifact.safe_members(FakeArchive([FakeEntry('manifest.json', 1)]))

    def test_download_zip_headroom_math(self):
        # Max: 6Gi ZIP + 6Gi expansion + 0.5Gi = 12.5Gi free required.
        self.assertEqual(6 * GIB + 6 * GIB + GIB // 2, 13421772800)
        # Measured transfer ZIP (~TRANSFER + 1MiB overhead) fits 6Gi with margin.
        measured_zip = TRANSFER + (1 << 20)
        self.assertLess(measured_zip, 6 * GIB)
        headroom_needed = measured_zip + 6 * GIB + GIB // 2
        self.assertLess(headroom_needed, 13421772800)
        source = (ROOT / 'scripts/lib/corral_artifact.py').read_text()
        self.assertIn('ARCHIVE_MAX_ZIP_BYTES', source)
        self.assertIn('size+ARCHIVE_MAX_TOTAL_BYTES+GIB//2', source.replace(' ', ''))

    def test_synthetic_fixture_refuses_56gi_reserve(self):
        # SYNTHETIC fixture (illustrative 100Gi/20Gi numbers): proves the
        # refusal arithmetic only. It is NOT any current node; see the
        # actual-baseline test below for observed values with provenance.
        node = {'metadata': {'name': 'synthetic-node', 'uid': 'synthetic-uid'}, 'status': {
            'conditions': [{'type': 'Ready', 'status': 'True'},
                           {'type': 'DiskPressure', 'status': 'False'}],
            'allocatable': {'devices.kubevirt.io/' + k: '1k' for k in ('kvm', 'tun', 'vhost-net')}}}
        fs = {'capacityBytes': 100 * GIB, 'availableBytes': 20 * GIB}
        stats = {'node': {'fs': fs, 'runtime': {'imageFs': fs, 'containerFs': fs}}}
        admission = capacity_receipt(node, stats, estimated_peak_bytes=DENSE_ESTIMATE)
        self.assertFalse(admission['ready_for_disk_admission'])
        self.assertIn('insufficient-disk-headroom', admission['reasons'])
        self.assertFalse(admission['performance_qualified'])
        # Producer 24Gi estimate also does NOT fit this synthetic fixture.
        producer_refused = capacity_receipt(node, stats, estimated_peak_bytes=ESTIMATE)
        self.assertFalse(producer_refused['ready_for_disk_admission'])

    def test_actual_baseline_17_35_mixed_fixture_refuses_56gi_reserve(self):
        # MIXED arithmetic fixture, NOT a full authentic node capture: the
        # fs capacity/available bytes are the ACTUAL observed receipt values
        # (provenance below; embedded because /dev/shm is ephemeral, so CI
        # cannot read the receipt file), while the node uid, conditions,
        # KVM allocatable, and runtime imageFs/containerFs pools are a
        # SYNTHETIC envelope (the receipt carries no UID/KVM/runtime pools).
        # Receipt /dev/shm/tuna-v1-aws-node-capacity-20261013.json,
        # observed_at 2026-10-13T17:35:03.090138+00:00, context
        # admin@aws-migration, node ip-10-20-1-12, source
        # 'explicit-context kubelet stats/summary read only', scope
        # 'capacity baseline only; no import/CDI peak/boot/performance
        # qualification'. fs capacityBytes 156680331264 (~146Gi),
        # availableBytes 55986307072 (~52Gi).
        node = {'metadata': {'name': 'ip-10-20-1-12', 'uid': 'synthetic-envelope'}, 'status': {
            'conditions': [{'type': 'Ready', 'status': 'True'},
                           {'type': 'DiskPressure', 'status': 'False'}],
            'allocatable': {'devices.kubevirt.io/' + k: '1k' for k in ('kvm', 'tun', 'vhost-net')}}}
        fs = {'capacityBytes': 156680331264, 'availableBytes': 55986307072}
        stats = {'node': {'fs': fs, 'runtime': {'imageFs': fs, 'containerFs': fs}}}
        admission = capacity_receipt(node, stats, estimated_peak_bytes=DENSE_ESTIMATE)
        # Headroom math, exact: 55986307072 - (156680331264*15+99)//100 - 2**30.
        self.assertEqual((156680331264 * 15 + 99) // 100, 23502049690)
        self.assertEqual(admission['minimum_available_disk_headroom_bytes'], 31410515558)
        self.assertFalse(admission['ready_for_disk_admission'])
        self.assertIn('insufficient-disk-headroom', admission['reasons'])
        # Refusal arithmetic only; asserts no import/CDI/boot qualification.

    def test_raw_mapped_over_5gi_stays_unadmitted(self):
        source = (ROOT / 'scripts/lib/corral_raw.py').read_text()
        self.assertIn('mapped>5*GIB', source.replace(' ', ''))
        self.assertGreater(NONZERO, 5 * GIB)
        # Planner raw path still demands a probe and stays unqualified.
        with self.assertRaisesRegex(ValueError, 'RAW probe required'):
            artifact.disk_plan(dense_manifest(), method='raw-upload')
        proof = {'full_image_import_qualified': False,
                 'original_probe_receipt_sha256': 'a' * 64}
        plan = artifact.disk_plan(dense_manifest(transfer=GIB), method='raw-upload', raw_proof=proof)
        self.assertEqual(plan['target_pvc_bytes'], 28 * GIB)
        self.assertEqual(plan['estimated_peak_bytes'], 28 * GIB + GIB + GIB + GIB // 2)
        self.assertFalse(plan['full_image_import_qualified'])
        self.assertFalse(plan['node_peak_qualified'])

    def test_hash_source_package_guards_byte_identical(self):
        source = (ROOT / 'scripts/lib/corral_artifact.py').read_text()
        for pin in ("head_sha')!=source", 'original repository/workflow identity differs',
                    'artifact identity/retention differs', 'original archive bytes differ',
                    'exported disk bytes differ', 'actual disk transfer size differs',
                    'validate_image(manifest.get', "paired_desktops')!=['gnome','tuna']",
                    'matched paired candidate source required', 'exporter evidence bytes differ',
                    'original exporter evidence missing', 'authenticated_original_export',
                    "'performance_qualified':False", 'unsafe artifact member',
                    'ambiguous artifact path', 'disk artifact members missing',
                    'bounded18Gi/20Gi UEFI export required',
                    'bounded18Gi/28Gi dense UEFI export required (5Gi transfer)',
                    'Reserve full target AND scratch capacity'):
            self.assertIn(pin, source)
        importer = (ROOT / 'scripts/tuna-corral-benchmark-import').read_text()
        self.assertIn("manifest['requested_pvc']", importer)
        self.assertNotIn("'storage':'20Gi'", importer)
        self.assertIn('node_peak_qualified', importer)


if __name__ == '__main__':
    unittest.main()
