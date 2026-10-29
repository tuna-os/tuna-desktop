#!/usr/bin/env python3
# tuna-rename: keep-file
"""Real, disposable dpkg lifecycle and native, chroot-confined PAM tests.

Usage: python3 scripts/lib/legacy-upgrade-lifecycle-tests.py \
    --candidate /checked/candidate.deb --beta /original/beta.deb \
    --artifacts /evidence [--dpkg-only]

No arbitrary installation-root option exists. Root runs must be in a disposable
container; ordinary users are re-executed in a new user namespace. All dpkg
roots are freshly generated here, with explicit private admin databases/logs,
no maintainer hooks, and no package-binary execution. --force-depends models
package-manager state only, never runtime/dependency qualification. confold is
used only for an explicit modeled administrator keep-local decision.

Without --candidate the candidate is production assets plus SYNTHETIC binaries. Without --beta
the historical beta is a SYNTHETIC fixture. Those modes cannot qualify actual
release archives. --dpkg-only explicitly leaves native PAM unqualified. The
full suite never silently skips unavailable dependencies or failed PAM results.
Actual GDM, old compositor unlock, pacman, and Marlin are always unqualified.
"""
import argparse
import base64
import hashlib
import io
import json
import os
from pathlib import Path, PurePosixPath
import re
import resource
import selectors
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
OLD_PAM = 'etc/pam.d/roost-lock'
NEW_PAM = 'etc/pam.d/tuna-lock'
OLD_SESSION = 'usr/share/wayland-sessions/roost.desktop'
NEW_SESSION = 'usr/share/wayland-sessions/tuna.desktop'
OLD_BINS = ('roost-compositor', 'roost-greeter', 'roost-ibus-bridge',
            'roost-session', 'roost-shell-gtk', 'roost-shell-host')
NEW_BINS = ('tuna-compositor', 'tuna-ibus-bridge',
            'tuna-session', 'tuna-shell-gtk', 'tuna-shell-host', 'tuna-wallpaper-svg')
# The released betas also shipped the greetd greeter, since removed (GDM only).
BETA_BINS = NEW_BINS + ('tuna-greeter',)
PAM_RULES = [['auth', 'include', 'login'], ['account', 'include', 'login']]
DEFAULT_PAM = b'auth include login\naccount include login\n'
ORIGINAL_BETA_OLD_PAM = b'''#%PAM-1.0
# tuna-rename: keep-file
# One-release compatibility copy of tuna-lock under the old service name
# (#505), installed as /etc/pam.d/roost-lock. A session started before
# an in-place upgrade keeps running the old lock screen, which asks PAM
# for "roost-lock"; without this file PAM falls back to "other" (deny on
# most distributions) and that session could never be unlocked. It also
# keeps an administrator's edits to the old conffile meaningful until
# they are moved to tuna-lock.
auth      include   login
account   include   login
'''
ORIGINAL_BETA_NEW_PAM = b'''#%PAM-1.0
# Tuna Desktop lock screen: unlocks the running session with its own user's
# password (used whenever no greetd daemon is reachable, e.g. under GDM).
auth      include   login
account   include   login
'''
LOCAL_PAM = b'# Explicit administrator deny policy; must survive upgrade.\nauth requisite pam_deny.so\naccount requisite pam_deny.so\n'
DESKTOP = b'''[Desktop Entry]
Name=Tuna Desktop
Comment=Tuna Desktop (GNOME-style shell with optional scrollable tiling)
Exec=tuna-session
TryExec=tuna-session
Type=Application
DesktopNames=Tuna;GNOME;
X-GDM-SessionRegisters=true
'''
PINNED_BETA_SHA256 = 'b2b9ec7c9c5eb9f0e2003e48e458a440a551836d31a831972b8561a43e15ed01'
MAX_TAR = 256 * 1024 * 1024
ARGS = None
EVIDENCE = None


def digest(path):
    value = hashlib.sha256()
    with Path(path).open('rb') as source:
        for block in iter(lambda: source.read(1024 * 1024), b''):
            value.update(block)
    return value.hexdigest()


def clean_environment():
    # In particular, never inherit LD_PRELOAD, LD_AUDIT, LD_LIBRARY_PATH or
    # DPKG_ROOT/ADMINDIR settings into native tools or the PAM child.
    return {'PATH': '/usr/sbin:/usr/bin:/sbin:/bin', 'LC_ALL': 'C',
            'DEBIAN_FRONTEND': 'noninteractive', 'DPKG_COLORS': 'never',
            'HOME': str(EVIDENCE.path / 'empty-home') if EVIDENCE else '/nonexistent',
            'TMPDIR': tempfile.gettempdir()}


