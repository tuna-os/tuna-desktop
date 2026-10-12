#!/usr/bin/env python3
"""Exercise the clean-install driver without product builds or containers."""
import json
import os
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
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        self.log = self.root / 'engine.json'
        self.env = dict(os.environ, PATH=str(self.bin) + os.pathsep + os.environ['PATH'],
                        ENGINE_LOG=str(self.log), BAD_OWNER='0', ENGINE_FAILURE='0')
        self.tool('dpkg-deb', '''#!/bin/sh
case "$1" in
  -f) printf '0.1.0-1\\n' ;;
  -c) owner=root/root; [ "$BAD_OWNER" = 0 ] || owner=1000/1000
      printf 'drwxr-xr-x %s 0 2026-10-12 20:00 ./\\n' "$owner"
      printf '%s %s 10 2026-10-12 20:00 ./usr/bin/tuna-shell-gtk\\n' '-rwxr-xr-x' "$owner" ;;
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
                              env=self.env, text=True, capture_output=True)

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

    def test_non_root_archive_never_reaches_container(self):
        self.env['BAD_OWNER'] = '1'
        result = self.run_check()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('not owned by root/root', result.stderr)
        self.assertFalse(self.log.exists())

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
