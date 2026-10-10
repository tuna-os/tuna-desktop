#!/usr/bin/python3
"""Owned-resource/error/byte-identity tests; no real privileged image build."""
import json
from pathlib import Path
import shutil
import tempfile
import unittest
from tuna_benchmark_disk import digest
from tuna_benchmark_esp import critical_hashes, fat_partition, repair


class FakeCommands:
    def __init__(self, raw, failure=None, versions='6.1-test', target_count=1, mismatch=False, foreign=False):
        self.raw=raw; self.failure=failure; self.versions=versions
        self.target_count=target_count; self.mismatch=mismatch; self.foreign=foreign
        self.calls=[]; self.mounted=False
    def call(self,args,check=True):
        self.calls.append(args)
        operation=args[1] if args[0]=='sudo' else args[0]
        if operation=='mount':
            self.mounted=True
            root=Path(args[-1])/'EFI/Linux';root.mkdir(parents=True)
            for i in range(self.target_count):
                target=root/f'bootc_composefs-{i}';target.mkdir()
                (target/'vmlinuz').write_bytes(bytes(20));(target/'initrd').write_bytes(bytes(30))
        if self.failure==operation or (self.failure=='copy-esp' and operation=='cp') or (self.failure=='detach' and '--detach' in args):
            raise RuntimeError('injected '+operation+' failure')
        if operation=='podman' and args[2]=='run':
            if 'printf' in args[-1]: return 0,self.versions
        if operation=='podman' and args[2]=='cp':
            name=args[3].rsplit('/',1)[1]
            Path(args[4]).write_bytes(b'kernel-real' if name=='vmlinuz' else b'initramfs-real')
        if operation=='losetup':
            if '--find' in args:return 0,'/dev/loop77'
            if '--json' in args:return 0,json.dumps({'loopdevices':[{'name':'/dev/loop77','back-file':str(self.raw) if not self.foreign else '/foreign/backing.raw'}]})
        if operation=='lsblk':return 0,json.dumps({'blockdevices':[{'path':'/dev/loop77','type':'loop','children':[{'path':'/dev/loop77p1','type':'part','fstype':'vfat'},{'path':'/dev/loop77p2','type':'part','fstype':'ext4'}]}]})
        if operation=='cp' and not self.mismatch:shutil.copyfile(args[-2],args[-1])
        if operation=='mountpoint':return (0 if self.mounted else 32),''
        if operation=='umount':self.mounted=False
        return 0,''


