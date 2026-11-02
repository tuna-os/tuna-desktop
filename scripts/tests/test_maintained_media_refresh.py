"""Real ZIP/byte and controlled API-admission tests; no UI qualification."""
import importlib.machinery
import importlib.util
import io
from pathlib import Path
import stat
import tempfile
import unittest
from unittest.mock import patch
import warnings
import zipfile

from PIL import Image

SOURCE = Path(__file__).resolve().parents[1] / 'tuna-maintained-media-refresh'
LOADER = importlib.machinery.SourceFileLoader('maintained_refresh', str(SOURCE))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
refresh = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(refresh)


class Tests(unittest.TestCase):
    def test_exact_main_run_and_attempt_are_required(self):
        source = {'id': 10, 'run_attempt': 2, 'workflow_id': 11, 'event': 'push',
                  'head_branch': 'main', 'head_sha': 'a' * 40, 'status': 'completed', 'conclusion': 'success',
                  'repository': {'full_name': refresh.REPOSITORY},
                  'head_repository': {'full_name': refresh.REPOSITORY}}
        refresh.inspect_run(source, 11, 10, 2, 'a' * 40)
        for key, value in [('event', 'pull_request'), ('head_branch', 'other'), ('run_attempt', 1),
                           ('workflow_id', 12), ('conclusion', 'failure'), ('status', 'in_progress'),
                           ('head_repository', {'full_name': 'untrusted/fork'})]:
            with self.subTest(key=key), self.assertRaises(ValueError):
                refresh.inspect_run(dict(source, **{key: value}), 11, 10, 2, 'a' * 40)

    def test_extracts_only_fixed_media_bytes_never_executable_members(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            image = io.BytesIO()
            Image.new('RGB', (8, 8), '#3584e4').save(image, format='PNG')
            archive = root / 'actual.zip'
            with zipfile.ZipFile(archive, 'w') as output:
                output.writestr('shot.png', image.getvalue())
                output.writestr('bin/untrusted', b'#!/bin/sh\nexit 0\n')
            target = root / 'selected'
            refresh.extract_selected(archive, target, {'shot.png'})
            self.assertEqual((target / 'shot.png').read_bytes(), image.getvalue())
            self.assertFalse((target / 'bin').exists())
            self.assertEqual((target / 'shot.png').stat().st_mode & 0o111, 0)

    def test_unsafe_archive_is_rejected_before_any_destination_write(self):
        for name in ('../escape', '/absolute', 'C:/drive', 'a\\escape', './shot.png', 'a//shot.png'):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as scratch:
                root = Path(scratch)
                archive = root / 'bad.zip'
                with zipfile.ZipFile(archive, 'w') as output:
                    output.writestr('shot.png', b'first otherwise selected')
                    output.writestr(name, b'unsafe')
                with self.assertRaises(ValueError):
                    refresh.extract_selected(archive, root / 'selected', {'shot.png'})
                self.assertFalse((root / 'selected').exists())

    def test_symlink_and_duplicate_entries_are_not_adopted(self):
        for symlink in (False, True):
            with self.subTest(symlink=symlink), tempfile.TemporaryDirectory() as scratch:
                root = Path(scratch)
                archive = root / 'bad.zip'
                with zipfile.ZipFile(archive, 'w') as output:
                    output.writestr('shot.png', b'first')
                    if symlink:
                        entry = zipfile.ZipInfo('alias.png')
                        entry.create_system = 3
                        entry.external_attr = (stat.S_IFLNK | 0o777) << 16
                        output.writestr(entry, 'shot.png')
                    else:
                        with warnings.catch_warnings():
                            warnings.simplefilter('ignore', UserWarning)
                            output.writestr('shot.png', b'second')
                with self.assertRaises(ValueError):
                    refresh.extract_selected(archive, root / 'selected', {'shot.png'})
                self.assertFalse((root / 'selected').exists())

    def test_newer_main_is_stale_not_an_overwrite_authorization(self):
        with patch.object(refresh, 'api', return_value={'object': {'sha': 'b' * 40}}):
            with self.assertRaisesRegex(ValueError, 'STALE'):
                refresh.latest_is('a' * 40)


if __name__ == '__main__':
    unittest.main()
