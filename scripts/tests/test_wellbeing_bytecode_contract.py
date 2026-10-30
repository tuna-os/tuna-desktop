#!/usr/bin/python3
"""Run actual launcher imports in fresh checkout with no developer ignore rules."""
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]

class SourceCleanImports(unittest.TestCase):
    def check_import(self, entry, remove_guard=False):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch) / 'source'
            root.mkdir()
            for name in ['scripts/tuna-wellbeing-proof', 'scripts/lib/wellbeing-bundle.py',
                         'scripts/lib/wellbeing-pixels.py', 'scripts/lib/wellbeing-pixels-tests.py',
                         'scripts/tests/test_wellbeing_bundle.py']:
                target = root / name
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(ROOT / name, target)
            if remove_guard:
                target = root / entry
                target.write_text(target.read_text().replace('sys.dont_write_bytecode = True',
                                                              'sys.dont_write_bytecode = False'))
            subprocess.run(['git', 'init', '-q', str(root)], check=True, timeout=60)
            subprocess.run(['git', '-c', 'core.excludesFile=/dev/null', 'add', '.'], cwd=root, check=True, timeout=60)
            subprocess.run(['git', '-c', 'user.name=Fixture', '-c', 'user.email=fixture@example.invalid',
                            'commit', '-qm', 'Source import fixture'], cwd=root, check=True, timeout=60)
            env = dict(os.environ)
            for name in ['PYTHONDONTWRITEBYTECODE', 'PYTHONPYCACHEPREFIX']:
                env.pop(name, None)
            subprocess.run(['/usr/bin/python3', '-c',
                            "import runpy,sys; runpy.run_path(sys.argv[1],run_name='import_probe')",
                            str(root / entry)], cwd=root, env=env, check=True, timeout=15)
            status = subprocess.check_output(['git', '-c', 'core.excludesFile=/dev/null',
                                               'status', '--porcelain'], cwd=root, text=True, timeout=60)
            self.assertEqual(status, '', 'actual import must not dirty the admitted tree')
            self.assertFalse(list(root.rglob('*.pyc')), 'no hidden bytecode side effect')

    def test_actual_proof_import_keeps_source_clean(self):
        self.check_import('scripts/tuna-wellbeing-proof')

    def test_unprotected_import_is_rejected_even_with_developer_global_ignores(self):
        for entry in ['scripts/tuna-wellbeing-proof', 'scripts/tests/test_wellbeing_bundle.py',
                      'scripts/lib/wellbeing-pixels-tests.py']:
            with self.subTest(entry=entry), self.assertRaisesRegex(AssertionError, 'actual import'):
                self.check_import(entry, remove_guard=True)

    def test_later_pixel_test_import_keeps_source_clean(self):
        self.check_import('scripts/lib/wellbeing-pixels-tests.py')

    def test_preceding_admission_test_import_keeps_source_clean(self):
        self.check_import('scripts/tests/test_wellbeing_bundle.py')

if __name__ == '__main__':
    unittest.main()
