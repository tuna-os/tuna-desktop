#!/usr/bin/env python3
# tuna-rename: keep-file
"""Export a bounded, payload-free projection after disposable lifecycle teardown.

Raw private evidence is never made runner-readable or put in the archive.
Only reviewed typed results, metadata and content digests are exported. The
archive has flat regular members; its index preserves original evidence file
identities without restoring their permissions onto the runner's filesystem.
"""
import argparse
import hashlib
import io
import json
import math
import os
from pathlib import Path
import re
import secrets
import stat
import subprocess
import sys
import tarfile
import tempfile

MAX_FILE = 16 * 1024 * 1024
MAX_TOTAL = 64 * 1024 * 1024
MAX_MANIFESTS = 256
MAX_ROWS = 16384
ARCHIVE = 'legacy-upgrade-results.tar.gz'
NOFOLLOW = os.O_RDONLY | os.O_CLOEXEC | os.O_NOFOLLOW


def sha(data):
    return hashlib.sha256(data).hexdigest()


def encoded(value):
    return (json.dumps(value, sort_keys=True, indent=2, allow_nan=False) + '\n').encode()


def mapping(value):
    if not isinstance(value, dict) or len(value) > MAX_ROWS:
        raise ValueError('invalid mapping')
    return value


def rows(value):
    if not isinstance(value, list) or len(value) > MAX_ROWS:
        raise ValueError('invalid rows')
    return value


def token(value, pattern=r'[A-Za-z0-9_.:+~() /=-]{1,256}'):
    if not isinstance(value, str) or re.fullmatch(pattern, value) is None:
        raise ValueError('invalid typed token')
    return value


def number(value):
    if (type(value) not in (int, float) or abs(value) > 2**64
            or (type(value) is float and not math.isfinite(value))):
        raise ValueError('invalid number')
    return value


def digest(value):
    return token(value, r'[0-9a-f]{64}')


def fingerprint(value):
    # Arbitrary tool text is represented only by its byte count and digest.
    raw = value.encode() if isinstance(value, str) else encoded(value)
    return {'size': len(raw), 'sha256': sha(raw)}


def build_ids(notes):
    if not isinstance(notes, str) or len(notes) > MAX_FILE:
        raise ValueError('invalid ELF notes')
    return sorted(set(re.findall(r'^[ \t]*Build ID: ([0-9a-fA-F]{16,128})[ \t]*$', notes, re.MULTILINE)))


def library_path(path):
    token(path, r'/(?:usr/)?lib(?:64)?/(?:(?:x86_64-linux-gnu|aarch64-linux-gnu)/)?'
          r'(?:security/)?[A-Za-z0-9_.+-]{1,128}')
    token(Path(path).name, r'(?:lib(?:pam|audit|cap-ng|c|econf|dl|m|pcre2-8|selinux|crypt)\.so(?:\.[0-9]+)*|'
          r'ld-linux-(?:x86-64|aarch64)\.so(?:\.[0-9]+)*|pam_(?:permit|deny)\.so)')
    return path


def native_library(row):
    result = selected(row, numeric=('uid', 'gid', 'mode', 'size'), hashes=('sha256',))
    for key in ('path', 'resolved_path'):
        if key in row:
            result[key + '_identity'] = fingerprint(row[key])
            try:
                result[key] = library_path(row[key])
            except ValueError:
                # A failed preflight can legitimately have an unsafe path;
                # identify it without copying unreviewed text into the export.
                result[key + '_allowlisted'] = False
    if 'elf_notes' in row:
        result['build_ids'] = build_ids(row['elf_notes'])
    return result


def package_versions(text):
    if not isinstance(text, str) or len(text) > 8192:
        raise ValueError('invalid package version output')
    result = []
    for line in text.splitlines():
        if not line.strip():
            continue
        match = re.fullmatch(r'(libpam0g|libpam-modules|libc6)(?::(amd64|arm64))? '
                             r'([0-9][A-Za-z0-9.+:~_-]{0,127})', line)
        if not match:
            raise ValueError('unexpected native package version row')
        result.append({'package': match[1], 'architecture': match[2], 'version': match[3]})
    if len(result) > 3 or len({row['package'] for row in result}) != len(result):
        raise ValueError('duplicate native package version')
    return result


