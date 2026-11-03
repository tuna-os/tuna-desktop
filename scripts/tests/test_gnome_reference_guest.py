"""Guest helper regressions. Controlled Gio/AT-SPI data is NOT native qualification.

Process/filesystem tests use this machine's real kernel and temporary files;
none claims a stock GDM, GNOME, accessibility or physical-input runtime result.
"""
import base64
import copy
import gzip
import hashlib
import importlib.machinery
import importlib.util
import io
import json
import os
from pathlib import Path
import socket
import stat
import subprocess
import sys
import tempfile
import unittest
from types import SimpleNamespace
from unittest import mock

SOURCE = Path(__file__).resolve().parents[2] / 'packaging/marlin/vm-lane/tuna-vm-gnome-reference'
LOADER = importlib.machinery.SourceFileLoader('gnome_reference_guest', str(SOURCE))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
GUEST = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(GUEST)
NONCE = 'a' * 32


class Arguments(unittest.TestCase):
    def test_all_fixed_operations_and_finite_local_budget(self):
        for op in ('admit', 'prepare', 'observe', 'stop', 'file'):
            value = {'budget_s': 25}
            if op != 'file':
                value['profile'] = 'gnome'
            if op != 'admit':
                value['nonce'] = NONCE
            if op == 'file':
                value['name'] = 'snapshot.json'
            self.assertGreater(GUEST.payload(op, value).remaining(), 0)
        for budget in (0, -1, 25.1, True, '20', float('nan'), float('inf')):
            with self.subTest(budget=budget), self.assertRaises(ValueError):
                GUEST.payload('admit', {'profile': 'gnome', 'budget_s': budget})

    def test_no_override_exec_alias_path_or_nonce_coercion(self):
        for extra in ('command', 'env', 'client_name', 'unsafeEval', 'TUNA_STATE', 'WAYLAND_DEBUG'):
            with self.subTest(extra=extra), self.assertRaises(ValueError):
                GUEST.payload('prepare', {'profile': 'gnome', 'nonce': NONCE, extra: 'anything'})
        for nonce in (None, 123, '', '../' + 'a' * 29, 'A' * 32, 'a' * 31 + '\n', 'a' * 33):
            with self.subTest(nonce=nonce), self.assertRaises(ValueError):
                GUEST.payload('observe', {'profile': 'tuna', 'nonce': nonce})
        for name in ('../claim.json', '/etc/passwd', 'snapshot.json.new', 'unknown.json'):
            with self.subTest(name=name), self.assertRaises(ValueError):
                GUEST.payload('file', {'nonce': NONCE, 'name': name})
        with self.assertRaises(ValueError):
            GUEST.payload('exec', {})
        with self.assertRaises(ValueError):
            GUEST.payload('admit', {'profile': 'gnome-reference'})

    def test_expired_deadline_never_grants_more_time(self):
        deadline = GUEST.Deadline(1)
        deadline.end = 0
        with self.assertRaises(TimeoutError):
            deadline.remaining()

    def test_exact_stock_version_boundary(self):
        for version in ('51.1', '51.1-1', '1:51.1-2', '51.1.1-1'):
            self.assertTrue(GUEST.version_51_1(version))
        for version in ('51.0-1', '51.10-1', '151.1', '51.1junk', ''):
            self.assertFalse(GUEST.version_51_1(version))

    def test_socket_basename_does_not_misclassify_native_tuna_as_nested(self):
        for value in ('wayland-0', 'tuna-nested-12345', 'native_display.sock', 'native:reference', 'native display'):
            self.assertEqual(GUEST.wayland_basename(value), value)
        for value in ('', '.', '..', '../wayland-0', '/run/user/1000/wayland-0', 'a/b', 'a\x00b', None):
            with self.subTest(value=value), self.assertRaises(RuntimeError):
                GUEST.wayland_basename(value)

    def test_chunk_arguments_and_immutable_allowlist_are_bounded(self):
        value = {'nonce': NONCE, 'name': 'observe-000001.json', 'offset': 262144, 'limit': 262144}
        self.assertGreater(GUEST.payload('file', value).remaining(), 0)
        for key, bad in (('offset', -1), ('offset', True), ('offset', 1.5), ('limit', 0), ('limit', 262145), ('limit', True)):
            with self.subTest(key=key, bad=bad), self.assertRaises(ValueError):
                GUEST.payload('file', dict(value, **{key: bad}))
        for name in ('observe-1.json', 'observe-000001.json/extra', '../observe-000001.json', 'exec-000001.json'):
            self.assertFalse(GUEST.allowed_receipt(name))
        with self.assertRaises(RuntimeError):
            GUEST.wire_bytes({'controlled_large_value': 'x' * (GUEST.MAX_WIRE_BYTES + 1)}, 'admit')


class DrmDriverObservation(unittest.TestCase):
    def test_actual_character_device_ioctl_uses_kernel_name_not_bus_driver(self):
        calls = []
        def kernel_version(descriptor, request, version, mutate):
            self.assertTrue(stat.S_ISCHR(os.fstat(descriptor).st_mode))
            self.assertTrue(mutate)
            calls.append(request)
            if version.name:
                GUEST.ctypes.memmove(version.name, b'virtio_gpu', 10)
            version.name_len = 10
            return 0
        with mock.patch.object(GUEST.fcntl, 'ioctl', side_effect=kernel_version):
            self.assertEqual(GUEST.drm_driver(Path('/dev/null')), 'virtio_gpu')
        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[0], calls[1])

    def test_real_non_drm_ioctl_failure_has_no_sysfs_or_bus_fallback(self):
        with self.assertRaises(OSError):
            GUEST.drm_driver(Path('/dev/null'))

    def test_regular_device_substitution_is_rejected_before_ioctl(self):
        with tempfile.NamedTemporaryFile() as file:
            with mock.patch.object(GUEST.fcntl, 'ioctl') as query:
                with self.assertRaisesRegex(RuntimeError, 'original device identity'):
                    GUEST.drm_driver(Path(file.name))
                query.assert_not_called()

    def test_driver_query_bounds_and_changed_length_fail_closed(self):
        for length in (0, 257):
            with self.subTest(length=length):
                def malformed(descriptor, request, version, mutate):
                    version.name_len = length
                with mock.patch.object(GUEST.fcntl, 'ioctl', side_effect=malformed):
                    with self.assertRaisesRegex(RuntimeError, 'length exceeds bound'):
                        GUEST.drm_driver(Path('/dev/null'))
        def changed(descriptor, request, version, mutate):
            version.name_len = 11 if version.name else 10
        with mock.patch.object(GUEST.fcntl, 'ioctl', side_effect=changed):
            with self.assertRaisesRegex(RuntimeError, 'changed during query'):
                GUEST.drm_driver(Path('/dev/null'))