class Evidence:
    def __init__(self, directory):
        directory.mkdir(parents=True, exist_ok=True)
        self.path = Path(tempfile.mkdtemp(prefix='legacy-upgrade-', dir=directory))
        (self.path / 'empty-home').mkdir()
        self.commands = self.path / 'commands.jsonl'
        self.report = {'schema': 1, 'result': 'running', 'packages': {}, 'cases': [],
                       'harness_source_sha256': digest(Path(__file__)),
                       'limitations': ['No GDM session reselection', 'No old-compositor unlock',
                                       'No pacman replacement', 'No Marlin activation',
                                       'No dependency/runtime qualification: --force-depends is fixture-only'],
                       'native_pam': 'not-run' if ARGS.dpkg_only else 'pending',
                       'candidate_kind': 'supplied-original-archive' if ARGS.candidate else 'production-assets-with-synthetic-binary-payload',
                       'beta_kind': 'pinned-published-original-archive' if ARGS.beta else 'synthetic-beta-fixture'}
        if not ARGS.candidate:
            self.report['limitations'].append('No complete built candidate supplied; production assets and synthetic binary payload only')
        if not ARGS.beta:
            self.report['limitations'].append('No original published beta supplied; synthetic historical fixture only')
        self.save()

    def save(self):
        (self.path / 'report.json').write_text(json.dumps(self.report, indent=2) + '\n')

    def log(self, record):
        with self.commands.open('a') as output:
            output.write(json.dumps(record) + '\n')

    def run(self, argv, *, binary=False, check=True, timeout=60, status=False):
        argv = list(map(str, argv))
        started = time.time()
        record = {'argv': argv, 'started_unix': started, 'environment': clean_environment()}
        try:
            if binary:
                # Bound decompressor output before retaining any archive bytes.
                def bound():
                    resource.setrlimit(resource.RLIMIT_FSIZE, (MAX_TAR, MAX_TAR))
                with tempfile.TemporaryFile() as output:
                    result = subprocess.run(argv, stdin=subprocess.DEVNULL, stdout=output,
                                            stderr=subprocess.PIPE, env=clean_environment(),
                                            timeout=timeout, preexec_fn=bound)
                    output.seek(0)
                    stdout = output.read(MAX_TAR + 1)
                record.update(stdout_bytes=len(stdout), stdout_sha256=hashlib.sha256(stdout).hexdigest())
            else:
                result = subprocess.run(argv, stdin=subprocess.DEVNULL, capture_output=True,
                                        env=clean_environment(), timeout=timeout)
                stdout = result.stdout
                record['stdout'] = stdout.decode('utf-8', errors='replace')
            record.update(exit_code=result.returncode, stderr=result.stderr.decode('utf-8', errors='replace'))
            if binary and len(stdout) >= MAX_TAR:
                raise RuntimeError('archive output exceeds bound')
            if check and result.returncode:
                raise RuntimeError(f'command failed ({result.returncode}): {argv!r}\n{record["stderr"]}')
            if status:
                return {'exit_code': result.returncode, 'stdout': stdout.decode('utf-8'),
                        'stderr': record['stderr']}
            return stdout if binary else stdout.decode('utf-8')
        except Exception as error:
            record['exception'] = repr(error)
            raise
        finally:
            record['duration_seconds'] = time.time() - started
            self.log(record)


def canonical(name, directory=False):
    if name in ('.', './') and directory:
        return '.'
    name = name.removeprefix('./')
    if directory:
        name = name.removesuffix('/')
    if (not name or '\\' in name or any(ord(char) < 32 or ord(char) == 127 for char in name)
            or any(part in ('', '.', '..') for part in name.split('/'))):
        raise ValueError('unsafe original archive path')
    return name


def check_ambient_dpkg_config():
    """dpkg has no general --no-config switch; reject unknown ambient options.

    These known image defaults cannot execute a command. The explicit command
    line overrides log and re-includes every payload path after image filters.
    No user configuration is possible in the freshly generated, empty HOME.
    """
    paths = [Path('/etc/dpkg/dpkg.cfg')]
    directory = Path('/etc/dpkg/dpkg.cfg.d')
    if directory.exists():
        paths += [path for path in sorted(directory.iterdir())
                  if re.fullmatch(r'[A-Za-z0-9_-]+', path.name)]
    configs = []
    for path in paths:
        if not path.exists():
            continue
        if not path.is_file():
            raise RuntimeError('non-regular ambient dpkg configuration')
        body = path.read_text()
        for line in body.splitlines():
            line = line.strip()
            if not line or line.startswith('#'):
                continue
            if not re.fullmatch(r'(no-debsig|force-unsafe-io|log[ =]/var/log/dpkg\.log|'
                                r'path-(?:exclude|include)[ =]/usr/share/[^\s]+)', line):
                raise RuntimeError(f'unsafe or unknown ambient dpkg configuration in {path}: {line}')
        configs.append({'path': str(path), 'sha256': digest(path), 'text': body})
    if any((EVIDENCE.path / 'empty-home').iterdir()):
        raise RuntimeError('generated HOME must stay empty')
    previous = EVIDENCE.report.setdefault('ambient_dpkg_config', configs)
    if previous != configs:
        raise RuntimeError('ambient dpkg configuration changed during run')
    EVIDENCE.save()


