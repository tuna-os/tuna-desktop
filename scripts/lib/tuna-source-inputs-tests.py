"""Functional admission fixtures; no Cargo, package build or publication."""
import copy
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch
import zipfile

spec = importlib.util.spec_from_file_location('inputs', Path(__file__).with_name('tuna-source-inputs.py'))
inputs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(inputs)
SOURCE_SHA = 'a' * 40
TREE_SHA = 'b' * 40
PRODUCER_SHA = 'c' * 40
SOURCE = {'run_id': 10, 'run_attempt': 1, 'event': 'workflow_dispatch', 'source_sha': SOURCE_SHA,
          'tree_sha': TREE_SHA, 'overall_conclusion': 'failure', 'required_jobs': {'check': 11, 'arch-package': 12}}


def tar(entries):
    result = io.BytesIO()
    with tarfile.open(fileobj=result, mode='w:gz') as archive:
        for name, content, kind in entries:
            member = tarfile.TarInfo(name)
            member.mode = 0o644
            if kind == 'symlink':
                member.type = tarfile.SYMTYPE
                member.linkname = content.decode()
                archive.addfile(member)
            else:
                member.size = len(content)
                archive.addfile(member, io.BytesIO(content))
    return result.getvalue()


def bundle(change=None, tar_change=None):
    lock = b'version = 4\n'
    config = b'[source.crates-io]\nreplace-with="vendored-sources"\n[source.vendored-sources]\ndirectory="vendor"\n'
    source_entries = [('tuna-desktop-' + SOURCE_SHA + '/Cargo.lock', lock, 'file')]
    if tar_change:
        source_entries = tar_change(source_entries)
    tree = {'sha': TREE_SHA, 'truncated': False, 'tree': [{'path': 'Cargo.lock', 'type': 'blob',
             'mode': '100644', 'sha': hashlib.sha1(b'blob ' + str(len(lock)).encode() + b'\0' + lock).hexdigest()}]}
    files = {name: b'evidence\n' for name in inputs.FILES}
    files.update({'Cargo.lock': lock, 'vendor-config.toml': config,
                  'cargo-metadata.json': b'{"packages":[{"name":"fixture"}]}',
                  'tuna-desktop-source.tar.gz': tar(source_entries),
                  'tuna-desktop-vendor.tar.gz': tar([('.cargo/config.toml', config, 'file'),
                                                   ('vendor/fixture/Cargo.toml', b'fixture', 'file')])})
    receipt = {'schema': 1, 'candidate_only': True, 'release_qualified': False, 'published': False,
               'source': copy.deepcopy(SOURCE),
               'producer': {'run_id': 20, 'run_attempt': 2, 'source_sha': PRODUCER_SHA, 'workflow': inputs.WORKFLOW},
               'lock_sha256': inputs.sha256(lock),
               'artifacts': {name: {'sha256': inputs.sha256(data), 'size': len(data)} for name, data in files.items()},
               'commands': ['cargo vendor --locked --versioned-dirs vendor',
                            'cargo metadata --locked --offline --format-version 1']}
    if change:
        change(receipt, files)
    files['receipt.json'] = json.dumps(receipt).encode()
    result = io.BytesIO()
    with zipfile.ZipFile(result, 'w') as archive:
        for name, data in files.items():
            archive.writestr(name, data)
    data = result.getvalue()
    artifact = {'id': 30, 'name': 'tuna-source-inputs-20-2', 'expired': False,
                'digest': 'sha256:' + inputs.sha256(data), 'size_in_bytes': len(data),
                'workflow_run': {'id': 20, 'head_sha': PRODUCER_SHA}}
    return data, artifact, tree


def run(run_id, sha, workflow, event='workflow_dispatch', conclusion='success'):
    return {'id': run_id, 'head_sha': sha, 'path': workflow, 'event': event, 'run_attempt': 1 if run_id == 10 else 2,
            'status': 'completed', 'conclusion': conclusion,
            'repository': {'full_name': inputs.REPO}, 'head_repository': {'full_name': inputs.REPO}}


def job(name, sha, job_id, steps=()):
    return {'id': job_id, 'name': name, 'head_sha': sha, 'status': 'completed', 'conclusion': 'success',
            'steps': [{'name': name, 'status': 'completed', 'conclusion': 'success'} for name in steps]}