def selected(value, numeric=(), hashes=(), enums=None):
    value = mapping(value)
    result = {key: number(value[key]) for key in numeric if key in value}
    if any(type(item) is not int for key, item in result.items()
           if key not in ('started_unix', 'duration_seconds')):
        raise ValueError('invalid integer result')
    result.update({key: digest(value[key]) for key in hashes if key in value})
    for key, allowed in (enums or {}).items():
        if key in value:
            if value[key] not in allowed:
                raise ValueError('invalid result enum')
            result[key] = value[key]
    return result


def file_metadata(info):
    return {'uid': info.st_uid, 'gid': info.st_gid, 'mode': stat.S_IMODE(info.st_mode),
            'inode': info.st_ino, 'device': info.st_dev, 'size': info.st_size,
            'mtime_ns': info.st_mtime_ns, 'ctime_ns': info.st_ctime_ns}


def manifest(value):
    result = {}
    for name, row in mapping(value).items():
        # Paths are metadata, never archive member names or extraction targets.
        token(name, r'[A-Za-z0-9_.+/@:-]{1,512}')
        if name.startswith('/') or '..' in name.split('/'):
            raise ValueError('unsafe manifest path')
        projected = selected(row, numeric=('mode', 'uid', 'gid', 'inode', 'device', 'size'),
                             enums={'kind': ('file', 'directory', 'symlink')})
        if row.get('sha256') is not None:
            projected['sha256'] = digest(row['sha256'])
        if row.get('target') is not None:
            projected['target_identity'] = fingerprint(row['target'])
        # bytes_base64, body and any other unreviewed field are never copied.
        result[name] = projected
    return result


def native(value):
    value = mapping(value)
    result = selected(value, numeric=('pid', 'exit_code'), hashes=('ready_sha256', 'stdout_sha256'),
                      enums={'event': ('private-pam-process',),
                             'mode': ('retained-service', 'missing-service', 'administrator-deny')})
    if 'ready_output' in value:
        if not isinstance(value['ready_output'], str):
            raise ValueError('invalid native ready record')
        ready = parse(value['ready_output'].encode())
        result['ready'] = selected(ready, numeric=('pid',), enums={'event': ('ready',)})
        for key in ('chroot_verified', 'outside_fds_closed', 'capabilities_zero', 'host_sentinel_absent',
                    'host_proc_absent', 'host_shadow_absent', 'traversal_sentinel_absent',
                    'no_new_privs', 'pam_loaded'):
            if key in ready:
                if type(ready[key]) is not bool:
                    raise ValueError('invalid ready boolean')
                result['ready'][key] = ready[key]
    if 'native_result' in value:
        result['native_result'] = selected(value['native_result'],
            numeric=('pid', 'start', 'authenticate', 'account', 'end'),
            enums={'event': ('pam-result',), 'service': ('roost-lock',), 'confdir': ('/etc/pam.d',)})
    for key in ('process_before', 'process_after'):
        if key in value:
            result[key] = selected(value[key], numeric=('device', 'inode'))
            result[key]['executable_deleted'] = str(value[key].get('exe_link', '')).endswith(' (deleted)')
    return result


def command(value):
    value = mapping(value)
    result = selected(value, numeric=('started_unix', 'duration_seconds', 'exit_code', 'stdout_bytes'),
                      hashes=('stdout_sha256',))
    if value.get('event') == 'private-pam-process':
        result.update(native(value))
    argv = value.get('argv', [])
    rows(argv)
    if argv and isinstance(argv[0], str):
        tool = Path(argv[0]).name
        if tool in ('dpkg', 'dpkg-deb', 'dpkg-query', 'cc', 'readelf', 'ldd', 'roost-compositor'):
            result['tool'] = tool
    result['argv_identity'] = fingerprint(argv)
    result['administrator_keep_local'] = '--force-confold' in argv
    for key in ('stdout', 'stderr', 'exception', 'environment'):
        if key in value:
            result[key + '_identity'] = fingerprint(value[key])
    return result


