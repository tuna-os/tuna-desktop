#!/usr/bin/env python3
# tuna-rename: keep-file
"""Real .deb archive/checker regressions for the exact two-file transition.

Uses native cc, dpkg-deb, ldd and desktop-file-validate. Release-builder
control-flow/staging regressions control cargo, git and dpkg-shlibdeps; staging
uses native ELF fixtures and real production builder/archive/checker tools.
These are package-content, release-staging and staged-Arch tests, not full
release, installed upgrade, native PAM, GDM, old-compositor unlock, pacman
replacement or Marlin qualification.
All writes and executions use disposable roots; no host PAM or accounts change.
"""
import copy
import importlib.util
import io
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
SCRIPT = ROOT / 'scripts/check-release-package'
RELEASE_SCRIPT = ROOT / 'scripts/tuna-release'
_spec = importlib.util.spec_from_file_location('transition_policy', ROOT / 'scripts/lib/transition-package-policy.py')
M = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(M)
# Expectations are maintained here, never imported from the policy under test.
BINS = ('tuna-compositor', 'tuna-session', 'tuna-shell-gtk', 'tuna-shell-host',
        'tuna-ibus-bridge', 'tuna-wallpaper-svg')
SESSION = 'usr/share/wayland-sessions/tuna.desktop'
OLD_SESSION = 'usr/share/wayland-sessions/roost.desktop'
PAM = 'etc/pam.d/tuna-lock'
OLD_PAM = 'etc/pam.d/roost-lock'
DESKTOP = '''[Desktop Entry]
Name=Tuna Desktop
Comment=Tuna Desktop (GNOME-style shell with optional scrollable tiling)
Exec=tuna-session
TryExec=tuna-session
Type=Application
DesktopNames=Tuna;GNOME;
X-GDM-SessionRegisters=true
'''
PAM_TEXT = 'auth include login\naccount include login\n'
CONFFILES = '/etc/pam.d/tuna-lock\n/etc/pam.d/roost-lock\n'


def member(name, body=b'', kind=tarfile.REGTYPE, mode=0o644, target=''):
    item = tarfile.TarInfo(name)
    item.type, item.mode, item.linkname = kind, mode, target
    item.uid = item.gid = 0
    item.uname = item.gname = 'root'
    if isinstance(body, str):
        body = body.encode()
    item.size = len(body) if kind == tarfile.REGTYPE else 0
    return item, body


def tar_bytes(entries):
    out = io.BytesIO()
    with tarfile.open(fileobj=out, mode='w', format=tarfile.USTAR_FORMAT) as archive:
        for item, body in entries:
            archive.addfile(item, io.BytesIO(body) if item.isfile() else None)
    return out.getvalue()


def read_tar(raw):
    with tarfile.open(fileobj=io.BytesIO(raw), mode='r:') as archive:
        return [(copy.copy(item), archive.extractfile(item).read() if item.isfile() else b'')
                for item in archive]


def deb_bytes(data, control):
    # A plain ar container preserves deliberately malformed/duplicate tar names.
    # Real dpkg-deb must read it; no extraction or installer is used to make it.
    result = bytearray(b'!<arch>\n')
    for name, raw in (('debian-binary', b'2.0\n'), ('control.tar', control), ('data.tar', data)):
        result.extend(f'{name + "/":<16}{0:<12}{0:<6}{0:<6}{0o100644:<8o}{len(raw):<10}`\n'.encode())
        result.extend(raw)
        if len(raw) % 2:
            result.extend(b'\n')
    return bytes(result)


def replace(entries, name, replacement=None):
    result = [(item, body) for item, body in entries if item.name.removeprefix('./') != name]
    if replacement is not None:
        result.append(replacement)
    return result