def inspect_archive(path, label):
    """Independent safety preflight; dpkg sees only the original checked bytes."""
    path = path.resolve(strict=True)
    identity = digest(path)
    result = {'path': str(path), 'sha256': identity, 'size': path.stat().st_size, 'label': label}
    indexes = {}
    for kind, flag in (('control', '--ctrl-tarfile'), ('data', '--fsys-tarfile')):
        raw = EVIDENCE.run(['dpkg-deb', flag, path], binary=True)
        entries = {}
        with tarfile.open(fileobj=io.BytesIO(raw), mode='r:') as archive:
            for item in archive:
                if len(entries) >= 4096 or item.sparse is not None:
                    raise ValueError('original archive member bound/type')
                name = canonical(item.name, item.isdir())
                # tarfile normalizes trailing directory slashes; also inspect
                # the last physical header so ambiguous names are not hidden.
                header = raw[item.offset_data - 512:item.offset_data]
                physical = header[:100].split(b'\0', 1)[0].decode('utf-8')
                if header[257:263] == b'ustar\0':
                    prefix = header[345:500].split(b'\0', 1)[0].decode('utf-8')
                    if prefix:
                        physical = prefix + '/' + physical
                canonical(physical, item.isdir())
                if name in entries or item.uid != 0 or item.gid != 0:
                    raise ValueError('original archive duplicate/ownership')
                if item.type not in (tarfile.REGTYPE, tarfile.AREGTYPE, tarfile.DIRTYPE, tarfile.SYMTYPE):
                    raise ValueError('original archive unsafe member type')
                if item.mode & 0o7000 or (item.isdir() and item.mode & 0o022):
                    raise ValueError('original archive unsafe permissions')
                if kind == 'control' and (name not in ('.', 'control', 'conffiles', 'md5sums')
                                          or (name != '.' and not item.isfile())):
                    raise ValueError('original archive has hooks or unexpected control member')
                body = archive.extractfile(item).read() if item.isfile() else None
                entries[name] = {'kind': 'directory' if item.isdir() else 'symlink' if item.issym() else 'file',
                                 'mode': item.mode, 'uid': item.uid, 'gid': item.gid,
                                 'size': item.size, 'target': item.linkname if item.issym() else None,
                                 'sha256': hashlib.sha256(body).hexdigest() if body is not None else None,
                                 'body': body if body is not None and len(body) <= 16384 else None}
        if not entries:
            raise ValueError('empty archive')
        for name, item in entries.items():
            for parent in PurePosixPath(name).parents:
                if str(parent) in entries and entries[str(parent)]['kind'] != 'directory':
                    raise ValueError('original archive non-directory ancestor')
            if item['kind'] == 'symlink':
                target = str(PurePosixPath(name).parent / canonical(item['target']))
                seen = {name}
                while True:
                    if target in seen or target not in entries:
                        raise ValueError('original archive escaping/dangling/cyclic link')
                    seen.add(target)
                    linked = entries[target]
                    if linked['kind'] != 'symlink':
                        break
                    target = str(PurePosixPath(target).parent / canonical(linked['target']))
        indexes[kind] = entries
        result[kind + '_manifest'] = {name: {key: value for key, value in item.items() if key != 'body'}
                                      for name, item in entries.items()}
    for line in indexes['control'].get('conffiles', {}).get('body', b'').decode().splitlines():
        if not line.startswith('/') or canonical(line[1:]) not in indexes['data']:
            raise ValueError('conffile must be a regular package file')
        if indexes['data'][line[1:]]['kind'] != 'file':
            raise ValueError('conffile must be regular')
    fields = EVIDENCE.run(['dpkg-deb', '-f', path, 'Package', 'Version', 'Architecture'])
    result['fields'] = dict(line.split(': ', 1) for line in fields.splitlines())
    if result['fields']['Package'] not in ('roost', 'tuna-desktop'):
        raise ValueError('unexpected lifecycle package')
    if digest(path) != identity:
        raise ValueError('original archive changed during inspection')
    EVIDENCE.report['packages'][label] = result
    EVIDENCE.save()
    return {'path': path, 'sha256': identity, 'fields': result['fields'], **indexes}


def build_fixture(directory, name, version, flavor, probe=None):
    stage = directory / ('stage-' + flavor)
    stage.mkdir()
    files = {'DEBIAN/control': f'''Package: {name}
Version: {version}
Architecture: amd64
Maintainer: Tuna lifecycle fixture <fixture@example.invalid>
Description: synthetic disposable lifecycle fixture, never release qualification
'''.encode()}
    if name == 'tuna-desktop':
        files['DEBIAN/control'] += b'Replaces: roost\nConflicts: roost\nProvides: roost\n'
    bins = OLD_BINS if flavor.startswith('prerename') else BETA_BINS if flavor == 'beta' else NEW_BINS
    for binary in bins:
        # These deliberate inert payloads are NEVER executed. The one optional
        # PAM process is a compiled, test-only binary in a synthetic old package.
        files['usr/bin/' + binary] = b'SYNTHETIC PACKAGE PAYLOAD: DO NOT EXECUTE\n'
    files[OLD_PAM] = b'# synthetic historical policy\n' + DEFAULT_PAM
    files[OLD_SESSION] = b'[Desktop Entry]\nName=Roost\nExec=roost-session\nType=Application\n'
    conffiles = '/' + OLD_PAM + '\n'
    if not flavor.startswith('prerename'):
        files[NEW_PAM] = DEFAULT_PAM
        files[NEW_SESSION] = DESKTOP
        conffiles = '/' + NEW_PAM + '\n' + conffiles
    if flavor in ('beta', 'removed'):
        files[OLD_PAM] = ORIGINAL_BETA_OLD_PAM
        files[NEW_PAM] = ORIGINAL_BETA_NEW_PAM
    if flavor in ('candidate', 'changed-policy'):
        for installed, source in ((NEW_SESSION, 'share/wayland-sessions/tuna.desktop'),
                                  (OLD_SESSION, 'share/compat/wayland-sessions/roost.desktop'),
                                  (NEW_PAM, 'share/pam.d/tuna-lock'),
                                  (OLD_PAM, 'share/compat/pam.d/roost-lock')):
            files[installed] = (ROOT / source).read_bytes()
    if flavor == 'changed-policy':
        files[OLD_PAM] += b'# Deliberately changed shipped bytes: prompt/control fixture only.\n'
    if flavor == 'removed':
        del files[OLD_PAM], files[OLD_SESSION]
        files[NEW_PAM] = (ROOT / 'share/pam.d/tuna-lock').read_bytes()
        files[NEW_SESSION] = (ROOT / 'share/wayland-sessions/tuna.desktop').read_bytes()
        conffiles = '/' + NEW_PAM + '\n'
    files['DEBIAN/conffiles'] = conffiles.encode()
    if probe is not None:
        files['usr/bin/roost-compositor'] = probe.read_bytes()
    for relative, body in files.items():
        target = stage / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(body)
        target.chmod(0o755 if relative.startswith('usr/bin/') else 0o644)
    if flavor == 'beta':
        for old in OLD_BINS:
            (stage / 'usr/bin' / old).symlink_to(old.replace('roost-', 'tuna-'))
    for directory_path in stage.rglob('*'):
        if directory_path.is_dir():
            directory_path.chmod(0o755)
    stage.chmod(0o755)
    package = directory / (flavor + '.deb')
    EVIDENCE.run(['dpkg-deb', '--root-owner-group', '-Znone', '--build', stage, package])
    return inspect_archive(package, 'synthetic-' + flavor)