def report(value):
    value = mapping(value)
    result = selected(value, numeric=('schema', 'tests_run'), hashes=('harness_source_sha256',), enums={
        'result': ('running', 'failed', 'passed-scoped-dpkg-only', 'passed-scoped-lifecycle'),
        'native_pam': ('not-run', 'pending', 'blocked-preflight', 'failed', 'passed'),
        'dpkg_lifecycle': ('failed', 'passed'),
        'candidate_kind': ('supplied-original-archive', 'production-assets-with-synthetic-binary-payload'),
        'beta_kind': ('pinned-published-original-archive', 'synthetic-beta-fixture')})
    result['cases'] = []
    references = []
    for case in rows(value.get('cases', [])):
        item = {'test': token(case['test'], r'test_[a-z0-9_]{1,160}'), 'steps': []}
        for step in rows(case.get('steps', [])):
            name = token(step['manifest'], r'test_[a-z0-9_]+-[0-9]{2,3}-[a-z0-9-]+\.json')
            if len(name) > 240 or not name.startswith(item['test'] + '-'):
                raise ValueError('manifest identity mismatch')
            references.append(name)
            out = selected(step, hashes=('archive_sha256',),
                           enums={'administrator_choice': ('none', 'keep-local (--force-confold)')})
            out.update(manifest=name, label=token(step['label'], r'[a-z0-9-]{1,80}'))
            item['steps'].append(out)
        if 'native_pam' in case:
            item['native_pam'] = native(case['native_pam'])
        result['cases'].append(item)
    if len(references) > MAX_MANIFESTS or len(set(references)) != len(references):
        raise ValueError('manifest count or duplicate')
    result['packages'] = {}
    for name, package in mapping(value.get('packages', {})).items():
        token(name, r'[a-z0-9-]{1,80}')
        item = selected(package, numeric=('size',), hashes=('sha256',))
        item['fields'] = {key: token(field, r'[A-Za-z0-9.+:~_-]{1,128}')
                          for key, field in mapping(package.get('fields', {})).items()
                          if key in ('Package', 'Version', 'Architecture')}
        for key in ('control_manifest', 'data_manifest'):
            if key in package:
                item[key] = manifest(package[key])
        result['packages'][name] = item
    result['version_relations'] = {}
    for name, relation in mapping(value.get('version_relations', {})).items():
        token(name, r'[a-z0-9-]{1,80}')
        item = selected(relation, enums={'comparison': ('lt', 'eq', 'gt'),
            'observed_operation': ('upgrade', 'same-version reinstall', 'downgrade')})
        for key in ('source_version', 'target_version'):
            item[key] = token(relation[key], r'[A-Za-z0-9.+:~_-]{1,128}')
        item['comparator_results'] = {key: selected(row, numeric=('exit_code',))
            for key, row in mapping(relation.get('comparator_results', {})).items() if key in ('lt', 'eq', 'gt')}
        result['version_relations'][name] = item
    for key in ('failures', 'errors'):
        result[key] = [{'record_identity': fingerprint(row)} for row in rows(value.get(key, []))]
    # No ambient configuration bodies, traceback text, ldd/readelf output or
    # arbitrary version stdout. Keep their identity plus native typed provenance.
    for key in ('environment', 'tools', 'ambient_dpkg_config', 'limitations'):
        if key in value:
            result[key + '_identity'] = fingerprint(value[key])
    for key in ('native_preflight', 'native_build'):
        if key not in value:
            continue
        raw = value[key]
        item = selected(raw, hashes=('probe_source_sha256', 'probe_sha256'),
                        enums={'state': ('checking', 'passed', 'failed-before-ldd-and-pam')})
        item['record_identity'] = fingerprint(raw)
        if key == 'native_build':
            if 'package_versions' in raw:
                item['package_versions'] = package_versions(raw['package_versions'])
            if 'probe_elf_notes' in raw:
                item['probe_build_ids'] = build_ids(raw['probe_elf_notes'])
            if 'abi' in raw:
                item.update(selected(raw, enums={'abi': (
                    'Linux-PAM five-argument pam_start_confdir(service,user,conv,confdir,pamh); no fallback',)}))
        for collection in ('core_library_metadata', 'libraries'):
            if collection in raw:
                item[collection] = [native_library(row) for row in rows(raw[collection])]
        result[key] = item
    return result, references


