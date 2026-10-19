#!/usr/bin/env python3
"""Generated caches retain strict identity and native/package provenance."""
import ast
from pathlib import Path
import stat
import subprocess
from types import SimpleNamespace
import unittest
from unittest.mock import MagicMock, patch
import tuna_svg_resources as resources


class ResourceProvenanceTests(unittest.TestCase):
    def setUp(self):
        self.directory = Path('/usr/lib/fontconfig/cache')
        self.path = self.directory / ('a' * 32 + '-le64.cache-12')
        self.observed = SimpleNamespace(st_mode=stat.S_IFREG | 0o644, st_uid=0, st_gid=0)
        self.parent = SimpleNamespace(st_mode=stat.S_IFDIR | 0o755, st_uid=0, st_gid=0)

    def classify(self, owners=(None, 'fontconfig'), directories=None, resolved=None, kind='file'):
        with patch.object(resources, 'rpm_owner', side_effect=owners), \
                patch.object(Path, 'resolve', return_value=resolved or self.path), \
                patch.object(Path, 'lstat', return_value=self.parent):
            return resources.resource_package(self.path, {'kind': kind}, self.observed,
                                              {self.directory} if directories is None else directories)

    def test_packaged_resource_keeps_ordinary_verification(self):
        self.path = Path('/usr/lib64/libfontconfig.so.1')
        self.assertEqual(self.classify(('fontconfig',)), ('fontconfig', {'kind': 'rpm-payload'}))

    def test_generated_cache_records_accurate_distinct_provenance(self):
        package, receipt = self.classify()
        self.assertEqual(package, 'fontconfig')
        self.assertEqual(receipt, {'kind': 'generated-fontconfig-cache',
                                  'directory': str(self.directory), 'directory_rpm_owner': 'fontconfig'})

    def test_unknown_unowned_resource_fails(self):
        for name in ['libfake.so', 'font.ttf', 'fonts.conf', 'a-le64.cache-12', 'a' * 32 + '-other.cache-12',
                     'a' * 32 + '-le64.cache-12.extra']:
            with self.subTest(name=name), self.assertRaises(RuntimeError):
                self.path = self.directory / name
                self.classify()

    def test_cache_outside_native_directory_fails(self):
        with self.assertRaises(RuntimeError):
            self.classify(directories={Path('/elsewhere')})

    def test_cache_symlink_fails(self):
        with self.assertRaises(RuntimeError):
            self.classify(resolved=Path('/other/cache'))

    def test_unsafe_cache_metadata_fails(self):
        for field, value in [('st_uid', 1000), ('st_gid', 1000), ('st_mode', stat.S_IFREG | 0o664),
                             ('st_mode', stat.S_IFREG | 0o646), ('st_mode', stat.S_IFREG | 0o744),
                             ('st_mode', stat.S_IFDIR | 0o644)]:
            old = getattr(self.observed, field)
            with self.subTest(field=field, value=value), self.assertRaises(RuntimeError):
                setattr(self.observed, field, value)
                self.classify()
            setattr(self.observed, field, old)

    def test_unsafe_directory_metadata_fails(self):
        for field, value in [('st_uid', 1000), ('st_gid', 1000), ('st_mode', stat.S_IFDIR | 0o775),
                             ('st_mode', stat.S_IFREG | 0o755)]:
            old = getattr(self.parent, field)
            with self.subTest(field=field, value=value), self.assertRaises(RuntimeError):
                setattr(self.parent, field, value)
                self.classify()
            setattr(self.parent, field, old)

    def test_unowned_or_other_package_directory_fails(self):
        for owner in (None, 'other-package'):
            with self.subTest(owner=owner), self.assertRaises(RuntimeError):
                self.classify(owners=(None, owner))

    def test_declared_config_directory_cannot_be_cache(self):
        with self.assertRaises(RuntimeError):
            self.classify(kind='fontconfig-directory')

    def test_rpm_process_failures_are_not_unowned(self):
        for code in (2, -9, 127):
            with self.subTest(code=code), patch.object(subprocess, 'run', return_value=
                    SimpleNamespace(returncode=code, stdout='', stderr='failure')), self.assertRaises(RuntimeError):
                resources.rpm_owner(self.path)

    def test_only_explicit_unowned_rpm_response_allows_cache_classification(self):
        with patch.object(subprocess, 'run', return_value=SimpleNamespace(
                returncode=1, stdout=f'file {self.path} is not owned by any package\n', stderr='')) as query:
            self.assertIsNone(resources.rpm_owner(self.path))
            self.assertEqual(query.call_args.kwargs['env']['LC_ALL'], 'C')
            self.assertEqual(query.call_args.kwargs['timeout'], 10)
        for stdout, stderr in [('', ''), ('database unavailable', ''),
                               (f'file {self.path} is not owned by any package', 'warning')]:
            with self.subTest(stdout=stdout, stderr=stderr), patch.object(subprocess, 'run',
                    return_value=SimpleNamespace(returncode=1, stdout=stdout, stderr=stderr)), \
                    self.assertRaises(RuntimeError):
                resources.rpm_owner(self.path)

    def test_rpm_timeout_remains_failure(self):
        with patch.object(subprocess, 'run', side_effect=subprocess.TimeoutExpired('rpm', 10)), \
                self.assertRaises(subprocess.TimeoutExpired):
            resources.rpm_owner(self.path)

    def test_receipt_verifies_identity_before_classifying_and_verifies_package(self):
        source = Path(__file__).with_name('tuna-svg-loader-receipt.py').read_text()
        ast.parse(source)
        self.assertLess(source.index('digest.hexdigest()!=resource'), source.index('resource_package(path'))
        self.assertIn("unchanged(named)!=unchanged(before)", source)
        self.assertIn("['rpm', '-V', package]", source)
        self.assertIn("if result.returncode or result.stdout or result.stderr:", source)


