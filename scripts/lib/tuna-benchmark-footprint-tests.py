#!/usr/bin/env python3
"""Actual diagnostic helpers/export sequence; mocked tools, no image or VM."""
import hashlib
import json
from pathlib import Path
import tempfile
import textwrap
import unittest
from tuna_benchmark_disk import storage_receipt, GIB, VIRTUAL_BYTES
from tuna_benchmark_footprint import PATHS, disk_evidence, image_footprints

ROOT = Path(__file__).resolve().parents[2]
ID = 'sha256:'+'a'*64

class Commands:
    def __init__(self, usage=None):
        self.calls = []
        self.usage = usage
    def call(self, command, **bounds):
        self.calls.append((command, bounds))
        if 'inspect' in command:
            return 0, ID+'\n123456\n'+json.dumps([ID])
        return 0, self.usage if self.usage is not None else '\n'.join('123\t'+path for path in PATHS)

class FootprintTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(); self.addCleanup(self.temp.cleanup)
        self.out = Path(self.temp.name)
        self.roles = {name:ID for name in ['base','coherent','preview','fixture']}

    def test_fixed_immutable_images_readonly_networkless_and_no_memory_claim(self):
        commands = Commands(); receipt = image_footprints(commands, self.roles, self.out)
        self.assertTrue(receipt['complete']); self.assertFalse(receipt['decoder_rss_measured'])
        parsed = json.loads((self.out/receipt['file']).read_text())
        self.assertFalse(parsed['storage_admission_qualified'])
        self.assertEqual(receipt['sha256'], hashlib.sha256((self.out/receipt['file']).read_bytes()).hexdigest())
        self.assertEqual(len(commands.calls),8)
        for command, bounds in commands.calls:
            self.assertEqual(bounds, {'timeout':60})
            if 'run' in command:
                self.assertIn('--read-only', command)
                self.assertEqual(command[command.index('--network')+1], 'none')
                self.assertEqual(command[command.index('--pull')+1], 'never')
                self.assertEqual(command[-len(PATHS):], list(PATHS))
                self.assertIn(ID, command)

    def test_missing_duplicate_unexpected_usage_is_exposed_not_current(self):
        for usage in ['1\t/usr', '\n'.join('1\t'+p for p in PATHS)+'\n1\t/usr', '1\t/secret']:
            receipt = image_footprints(Commands(usage), self.roles, self.out)
            self.assertFalse(receipt['complete'])
            self.assertTrue(all('error' in item for item in json.loads((self.out/receipt['file']).read_text())['images'].values()))

    def test_mutable_reference_or_changed_image_refused_before_content_run(self):
        for reference in ['localhost/example:latest', 'sha256:'+'b'*64]:
            commands = Commands()
            receipt = image_footprints(commands, self.roles | {'fixture':reference}, self.out)
            self.assertFalse(receipt['complete'])
            self.assertFalse(any(reference in c and 'run' in c for c,_ in commands.calls))

    def test_missing_paths_explicit_none_no_summing(self):
        receipt = image_footprints(Commands('\n'.join('MISSING\t'+p for p in PATHS)), self.roles, self.out)
        observed = json.loads((self.out/receipt['file']).read_text())
        self.assertTrue(receipt['complete'])
        self.assertIsNone(observed['images']['fixture']['container_view_allocated_bytes']['/boot'])
        self.assertFalse(observed['images']['fixture']['layer_physical_bytes_measured'])

    def test_actual_export_sequence_persists_full_disk_evidence_before_guard_failure(self):
        source = (ROOT/'scripts/tuna-benchmark-disk-export').read_text()
        begin = source.index("        info = json.loads(output(['qemu-img', 'info'")
        end = source.index("        receipt['disk_sha256']", begin)
        sequence = textwrap.dedent(source[begin:end])
        disk=self.out/'disk.qcow2'; disk.write_bytes(b'owned-disk')
        raw=self.out/'disk.raw'; raw.write_bytes(b'owned-raw')
        info={'format':'qcow2','virtual-size':VIRTUAL_BYTES,'actual-size':3*GIB}
        mapping=[{'start':0,'length':VIRTUAL_BYTES,'data':True,'zero':False}]
        receipt={}
        scope={'json':json,'output':lambda args:json.dumps(info if 'info' in args else mapping),
               'disk_evidence':disk_evidence,'storage_receipt':storage_receipt,'disk':disk,'raw':raw,
               'artifacts':self.out,'receipt':receipt}
        with self.assertRaisesRegex(ValueError, 'allocation exceeds'):exec(compile(sequence,'<actual-export-sequence>','exec'),scope)
        self.assertNotIn('storage',receipt)
        for name, wanted in receipt['storage_evidence_sha256'].items():
            self.assertEqual(hashlib.sha256((self.out/name).read_bytes()).hexdigest(),wanted)
        self.assertEqual(json.loads((self.out/'qemu-map.json').read_text()),mapping)
        stat=json.loads((self.out/'disk-file-stat.json').read_text())
        self.assertEqual(stat['files']['qcow2']['file_bytes'],len(b'owned-disk'))
        self.assertFalse(stat['decoder_rss_measured'])
        # Existing resource limits and exporter failure cleanup remain enforced.
        self.assertIn('disk.unlink(missing_ok=True)',source)
        self.assertIn('storage_receipt(info, mapping, disk.stat().st_size)',sequence)
        old_sequence='\n'.join(line for line in sequence.splitlines() if "receipt['storage_evidence_sha256']" not in line)
        scope['artifacts']=self.out/'old';scope['artifacts'].mkdir();scope['receipt']={}
        with self.assertRaisesRegex(ValueError, 'allocation exceeds'):exec(compile(old_sequence,'<old-no-persist-sequence>','exec'),scope)
        self.assertEqual(list(scope['artifacts'].iterdir()),[])
        self.assertNotIn('storage_evidence_sha256',scope['receipt'])

if __name__ == '__main__': unittest.main()
