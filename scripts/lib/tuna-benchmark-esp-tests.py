#!/usr/bin/python3
"""Owned-resource/error/byte-identity tests; no real privileged image build."""
import json
import os
import subprocess
from pathlib import Path
import shutil
import tempfile
import unittest
from tuna_benchmark_disk import digest
from tuna_benchmark_esp import Commands, critical_hashes, fat_partition, repair, retire_backing


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
        if operation=='podman' and args[2]=='container':return 1,''
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
        self.assertLess(source.index("receipt['raw_sha256'] = digest(raw)"),source.index("'disk-convert')"))
        self.assertIn("get('owned_resources_removed', True)",source)


class DeadlineTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='tuna-deadline-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.commands = Commands(self.root/'commands.log')

    def test_success_and_nonzero_status_are_reaped(self):
        import sys
        self.assertEqual(self.commands.call([sys.executable,'-c','print("receipt")']), (0,'receipt'))
        self.assertTrue(self.commands.events[-1]['leader_reaped'])
        with self.assertRaisesRegex(RuntimeError,'command failed'):
            self.commands.call([sys.executable,'-c','raise SystemExit(7)'])

    def test_real_timeout_kills_owned_parent_and_child(self):
        import os, sys, time
        pidfile = self.root/'child.pid'
        program = ('import os,signal,time,pathlib; '
                   'signal.signal(signal.SIGTERM,signal.SIG_IGN); '
                   'child=os.fork(); '
                   f'pathlib.Path({str(pidfile)!r}).write_text(str(os.getpid())) if child==0 else None; '
                   'time.sleep(30)')
        started = time.monotonic()
        with self.assertRaisesRegex(RuntimeError,'deadline exceeded'):
            self.commands.call([sys.executable,'-c',program],timeout=1)
        self.assertLess(time.monotonic()-started,9)
        self.assertTrue(self.commands.events[-1]['leader_reaped'])
        child = int(pidfile.read_text())
        path = Path('/proc')/str(child)/'stat'
        deadline = time.monotonic()+1
        def stopped():
            try: return path.read_text().split()[2] == 'Z'
            except FileNotFoundError: return True
        while not stopped() and time.monotonic()<deadline:
            time.sleep(.02)
        self.assertTrue(stopped())

    def test_privileged_timeout_reaps_owned_sudo_group(self):
        import shutil, subprocess, time
        if not shutil.which('sudo'):
            self.skipTest('sudo unavailable')
        probe = subprocess.run(['sudo','-n','true'], timeout=3, capture_output=True)
        if probe.returncode:
            self.skipTest('noninteractive sudo unavailable')
        pidfile = self.root/'root-child.pid'
        program = ('import os,signal,time,pathlib; '
                   'signal.signal(signal.SIGTERM,signal.SIG_IGN); '
                   'child=os.fork(); '
                   f'pathlib.Path({str(pidfile)!r}).write_text(str(os.getpid())) if child==0 else None; '
                   'time.sleep(30)')
        with self.assertRaisesRegex(RuntimeError,'deadline exceeded'):
            self.commands.call(['sudo','-n','/usr/bin/python3','-c',program],timeout=1)
        self.assertFalse(self.commands.process_cleanup_uncertain)
        child = int(pidfile.read_text())
        deadline = time.monotonic()+1
        while True:
            try: state = (Path('/proc')/str(child)/'stat').read_text().split()[2]
            except FileNotFoundError: state = 'Z'
            if state == 'Z' or time.monotonic() >= deadline: break
            time.sleep(.02)
        self.assertEqual(state,'Z')
        self.assertTrue(self.commands.events[-1]['leader_reaped'])

    def test_run_name_is_recorded_before_process_launch(self):
        from unittest.mock import patch
        class Process:
            pid = 99999999
            returncode = 0
            def poll(self):return 0
            def wait(self,timeout):return 0
        def launch(args, **kwargs):
            self.assertIn('--name',args)
            self.assertIn(args[args.index('--name')+1], self.commands.containers)
            return Process()
        with patch('tuna_benchmark_esp.subprocess.Popen',side_effect=launch), patch.object(self.commands,'_signal'):
            self.commands.call(['sudo','podman','run','--rm','sha256:'+'a'*64,'true'])
        self.assertEqual(len(self.commands.containers),1)

    def test_output_bound_terminates_real_writer(self):
        import sys
        with self.assertRaisesRegex(RuntimeError,'output bound exceeded'):
            self.commands.call([sys.executable,'-c','import os,time; os.write(1,b"x"*8192); time.sleep(30)'],
                               timeout=2,max_output=1024)
        self.assertLess((self.root/'commands.log').stat().st_size,2048)
        self.assertTrue(self.commands.events[-1]['leader_reaped'])

    def test_unconfirmed_process_cleanup_is_recorded_and_fatal(self):
        import sys
        from unittest.mock import patch
        with patch.object(self.commands,'_verify_group_stopped',side_effect=RuntimeError('injected uncertain group')):
            with self.assertRaisesRegex(RuntimeError,'injected uncertain group'):
                self.commands.call([sys.executable,'-c','print("done")'])
        self.assertTrue(self.commands.process_cleanup_uncertain)
        self.assertFalse(self.commands.events[-1]['group_execution_stopped'])
        self.assertIn('cleanup_error',self.commands.events[-1])

    def test_cleanup_tries_every_named_container_and_checks_absence(self):
        commands = self.commands
        commands.containers = ['tuna-owned-one','tuna-owned-two']
        calls = []
        def fake(args, **kwargs):
            calls.append(args)
            if args[-1]=='tuna-owned-one': raise RuntimeError('injected cleanup deadline')
            return (1,'') if 'exists' in args else (0,'')
        commands.call = fake
        with self.assertRaisesRegex(RuntimeError,'owned run cleanup failed'):
            commands.cleanup_containers()
        self.assertTrue(any(args[-1]=='tuna-owned-two' and 'exists' in args for args in calls))

    def test_backing_cleanup_refuses_mounted_or_replaced_loop(self):
        class Fake:
            def __init__(self, mounted, replaced):self.calls=[];self.mounted=mounted;self.replaced=replaced
            def call(self,args):
                self.calls.append(args)
                if 'lsblk' in args:
                    return 0,json.dumps({'blockdevices':[{'mountpoints':['/owned/mount'] if self.mounted else []}]})
                return 0,json.dumps({'loopdevices':[{'name':'/dev/loop77','back-file':'/foreign' if self.replaced and args[-1]=='/dev/loop77' else '/owned/disk.raw'}]})
        for mounted,replaced in [(True,False),(False,True)]:
            fake=Fake(mounted,replaced)
            with self.assertRaises(ValueError):retire_backing(fake,Path('/owned/disk.raw'))
            self.assertFalse(any('--detach' in args for args in fake.calls))

    def test_exporter_uses_finite_phases_and_retains_uncertain_backing(self):
        source=Path(__file__).resolve().parents[1].joinpath('tuna-benchmark-disk-export').read_text()
        self.assertNotIn('subprocess.run',source)
        self.assertNotIn('subprocess.check_output',source)
        self.assertIn('commands.interrupted_build',source)
        self.assertIn('commands.process_cleanup_uncertain',source)
        self.assertLess(source.index('retire_backing(commands,'),source.index('raw.unlink('))


