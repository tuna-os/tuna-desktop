"""Real ZIP/byte and controlled API-admission tests; no UI qualification."""
import base64
import copy
import hashlib
import json
import importlib.machinery
import importlib.util
import io
from pathlib import Path
import stat
import tempfile
import subprocess
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
import maintained_media
from vm_gnome_reference import cosign_argv


def git(root, *arguments):
    return subprocess.check_output(['git', *arguments], cwd=root, stderr=subprocess.PIPE).decode().strip()


def fixture_repository(root):
    git(root, 'init', '-q')
    git(root, 'config', 'user.name', 'Controlled media regression')
    git(root, 'config', 'user.email', 'controlled@example.invalid')
    git(root, 'config', 'diff.renames', 'true')
    inventory = root / 'scripts/lib/maintained-media.tsv'
    inventory.parent.mkdir(parents=True)
    inventory.write_bytes(maintained_media.INVENTORY.read_bytes())
    destination = refresh.inventory()['gtk-walkthrough'][0][1]
    path = root / destination
    path.parent.mkdir(parents=True)
    Image.new('RGB', (8, 8), '#3584e4').save(path)
    git(root, 'add', '.')
    git(root, 'commit', '-qm', 'Controlled original inventory')
    return git(root, 'rev-parse', 'HEAD'), inventory, destination


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

    def test_downloaded_archive_must_equal_exact_upload_digest(self):
        mapping = refresh.inventory()
        member_name = mapping['gtk-walkthrough'][0][0]
        image = io.BytesIO()
        Image.new('RGB', (8, 8), '#3584e4').save(image, format='PNG')
        # Controlled modern output/process bytes for ZIP retention only, not
        # an actual successful verifier or native-image qualification.
        platform_digest = 'sha256:' + '5' * 64
        cosign = json.dumps([{
            'critical': {'image': {'docker-manifest-digest': platform_digest},
                         'identity': {'docker-reference': maintained_media.BASE_REPOSITORY + '@' + platform_digest},
                         'type': 'https://sigstore.dev/cosign/sign/v1'},
            'optional': {},
        }]).encode()
        process = json.dumps({
            'schema': 1, 'argv': cosign_argv('/usr/bin/cosign', platform_digest), 'exit_code': 0,
            'begin_host_monotonic_ns': 100, 'end_host_monotonic_ns': 200,
            'tool': {'executable': '/usr/bin/cosign', 'sha256': 'e' * 64, 'version': 'GitVersion: v3.1.4\n'},
            'stdout_sha256': hashlib.sha256(cosign).hexdigest(), 'stdout_bytes': len(cosign), 'stderr': '',
        }).encode()
        archives, members = {}, []
        for identifier, name in enumerate(('gtk-shell', 'marlin-vm', 'tuna-tour', 'overview-demo'), 1):
            stream = io.BytesIO()
            with zipfile.ZipFile(stream, 'w') as output:
                output.writestr(member_name if name == 'gtk-shell' else 'unselected.txt', image.getvalue())
                output.writestr('bin/untrusted', b'#!/bin/sh\nexit 0\n')
                if name == 'marlin-vm':
                    output.writestr('native-base/platform-cosign.json', cosign)
                    output.writestr('native-base/platform-verification.json', process)
            archives[identifier] = stream.getvalue()
            members.append({'id': identifier, 'name': name, 'expired': False,
                            'workflow_run': {'id': 10}, 'size_in_bytes': len(archives[identifier]),
                            'digest': 'sha256:' + hashlib.sha256(archives[identifier]).hexdigest()})
        def transfer(argv, *, stdout, **kwargs):
            identifier = int(argv[-1].split('/')[-2])
            stdout.write(archives[identifier])
            return subprocess.CompletedProcess(argv, 0)
        with patch.object(refresh, 'api', return_value={'total_count': 4, 'artifacts': members}), \
                patch.object(refresh.subprocess, 'run', side_effect=transfer):
            with tempfile.TemporaryDirectory() as scratch:
                roots, receipts = refresh.download_artifacts(10, 2, Path(scratch), mapping)
                self.assertEqual((roots['gtk-shell'] / member_name).read_bytes(), image.getvalue())
                self.assertEqual(receipts['gtk-shell']['digest'], members[0]['digest'])
                self.assertFalse((roots['gtk-shell'] / 'bin').exists())
                self.assertEqual((roots['marlin-vm'] / 'native-base/platform-cosign.json').read_bytes(), cosign)
                self.assertEqual((roots['marlin-vm'] / 'native-base/platform-verification.json').read_bytes(), process)
                self.assertFalse((roots['marlin-vm'] / 'bin').exists())
            archives[1] += b'bytes not in the authenticated upload'
            with tempfile.TemporaryDirectory() as scratch:
                with self.assertRaisesRegex(ValueError, 'differs from upload digest'):
                    refresh.download_artifacts(10, 2, Path(scratch), mapping)
                self.assertFalse((Path(scratch) / 'gtk-shell').exists())

    def test_hero_download_reassembles_authenticated_vm_evidence_and_tour(self):
        mapping = refresh.inventory()
        tour_name = mapping['vm-tour'][0][0].removeprefix('tour/')
        uploads = {'marlin-vm': {'media-receipt.json': b'original native receipt',
                                 'media-source.json': b'original native source'},
                   'tuna-tour': {tour_name: b'original hero bytes',
                                 'tuna-tour.webm': b'original walkthrough video'}}
        members, archives = [], {}
        for identifier, (name, files) in enumerate(uploads.items(), 1):
            stream = io.BytesIO()
            with zipfile.ZipFile(stream, 'w') as archive:
                for path, data in files.items():
                    archive.writestr(path, data)
            archives[identifier] = stream.getvalue()
            members.append({'id': identifier, 'name': name, 'expired': False,
                            'workflow_run': {'id': 10}, 'size_in_bytes': len(stream.getvalue()),
                            'digest': 'sha256:' + hashlib.sha256(stream.getvalue()).hexdigest()})
        downloaded = []
        def transfer(argv, *, stdout, **kwargs):
            identifier = int(argv[-1].split('/')[-2])
            downloaded.append(identifier)
            stdout.write(archives[identifier])
            return subprocess.CompletedProcess(argv, 0)
        with patch.object(refresh, 'api', return_value={'total_count': 2, 'artifacts': members}), \
                patch.object(refresh.subprocess, 'run', side_effect=transfer):
            with tempfile.TemporaryDirectory() as scratch:
                roots, receipts = refresh.download_artifacts(10, 2, Path(scratch), mapping, vm_tour_only=True)
                self.assertEqual(downloaded, [1, 2])
                self.assertEqual(set(receipts), {'marlin-vm', 'tuna-tour'})
                native = roots['marlin-vm']
                self.assertEqual((native / 'media-receipt.json').read_bytes(), b'original native receipt')
                self.assertEqual((native / 'tour' / tour_name).read_bytes(), b'original hero bytes')
                self.assertEqual((native / 'tour/tuna-tour.webm').read_bytes(), b'original walkthrough video')
            archives[2] += b'forged outside upload digest'
            with tempfile.TemporaryDirectory() as scratch:
                with self.assertRaisesRegex(ValueError, 'differs from upload digest'):
                    refresh.download_artifacts(10, 2, Path(scratch), mapping, vm_tour_only=True)
                self.assertFalse((Path(scratch) / 'marlin-vm/tour').exists())

    def test_ordinary_and_automation_branches_cannot_change_unproven_canonical_pixels(self):
        for branch in ('ordinary-doc-fix', 'automation/maintained-media-10-2'):
            with self.subTest(branch=branch), tempfile.TemporaryDirectory() as scratch:
                root = Path(scratch)
                base, inventory, destination = fixture_repository(root)
                git(root, 'switch', '-qc', branch)
                Image.new('RGB', (8, 8), '#f66151').save(root / destination)
                git(root, 'add', '.')
                git(root, 'commit', '-qm', 'Unproven canonical replacement')
                with patch.object(refresh, 'ROOT', root), patch.object(maintained_media, 'INVENTORY', inventory):
                    self.assertIn(destination, refresh.canonical_changes(base))
                    with self.assertRaises(FileNotFoundError):
                        refresh.verify_if_changed(base)

    def test_removing_an_inventory_row_does_not_hide_changed_canonical_bytes(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            base, inventory, destination = fixture_repository(root)
            rows = inventory.read_text().splitlines()
            inventory.write_text('\n'.join(row for row in rows if destination not in row) + '\n')
            Image.new('RGB', (8, 8), '#f66151').save(root / destination)
            git(root, 'add', '.')
            git(root, 'commit', '-qm', 'Remove row and replace its formerly canonical pixels')
            with patch.object(refresh, 'ROOT', root), patch.object(maintained_media, 'INVENTORY', inventory):
                self.assertIn(destination, refresh.canonical_changes(base))
                with self.assertRaises(FileNotFoundError):
                    refresh.verify_if_changed(base)

    def test_real_renames_cannot_hide_native_hero_video_or_manifest_removal(self):
        mapping = refresh.inventory()
        destinations = (mapping['vm-tour'][0][1], mapping['overview-demo'][0][1],
                        refresh.manifest_name('vm-tour'))
        for destination in destinations:
            with self.subTest(destination=destination), tempfile.TemporaryDirectory() as scratch:
                root = Path(scratch)
                _, inventory, _ = fixture_repository(root)
                original = root / destination
                original.parent.mkdir(parents=True, exist_ok=True)
                original.write_bytes(b'Controlled exact bytes for the Git rename detector, not UI proof.\n')
                git(root, 'add', '.')
                git(root, 'commit', '-qm', 'Controlled canonical path')
                base = git(root, 'rev-parse', 'HEAD')
                moved = root / 'outside-inventory' / original.name
                moved.parent.mkdir()
                git(root, 'mv', destination, str(moved.relative_to(root)))
                git(root, 'commit', '-qm', 'Move canonical bytes outside the inventory')
                self.assertIn('R100', git(root, 'diff', '--name-status', base + '...HEAD'))
                with patch.object(refresh, 'ROOT', root), patch.object(maintained_media, 'INVENTORY', inventory):
                    self.assertIn(destination, refresh.canonical_changes(base))
                    with self.assertRaises(FileNotFoundError):
                        refresh.verify_if_changed(base)

    def test_unrelated_source_change_is_not_a_generated_media_replacement(self):
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            base, inventory, _ = fixture_repository(root)
            (root / 'product.rs').write_text('fn main() {}\n')
            git(root, 'add', '.')
            git(root, 'commit', '-qm', 'Normal source change')
            with patch.object(refresh, 'ROOT', root), patch.object(maintained_media, 'INVENTORY', inventory):
                self.assertFalse(refresh.verify_if_changed(base))

    def test_selected_elf_equality_cannot_prove_whole_image_or_requested_base_currentness(self):
        original = {'base_image': {'reference': 'immutable-base'},
                    'member_sha256': {'usr/bin/tuna-desktop': 'a' * 64},
                    'image_id': 'sha256:' + '1' * 64, 'package_sha256': '2' * 64}
        index = 'sha256:' + '3' * 64
        documents = [{'receipt': {'bundle': 'vm-tour', 'base_verification': {'index': {'sha256': '3' * 64}}},
                      'evidence': {'image_receipt': base64.b64encode(json.dumps(original).encode()).decode()}}]
        self.assertTrue(refresh.same_native_product(original, documents, index))
        for key, replacement in (
                ('image_id', 'sha256:' + '4' * 64),  # GTK/Mesa/fonts may differ despite identical selected ELFs.
                ('package_sha256', '5' * 64),
                ('base_image', {'reference': 'different-immutable-base'}),
                ('member_sha256', {'usr/bin/tuna-desktop': 'b' * 64})):
            with self.subTest(key=key):
                changed = copy.deepcopy(original)
                changed[key] = replacement
                self.assertFalse(refresh.same_native_product(changed, documents, index))
        self.assertFalse(refresh.same_native_product(original, documents, 'sha256:' + '6' * 64))
        self.assertEqual(json.loads(base64.b64decode(documents[0]['evidence']['image_receipt'])), original)


if __name__ == '__main__':
    unittest.main()
