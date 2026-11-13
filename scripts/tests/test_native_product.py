"""Real temporary Git/ZIP/byte tests with controlled GitHub and Podman boundaries.

Opaque fixture bytes are NOT an OCI load, signature, native image or UI proof.
Set TUNA_NATIVE_PRODUCT_PODMAN_CONTROL=1 for the separate actual host load test;
ordinary native CI remains mandatory for product/UI qualification.
"""
import base64
import copy
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import warnings
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
import maintained_media
import native_product as product
from vm_gnome_reference import cosign_argv

# Actual Podman prints '{{.Id}}' as bare hex; product identities carry 'sha256:'.
PODMAN_ID = 'a' * 64
IMAGE = 'sha256:' + PODMAN_ID
BASENAME = 'tuna-desktop-0.1.0-1-x86_64.pkg.tar.zst'


def git(root, *args):
    return subprocess.check_output(['git', *args], cwd=root, stderr=subprocess.PIPE).decode().strip()


def repository(root):
    root.mkdir()
    git(root, 'init', '-q')
    git(root, 'config', 'user.name', 'Controlled product test')
    git(root, 'config', 'user.email', 'controlled@example.invalid')
    git(root, 'remote', 'add', 'origin', product.ORIGIN)
    inventory = root / 'scripts/lib/maintained-media.tsv'
    inventory.parent.mkdir(parents=True)
    inventory.write_bytes(maintained_media.INVENTORY.read_bytes())
    (root / 'source.txt').write_text('actual source input\n')
    git(root, 'add', '.')
    git(root, 'commit', '-qm', 'original source')
    return product.source_identity(root)


def media_commit(root):
    name = next(iter(maintained_media.inventory().values()))[0][1]
    destination = root / name
    destination.parent.mkdir(parents=True, exist_ok=True)
    destination.write_bytes(b'new generated media fixture, no UI claim')
    git(root, 'add', '.')
    git(root, 'commit', '-qm', 'generated media only')
    return product.source_identity(root)


def zip_bytes(files):
    output = io.BytesIO()
    with zipfile.ZipFile(output, 'w', compression=zipfile.ZIP_DEFLATED) as archive:
        for name, data in files.items():
            member = zipfile.ZipInfo(name)
            member.create_system = 3
            member.external_attr = (stat.S_IFREG | 0o600) << 16
            archive.writestr(member, data)
    return output.getvalue()


def metadata(original, package, platform_seed=b'controlled base manifest'):
    platform = 'sha256:' + hashlib.sha256(platform_seed).hexdigest()
    index = {'schemaVersion': 2, 'manifests': [{'digest': platform, 'platform': {'os': 'linux', 'architecture': 'amd64'}}]}
    cosign = json.dumps([{'critical': {'image': {'docker-manifest-digest': platform},
        'identity': {'docker-reference': 'ghcr.io/tuna-os/marlin@' + platform},
        'type': 'https://sigstore.dev/cosign/sign/v1'}, 'optional': {}}]).encode()
    witness = {'schema': 1, 'tool': {'executable': '/controlled/cosign', 'sha256': 'f' * 64,
                                   'version': 'GitVersion: v3.1.4\n'},
        'argv': cosign_argv('/controlled/cosign', platform), 'exit_code': 0,
        'begin_host_monotonic_ns': 1, 'end_host_monotonic_ns': 2,
        'stdout_sha256': hashlib.sha256(cosign).hexdigest(), 'stdout_bytes': len(cosign), 'stderr': ''}
    files = {name: b'controlled retained metadata\n' for name in product.metadata_names('distribution')}
    files.update({'native-base/index.json': json.dumps(index).encode(),
                  'native-base/platform-cosign.json': cosign,
                  'native-base/platform-verification.json': json.dumps(witness).encode(),
                  'gdm51/final-image-id': (IMAGE + '\n').encode(),
                  'strip-image.json': json.dumps({'schema': 1, 'commit': original['commit'], 'image_id': IMAGE,
                       'package_sha256': hashlib.sha256(package).hexdigest(),
                       'built_tree': original['tree'], 'capture_commit': original['commit'], 'capture_tree': original['tree'],
                       'required_source_inputs_sha256': original['required_source_inputs_sha256'],
                       'member_sha256': {'/controlled/member': hashlib.sha256(b'member').hexdigest()},
                       'package_inspection': {'package': {'sha256': hashlib.sha256(package).hexdigest(), 'size': len(package)},
                                              'required_members': {'controlled/member': {'sha256': hashlib.sha256(b'member').hexdigest()}},
                                              'pkginfo': {'pkgname': 'tuna-desktop'}, 'member_count': 1, 'decompressed_bytes': 6},
                       'base_image': 'ghcr.io/tuna-os/marlin:gnome@' + platform}).encode()})
    return files