class ReleaseVersionSelection(unittest.TestCase):
    """Exercise real CLI/control flow, stopping at the controlled Cargo call."""

    def release(self, args, *, exact_tag='', dirty='', review=None):
        with tempfile.TemporaryDirectory(prefix='tuna-release-version-') as temp:
            work = Path(temp)
            delegates = work / 'delegates'
            delegates.mkdir()
            trace = work / 'trace'
            commands = {
                'git': '''printf 'git %s\\n' "$*" >>"$TRACE"
case "$*" in
    'status --porcelain') printf '%s' "$DIRTY" ;;
    'describe --tags --exact-match --match v[0-9]* HEAD')
        test -n "$EXACT_TAG" || exit 1
        printf '%s\\n' "$EXACT_TAG" ;;
    'describe --tags --match v[0-9]*') printf '%s\\n' v0.1.0-beta.2-7-gabcdef ;;
    *) exit 2 ;;
esac
''',
                'cargo': '''printf 'cargo version=%s\\n' "$TUNA_VERSION" >>"$TRACE"
exit 91
''',
            }
            for name, body in commands.items():
                path = delegates / name
                path.write_text('#!/bin/sh\nset -eu\n' + body)
                path.chmod(0o755)
            if review is not None:
                path = work / 'docs/reviews' / f'{exact_tag}.md'
                path.parent.mkdir(parents=True)
                path.write_text(review)
            result = subprocess.run([str(RELEASE_SCRIPT), *args], cwd=work,
                env=dict(os.environ, PATH=str(delegates) + os.pathsep + os.environ['PATH'],
                         TRACE=str(trace), EXACT_TAG=exact_tag, DIRTY=dirty,
                         TUNA_VERSION='ambient-version-must-not-win'),
                text=True, capture_output=True, timeout=10)
            return result, trace.read_text() if trace.exists() else ''

    def test_help_documents_explicit_candidate_and_clean_tree_policy(self):
        result, trace = self.release(['--help'])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('--candidate-version VERSION', result.stdout)
        self.assertIn('X.Y.Z, X.Y.Z-beta.N, or X.Y.Z-rc.N', result.stdout)
        self.assertIn('no leading zeros', result.stdout)
        self.assertIn('build metadata is not supported', result.stdout)
        self.assertIn('Candidates still require a clean tree', result.stdout)
        self.assertEqual(trace, '')

    def test_candidate_overrides_nearest_beta2_tag_and_ambient_stamp(self):
        for version in ('0.1.0-beta.3', '1.2.3', '1.2.3-rc.1', '0.0.0-beta.0'):
            with self.subTest(version=version):
                result, trace = self.release(['--candidate-version', version])
                self.assertEqual(result.returncode, 91, result.stderr)
                self.assertIn(f'cargo version={version}\n', trace)
                self.assertIn('git status --porcelain\n', trace)
                self.assertNotIn('git describe --tags --match', trace)

    def test_candidate_rejects_missing_empty_and_malformed_semver_before_build(self):
        cases = [[], [''], ['v0.1.0-beta.3'], ['0.1'], ['01.2.3'], ['1.02.3'],
                 ['1.2.03'], ['1.2.3-01'], ['1.2.3-beta.03'], ['1.2.3-'],
                 ['1.2.3+'], ['1.2.3-beta..3'], ['1.2.3 beta.3'],
                 ['1.2.3\n0.1.0-beta.3'], ['--allow-dirty'], ['1.2.3+build-'],
                 ['1.2.3+build.7'], ['1.2.3+build-with-hyphen'], ['1.2.3-beta-'],
                 ['1.2.3-beta.3+build.7'], ['1.2.3-rc.1+build.7'], ['1.2.3-alpha.1'],
                 ['1.2.3-beta'], ['1.2.3-rc.01'], ['1.2.3-preview.1'], ['1.2.3-1']]
        for value in cases:
            with self.subTest(value=value):
                result, trace = self.release(['--candidate-version', *value])
                self.assertNotEqual(result.returncode, 0)
                self.assertIn('--candidate-version requires', result.stderr)
                self.assertIn('X.Y.Z, X.Y.Z-beta.N, or X.Y.Z-rc.N', result.stderr)
                self.assertEqual(trace, '')

    def test_candidate_is_refused_on_any_exact_release_tag_even_allow_dirty(self):
        for tag in ('v0.1.0-beta.2', 'v0.1.0-beta.3', 'v1.2.3'):
            for flags in ([], ['--allow-dirty']):
                with self.subTest(tag=tag, flags=flags):
                    result, trace = self.release(
                        ['--candidate-version', '0.1.0-beta.3', *flags], exact_tag=tag)
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn('not allowed on an exact release tag', result.stderr)
                    self.assertNotIn('cargo version=', trace)

    def test_candidate_keeps_dirty_tree_refusal_unless_explicitly_allowed(self):
        result, trace = self.release(['--candidate-version', '0.1.0-beta.3'], dirty=' M tracked\n')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('working tree is dirty', result.stderr)
        self.assertNotIn('cargo version=', trace)
        result, trace = self.release(
            ['--candidate-version', '0.1.0-beta.3', '--allow-dirty'], dirty=' M tracked\n')
        self.assertEqual(result.returncode, 91, result.stderr)
        self.assertIn('cargo version=0.1.0-beta.3\n', trace)

    def test_normal_tagged_release_keeps_tag_stamp_and_required_review(self):
        for review, error in ((None, 'missing visual review'), ('TBD\n', 'still has TBD')):
            with self.subTest(review=review):
                result, trace = self.release([], exact_tag='v0.1.0-beta.3', review=review)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(error, result.stderr)
                self.assertNotIn('cargo version=', trace)
        result, trace = self.release([], exact_tag='v0.1.0-beta.3', review='Recorded visual review\n')
        self.assertEqual(result.returncode, 91, result.stderr)
        self.assertIn('cargo version=0.1.0-beta.3\n', trace)

    def test_existing_allow_dirty_nearest_tag_stamp_is_unchanged(self):
        result, trace = self.release(['--allow-dirty'])
        self.assertEqual(result.returncode, 91, result.stderr)
        self.assertIn('cargo version=0.1.0-beta.2\n', trace)