class AdmissionTests(unittest.TestCase):
    def verify(self, change=None, tar_change=None):
        data, artifact, tree = bundle(change, tar_change)
        return inputs.verify_bundle(data, artifact, SOURCE, 20, PRODUCER_SHA, 2, tree)

    def test_failed_overall_source_with_real_successful_gates_is_candidate_only(self):
        result = self.verify()
        self.assertEqual(result['source']['overall_conclusion'], 'failure')
        self.assertTrue(result['candidate_only'])
        self.assertFalse(result['release_qualified'])
        self.assertEqual(result['vendor_receipt']['source_sha'], SOURCE_SHA)
        self.assertEqual(result['vendor_receipt']['lock_sha256'], result['lock_sha256'])
        self.assertEqual(result['vendor_receipt']['archive_sha256'], result['vendor_archive_sha256'])
        self.assertEqual(result['vendor_receipt']['run_id'], 20)

    def test_independent_api_gates_not_receipt_boolean(self):
        data, artifact, tree = bundle()
        source_run = run(10, SOURCE_SHA, '.github/workflows/ci.yml', conclusion='failure')
        producer_run = run(20, PRODUCER_SHA, inputs.WORKFLOW)
        source_jobs = [job('check', SOURCE_SHA, 11), job('arch-package', SOURCE_SHA, 12)]
        producer_jobs = [job('unit', PRODUCER_SHA, 21), job('capture', PRODUCER_SHA, 22,
                         ('Capture exact offline inputs', 'Upload exact inputs'))]

        def api(path, raw=False):
            return {'actions/runs/10': source_run, 'actions/runs/20': producer_run,
                    'actions/runs/10/attempts/1/jobs?per_page=100&page=1': {'jobs': source_jobs, 'total_count': len(source_jobs)},
                    'actions/runs/20/attempts/2/jobs?per_page=100&page=1': {'jobs': producer_jobs, 'total_count': len(producer_jobs)},
                    f'git/commits/{SOURCE_SHA}': {'sha': SOURCE_SHA, 'tree': {'sha': TREE_SHA}},
                    f'git/trees/{TREE_SHA}?recursive=1': tree,
                    'actions/artifacts/30': artifact, 'actions/artifacts/30/zip': data}[path]
        with patch.object(inputs, 'api', side_effect=api):
            admitted = inputs.admit(10, 20, PRODUCER_SHA, 30)
            self.assertFalse(admitted['release_qualified'])
            self.assertEqual(admitted['admission']['producer_required_jobs'], {'unit': 21, 'capture': 22})
            for target, field, value in [(producer_run, 'conclusion', 'failure'),
                                          (producer_run, 'event', 'pull_request'),
                                          (producer_run, 'head_sha', 'd' * 40),
                                          (source_run, 'status', 'in_progress'),
                                          (source_run, 'conclusion', 'cancelled'),
                                          (source_run, 'event', 'pull_request'),
                                          (source_jobs[1], 'conclusion', 'skipped'),
                                          (source_jobs[0], 'head_sha', 'd' * 40),
                                          (producer_jobs[1]['steps'][0], 'conclusion', 'skipped')]:
                with self.subTest(field=field, value=value):
                    old = target[field]; target[field] = value
                    with self.assertRaises(ValueError):
                        inputs.admit(10, 20, PRODUCER_SHA, 30)
                    target[field] = old
            source_run['head_repository']['full_name'] = 'other/fork'
            with self.assertRaises(ValueError):
                inputs.admit(10, 20, PRODUCER_SHA, 30)

    def test_manifest_and_identity_negative_controls(self):
        for change in [lambda r, f: r.update(release_qualified=True),
                       lambda r, f: r['source'].update(tree_sha='d' * 40),
                       lambda r, f: r['producer'].update(run_attempt=1),
                       lambda r, f: r['artifacts']['Cargo.lock'].update(sha256='0' * 64),
                       lambda r, f: f.pop('cargo.txt'),
                       lambda r, f: f.update(extra=b'foreign'),
                       lambda r, f: r.update(commands=['cargo metadata'])]:
            with self.subTest(change=change), self.assertRaises((ValueError, KeyError)):
                self.verify(change)

    def test_artifact_metadata_negative_controls(self):
        for field, value in [('digest', 'sha256:' + '0' * 64), ('size_in_bytes', 1),
                             ('expired', True), ('name', 'other')]:
            data, artifact, tree = bundle()
            artifact[field] = value
            with self.subTest(field=field), self.assertRaises(ValueError):
                inputs.verify_bundle(data, artifact, SOURCE, 20, PRODUCER_SHA, 2, tree)

    def test_source_archive_duplicate_extra_missing_and_changed_rejected(self):
        for transform in [lambda entries: entries * 2, lambda entries: [],
                          lambda entries: entries + [('tuna-desktop-' + SOURCE_SHA + '/extra', b'x', 'file')],
                          lambda entries: [(entries[0][0], b'changed', 'file')],
                          lambda entries: entries + [('../escape', b'x', 'file')],
                          lambda entries: entries + [('tuna-desktop-' + SOURCE_SHA + '/link', b'../../escape', 'symlink')]]:
            with self.subTest(transform=transform), self.assertRaises(ValueError):
                self.verify(tar_change=transform)

    def test_caller_cannot_run_capture_outside_ci(self):
        with patch.dict('os.environ', {}, clear=True), self.assertRaises(ValueError):
            inputs.capture(10, '/unused')

    def test_actual_capture_command_sequence_retains_emitted_config(self):
        data, _, tree = bundle()
        with zipfile.ZipFile(io.BytesIO(data)) as archive:
            source_archive = archive.read('tuna-desktop-source.tar.gz')
            vendor_archive = archive.read('tuna-desktop-vendor.tar.gz')
        emitted = b'[source.crates-io]\nreplace-with="real-emitted-config"\n'
        calls = []

        def command(args, cwd, log=None):
            calls.append(args)
            if log:
                Path(log).write_bytes(b'actual fixture command evidence')
            if args[:2] == ['git', 'checkout']:
                (cwd / 'Cargo.lock').write_bytes(b'version = 4\n')
            if args[:2] == ['git', 'rev-parse']:
                return (TREE_SHA + '\n').encode()
            if args[:2] == ['cargo', 'vendor']:
                return emitted
            if args[:2] == ['cargo', 'metadata']:
                self.assertEqual((cwd / '.cargo/config.toml').read_bytes(), emitted)
                return b'{"packages":[{"name":"fixture"}]}'
            if args[0] == 'tar':
                import gzip
                return gzip.decompress(vendor_archive)
            return b''
        producer = run(20, PRODUCER_SHA, inputs.WORKFLOW)
        response = io.BytesIO(source_archive)
        with tempfile.TemporaryDirectory() as temp, patch.dict('os.environ', {
                'GITHUB_ACTIONS': 'true', 'GITHUB_REPOSITORY': inputs.REPO,
                'GITHUB_RUN_ID': '20', 'GITHUB_RUN_ATTEMPT': '2', 'GITHUB_SHA': PRODUCER_SHA}), \
                patch.object(inputs, 'source_gate', return_value=SOURCE), \
                patch.object(inputs, 'api', side_effect=lambda path: producer if path == 'actions/runs/20' else tree), \
                patch.object(inputs, 'command', side_effect=command), \
                patch.object(inputs.urllib.request, 'urlopen', return_value=response):
            out = Path(temp) / 'inputs'
            inputs.capture(10, out)
            self.assertEqual((out / 'vendor-config.toml').read_bytes(), emitted)
            receipt = json.loads((out / 'receipt.json').read_text())
            self.assertEqual(receipt['source']['overall_conclusion'], 'failure')
            self.assertIn(['cargo', 'vendor', '--locked', '--versioned-dirs', 'vendor'], calls)
            self.assertIn(['cargo', 'metadata', '--locked', '--offline', '--format-version', '1'], calls)

    def test_empty_metadata_and_vendor_config_mismatch_not_admitted_even_rehashed(self):
        def empty(receipt, files):
            files['cargo-metadata.json'] = b'{"packages":[]}'
            receipt['artifacts']['cargo-metadata.json'] = {'sha256': inputs.sha256(files['cargo-metadata.json']),
                                                          'size': len(files['cargo-metadata.json'])}
        def mismatch(receipt, files):
            files['tuna-desktop-vendor.tar.gz'] = tar([('.cargo/config.toml', b'wrong', 'file'),
                                                      ('vendor/fixture/Cargo.toml', b'x', 'file')])
            blob = files['tuna-desktop-vendor.tar.gz']
            receipt['artifacts']['tuna-desktop-vendor.tar.gz'] = {'sha256': inputs.sha256(blob), 'size': len(blob)}
        for change in (empty, mismatch):
            with self.subTest(change=change), self.assertRaises(ValueError):
                self.verify(change)

    def test_duplicate_zip_members_and_truncated_tree_rejected(self):
        data, artifact, tree = bundle()
        tree['truncated'] = True
        with self.assertRaises(ValueError):
            inputs.verify_bundle(data, artifact, SOURCE, 20, PRODUCER_SHA, 2, tree)
        tree['truncated'] = False
        buffer = io.BytesIO(data)
        import warnings
        with warnings.catch_warnings():
            warnings.simplefilter('ignore', UserWarning)
            with zipfile.ZipFile(buffer, 'a') as archive:
                archive.writestr('Cargo.lock', b'wrong')
        data = buffer.getvalue()
        artifact.update(digest='sha256:' + inputs.sha256(data), size_in_bytes=len(data))
        with self.assertRaises(ValueError):
            inputs.verify_bundle(data, artifact, SOURCE, 20, PRODUCER_SHA, 2, tree)

    def test_workflow_is_read_only_and_native_capture_is_dispatch_only(self):
        text = (Path(__file__).resolve().parents[2] / '.github/workflows/tuna-source-inputs.yml').read_text()
        self.assertIn("if: github.event_name == 'workflow_dispatch'", text)
        self.assertIn('needs: unit', text)
        self.assertIn('contents: read', text)
        self.assertIn('actions: read', text)
        for forbidden in ['contents: write', 'secrets.', 'rclone', 'gh release', 'cargo build']:
            self.assertNotIn(forbidden, text)


if __name__ == '__main__':
    unittest.main()