class Process:
    def __init__(self, data, code=0):
        self.stdout = io.BytesIO(data)
        self.code = code
    def wait(self, timeout=None):
        return self.code
    def poll(self):
        return self.code
    def kill(self):
        self.code = -9


class ControlledAPI(product.GitHub):
    """REST fixture transport; real ZIP digest verification/extraction stays live."""
    def __init__(self, run, data_files, pointer_value):
        self.env = {}
        self.run = copy.deepcopy(run)
        self.workflow = {'id': 11, 'name': 'ci', 'path': product.WORKFLOW_PATH, 'state': 'active'}
        self.data_files = data_files
        self.pointer_value = pointer_value
        self.blobs = {}
        self.downloaded = []
        self.repack()

    def artifact(self, number, name, blob):
        return {'id': number, 'name': name, 'expired': False, 'size_in_bytes': len(blob),
                'digest': 'sha256:' + hashlib.sha256(blob).hexdigest(), 'workflow_run': {
                    'id': self.run['id'], 'head_sha': self.run['head_sha'], 'head_branch': 'main',
                    'repository_id': 1, 'head_repository_id': 1}}

    def repack(self):
        self.blobs[22] = zip_bytes(self.data_files)
        self.data_artifact = self.artifact(22, product.artifact_name(self.pointer_value['profile'], True), self.blobs[22])
        self.pointer_value['data_artifact'] = {k: self.data_artifact[k] for k in ('id', 'name', 'digest')}
        self.blobs[23] = zip_bytes({'product-identity.json': json.dumps(self.pointer_value).encode()})
        self.pointer_artifact = self.artifact(23, product.artifact_name(self.pointer_value['profile']), self.blobs[23])
        self.artifacts = [self.data_artifact, self.pointer_artifact]

    def api(self, path):
        if path == 'actions/workflows/ci.yml':
            return copy.deepcopy(self.workflow)
        if path.startswith('actions/workflows/11/runs?'):
            return {'workflow_runs': [copy.deepcopy(self.run)]}
        if '/artifacts?per_page=' in path:
            return {'total_count': len(self.artifacts), 'artifacts': copy.deepcopy(self.artifacts)}
        if path == 'actions/artifacts/22':
            return copy.deepcopy(self.data_artifact)
        if path in ('actions/runs/10', 'actions/runs/10/attempts/2'):
            return copy.deepcopy(self.run)
        raise AssertionError('unexpected API request: ' + path)

    def download(self, artifact, destination, bound):
        self.downloaded.append(artifact['id'])
        with patch.object(product.subprocess, 'Popen', return_value=Process(self.blobs[artifact['id']])):
            return super().download(artifact, destination, bound)


def capsule(original, qualification=None, profile='distribution'):
    qualification = qualification or original
    package = b'opaque original package bytes, not a package qualification'
    files = metadata(original, package)
    if profile == 'upstream51':
        files.update({name: b'controlled actual metadata bytes' for name in product.metadata_names(profile)
                      if name.startswith('orca51-package-provenance/')})
        for suffix in ('buildinfo', 'pkginfo'):
            files['orca51-package-provenance/orca-51.0-1-any.pkg.tar.zst.' + suffix] = b'controlled package metadata'
    files.update({'product-image.oci.tar': b'opaque OCI fixture; never actually loaded', 'tuna.pkg.tar.zst': package})
    provenance = {'schema': product.SCHEMA, 'profile': profile, 'image_id': IMAGE,
                  'package': {'file': 'tuna.pkg.tar.zst', 'basename': BASENAME, 'sha256': hashlib.sha256(package).hexdigest()},
                  'product_source': original, 'qualification_source': qualification,
                  'qualification_run': {'id': 10, 'attempt': 2},
                  'base_index': 'sha256:' + hashlib.sha256(files['native-base/index.json']).hexdigest(),
                  'base_platform': json.loads(files['native-base/index.json'])['manifests'][0]['digest'],
                  'files': {name: {'sha256': hashlib.sha256(data).hexdigest(), 'bytes': len(data)} for name, data in files.items()}}
    raw = json.dumps(provenance).encode()
    files['product-provenance.json'] = raw
    identity = dict(provenance, provenance={'sha256': hashlib.sha256(raw).hexdigest(), 'bytes': len(raw)})
    run = {'id': 10, 'run_attempt': 2, 'workflow_id': 11, 'path': product.WORKFLOW_PATH, 'name': 'ci',
           'head_sha': qualification['commit'], 'head_branch': 'main', 'event': 'push',
           'status': 'completed', 'conclusion': 'success',
           'repository': {'full_name': product.REPOSITORY, 'id': 1},
           'head_repository': {'full_name': product.REPOSITORY, 'id': 1}}
    return ControlledAPI(run, files, identity)


class NativeProductTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.directory = Path(self.temp.name)
        self.root = self.directory / 'repo'
        self.original = repository(self.root)
        self.package = self.directory / BASENAME
        self.package.write_bytes(b'actual current downloaded package, not the original')
        self.out = self.directory / 'selected'
        self.base_evidence = self.directory / 'current-native-base'
        self.base_evidence.mkdir()
        for name, data in metadata(self.original, b'current package').items():
            if name.startswith('native-base/'):
                (self.base_evidence / Path(name).name).write_bytes(data)
        self.base = product.validate_base(self.base_evidence)
        self.selection_args = {'expected_base_index': self.base['index_digest'],
                               'expected_base_platform': self.base['platform'], 'base_evidence': self.base_evidence}
        environment = patch.dict(os.environ, {'GITHUB_WORKFLOW': 'ci', 'GITHUB_JOB': 'marlin-vm',
                                             'GITHUB_REPOSITORY': product.REPOSITORY, 'GITHUB_EVENT_NAME': 'push'})
        environment.start()
        self.addCleanup(environment.stop)

    def choose(self, api, profile='distribution'):
        # This boundary records commands only; it does not prove an OCI load.
        calls = []
        def native(root, *args):
            calls.append(args)
            return PODMAN_ID if args[:2] == ('image', 'inspect') else ''
        with patch.object(product, 'podman', side_effect=native):
            result = product.select(self.root, profile, self.out, self.package, api=api, **self.selection_args)
        return result, calls

    def test_real_git_generated_change_keeps_original_product_and_current_capture(self):
        api = capsule(self.original)
        current = media_commit(self.root)
        self.assertNotEqual(current['commit'], self.original['commit'])
        self.assertNotEqual(current['tree'], self.original['tree'])
        self.assertEqual(current['required_source_inputs_sha256'], self.original['required_source_inputs_sha256'])
        result, calls = self.choose(api)
        self.assertEqual(api.downloaded, [23, 22])
        self.assertEqual(result['product_source'], self.original)
        self.assertEqual(result['current_capture_source'], current)
        self.assertEqual(result['package']['sha256'], hashlib.sha256(api.data_files['tuna.pkg.tar.zst']).hexdigest())
        self.assertNotEqual(result['package']['sha256'], product.file_identity(self.package)['sha256'])
        self.assertEqual(result['authenticated_origin']['data_artifact']['id'], 22)
        original_receipt = result['original_image_receipt']
        self.assertEqual(base64.b64decode(original_receipt['raw_base64']), api.data_files['strip-image.json'])
        self.assertEqual(calls[0][0], 'load')
        self.assertEqual(calls[2], ('tag', IMAGE, product.IMAGE_TAG))
        # Original bytes stay authenticated in the capsule/origin evidence.
        # The ordinary native admission writer exclusively creates this path.
        admission = self.root / 'vm-lane-artifacts/strip-image.json'
        self.assertFalse(admission.exists())
        with admission.open('x') as stream:
            stream.write('current independent native admission')
        self.assertEqual((self.out / 'strip-image.json').read_bytes(), api.data_files['strip-image.json'])
        outputs = product.github_outputs(result, self.out)
        self.assertEqual(outputs['restored'], 'true')
        self.assertEqual(outputs['product_commit'], self.original['commit'])
        self.assertEqual(outputs['package_path'], str(self.out / 'tuna.pkg.tar.zst'))
        self.assertEqual(outputs['product_origin_path'], str(self.out / 'selection.json'))

    def test_ordinary_dispatch_and_schedule_reuse_keep_current_admission_new(self):
        for event in ('workflow_dispatch', 'schedule'):
            with self.subTest(event=event), patch.dict(os.environ, {'GITHUB_EVENT_NAME': event}):
                self.out = self.directory / ('selected-' + event)
                selected, calls = self.choose(capsule(self.original))
                self.assertEqual(selected['mode'], 'reused')
                self.assertEqual(calls[0][0], 'load')
                self.assertFalse((self.root / 'vm-lane-artifacts/strip-image.json').exists())

    def test_privileged_and_unknown_events_never_load_authenticated_oci(self):
        for event in ('workflow_run', 'pull_request_target', 'unknown'):
            with self.subTest(event=event), patch.dict(os.environ, {'GITHUB_EVENT_NAME': event}):
                with self.assertRaisesRegex(ValueError, 'ordinary native CI job'):
                    self.choose(capsule(self.original))
                self.assertFalse(self.out.exists())

    def test_real_non_generated_change_does_not_download_large_data(self):
        api = capsule(self.original)
        (self.root / 'source.txt').write_text('changed real runtime input\n')
        git(self.root, 'add', '.')
        git(self.root, 'commit', '-qm', 'runtime input changed')
        result, calls = self.choose(api)
        self.assertEqual(result['mode'], 'fresh')
        self.assertIn('different required source inputs', result['reason'])
        self.assertEqual(result['product_source'], product.source_identity(self.root))
        self.assertEqual(result['package']['file'], str(self.package))
        self.assertEqual(result['package']['sha256'], product.file_identity(self.package)['sha256'])
        self.assertEqual(api.downloaded, [23])
        self.assertEqual(calls, [])

    def test_expired_or_missing_artifacts_require_ordinary_fresh(self):
        for mode in ('pointer', 'data', 'missing', 'gone'):
            with self.subTest(mode=mode):
                api = capsule(self.original)
                if mode == 'pointer':
                    api.pointer_artifact['expired'] = True
                elif mode == 'data':
                    api.data_artifact['expired'] = True
                elif mode == 'missing':
                    api.artifacts.remove(api.data_artifact)
                else:
                    original_api = api.api
                    def missing(path):
                        if path == 'actions/artifacts/22':
                            raise product.ArtifactUnavailable('410')
                        return original_api(path)
                    api.api = missing
                result, calls = self.choose(api)
                self.assertEqual(result['mode'], 'fresh')
                self.assertEqual(calls, [])
                shutil.rmtree(self.out)

    def test_api_auth_transport_errors_are_fatal_not_fresh(self):
        for message in ('401', '403', 'connection reset'):
            api = capsule(self.original)
            with patch.object(api, 'api', side_effect=ValueError(message)), self.assertRaises(ValueError):
                self.choose(api)
            self.assertFalse(self.out.exists())

    def test_matching_pointer_or_data_zip_digest_corruption_is_fatal(self):
        for number in (23, 22):
            api = capsule(self.original)
            api.blobs[number] += b'corrupted transport bytes'
            with self.subTest(number=number), self.assertRaisesRegex(ValueError, 'ZIP digest mismatch'):
                self.choose(api)
            self.assertFalse(self.out.exists())

    def test_inner_file_and_actual_git_tree_corruption_are_fatal(self):
        for mode in ('file', 'tree', 'head', 'run', 'artifact'):
            api = capsule(self.original)
            if mode == 'file':
                api.data_files['tuna.pkg.tar.zst'] += b'corruption'
            elif mode == 'tree':
                api.pointer_value['product_source'] = dict(self.original, tree='f' * 40)
            elif mode == 'head':
                api.run['head_sha'] = 'b' * 40
            elif mode == 'run':
                api.run['event'] = 'pull_request'
            else:
                api.data_artifact['workflow_run']['head_repository_id'] = 4
            if mode in ('file', 'tree', 'head'):
                api.repack()
            with self.subTest(mode=mode), self.assertRaises(ValueError):
                self.choose(api)
            self.assertFalse(self.out.exists())

    def test_loaded_image_identity_is_checked_before_tagging(self):
        api = capsule(self.original)
        with patch.object(product, 'podman', return_value='b' * 64) as native:
            with self.assertRaisesRegex(ValueError, 'loaded Podman image ID differs'):
                product.select(self.root, 'distribution', self.out, self.package, api=api, **self.selection_args)
        self.assertEqual(native.call_count, 2)
        self.assertFalse(self.out.exists())

    def test_selected_tag_identity_is_checked_after_tagging(self):
        api = capsule(self.original)
        answers = iter(('', PODMAN_ID, '', 'b' * 64))
        with patch.object(product, 'podman', side_effect=lambda root, *args: next(answers)) as native:
            with self.assertRaisesRegex(ValueError, 'selected Podman tag differs'):
                product.select(self.root, 'distribution', self.out, self.package, api=api, **self.selection_args)
        self.assertEqual([call.args[1] for call in native.call_args_list], ['load', 'image', 'tag', 'image'])
        self.assertFalse(self.out.exists())

    def test_actual_podman_id_format_is_canonicalized_without_admitting_another_image(self):
        # Regression: every Main retention compared bare '{{.Id}}' output with
        # the 'sha256:' product ID and failed although the image was identical.
        for observed in (PODMAN_ID, IMAGE):
            with self.subTest(observed=observed), patch.object(product, 'podman', return_value=observed) as native:
                self.assertEqual(product.podman_image_id(self.root, IMAGE), IMAGE)
                native.assert_called_once_with(self.root, 'image', 'inspect', IMAGE, '--format', '{{.Id}}')
        env = {'GITHUB_REF': 'refs/heads/main', 'GITHUB_SHA': self.original['commit'],
               'GITHUB_RUN_ID': '100', 'GITHUB_RUN_ATTEMPT': '1'}
        for observed, message in (('b' * 64, 'retained actual Podman image differs'),
                                  ('sha256:' + 'b' * 64, 'retained actual Podman image differs'),
                                  (PODMAN_ID[:12], 'full immutable Podman image ID required'),
                                  (PODMAN_ID.upper(), 'full immutable Podman image ID required'),
                                  (PODMAN_ID + '\n' + PODMAN_ID, 'full immutable Podman image ID required'),
                                  (product.IMAGE_TAG, 'full immutable Podman image ID required'),
                                  ('', 'full immutable Podman image ID required')):
            with self.subTest(observed=observed), patch.dict(os.environ, env), \
                    patch.object(product, 'podman', return_value=observed), \
                    self.assertRaisesRegex(ValueError, message):
                product.retain(self.root, 'distribution', self.out, IMAGE,
                               self.package, BASENAME, self.original['commit'])
            self.assertFalse(self.out.exists())

    def test_strict_actual_git_refs_origin_and_tree(self):
        for ref in ('HEAD~1', 'abc', '-x', 'A' * 40):
            with self.subTest(ref=ref), self.assertRaises(ValueError):
                product.source_identity(self.root, ref)
        git(self.root, 'remote', 'set-url', 'origin', 'https://github.com/other/repo.git')
        with self.assertRaisesRegex(ValueError, 'fixed trusted repository'):
            product.source_identity(self.root)

    def test_retain_requalification_preserves_original_archive_and_receipt_without_copy(self):
        api = capsule(self.original)
        current = media_commit(self.root)
        selected, _ = self.choose(api)
        archive = self.out / 'product-image.oci.tar'
        archive_inode = archive.stat().st_ino
        # Current ordinary admission is different; it must NOT become recursive retained receipt.
        current_receipt = json.loads(api.data_files['strip-image.json'])
        current_receipt['product_origin'] = selected
        current_receipt['capture_commit'] = current['commit']
        current_receipt['capture_tree'] = current['tree']
        shutil.copytree(self.base_evidence, self.root / 'vm-lane-artifacts/native-base')
        (self.root / 'vm-lane-artifacts/strip-image.json').write_text(json.dumps(current_receipt))
        env = {'GITHUB_REPOSITORY': product.REPOSITORY, 'GITHUB_EVENT_NAME': 'push',
               'GITHUB_REF': 'refs/heads/main', 'GITHUB_SHA': current['commit'], 'GITHUB_WORKFLOW': 'ci',
               'GITHUB_RUN_ID': '100', 'GITHUB_RUN_ATTEMPT': '3'}
        with patch.dict(os.environ, env), patch.object(product, 'podman', return_value=PODMAN_ID):
            result = product.retain(self.root, 'distribution', self.out, IMAGE,
                                    self.out / 'tuna.pkg.tar.zst', BASENAME, self.original['commit'])
        self.assertEqual(archive.stat().st_ino, archive_inode)
        self.assertEqual(result['product_source'], self.original)
        self.assertEqual(result['qualification_source'], current)
        self.assertEqual(result['qualification_run'], {'id': 100, 'attempt': 3})
        self.assertEqual((self.out / 'strip-image.json').read_bytes(), api.data_files['strip-image.json'])
        identity = product.pointer(self.out, 321, 'e' * 64)
        self.assertEqual(identity['data_artifact'], {'id': 321, 'name': 'native-product-DATA-distribution', 'digest': 'sha256:' + 'e' * 64})
        self.assertEqual(identity['provenance'], product.file_identity(self.out / 'product-provenance.json'))
        self.assertEqual(identity['product_source'], self.original)

    def test_pointer_rejects_untrusted_upload_ids_or_digests(self):
        for number, digest in ((0, 'e' * 64), (-1, 'e' * 64), (True, 'e' * 64), (12, 'tag:latest'), (12, 'E' * 64)):
            with self.subTest(number=number, digest=digest), self.assertRaises(ValueError):
                product.pointer(self.directory, number, digest)

    def test_package_and_profiles_are_strict(self):
        for name in ('../evil.pkg.tar.zst', 'tuna-desktop-latest.pkg.tar.zst', 'evil\noutput=x', '/tmp/' + BASENAME):
            with self.subTest(name=name), self.assertRaises(ValueError):
                product.package_basename(name)
        with self.assertRaises(ValueError):
            product.artifact_name('paired-reference')

    def test_current_index_change_even_same_platform_forces_fresh_before_data_download(self):
        api = capsule(self.original)
        index = json.loads((self.base_evidence / 'index.json').read_bytes())
        index['annotations'] = {'controlled-current-index-change': 'new published index'}
        (self.base_evidence / 'index.json').write_text(json.dumps(index))
        current_base = product.validate_base(self.base_evidence)
        self.selection_args['expected_base_index'] = current_base['index_digest']
        result, calls = self.choose(api)
        self.assertEqual(result['mode'], 'fresh')
        self.assertIn('published native base', result['reason'])
        self.assertEqual(result['base_index'], current_base['index_digest'])
        self.assertEqual(api.downloaded, [23])
        self.assertEqual(calls, [])

    def test_current_base_evidence_is_required_and_not_overwritten_by_restore(self):
        api = capsule(self.original)
        current = self.root / 'vm-lane-artifacts/native-base'
        shutil.copytree(self.base_evidence, current)
        witness = json.loads((current / 'platform-verification.json').read_bytes())
        witness['begin_host_monotonic_ns'] = 10
        witness['end_host_monotonic_ns'] = 20
        (current / 'platform-verification.json').write_text(json.dumps(witness))
        expected = (current / 'platform-verification.json').read_bytes()
        self.selection_args['base_evidence'] = current
        selected, _ = self.choose(api)
        self.assertEqual(selected['mode'], 'reused')
        self.assertEqual((current / 'platform-verification.json').read_bytes(), expected)
        self.assertNotEqual((self.out / 'native-base/platform-verification.json').read_bytes(), expected)

    def test_forged_retained_base_pair_is_rejected_before_load(self):
        api = capsule(self.original)
        provenance = json.loads(api.data_files['product-provenance.json'])
        raw_index = json.loads(api.data_files['native-base/index.json'])
        raw_index['annotations'] = {'forged-historical-index': 'does not equal authenticated pair'}
        api.data_files['native-base/index.json'] = json.dumps(raw_index).encode()
        provenance['files']['native-base/index.json'] = {
            'sha256': hashlib.sha256(api.data_files['native-base/index.json']).hexdigest(),
            'bytes': len(api.data_files['native-base/index.json'])}
        raw = json.dumps(provenance).encode()
        api.data_files['product-provenance.json'] = raw
        api.pointer_value = dict(provenance, provenance={'sha256': hashlib.sha256(raw).hexdigest(), 'bytes': len(raw)})
        api.repack()
        with self.assertRaisesRegex(ValueError, 'base pair differs'):
            self.choose(api)
        self.assertFalse(self.out.exists())

    def test_fresh_retention_exports_exact_selected_product_and_builds_bound_pointer(self):
        api = capsule(self.original)
        api.pointer_artifact['expired'] = True
        selected, _ = self.choose(api)
        evidence = metadata(self.original, self.package.read_bytes())
        receipt = json.loads(evidence['strip-image.json'])
        receipt['product_origin'] = selected
        evidence['strip-image.json'] = json.dumps(receipt).encode()
        for name, data in evidence.items():
            destination = self.root / 'vm-lane-artifacts' / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(data)
        real_run = subprocess.run
        def native_process(arguments, **kwargs):
            if arguments[:3] == ['sudo', 'podman', 'save']:
                output = Path(arguments[arguments.index('--output') + 1])
                output.write_bytes(b'controlled fresh export bytes; no actual image qualification')
                self.assertEqual(arguments[-1], IMAGE)
                return subprocess.CompletedProcess(arguments, 0)
            if arguments[:2] == ['sudo', 'chown']:
                return subprocess.CompletedProcess(arguments, 0)
            return real_run(arguments, **kwargs)
        env = {'GITHUB_REF': 'refs/heads/main', 'GITHUB_SHA': self.original['commit'],
               'GITHUB_RUN_ID': '100', 'GITHUB_RUN_ATTEMPT': '1'}
        with patch.dict(os.environ, env), patch.object(product, 'podman', return_value=PODMAN_ID), \
                patch.object(product.subprocess, 'run', side_effect=native_process):
            retained = product.retain(self.root, 'distribution', self.out, IMAGE,
                                      self.package, BASENAME, self.original['commit'])
        self.assertEqual(retained['product_source'], self.original)
        self.assertEqual(retained['qualification_source'], self.original)
        self.assertEqual(retained['base_index'], self.base['index_digest'])
        self.assertEqual(retained['package']['sha256'], product.file_identity(self.package)['sha256'])
        self.assertNotIn('product-provenance.json', retained['files'])
        identity = product.pointer(self.out, 345, 'd' * 64)
        self.assertEqual(identity['files'], retained['files'])
        self.assertEqual(identity['base_platform'], self.base['platform'])

    def test_pointer_attempt_and_full_workflow_run_trust_are_not_optional(self):
        for key, value in (('path', '.github/workflows/other.yml'), ('workflow_id', 12),
                           ('head_branch', 'other'), ('status', 'in_progress'),
                           ('conclusion', 'failure'), ('head_repository', {'full_name': 'other/fork', 'id': 2})):
            api = capsule(self.original)
            api.run[key] = value
            with self.subTest(key=key), self.assertRaises(ValueError):
                self.choose(api)
        api = capsule(self.original)
        api.pointer_value['qualification_run'] = {'id': 10, 'attempt': 1}
        api.repack()
        selected, calls = self.choose(api)
        self.assertEqual(selected['mode'], 'fresh')
        self.assertEqual(api.downloaded, [23])
        self.assertEqual(calls, [])

    def test_upstream_profile_restores_actual_any_arch_orca_metadata(self):
        api = capsule(self.original, profile='upstream51')
        selected, _ = self.choose(api, profile='upstream51')
        self.assertEqual(selected['profile'], 'upstream51')
        self.assertEqual(selected['authenticated_origin']['data_artifact']['name'], 'native-product-DATA-upstream51')
        self.assertEqual((self.root / 'orca51-package-provenance/orca-51.0-1-any.pkg.tar.zst.buildinfo').read_bytes(),
                         b'controlled package metadata')
        self.assertFalse((self.root / 'orca51-package-provenance/orca.PKGBUILD').exists())

    def test_missing_or_forged_current_base_evidence_never_reaches_lookup(self):
        api = capsule(self.original)
        self.selection_args['expected_base_index'] = 'sha256:' + 'f' * 64
        with self.assertRaisesRegex(ValueError, 'current base evidence differs'):
            self.choose(api)
        self.assertEqual(api.downloaded, [])
        self.assertFalse(self.out.exists())

    def test_claimed_equal_source_hash_cannot_relabel_actual_old_runtime_inputs(self):
        api = capsule(self.original)
        (self.root / 'source.txt').write_text('genuinely different runtime source')
        git(self.root, 'add', '.')
        git(self.root, 'commit', '-qm', 'changed source')
        current = product.source_identity(self.root)
        api.pointer_value['product_source'] = dict(self.original, required_source_inputs_sha256=current['required_source_inputs_sha256'])
        api.pointer_value['qualification_source'] = dict(self.original, required_source_inputs_sha256=current['required_source_inputs_sha256'])
        api.repack()
        with self.assertRaisesRegex(ValueError, 'actual Git source trees/inputs'):
            self.choose(api)
        self.assertEqual(api.downloaded, [23])
        self.assertFalse(self.out.exists())

    def test_changed_current_platform_requires_current_fresh_product(self):
        api = capsule(self.original)
        for name, data in metadata(self.original, b'current', platform_seed=b'new current platform').items():
            if name.startswith('native-base/'):
                (self.base_evidence / Path(name).name).write_bytes(data)
        current_base = product.validate_base(self.base_evidence)
        self.selection_args.update(expected_base_index=current_base['index_digest'],
                                   expected_base_platform=current_base['platform'])
        selected, calls = self.choose(api)
        self.assertEqual(selected['mode'], 'fresh')
        self.assertEqual(selected['base_platform'], current_base['platform'])
        self.assertEqual(api.downloaded, [23])
        self.assertEqual(calls, [])

    def test_select_cli_emits_validated_actual_fresh_package_and_base_outputs(self):
        from contextlib import redirect_stdout
        api = capsule(self.original)
        api.pointer_artifact['expired'] = True
        output = self.directory / 'github-output'
        stdout = io.StringIO()
        arguments = ['select', '--profile', 'distribution', '--out', str(self.out),
                     '--current-package', str(self.package), '--github-output', str(output),
                     '--expected-base-index', self.base['index_digest'],
                     '--expected-base-platform', self.base['platform'], '--base-evidence', str(self.base_evidence)]
        with patch.object(product, 'GitHub', return_value=api), redirect_stdout(stdout):
            self.assertEqual(product.main(arguments, root=self.root), 0)
        values = json.loads(stdout.getvalue())
        self.assertEqual(values['restored'], 'false')
        self.assertEqual(values['package_path'], str(self.package))
        self.assertEqual(values['package_sha256'], product.file_identity(self.package)['sha256'])
        self.assertEqual(values['base_index'], self.base['index_digest'])
        self.assertEqual(dict(line.split('=', 1) for line in output.read_text().splitlines()), values)

    def test_absent_strict_commit_fetches_real_git_tree_only_from_fixed_origin(self):
        donor = self.directory / 'donor'
        repository(donor)
        (donor / 'source.txt').write_text('different real donor input')
        git(donor, 'add', '.')
        git(donor, 'commit', '-qm', 'distinct fetchable actual source')
        expected = product.source_identity(donor)
        real_run = subprocess.run
        fetched = []
        def controlled_network(arguments, **kwargs):
            if 'fetch' in arguments:
                self.assertEqual(arguments[-2], product.ORIGIN)
                self.assertEqual(arguments[-1], expected['commit'])
                self.assertIn('credential.helper=', arguments)
                self.assertEqual(kwargs['env']['GIT_TERMINAL_PROMPT'], '0')
                fetched.append(arguments[-1])
                arguments = list(arguments)
                arguments[-2] = str(donor)
            return real_run(arguments, **kwargs)
        with patch.object(product.subprocess, 'run', side_effect=controlled_network):
            actual = product.source_identity(self.root, expected['commit'])
        self.assertEqual(actual, expected)
        self.assertEqual(fetched, [expected['commit']])
        self.assertEqual(git(self.root, 'rev-parse', 'HEAD'), self.original['commit'])