def snapshot(root):
    result = {}
    for path in sorted(root.rglob('*')):
        relative = str(path.relative_to(root))
        info = path.lstat()
        row = {'mode': stat.S_IMODE(info.st_mode), 'uid': info.st_uid, 'gid': info.st_gid,
               'inode': info.st_ino, 'device': info.st_dev, 'size': info.st_size}
        if path.is_symlink():
            row.update(kind='symlink', target=os.readlink(path))
        elif path.is_file():
            row.update(kind='file', sha256=digest(path))
            if info.st_size <= 16384:
                row['bytes_base64'] = base64.b64encode(path.read_bytes()).decode()
        elif path.is_dir():
            row['kind'] = 'directory'
        else:
            raise ValueError('unexpected private-root entry type')
        result[relative] = row
    return result


def package_status(root, name):
    for paragraph in (root / 'var/lib/dpkg/status').read_text().split('\n\n'):
        if f'Package: {name}\n' in paragraph + '\n':
            return paragraph
    return ''


def version_relation(source, target):
    comparisons = {}
    for operator in ('lt', 'eq', 'gt'):
        result = EVIDENCE.run(['dpkg', '--compare-versions', source, operator, target],
                              check=False, status=True)
        if result['exit_code'] not in (0, 1):
            raise RuntimeError('dpkg could not compare original package versions')
        comparisons[operator] = result
    matches = [operator for operator, result in comparisons.items() if result['exit_code'] == 0]
    if len(matches) != 1:
        raise RuntimeError('dpkg version comparison is inconsistent')
    operator = matches[0]
    return {'source_version': source, 'target_version': target, 'comparison': operator,
            'observed_operation': {'lt': 'upgrade', 'eq': 'same-version reinstall', 'gt': 'downgrade'}[operator],
            'comparator_results': comparisons}


