#!/usr/bin/env python3
"""Unit-only controlled-tool/source policies; not package lifecycle evidence.

Genuine archive/native ELF checks live in transition-package-policy-tests.py.
"""
import importlib.util
import json
import os
import shutil
from pathlib import Path
import subprocess
import tempfile
import unittest
import io
from types import SimpleNamespace
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
spec = importlib.util.spec_from_file_location('installed', ROOT / 'scripts/lib/installed-deb-proof.py')
M = importlib.util.module_from_spec(spec)
spec.loader.exec_module(M)


class InstalledPolicy(unittest.TestCase):
    def test_release_keeps_single_versioned_gtk_and_sound_dependencies(self):
        source = (ROOT / 'scripts/tuna-release').read_text()
        depends = next(line for line in source.splitlines() if line.startswith('Depends: '))
        fields = depends[len('Depends: '):].split(', ')
        for name, floor in [('libgtk-4-1', '(>= 4.12)'), ('libadwaita-1-0', '(>= 1.0)'), ('libgtk4-layer-shell0', '(>= 1.1)')]:
            matches = [field for field in fields if field.split()[0] == name]
            self.assertEqual(matches, [name + ' ' + floor])
        for name in ['gnome-session-canberra', 'libcanberra-pulse', 'sound-theme-freedesktop']:
            self.assertEqual(fields.count(name), 1)
        self.assertTrue(fields[0].startswith('$SHLIBS'))

    def process_case(self, inode=20, replacement=None, changed_field=None):
        original = {'sha256': 'a' * 64, 'dev': 1, 'ino': 20, 'size': 100}
        executed = SimpleNamespace(st_dev=1, st_ino=inode, st_size=100,
                                   st_uid=0, st_mode=0o100755, st_mtime_ns=10, st_ctime_ns=11)
        current = SimpleNamespace(**vars(executed))
        if replacement is not None:
            current.st_ino = replacement
        tail = ['S', '1'] + ['0'] * 17 + ['123'] + ['0'] * 4
        reads = {'stat': 0, 'status': 0}
        def text(path, *args, **kwargs):
            key = path.name
            reads[key] += 1
            if key == 'stat':
                after = tail[:]
                if reads[key] == 2 and changed_field in ('start', 'parent'):
                    after[19 if changed_field == 'start' else 1] = '999'
                return '99 (tuna-compositor) ' + ' '.join(after)
            if key == 'status':
                uid = 999 if reads[key] == 2 and changed_field == 'uid' else 1000
                return 'Uid:\t' + '\t'.join([str(uid)] * 4) + '\n'
            raise AssertionError('unexpected process read')
        links = ['/usr/bin/tuna-compositor'] * 2
        if changed_field == 'exe':
            links[1] = '/usr/bin/replaced'
        with patch.object(Path, 'read_text', text), patch.object(os, 'getuid', return_value=1000), \
             patch.object(os, 'readlink', side_effect=links), patch.object(os, 'open', return_value=50) as opened, \
             patch.object(os, 'fstat', return_value=executed), patch.object(os, 'stat', return_value=current), \
             patch.object(os, 'close') as closed, patch.object(M, 'digest', return_value=original) as digest:
            try:
                return M.process(99, 'tuna-compositor', {'tuna-compositor': original})
            finally:
                opened.assert_called_once_with(Path('/proc/99/exe'), os.O_RDONLY | os.O_NONBLOCK | os.O_CLOEXEC)
                closed.assert_called_once_with(50)
                if inode != original['ino']:
                    digest.assert_not_called()

    def test_original_actual_executable_fd_bound_to_preflight_inode(self):
        self.assertEqual(self.process_case()['binary']['ino'], 20)

    def test_same_path_hash_different_executed_inode_rejected(self):
        with self.assertRaisesRegex(ValueError, 'executed-file-identity'):
            self.process_case(inode=21)

    def test_original_executable_replaced_during_observation_rejected(self):
        with self.assertRaisesRegex(ValueError, 'executed-file-replaced'):
            self.process_case(replacement=21)

    def test_post_hash_uid_start_parent_and_exe_change_rejected(self):
        for field in ('uid', 'start', 'parent', 'exe'):
            with self.subTest(field=field), self.assertRaisesRegex(ValueError, 'process-replaced'):
                self.process_case(changed_field=field)

    def test_bounded_command_nonzero(self):
        with self.assertRaises(subprocess.CalledProcessError):
            M.run(['/bin/sh', '-c', 'printf private; exit 9'])

    def test_bounded_command_noise(self):
        with self.assertRaisesRegex(ValueError, 'command-output-limit'):
            M.run(['/usr/bin/python3', '-c', 'print("x"*65537)'])

    def test_override_refused_before_package_commands(self):
        with patch.object(os, 'getuid', return_value=1000), patch.dict(os.environ, {'TUNA_SHELL_BIN': '/tmp/wrong'}), patch.object(M, 'run') as run:
            with self.assertRaisesRegex(ValueError, 'route-override'):
                M.preflight()
            run.assert_not_called()

    def test_uid_zero_refused(self):
        with patch.object(os, 'getuid', return_value=0), patch.object(M, 'run') as run:
            with self.assertRaises(ValueError):
                M.preflight()
            run.assert_not_called()

    def test_pid_bool_refused(self):
        with self.assertRaisesRegex(ValueError, 'pid-type'):
            M.process(True, 'tuna-compositor', {})

    def test_unowned_library_fatal(self):
        with patch.object(M, 'run', side_effect=subprocess.CalledProcessError(1, ['dpkg-query'])):
            with self.assertRaisesRegex(ValueError, 'unowned-library'):
                M.owner('/usr/lib/fixture.so')

    def test_receipt_bound(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / 'receipt'
            path.write_bytes(b' ' * (1024 * 1024 + 1))
            with self.assertRaisesRegex(ValueError, 'receipt-limit'):
                M.bounded_json(path)

    def test_distro_scale_library_hashes_within_file_bound(self):
        # libLLVM.so.20.1 is 143,545,784 root-owned bytes and is mapped by the
        # GTK shell via Mesa; the bound must admit current distro LLVM scale.
        size = 143545784
        status = SimpleNamespace(st_dev=1, st_ino=99, st_uid=0, st_mode=0o100644,
                                 st_size=size, st_mtime_ns=10, st_ctime_ns=11)
        remaining = [size]
        def fake_read(fd, count):
            chunk = min(count, remaining[0], 65536)
            if chunk <= 0:
                return b''
            remaining[0] -= chunk
            return b'\0' * chunk
        with patch.object(os, 'open', return_value=50), patch.object(os, 'fstat', return_value=status), \
             patch.object(os, 'stat', return_value=status), patch.object(os, 'read', side_effect=fake_read), \
             patch.object(os, 'close'):
            result = M.digest('/usr/lib/x86_64-linux-gnu/libLLVM.so.21.1')
        self.assertEqual(result['size'], size)

    def test_mapped_inode_mismatch_rejected_before_hashing(self):
        maps = '1-2 r-xp 0 01:01 3 /usr/lib/libgtk-4.so.1\n'
        with patch('builtins.open', return_value=io.BytesIO(maps.encode())), patch.object(Path, 'resolve', lambda p, **_: p), patch.object(os, 'stat', return_value=SimpleNamespace(st_dev=257, st_ino=4)), patch.object(M, 'digest') as digest:
            with self.assertRaisesRegex(ValueError, 'mapped-file-identity'):
                M.libraries(100)
            digest.assert_not_called()

    def test_unpacked_usr_local_library_cannot_qualify(self):
        maps = '1-2 r-xp 0 01:01 3 /usr/local/lib/libgtk4-layer-shell.so.0\n'
        with patch('builtins.open', return_value=io.BytesIO(maps.encode())):
            with self.assertRaisesRegex(ValueError, 'foreign-loaded-library'):
                M.libraries(100)

    def test_companion_wrong_commit_refused(self):
        def command(args):
            if args == ['dpkg', '--verify', 'tuna-desktop']:
                return ''
            if args[0] == 'git':
                return 'a' * 40
            if args[0].startswith('/usr/bin/tuna-'):
                return Path(args[0]).name + ' 0.1.0'
            return '0.1.0'
        with patch.object(os, 'getuid', return_value=1000), patch.dict(os.environ, {}, clear=True), patch.object(Path, 'read_text', return_value='ID=ubuntu\nVERSION_ID="26.04"\n'), patch.object(M, 'run', side_effect=command), patch.object(M, 'owner', return_value={'name': 'tuna-desktop', 'version': '0.1.0'}), patch.object(M, 'digest', return_value={'sha256': '0' * 64}), patch.object(M, 'bounded_json', return_value={'commit': 'b' * 40, 'files': {'icon-probe': '0' * 64, 'shortcut-inhibit-client': '0' * 64, 'tuna-bell-client': '0' * 64}}):
            with self.assertRaisesRegex(ValueError, 'companion-source-contract'):
                M.preflight()

    def _preflight_with(self, deb_version, reported):
        def command(args):
            if args == ['dpkg', '--verify', 'tuna-desktop']:
                return ''
            if args[0] == 'git':
                return 'a' * 40
            if args[0].startswith('/usr/bin/tuna-'):
                return Path(args[0]).name + ' ' + reported
            return deb_version
        with patch.object(os, 'getuid', return_value=1000), patch.dict(os.environ, {}, clear=True), patch.object(Path, 'read_text', return_value='ID=ubuntu\nVERSION_ID="26.04"\n'), patch.object(M, 'run', side_effect=command), patch.object(M, 'owner', return_value={'name': 'tuna-desktop', 'version': deb_version}), patch.object(M, 'digest', return_value={'sha256': '0' * 64}), patch.object(M, 'bounded_json', return_value={'commit': 'b' * 40, 'files': {'icon-probe': '0' * 64, 'shortcut-inhibit-client': '0' * 64, 'tuna-bell-client': '0' * 64}}):
            M.preflight()

    def test_prerelease_package_accepts_semver_binaries(self):
        # 1.0.0~beta.1 is the Debian form of the binaries' 1.0.0-beta.1; the
        # binary contract passes and the next check (companions) decides.
        with self.assertRaisesRegex(ValueError, 'companion-source-contract'):
            self._preflight_with('1.0.0~beta.1', '1.0.0-beta.1')

    def test_prerelease_package_rejects_mismatched_binaries(self):
        for reported in ('1.0.0~beta.1', '1.0.0', '1.0.0-beta.2'):
            with self.subTest(reported=reported), self.assertRaisesRegex(ValueError, 'installed-binary-contract'):
                self._preflight_with('1.0.0~beta.1', reported)

    def test_original_session_and_replacement_contract(self):
        base = {'files': {}}
        compositor = {'pid': 100, 'exe': '/usr/bin/tuna-compositor'}
        shell = {'pid': 101, 'parent': 100}
        sockets = {n: {'pid': 100} for n in ('owned', 'tuna-owned.control')}
        previous = {'compositor': compositor, 'shell': shell, 'sockets': sockets}
        def proc(pid, *_):
            return compositor if pid == 100 else shell
        with patch.object(M, 'process', side_effect=proc), patch.object(Path, 'read_text', return_value='101'), patch.object(os, 'readlink', return_value='/usr/bin/tuna-shell-gtk'), patch.object(M, 'peer', return_value={'pid': 100}), patch.object(M, 'libraries', return_value=[]), patch.dict(os.environ, {'XDG_RUNTIME_DIR': '/tmp/owned'}):
            self.assertEqual(M.observe(100, 'owned', base, previous)['shell'], shell)
            with self.assertRaisesRegex(ValueError, 'shell-replacement-contract'):
                M.observe(100, 'owned', base, previous, replacement=True)
            old = dict(previous, compositor={'pid': 99})
            with self.assertRaisesRegex(ValueError, 'original-session-replaced'):
                M.observe(100, 'owned', base, old)
            shell['pid'] = 102
            with patch.object(M, 'process', side_effect=lambda pid, *_: compositor if pid == 100 else shell):
                prior = dict(previous, shell={'pid': 101, 'parent': 100})
                with self.assertRaisesRegex(ValueError, 'shell-replacement-contract'):
                    M.observe(100, 'owned', base, prior)
                self.assertEqual(M.observe(100, 'owned', base, prior, True)['shell']['pid'], 102)


def copy_shipped_icons(destination):
    # Local worktrees may use a group-writable checkout umask. Build an
    # immutable fixture payload with the same bytes/symlinks as the CI source.
    shutil.copytree(ROOT / 'share/tuna-desktop/icons', destination, symlinks=True)
    for path in destination.rglob('*'):
        if path.is_file() and not path.is_symlink():
            path.chmod(path.stat().st_mode & ~0o022)


class PackagePolicy(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        self.tools = self.dir / 'tools'
        self.tools.mkdir()
        self.stage = self.dir / 'stage'
        self.stage.mkdir()
        for name in M.BINS:
            p = self.stage / 'usr/bin' / name
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text('#!/bin/sh\nprintf "%s\\n" "' + name + ' 0.1.0"\n')
            p.chmod(0o755)
        # The policy sees actual tar streams and real bridge bytes, even though
        # the executable/loader/package commands here remain unit-test doubles.
        # tuna-rename: keep-begin
        for installed, source in (
                ('usr/share/wayland-sessions/tuna.desktop', 'share/wayland-sessions/tuna.desktop'),
                ('usr/share/wayland-sessions/roost.desktop', 'share/compat/wayland-sessions/roost.desktop'),
                ('usr/lib/systemd/user/tuna-session.target', 'share/systemd/user/tuna-session.target'),
                ('etc/pam.d/tuna-lock', 'share/pam.d/tuna-lock'),
                ('etc/pam.d/roost-lock', 'share/compat/pam.d/roost-lock')):
            p = self.stage / installed
            p.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / source, p)
            p.chmod(0o644)
        # tuna-rename: keep-end
        # Use the real shipped private theme and its integrity manifest rather
        # than bypassing asset admission in these controlled package fixtures.
        copy_shipped_icons(self.stage / 'usr/share/tuna-desktop/icons')
        for directory in self.stage.rglob('*'):
            if directory.is_dir() and not directory.is_symlink():
                directory.chmod(0o755)
        self.env = dict(os.environ, PATH=str(self.tools) + ':' + os.environ['PATH'], FIXTURE=str(self.stage))
        self.tool('dpkg-deb', '''case "$1" in
-c) find "$FIXTURE" -type f | while read -r path; do echo "-rwxr-xr-x $path"; done ;;
-f) case "$3" in Version) echo 0.1.0 ;; Depends) echo 'libgtk4-layer-shell0 (>= 1.1), libgtk-4-1 (>= 4.12), libadwaita-1-0 (>= 1.0), libpam0g, gsettings-desktop-schemas, gnome-settings-daemon-common' ;; Replaces|Conflicts|Provides) echo roost ;; esac ;;  # tuna-rename: keep
-x) cp -pR "$FIXTURE/." "$3/" ;;
--ctrl-tarfile) tar --owner=0 --group=0 -C "$FIXTURE" -cf - ./conffiles ;;
--fsys-tarfile) tar --owner=0 --group=0 -C "$FIXTURE" -cf - ./usr ./etc ;;
esac''')
        (self.stage / 'conffiles').write_text('/etc/pam.d/tuna-lock\n/etc/pam.d/roost-lock\n')  # tuna-rename: keep
        (self.stage / 'conffiles').chmod(0o644)
        self.tool('ldd', 'echo "libc.so.6 => /lib/libc.so.6 (0)"')
        self.tool('desktop-file-validate', 'exit 0')

    def tool(self, name, body):
        path = self.tools / name
        path.write_text('#!/bin/sh\nset -eu\n' + body + '\n')
        path.chmod(0o755)

    def check(self):
        return subprocess.run(['sh', str(ROOT / 'scripts/check-release-package'), 'controlled.deb'], env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10).returncode

    def test_positive_controlled_tool_policy(self):
        self.assertEqual(self.check(), 0)

    def test_missing_shipped_cursor_asset_is_rejected(self):
        theme = self.stage / 'usr/share/tuna-desktop/icons/Adwaita'
        manifest = json.loads((theme / 'UPSTREAM.json').read_text())
        entry = next(item for item in manifest['entries'] if item['path'].endswith('.svg'))
        (theme / entry['path']).unlink()
        self.assertNotEqual(self.check(), 0)

    def test_group_writable_shipped_cursor_asset_is_rejected(self):
        theme = self.stage / 'usr/share/tuna-desktop/icons/Adwaita'
        manifest = json.loads((theme / 'UPSTREAM.json').read_text())
        entry = next(item for item in manifest['entries'] if item['path'].endswith('.svg'))
        path = theme / entry['path']
        path.chmod(path.stat().st_mode | 0o020)
        # dpkg-deb extraction preserves archive modes. The mock must do the
        # same under both the CI umask and a group-writable local checkout.
        for mask in (0o022, 0o002):
            with self.subTest(umask=oct(mask)):
                previous = os.umask(mask)
                try:
                    self.assertNotEqual(self.check(), 0)
                finally:
                    os.umask(previous)

    def test_only_retained_legacy_bridge_names_are_admitted(self):
        # Unit policy assertions; the separate genuine package suite establishes
        # original archive behavior with native package/loader dependencies.
        # tuna-rename: keep-begin
        for forbidden in ('usr/bin/roost-session', 'etc/pam.d/roost-extra',
                          'usr/share/wayland-sessions/roost-extra.desktop'):
            with self.subTest(forbidden=forbidden):
                path = self.stage / forbidden
                path.write_text('forbidden\n')
                self.assertNotEqual(self.check(), 0)
                path.unlink()
        for retained in ('usr/share/wayland-sessions/roost.desktop', 'etc/pam.d/roost-lock'):
            with self.subTest(missing=retained):
                path = self.stage / retained
                original = path.read_bytes()
                path.unlink()
                self.assertNotEqual(self.check(), 0)
                path.write_bytes(original)
                path.chmod(0o644)
        # tuna-rename: keep-end
        self.assertEqual(self.check(), 0)

    def test_missing_preferred_gtk(self):
        (self.stage / 'usr/bin/tuna-shell-gtk').unlink()
        self.assertNotEqual(self.check(), 0)

    def test_each_binary_mode_required(self):
        (self.stage / 'usr/bin/tuna-shell-gtk').chmod(0o644)
        self.assertNotEqual(self.check(), 0)

    def test_version_exit_not_hidden_by_pipeline(self):
        (self.stage / 'usr/bin/tuna-shell-gtk').write_text('#!/bin/sh\necho "tuna-shell-gtk 0.1.0"\nexit 9\n')
        self.assertNotEqual(self.check(), 0)

    def test_noisy_or_wrong_whole_line(self):
        for value in ('tuna-shell-gtk 0.1.0 [fixture]', 'wrong-name 0.1.0', 'tuna-shell-gtk 0.1.0\nextra', 'tuna-shell-gtk 0.1.0\n'):
            (self.stage / 'usr/bin/tuna-shell-gtk').write_text('#!/bin/sh\nprintf "%s\\n" "' + value + '"\n')
            self.assertNotEqual(self.check(), 0)

    def test_missing_loaded_elf_dependency(self):
        self.tool('ldd', 'echo "libgtk4-layer-shell.so.0 => not found"')
        self.assertNotEqual(self.check(), 0)

    def test_suffixed_floor_does_not_match(self):
        path = self.tools / 'dpkg-deb'
        path.write_text(path.read_text().replace('libgtk4-layer-shell0 (>= 1.1)', 'not-libgtk4-layer-shell0 (>= 1.1)'))
        self.assertNotEqual(self.check(), 0)

    def test_missing_or_obsolete_pam_runtime_dependency_rejected(self):
        path = self.tools / 'dpkg-deb'
        original = path.read_text()
        for replacement in ('', 'libpam0', 'not-libpam0g'):
            with self.subTest(replacement=replacement):
                path.write_text(original.replace('libpam0g', replacement))
                self.assertNotEqual(self.check(), 0)

    def test_missing_desktop_schemas_runtime_dependency_rejected(self):
        path = self.tools / 'dpkg-deb'
        path.write_text(path.read_text().replace(', gsettings-desktop-schemas', ''))
        self.assertNotEqual(self.check(), 0)

    def test_missing_power_schemas_runtime_dependency_rejected(self):
        path = self.tools / 'dpkg-deb'
        path.write_text(path.read_text().replace(', gnome-settings-daemon-common', ''))
        self.assertNotEqual(self.check(), 0)

    def test_launcher_override_denies_before_any_process(self):
        result = subprocess.run(['sh', str(ROOT / 'scripts/lib/tuna-proof-session-launch'), '1', str(ROOT), 'owned'], env=dict(self.env, TUNA_SHELL_BIN='/tmp/wrong'), timeout=5)
        self.assertNotEqual(result.returncode, 0)

    def test_supplied_source_binaries_denied_in_installed_launcher(self):
        result = subprocess.run(['sh', str(ROOT / 'scripts/lib/tuna-proof-session-launch'), '1', str(ROOT), 'owned'], env=dict(self.env, TUNA_PROOF_BIN_DIR='/tmp/source-binaries'), timeout=5)
        self.assertNotEqual(result.returncode, 0)


class ReleaseDelegatePolicy(unittest.TestCase):
    def run_release(self, failure='', version_exit=0, missing_cursor=False, umask=-1):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            tools = root / 'tools'
            tools.mkdir()
            def tool(name, body):
                path = tools / name
                path.write_text('#!/bin/sh\nset -eu\n' + body + '\n')
                path.chmod(0o755)
            tool('git', 'echo v0.1.0')
            tool('cargo', 'test "$*" = "build --locked --release -p tuna-compositor -p tuna-wallpaper-svg -p tuna-shell-gtk -p tuna-shell-host"')
            tool('dpkg-shlibdeps', 'exit 9' if failure == 'shlibs' else ('echo unknown' if failure == 'malformed' else 'echo "shlibs:Depends=libc6 (>= 2.36)"'))
            # tuna-rename: keep-begin
            tool('dpkg-deb', '''test "$1" = --root-owner-group
test "$2" = --build
assert_mode() {
    actual="$(stat -c %a "$2")"
    [ "$actual" = "$1" ] || {
        echo "staged mode $actual, expected $1: $2" >&2
        exit 1
    }
}
assert_mode 755 "$3"
for path in usr usr/share usr/share/wayland-sessions etc etc/pam.d; do
    assert_mode 755 "$3/$path"
done
for path in DEBIAN/control DEBIAN/conffiles; do assert_mode 644 "$3/$path"; done
printf '%s\\n' /etc/pam.d/tuna-lock /etc/pam.d/roost-lock >"$EXPECTED_CONFFILES"
cmp "$EXPECTED_CONFFILES" "$3/DEBIAN/conffiles"
for path in usr/share/wayland-sessions/roost.desktop etc/pam.d/roost-lock; do
    test -f "$3/$path"; test ! -L "$3/$path"; test "$(stat -c %a "$3/$path")" = 644
done
for hook in preinst postinst prerm postrm config triggers; do test ! -e "$3/DEBIAN/$hook"; done
cp "$3/DEBIAN/control" "$OUTPUT"''')
            # tuna-rename: keep-end
            for name in M.BINS:
                path = root / 'target/release' / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text('#!/bin/sh\necho "' + name + ' 0.1.0"\nexit ' + str(version_exit if name == 'tuna-shell-gtk' else 0) + '\n')
                path.chmod(0o755)
            for name in ('share/wayland-sessions/tuna.desktop', 'share/systemd/user/tuna-session.target', 'share/pam.d/tuna-lock', 'share/xdg-desktop-portal/portals/tuna.portal', 'packaging/marlin/tuna-portals.conf', 'share/compat/wayland-sessions/roost.desktop', 'share/compat/pam.d/roost-lock'):  # tuna-rename: keep
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / name, path)
                path.chmod(0o644)
            copy_shipped_icons(root / 'share/tuna-desktop/icons')
            checker = root / 'scripts/tuna-cursor-assets-check'
            checker.parent.mkdir(parents=True, exist_ok=True)
            shutil.copy2(ROOT / 'scripts/tuna-cursor-assets-check', checker)
            if missing_cursor:
                theme = root / 'share/tuna-desktop/icons/Adwaita'
                manifest = json.loads((theme / 'UPSTREAM.json').read_text())
                entry = next(item for item in manifest['entries'] if item['path'].endswith('.svg'))
                (theme / entry['path']).unlink()
            output = root / 'control'
            result = subprocess.run(['sh', str(ROOT / 'scripts/tuna-release'), '--allow-dirty', '--out', str(root/'out')], cwd=root, env=dict(os.environ, PATH=str(tools)+':'+os.environ['PATH'], OUTPUT=str(output), EXPECTED_CONFFILES=str(root / 'expected-conffiles')), stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10, umask=umask)
            self.release_diagnostics = (f"release exited {result.returncode}\n"
                                        f"stdout:\n{result.stdout.decode(errors='replace')}\n"
                                        f"stderr:\n{result.stderr.decode(errors='replace')}")
            return result.returncode, output.read_text() if output.exists() else None

    def test_real_release_delegates_locked_all_six_and_abi_closure(self):
        status, control = self.run_release()
        self.assertEqual(status, 0, self.release_diagnostics)
        self.assertIn('libc6 (>= 2.36)', control)
        self.assertIn('libgtk4-layer-shell0 (>= 1.1)', control)
        for field in ('Replaces', 'Conflicts', 'Provides'):
            self.assertIn(field + ': roost\n', control)  # tuna-rename: keep
        depends = next(line.removeprefix('Depends: ') for line in control.splitlines() if line.startswith('Depends: ')).split(', ')
        self.assertIn('libpam0g', depends)
        self.assertIn('gsettings-desktop-schemas', depends)
        self.assertIn('gnome-settings-daemon-common', depends)
        self.assertNotIn('libpam0', depends)

    def test_release_stages_protected_ancestors_under_group_writable_umask(self):
        # The actual shell builder runs; its controlled dpkg delegate asserts
        # stage/ancestor modes, both regular bridges, and exact conffiles above.
        status, control = self.run_release(umask=0o002)
        self.assertEqual(status, 0, self.release_diagnostics)
        self.assertIsNotNone(control)

    def test_release_rejects_missing_private_cursor_asset(self):
        status, control = self.run_release(missing_cursor=True)
        self.assertNotEqual(status, 0)
        self.assertIsNone(control)

    def test_shlibs_failure_or_unknown_output_cannot_package(self):
        for failure in ('shlibs', 'malformed'):
            status, control = self.run_release(failure)
            self.assertNotEqual(status, 0)
            self.assertIsNone(control)

    def test_release_version_failure_preserves_fail_closed(self):
        status, control = self.run_release(version_exit=9)
        self.assertNotEqual(status, 0)
        self.assertIsNone(control)