class RealFilesystem(unittest.TestCase):
    def test_metadata_is_exclusive_private_and_not_followed(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'nonce'
            record = GUEST.private_directory(path, os.getuid(), create=True)
            self.assertEqual(record['mode'], 0o700)
            with self.assertRaises(FileExistsError):
                GUEST.private_directory(path, os.getuid(), create=True)
            with self.assertRaises(RuntimeError):
                GUEST.private_directory(path, os.getuid() + 1)
            path.chmod(0o755)
            with self.assertRaises(RuntimeError):
                GUEST.private_directory(path, os.getuid())
            link = Path(temporary) / 'link'
            link.symlink_to(path, target_is_directory=True)
            with self.assertRaises(RuntimeError):
                GUEST.private_directory(link, os.getuid())

    def test_actual_saved_bytes_hash_and_read_after_principal_exit(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            GUEST.save(directory, 'snapshot.json', {'actual': 'bytes', 'nonce': NONCE})
            child = subprocess.Popen(['/usr/bin/true'])
            child.wait(timeout=3)
            actual_read = GUEST.read_bytes
            with mock.patch.object(GUEST, 'read_bytes', side_effect=lambda path: actual_read(path, os.getuid())):
                result = GUEST.retrieve_file(directory, 'snapshot.json')
            actual = (directory / 'snapshot.json').read_bytes()
            self.assertEqual(base64.b64decode(result['base64'], validate=True), actual)
            self.assertEqual(result['sha256'], hashlib.sha256(actual).hexdigest())
            self.assertEqual(result['size'], len(actual))
            self.assertEqual(stat.S_IMODE((directory / 'snapshot.json').stat().st_mode), 0o600)
            self.assertEqual(GUEST.retained_files(directory), ['snapshot.json'])
            with self.assertRaises(ValueError):
                GUEST.retrieve_file(directory, '../snapshot.json')

    def test_large_immutable_response_chunks_preserve_full_bytes_without_qga_overflow(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            response = {'schema': 1, 'profile': 'gnome', 'nonce': NONCE, 'ready': True,
                        'controlled_transport_data': 'x' * (1024 * 1024 + 123)}
            envelope = GUEST.response_envelope(directory, 'observe', response, GUEST.Deadline(5))
            name = envelope['response_file']['name']
            actual = (directory / name).read_bytes()
            self.assertEqual((directory / 'snapshot.json').read_bytes(), actual)
            self.assertEqual(json.loads(actual), response)
            self.assertIn(name, response['files'])
            self.assertNotIn('response_file', response)
            self.assertLess(len(GUEST.wire_bytes(envelope, 'observe')), GUEST.MAX_WIRE_BYTES)
            self.assertEqual(envelope['response_file']['sha256'], hashlib.sha256(actual).hexdigest())
            assembled = bytearray()
            actual_read = GUEST.read_bytes
            with mock.patch.object(GUEST, 'read_bytes', side_effect=lambda path: actual_read(path, os.getuid())):
                while len(assembled) < len(actual):
                    chunk = GUEST.retrieve_file(directory, name, len(assembled), 262144)
                    decoded = base64.b64decode(chunk['base64'], validate=True)
                    self.assertEqual(chunk['length'], len(decoded))
                    self.assertEqual(chunk['size'], len(actual))
                    self.assertEqual(chunk['chunk_sha256'], hashlib.sha256(decoded).hexdigest())
                    self.assertEqual(chunk['sha256'], envelope['response_file']['sha256'])
                    self.assertEqual(chunk['offset'], len(assembled))
                    self.assertLess(len(base64.b64encode(GUEST.wire_bytes(chunk, 'file'))), 1024 * 1024)
                    assembled.extend(decoded)
                    self.assertEqual(chunk['eof'], len(assembled) == len(actual))
                final = GUEST.retrieve_file(directory, name, len(actual), 1)
                self.assertEqual(final['length'], 0)
                self.assertTrue(final['eof'])
                with self.assertRaises(ValueError):
                    GUEST.retrieve_file(directory, name, len(actual) + 1)
            self.assertEqual(bytes(assembled), actual)
            later = GUEST.response_envelope(directory, 'observe',
                {'schema': 1, 'profile': 'gnome', 'nonce': NONCE, 'ready': False},
                GUEST.Deadline(5))
            self.assertNotEqual(later['response_file']['name'], name)
            self.assertEqual((directory / name).read_bytes(), actual)
            self.assertEqual(stat.S_IMODE((directory / name).stat().st_mode), 0o600)

    def test_retained_file_replacement_during_read_cannot_mix_identity_or_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            path = directory / 'snapshot.json'
            GUEST.save(directory, 'snapshot.json', {'original': True})
            actual_read = os.read
            def replace(fd, size):
                data = actual_read(fd, size)
                replacement = directory / 'replacement'
                replacement.write_bytes(b'changed')
                replacement.chmod(0o600)
                replacement.replace(path)
                return data
            with mock.patch.object(GUEST.os, 'read', side_effect=replace):
                with self.assertRaisesRegex(RuntimeError, 'changed'):
                    GUEST.read_bytes(path, os.getuid())

    def test_receipt_rejects_foreign_mode_symlink_hardlink_and_large_bytes(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'snapshot.json'
            path.write_bytes(b'actual')
            path.chmod(0o600)
            self.assertEqual(GUEST.read_bytes(path, os.getuid()), b'actual')
            with self.assertRaises(RuntimeError):
                GUEST.read_bytes(path, os.getuid() + 1)
            path.chmod(0o644)
            with self.assertRaises(RuntimeError):
                GUEST.read_bytes(path, os.getuid())
            path.chmod(0o600)
            alias = Path(temporary) / 'alias'
            alias.symlink_to(path)
            with self.assertRaises(OSError):
                GUEST.read_bytes(alias, os.getuid())
            alias.unlink()
            os.link(path, alias)
            with self.assertRaises(RuntimeError):
                GUEST.read_bytes(path, os.getuid())
            alias.unlink()
            with mock.patch.object(GUEST, 'MAX_BYTES', 4), self.assertRaises(RuntimeError):
                GUEST.read_bytes(path, os.getuid())

    def test_directory_cleanup_refuses_changed_identity_and_unowned_contents(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'owned'
            original = GUEST.private_directory(path, os.getuid(), create=True)
            changed = dict(original, inode=original['inode'] + 1)
            with self.assertRaises(RuntimeError):
                GUEST.remove_directory(changed)
            (path / 'not-created-by-fixture').write_text('preserve')
            with self.assertRaises(OSError):
                GUEST.remove_directory(original)
            self.assertEqual((path / 'not-created-by-fixture').read_text(), 'preserve')
            (path / 'not-created-by-fixture').unlink()
            self.assertTrue(GUEST.remove_directory(original)['removed'])
            self.assertFalse(path.exists())

    def test_socket_credentials_are_actual_kernel_peer(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = str(Path(temporary) / 'bus')
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as server:
                server.bind(path)
                server.listen(1)
                peer = GUEST.check_socket_peer(path, os.getuid(), GUEST.Deadline(3))
                connection, _ = server.accept()
                connection.close()
                self.assertEqual(peer['pid'], os.getpid())
                self.assertEqual(peer['uid'], os.getuid())
                with self.assertRaises(RuntimeError):
                    GUEST.check_socket_peer(path, os.getuid() + 1, GUEST.Deadline(3))


class RealProcess(unittest.TestCase):
    def child(self):
        process = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(30)'],
                                   stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                                   stderr=subprocess.DEVNULL)
        def close():
            if process.poll() is None:
                process.kill()
            process.wait(timeout=3)
        self.addCleanup(close)
        return process

    def test_identity_records_real_elf_and_generation(self):
        child = self.child()
        actual = GUEST.proc_identity(child.pid, os.getuid())
        self.assertEqual(actual['pid'], child.pid)
        self.assertEqual(actual['exe_sha256'], hashlib.sha256(Path(f'/proc/{child.pid}/exe').read_bytes()).hexdigest())
        self.assertEqual(actual['boot_id'], Path('/proc/sys/kernel/random/boot_id').read_text().strip())
        with self.assertRaises(RuntimeError):
            GUEST.proc_identity(child.pid, os.getuid() + 1)
        for key, value in (('start_ticks', actual['start_ticks'] + 1), ('exe_sha256', '0' * 64), ('uid', actual['uid'] + 1), ('boot_id', 'foreign')):
            with self.subTest(key=key), self.assertRaises(RuntimeError):
                GUEST.same_process(dict(actual, **{key: value}), actual)

    def test_session_selector_matches_actual_unfiltered_logind_properties(self):
        listed = subprocess.run(['/usr/bin/loginctl', 'list-sessions', '--no-legend', '--no-pager'],
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                text=True, timeout=3)
        if listed.returncode or not listed.stdout.strip():
            self.skipTest('no actual host logind session; installed native admission remains mandatory')
        expected = {}
        for line in listed.stdout.splitlines():
            sid = line.split()[0]
            raw = subprocess.check_output(['/usr/bin/loginctl', 'show-session', sid,
                                           '--all', '--no-pager'], text=True, timeout=3)
            expected[sid] = GUEST.properties(raw)
        observed = GUEST.sessions(GUEST.Deadline(3))
        self.assertEqual(len(observed), len(expected))
        for session in observed:
            self.assertIn(session.get('Id'), expected)
            actual = expected[session['Id']]
            self.assertEqual({key: session.get(key) for key in ('Id', 'User', 'Type', 'Class', 'Service')},
                             {key: actual.get(key) for key in ('Id', 'User', 'Type', 'Class', 'Service')})

    @unittest.skipUnless(hasattr(os, 'pidfd_open'), 'kernel pidfd interface required')
    def test_pidfd_cleanup_retires_only_actual_owned_child(self):
        child = self.child()
        unrelated = self.child()
        identity = GUEST.proc_identity(child.pid, os.getuid())
        result = GUEST.retire(identity, GUEST.Deadline(3))
        self.assertTrue(result['retired'])
        self.assertEqual(result['method'], 'pidfd')
        child.wait(timeout=3)
        self.assertIsNone(unrelated.poll())

    @unittest.skipUnless(hasattr(os, 'pidfd_open'), 'kernel pidfd interface required')
    def test_stale_or_foreign_identity_never_signals_live_child(self):
        child = self.child()
        identity = GUEST.proc_identity(child.pid, os.getuid())
        for field, value in (('start_ticks', identity['start_ticks'] + 1), ('uid', identity['uid'] + 1), ('exe_sha256', '0' * 64)):
            with self.subTest(field=field), self.assertRaises(RuntimeError):
                GUEST.retire(dict(identity, **{field: value}), GUEST.Deadline(3))
            self.assertIsNone(child.poll())
        deadline = GUEST.Deadline(1)
        deadline.end = 0
        with self.assertRaises(TimeoutError):
            GUEST.retire(identity, deadline)
        self.assertIsNone(child.poll())

    def test_actual_exited_child_is_reported_without_signaling(self):
        child = self.child()
        identity = GUEST.proc_identity(child.pid, os.getuid())
        child.terminate()
        child.wait(timeout=3)
        with mock.patch.object(GUEST.signal, 'pidfd_send_signal') as send:
            self.assertTrue(GUEST.retire(identity, GUEST.Deadline(3))['retired'])
        send.assert_not_called()

    def test_actual_process_without_drm_cannot_be_called_native_gpu(self):
        child = self.child()
        identity = GUEST.proc_identity(child.pid, os.getuid())
        with mock.patch.object(GUEST, 'UID', os.getuid()), self.assertRaisesRegex(RuntimeError, 'native DRM'):
            GUEST.native_gpu(identity, GUEST.Deadline(3))

    def test_original_package_manifest_hash_checked_against_actual_process_elf(self):
        child = self.child()
        identity = GUEST.proc_identity(child.pid, os.getuid())
        with tempfile.TemporaryDirectory() as temporary:
            database = Path(temporary) / 'retained-image-pacman'
            local = database / 'local'
            local.mkdir(parents=True)
            package = local / 'controlled-package-1-1'
            package.mkdir()
            manifest = package / 'mtree'
            for digest in (identity['exe_sha256'], '0' * 64, None):
                fields = '' if digest is None else ' sha256digest=' + digest
                with gzip.open(manifest, 'wt') as stream:
                    stream.write('./' + identity['exe'].lstrip('/') + ' type=file' + fields + '\n')
                with mock.patch.object(GUEST, 'run', side_effect=['controlled-package\n', 'controlled-package 1-1\n', str(database) + '\n']):
                    if digest == identity['exe_sha256']:
                        actual = GUEST.packaged(identity, GUEST.Deadline(3), {})
                        self.assertEqual(actual['package']['manifest_sha256'], digest)
                    else:
                        with self.assertRaises(RuntimeError):
                            GUEST.packaged(identity, GUEST.Deadline(3), {})

    def test_installed_nonstandard_database_path_is_derived_and_cached(self):
        with tempfile.TemporaryDirectory() as temporary:
            database = Path(temporary) / 'sysimage-pacman'
            local = database / 'local'
            local.mkdir(parents=True)
            cache = {}
            with mock.patch.object(GUEST, 'run', return_value=str(database) + '\n') as command:
                self.assertEqual(GUEST.local_package_database(GUEST.Deadline(3), cache), local)
                self.assertEqual(GUEST.local_package_database(GUEST.Deadline(3), cache), local)
            self.assertEqual(command.call_count, 1)
            self.assertEqual(command.call_args.args[0], ['/usr/bin/pacman-conf', 'DBPath'])

    def test_missing_malformed_or_absent_installed_database_never_uses_var_alias(self):
        with tempfile.TemporaryDirectory() as temporary:
            missing = str(Path(temporary) / 'not-created')
            for value in ('', 'relative/database', '/one\n/two\n', missing):
                with self.subTest(value=value), mock.patch.object(GUEST, 'run', return_value=value):
                    with self.assertRaises((RuntimeError, FileNotFoundError)):
                        GUEST.local_package_database(GUEST.Deadline(3), {})


class ControlledImageReceipts(unittest.TestCase):
    def test_packages_require_complete_distinct_actual_entries(self):
        self.assertEqual(GUEST.package_list(b'gdm 51.1-1\nnautilus 51.1-1\n'),
                         {'gdm': '51.1-1', 'nautilus': '51.1-1'})
        for data in (b'', b'gdm\n', b'gdm 51.1-1 extra\n', b'gdm 51.1-1\ngdm 51.1-2\n'):
            with self.subTest(data=data), self.assertRaises(RuntimeError):
                GUEST.package_list(data)

    def test_manifest_duplicate_identical_entries_are_real_layer_format(self):
        digest = 'a' * 64
        row = digest + '  /usr/bin/gdm\n'
        self.assertEqual(GUEST.hash_manifest((row + row).encode()), {'/usr/bin/gdm': digest})
        for data in (b'', b'not-a-hash  /usr/bin/gdm\n',
                     (digest + '  relative\n').encode(),
                     (row + 'b' * 64 + '  /usr/bin/gdm\n').encode()):
            with self.subTest(data=data), self.assertRaises(RuntimeError):
                GUEST.hash_manifest(data)

    def test_controlled_sysfs_requires_real_pci_ancestor_files_and_driver(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            subsystem = root / 'subsystems' / 'pci'
            subsystem.mkdir(parents=True)
            driver = root / 'drivers' / 'virtio-pci'
            driver.mkdir(parents=True)
            pci = root / '0000:00:02.0'
            pci.mkdir()
            (pci / 'subsystem').symlink_to(subsystem)
            (pci / 'driver').symlink_to(driver)
            (pci / 'vendor').write_text('0x1af4\n')
            (pci / 'device').write_text('0x1050\n')
            gpu = pci / 'virtio0'
            gpu.mkdir()
            evidence = GUEST.pci_identity(gpu)
            self.assertEqual(evidence['address'], '0000:00:02.0')
            self.assertEqual(evidence['vendor'], '0x1af4')
            self.assertEqual(evidence['driver'], 'virtio-pci')
            (pci / 'vendor').write_text('invented')
            with self.assertRaises(RuntimeError):
                GUEST.pci_identity(gpu)


class ControlledClaims(unittest.TestCase):
    def claim(self):
        return {'schema': 1, 'nonce': NONCE, 'profile': 'gnome',
                'boot_id': Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
                'fixture': {'directories': [], 'directory_identities': [], 'processes': []}}

    def test_empty_claim_has_no_effects_but_missing_ownership_is_not_accepted(self):
        args = {'nonce': NONCE, 'profile': 'gnome'}
        claim = self.claim()
        self.assertIs(GUEST.validate_claim(claim, args), claim)
        for change in ({'fixture': None}, {'fixture': {}}, {'profile': 'tuna'}, {'nonce': 'b' * 32}, {'boot_id': 'stale'}):
            with self.subTest(change=change), self.assertRaises(RuntimeError):
                GUEST.validate_claim(dict(claim, **change), args)

    def test_foreign_paths_and_malformed_ownership_rejected(self):
        for fixture in ({'directories': ['/tmp/foreign']}, {'processes': [{}]},
                        {'directory_identities': [{'path': '/tmp/foreign', 'uid': 1000, 'mode': 0o700, 'device': 1, 'inode': 1}]},
                        {'processes': 'not-a-list'}):
            claim = self.claim()
            claim['fixture'].update(fixture)
            with self.subTest(fixture=fixture), self.assertRaises(RuntimeError):
                GUEST.validate_claim(claim, {'nonce': NONCE, 'profile': 'gnome'})

    def test_cleanup_surfaces_failure_and_preserves_directories(self):
        claim = self.claim()
        claim['fixture']['processes'] = [{'pid': 321}]
        claim['fixture']['directory_identities'] = [{'path': 'must-not-remove'}]
        with tempfile.TemporaryDirectory() as temporary:
            with mock.patch.object(GUEST, 'retire', side_effect=RuntimeError('identity changed')), mock.patch.object(GUEST, 'remove_directory') as remove:
                result = GUEST.cleanup({'nonce': NONCE, 'profile': 'gnome'}, claim, Path(temporary), GUEST.Deadline(3))
            self.assertFalse(result['ready'])
            self.assertFalse(result['cleaned'])
            self.assertEqual(result['errors'], ['identity changed'])
            remove.assert_not_called()
            self.assertEqual(json.loads((Path(temporary) / 'stop.json').read_text()), result)


class ControlledAdmission(unittest.TestCase):
    """Controlled service/logind replies exercise login routing, not real GDM."""
    def admit(self, started, native=True, integrity=True):
        boot = 'controlled-boot'
        def identity(pid, uid, exe, package):
            return {'pid': pid, 'uid': uid, 'start_ticks': pid, 'exe': exe,
                    'exe_sha256': 'a' * 64, 'boot_id': boot,
                    'package': {'name': package, 'version': '51.1-1'}}
        gdm = identity(10, 0, '/usr/bin/gdm', 'gdm')
        greeter = identity(20, 120, '/usr/bin/gnome-shell', 'gnome-shell')
        shell = identity(30, 1000, '/usr/bin/gnome-shell', 'gnome-shell')
        bus = identity(40, 1000, '/usr/bin/dbus-broker', 'dbus-broker')
        current = {'Id': 'c2', 'User': '1000', 'Name': 'tuna-test', 'Class': 'user',
                   'Service': 'gdm-password', 'Type': 'wayland', 'Active': 'yes',
                   'Remote': 'no', 'Seat': 'seat0', 'State': 'active', 'VTNr': '2'}
        login = {'Id': 'c1', 'User': '120', 'Name': 'gdm', 'Class': 'greeter',
                 'Service': 'gdm-launch-environment', 'Type': 'wayland', 'Active': 'yes',
                 'Remote': 'no', 'Seat': 'seat0', 'State': 'active', 'VTNr': '2'}
        def candidates(paths, uid, deadline):
            if uid == 120:
                return [greeter]
            if '/usr/bin/gnome-shell' in paths:
                return [shell] if native else []
            return []
        def path(value):
            if value == '/sys/class/tty/tty0/active':
                return SimpleNamespace(read_text=lambda: 'tty2\n')
            return Path(value)
        with mock.patch.object(GUEST, 'sessions', return_value=[current] if started else [login]), \
             mock.patch.object(GUEST, 'payload_integrity', return_value={'ready': integrity}), \
             mock.patch.object(GUEST, 'run', return_value='MainPID=10\nActiveState=active\nSubState=running\n'), \
             mock.patch.object(GUEST, 'proc_identity', side_effect=lambda pid, uid: gdm), \
             mock.patch.object(GUEST, 'packaged', side_effect=lambda value, deadline, cache: value), \
             mock.patch.object(GUEST, 'process_candidates', side_effect=candidates), \
             mock.patch.object(GUEST, 'session_environment', return_value={'WAYLAND_DISPLAY': 'wayland-0'}), \
             mock.patch.object(GUEST, 'check_socket_peer', return_value=bus), \
             mock.patch.object(GUEST, 'native_gpu', return_value={'ready': True}), \
             mock.patch.object(GUEST, 'Path', side_effect=path):
            return GUEST.admission('gnome', GUEST.Deadline(3), {})[0]

    def test_greeter_ready_is_separate_from_not_logged_in_session(self):
        actual = self.admit(False)
        self.assertTrue(actual['gdm']['ready'])
        self.assertFalse(actual['session']['started'])
        self.assertFalse(actual['session']['ready'])
        self.assertFalse(actual['ready'])

    def test_logged_in_session_never_offers_gdm_credential_entry(self):
        actual = self.admit(True)
        self.assertTrue(actual['gdm']['available'])
        self.assertFalse(actual['gdm']['ready'])
        self.assertTrue(actual['session']['started'])
        self.assertTrue(actual['session']['ready'])
        self.assertTrue(actual['ready'])

    def test_started_but_rejected_native_session_must_not_retype_password(self):
        actual = self.admit(True, native=False)
        self.assertTrue(actual['session']['started'])
        self.assertFalse(actual['session']['ready'])
        self.assertFalse(actual['gdm']['ready'])
        self.assertFalse(actual['ready'])
        self.assertTrue(actual['missing'])

    def test_missing_image_integrity_never_becomes_default_ready(self):
        actual = self.admit(True, integrity=False)
        self.assertFalse(actual['ready'])
        self.assertFalse(actual['gdm']['ready'])


class ControlledGio(unittest.TestCase):
    """A read-only controlled Gio API, not an installed schema/runtime claim."""
    def api(self, missing_schema=None, missing_key=None):
        read_keys = []
        definitions = {}
        class Definition:
            def __init__(self, name):
                self.name = name
            def has_key(self, key):
                return (self.name, key) != missing_key
        class Source:
            @staticmethod
            def get_default():
                return Source()
            def lookup(self, name, recursive):
                if name == missing_schema:
                    return None
                definitions[name] = Definition(name)
                return definitions[name]
        class Settings:
            @staticmethod
            def new_full(definition, path, backend):
                def get_value(key):
                    read_keys.append((definition.name, key))
                    if definition.name.endswith('wm.keybindings'):
                        value = ['<Alt>Tab'] if key == 'switch-applications' else ['<Super>F12', '<Alt>F11']
                    elif key == 'current-workspace-only':
                        value = definition.name.endswith('window-switcher')
                    elif key == 'sources':
                        value = [('xkb', 'us'), ('xkb', 'de')]
                    elif key == 'text-scaling-factor':
                        value = 1.25
                    else:
                        value = 'controlled:' + key
                    return SimpleNamespace(unpack=lambda: value)
                return SimpleNamespace(get_value=get_value)
        return SimpleNamespace(SettingsSchemaSource=Source, Settings=Settings), read_keys

    def test_all_six_bindings_backward_group_and_actual_values_are_read(self):
        api, reads = self.api()
        result = GUEST.read_gio_settings(api)
        self.assertEqual(set(result['bindings']), set(GUEST.BINDINGS))
        self.assertEqual(result['bindings']['switch-applications'], ['<Alt>Tab'])
        self.assertEqual(result['bindings']['switch-group-backward'], ['<Super>F12', '<Alt>F11'])
        self.assertFalse(result['app_switcher']['current-workspace-only'])
        self.assertTrue(result['window_switcher']['current-workspace-only'])
        self.assertEqual(result['interface']['text-scaling-factor'], 1.25)
        self.assertEqual(result['input_sources']['sources'], [('xkb', 'us'), ('xkb', 'de')])
        self.assertIn(('org.gnome.desktop.input-sources', 'current'), reads)
        self.assertEqual(len(reads), len(set(reads)))

    def test_missing_schema_or_key_has_no_invented_default(self):
        for schema, key in (('org.gnome.shell.app-switcher', None), (None, ('org.gnome.desktop.wm.keybindings', 'switch-group-backward'))):
            api, _ = self.api(missing_schema=schema, missing_key=key)
            with self.assertRaisesRegex(RuntimeError, 'schema/key missing'):
                GUEST.read_gio_settings(api)


class ControlledIdentityBoundary(unittest.TestCase):
    """Controlled root/child transport semantics; not native qualification."""
    user_bus = 'unix:path=/run/user/1000/bus'
    a11y_bus = 'unix:path=/run/user/1000/at-spi/bus_0'

    def principal(self, pid=42):
        return {'pid': pid, 'uid': 1000, 'start_ticks': 123, 'exe': '/usr/bin/dbus-broker',
                'exe_sha256': 'a' * 64, 'boot_id': 'controlled-boot'}

    def observer(self):
        value = GUEST.IdentityObserver.__new__(GUEST.IdentityObserver)
        value.deadline = GUEST.Deadline(3)
        value.allowed = {self.user_bus, self.a11y_bus}
        value.requests = 0
        value.connections = {self.user_bus: object(), self.a11y_bus: object()}
        value.principals = {}
        value.pinned = {address: self.principal() for address in value.allowed}
        value.GLib = SimpleNamespace(Variant=lambda signature, values: values)
        value.a11y_address = self.a11y_bus
        return value

    def test_child_reads_public_objects_but_never_proc_or_privileged_socket_identity(self):
        class Variant:
            def __init__(self, signature, value):
                self.value = value
            def unpack(self):
                return self.value
        connections = [object(), object()]
        credentials = []
        principal = self.principal()
        def identity(channel, deadline, operation, address, name=None, descriptor=None):
            credentials.append((operation, address, name, descriptor))
            return principal if operation == 'socket' else [':1.42', principal]
        # Controlled Gio stream descriptors are transferred, not interpreted by
        # the UID1000 worker. The root-side real-FD test below covers the kernel.
        streams = [SimpleNamespace(get_stream=lambda: SimpleNamespace(
            get_socket=lambda: SimpleNamespace(get_fd=lambda: 91))) for _ in connections]
        def call(connection, bus, path, interface, method, parameters, deadline, Gio):
            if method == 'GetAddress':
                return (self.a11y_bus,)
            if method == 'GetCurrentState':
                return (1, [], [], {})
            if method == 'GetChildren':
                return ([],)
            self.fail('unexpected child DBus identity query: ' + method)
        modules = {'gi': SimpleNamespace(require_version=lambda *args: None),
                   'gi.repository': SimpleNamespace(Gio=object(), GLib=SimpleNamespace(Variant=Variant))}
        with mock.patch.dict(sys.modules, modules), mock.patch.object(GUEST.os, 'geteuid', return_value=1000), \
             mock.patch.object(GUEST, 'bus_connection', side_effect=streams), \
             mock.patch.object(GUEST, 'identity_request', side_effect=identity), \
             mock.patch.object(GUEST, 'dbus_call', side_effect=call), \
             mock.patch.object(GUEST, 'read_gio_settings', return_value={}), \
             mock.patch.object(GUEST, 'proc_identity', side_effect=AssertionError('unprivileged proc read')), \
             mock.patch.object(GUEST, 'check_socket_peer', side_effect=AssertionError('unprivileged identity read')):
            result = GUEST.session_observe({'compositor': principal}, GUEST.Deadline(3), object())
        self.assertEqual(result['atspi']['session_bus_peer'], principal)
        self.assertEqual([entry[0] for entry in credentials], ['socket', 'owner', 'socket', 'owner', 'owner'])
        self.assertEqual([entry[3] for entry in credentials if entry[0] == 'socket'], [91, 91])

    def test_root_reads_actual_transferred_descriptor_not_reported_pid(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = str(Path(temporary) / 'bus')
            other_path = str(Path(temporary) / 'different-bus')
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as server, \
                 socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as other, \
                 socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client, \
                 socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as wrong:
                server.bind(path)
                server.listen(1)
                other.bind(other_path)
                other.listen(1)
                client.connect(path)
                wrong.connect(other_path)
                left, right = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
                try:
                    descriptors = GUEST.array.array('i', [client.fileno()])
                    left.sendmsg([b'actual-fd'], [(socket.SOL_SOCKET, socket.SCM_RIGHTS, descriptors)])
                    raw, ancillary, flags, _ = right.recvmsg(64, socket.CMSG_SPACE(descriptors.itemsize))
                    received = GUEST.array.array('i')
                    received.frombytes(ancillary[0][2])
                    self.assertEqual(raw, b'actual-fd')
                    self.assertEqual(flags, 0)
                    with socket.socket(fileno=received[0]) as transferred, mock.patch.object(GUEST, 'UID', os.getuid()):
                        actual = GUEST.connection_identity(transferred, path)
                        self.assertEqual(actual['pid'], os.getpid())
                        with self.assertRaisesRegex(RuntimeError, 'fixed public bus'):
                            GUEST.connection_identity(wrong, path)
                    observer = self.observer()
                    observer.pinned[self.user_bus] = actual
                    with mock.patch.object(observer, 'connection'), \
                         mock.patch.object(GUEST, 'bus_path', return_value=path), \
                         mock.patch.object(GUEST, 'UID', os.getuid()):
                        value = {'operation': 'socket', 'address': self.user_bus, 'name': None}
                        self.assertEqual(observer.request(value, client.fileno()), actual)
                        with self.assertRaisesRegex(RuntimeError, 'fixed public bus'):
                            observer.request(value, wrong.fileno())
                        with tempfile.TemporaryFile() as regular, self.assertRaises(OSError):
                            observer.request(value, regular.fileno())
                finally:
                    left.close()
                    right.close()

    def test_foreign_kernel_uid_cannot_trigger_proc_identity(self):
        stream = SimpleNamespace(family=socket.AF_UNIX, type=socket.SOCK_STREAM,
            getpeername=lambda: '/run/user/1000/bus',
            getsockopt=lambda *args: GUEST.struct.pack('3i', 42, 999, 999))
        with mock.patch.object(GUEST, 'proc_identity') as query:
            with self.assertRaisesRegex(RuntimeError, 'foreign UID'):
                GUEST.connection_identity(stream, '/run/user/1000/bus')
            query.assert_not_called()

    def test_no_pid_exe_generic_name_address_or_unbounded_request_authority(self):
        observer = self.observer()
        valid = {'operation': 'owner', 'address': self.user_bus, 'name': 'org.a11y.Bus'}
        invalid = [dict(valid, pid=42), dict(valid, exe='/usr/bin/dbus-broker'),
                   dict(valid, operation='exec'), dict(valid, address='unix:path=/tmp/bus'),
                   dict(valid, address='unix:path=/run/user/1000/other'),
                   dict(valid, name='org.freedesktop.systemd1'),
                   dict(valid, name=':1.42'), dict(valid, address=self.a11y_bus, name='org.arbitrary.Service')]
        with mock.patch.object(observer, 'owner') as owner, mock.patch.object(observer, 'connection') as connect:
            for value in invalid:
                with self.subTest(value=value), self.assertRaises(RuntimeError):
                    observer.request(value)
            owner.assert_not_called()
            connect.assert_not_called()
            observer.requests = 12010
            with self.assertRaisesRegex(RuntimeError, 'bound'):
                observer.request(valid)
            observer.requests = 0
            observer.deadline.end = 0
            with self.assertRaises(TimeoutError):
                observer.request(valid)
            owner.assert_not_called()

    def test_owner_is_root_bus_query_and_full_generation_is_checked_before_after(self):
        for changed in (None, 'owner', 'pid', 'uid', 'generation'):
            observer = self.observer()
            principal = self.principal()
            names = iter([':1.42', ':1.43' if changed == 'owner' else ':1.42'])
            pids = iter([42, 43 if changed == 'pid' else 42])
            def call(connection, bus, path, interface, method, parameters):
                return ({'GetNameOwner': lambda: next(names),
                         'GetConnectionUnixProcessID': lambda: next(pids),
                         'GetConnectionUnixUser': lambda: 999 if changed == 'uid' else 1000}[method](),)
            identities = [principal, dict(principal, start_ticks=124) if changed == 'generation' else principal]
            with self.subTest(changed=changed), mock.patch.object(observer, 'connection', return_value=object()), \
                 mock.patch.object(observer, 'call', side_effect=call), \
                 mock.patch.object(GUEST, 'proc_identity', side_effect=identities) as query:
                if changed:
                    with self.assertRaises(RuntimeError):
                        observer.owner(self.a11y_bus, ':1.42')
                else:
                    self.assertEqual(observer.owner(self.a11y_bus, ':1.42'), (':1.42', principal))
                    self.assertEqual(query.call_args_list, [mock.call(42, 1000), mock.call(42, 1000)])

    def test_registry_membership_refreshes_for_later_fixture_apps_without_arbitrary_name_fallback(self):
        observer = self.observer()
        principal = self.principal()
        value = {'operation': 'owner', 'address': self.a11y_bus, 'name': ':1.99'}
        with mock.patch.object(observer, 'owner', return_value=(':1.42', principal)) as owner, \
             mock.patch.object(observer, 'call', side_effect=[([],), ([(':1.99', '/actual/root')],)]):
            with self.assertRaisesRegex(RuntimeError, 'non-observation principal'):
                observer.request(value)
            self.assertEqual(observer.request(value), [':1.42', principal])
            self.assertEqual(owner.call_args_list[-1], mock.call(self.a11y_bus, ':1.99'))

    def test_bus_connection_authenticates_as_session_uid_and_restores_root(self):
        observer = self.observer()
        fresh = 'unix:path=/run/user/1000/at-spi/bus_9'
        observer.allowed.add(fresh)
        observer.Gio = object()
        observer.socket_identities = {address: None for address in observer.allowed}
        principal = self.principal()
        lstat = SimpleNamespace(st_dev=1, st_ino=2, st_uid=1000, st_mode=0o100755)
        patches = lambda: (mock.patch.object(GUEST.os, 'seteuid'),
                            mock.patch.object(GUEST, 'check_socket_peer', return_value=principal),
                            mock.patch.object(GUEST, 'connection_identity', return_value=principal),
                            mock.patch.object(GUEST, 'same_process', return_value=None),
                            mock.patch.object(GUEST.Path, 'lstat', return_value=lstat),
                            mock.patch.object(GUEST.os, 'dup', return_value=99),
                            mock.patch.object(GUEST.socket, 'socket'))
        connection = mock.Mock()
        entered = patches()
        with entered[0] as euid, entered[1], entered[2], entered[3], entered[4], entered[5], entered[6], \
             mock.patch.object(GUEST, 'bus_connection', return_value=connection) as dial:
            self.assertIs(observer.connection(fresh), connection)
        self.assertEqual(dial.call_count, 1)
        self.assertEqual(euid.call_args_list, [mock.call(1000), mock.call(0)])
        # Root is restored even when the handshake itself fails.
        observer.connections.pop(fresh)
        entered = patches()
        with entered[0] as euid, entered[1], entered[2], entered[3], entered[4], entered[5], entered[6], \
             mock.patch.object(GUEST, 'bus_connection', side_effect=RuntimeError('handshake broke')):
            with self.assertRaisesRegex(RuntimeError, 'handshake broke'):
                observer.connection(fresh)
        self.assertEqual(euid.call_args_list, [mock.call(1000), mock.call(0)])

    def test_per_message_kernel_credentials_bind_actual_spawned_child_not_socketpair_creator(self):
        controller, child_channel = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
        controller.setsockopt(socket.SOL_SOCKET, socket.SO_PASSCRED, 1)
        controller.settimeout(3)
        code = ('import socket,sys; s=socket.socket(fileno=int(sys.argv[1])); '
                's.sendall(b\'{"operation":"owner","address":"unix:path=/run/user/1000/bus",'
                '"name":"org.a11y.Bus"}\'); s.recv(65536)')
        child = subprocess.Popen([sys.executable, '-c', code, str(child_channel.fileno())],
                                 pass_fds=(child_channel.fileno(),), stdin=subprocess.DEVNULL)
        child_channel.close()
        try:
            principal = GUEST.proc_identity(child.pid, os.getuid())
            observer = SimpleNamespace(request=mock.Mock(return_value=[':1.42', self.principal()]))
            with mock.patch.object(GUEST, 'UID', os.getuid()):
                self.assertTrue(GUEST.dispatch_identity(controller, observer, principal, os.getgid(), GUEST.Deadline(3)))
            child.wait(timeout=3)
            self.assertEqual(child.returncode, 0)
            observer.request.assert_called_once_with(
                {'operation': 'owner', 'address': self.user_bus, 'name': 'org.a11y.Bus'}, None)
        finally:
            controller.close()
            if child.poll() is None:
                child.kill()
            child.wait(timeout=3)

    def test_truncated_forged_and_excessive_ancillary_packets_close_every_received_fd(self):
        principal = self.principal(os.getpid())
        for failure in ('truncated', 'control-truncated', 'sender', 'missing-credentials',
                        'missing-fd', 'extra-fd', 'owner-fd'):
            with self.subTest(failure=failure), tempfile.TemporaryFile() as file:
                descriptors = [os.dup(file.fileno()) for _ in range(2 if failure == 'extra-fd' else 1)]
                if failure == 'missing-fd':
                    for descriptor in descriptors:
                        os.close(descriptor)
                    descriptors = []
                credentials = GUEST.struct.pack('3i',
                    principal['pid'] + (1 if failure == 'sender' else 0), 1000, 1000)
                ancillary = [] if failure == 'missing-credentials' else [
                    (socket.SOL_SOCKET, socket.SCM_CREDENTIALS, credentials)]
                if descriptors:
                    ancillary.append((socket.SOL_SOCKET, socket.SCM_RIGHTS, GUEST.array.array('i', descriptors).tobytes()))
                operation = 'owner' if failure == 'owner-fd' else 'socket'
                raw = json.dumps({'operation': operation, 'address': self.user_bus, 'name': None}).encode()
                flags = {'truncated': socket.MSG_TRUNC, 'control-truncated': socket.MSG_CTRUNC}.get(failure, 0)
                channel = SimpleNamespace(recvmsg=lambda *args: (raw, ancillary, flags, None))
                observer = SimpleNamespace(request=mock.Mock())
                with mock.patch.object(GUEST, 'proc_identity', return_value=principal):
                    with self.assertRaises(RuntimeError):
                        GUEST.dispatch_identity(channel, observer, principal, 1000, GUEST.Deadline(3))
                observer.request.assert_not_called()
                for descriptor in descriptors:
                    with self.assertRaises(OSError):
                        os.fstat(descriptor)

    def test_root_rejects_unobserved_or_changed_child_process_receipts(self):
        observer = self.observer()
        principal = self.principal()
        observer.principals = {42: principal}
        observation = {'trees': [], 'atspi': dict.fromkeys(
            ('registry', 'session_bus_peer', 'bus_peer', 'launcher'), principal)}
        observation['atspi']['peers'] = [principal]
        with mock.patch.object(observer, 'connection'), mock.patch.object(GUEST, 'proc_identity', return_value=principal):
            observer.verify(observation)
            for changed in (dict(principal, pid=43), dict(principal, exe_sha256='b' * 64)):
                observation['atspi']['registry'] = changed
                with self.assertRaises(RuntimeError):
                    observer.verify(observation)

    def test_missing_file_and_later_cleanup_never_replace_first_terminal_prepare_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            directory = Path(temporary)
            first = {'schema': 1, 'operation': 'prepare', 'error': 'original controlled permission failure'}
            GUEST.save(directory, 'failure.json', first)
            actual_read = GUEST.read_bytes
            with mock.patch.object(GUEST, 'read_bytes', side_effect=lambda path: actual_read(path, os.getuid())):
                GUEST.retain_failure(directory, 'stop', {'operation': 'stop', 'error': 'later cleanup failure'})
                with mock.patch.object(GUEST, 'ROOT', directory.parent), \
                     mock.patch.object(GUEST, 'load_claim', return_value={}), \
                     mock.patch.object(GUEST, 'private_directory'), \
                     mock.patch.object(GUEST, 'retrieve_file', side_effect=FileNotFoundError('snapshot.json')), \
                     mock.patch.object(GUEST.os, 'geteuid', return_value=0), \
                     mock.patch.object(sys, 'argv', ['helper', 'file', json.dumps({'nonce': NONCE, 'name': 'snapshot.json'})]), \
                     mock.patch.object(sys, 'stdout', new_callable=io.StringIO), \
                     mock.patch.object(GUEST, 'retained_files', return_value=['failure.json']), \
                     mock.patch.object(GUEST, 'retain_failure', wraps=GUEST.retain_failure) as retain:
                    with self.assertRaises(SystemExit):
                        GUEST.main()
                    self.assertEqual(retain.call_args.args[1], 'file')
            self.assertEqual(json.loads((directory / 'failure.json').read_bytes()), first)
            clean = directory / 'fresh'
            clean.mkdir()
            GUEST.retain_failure(clean, 'prepare', first)
            self.assertEqual(json.loads((clean / 'failure.json').read_bytes()), first)
            GUEST.retain_failure(clean, 'file', {'operation': 'file'})
            self.assertEqual(json.loads((clean / 'failure.json').read_bytes()), first)


class ControlledAtspi(unittest.TestCase):
    """Semantic validation of controlled public-object data, not native evidence."""
    def context(self):
        boot = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
        def principal(pid, exe):
            return {'pid': pid, 'uid': 1000, 'start_ticks': pid * 5, 'exe': exe,
                    'exe_sha256': str(pid % 10) * 64, 'boot_id': boot}
        shell = principal(10, '/usr/bin/gnome-shell')
        files = principal(11, '/usr/bin/nautilus')
        settings = principal(12, '/usr/bin/gnome-control-center')
        def node(pid, path, name, role, states):
            process = {10: shell, 11: files, 12: settings}[pid]
            return {'atspi': {'bus_name': ':1.' + str(pid), 'path': path, 'id_domain': 'AT-SPI'},
                    'name': name, 'role_name': role, 'states': states,
                    'process': copy.deepcopy(process), 'children': []}
        roots = [node(10, '/shell', 'GNOME Shell', 'application', []),
                 node(11, '/files', 'Files', 'application', []),
                 node(12, '/settings', 'Settings', 'application', [])]
        roots[0]['children'] = [node(10, '/shell/selected', 'Files', 'push button', [23, 25, 30])]
        roots[1]['children'] = [node(11, '/files/first', 'first', 'frame', [25, 30]),
                                node(11, '/files/second', 'second', 'frame', [25, 30])]
        roots[2]['children'] = [node(12, '/settings/network', 'Settings', 'frame', [1, 25, 30])]
        row = node(12, '/settings/network/nav', '', 'list item', [23, 25, 30])
        row['children'] = [node(12, '/settings/network/nav/label', 'Network', 'label', [25, 30])]
        roots[2]['children'][0]['children'] = [row]
        fixture = {'directories': ['/run/owned/first', '/run/owned/second'], 'processes': [files, settings]}
        return roots, fixture, shell

    def test_selection_keeps_own_shell_id_and_active_window_independent(self):
        trees, fixture, shell = self.context()
        windows, selection = GUEST.window_context(trees, fixture, shell)
        self.assertEqual(len(windows), 3)
        self.assertEqual(selection['selected'][0]['atspi']['path'], '/shell/selected')
        self.assertEqual(selection['selected'][0]['process']['pid'], shell['pid'])
        self.assertEqual(selection['active_windows'][0]['role'], 'settings')
        settings_window = selection['active_windows'][0]
        self.assertEqual(settings_window['name'], 'Settings')
        self.assertEqual(settings_window['panel_evidence'][0]['selected_object']['atspi']['path'], '/settings/network/nav')
        self.assertEqual(settings_window['panel_evidence'][0]['label_objects'][0]['name'], 'Network')
        self.assertEqual(settings_window['accessibility_tree'], trees[2]['children'][0])
        self.assertNotIn(selection['selected'][0]['atspi'], [w['atspi'] for w in windows])
        self.assertEqual(selection['objects'], [trees[0]])
        trees[0]['children'][0]['states'] = []
        self.assertEqual(GUEST.window_context(trees, fixture, shell)[1]['selected'], [])

    def test_network_label_requires_selected_visible_original_descendant_not_window_title(self):
        for change in ('unselected', 'hidden-label', 'foreign-label', 'title-only'):
            trees, fixture, shell = self.context()
            window = trees[2]['children'][0]
            row = window['children'][0]
            label = row['children'][0]
            if change == 'unselected':
                row['states'] = [25, 30]
            elif change == 'hidden-label':
                label['states'] = []
            elif change == 'foreign-label':
                label['process']['uid'] = 42
            else:
                window['name'] = 'Network'
                window['children'] = []
            with self.subTest(change=change), self.assertRaises(RuntimeError):
                GUEST.window_context(trees, fixture, shell)

    def test_missing_foreign_stale_duplicate_hidden_or_extra_context_fails(self):
        changes = ('missing-shell', 'stale-shell', 'foreign-app', 'missing-window',
                   'duplicate-id', 'hidden-window', 'unowned-directory', 'wrong-panel',
                   'two-active', 'extra-window')
        for change in changes:
            trees, fixture, shell = self.context()
            if change == 'missing-shell':
                trees.pop(0)
            elif change == 'stale-shell':
                trees[0]['process']['start_ticks'] += 1
            elif change == 'foreign-app':
                trees[1]['children'][0]['process']['uid'] = 42
            elif change == 'missing-window':
                trees[1]['children'].pop()
            elif change == 'duplicate-id':
                trees[1]['children'][1]['atspi'] = trees[1]['children'][0]['atspi']
            elif change == 'hidden-window':
                trees[1]['children'][0]['states'] = [30]
            elif change == 'unowned-directory':
                trees[1]['children'][0]['name'] = 'unrelated'
            elif change == 'wrong-panel':
                trees[2]['children'][0]['children'][0]['children'][0]['name'] = 'Appearance'
            elif change == 'two-active':
                trees[1]['children'][0]['states'].append(1)
            else:
                trees[1]['children'].append(copy.deepcopy(trees[1]['children'][0]))
            with self.subTest(change=change), self.assertRaises(RuntimeError):
                GUEST.window_context(trees, fixture, shell)


class ControlledObservationEvidence(unittest.TestCase):
    """Timeout/state evidence semantics; not native qualification."""

    def test_observation_timeout_retries_as_fixture_not_ready(self):
        self.assertTrue(issubclass(GUEST.ObservationTimeout, GUEST.FixtureNotReady))
        with self.assertRaises(GUEST.FixtureNotReady):
            raise GUEST.ObservationTimeout('expired')

    def test_worker_timeout_exit_maps_to_retryable_timeout(self):
        self.assertIsNone(GUEST.check_worker_status(0, 'anything'))
        with self.assertRaises(GUEST.ObservationTimeout) as timeout:
            GUEST.check_worker_status(GUEST.WORKER_TIMEOUT_EXIT, 'observe progress nodes=2000 elapsed_ms=24000')
        self.assertIn('nodes=2000', str(timeout.exception))
        with self.assertRaises(RuntimeError) as failed:
            GUEST.check_worker_status(1, 'boom')
        self.assertNotIsInstance(failed.exception, GUEST.FixtureNotReady)
        self.assertIn('boom', str(failed.exception))

    def test_worker_main_reports_own_deadline_as_exit_ten(self):
        body = io.BytesIO(json.dumps({'compositor': {}, 'budget_s': 5}).encode())
        with mock.patch.object(GUEST.sys, 'argv', ['guest', '_observe', '99']), \
             mock.patch.object(GUEST.sys, 'stdin', SimpleNamespace(buffer=body)), \
             mock.patch.object(GUEST.socket, 'socket'), \
             mock.patch.object(GUEST.traceback, 'print_exc'), \
             mock.patch.object(GUEST, 'session_observe', side_effect=TimeoutError('expired')):
            with self.assertRaises(SystemExit) as done:
                GUEST.main()
        self.assertEqual(done.exception.code, GUEST.WORKER_TIMEOUT_EXIT)

    def test_first_state_error_survives_later_timeouts(self):
        deadline = GUEST.Deadline(25)
        calls = []

        def attempt():
            calls.append(1)
            if len(calls) == 1:
                raise GUEST.FixtureNotReady('tab inventory []')
            deadline.end = 0
            raise GUEST.ObservationTimeout('finite public observation deadline expired')

        with mock.patch.object(GUEST.time, 'sleep'):
            with self.assertRaises(RuntimeError) as failed:
                GUEST.snapshot_until_ready(attempt, [], deadline)
        message = str(failed.exception)
        self.assertIn('first: FixtureNotReady: tab inventory []', message)
        self.assertIn('last: ObservationTimeout: finite public observation deadline expired', message)
        self.assertIsInstance(failed.exception.__cause__, GUEST.ObservationTimeout)

    def test_timeout_only_run_reports_last_without_first(self):
        deadline = GUEST.Deadline(25)
        deadline.end = 0

        def attempt():
            raise GUEST.ObservationTimeout('expired')

        with mock.patch.object(GUEST.time, 'sleep'):
            with self.assertRaises(RuntimeError) as failed:
                GUEST.snapshot_until_ready(attempt, [], deadline)
        self.assertIn('last: ObservationTimeout: expired', str(failed.exception))
        self.assertNotIn('first:', str(failed.exception))

    def test_inactive_settings_counts_as_state_not_timeout(self):
        deadline = GUEST.Deadline(25)
        deadline.end = 0

        def attempt():
            return {'selection': {'active_windows': [{'role': 'files-first'}]}}

        with mock.patch.object(GUEST.time, 'sleep'):
            with self.assertRaises(RuntimeError) as failed:
                GUEST.snapshot_until_ready(attempt, [], deadline)
        self.assertIn('original Settings Network window is not actually active', str(failed.exception))

    def test_ready_snapshot_returns_without_sleep(self):
        deadline = GUEST.Deadline(25)
        expected = {'selection': {'active_windows': [{'role': 'settings'}]}}
        with mock.patch.object(GUEST.time, 'sleep') as slept:
            self.assertIs(GUEST.snapshot_until_ready(lambda: expected, [], deadline), expected)
        slept.assert_not_called()

    def test_drain_recovers_pipe_bytes_without_blocking(self):
        out_read, out_write = os.pipe()
        err_read, err_write = os.pipe()
        os.write(out_write, b'{"partial": true')
        os.write(err_write, b'observe progress nodes=2000 elapsed_ms=24000')
        os.close(out_write)
        os.close(err_write)
        out_stream = os.fdopen(out_read, 'rb')
        err_stream = os.fdopen(err_read, 'rb')
        try:
            worker = SimpleNamespace(stdout=out_stream, stderr=err_stream)
            streams = {out_stream: bytearray(), err_stream: bytearray()}
            GUEST.drain_worker_streams(worker, streams)
            self.assertEqual(bytes(streams[out_stream]), b'{"partial": true')
            self.assertEqual(bytes(streams[err_stream]), b'observe progress nodes=2000 elapsed_ms=24000')
        finally:
            out_stream.close()
            err_stream.close()

    def test_drain_leaves_empty_pipes_alone(self):
        out_read, out_write = os.pipe()
        err_read, err_write = os.pipe()
        out_stream = os.fdopen(out_read, 'rb')
        err_stream = os.fdopen(err_read, 'rb')
        try:
            worker = SimpleNamespace(stdout=out_stream, stderr=err_stream)
            streams = {out_stream: bytearray(b'kept'), err_stream: bytearray()}
            GUEST.drain_worker_streams(worker, streams)
            self.assertEqual(bytes(streams[out_stream]), b'kept')
            self.assertEqual(bytes(streams[err_stream]), b'')
        finally:
            out_stream.close()
            err_stream.close()
            os.close(out_write)
            os.close(err_write)

    def test_stale_tree_names_retry_wedged_peer_stays_fatal(self):
        for name in ('org.freedesktop.DBus.Error.UnknownMethod',
                     'org.freedesktop.DBus.Error.UnknownObject',
                     'org.freedesktop.DBus.Error.UnknownInterface',
                     'org.freedesktop.DBus.Error.ServiceUnknown'):
            with self.subTest(name=name):
                self.assertTrue(GUEST.stale_tree_message('GDBus.Error:' + name + ': gone'))
        for text in ('GDBus.Error:org.freedesktop.DBus.Error.NoReply: slow peer',
                     'g-dbus-error-quark: timeout', '', None, 19):
            with self.subTest(text=text):
                self.assertFalse(GUEST.stale_tree_message(text))

    def test_worker_stale_tree_exit_maps_to_retryable_timeout(self):
        with self.assertRaises(GUEST.ObservationTimeout) as timeout:
            GUEST.check_worker_status(GUEST.WORKER_STALE_TREE_EXIT, 'UnknownMethod on /org/tuna/Shell/a11y/x')
        self.assertIn('changed during observation', str(timeout.exception))

    def test_worker_main_reports_torn_tree_as_exit_eleven(self):
        body = io.BytesIO(json.dumps({'compositor': {}, 'budget_s': 5}).encode())
        with mock.patch.object(GUEST.sys, 'argv', ['guest', '_observe', '99']), \
             mock.patch.object(GUEST.sys, 'stdin', SimpleNamespace(buffer=body)), \
             mock.patch.object(GUEST.socket, 'socket'), \
             mock.patch.object(GUEST.traceback, 'print_exc'), \
             mock.patch.object(GUEST, 'session_observe', side_effect=GUEST.FixtureNotReady('torn')):
            with self.assertRaises(SystemExit) as done:
                GUEST.main()
        self.assertEqual(done.exception.code, GUEST.WORKER_STALE_TREE_EXIT)

    def test_torn_tree_read_retries_instead_of_crashing(self):
        user_bus = 'unix:path=/run/user/1000/bus'
        a11y_bus = 'unix:path=/run/user/1000/at-spi/bus_0'

        class Variant:
            def __init__(self, signature, value):
                self.value = value
            def unpack(self):
                return self.value

        class FakeGError(Exception):
            def __init__(self, message):
                super().__init__(message)
                self.message = message

        principal = {'pid': 42, 'uid': 1000, 'start_ticks': 123, 'exe': '/usr/bin/dbus-broker',
                     'exe_sha256': 'a' * 64, 'boot_id': 'controlled-boot'}

        def observe(churn):
            def identity(channel, deadline, operation, address, name=None, descriptor=None):
                return principal if operation == 'socket' else [':1.42', principal]
            streams = [SimpleNamespace(get_stream=lambda: SimpleNamespace(
                get_socket=lambda: SimpleNamespace(get_fd=lambda: 91))) for _ in range(2)]
            def call(connection, bus, path, interface, method, parameters, deadline, Gio):
                if method == 'GetAddress':
                    return (a11y_bus,)
                if method == 'GetCurrentState':
                    return (1, [], [], {})
                if method == 'GetChildren':
                    if path == '/org/a11y/atspi/accessible/root':
                        return ([(':1.50', '/app')],)
                    self.fail('walk reached past the torn node read')
                if method == 'GetAll':
                    raise churn
                self.fail('unexpected child DBus query: ' + method)
            modules = {'gi': SimpleNamespace(require_version=lambda *args: None),
                       'gi.repository': SimpleNamespace(
                           Gio=object(), GLib=SimpleNamespace(Variant=Variant, GError=FakeGError))}
            with mock.patch.dict(sys.modules, modules), \
                 mock.patch.object(GUEST.os, 'geteuid', return_value=1000), \
                 mock.patch.object(GUEST, 'bus_connection', side_effect=streams), \
                 mock.patch.object(GUEST, 'identity_request', side_effect=identity), \
                 mock.patch.object(GUEST, 'dbus_call', side_effect=call), \
                 mock.patch.object(GUEST, 'read_gio_settings', return_value={}), \
                 mock.patch.object(GUEST, 'proc_identity', side_effect=AssertionError('unprivileged proc read')), \
                 mock.patch.object(GUEST, 'check_socket_peer', side_effect=AssertionError('unprivileged identity read')):
                return GUEST.session_observe({'compositor': principal}, GUEST.Deadline(3), object())

        for name in ('org.freedesktop.DBus.Error.UnknownMethod',
                     'org.freedesktop.DBus.Error.UnknownObject',
                     'org.freedesktop.DBus.Error.UnknownInterface',
                     'org.freedesktop.DBus.Error.ServiceUnknown'):
            with self.subTest(name=name), self.assertRaises(GUEST.FixtureNotReady):
                observe(FakeGError('GDBus.Error:' + name + ': No such interface on object at path /org/tuna/Shell/a11y/x'))
        with self.assertRaises(FakeGError):
            observe(FakeGError('GDBus.Error:org.freedesktop.DBus.Error.NoReply: wedged peer'))

    def test_heartbeat_reports_walk_progress(self):
        start = GUEST.time.monotonic()
        self.assertIsNone(GUEST.observe_progress(0, start))
        self.assertIsNone(GUEST.observe_progress(1, start))
        self.assertIsNone(GUEST.observe_progress(499, start))
        self.assertIsNone(GUEST.observe_progress(501, start))
        self.assertIsNone(GUEST.observe_progress(1999, start))
        self.assertRegex(GUEST.observe_progress(500, start),
                         r'^observe progress nodes=500 elapsed_ms=\d+ \(early\)$')
        self.assertRegex(GUEST.observe_progress(1000, start),
                         r'^observe progress nodes=1000 elapsed_ms=\d+ \(early\)$')
        line = GUEST.observe_progress(2000, start)
        self.assertRegex(line, r'^observe progress nodes=2000 elapsed_ms=\d+$')
        self.assertRegex(GUEST.observe_progress(4000, start), r'^observe progress nodes=4000 elapsed_ms=\d+$')


if __name__ == '__main__':
    unittest.main()