class ArchiveSecurityTests(unittest.TestCase):
    def test_every_unsafe_path_is_rejected_before_destination_creation(self):
        for name in ('../escape', '/absolute', 'C:/drive', 'a\\escape', './strip-image.json',
                     'native-base//index.json', 'native-base/../strip-image.json', 'native-base/index.json/child',
                     'downloaded-script.sh'):
            with self.subTest(name=name), tempfile.TemporaryDirectory() as scratch:
                root = Path(scratch)
                (root / 'source.zip').write_bytes(zip_bytes({'strip-image.json': b'{}', name: b'bad'}))
                with self.assertRaises(ValueError):
                    product.extract_zip(root / 'source.zip', root / 'target', 'distribution')
                self.assertFalse((root / 'target').exists())

    def test_symlink_fifo_executable_duplicate_and_size_bounds(self):
        for mode in ('symlink', 'fifo', 'exec', 'duplicate', 'bound'):
            with self.subTest(mode=mode), tempfile.TemporaryDirectory() as scratch:
                root = Path(scratch)
                with warnings.catch_warnings(), zipfile.ZipFile(root / 'source.zip', 'w') as archive:
                    warnings.simplefilter('ignore', UserWarning)
                    entry = zipfile.ZipInfo('strip-image.json')
                    entry.create_system = 3
                    entry.external_attr = ({'symlink': stat.S_IFLNK | 0o777, 'fifo': stat.S_IFIFO | 0o600,
                                            'exec': stat.S_IFREG | 0o700}.get(mode, stat.S_IFREG | 0o600)) << 16
                    archive.writestr(entry, b'{}')
                    if mode == 'duplicate':
                        archive.writestr(entry, b'{}')
                with patch.object(product, 'MAX_METADATA', 1 if mode == 'bound' else product.MAX_METADATA):
                    with self.assertRaises(ValueError):
                        product.extract_zip(root / 'source.zip', root / 'target', 'distribution')
                self.assertFalse((root / 'target').exists())

    def test_duplicate_json_keys_and_nonregular_local_files(self):
        with self.assertRaises(ValueError):
            product.json_object(b'{"schema":1,"schema":2}')
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            (root / 'regular').write_bytes(b'real bytes')
            (root / 'alias').symlink_to(root / 'regular')
            with self.assertRaises(OSError):
                product.file_identity(root / 'alias')