class BaselineCriticalProducerTests(unittest.TestCase):
    # Execute each actual producer block with owned paths and a private pacman
    # listing. No image, package install, privileged mount or boot occurs.
    def producers(self):
        source = Path(__file__).resolve().parents[2].joinpath(
            'packaging/marlin/perf/Containerfile.baseline').read_text()
        parts = source.split('pacman -Qlq ')[1:]
        self.assertEqual(len(parts), 2, 'stock and diagnostic producers required')
        return [('pacman -Qlq ' + part.split(
            '    test -s /usr/share/tuna-perf-baseline-critical.sha256', 1)[0])
            .replace('\\\n', '').rstrip().removesuffix('&&').rstrip() for part in parts]

    def execute(self, producer):
        with tempfile.TemporaryDirectory(prefix='tuna-critical-producer-') as temporary:
            root = Path(temporary)
            modules = root / 'modules'
            kernel = modules / '7.2.9-arch1-1'
            kernel.mkdir(parents=True)
            members = {kernel / 'vmlinuz': b'owned real kernel bytes',
                       kernel / 'initramfs.img': b'owned initramfs bytes',
                       kernel / 'modules.dep': b'owned module dependency bytes',
                       root / 'mutter': b'owned Mutter bytes'}
            for path, content in members.items():
                path.write_bytes(content)
            listing = root / 'package-files'
            listing.write_text(str(modules) + '/\n' + str(kernel / 'vmlinuz') + '\n'
                               + str(root / 'mutter') + '\n')
            binary = root / 'bin'
            binary.mkdir()
            pacman = binary / 'pacman'
            pacman.write_text('#!/bin/sh\ncat "$TUNA_TEST_PACKAGED_FILES"\n')
            pacman.chmod(0o700)
            critical = root / 'critical.sha256'
            body = producer.replace('/usr/lib/modules', str(modules)).replace(
                '/tmp/tuna-baseline-critical-files.txt', str(root / 'critical-files')).replace(
                '/usr/share/tuna-perf-baseline-critical.sha256', str(critical))
            result = subprocess.run(['sh', '-c', 'set -eu; ' + body],
                                    env={**os.environ, 'PATH': str(binary) + os.pathsep + os.environ['PATH'],
                                         'TUNA_TEST_PACKAGED_FILES': str(listing)},
                                    capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr)
            rows = [line.split('  ', 1) for line in critical.read_text().splitlines()]
            for sha, path in rows:
                self.assertEqual(sha, digest(path), 'every recorded byte hash must remain exact')
            self.assertEqual(set(path for _, path in rows), set(map(str, members)))
            # Only translate this owned fixture module prefix for the unchanged
            # production validator; it expects the installed /usr/lib layout.
            text = critical.read_text().replace(str(modules), '/usr/lib/modules')
            return rows, text

    def test_stock_and_diagnostic_producers_emit_complete_unique_hash_receipts(self):
        for producer in self.producers():
            with self.subTest(producer=producer.splitlines()[0]):
                rows, text = self.execute(producer)
                self.assertEqual(len(rows), 4, 'each unique source member recorded once')
                self.assertEqual(len(critical_hashes(text, '7.2.9-arch1-1')), 2)

    def test_old_unsorted_path_producers_fail_unchanged_strict_consumer(self):
        dedup = ('    LC_ALL=C sort -u -o /tmp/tuna-baseline-critical-files.txt '
                 '/tmp/tuna-baseline-critical-files.txt && ')
        for producer in self.producers():
            with self.subTest(producer=producer.splitlines()[0]):
                self.assertIn(dedup, producer)
                rows, text = self.execute(producer.replace(dedup, ''))
                self.assertEqual(len(rows), 5, 'old overlap records the kernel twice')
                with self.assertRaisesRegex(ValueError, 'duplicate protected kernel member'):
                    critical_hashes(text, '7.2.9-arch1-1')


if __name__=='__main__':unittest.main()