class DisposableLifecycle(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        for tool in ('dpkg', 'dpkg-deb', 'cc', 'readelf', 'ldd'):
            if shutil.which(tool) is None:
                raise RuntimeError(f'required native dependency missing: {tool}')
        cls.temp = tempfile.TemporaryDirectory(prefix='tuna-upgrade-work-')
        cls.addClassCleanup(cls.temp.cleanup)
        cls.workspace = Path(cls.temp.name)
        check_ambient_dpkg_config()
        EVIDENCE.report['tools'] = {tool: EVIDENCE.run([tool, '--version']).splitlines()[0]
                                    for tool in ('dpkg', 'dpkg-deb', 'cc', 'readelf', 'ldd')}
        EVIDENCE.report['environment'] = {'uname': list(os.uname()), 'uid': os.getuid(),
                                          'uid_map': Path('/proc/self/uid_map').read_text(),
                                          'python': sys.version}
        cls.candidate = (inspect_archive(Path(ARGS.candidate), 'supplied-candidate') if ARGS.candidate
                         else build_fixture(cls.workspace, 'tuna-desktop', '1.0.0~beta.2', 'candidate'))
        if ARGS.beta and digest(ARGS.beta) != PINNED_BETA_SHA256:
            raise RuntimeError('supplied beta is not the pinned original published archive')
        cls.beta = (inspect_archive(Path(ARGS.beta), 'published-beta') if ARGS.beta
                    else build_fixture(cls.workspace, 'tuna-desktop', '1.0.0~beta.1', 'beta'))
        cls.prerename = build_fixture(cls.workspace, 'roost', '0.9.0', 'prerename')
        cls.removed = build_fixture(cls.workspace, 'tuna-desktop', '1.0.0~beta.1+bridge-removed', 'removed')
        cls.changed_policy = build_fixture(cls.workspace, 'tuna-desktop', '1.0.0~beta.2+changed-policy', 'changed-policy')
        EVIDENCE.report['version_relations'] = {
            'beta-to-candidate': version_relation(cls.beta['fields']['Version'], cls.candidate['fields']['Version']),
            'candidate-to-beta': version_relation(cls.candidate['fields']['Version'], cls.beta['fields']['Version']),
            'beta-to-removed-fixture': version_relation(cls.beta['fields']['Version'], cls.removed['fields']['Version']),
            'removed-fixture-to-candidate': version_relation(cls.removed['fields']['Version'], cls.candidate['fields']['Version'])}
        if EVIDENCE.report['version_relations']['beta-to-candidate']['comparison'] == 'eq':
            EVIDENCE.report['limitations'].append('Candidate/beta versions are identical; their rollback is a same-version reinstall, not a distinct-version downgrade')
        cls.native = None
        cls.native_preflight_error = None
        EVIDENCE.save()

    def setUp(self):
        self.temp_root = tempfile.TemporaryDirectory(prefix='install-', dir=self.workspace)
        self.addCleanup(self.temp_root.cleanup)
        self.root = Path(self.temp_root.name)
        (self.root / 'var/lib/dpkg').mkdir(parents=True)
        (self.root / 'var/lib/dpkg/status').write_text('')
        self.case = {'test': self._testMethodName, 'steps': []}
        EVIDENCE.report['cases'].append(self.case)
        self.addCleanup(EVIDENCE.save)

    def record(self, label):
        index = len(self.case['steps'])
        filename = f'{self._testMethodName}-{index:02}-{label}.json'
        (EVIDENCE.path / filename).write_text(json.dumps(snapshot(self.root), indent=2) + '\n')
        self.case['steps'].append({'label': label, 'manifest': filename})
        EVIDENCE.save()

    def install(self, package, *, keep_local=False):
        check_ambient_dpkg_config()
        self.assertEqual(digest(package['path']), package['sha256'], 'original archive identity changed')
        self.record('before-install')
        argv = ['dpkg', '--root=' + str(self.root), '--admindir=' + str(self.root / 'var/lib/dpkg'),
                '--log=' + str(self.root / 'dpkg.log'), '--no-triggers', '--force-depends', '--path-include=/*']
        if keep_local:
            argv.append('--force-confold')
        argv += ['--install', str(package['path'])]
        try:
            EVIDENCE.run(argv)
        finally:
            self.record('after-install')
        self.case['steps'][-1]['administrator_choice'] = 'keep-local (--force-confold)' if keep_local else 'none'
        self.case['steps'][-1]['archive_sha256'] = package['sha256']
        status_text = package_status(self.root, package['fields']['Package'])
        self.assertIn('Status: install ok installed', status_text)
        self.assertIn('Version: ' + package['fields']['Version'], status_text)

    def assert_regular(self, relative, body):
        path = self.root / relative
        self.assertFalse(path.is_symlink())
        self.assertTrue(path.is_file(), relative)
        self.assertEqual(path.read_bytes(), body)
        self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o644)
        self.assertEqual((path.stat().st_uid, path.stat().st_gid), (0, 0))

    def assert_bridge(self, pam=None, new_pam=None):
        self.assert_regular(OLD_SESSION, DESKTOP + b'NoDisplay=true\n')
        self.assert_regular(NEW_SESSION, DESKTOP)
        self.assert_regular(OLD_PAM, self.candidate['data'][OLD_PAM]['body'] if pam is None else pam)
        self.assert_regular(NEW_PAM, self.candidate['data'][NEW_PAM]['body'] if new_pam is None else new_pam)
        self.assertEqual({str(path.relative_to(self.root)) for path in self.root.rglob('*')
                          if 'roost' in str(path.relative_to(self.root)).lower()
                          and not str(path.relative_to(self.root)).startswith('var/lib/dpkg/')
                          and not str(path.relative_to(self.root)).endswith('.dpkg-dist')},
                         {OLD_PAM, OLD_SESSION})
        for old in OLD_BINS:
            self.assertFalse(os.path.lexists(self.root / 'usr/bin' / old))
        status_text = package_status(self.root, 'tuna-desktop')
        for name in (OLD_PAM, NEW_PAM):
            line = next(line for line in status_text.splitlines() if line.startswith(' /' + name + ' '))
            self.assertNotIn('obsolete', line)

    def test_candidate_original_policy_and_fresh_install(self):
        legacy = {name for name in self.candidate['data'] if 'roost' in name.lower()}
        self.assertEqual(legacy, {OLD_PAM, OLD_SESSION})
        for name in (OLD_PAM, NEW_PAM):
            data = self.candidate['data'][name]
            self.assertEqual((data['kind'], data['mode']), ('file', 0o644))
            rules = [line.split() for line in data['body'].decode().splitlines()
                     if line.strip() and not line.lstrip().startswith('#')]
            self.assertEqual(rules, PAM_RULES)
        self.assertEqual(set(self.candidate['control']['conffiles']['body'].decode().splitlines()),
                         {'/' + NEW_PAM, '/' + OLD_PAM})
        self.assertEqual(self.candidate['data'][OLD_PAM]['body'], ORIGINAL_BETA_OLD_PAM)
        self.assertEqual(self.candidate['data'][NEW_PAM]['body'], ORIGINAL_BETA_NEW_PAM)
        self.install(self.candidate)
        self.assert_bridge()

    def test_prerename_unchanged_conffile_and_package_replacement(self):
        self.install(self.prerename)
        self.install(self.candidate)
        self.assert_bridge()
        self.assertNotIn('Status: install ok installed', package_status(self.root, 'roost'))

    def test_prerename_administrator_modified_policy_is_preserved(self):
        self.install(self.prerename)
        (self.root / OLD_PAM).write_bytes(LOCAL_PAM)
        self.record('administrator-deny-policy')
        self.install(self.candidate, keep_local=True)
        self.assert_bridge(LOCAL_PAM)

    def test_prerename_administrator_deleted_policy_stays_absent(self):
        self.install(self.prerename)
        (self.root / OLD_PAM).unlink()
        self.record('administrator-deleted-policy')
        self.install(self.candidate, keep_local=True)
        self.assertFalse(os.path.lexists(self.root / OLD_PAM))
        self.assert_regular(OLD_SESSION, DESKTOP + b'NoDisplay=true\n')

    def test_beta_original_unchanged_package_replacement(self):
        self.install(self.beta)
        self.assertEqual({name for name, item in self.beta['data'].items()
                          if name.startswith('usr/bin/roost-') and item['kind'] == 'symlink'},
                         {'usr/bin/' + name for name in OLD_BINS})
        self.case['version_relation'] = EVIDENCE.report['version_relations']['beta-to-candidate']
        self.install(self.candidate)
        self.assert_bridge()
        # The candidate no longer ships the greeter: dpkg removes it.
        self.assertFalse(os.path.lexists(self.root / 'usr/bin/tuna-greeter'))

    def test_beta_administrator_modified_policy_is_preserved(self):
        self.install(self.beta)
        for name in (OLD_PAM, NEW_PAM):
            (self.root / name).write_bytes(LOCAL_PAM)
        self.record('administrator-deny-policy')
        self.install(self.candidate)
        self.assert_bridge(LOCAL_PAM, LOCAL_PAM)

    def test_beta_administrator_deleted_policy_stays_absent(self):
        self.install(self.beta)
        for name in (OLD_PAM, NEW_PAM):
            (self.root / name).unlink()
        self.record('administrator-deleted-policy')
        self.install(self.candidate)
        for name in (OLD_PAM, NEW_PAM):
            self.assertFalse(os.path.lexists(self.root / name))

    def test_deleted_changed_conffile_without_admin_choice_requires_prompt(self):
        self.install(self.beta)
        (self.root / OLD_PAM).unlink()
        self.record('administrator-deleted-policy')
        self.assertNotEqual(self.beta['data'][OLD_PAM]['body'], self.changed_policy['data'][OLD_PAM]['body'])
        with self.assertRaisesRegex(RuntimeError, 'end of file on stdin at conffile prompt'):
            self.install(self.changed_policy)
        self.assertFalse(os.path.lexists(self.root / OLD_PAM))
        self.assertNotIn('Status: install ok installed', package_status(self.root, 'tuna-desktop'))
        self.case['expected_negative'] = 'Changed deleted conffile prompts; EOF cannot configure the package'
        self.install(self.changed_policy, keep_local=True)
        self.assertFalse(os.path.lexists(self.root / OLD_PAM))

    def test_beta_to_removed_bridge_to_repair_obsolete_policy(self):
        self.install(self.beta)
        original = (self.root / OLD_PAM).read_bytes()
        self.install(self.removed)
        self.assert_regular(OLD_PAM, original)
        self.assertIn('obsolete', next(line for line in package_status(self.root, 'tuna-desktop').splitlines()
                                       if line.startswith(' /' + OLD_PAM + ' ')))
        self.assertFalse(os.path.lexists(self.root / OLD_SESSION))
        self.install(self.candidate)
        self.assert_bridge()

    def test_beta_removed_bridge_modified_obsolete_policy_is_preserved(self):
        self.install(self.beta)
        for name in (OLD_PAM, NEW_PAM):
            (self.root / name).write_bytes(LOCAL_PAM)
        self.record('administrator-deny-policy')
        self.install(self.removed)
        for name in (OLD_PAM, NEW_PAM):
            self.assert_regular(name, LOCAL_PAM)
        self.install(self.candidate)
        self.assert_bridge(LOCAL_PAM, LOCAL_PAM)

    def test_beta_removed_bridge_deleted_obsolete_policy_stays_absent(self):
        self.install(self.beta)
        self.install(self.removed)
        for name in (OLD_PAM, NEW_PAM):
            (self.root / name).unlink()
        self.record('administrator-deleted-obsolete-policy')
        self.install(self.candidate)
        for name in (OLD_PAM, NEW_PAM):
            self.assertFalse(os.path.lexists(self.root / name))

    def test_package_rollback_restores_beta_aliases_and_preserves_admin_policy(self):
        self.install(self.candidate)
        for name in (OLD_PAM, NEW_PAM):
            (self.root / name).write_bytes(LOCAL_PAM)
        self.record('administrator-deny-policy')
        self.install(self.beta)
        for name in (OLD_PAM, NEW_PAM):
            self.assert_regular(name, LOCAL_PAM)
        for old in OLD_BINS:
            self.assertTrue((self.root / 'usr/bin' / old).is_symlink())
            self.assertEqual(os.readlink(self.root / 'usr/bin' / old), old.replace('roost-', 'tuna-'))
        self.case['version_relation'] = EVIDENCE.report['version_relations']['candidate-to-beta']

    @classmethod
    def prepare_native_pam(cls):
        if cls.native is not None:
            return
        if cls.native_preflight_error is not None:
            raise RuntimeError('native preflight previously failed: ' + cls.native_preflight_error)
        probe = cls.workspace / 'private-pam-probe'
        source = Path(__file__).with_name('legacy-upgrade-pam-probe.c')
        EVIDENCE.run(['cc', '-std=c11', '-O2', '-Wall', '-Wextra', '-Werror', source, '-ldl', '-o', probe])
        arch = EVIDENCE.run(['cc', '-print-multiarch']).strip()
        if not re.fullmatch(r'[a-z0-9_-]+', arch):
            raise RuntimeError('unexpected native multiarch name')
        libpam = next((path for path in (Path('/lib') / arch / 'libpam.so.0',
                                         Path('/usr/lib') / arch / 'libpam.so.0') if path.exists()), None)
        if libpam is None:
            raise RuntimeError('native libpam.so.0 is required')
        permit = libpam.parent / 'security/pam_permit.so'
        deny = libpam.parent / 'security/pam_deny.so'
        allowed = {'libpam.so.0', 'libaudit.so.1', 'libcap-ng.so.0', 'libc.so.6',
                   'libeconf.so.0', 'libeconf.so.1', 'libdl.so.2', 'libm.so.6',
                   'libpcre2-8.so.0', 'libselinux.so.1', 'libcrypt.so.1',
                   'ld-linux-x86-64.so.2', 'ld-linux-aarch64.so.1',
                   'pam_permit.so', 'pam_deny.so'}
        paths = {libpam, permit, deny}
        EVIDENCE.report['native_preflight'] = {
            'state': 'checking', 'probe_source_sha256': digest(source), 'probe_sha256': digest(probe),
            'ownership_policy': 'Require numeric uid/gid 0 for native system ELF files, links and ancestry; ambiguous namespace overflow ownership is rejected',
            'core_library_metadata': []}
        # Fail BEFORE any ldd or PAM execution if these system libraries cannot
        # be positively identified as root-owned. An overflow UID is ambiguous:
        # it could represent any unmapped outer owner, not necessarily root.
        try:
            for native_file in (libpam, permit, deny):
                metadata = native_file.stat()
                EVIDENCE.report['native_preflight']['core_library_metadata'].append({
                    'path': str(native_file), 'resolved_path': str(native_file.resolve()),
                    'uid': metadata.st_uid, 'gid': metadata.st_gid,
                    'mode': stat.S_IMODE(metadata.st_mode), 'sha256': digest(native_file)})
                cls.check_native_file(native_file, allowed)
            EVIDENCE.report['native_preflight']['state'] = 'passed'
        except Exception as error:
            cls.native_preflight_error = str(error)
            EVIDENCE.report['native_preflight'].update(state='failed-before-ldd-and-pam', error=str(error))
            raise
        finally:
            EVIDENCE.save()
        for native_file in (probe, libpam, permit, deny):
            # Only newly compiled test code and the native, read-only distro
            # library/module paths reach ldd. Release binaries never do.
            if native_file != probe:
                cls.check_native_file(native_file, allowed)
            linked = EVIDENCE.run(['ldd', native_file])
            for line in linked.splitlines():
                line = line.strip()
                if not line or line.startswith('linux-vdso.so.') or line == 'statically linked':
                    continue
                match = re.fullmatch(r'(?:[^ /]+ => )?(/[^\s]+) \(0x[0-9a-f]+\)', line)
                if match is None:
                    raise RuntimeError('unresolved or unexpected native library dependency: ' + line)
                dependency = Path(match[1])
                cls.check_native_file(dependency, allowed)
                paths.add(dependency)
        identities = []
        for path in sorted(paths):
            cls.check_native_file(path, allowed)
            info = path.stat()
            notes = EVIDENCE.run(['readelf', '-n', path])
            identities.append({'path': str(path), 'resolved_path': str(path.resolve()),
                               'sha256': digest(path), 'size': info.st_size, 'uid': info.st_uid,
                               'mode': stat.S_IMODE(info.st_mode), 'elf_notes': notes})
        cls.native = {'probe': probe, 'libpam': libpam, 'permit': permit, 'deny': deny,
                      'identities': identities,
                      'old_package': build_fixture(cls.workspace, 'roost', '0.9.0', 'prerename-pam', probe)}
        EVIDENCE.report['native_build'] = {
            'probe_source_sha256': digest(source), 'probe_sha256': digest(probe),
            'probe_elf_notes': EVIDENCE.run(['readelf', '-n', probe]),
            'abi': 'Linux-PAM five-argument pam_start_confdir(service,user,conv,confdir,pamh); no fallback',
            'trust_boundary': 'Native system ELF files only: canonical system-library paths, numeric uid/gid 0, no group/other-writable file or ancestry; hashes/build IDs recorded. Namespace overflow ownership is rejected. This is host-library mechanism evidence, not a release compositor or distribution runtime qualification.',
            'libraries': identities,
            'package_versions': EVIDENCE.run(['dpkg-query', '-W', '-f=${Package} ${Version}\n',
                                             'libpam0g', 'libpam-modules', 'libc6'], check=False)}
        EVIDENCE.save()

    @staticmethod
    def check_native_file(path, allowed):
        prefixes = ('/lib/', '/lib64/', '/usr/lib/', '/usr/lib64/')
        if path.name not in allowed or not any(str(path).startswith(prefix) for prefix in prefixes):
            raise RuntimeError('native library is not allowlisted: ' + str(path))
        resolved = path.resolve(strict=True)
        if not any(str(resolved).startswith(prefix) for prefix in prefixes):
            raise RuntimeError('native library resolves outside system-library roots: ' + str(path))
        # Check both lexical link ancestry and resolved ancestry. A familiar
        # /lib basename alone is not authority to run ldd on a redirected file.
        for entry in set((path, *path.parents, resolved, *resolved.parents)):
            metadata = entry.lstat()
            if metadata.st_uid != 0 or metadata.st_gid != 0:
                raise RuntimeError('untrusted native library ancestry owner: ' + str(entry))
            if not stat.S_ISLNK(metadata.st_mode) and metadata.st_mode & 0o022:
                raise RuntimeError('writable native library ancestry: ' + str(entry))
        if not path.is_file() or path.stat().st_mode & 0o7022:
            raise RuntimeError('native library must be a non-writable regular file: ' + str(path))
        with path.open('rb') as source:
            if source.read(4) != b'\x7fELF':
                raise RuntimeError('native library is not ELF: ' + str(path))

    def private_pam_case(self, mode):
        self.prepare_native_pam()
        native = self.native
        self.install(native['old_package'])
        # A fresh chroot with copied native ELF closure, never a bind mount.
        # None of these files may overlap a package payload or be mutated by it.
        for identity in native['identities']:
            source = Path(identity['path'])
            relative = str(source).lstrip('/')
            self.assertNotIn(relative, self.candidate['data'])
            destination = self.root / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, destination)
            destination.chmod(0o755 if 'ld-linux' in source.name else 0o644)
            self.assertEqual(digest(destination), identity['sha256'])
        login = f'auth required {native["permit"]}\naccount required {native["permit"]}\n'.encode()
        other = f'auth required {native["deny"]}\naccount required {native["deny"]}\n'.encode()
        (self.root / 'etc/pam.d/login').write_bytes(login)
        (self.root / 'etc/pam.d/other').write_bytes(other)
        for relative in ('etc/pam.d/login', 'etc/pam.d/other'):
            (self.root / relative).chmod(0o644)
        if mode == 'administrator-deny':
            (self.root / OLD_PAM).write_bytes(other)
        elif mode == 'missing-service':
            (self.root / OLD_PAM).unlink()
        sentinel = self.workspace / ('outside-' + self._testMethodName)
        sentinel.write_text('Must never be visible inside the private PAM filesystem.\n')
        self.assertTrue(sentinel.is_file())
        self.assertFalse((self.root / 'proc').exists())
        self.assertFalse((self.root / 'etc/shadow').exists())
        executable = self.root / 'usr/bin/roost-compositor'
        initial = executable.stat()
        argv = [str(executable), str(self.root), str(sentinel), str(native['libpam'])]
        self.record('before-old-process-start')
        child = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, env=clean_environment(), close_fds=True,
                                 bufsize=0)
        record = {'argv': argv, 'event': 'private-pam-process', 'pid': child.pid,
                  'mode': mode, 'environment': clean_environment()}
        try:
            with selectors.DefaultSelector() as poll:
                poll.register(child.stdout, selectors.EVENT_READ)
                # The helper writes one bounded JSON line then waits at a
                # barrier. A failed boundary must never proceed to package change.
                ready_line = b''
                deadline = time.monotonic() + 15
                while not ready_line.endswith(b'\n'):
                    remaining = deadline - time.monotonic()
                    if remaining <= 0 or not poll.select(remaining):
                        raise RuntimeError('native PAM isolation readiness timed out')
                    part = os.read(child.stdout.fileno(), 4096 - len(ready_line))
                    if not part or len(ready_line) + len(part) >= 4096:
                        raise RuntimeError('native PAM readiness missing or oversized')
                    ready_line += part
            record['ready_output'] = ready_line.decode(errors='replace')
            record['ready_sha256'] = hashlib.sha256(ready_line).hexdigest()
            ready = json.loads(ready_line)
            self.assertEqual(ready, {'event': 'ready', 'pid': child.pid, 'chroot_verified': True,
                                    'outside_fds_closed': True, 'capabilities_zero': True,
                                    'host_sentinel_absent': True, 'host_proc_absent': True,
                                    'host_shadow_absent': True, 'traversal_sentinel_absent': True,
                                    'no_new_privs': True, 'pam_loaded': False})
            proc_exe = Path('/proc') / str(child.pid) / 'exe'
            before = proc_exe.stat()
            self.assertEqual((before.st_dev, before.st_ino), (initial.st_dev, initial.st_ino))
            record['process_before'] = {'device': before.st_dev, 'inode': before.st_ino,
                                        'exe_link': os.readlink(proc_exe)}
            self.install(self.candidate, keep_local=mode in ('administrator-deny', 'missing-service'))
            self.assertFalse(os.path.lexists(executable))
            self.assertTrue((self.root / 'usr/bin/tuna-compositor').is_file())
            after = proc_exe.stat()
            self.assertEqual((before.st_dev, before.st_ino), (after.st_dev, after.st_ino))
            record['process_after'] = {'device': after.st_dev, 'inode': after.st_ino,
                                       'exe_link': os.readlink(proc_exe)}
            self.assertTrue(os.readlink(proc_exe).endswith(' (deleted)'))
            for identity in native['identities']:
                self.assertEqual(digest(self.root / identity['path'].lstrip('/')), identity['sha256'])
            self.assert_regular('etc/pam.d/login', login)
            self.assert_regular('etc/pam.d/other', other)
            if mode == 'missing-service':
                self.assertFalse(os.path.lexists(self.root / OLD_PAM))
            elif mode == 'administrator-deny':
                self.assert_regular(OLD_PAM, other)
            else:
                self.assert_regular(OLD_PAM, self.candidate['data'][OLD_PAM]['body'])
            self.record('after-package-change-before-pam-initialization')
            stdout, stderr = child.communicate(b'INITIALIZE\n', timeout=20)
            record.update(exit_code=child.returncode, stdout=stdout.decode(errors='replace'),
                          stdout_sha256=hashlib.sha256(stdout).hexdigest(),
                          stderr=stderr.decode(errors='replace'))
            self.assertEqual(child.returncode, 0, record)
            result = json.loads(stdout)
            record['native_result'] = result
            self.assertEqual((result['event'], result['pid'], result['service'], result['confdir']),
                             ('pam-result', child.pid, 'roost-lock', '/etc/pam.d'))
            self.assertEqual((result['start'], result['end']), (0, 0), record)
            if mode == 'retained-service':
                self.assertEqual((result['authenticate'], result['account']), (0, 0), record)
            else:
                # Linux-PAM pam_deny returns PAM_AUTH_ERR (7) for BOTH hooks.
                # PAM_SYSTEM_ERR (4), loader errors, and all other failures are
                # failed native evidence, never successful deny qualification.
                self.assertEqual((result['authenticate'], result['account']), (7, 7), record)
            self.case['native_pam'] = record
        except Exception as error:
            record['exception'] = repr(error)
            raise
        finally:
            if child.poll() is None:
                child.kill()
            stdout, stderr = child.communicate(timeout=5)
            record.setdefault('exit_code', child.returncode)
            record.setdefault('stdout', stdout.decode(errors='replace'))
            record.setdefault('stderr', stderr.decode(errors='replace'))
            EVIDENCE.log(record)
            self.case['native_pam'] = record
            EVIDENCE.save()

    def test_private_pam_retained_service_allows_native_permit(self):
        self.private_pam_case('retained-service')

    def test_private_pam_missing_service_reaches_private_other_deny(self):
        self.private_pam_case('missing-service')

    def test_private_pam_administrator_deny_is_preserved_and_denies(self):
        self.private_pam_case('administrator-deny')