class GenuineTransitionPackages(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # Missing native dependencies must fail this suite, rather than silently
        # converting its required genuine-package coverage into skipped tests.
        for tool in ('cc', 'dpkg-deb', 'ldd', 'desktop-file-validate', 'bash'):
            if shutil.which(tool) is None:
                raise RuntimeError(f'required package-test dependency missing: {tool}')
        cls.temp = tempfile.TemporaryDirectory(prefix='tuna-transition-')
        cls.addClassCleanup(cls.temp.cleanup)
        cls.root = Path(cls.temp.name)
        cls.stage = cls.root / 'stage'
        cls.stage.mkdir(mode=0o755)
        source = cls.root / 'version.c'
        source.write_text('''#include <stdio.h>
#include <string.h>
int main(int argc, char **argv) {
    const char *name = strrchr(argv[0], '/');
    if (argc != 2 || strcmp(argv[1], "--version")) return 2;
    printf("%s 0.1.0-beta.1\\n", name ? name + 1 : argv[0]);
    return 0;
}
''')
        cls.elf = cls.root / 'native-version'
        subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', str(source), '-o', str(cls.elf)], check=True, timeout=120)
        for name in BINS:
            path = cls.stage / 'usr/bin' / name
            path.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(cls.elf, path)
            path.chmod(0o755)
        for name, body in ((SESSION, DESKTOP), (OLD_SESSION, DESKTOP + 'NoDisplay=true\n'),
                           (PAM, PAM_TEXT), (OLD_PAM, PAM_TEXT),
                           ('usr/lib/systemd/user/tuna-session.target', '[Unit]\nDescription=Fixture session\n'),
                           ('DEBIAN/conffiles', CONFFILES), ('DEBIAN/control', '''Package: tuna-desktop
Version: 0.1.0~beta.1
Architecture: amd64
Maintainer: Tuna fixture <fixture@example.invalid>
Description: disposable archive policy fixture
Depends: libgtk4-layer-shell0 (>= 1.1), libgtk-4-1 (>= 4.12), libadwaita-1-0 (>= 1.0), libpam0g, gsettings-desktop-schemas, gnome-settings-daemon-common
Replaces: roost
Conflicts: roost
Provides: roost
''')):
            path = cls.stage / name
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(body)
            path.chmod(0o644)
        shutil.copytree(ROOT / 'share/tuna-desktop/icons', cls.stage / 'usr/share/tuna-desktop/icons', symlinks=True)
        for path in cls.stage.rglob('*'):
            if not path.is_symlink():
                path.chmod(0o755 if path.is_dir() or path.parent == cls.stage / 'usr/bin' else 0o644)
        cls.baseline = cls.root / 'genuine.deb'
        subprocess.run(['dpkg-deb', '--root-owner-group', '-Znone', '--build', str(cls.stage), str(cls.baseline)],
                       check=True, capture_output=True, timeout=120)
        cls.data = read_tar(subprocess.check_output(['dpkg-deb', '--fsys-tarfile', str(cls.baseline)], timeout=120))
        cls.control = read_tar(subprocess.check_output(['dpkg-deb', '--ctrl-tarfile', str(cls.baseline)], timeout=120))

    def check(self, data=None, control=None):
        path = self.root / 'candidate.deb'
        path.write_bytes(deb_bytes(tar_bytes(self.data if data is None else data),
                                   tar_bytes(self.control if control is None else control)))
        return subprocess.run([str(SCRIPT), str(path)], text=True, capture_output=True, timeout=30)

    def reject(self, *, data=None, control=None, reason):
        result = self.check(data, control)
        self.assertNotEqual(result.returncode, 0, result.stdout)
        self.assertIn(reason, result.stderr, result.stderr)
        self.assertNotIn(' OK (version ', result.stdout)

    def test_genuine_package_has_exact_regular_bridge_and_native_binaries(self):
        result = subprocess.run([str(SCRIPT), str(self.baseline)], text=True, capture_output=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn('OK (version 0.1.0~beta.1)', result.stdout)
        entries = {item.name.removeprefix('./'): (item, body) for item, body in self.data}
        self.assertEqual({name for name in entries if 'roost' in name.lower()}, {OLD_SESSION, OLD_PAM})
        for path, body in ((OLD_SESSION, (DESKTOP + 'NoDisplay=true\n').encode()), (OLD_PAM, PAM_TEXT.encode())):
            item, content = entries[path]
            self.assertTrue(item.isfile())
            self.assertEqual((item.mode, item.uid, item.gid), (0o644, 0, 0))
            self.assertEqual(content, body)
        for name in BINS:
            self.assertEqual(entries['usr/bin/' + name][1][:4], b'\x7fELF')
        self.assertEqual(next(body.decode() for item, body in self.control if item.name == './conffiles'), CONFFILES)

    def test_actual_release_builder_preserves_safe_archive_modes_under_umask_002(self):
        """Exercise production staging, with controlled build/tag/ABI delegates.

        Native cc-built ELF fixtures replace Cargo output; git supplies the
        fixture tag and dpkg-shlibdeps supplies a bounded dependency string.
        Neither dpkg-deb nor the archive, desktop or ELF validators is mocked.
        This proves staging metadata, not a real release build/ABI closure.
        """
        work = self.root / 'release-builder'
        target = work / 'target/release'
        target.mkdir(parents=True)
        for name in BINS:
            shutil.copyfile(self.elf, target / name)
        # The actual builder reads checked-in assets and cursor validation;
        # only its output, build fixtures and staging roots are disposable.
        for name in ('share', 'packaging', 'scripts'):
            (work / name).symlink_to(ROOT / name, target_is_directory=True)
        delegates = work / 'controlled-delegates'
        delegates.mkdir()
        commands = {
            'cargo': '''test "$*" = 'build --locked --release -p tuna-compositor -p tuna-wallpaper-svg -p tuna-shell-gtk -p tuna-shell-host'
test "$TUNA_VERSION" = '0.1.0-beta.1'
''',
            'git': '''test "$*" = 'describe --tags --exact-match --match v[0-9]* HEAD'
printf '%s\\n' v0.1.0-beta.1
''',
            'dpkg-shlibdeps': '''test "$*" = '-O -eusr/bin/tuna-compositor -eusr/bin/tuna-session -eusr/bin/tuna-shell-gtk -eusr/bin/tuna-shell-host -eusr/bin/tuna-ibus-bridge -eusr/bin/tuna-wallpaper-svg'
printf '%s\\n' 'shlibs:Depends=libc6 (>= 2.34)'
''',
        }
        for name, body in commands.items():
            path = delegates / name
            path.write_text('#!/bin/sh\nset -eu\n' + body)
            path.chmod(0o755)
        staging_tmp = work / 'tmp'
        staging_tmp.mkdir()
        result = subprocess.run([str(RELEASE_SCRIPT), '--allow-dirty', '--out', str(work / 'dist')],
            cwd=work, env=dict(os.environ, PATH=str(delegates) + os.pathsep + os.environ['PATH'],
                               TMPDIR=str(staging_tmp)),
            text=True, capture_output=True, timeout=60, umask=0o002)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        package = work / 'dist/tuna-desktop_0.1.0~beta.1_amd64.deb'
        self.assertTrue(package.is_file(), result.stdout)
        # Inspect original archives, so extraction cannot conceal unsafe modes.
        data = {item.name.removeprefix('./'): (item, body) for item, body in
                read_tar(subprocess.check_output(['dpkg-deb', '--fsys-tarfile', str(package)], timeout=120))}
        control = {item.name.removeprefix('./'): (item, body) for item, body in
                   read_tar(subprocess.check_output(['dpkg-deb', '--ctrl-tarfile', str(package)], timeout=120))}
        for name in ('etc', 'etc/pam.d', 'usr', 'usr/bin', 'usr/share', 'usr/share/wayland-sessions'):
            with self.subTest(ancestor=name):
                item, _ = data[name]
                self.assertTrue(item.isdir())
                self.assertEqual((item.mode, item.uid, item.gid), (0o755, 0, 0))
        for name in ('control', 'conffiles'):
            with self.subTest(control_member=name):
                item, _ = control[name]
                self.assertTrue(item.isfile())
                self.assertEqual((item.mode, item.uid, item.gid), (0o644, 0, 0))
        self.assertEqual(control['conffiles'][1].decode(), CONFFILES)
        for name, (item, _) in data.items():
            if name.startswith('usr/share/tuna-desktop/') and (item.isdir() or item.isfile()):
                with self.subTest(cursor_member=name):
                    self.assertEqual(item.mode, 0o755 if item.isdir() else 0o644)
        self.assertEqual({name for name in data if 'roost' in name.lower()}, {OLD_SESSION, OLD_PAM})
        for name in BINS:
            self.assertEqual(data['usr/bin/' + name][1][:4], b'\x7fELF')
        checked = subprocess.run([str(SCRIPT), str(package)],
            text=True, capture_output=True, timeout=30, umask=0o002)
        self.assertEqual(checked.returncode, 0, checked.stdout + checked.stderr)
        self.assertIn('OK (version 0.1.0~beta.1)', checked.stdout)

    def test_actual_candidate_builder_stamps_beta3_binaries_and_debian_control(self):
        """Controlled Cargo/ABI delegates, but real staging, ELF and .deb tools."""
        for index, (version, deb_version) in enumerate((
                ('0.1.0-beta.3', '0.1.0~beta.3'),
                ('1.2.3-rc.1', '1.2.3~rc.1'))):
            with self.subTest(version=version):
                work = self.root / f'candidate-builder-{index}'
                target = work / 'target/release'
                target.mkdir(parents=True)
                source = work / 'version.c'
                source.write_text((self.root / 'version.c').read_text().replace('0.1.0-beta.1', version))
                elf = work / 'native-version'
                subprocess.run(['cc', '-Wall', '-Wextra', '-Werror', str(source), '-o', str(elf)],
                               check=True, capture_output=True, timeout=120)
                for name in BINS:
                    shutil.copyfile(elf, target / name)
                for name in ('share', 'packaging', 'scripts'):
                    (work / name).symlink_to(ROOT / name, target_is_directory=True)
                delegates = work / 'delegates'
                delegates.mkdir()
                commands = {
                    'cargo': '''test "$*" = 'build --locked --release -p tuna-compositor -p tuna-wallpaper-svg -p tuna-shell-gtk -p tuna-shell-host'
test "$TUNA_VERSION" = "$EXPECTED_VERSION"
''',
                    'git': '''case "$*" in
    'status --porcelain') exit 0 ;;
    'describe --tags --exact-match --match v[0-9]* HEAD') exit 1 ;;
    'describe --tags --match v[0-9]*') printf '%s\\n' v0.1.0-beta.2-7-gabcdef ;;
    *) exit 2 ;;
esac
''',
                    'dpkg-shlibdeps': '''test "$*" = '-O -eusr/bin/tuna-compositor -eusr/bin/tuna-session -eusr/bin/tuna-shell-gtk -eusr/bin/tuna-shell-host -eusr/bin/tuna-ibus-bridge -eusr/bin/tuna-wallpaper-svg'
printf '%s\\n' 'shlibs:Depends=libc6 (>= 2.34)'
''',
                }
                for name, body in commands.items():
                    path = delegates / name
                    path.write_text('#!/bin/sh\nset -eu\n' + body)
                    path.chmod(0o755)
                result = subprocess.run(
                    [str(RELEASE_SCRIPT), '--candidate-version', version, '--out', str(work / 'dist')],
                    cwd=work, env=dict(os.environ,
                        PATH=str(delegates) + os.pathsep + os.environ['PATH'],
                        EXPECTED_VERSION=version, TUNA_VERSION='0.1.0-beta.2'),
                    text=True, capture_output=True, timeout=60)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                package = work / f'dist/tuna-desktop_{deb_version}_amd64.deb'
                self.assertTrue(package.is_file(), result.stdout)
                control_version = subprocess.check_output(
                    ['dpkg-deb', '-f', str(package), 'Version'], text=True, timeout=30).strip()
                self.assertEqual(control_version, deb_version)
                extracted = work / 'extracted'
                subprocess.run(['dpkg-deb', '-x', str(package), str(extracted)],
                               check=True, capture_output=True, timeout=30)
                for name in BINS:
                    reported = subprocess.check_output(
                        [str(extracted / 'usr/bin' / name), '--version'], text=True, timeout=10)
                    self.assertEqual(reported, f'{name} {version}\n')
                if version == '0.1.0-beta.3':
                    checked = subprocess.run([str(SCRIPT), str(package)],
                        text=True, capture_output=True, timeout=30)
                    self.assertEqual(checked.returncode, 0, checked.stdout + checked.stderr)
                    self.assertIn('OK (version 0.1.0~beta.3)', checked.stdout)

    def test_actual_loader_rejects_a_version_spoofing_shell_script(self):
        path = 'usr/bin/tuna-shell-gtk'
        self.reject(data=replace(self.data, path, member(path,
                    '#!/bin/sh\nprintf "tuna-shell-gtk 0.1.0-beta.1\\n"\n', mode=0o755)),
                    reason='tuna-shell-gtk ELF loader check failed')

    def test_current_asset_aliases_and_same_directory_link_chain_are_valid(self):
        aliases = [member('usr/share/tuna-desktop/alias-source', 'current asset\n'),
                   member('usr/share/tuna-desktop/alias-one', kind=tarfile.SYMTYPE, target='alias-source'),
                   member('usr/share/tuna-desktop/alias-two', kind=tarfile.SYMTYPE, target='alias-one')]
        result = self.check(self.data + aliases)
        self.assertEqual(result.returncode, 0, result.stderr)
        shipped = {item.name.removeprefix('./'): item.linkname for item, _ in self.data if item.issym()}
        self.assertEqual(shipped, {
            'usr/share/tuna-desktop/icons/Adwaita/cursors_scalable/move': 'default',
            'usr/share/tuna-desktop/icons/Adwaita/cursors_scalable/dnd-ask': 'context-menu',
        })

    def test_forbidden_old_binaries_extra_session_pam_and_directories(self):
        for path in ('usr/bin/roost-session', 'usr/bin/roost-compositor',
                     'usr/bin/ROOST-shell', 'usr/share/wayland-sessions/roost-extra.desktop',
                     'etc/pam.d/roost-unlock', 'usr/share/roost',
                     'usr/share/wayland-sessions/roost.desktop.bak'):
            for kind in (tarfile.REGTYPE, tarfile.SYMTYPE, tarfile.DIRTYPE):
                with self.subTest(path=path, kind=kind):
                    self.reject(data=self.data + [member(path, 'forbidden\n', kind=kind, target='tuna-session')],
                                reason='forbidden-legacy-path')

    def test_missing_each_current_or_transition_file_is_rejected(self):
        for path in (SESSION, OLD_SESSION, PAM, OLD_PAM):
            with self.subTest(path=path):
                self.reject(data=replace(self.data, path), reason='transition-file-missing')

    def test_transition_regular_file_and_mode_requirements(self):
        for path in (SESSION, OLD_SESSION, PAM, OLD_PAM):
            for kind in (tarfile.SYMTYPE, tarfile.DIRTYPE, tarfile.LNKTYPE, tarfile.FIFOTYPE):
                with self.subTest(path=path, kind=kind):
                    self.reject(data=replace(self.data, path, member(path, kind=kind, target='tuna-lock')),
                                reason='transition-regular-mode' if kind in (tarfile.SYMTYPE, tarfile.DIRTYPE) else 'archive-entry-type')
            for mode in (0o600, 0o664, 0o755, 0o4644):
                with self.subTest(path=path, mode=oct(mode)):
                    self.reject(data=replace(self.data, path, member(path, 'content\n', mode=mode)),
                                reason='transition-regular-mode')

    def test_desktop_and_pam_content_are_exact(self):
        for path in (SESSION, OLD_SESSION):
            original = DESKTOP + ('NoDisplay=true\n' if path == OLD_SESSION else '')
            mutations = (original.replace('Exec=tuna-session', 'Exec=roost-session'),
                         original.replace('TryExec=tuna-session', 'TryExec=/bin/true'),
                         original.replace('DesktopNames=Tuna;GNOME;', 'DesktopNames=Roost;'),
                         original.replace('X-GDM-SessionRegisters=true', 'X-GDM-SessionRegisters=false'),
                         original + 'Hidden=true\n', original + 'Exec=tuna-session\n',
                         original.replace('[Desktop Entry]', '[Other]'),
                         original + '[Desktop Action Launch]\nExec=tuna-session\n')
            for content in mutations:
                with self.subTest(path=path, content=content):
                    self.reject(data=replace(self.data, path, member(path, content)), reason='transition-')
        for content in (DESKTOP, DESKTOP + 'NoDisplay=false\n', DESKTOP + 'NoDisplay=true\nX-Extra=true\n'):
            with self.subTest(legacy_visibility=content):
                self.reject(data=replace(self.data, OLD_SESSION, member(OLD_SESSION, content)), reason='transition-legacy-desktop-content')
        for path in (PAM, OLD_PAM):
            for content in ('auth include login\n', 'auth include system-auth\naccount include login\n',
                            'auth include login\naccount include login extra\n',
                            PAM_TEXT + 'auth sufficient pam_permit.so\n', 'account include login\nauth include login\n'):
                with self.subTest(path=path, content=content):
                    self.reject(data=replace(self.data, path, member(path, content)), reason='transition-pam-content')

    def test_transition_text_is_bounded_and_utf8(self):
        for content in (b'', b'x' * (16 * 1024 + 1), b'\xff'):
            with self.subTest(content_size=len(content)):
                self.reject(data=replace(self.data, OLD_PAM, member(OLD_PAM, content)),
                            reason='transition-text-bound' if content != b'\xff' else 'original archive rejected')

    def test_exact_two_conffiles_and_no_maintainer_hooks(self):
        for content in ('', '/etc/pam.d/tuna-lock\n', '/etc/pam.d/roost-lock\n',
                        '/etc/pam.d/tuna-lock\n/etc/pam.d/tuna-lock\n', CONFFILES + '/etc/extra\n',
                        'etc/pam.d/tuna-lock\n/etc/pam.d/roost-lock\n', CONFFILES + '\n',
                        '/etc/pam.d/tuna-lock remove-on-upgrade\n/etc/pam.d/roost-lock\n'):
            with self.subTest(content=content):
                self.reject(control=replace(self.control, 'conffiles', member('conffiles', content)),
                            reason='transition-text-bound' if not content else 'transition-conffiles')
        self.reject(control=replace(self.control, 'conffiles'), reason='transition-conffiles')
        for kind, mode in ((tarfile.SYMTYPE, 0o644), (tarfile.REGTYPE, 0o600),
                           (tarfile.REGTYPE, 0o664)):
            with self.subTest(conffile_kind=kind, mode=mode):
                self.reject(control=replace(self.control, 'conffiles', member('conffiles', CONFFILES, kind, mode)), reason='transition-regular-mode')
        for hook in ('preinst', 'postinst', 'prerm', 'postrm', 'config', 'triggers'):
            with self.subTest(hook=hook):
                self.reject(control=self.control + [member(hook, '#!/bin/sh\nexit 0\n', mode=0o755)], reason='transition-lifecycle-hook')

    def test_original_ambiguous_paths_are_rejected_without_normalization(self):
        for path in ('/usr/share/extra', '../outside', '././usr/share/extra',
                     'usr/../outside', 'usr//share/extra', 'usr/./share/extra',
                     'usr\\share\\extra', 'usr/share/extra\nname', 'usr/share/extra\x7f'):
            for kind in ('data', 'control'):
                with self.subTest(path=path, archive=kind):
                    baseline = self.data if kind == 'data' else self.control
                    self.reject(**{kind: baseline + [member(path, 'payload\n')]}, reason='archive-path-ambiguous')
        self.reject(data=self.data + [member('usr/share/extra//', kind=tarfile.DIRTYPE)], reason='archive-path-ambiguous')

    def test_duplicate_paths_include_dot_prefix_and_bridge(self):
        for name in (OLD_SESSION, './' + OLD_SESSION, 'usr/bin/tuna-session', './usr/bin/tuna-session'):
            with self.subTest(name=name):
                self.reject(data=self.data + [member(name, DESKTOP + 'NoDisplay=true\n')], reason='archive-path-duplicate')
        self.reject(control=self.control + [member('conffiles', CONFFILES)], reason='archive-path-duplicate')

    def test_required_transition_ancestors_reject_group_or_other_writes(self):
        for path in ('etc', 'etc/pam.d', 'usr', 'usr/share', 'usr/share/wayland-sessions'):
            for mode in (0o775, 0o757, 0o777, 0o1777):
                with self.subTest(path=path, mode=oct(mode)):
                    self.reject(data=replace(self.data, path, member(path, kind=tarfile.DIRTYPE, mode=mode)),
                                reason='transition-parent-writable')

    def test_non_directory_ancestor_rejected_in_either_archive_order(self):
        for kind in (tarfile.REGTYPE, tarfile.SYMTYPE):
            entries = [member('usr/share/alias-parent', 'parent', kind=kind, target='tuna-desktop'),
                       member('usr/share/alias-parent/child', 'child')]
            for ordered in (entries, entries[::-1]):
                with self.subTest(kind=kind, child_first=ordered is not entries):
                    self.reject(data=self.data + ordered, reason='archive-parent-not-directory')

    def test_dangling_escaping_and_cyclic_asset_links_rejected(self):
        for target in ('missing', '/etc/passwd', '../../outside', 'sub/../outside',
                       'sub//file', '././missing', ''):
            with self.subTest(target=target):
                self.reject(data=self.data + [member('usr/share/alias', kind=tarfile.SYMTYPE, target=target)], reason='archive-')
        for links in ([('one', 'one')], [('one', 'two'), ('two', 'one')],
                      [('one', 'two'), ('two', 'missing')]):
            with self.subTest(links=links):
                self.reject(data=self.data + [member('usr/share/' + name, kind=tarfile.SYMTYPE, target=target) for name, target in links],
                            reason='archive-link-dangling-or-cycle')

    def test_malformed_tar_and_unsupported_entries_fail_actual_checker(self):
        for payload in (b'not a tar', b''):
            path = self.root / 'malformed.deb'
            path.write_bytes(deb_bytes(payload, tar_bytes(self.control)))
            result = subprocess.run([str(SCRIPT), str(path)], text=True, capture_output=True, timeout=30)
            self.assertNotEqual(result.returncode, 0)
            self.assertIn('archive ownership rejected', result.stderr)
        for kind in (tarfile.LNKTYPE, tarfile.CHRTYPE, tarfile.BLKTYPE, tarfile.FIFOTYPE):
            with self.subTest(kind=kind):
                self.reject(data=self.data + [member('usr/share/special', kind=kind, target='tuna-desktop')], reason='archive-entry-type')

    def test_direct_archive_entry_and_text_bounds_use_real_tar(self):
        # Unit boundary checks supplement (never replace) genuine checker cases.
        for raw, kind in ((b'', 'data'), (tar_bytes([member('file', 'x')]), 'unknown')):
            with self.subTest(kind=kind), self.assertRaisesRegex(ValueError, 'archive-bounds'):
                M.archive_index(raw, kind)
        raw = tar_bytes([member(f'file-{number}', 'x') for number in range(4097)])
        with self.assertRaisesRegex(ValueError, 'archive-entry-bound'):
            M.archive_index(raw, 'data')

    def test_actual_arch_package_stages_regular_bridge_and_declares_exact_backup(self):
        target = self.root / 'arch-target/release'
        target.mkdir(parents=True)
        for name in BINS:
            shutil.copyfile(self.elf, target / name)
        stage = self.root / 'arch-stage'
        stage.mkdir()
        result = subprocess.run(['bash', '-euc', '''source "$1"
printf '%s\n' "${backup[@]}"
test -z "${install:-}"
package
''', 'fixture', str(ROOT / 'packaging/arch/PKGBUILD')],
            env=dict(os.environ, TUNA_LOCAL_SRC=str(ROOT), CARGO_TARGET_DIR=str(target.parent),
                     pkgdir=str(stage), srcdir=str(self.root)), text=True, capture_output=True, timeout=30, umask=0o002)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), ['etc/pam.d/tuna-lock', 'etc/pam.d/roost-lock'])
        for name in ('usr', 'usr/share', 'usr/share/wayland-sessions', 'etc', 'etc/pam.d'):
            self.assertEqual((stage / name).stat().st_mode & 0o7777, 0o755, name)
        self.assertEqual({str(path.relative_to(stage)) for path in stage.rglob('*') if 'roost' in path.name.lower()},
                         {OLD_SESSION, OLD_PAM})
        for name in (OLD_SESSION, OLD_PAM, PAM):
            path = stage / name
            self.assertTrue(path.is_file())
            self.assertFalse(path.is_symlink())
            self.assertEqual(path.stat().st_mode & 0o7777, 0o644)
        self.assertEqual((stage / OLD_SESSION).read_text(), DESKTOP + 'NoDisplay=true\n')
        for name in (OLD_PAM, PAM):
            rules = [line.split() for line in (stage / name).read_text().splitlines() if line.strip() and not line.startswith('#')]
            self.assertEqual(rules, [['auth', 'include', 'login'], ['account', 'include', 'login']])


if __name__ == '__main__':
    unittest.main()
