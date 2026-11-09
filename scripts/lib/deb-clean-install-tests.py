#!/usr/bin/env python3
"""Unit-only clean-install orchestration; no real container or install proof.

Original archive policy reads real tar streams; container/package CLI remain
controlled fixtures. Genuine package checks have a separate transition suite.
"""
import json
import os
import shutil
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / 'check-deb-clean-install'


class CleanInstallTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.package = self.root / 'candidate with spaces.deb'
        self.package.write_text('metadata fixture, not an executable package')
        self.stage = self.root / 'stage'
        # tuna-rename: keep-begin
        for installed, source in (
                ('usr/share/wayland-sessions/tuna.desktop', 'share/wayland-sessions/tuna.desktop'),
                ('usr/share/wayland-sessions/roost.desktop', 'share/compat/wayland-sessions/roost.desktop'),
                ('etc/pam.d/tuna-lock', 'share/pam.d/tuna-lock'),
                ('etc/pam.d/roost-lock', 'share/compat/pam.d/roost-lock')):
            path = self.stage / installed
            path.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(SCRIPT.parents[1] / source, path)
            path.chmod(0o644)
        (self.stage / 'conffiles').write_text('/etc/pam.d/tuna-lock\n/etc/pam.d/roost-lock\n')
        (self.stage / 'conffiles').chmod(0o644)
        # tuna-rename: keep-end
        for directory in self.stage.rglob('*'):
            if directory.is_dir():
                directory.chmod(0o755)
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.log = self.root / 'engine.json'
        self.env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ['PATH'],
                        ENGINE_LOG=str(self.log), BAD_OWNER='0', ENGINE_FAILURE='0',
                        FIXTURE_VERSION='0.1.0-1', FIXTURE=str(self.stage))
        self.tool('dpkg-deb', '''#!/bin/sh
case "$1" in
  -f) printf '%s\\n' "$FIXTURE_VERSION" ;;
  -c) owner=root/root; [ "$BAD_OWNER" = 0 ] || owner=1000/1000
      printf 'drwxr-xr-x %s 0 2026-10-12 20:00 ./\\n' "$owner"
      printf '%s %s 10 2026-10-12 20:00 ./usr/bin/tuna-shell-gtk\\n' '-rwxr-xr-x' "$owner" ;;
  --fsys-tarfile) uid=0; [ "$BAD_OWNER" = 0 ] || uid=1000
      tar --owner="$uid" --group=0 -C "$FIXTURE" -cf - ./usr ./etc ;;
  --ctrl-tarfile) tar --owner=0 --group=0 -C "$FIXTURE" -cf - ./conffiles ;;
  *) exit 99 ;;
esac
''')
        self.tool('docker', '''#!/usr/bin/env python3
import json, os, sys
from pathlib import Path
Path(os.environ['ENGINE_LOG']).write_text(json.dumps(sys.argv[1:]))
sys.exit(int(os.environ['ENGINE_FAILURE']))
''')

    def tool(self, name, body):
        path = self.bin / name
        path.write_text(body)
        path.chmod(0o755)

    def run_check(self):
        return subprocess.run([str(SCRIPT), str(self.package), 'ubuntu:26.04'],
                              env=self.env, text=True, capture_output=True, timeout=60)

    def test_clean_image_and_dependencies_are_explicit(self):
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stderr)
        argv = json.loads(self.log.read_text())
        self.assertIn('ubuntu:26.04', argv)
        self.assertIn(f'{self.root}:/pkg:ro', argv)
        self.assertIn('PKG_NAME=candidate with spaces.deb', argv)
        self.assertIn('PKG_VERSION=0.1.0-1', argv)
        body = argv[-1]
        self.assertIn('apt-get install -y --no-install-recommends "/pkg/$PKG_NAME"', body)
        self.assertIn('ldd "$path"', body)
        self.assertIn('"$path" --version', body)
        self.assertNotIn('-dev', body)
        self.assertIn('BIN_VERSION=0.1.0-1', argv)
        self.assertIn('= "$bin $BIN_VERSION"', body)

    def test_prerelease_package_expects_semver_binaries(self):
        # tuna-release maps a SemVer prerelease's hyphen to "~" in the package
        # Version; the binaries keep reporting the SemVer form.
        self.env['FIXTURE_VERSION'] = '0.2.0~beta.1'
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stderr)
        argv = json.loads(self.log.read_text())
        self.assertIn('PKG_VERSION=0.2.0~beta.1', argv)
        self.assertIn('BIN_VERSION=0.2.0-beta.1', argv)

    def test_native_audio_runtime_is_a_hard_dependency_and_checked_without_recommends(self):
        release = SCRIPT.with_name('tuna-release').read_text()
        depends = next(line for line in release.splitlines() if line.startswith('Depends: '))
        for package in ('gnome-session-canberra', 'libcanberra-pulse', 'sound-theme-freedesktop'):
            self.assertIn(package, depends.split(': ', 1)[1].split(', '))
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stderr)
        body = json.loads(self.log.read_text())[-1]
        self.assertIn('command -v canberra-gtk-play', body)
        self.assertIn('dpkg-query -L libcanberra-pulse', body)
        self.assertIn('for event in bell audio-volume-change', body)
        self.assertIn('/usr/share/sounds/freedesktop/stereo/$event.oga', body)

    def test_non_root_archive_never_reaches_container(self):
        self.env['BAD_OWNER'] = '1'
        result = self.run_check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('archive ownership rejected', result.stderr)
        self.assertFalse(self.log.exists())

    def test_narrow_bridge_is_checked_and_bad_archive_never_reaches_container(self):
        result = self.run_check()
        self.assertEqual(result.returncode, 0, result.stderr)
        body = json.loads(self.log.read_text())[-1]
        # tuna-rename: keep-begin
        self.assertIn('/usr/share/wayland-sessions/roost.desktop /etc/pam.d/roost-lock /etc/pam.d/tuna-lock', body)
        self.assertIn('[ ! -L "$path" ]', body)
        self.assertIn('NoDisplay=true', body)
        self.assertIn('unexpected legacy paths', body)
        self.assertIn('for service in tuna-lock roost-lock', body)
        self.log.unlink()
        (self.stage / 'etc/pam.d/roost-lock').unlink()
        result = self.run_check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('transition-file-missing', result.stderr)
        self.assertFalse(self.log.exists())
        # tuna-rename: keep-end

    def test_container_failure_is_not_a_pass(self):
        self.env['ENGINE_FAILURE'] = '8'
        result = self.run_check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('clean install on ubuntu:26.04 failed', result.stderr)

    def test_missing_package_is_rejected(self):
        self.package.unlink()
        self.assertNotEqual(self.run_check().returncode, 0)
        self.assertFalse(self.log.exists())


if __name__ == '__main__':
    unittest.main()