class EspTests(unittest.TestCase):
    def setUp(self):
        self.temporary=tempfile.TemporaryDirectory(prefix='tuna-esp-test-');self.addCleanup(self.temporary.cleanup)
        self.root=Path(self.temporary.name);self.raw=self.root/'disk.raw';self.raw.write_bytes(b'owned')
        self.critical=self.root/'critical.sha256'
        import hashlib
        self.critical.write_text(''.join(hashlib.sha256(data).hexdigest()+'  /usr/lib/modules/6.1-test/'+name+'\n' for name,data in [('vmlinuz',b'kernel-real'),('initramfs.img',b'initramfs-real')]))
        self.record={}
    def invoke(self,**kwargs):
        command=FakeCommands(self.raw,**kwargs)
        self.command=command
        repair(self.raw,'a'*64,self.root,self.critical,self.record,self.root/'repair.log',command)
        return command
    def test_replaces_zero_files_with_protected_exact_source_bytes_and_cleans(self):
        command=self.invoke()
        self.assertTrue(self.record['repaired']);self.assertTrue(self.record['owned_resources_removed'])
        self.assertFalse((self.root/'esp-repair').exists())
        self.assertTrue(any('--detach' in args for args in command.calls))
        self.assertTrue(any('lsblk' in args and '--tree' in args for args in command.calls))
        self.assertTrue(any('rm' in args for args in command.calls))
        for member in self.record['members'].values():
            self.assertEqual(member['source_sha256'],member['esp_after_sha256'])
            self.assertNotEqual(member['source_sha256'],member['esp_before_sha256'])
        self.assertTrue(self.raw.exists())
    def test_missing_or_multiple_kernel_versions_refused_and_container_cleaned(self):
        for versions in ('','6.1-test\n6.2-other'):
            with self.subTest(versions=versions),self.assertRaises(ValueError):self.invoke(versions=versions)
            self.assertTrue(self.record['owned_resources_removed'])
    def test_source_baseline_mismatch_refused_before_loop(self):
        self.critical.write_text(('0'*64)+self.critical.read_text()[64:])
        with self.assertRaises(ValueError):self.invoke()
        self.assertTrue(self.record['owned_resources_removed'])
        self.assertFalse(any('losetup' in args for args in self.command.calls))
    def test_unique_fat_and_owned_partition_required(self):
        base={'blockdevices':[{'path':'/dev/loop77','type':'loop','children':[]}]}
        for children in [[],[{'path':'/dev/other','type':'part','fstype':'vfat'}],
                         [{'path':f'/dev/loop77p{i}','type':'part','fstype':'vfat'} for i in (1,2)]]:
            with self.subTest(children=children),self.assertRaises(ValueError):
                fat_partition({'blockdevices':[base['blockdevices'][0]|{'children':children}]},'/dev/loop77')
    def test_missing_or_multiple_composefs_targets_refused_with_mount_loop_cleanup(self):
        for count in (0,2):
            with self.subTest(count=count),self.assertRaises(ValueError):self.invoke(target_count=count)
            self.assertTrue(self.record['owned_resources_removed'])
    def test_install_copy_hash_mismatch_rejected_and_all_resources_cleaned(self):
        with self.assertRaises(ValueError):self.invoke(mismatch=True)
        self.assertFalse(self.record['repaired']);self.assertTrue(self.record['owned_resources_removed'])
    def test_mount_copy_sync_failures_always_clean_known_resources(self):
        for failure in ('mount','copy-esp','sync'):
            with self.subTest(failure=failure),self.assertRaises(RuntimeError):self.invoke(failure=failure)
            self.assertTrue(self.record['owned_resources_removed'])
    def test_cleanup_failure_fatal_and_retains_unsafe_backing(self):
        with self.assertRaisesRegex(RuntimeError,'owned ESP cleanup failed'):self.invoke(failure='umount')
        self.assertFalse(self.record['owned_resources_removed']);self.assertTrue(self.raw.exists())
        self.assertTrue(self.record['cleanup_errors'])
    def test_loop_detach_failure_fatal_retains_raw(self):
        with self.assertRaisesRegex(RuntimeError,'owned ESP cleanup failed'):self.invoke(failure='detach')
        self.assertFalse(self.record['owned_resources_removed']);self.assertTrue(self.raw.exists())
    def test_foreign_loop_backing_refused_without_detach(self):
        with self.assertRaisesRegex(RuntimeError,'owned ESP cleanup failed'):self.invoke(foreign=True)
        self.assertFalse(any('--detach' in args for args in self.command.calls))
        self.assertFalse(self.record['owned_resources_removed']);self.assertTrue(self.raw.exists())
    def test_missing_critical_file_still_cleans_owned_scratch(self):
        self.critical.unlink()
        with self.assertRaises(FileNotFoundError):self.invoke()
        self.assertTrue(self.record['owned_resources_removed'])
        self.assertFalse((self.root/'esp-repair').exists())
    def test_protected_missing_duplicate_or_malformed_hashes_refused(self):
        text=self.critical.read_text()
        for invalid in ('',text+text,text.replace('  /usr',' */usr')):
            with self.subTest(invalid=invalid),self.assertRaises(ValueError):critical_hashes(invalid,'6.1-test')
    def test_nonimmutable_image_identity_refused(self):
        with self.assertRaises(ValueError):repair(self.raw,'localhost/mutable:tag',self.root,self.critical,self.record,self.root/'log')
    def test_export_repairs_before_raw_hash_and_conversion_and_refuses_unsafe_unlink(self):
        source=Path(__file__).resolve().parents[1].joinpath('tuna-benchmark-disk-export').read_text()
        self.assertLess(source.index('repair_esp(raw,'),source.index("receipt['raw_sha256'] = digest(raw)"))
        self.assertLess(source.index("receipt['raw_sha256'] = digest(raw)"),source.index("'disk-convert'"))
        self.assertIn("get('owned_resources_removed', True)",source)


if __name__=='__main__':unittest.main()