@unittest.skipUnless(os.environ.get('TUNA_NATIVE_PRODUCT_PODMAN_CONTROL') == '1',
                     'opt-in real ordinary host Podman load control; no GUI qualification')
class ActualPodmanLoadControl(unittest.TestCase):
    def test_actual_opaque_export_load_preserves_real_image_id(self):
        self.assertIsNotNone(shutil.which('podman'))
        self.assertIsNotNone(shutil.which('sudo'))
        with tempfile.TemporaryDirectory() as scratch:
            root = Path(scratch)
            (root / 'payload').write_bytes(b'actual OCI load positive control only\n')
            (root / 'Containerfile').write_text('FROM scratch\nCOPY payload /payload\n')
            tag = 'localhost/tuna-native-product-load-control:' + str(os.getpid())
            image = None
            try:
                subprocess.run(['sudo', 'podman', 'build', '--network=none', '--pull=never', '-t', tag, str(root)], check=True)
                image = product.podman(root, 'image', 'inspect', tag, '--format', '{{.Id}}')
                self.assertTrue(product.hex_value(image, 64))
                canonical = 'sha256:' + image
                self.assertEqual(product.podman_image_id(root, tag), canonical)
                # Retention exports and checks by the canonical ID, exactly as here.
                self.assertEqual(product.podman_image_id(root, canonical), canonical)
                subprocess.run(['sudo', 'podman', 'save', '--format', 'oci-archive', '--output', str(root / 'actual.oci.tar'), canonical], check=True)
                subprocess.run(['sudo', 'podman', 'rmi', tag], check=True)
                product.podman(root, 'load', '--input', root / 'actual.oci.tar')
                self.assertEqual(product.podman(root, 'image', 'inspect', image, '--format', '{{.Id}}'), image)
                self.assertEqual(product.podman_image_id(root, canonical), canonical)
            finally:
                subprocess.run(['sudo', 'podman', 'rmi', image or tag], check=False, stdout=subprocess.DEVNULL)


if __name__ == '__main__':
    unittest.main()
