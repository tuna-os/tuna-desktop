"""Real native-database controls, not signed-image or GeoClue runtime proof.

These tests install a declared synthetic package into a private pacman root.
They execute genuine pacman/pacman-conf against its real configured database;
there is no mocked command output or alternate production trust path. Root is
needed only for the helper's genuine root-owned metadata checks.
"""
import importlib.machinery
import importlib.util
import io
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import tarfile
import tempfile
import unittest


SOURCE = Path(__file__).resolve().parents[2] / 'packaging/marlin/tuna-geoclue-agent-policy'
LOADER = importlib.machinery.SourceFileLoader('geoclue_native_policy_controls', str(SOURCE))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
POLICY = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(POLICY)
PACMAN = shutil.which('pacman')
PACMAN_CONF = shutil.which('pacman-conf')
ORIGINAL = b'[agent]\nwhitelist=gnome-shell;geoclue-demo-agent;\n\n[org.gnome.Maps]\nallowed=false\n'


@unittest.skipUnless(os.geteuid() == 0 and PACMAN and PACMAN_CONF,
                     'real pacman/pacman-conf plus root-owned isolated metadata required')
class ConfiguredPacmanDatabaseControls(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='tuna-geoclue-native-db-')
        self.addCleanup(self.directory.cleanup)
        self.work = Path(self.directory.name)
        self.root = self.work / 'root'
        self.database = self.work / 'usr/lib/sysimage/lib/pacman'
        for path in (self.root, self.database, self.work / 'cache', self.work / 'bin'):
            path.mkdir(parents=True)
        self.config = self.work / 'pacman.conf'
        self.config.write_text('[options]\nRootDir = ' + str(self.root) + '\nDBPath = '
                               + str(self.database) + '\nLogFile = ' + str(self.work / 'pacman.log')
                               + '\nCacheDir = ' + str(self.work / 'cache')
                               + '\nArchitecture = auto\nSigLevel = Never\n')
        # SigLevel applies only to this explicitly synthetic regression package
        # and private database; production helper invocations never alter it.
        self.environment = dict(os.environ)
        self.environment['PATH'] = str(self.work / 'bin') + os.pathsep + os.environ['PATH']
        self.original_path = os.environ['PATH']
        os.environ['PATH'] = self.environment['PATH']
        self.addCleanup(os.environ.__setitem__, 'PATH', self.original_path)
        executable = self.work / 'bin/pacman'
        executable.write_text('#!/bin/sh\nif [ "$1" = -Qqo ]; then\n'
                              '  exec ' + shlex.quote(PACMAN) + ' --config ' + shlex.quote(str(self.config))
                              + ' "$1" ' + shlex.quote(str(self.root)) + '"$2"\nfi\nexec '
                              + shlex.quote(PACMAN) + ' --config ' + shlex.quote(str(self.config)) + ' "$@"\n')
        executable.chmod(0o755)
        executable = self.work / 'bin/pacman-conf'
        executable.write_text('#!/bin/sh\nexec ' + shlex.quote(PACMAN_CONF) + ' --config '
                              + shlex.quote(str(self.config)) + ' "$@"\n')
        executable.chmod(0o755)

    def install(self, *, name='geoclue', backup=True, policy=True):
        archive = self.work / (name + '-1.0-1-any.pkg.tar.gz')
        filename = 'etc/geoclue/geoclue.conf' if policy else 'usr/share/geoclue-control'
        metadata = ('pkgname = ' + name + '\npkgbase = ' + name
                    + '\npkgver = 1.0-1\npkgdesc = declared synthetic native database control\n'
                    'builddate = 1\npackager = native metadata regression\nsize = '
                    + str(len(ORIGINAL)) + '\narch = any\nlicense = GPL\n')
        if backup and policy:
            metadata += 'backup = ' + filename + '\n'
        with tarfile.open(archive, 'w:gz') as stream:
            for path, raw in (('.PKGINFO', metadata.encode()), (filename, ORIGINAL)):
                info = tarfile.TarInfo(path)
                info.size, info.mode, info.uid, info.gid = len(raw), 0o644, 0, 0
                stream.addfile(info, io.BytesIO(raw))
        subprocess.run([PACMAN, '--config', str(self.config), '-U', '--noconfirm', str(archive)],
                       check=True, text=True, capture_output=True, env=self.environment, timeout=15)

    def test_actual_relocated_database_admits_only_its_original_config(self):
        self.install()
        observed = POLICY.vendor_package(ORIGINAL)
        self.assertEqual(observed['database_path'], str(self.database))
        self.assertEqual(observed['backend'], 'pacman')
        self.assertEqual(observed['identity'], 'geoclue 1.0-1')
        self.assertEqual(observed['original_digest_algorithm'], 'md5')

    def test_admin_changed_bytes_fail_against_actual_native_backup_digest(self):
        self.install()
        with self.assertRaisesRegex(RuntimeError, 'administrator-modified'):
            POLICY.vendor_package(ORIGINAL + b'\n[agent]\nwhitelist=tuna-shell;\n')

    def test_another_actual_package_owner_cannot_replace_geoclue(self):
        self.install(policy=False)
        self.install(name='another-native-owner')
        with self.assertRaisesRegex(RuntimeError, 'another package owner'):
            POLICY.vendor_package(ORIGINAL)

    def test_missing_actual_package_backup_metadata_is_not_a_checksum_pass(self):
        self.install(backup=False)
        with self.assertRaisesRegex(RuntimeError, 'backup metadata|conffile digest unavailable'):
            POLICY.vendor_package(ORIGINAL)

    def test_mutable_configured_native_database_is_refused(self):
        self.install()
        self.database.chmod(0o777)
        with self.assertRaisesRegex(RuntimeError, 'immutable root-owned metadata'):
            POLICY.vendor_package(ORIGINAL)


if __name__ == '__main__':
    unittest.main()