class NativeCacheDirectoryTests(unittest.TestCase):
    def native(self):
        native = MagicMock()
        native.FcInitLoadConfig.return_value = 1
        native.FcConfigGetCacheDirs.return_value = 2
        native.FcStrListNext.side_effect = [b'/usr/lib/fontconfig/cache', None]
        return native

    def test_native_list_and_lifetimes(self):
        native = self.native()
        with patch.object(resources.ctypes, 'CDLL', return_value=native):
            self.assertEqual(resources.fontconfig_cache_directories(), {Path('/usr/lib/fontconfig/cache')})
        native.FcStrListDone.assert_called_once_with(2)
        native.FcConfigDestroy.assert_called_once_with(1)

    def test_missing_configuration_fails(self):
        native = self.native()
        native.FcInitLoadConfig.return_value = None
        with patch.object(resources.ctypes, 'CDLL', return_value=native), self.assertRaises(RuntimeError):
            resources.fontconfig_cache_directories()
        native.FcConfigDestroy.assert_not_called()

    def test_missing_list_fails_and_destroys_configuration(self):
        native = self.native()
        native.FcConfigGetCacheDirs.return_value = None
        with patch.object(resources.ctypes, 'CDLL', return_value=native), self.assertRaises(RuntimeError):
            resources.fontconfig_cache_directories()
        native.FcConfigDestroy.assert_called_once_with(1)

    def test_relative_directory_fails_and_releases_list(self):
        native = self.native()
        native.FcStrListNext.side_effect = [b'relative']
        with patch.object(resources.ctypes, 'CDLL', return_value=native), self.assertRaises(RuntimeError):
            resources.fontconfig_cache_directories()
        native.FcStrListDone.assert_called_once_with(2)
        native.FcConfigDestroy.assert_called_once_with(1)

    def test_unbounded_native_list_fails(self):
        native = self.native()
        native.FcStrListNext.side_effect = None
        native.FcStrListNext.return_value = b'/usr/lib/fontconfig/cache'
        with patch.object(resources.ctypes, 'CDLL', return_value=native), self.assertRaises(RuntimeError):
            resources.fontconfig_cache_directories()
        self.assertEqual(native.FcStrListNext.call_count, 16384)
        native.FcStrListDone.assert_called_once_with(2)
        native.FcConfigDestroy.assert_called_once_with(1)


if __name__ == '__main__':
    unittest.main()