class StartupOverviewPolicy(unittest.TestCase):
    def actual_script(self, mode='1', failure='', after=None, startup=None):
        script = (ROOT / 'scripts/tuna-gtk-shell-proof').read_text()
        start = script.index('if [ "$INSTALLED_PACKAGE" = 1 ]; then', script.index('[ -n "$WID" ] || fail G-PANEL'))
        end = script.index('# The panel is up once', start)
        fragment = script[start:end]
        with tempfile.TemporaryDirectory() as directory:
            out = Path(directory)
            original = {'overview_open': True, 'overview_progress': 1, 'locked': False,
                        'pointer_position': [640, 400]}
            (out/'state').write_text(json.dumps(startup or original))
            final = after or dict(original, overview_open=False, overview_progress=0)
            (out/'after').write_text(json.dumps(final))
            prelude = r'''set -eu
fail() { printf 'FAIL:%s\n' "$1" >>"$ARTIFACTS/events"; exit 9; }
pass() { :; }
fixture_a11y() { printf 'a11y\n' >>"$ARTIFACTS/events"; printf '[]' >"$3"; }
python3() {
    printf 'proof:%s\n' "$2" >>"$ARTIFACTS/events"
    case "$FAILURE:$2:$7" in
        start:start:*|before:stable:*/installed-before-escape.json|after:stable:*/installed-after-escape.json) return 9;;
    esac
    printf '{}' >"$7"
}
timeout() { shift; "$@"; }
xdotool() {
    printf 'input:%s\n' "$*" >>"$ARTIFACTS/events"
    case "$1" in
        search) if [ "$FAILURE" = ambiguous ]; then printf '99\n100\n'; else printf '99\n'; fi;;
        getwindowpid) if [ "$FAILURE" = owner ]; then printf '456\n'; else printf '123\n'; fi;;
        getwindowfocus) if [ "$FAILURE" = focus ]; then printf '100\n'; else printf '99\n'; fi;;
        key) cp "$ARTIFACTS/after" "$CSTATE";;
    esac
}
scrot() { printf 'screenshot' >"$1"; }
sleep() { :; }
date() {
    if [ -f "$ARTIFACTS/clock" ]; then clock=$(cat "$ARTIFACTS/clock"); else clock=0; fi
    clock=$((clock + 25)); printf '%s\n' "$clock" >"$ARTIFACTS/clock"; printf '%s\n' "$clock"
}
'''
            env = dict(os.environ, INSTALLED_PACKAGE=mode, FAILURE=failure, PY='fixture_a11y',
                       ROOT=str(ROOT), ARTIFACTS=str(out), CSTATE=str(out/'state'),
                       COMPOSITOR_PID='123', WID='99', WIDTH='1280', HEIGHT='800')
            result = subprocess.run(['bash', '-c', prelude+fragment], env=env,
                                    stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5)
            return result.returncode, (out/'events').read_text().splitlines(), sorted(p.name for p in out.iterdir())

    def test_actual_installed_admission_precedes_all_input_and_one_escape(self):
        status, events, files = self.actual_script()
        self.assertEqual(status, 0)
        self.assertLess(events.index('proof:start'), next(i for i, x in enumerate(events) if x.startswith('input:')))
        self.assertEqual(events.count('input:key Escape'), 1)
        self.assertEqual(events.count('proof:stable'), 2)
        for path in ('installed-startup-overview.json', 'installed-startup-overview.png',
                     'installed-desktop-before-panel.json', 'a11y-installed-startup.json'):
            self.assertIn(path, files)

    def test_unready_startup_overview_never_sends_escape(self):
        for startup in ({'overview_open': False, 'overview_progress': 0, 'locked': False},
                        {'overview_open': True, 'overview_progress': .5, 'locked': False},
                        {'overview_open': True, 'overview_progress': 1, 'locked': True}):
            with self.subTest(startup=startup):
                status, events, files = self.actual_script(startup=startup)
                self.assertNotEqual(status, 0)
                self.assertNotIn('input:key Escape', events)
                self.assertNotIn('installed-startup-overview.json', files)

    def test_source_mode_never_admits_installed_or_sends_startup_escape(self):
        status, events, files = self.actual_script(mode='0')
        self.assertEqual(status, 0)
        self.assertEqual(events, ['input:windowfocus --sync 99', 'input:mousemove --window 99 640 400'])
        self.assertNotIn('installed-start.json', files)

    def test_failed_original_admission_or_ambiguous_host_prevents_input(self):
        for failure in ('start', 'owner', 'ambiguous'):
            with self.subTest(failure=failure):
                status, events, _ = self.actual_script(failure=failure)
                self.assertNotEqual(status, 0)
                self.assertFalse(any(x.startswith('input:windowfocus') or x.startswith('input:key') for x in events))

    def test_lost_host_focus_or_principal_before_escape_prevents_key(self):
        for failure in ('focus', 'before'):
            with self.subTest(failure=failure):
                status, events, _ = self.actual_script(failure=failure)
                self.assertNotEqual(status, 0)
                self.assertNotIn('input:key Escape', events)

    def test_failed_post_escape_principal_never_accepts_panel_precondition(self):
        status, events, files = self.actual_script(failure='after')
        self.assertNotEqual(status, 0)
        self.assertEqual(events.count('input:key Escape'), 1)
        self.assertNotIn('installed-desktop-before-panel.json', files)

    def test_real_script_restore_predicate_rejects_open_progress_lock_or_wrong_pointer(self):
        valid = {'overview_open': False, 'overview_progress': 0, 'locked': False,
                 'pointer_position': [640, 400]}
        for key, value in [('overview_open', True), ('overview_progress', .5),
                           ('locked', True), ('pointer_position', [1, 1])]:
            with self.subTest(key=key):
                status, events, files = self.actual_script(after=dict(valid, **{key: value}))
                self.assertNotEqual(status, 0)
                self.assertEqual(events.count('input:key Escape'), 1)
                self.assertNotIn('installed-desktop-before-panel.json', files)


if __name__ == '__main__':
    unittest.main()