def parse(raw):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError('duplicate JSON key')
            result[key] = value
        return result
    return json.loads(raw, object_pairs_hook=unique,
                      parse_constant=lambda _: (_ for _ in ()).throw(ValueError('nonfinite JSON')))


def open_directory(path):
    path = Path(path)
    if not path.is_absolute() or '..' in path.parts:
        raise ValueError('directory must be absolute and anchored')
    fd = os.open('/', NOFOLLOW | os.O_DIRECTORY)
    try:
        for component in path.parts[1:]:
            child = os.open(component, NOFOLLOW | os.O_DIRECTORY, dir_fd=fd)
            os.close(fd)
            fd = child
        return fd
    except BaseException:
        os.close(fd)
        raise


def names_at(fd, limit):
    names = []
    with os.scandir(fd) as entries:
        for entry in entries:
            names.append(entry.name)
            if len(names) > limit:
                raise ValueError('evidence directory entry bound')
    return names


def publish(files, output_dir):
    if len(files) > MAX_MANIFESTS + 4 or sum(map(len, files.values())) > MAX_TOTAL:
        raise ValueError('projected evidence bound')
    outfd = open_directory(output_dir)
    name = '.legacy-upgrade-' + secrets.token_hex(12)
    fd = None
    try:
        fd = os.open(name, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_CLOEXEC | os.O_NOFOLLOW, 0o600, dir_fd=outfd)
        with os.fdopen(os.dup(fd), 'wb') as stream:
            # Some lifecycle manifest basenames exceed USTAR's 100-byte name
            # field. PAX carries only our validated, flat regular-file names.
            with tarfile.open(fileobj=stream, mode='w:gz', format=tarfile.PAX_FORMAT) as archive:
                for member, raw in sorted(files.items()):
                    token(member, r'[a-zA-Z0-9_.-]{1,240}')
                    if member.startswith('.') or '/' in member:
                        raise ValueError('unsafe export name')
                    item = tarfile.TarInfo(member)
                    item.size, item.mode, item.uid, item.gid = len(raw), 0o444, 0, 0
                    archive.addfile(item, io.BytesIO(raw))
        os.fchmod(fd, 0o444)
        os.fsync(fd)
        # No replacement of a previous export or a destination symlink.
        os.link(name, ARCHIVE, src_dir_fd=outfd, dst_dir_fd=outfd, follow_symlinks=False)
        return Path(output_dir) / ARCHIVE
    finally:
        if fd is not None:
            os.close(fd)
            os.unlink(name, dir_fd=outfd)
        os.close(outfd)


