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
from vm_strip_image import (admission, canonical_image_id, fixture_members,
                            PROTOCOL_INPUTS, PROTOCOL_MEMBERS)
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

    def test_native_protocol_original_build_and_installed_outputs_are_bound(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            expected = {'/usr/bin/tuna-compositor': 'a' * 64}
            sources = {'/usr/libexec/' + name: 'packaging/marlin/vm-lane/' + name
                       for name in ('tuna-vm-input', 'tuna-vm-lifecycle', 'tuna-vm-shell-focus')}
            sources.update(fixture_members('explicit-sync'))
            for installed, name in sources.items():
                if name == 'native-protocol-build.json':
                    continue
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(name.encode())
                expected[installed] = hashlib.sha256(path.read_bytes()).hexdigest()
            build = {'schema': 1, 'source_sha256': {}, 'member_sha256': {}}
            for name in PROTOCOL_INPUTS:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(name.encode())
                build['source_sha256'][name] = hashlib.sha256(path.read_bytes()).hexdigest()
            staged = root / 'protocol-xml/wayland-protocols/source.json'
            staged.parent.mkdir(parents=True)
            staged.write_bytes(b'actual independently staged protocol source receipt')
            for name in PROTOCOL_MEMBERS:
                content = staged.read_bytes() if name.endswith('/protocol-source.json') else name.encode()
                build['member_sha256'][name] = hashlib.sha256(content).hexdigest()
            original = root / 'native-protocol-build.json'
            original.write_text(json.dumps(build))
            expected['/usr/share/tuna-explicit-sync/native-build.json'] = hashlib.sha256(original.read_bytes()).hexdigest()
            expected.update(build['member_sha256'])
            package = {'package': {'sha256': 'b' * 64}, 'required_members':
                       {'usr/bin/tuna-compositor': {'sha256': 'a' * 64}}}
            inventory = root / 'scripts/lib/maintained-media.tsv'
            inventory.parent.mkdir(parents=True, exist_ok=True)
            inventory.write_bytes(INVENTORY.read_bytes())
            def git(*arguments):
                return subprocess.check_output(['git', '-C', str(root), *arguments],
                                               text=True, stderr=subprocess.PIPE).strip()
            git('init', '-q')
            git('config', 'user.name', 'Controlled immutable image admission')
            git('config', 'user.email', 'controlled@example.invalid')
            git('add', '.')
            git('commit', '-qm', 'Actual fixture source snapshot')
            observed = {'schema': 1, 'image_id': 'sha256:' + 'c' * 64, 'member_sha256': expected}
            args = (root, git('rev-parse', 'HEAD'), observed['image_id'],
                    'ghcr.io/tuna-os/marlin:gnome@sha256:' + 'e' * 64)
            admission(package, observed, *args, fixture='explicit-sync')
            changed = copy.deepcopy(observed)
            changed['member_sha256'][PROTOCOL_MEMBERS[0]] = 'f' * 64
            with self.assertRaisesRegex(ValueError, 'installed executables'):
                admission(package, changed, *args, fixture='explicit-sync')
            staged.write_bytes(b'different archive source')
            with self.assertRaisesRegex(ValueError, 'locked archive'):
                admission(package, observed, *args, fixture='explicit-sync')
            (root / PROTOCOL_INPUTS[0]).write_bytes(b'different compiler invocation')
            with self.assertRaisesRegex(ValueError, 'build inputs'):
                admission(package, observed, *args, fixture='explicit-sync')



if __name__ == '__main__':
    unittest.main()
