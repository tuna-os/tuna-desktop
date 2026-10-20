#!/usr/bin/env python3
# tuna-rename: keep-file
"""Filesystem regressions for the bounded, payload-free lifecycle exporter.

All inputs are synthetic JSON in disposable directories. These tests never
install a package, invoke dpkg, load a PAM library, or execute a PAM probe.
"""
import base64
import errno
import hashlib
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import shutil
import socket
import stat
import subprocess
import sys
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
EXPORTER = ROOT / 'scripts/lib/legacy-upgrade-evidence.py'
SPEC = importlib.util.spec_from_file_location('legacy_upgrade_evidence', EXPORTER)
M = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(M)
MARKER = 'PRIVATE_PAYLOAD_MUST_NEVER_APPEAR_5ee968b6'
ENCODED_MARKER = base64.b64encode(MARKER.encode()).decode()
DIGEST = hashlib.sha256(b'synthetic payload identity').hexdigest()
MANIFESTS = ('test_upgrade-00-before-install.json', 'test_upgrade-01-after-install.json')


def write_json(path, value):
    path.write_text(json.dumps(value, sort_keys=True) + '\n')


def identities(root):
    """Include bytes and metadata, but not atime, which a read may change."""
    result = {}
    for path in [root, *sorted(root.rglob('*'))]:
        info = path.lstat()
        result[str(path.relative_to(root))] = {
            'mode': info.st_mode, 'uid': info.st_uid, 'gid': info.st_gid,
            'inode': info.st_ino, 'device': info.st_dev, 'nlink': info.st_nlink,
            'size': info.st_size, 'mtime_ns': info.st_mtime_ns,
            'ctime_ns': info.st_ctime_ns,
            'sha256': hashlib.sha256(path.read_bytes()).hexdigest()
            if stat.S_ISREG(info.st_mode) else None,
            'target': os.readlink(path) if stat.S_ISLNK(info.st_mode) else None,
        }
    return result


class EvidenceExportTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='evidence-export-test-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.source = self.root / 'source'
        self.source.mkdir(mode=0o700)
        self.evidence = self.source / 'legacy-upgrade-synthetic'
        self.evidence.mkdir(mode=0o700)
        (self.evidence / 'empty-home').mkdir(mode=0o700)
        self.output = self.root / 'export'
        self.output.mkdir(mode=0o755)
        self.report = {
            'schema': 1, 'result': 'passed-scoped-lifecycle', 'tests_run': 15,
            'dpkg_lifecycle': 'passed', 'native_pam': 'passed',
            'candidate_kind': 'production-assets-with-synthetic-binary-payload',
            'beta_kind': 'synthetic-beta-fixture', 'harness_source_sha256': DIGEST,
            'limitations': ['No GDM session reselection'],
            'packages': {},
            'cases': [{'test': 'test_upgrade', 'steps': [
                {'label': 'before-install', 'manifest': MANIFESTS[0]},
                {'label': 'after-install', 'manifest': MANIFESTS[1],
                 'administrator_choice': 'none', 'archive_sha256': DIGEST}]}],
            'failures': [], 'errors': [],
        }
        self.manifest = {
            'etc': {'kind': 'directory', 'mode': 0o755, 'uid': 0, 'gid': 0,
                    'inode': 100, 'device': 200, 'size': 4096},
            'etc/pam.d/roost-lock': {
                'kind': 'file', 'mode': 0o644, 'uid': 0, 'gid': 0,
                'inode': 101, 'device': 200, 'size': len(MARKER),
                'sha256': DIGEST, 'bytes_base64': ENCODED_MARKER},
            'usr/bin/roost-compositor': {
                'kind': 'symlink', 'mode': 0o777, 'uid': 0, 'gid': 0,
                'inode': 102, 'device': 200, 'size': 15,
                'target': 'tuna-compositor'},
        }
        self.commands = [{'argv': ['dpkg', '--compare-versions', '1.0', 'lt', '2.0'],
                          'started_unix': 1.0, 'duration_seconds': 0.25,
                          'exit_code': 0, 'stdout': MARKER, 'stderr': MARKER,
                          'environment': {'HOME': MARKER}}]
        self.save()

    def save(self):
        write_json(self.evidence / 'report.json', self.report)
        for name in MANIFESTS:
            write_json(self.evidence / name, self.manifest)
        self.save_commands()

    def save_commands(self):
        (self.evidence / 'commands.jsonl').write_text(
            ''.join(json.dumps(row) + '\n' for row in self.commands))

    def export(self, status=0):
        before = identities(self.source)
        archive = M.export_evidence(self.source, self.output, status)
        self.assertEqual(identities(self.source), before,
                         'export must not rewrite, chmod, chown, or replace raw evidence')
        self.assertIsInstance(archive, Path)
        self.assertTrue(archive.is_file())
        self.assertTrue(archive.is_relative_to(self.output))
        return archive

    def read_archive(self, archive):
        with tarfile.open(archive, 'r:*') as packed:
            members = packed.getmembers()
            names = [member.name for member in members]
            self.assertEqual(len(names), len(set(names)), 'duplicate archive member')
            for member in members:
                self.assertTrue(member.isfile(), member.name)
                self.assertFalse(member.issym() or member.islnk(), member.name)
                self.assertEqual(PurePosixPath(member.name).name, member.name)
                self.assertNotIn(member.name, ('.', '..'))
                self.assertNotIn('\\', member.name)
                self.assertFalse(member.mode & 0o7022, member.name)
            return {member.name: packed.extractfile(member).read() for member in members}

    def assert_rejected(self):
        before = identities(self.source)
        with self.assertRaises((ValueError, OSError, RuntimeError)):
            M.export_evidence(self.source, self.output, 0)
        self.assertEqual(identities(self.source), before,
                         'rejected input must also remain unchanged')
        if self.output.exists():
            self.assertFalse(any(path.is_file() for path in self.output.rglob('*')),
                             'failure must not leave a published archive')

    def test_missing_report_is_rejected(self):
        (self.evidence / 'report.json').unlink()
        self.assert_rejected()

    def test_missing_referenced_manifest_is_rejected(self):
        (self.evidence / MANIFESTS[0]).unlink()
        self.assert_rejected()

    def test_unreferenced_files_are_rejected(self):
        for name in ('unused.json', 'candidate.deb', 'probe', 'libpam.so.0'):
            with self.subTest(name=name):
                path = self.evidence / name
                path.write_text(MARKER)
                self.assert_rejected()
                path.unlink()

    def test_nonempty_home_is_rejected(self):
        (self.evidence / 'empty-home' / '.dpkg.cfg').write_text(MARKER)
        self.assert_rejected()

    def test_extra_directory_is_rejected(self):
        (self.evidence / 'private-pam-root').mkdir()
        self.assert_rejected()

    def test_multiple_evidence_directories_are_rejected(self):
        (self.source / 'legacy-upgrade-second').mkdir(mode=0o700)
        self.assert_rejected()

    def test_unexpected_parent_entry_is_rejected(self):
        (self.source / 'payload.deb').write_text(MARKER)
        self.assert_rejected()

    def test_unsafe_manifest_references_are_rejected(self):
        for name in ('../outside.json', '/tmp/outside.json', 'sub/inside.json',
                     './report.json', 'sub\\inside.json', '..', '.',
                     'line\nbreak.json', 'nul\x00name.json'):
            with self.subTest(name=name):
                self.report['cases'][0]['steps'][0]['manifest'] = name
                write_json(self.evidence / 'report.json', self.report)
                self.assert_rejected()

    def test_reserved_manifest_names_are_rejected(self):
        for name in ('report.json', 'commands.jsonl', 'run.json', 'index.json'):
            with self.subTest(name=name):
                self.report['cases'][0]['steps'][0]['manifest'] = name
                write_json(self.evidence / 'report.json', self.report)
                self.assert_rejected()

    def test_actual_evidence_files_cannot_be_symlinks(self):
        outside = self.root / 'outside.json'
        for name in ('report.json', 'commands.jsonl', *MANIFESTS):
            with self.subTest(name=name):
                path = self.evidence / name
                body = path.read_bytes()
                outside.write_bytes(body)
                path.unlink()
                path.symlink_to(outside)
                self.assert_rejected()
                path.unlink()
                path.write_bytes(body)

    def test_actual_evidence_files_cannot_be_hardlinks(self):
        for name in ('report.json', 'commands.jsonl', *MANIFESTS):
            with self.subTest(name=name):
                outside = self.root / ('hardlinked-' + name)
                os.link(self.evidence / name, outside)
                self.assert_rejected()
                outside.unlink()

    def test_evidence_directory_cannot_be_a_symlink(self):
        outside = self.root / 'actual-evidence'
        self.evidence.rename(outside)
        self.evidence.symlink_to(outside, target_is_directory=True)
        self.assert_rejected()

    def test_empty_home_cannot_be_a_symlink(self):
        (self.evidence / 'empty-home').rmdir()
        outside = self.root / 'outside-home'
        outside.mkdir()
        (self.evidence / 'empty-home').symlink_to(outside, target_is_directory=True)
        self.assert_rejected()

    def test_fifo_is_rejected_without_opening_or_hanging(self):
        path = self.evidence / MANIFESTS[0]
        path.unlink()
        os.mkfifo(path)
        before = identities(self.source)
        # A broken exporter must time out this test instead of hanging the suite
        # by opening a FIFO while its writer does not exist.
        script = '''import importlib.util, sys
from pathlib import Path
s = importlib.util.spec_from_file_location('exporter', sys.argv[1])
m = importlib.util.module_from_spec(s)
s.loader.exec_module(m)
try:
    m.export_evidence(Path(sys.argv[2]), Path(sys.argv[3]), 0)
except (ValueError, OSError, RuntimeError):
    sys.exit(0)
sys.exit(91)
'''
        result = subprocess.run([sys.executable, '-c', script, str(EXPORTER),
                                 str(self.source), str(self.output)],
                                capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertEqual(identities(self.source), before)

    def test_socket_is_rejected(self):
        path = self.evidence / MANIFESTS[0]
        path.unlink()
        try:
            server = socket.socket(socket.AF_UNIX)
        except OSError as error:
            if error.errno in (errno.EPERM, errno.EACCES, errno.EAFNOSUPPORT):
                self.skipTest('runner forbids Unix sockets; FIFO refusal is covered separately')
            raise
        with server:
            # The deliberately long TMPDIR can exceed sockaddr_un.sun_path.
            previous = Path.cwd()
            try:
                os.chdir(self.evidence)
                server.bind(path.name)
            finally:
                os.chdir(previous)
            self.assert_rejected()

    def test_oversized_file_is_rejected(self):
        with (self.evidence / MANIFESTS[0]).open('wb') as output:
            output.truncate(16 * 1024 * 1024 + 1)
        self.assert_rejected()

    def test_more_than_256_manifests_is_rejected(self):
        for name in MANIFESTS:
            (self.evidence / name).unlink()
        self.report['cases'][0]['steps'] = []
        for number in range(257):
            name = f'test_upgrade-{number:03}-snapshot.json'
            write_json(self.evidence / name, {})
            self.report['cases'][0]['steps'].append({'label': 'snapshot', 'manifest': name})
        write_json(self.evidence / 'report.json', self.report)
        self.assert_rejected()

    def test_more_than_16384_command_rows_is_rejected(self):
        (self.evidence / 'commands.jsonl').write_text(
            (json.dumps(self.commands[0]) + '\n') * 16385)
        self.assert_rejected()

    def test_more_than_16384_manifest_rows_is_rejected(self):
        write_json(self.evidence / MANIFESTS[0], {
            f'file-{number}': {'kind': 'file', 'mode': 0o644, 'uid': 0,
                              'gid': 0, 'size': 0, 'sha256': DIGEST}
            for number in range(16385)})
        self.assert_rejected()

    def test_more_than_64_mib_aggregate_is_rejected(self):
        for name in MANIFESTS:
            (self.evidence / name).unlink()
        self.report['cases'][0]['steps'] = []
        for number in range(5):
            name = f'test_upgrade-{number:02}-large.json'
            with (self.evidence / name).open('wb') as output:
                output.write(b'{}')
                output.write(b' ' * (14 * 1024 * 1024 - 2))
            self.report['cases'][0]['steps'].append({'label': 'large', 'manifest': name})
        write_json(self.evidence / 'report.json', self.report)
        self.assert_rejected()

    def test_malformed_report_and_manifest_json_are_rejected(self):
        for name in ('report.json', *MANIFESTS):
            with self.subTest(name=name):
                path = self.evidence / name
                body = path.read_bytes()
                path.write_text('{incomplete')
                self.assert_rejected()
                path.write_bytes(body)

    def test_malformed_command_row_is_rejected(self):
        with (self.evidence / 'commands.jsonl').open('a') as output:
            output.write('{incomplete\n')
        self.assert_rejected()

    def test_duplicate_json_object_keys_are_rejected(self):
        (self.evidence / MANIFESTS[0]).write_text('{"duplicate": {}, "duplicate": {}}')
        self.assert_rejected()

    def test_successful_export_has_exact_ledger_and_no_raw_payload(self):
        self.report['ambient_dpkg_config'] = [
            {'path': '/etc/dpkg/dpkg.cfg', 'sha256': DIGEST, 'text': MARKER}]
        self.report['errors'] = [{'test': 'setUpClass', 'traceback': MARKER}]
        self.report['failures'] = [{'test': 'test_upgrade', 'traceback': MARKER}]
        self.report['unexpected_private_field'] = MARKER
        self.report['cases'][0]['unexpected_private_field'] = MARKER
        self.save()
        raw_identity = identities(self.source)
        files = self.read_archive(self.export())
        expected = {'run.json', 'report.json', 'commands.jsonl', *MANIFESTS}
        self.assertEqual(set(files) - {'index.json'}, expected)
        for name, body in files.items():
            self.assertNotIn(MARKER.encode(), body, name)
            self.assertNotIn(ENCODED_MARKER.encode(), body, name)
            self.assertNotIn(b'bytes_base64', body, name)
        projected = json.loads(files['report.json'])
        self.assertEqual(projected['result'], self.report['result'])
        self.assertEqual(projected['tests_run'], 15)
        self.assertEqual(projected['native_pam'], 'passed')
        self.assertEqual(projected['dpkg_lifecycle'], 'passed')
        references = {step['manifest'] for case in projected['cases']
                      for step in case['steps']}
        self.assertEqual(references, set(MANIFESTS))
        for name in MANIFESTS:
            rows = json.loads(files[name])
            self.assertEqual(set(rows), set(self.manifest))
            for path, raw in self.manifest.items():
                for key in ('kind', 'mode', 'uid', 'gid', 'size', 'sha256'):
                    if key in raw:
                        self.assertEqual(rows[path][key], raw[key])
        self.assertEqual(identities(self.source), raw_identity)

    def test_index_preserves_original_hashes_ownership_and_modes(self):
        before = identities(self.source)
        files = self.read_archive(self.export())
        index = json.loads(files['index.json'])
        source_files = index['source_files']
        self.assertEqual(set(source_files), {'report.json', 'commands.jsonl', *MANIFESTS})
        for name, recorded in source_files.items():
            original = before[self.evidence.name + '/' + name]
            for key in ('sha256', 'uid', 'gid', 'size', 'inode', 'device', 'mtime_ns', 'ctime_ns'):
                self.assertEqual(recorded[key], original[key], (name, key))
            self.assertEqual(recorded['mode'], stat.S_IMODE(original['mode']))
        root = before[self.evidence.name]
        self.assertEqual(index['source_root']['mode'], 0o700)
        for key in ('uid', 'gid', 'inode', 'device', 'mtime_ns', 'ctime_ns'):
            self.assertEqual(index['source_root'][key], root[key])
        self.assertEqual(set(index['projected_files']), set(files) - {'index.json'})
        for name, recorded in index['projected_files'].items():
            self.assertEqual(recorded['sha256'], hashlib.sha256(files[name]).hexdigest())
            self.assertEqual(recorded['size'], len(files[name]))

    def test_failed_test_evidence_still_exports(self):
        self.report.update(result='failed', native_pam='blocked-preflight',
                           dpkg_lifecycle='passed', tests_run=15)
        self.report['errors'] = [{'test': 'test_private_pam', 'traceback': MARKER}]
        self.save()
        files = self.read_archive(self.export(status=17))
        projected = json.loads(files['report.json'])
        self.assertEqual(projected['result'], 'failed')
        self.assertEqual(projected['native_pam'], 'blocked-preflight')
        self.assertEqual(projected['dpkg_lifecycle'], 'passed')
        self.assertEqual(json.loads(files['run.json'])['test_exit'], 17)
        self.assertEqual(json.loads(files['run.json'])['projection'], 'complete')
        self.assertNotIn(MARKER.encode(), b''.join(files.values()))

    def test_commands_are_optional(self):
        (self.evidence / 'commands.jsonl').unlink()
        files = self.read_archive(self.export(status=17))
        self.assertEqual(set(files) - {'index.json', 'commands.jsonl'},
                         {'run.json', 'report.json', *MANIFESTS})
        if 'commands.jsonl' in files:
            self.assertFalse(files['commands.jsonl'].strip())

    def test_success_without_commands_is_rejected(self):
        (self.evidence / 'commands.jsonl').unlink()
        self.assert_rejected()

    def test_duplicate_manifest_reference_is_rejected(self):
        self.report['cases'][0]['steps'].append(dict(self.report['cases'][0]['steps'][0]))
        write_json(self.evidence / 'report.json', self.report)
        self.assert_rejected()

    def test_missing_evidence_is_rejected(self):
        shutil.rmtree(self.evidence)
        self.assert_rejected()

    def test_nonfinite_json_is_rejected(self):
        (self.evidence / 'commands.jsonl').write_text('{"exit_code": NaN}\n')
        self.assert_rejected()

    def test_output_is_readable_and_safely_extractable_by_runner(self):
        archive = self.export()
        files = self.read_archive(archive)
        self.assertTrue(os.access(archive, os.R_OK))
        self.assertTrue(archive.stat().st_mode & stat.S_IROTH)
        self.assertTrue(self.output.stat().st_mode & stat.S_IXOTH)
        self.assertFalse(archive.stat().st_mode & 0o7022)
        target = self.root / 'extracted'
        target.mkdir()
        # Test a separate ordinary Python reader, rather than trusting only
        # the exporter process's descriptors or owner-only permissions.
        script = '''import pathlib, sys, tarfile
with tarfile.open(sys.argv[1], 'r:*') as archive:
    archive.extractall(sys.argv[2], filter='data')
for path in pathlib.Path(sys.argv[2]).iterdir():
    assert path.is_file() and not path.is_symlink()
    path.read_bytes()
'''
        result = subprocess.run([sys.executable, '-c', script, str(archive), str(target)],
                                capture_output=True, timeout=10)
        self.assertEqual(result.returncode, 0, result.stderr.decode())
        self.assertEqual({path.name for path in target.iterdir()}, set(files))
        for path in target.iterdir():
            self.assertEqual(path.read_bytes(), files[path.name])

    def test_repeated_export_cannot_overwrite_existing_output(self):
        archive = self.export()
        before = identities(self.output)
        with self.assertRaises((ValueError, OSError, RuntimeError)):
            M.export_evidence(self.source, self.output, 1)
        after = identities(self.output)
        del before['.'], after['.']  # Private temporary publication can touch directory times.
        self.assertEqual(after, before)
        self.assertTrue(archive.is_file())

    def test_existing_archive_symlink_cannot_be_replaced_or_followed(self):
        outside = self.root / 'outside-archive'
        outside.write_text(MARKER)
        archive = self.output / 'legacy-upgrade-results.tar.gz'
        archive.symlink_to(outside)
        before = outside.stat()
        with self.assertRaises((ValueError, OSError, RuntimeError)):
            M.export_evidence(self.source, self.output, 0)
        self.assertTrue(archive.is_symlink())
        self.assertEqual(outside.read_text(), MARKER)
        self.assertEqual(outside.stat(), before)
        self.assertEqual(list(self.output.iterdir()), [archive])

    def test_long_realistic_manifest_names_remain_flat_regular_members(self):
        test_name = 'test_' + 'legacy_upgrade_' * 8 + 'policy_preserved'
        self.report['cases'][0]['test'] = test_name
        names = []
        for number, step in enumerate(self.report['cases'][0]['steps']):
            name = f'{test_name}-{number:02}-{step["label"]}.json'
            self.assertGreater(len(name), 100)
            (self.evidence / step['manifest']).rename(self.evidence / name)
            step['manifest'] = name
            names.append(name)
        write_json(self.evidence / 'report.json', self.report)
        files = self.read_archive(self.export())
        self.assertEqual(set(files), {'run.json', 'report.json', 'commands.jsonl',
                                     'index.json', *names})

    def test_symlink_output_directory_is_rejected_without_outside_writes(self):
        outside = self.root / 'outside-output'
        outside.mkdir()
        sentinel = outside / 'sentinel'
        sentinel.write_text(MARKER)
        before = identities(outside)
        self.output.rmdir()
        self.output.symlink_to(outside, target_is_directory=True)
        with self.assertRaises((ValueError, OSError, RuntimeError)):
            M.export_evidence(self.source, self.output, 0)
        self.assertEqual(identities(outside), before)

    def test_output_cannot_mutate_private_evidence(self):
        self.output = self.evidence / 'empty-home'
        self.assert_rejected()

    def test_symlink_source_ancestor_is_rejected(self):
        alias = self.root / 'source-alias'
        alias.symlink_to(self.source, target_is_directory=True)
        before = identities(self.source)
        with self.assertRaises((ValueError, OSError)):
            M.export_evidence(alias, self.output, 0)
        self.assertEqual(identities(self.source), before)
        self.assertEqual(list(self.output.iterdir()), [])

    def test_nonprivate_evidence_directory_is_rejected(self):
        self.evidence.chmod(0o755)
        self.assert_rejected()

    def run_command(self, status):
        marker = self.root / 'explicit-command-executed'
        command = [sys.executable, '-c',
                   'from pathlib import Path; import sys; '
                   'Path(sys.argv[1]).write_text("executed once"); '
                   'sys.exit(int(sys.argv[2]))', str(marker), str(status)]
        observed = M.run_and_export(command, self.source, self.output)
        self.assertEqual(marker.read_text(), 'executed once')
        return observed

    def test_successful_command_returns_zero_after_export(self):
        self.assertEqual(self.run_command(0), 0)
        self.assertTrue(any(path.is_file() for path in self.output.iterdir()))

    def test_failed_command_preserves_original_exit_and_exports(self):
        self.assertEqual(self.run_command(23), 23)
        self.assertTrue(any(path.is_file() for path in self.output.iterdir()))

    def test_failed_command_exit_wins_over_export_failure(self):
        (self.evidence / MANIFESTS[0]).unlink()
        self.assertEqual(self.run_command(23), 23)
        self.assert_failure_archive(23)

    def test_export_failure_cannot_turn_successful_command_green(self):
        (self.evidence / MANIFESTS[0]).unlink()
        self.assertNotEqual(self.run_command(0), 0)
        self.assert_failure_archive(0)

    def assert_failure_archive(self, test_exit):
        archive = self.output / 'legacy-upgrade-results.tar.gz'
        files = self.read_archive(archive)
        self.assertEqual(set(files), {'run.json'})
        run = json.loads(files['run.json'])
        self.assertEqual(run['test_exit'], test_exit)
        self.assertEqual(run['projection'], 'failed')
        self.assertNotIn(MARKER.encode(), files['run.json'])

    def test_command_failure_before_any_evidence_has_status_archive(self):
        shutil.rmtree(self.evidence)
        self.assertEqual(self.run_command(29), 29)
        self.assert_failure_archive(29)

    def test_missing_command_still_attempts_export(self):
        result = M.run_and_export([str(self.root / 'nonexistent-executable')],
                                  self.source, self.output)
        self.assertNotEqual(result, 0)
        files = self.read_archive(self.output / 'legacy-upgrade-results.tar.gz')
        self.assertNotEqual(json.loads(files['run.json'])['test_exit'], 0)

    def test_failed_command_status_survives_unwritable_publication(self):
        # A regular file cannot serve as the output directory, regardless of
        # whether the suite runs privileged or as an ordinary CI runner.
        self.output.rmdir()
        self.output.write_text('must not be replaced')
        self.assertEqual(self.run_command(31), 31)
        self.assertEqual(self.output.read_text(), 'must not be replaced')

    def test_native_result_retains_typed_outcomes_but_no_raw_text(self):
        ready = {'event': 'ready', 'pid': 123, 'chroot_verified': True,
                 'outside_fds_closed': True, 'capabilities_zero': True,
                 'host_sentinel_absent': True, 'host_proc_absent': True,
                 'host_shadow_absent': True, 'traversal_sentinel_absent': True,
                 'no_new_privs': True, 'pam_loaded': False}
        result = {'event': 'pam-result', 'pid': 123, 'service': 'roost-lock',
                  'confdir': '/etc/pam.d', 'start': 0, 'authenticate': 7,
                  'account': 7, 'end': 0, 'unexpected': MARKER}
        native = {'event': 'private-pam-process', 'mode': 'administrator-deny',
                  'pid': 123, 'exit_code': 0, 'argv': [MARKER],
                  'ready_output': json.dumps(ready), 'ready_sha256': DIGEST,
                  'stdout': MARKER, 'stderr': MARKER, 'exception': MARKER,
                  'native_result': result,
                  'process_before': {'device': 1, 'inode': 2, 'exe_link': MARKER},
                  'process_after': {'device': 1, 'inode': 2, 'exe_link': MARKER + ' (deleted)'}}
        self.report['cases'][0]['native_pam'] = native
        self.commands.append(native)
        self.save()
        files = self.read_archive(self.export())
        self.assertNotIn(MARKER.encode(), b''.join(files.values()))
        projected = json.loads(files['report.json'])['cases'][0]['native_pam']
        self.assertEqual(projected['ready'], ready)
        for key in ('start', 'authenticate', 'account', 'end'):
            self.assertEqual(projected['native_result'][key], result[key])
        self.assertFalse(projected['process_before']['executable_deleted'])
        self.assertTrue(projected['process_after']['executable_deleted'])

    def test_native_provenance_retains_reviewed_versions_paths_build_ids_and_abi(self):
        build_id = 'ab0123456789cdef' * 3
        probe_build_id = 'FEDCBA9876543210' * 2
        abi = 'Linux-PAM five-argument pam_start_confdir(service,user,conv,confdir,pamh); no fallback'
        paths = (
            '/lib/x86_64-linux-gnu/libpam.so.0',
            '/usr/lib/x86_64-linux-gnu/libpam.so.0.85.1',
            '/lib/aarch64-linux-gnu/security/pam_permit.so',
            '/usr/lib/aarch64-linux-gnu/security/pam_deny.so',
            '/lib64/ld-linux-x86-64.so.2',
            '/lib/ld-linux-aarch64.so.1',
        )
        libraries = [{'path': path, 'resolved_path': path, 'uid': 0, 'gid': 0,
                      'mode': 0o644, 'size': 1234, 'sha256': DIGEST,
                      'elf_notes': MARKER + '\n  Build ID: ' + build_id + '\n' + MARKER}
                     for path in paths]
        self.report['native_build'] = {
            'probe_source_sha256': DIGEST, 'probe_sha256': DIGEST,
            'package_versions': 'libpam0g:amd64 1.5.2-6+deb12u1\n'
                                'libpam-modules:arm64 1.5.2-6+deb12u1\n'
                                'libc6 2.36-9+deb12u10\n',
            'probe_elf_notes': MARKER + '\nBuild ID: ' + probe_build_id + '\n',
            'abi': abi, 'libraries': libraries, 'trust_boundary': MARKER,
        }
        self.report['native_preflight'] = {
            'state': 'passed', 'probe_source_sha256': DIGEST,
            'probe_sha256': DIGEST, 'core_library_metadata': libraries,
            'ownership_policy': MARKER,
        }
        self.save()
        files = self.read_archive(self.export())
        self.assertNotIn(MARKER.encode(), b''.join(files.values()))
        projected = json.loads(files['report.json'])
        build = projected['native_build']
        self.assertEqual(build['abi'], abi)
        self.assertEqual(build['probe_build_ids'], [probe_build_id])
        self.assertEqual(build['package_versions'], [
            {'package': 'libpam0g', 'architecture': 'amd64', 'version': '1.5.2-6+deb12u1'},
            {'package': 'libpam-modules', 'architecture': 'arm64', 'version': '1.5.2-6+deb12u1'},
            {'package': 'libc6', 'architecture': None, 'version': '2.36-9+deb12u10'},
        ])
        for collection in (build['libraries'], projected['native_preflight']['core_library_metadata']):
            self.assertEqual(len(collection), len(paths))
            for row, path in zip(collection, paths):
                self.assertEqual(row['path'], path)
                self.assertEqual(row['resolved_path'], path)
                self.assertEqual(row['sha256'], DIGEST)
                self.assertEqual(row['build_ids'], [build_id])
                self.assertNotIn('elf_notes', row)

    def test_unknown_native_library_paths_are_identity_only(self):
        paths = (
            '/tmp/' + MARKER + '/libpam.so.0',
            '/usr/lib/' + MARKER + '/libpam.so.0',
            '/lib/x86_64-linux-gnu/' + MARKER + '.so',
            '/usr/lib/x86_64-linux-gnu/../' + MARKER + '/libpam.so.0',
        )
        self.report['native_preflight'] = {
            'state': 'failed-before-ldd-and-pam', 'error': MARKER,
            'core_library_metadata': [
                {'path': path, 'resolved_path': path, 'uid': 0, 'gid': 0,
                 'mode': 0o644, 'sha256': DIGEST} for path in paths],
        }
        self.save()
        files = self.read_archive(self.export(status=1))
        self.assertNotIn(MARKER.encode(), b''.join(files.values()))
        rows = json.loads(files['report.json'])['native_preflight']['core_library_metadata']
        for row, path in zip(rows, paths):
            for key in ('path', 'resolved_path'):
                self.assertNotIn(key, row)
                self.assertIs(row[key + '_allowlisted'], False)
                self.assertEqual(row[key + '_identity'], {
                    'size': len(path.encode()), 'sha256': hashlib.sha256(path.encode()).hexdigest()})

    def test_native_package_version_rows_reject_unreviewed_output(self):
        for text in (
            'libpam0g 1.5.2 ' + MARKER,
            MARKER + ' 1.0',
            'libpam0g:' + MARKER + ' 1.0',
            'libpam0g 1.0\n' + MARKER,
            'libpam0g 1.0\nlibpam0g:amd64 1.0\n',
            'libc6 1.0\nlibaudit1 1.0\n',
            'libpam0g\t1.0',
            'libc6 not-a-version',
        ):
            with self.subTest(text=text):
                self.report['native_build'] = {'package_versions': text}
                write_json(self.evidence / 'report.json', self.report)
                self.assert_rejected()

    def test_native_abi_cannot_carry_unreviewed_text(self):
        self.report['native_build'] = {'abi': MARKER}
        self.save()
        self.assert_rejected()

    def test_elf_prose_and_malformed_build_ids_never_escape(self):
        valid = 'a0123456789bcdef' * 2
        notes = '\n'.join((MARKER, 'Build ID: ' + MARKER,
                           'Build ID: 012345', 'Build ID: ' + 'a' * 129,
                           'Build ID: ' + valid + ' ' + MARKER,
                           'embedded Build ID: ' + valid,
                           '  Build ID: ' + valid, '  Build ID: ' + valid))
        self.report['native_build'] = {
            'probe_elf_notes': notes,
            'libraries': [{'path': '/lib/x86_64-linux-gnu/libpam.so.0',
                           'sha256': DIGEST, 'elf_notes': notes}],
        }
        self.save()
        files = self.read_archive(self.export())
        self.assertNotIn(MARKER.encode(), b''.join(files.values()))
        build = json.loads(files['report.json'])['native_build']
        self.assertEqual(build['probe_build_ids'], [valid])
        self.assertEqual(build['libraries'][0]['build_ids'], [valid])
        self.assertNotIn('probe_elf_notes', build)
        self.assertNotIn('elf_notes', build['libraries'][0])


if __name__ == '__main__':
    unittest.main(verbosity=2)