def export_evidence(source_parent, output_dir, test_exit):
    if Path(output_dir).is_relative_to(Path(source_parent)):
        raise ValueError('export destination must be outside private evidence')
    parent = open_directory(source_parent)
    directory = None
    identities = {}
    total = 0
    try:
        children = names_at(parent, 1)
        if len(children) != 1 or re.fullmatch(r'legacy-upgrade-[a-zA-Z0-9_-]+', children[0]) is None:
            raise ValueError('expected one private evidence ledger')
        directory = os.open(children[0], NOFOLLOW | os.O_DIRECTORY, dir_fd=parent)
        original_root = file_metadata(os.fstat(directory))
        if original_root['mode'] != 0o700 or original_root['uid'] != os.geteuid():
            raise ValueError('ledger must remain private and owned by exporter')

        def read(name):
            nonlocal total
            fd = os.open(name, NOFOLLOW | os.O_NONBLOCK, dir_fd=directory)
            try:
                before = os.fstat(fd)
                if not stat.S_ISREG(before.st_mode) or before.st_nlink != 1 or before.st_uid != original_root['uid']:
                    raise ValueError('evidence must be singly linked owned regular files')
                if before.st_size > MAX_FILE or total + before.st_size > MAX_TOTAL:
                    raise ValueError('raw evidence bound')
                with os.fdopen(os.dup(fd), 'rb') as stream:
                    raw = stream.read(MAX_FILE + 1)
                if len(raw) != before.st_size or file_metadata(os.fstat(fd)) != file_metadata(before):
                    raise ValueError('evidence changed during export')
                total += len(raw)
                identities[name] = dict(file_metadata(before), sha256=sha(raw))
                return raw
            finally:
                os.close(fd)

        projected, references = report(parse(read('report.json')))
        names = set(names_at(directory, MAX_MANIFESTS + 3))
        expected = {'report.json', 'empty-home', *references}
        if 'commands.jsonl' in names:
            expected.add('commands.jsonl')
        elif test_exit == 0:
            raise ValueError('successful ledger missing commands')
        if names != expected:
            raise ValueError('unexpected or missing evidence member')
        home = os.open('empty-home', NOFOLLOW | os.O_DIRECTORY, dir_fd=directory)
        try:
            if names_at(home, 0):
                raise ValueError('generated HOME is not empty')
        finally:
            os.close(home)
        files = {'report.json': encoded(projected)}
        if 'commands.jsonl' in names:
            lines = read('commands.jsonl').splitlines()
            if len(lines) > MAX_ROWS:
                raise ValueError('command count bound')
            files['commands.jsonl'] = b''.join(json.dumps(command(parse(line)), sort_keys=True).encode() + b'\n' for line in lines)
        for name in references:
            files[name] = encoded(manifest(parse(read(name))))
        if set(names_at(directory, MAX_MANIFESTS + 3)) != names or file_metadata(os.fstat(directory)) != original_root:
            raise ValueError('evidence directory changed during export')
        files['run.json'] = encoded({'schema': 1, 'test_exit': test_exit, 'projection': 'complete',
            'exporter_source_sha256': sha(Path(__file__).read_bytes()),
            'scope': 'Result metadata only; not end-to-end upgrade qualification'})
        files['index.json'] = encoded({'schema': 1, 'source_root': original_root,
            'source_files': identities, 'projected_files': {name: fingerprint(raw.decode()) for name, raw in files.items()}})
        return publish(files, output_dir)
    finally:
        if directory is not None:
            os.close(directory)
        os.close(parent)


def run_and_export(command_argv, source_parent, output_dir):
    # Output stays in the CI log, never in the public evidence archive. The
    # lifecycle process performs its own fixture teardown before returning.
    try:
        status = subprocess.run(command_argv, check=False).returncode
    except OSError:
        status = 127
    status = status if status >= 0 else 128 - status
    try:
        export_evidence(source_parent, output_dir, status)
    except (OSError, ValueError, TypeError, KeyError, RecursionError) as error:
        print('legacy-upgrade export failed: ' + type(error).__name__, file=sys.stderr)
        try:
            publish({'run.json': encoded({'schema': 1, 'test_exit': status, 'projection': 'failed',
                                         'error_type': type(error).__name__})}, output_dir)
        except (OSError, ValueError):
            pass
        return status or 2
    return status


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--candidate', type=Path, required=True)
    parser.add_argument('--beta', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    with tempfile.TemporaryDirectory(prefix='legacy-upgrade-private-') as private:
        command_argv = [sys.executable, str(Path(__file__).with_name('legacy-upgrade-lifecycle-tests.py')),
                        '--candidate', str(args.candidate), '--beta', str(args.beta), '--artifacts', private]
        return run_and_export(command_argv, Path(private), args.output)


if __name__ == '__main__':
    raise SystemExit(main())
