#!/usr/bin/env python3
"""Generated caches retain strict identity and native/package provenance."""
import ast
import hashlib
import os
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


class SvgPointerCaptureTests(unittest.TestCase):
    def helpers(self, original=None):
        path = Path(__file__).with_name('tuna-stock-svg-proof.py')
        tree = ast.parse(path.read_text())
        nodes = [next(n for n in tree.body if isinstance(n, ast.FunctionDef) and n.name == name)
                 for name in ('verified_host', 'park_pointer_for_pixels')]
        pid = os.getpid()
        executable = Path(f'/proc/{pid}/exe').resolve(strict=True)
        actual = {'pid': pid, 'executable': str(executable),
                  'start_ticks': Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[19],
                  'sha256': hashlib.sha256(executable.read_bytes()).hexdigest()}
        scope = {'subprocess': subprocess, 'Path': Path, 'hashlib': hashlib,
                 'ORIGINAL_HOST': original or actual, 'budget': lambda label, cap=10: cap}
        exec(compile(ast.Module(body=nodes, type_ignores=[]), str(path), 'exec'), scope)
        return scope, actual

    def test_host_requires_original_pid_start_executable_and_hash(self):
        scope, actual = self.helpers()
        with patch.object(subprocess, 'check_output', side_effect=['41\n', str(os.getpid())]):
            self.assertEqual(scope['verified_host'](), ('41', actual))
        for field, value in [('pid', 1), ('start_ticks', '0'), ('executable', '/other'), ('sha256', '0' * 64)]:
            scope, _ = self.helpers({**actual, field: value})
            with self.subTest(field=field), patch.object(subprocess, 'check_output',
                    side_effect=['41\n', str(os.getpid())]), self.assertRaises(RuntimeError):
                scope['verified_host']()

    def test_nonunique_host_is_rejected(self):
        scope, _ = self.helpers()
        for hosts in ('', '41 42'):
            with self.subTest(hosts=hosts), patch.object(subprocess, 'check_output', return_value=hosts), \
                    self.assertRaises(RuntimeError):
                scope['verified_host']()

    def test_physical_motion_and_observed_position_are_required(self):
        scope, actual = self.helpers()
        def wait(predicate, label, seconds):
            self.assertEqual(seconds, 5)
            if not predicate(): raise RuntimeError(label)
        scope.update(verified_host=lambda: ('41', actual), wait_for=wait,
                     scene=lambda: {'pointer_position': [640.0, 16.0]})
        with patch.object(subprocess, 'run') as run:
            receipt = scope['park_pointer_for_pixels']()
        run.assert_called_once_with(['xdotool', 'mousemove', '--window', '41', '640', '16'],
                                    check=True, timeout=5)
        self.assertEqual(receipt['host'], actual)
        scope['scene'] = lambda: {'pointer_position': [640.0, 400.0]}
        with patch.object(subprocess, 'run'), self.assertRaises(RuntimeError):
            scope['park_pointer_for_pixels']()

    def test_failed_input_and_replaced_host_fail_closed(self):
        scope, actual = self.helpers()
        scope.update(verified_host=lambda: ('41', actual), wait_for=lambda *args: None)
        with patch.object(subprocess, 'run', side_effect=subprocess.TimeoutExpired('xdotool', 5)), \
                self.assertRaises(subprocess.TimeoutExpired):
            scope['park_pointer_for_pixels']()
        hosts = iter([('41', actual), ('42', actual)])
        scope['verified_host'] = lambda: next(hosts)
        with patch.object(subprocess, 'run'), self.assertRaises(RuntimeError):
            scope['park_pointer_for_pixels']()

    def test_capture_checks_pointer_and_keeps_original_samples_and_tolerance(self):
        source = Path(__file__).with_name('tuna-stock-svg-proof.py').read_text()
        self.assertIn('for y in range(60,800,23) for x in range(10,1280,17)', source)
        self.assertIn("<=1 for p in observed", source)
        self.assertIn("differ from the actual independent native-loader reference',35)", source)
        capture = source[source.index('def pixels('):source.index('def fallback(')]
        self.assertEqual(capture.count("scene().get('pointer_position') != pointer['pointer_position']"), 2)
        self.assertIn('pointer = park_pointer_for_pixels()', capture)
        self.assertIn("timeout=budget('SVG desktop screenshot')", capture)


if __name__ == '__main__':
    unittest.main()
