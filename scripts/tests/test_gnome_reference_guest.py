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


if __name__ == '__main__':
    unittest.main()