def main():
    global ARGS, EVIDENCE
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--candidate', type=Path)
    parser.add_argument('--beta', type=Path)
    parser.add_argument('--artifacts', type=Path, required=True)
    parser.add_argument('--dpkg-only', action='store_true')
    ARGS = parser.parse_args()
    if os.geteuid() != 0:
        if shutil.which('unshare') is None:
            parser.error('ordinary-user execution needs unshare; use a disposable root container otherwise')
        # No recursive fallback: inability to create the namespace is fatal.
        result = subprocess.run(['unshare', '--user', '--map-root-user', '--', sys.executable,
                                 str(Path(__file__).resolve()), *sys.argv[1:]], env=clean_environment())
        return result.returncode
    EVIDENCE = Evidence(ARGS.artifacts.resolve())
    print('Evidence: ' + str(EVIDENCE.path), flush=True)
    names = unittest.defaultTestLoader.getTestCaseNames(DisposableLifecycle)
    suite = unittest.TestSuite(DisposableLifecycle(name) for name in names
                              if not ARGS.dpkg_only or not name.startswith('test_private_pam_'))
    result = unittest.TextTestRunner(verbosity=2).run(suite)
    problems = result.failures + result.errors
    dpkg_failed = result.testsRun < 12 or any('test_private_pam_' not in str(test) for test, _ in problems)
    pam_failed = any('test_private_pam_' in str(test) for test, _ in problems) or result.testsRun != 15
    native_status = ('not-run' if ARGS.dpkg_only else 'blocked-preflight'
                     if EVIDENCE.report.get('native_preflight', {}).get('state') == 'failed-before-ldd-and-pam'
                     else 'failed' if pam_failed else 'passed')
    EVIDENCE.report.update(result=('passed-scoped-dpkg-only' if ARGS.dpkg_only else 'passed-scoped-lifecycle')
                           if result.wasSuccessful() else 'failed',
                           dpkg_lifecycle='failed' if dpkg_failed else 'passed',
                           native_pam=native_status,
                           tests_run=result.testsRun,
                           failures=[{'test': str(test), 'traceback': detail} for test, detail in result.failures],
                           errors=[{'test': str(test), 'traceback': detail} for test, detail in result.errors])
    EVIDENCE.save()
    return 0 if result.wasSuccessful() else 1


if __name__ == '__main__':
    raise SystemExit(main())
