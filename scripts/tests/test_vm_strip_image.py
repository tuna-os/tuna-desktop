"""Original package/image/source identity negatives; no image or guest execution."""
import copy
import hashlib
import json
from pathlib import Path
import sys
import subprocess
import tempfile
import unittest
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
from vm_strip_image import admission, fixture_members, canonical_image_id
from maintained_media import INVENTORY


class Tests(unittest.TestCase):
    def test_full_podman_image_id_canonicalization(self):
        raw = '2037d348d48bf8e0172b59b17c065b5401e92c988154dc8c90a75b8895b41dbd'
        self.assertEqual(canonical_image_id(raw), 'sha256:' + raw)
        self.assertEqual(canonical_image_id('sha256:' + raw), 'sha256:' + raw)
        for invalid in (None, True, raw[:12], raw.upper(), 'sha256:sha256:' + raw,
                        'localhost/marlin-tuna-vm:ci', raw + '\n', ' ' + raw,
                        'sha512:' + raw, raw + '0'):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                canonical_image_id(invalid)

    def test_installed_bytes_and_exact_image_identity_required(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            fixture = root / 'packaging/marlin/vm-lane'
            fixture.mkdir(parents=True)
            expected = {'/usr/bin/tuna-compositor': 'a' * 64}
            for name in ('tuna-vm-input', 'tuna-vm-lifecycle', 'tuna-vm-shell-focus'):
                (fixture / name).write_text(name)
                expected['/usr/libexec/' + name] = hashlib.sha256(name.encode()).hexdigest()
            inventory = root / 'scripts/lib/maintained-media.tsv'
            inventory.parent.mkdir(parents=True)
            inventory.write_bytes(INVENTORY.read_bytes())
            def git(*arguments):
                return subprocess.check_output(['git', '-C', str(root), *arguments],
                                               text=True, stderr=subprocess.PIPE).strip()
            git('init', '-q')
            git('config', 'user.name', 'Controlled immutable image admission')
            git('config', 'user.email', 'controlled@example.invalid')
            git('add', '.')
            git('commit', '-qm', 'Actual fixture source snapshot')
            commit = git('rev-parse', 'HEAD')
            package = {'package': {'sha256': 'b' * 64}, 'required_members':
                       {'usr/bin/tuna-compositor': {'sha256': 'a' * 64}}}
            observed = {'schema': 1, 'image_id': 'sha256:' + 'c' * 64, 'member_sha256': expected}
            args = (root, commit, observed['image_id'], 'ghcr.io/tuna-os/marlin:gnome@sha256:' + 'e' * 64)
            receipt = admission(package, observed, *args)
            self.assertFalse(receipt['installed_signed_release_qualified'])
            self.assertEqual(receipt['commit'], commit)
            self.assertEqual(receipt['capture_commit'], commit)
            self.assertEqual(receipt['built_tree'], git('rev-parse', 'HEAD^{tree}'))
            with self.assertRaises(ValueError):
                admission(package, observed, root, commit, 'c' * 64, args[-1])
            for key, value in [('schema', 2), ('image_id', 'sha256:' + 'f' * 64),
                               ('member_sha256', {**expected, '/unrelated': 'a' * 64}),
                               ('member_sha256', {**expected, '/usr/bin/tuna-compositor': 'f' * 64})]:
                wrong = copy.deepcopy(observed)
                wrong[key] = value
                with self.assertRaises(ValueError):
                    admission(package, wrong, *args)
            (fixture / 'tuna-vm-input').write_text('changed source')
            with self.assertRaises(ValueError):
                admission(package, observed, *args)

    def test_colour_admission_binds_actual_overlay_bytes_and_source_revision(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            expected = {'/usr/bin/tuna-compositor': 'a' * 64}
            sources = {**{('/usr/libexec/' + name): ('packaging/marlin/vm-lane/' + name)
                          for name in ('tuna-vm-input', 'tuna-vm-lifecycle', 'tuna-vm-shell-focus')},
                       **fixture_members('colour')}
            # The overlay is a CI build artifact outside the source tree: the
            # source commit is recorded inside it, after that commit exists.
            overlay = {installed: source for installed, source in sources.items()
                       if source.startswith('native-colour-overlay/')}
            for installed, source in sources.items():
                if installed in overlay:
                    continue
                path = root / source
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(installed.encode())
                expected[installed] = hashlib.sha256(path.read_bytes()).hexdigest()
            inventory = root / 'scripts/lib/maintained-media.tsv'
            inventory.parent.mkdir(parents=True)
            inventory.write_bytes(INVENTORY.read_bytes())
            def git(*arguments):
                return subprocess.check_output(['git', '-C', str(root), *arguments],
                                               text=True, stderr=subprocess.PIPE).strip()
            git('init', '-q')
            git('config', 'user.name', 'Controlled immutable image admission')
            git('config', 'user.email', 'controlled@example.invalid')
            git('add', '.')
            git('commit', '-qm', 'Actual fixture source snapshot')
            commit = git('rev-parse', 'HEAD')
            for installed, source in overlay.items():
                if installed == '/usr/share/tuna-colour/source.json':
                    continue
                path = root / source
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(installed.encode())
                expected[installed] = hashlib.sha256(path.read_bytes()).hexdigest()
            receipt = {'revision': commit, 'installed_signed_release_qualified': False,
                       'elf_sha256': {name: expected['/usr/libexec/tuna-colour/' + name]
                                      for name in ('unit', 'pixel', 'yuv', 'cast')}}
            source_receipt = root / sources['/usr/share/tuna-colour/source.json']
            source_receipt.parent.mkdir(parents=True)
            source_receipt.write_text(json.dumps(receipt))
            expected['/usr/share/tuna-colour/source.json'] = hashlib.sha256(source_receipt.read_bytes()).hexdigest()
            package = {'package': {'sha256': 'b' * 64}, 'required_members':
                       {'usr/bin/tuna-compositor': {'sha256': 'a' * 64}}}
            observed = {'schema': 1, 'image_id': 'sha256:' + 'c' * 64, 'member_sha256': expected}
            args = (root, commit, observed['image_id'], 'ghcr.io/tuna-os/marlin:gnome@sha256:' + 'e' * 64)
            self.assertEqual(admission(package, observed, *args, fixture='colour')['member_sha256'], expected)
            (root / sources['/usr/libexec/tuna-colour/cast']).write_bytes(b'changed native consumer')
            with self.assertRaisesRegex(ValueError, 'ELF receipt'):
                admission(package, observed, *args, fixture='colour')
            receipt['revision'] = 'f' * 40
            source_receipt.write_text(json.dumps(receipt))
            with self.assertRaisesRegex(ValueError, 'exact source'):
                admission(package, observed, *args, fixture='colour')

    def test_native_autotz_requires_every_original_observer(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            sources = {'/usr/libexec/' + name: 'packaging/marlin/vm-lane/' + name
                       for name in ('tuna-vm-input', 'tuna-vm-lifecycle', 'tuna-vm-shell-focus')}
            sources.update(fixture_members('autotz'))
            expected = {'/usr/bin/tuna-compositor': 'a' * 64}
            for installed, source in sources.items():
                path = root / source
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(source)
                expected[installed] = hashlib.sha256(source.encode()).hexdigest()
            inventory = root / 'scripts/lib/maintained-media.tsv'
            inventory.parent.mkdir(parents=True)
            inventory.write_bytes(INVENTORY.read_bytes())
            def git(*arguments):
                return subprocess.check_output(['git', '-C', str(root), *arguments],
                                               text=True, stderr=subprocess.PIPE).strip()
            git('init', '-q')
            git('config', 'user.name', 'Controlled immutable image admission')
            git('config', 'user.email', 'controlled@example.invalid')
            git('add', '.')
            git('commit', '-qm', 'Actual fixture source snapshot')
            package = {'package': {'sha256': 'b' * 64}, 'required_members':
                       {'usr/bin/tuna-compositor': {'sha256': 'a' * 64}}}
            observed = {'schema': 1, 'image_id': 'sha256:' + 'c' * 64, 'member_sha256': expected}
            args = (root, git('rev-parse', 'HEAD'), observed['image_id'],
                    'ghcr.io/tuna-os/marlin:gnome@sha256:' + 'e' * 64)
            self.assertFalse(admission(package, observed, *args, fixture='autotz')['installed_signed_release_qualified'])
            for installed in sources:
                missing = copy.deepcopy(observed)
                del missing['member_sha256'][installed]
                with self.subTest(installed=installed), self.assertRaises(ValueError):
                    admission(package, missing, *args, fixture='autotz')
                altered = copy.deepcopy(observed)
                altered['member_sha256'][installed] = 'f' * 64
                with self.assertRaises(ValueError):
                    admission(package, altered, *args, fixture='autotz')

    def test_unknown_privileged_fixture_is_rejected(self):
        with self.assertRaises(ValueError):
            fixture_members('arbitrary-path')


if __name__ == '__main__':
    unittest.main()
