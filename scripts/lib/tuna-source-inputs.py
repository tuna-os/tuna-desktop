"""Produce and admit exact-source offline inputs, independently of release readiness."""
import argparse
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import tarfile
import tempfile
from pathlib import PurePosixPath
import urllib.request
import zipfile

REPO = 'tuna-os/tuna-desktop'
WORKFLOW = '.github/workflows/tuna-source-inputs.yml'
SHA = re.compile(r'[0-9a-f]{40}\Z')
LIMIT = 512 * 1024 * 1024
FILES = {'tuna-desktop-source.tar.gz', 'tuna-desktop-vendor.tar.gz', 'Cargo.lock',
         'vendor-config.toml', 'cargo-metadata.json', 'vendor.log', 'metadata.log', 'rustc.txt', 'cargo.txt'}


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def api(path, raw=False):
    result = subprocess.run(['gh', 'api', f'repos/{REPO}/{path}'], check=True, capture_output=True)
    require(len(result.stdout) <= LIMIT, 'API response exceeds budget')
    return result.stdout if raw else json.loads(result.stdout)


def jobs(run_id, attempt):
    result = []
    for page in range(1, 21):
        value = api(f'actions/runs/{run_id}/attempts/{attempt}/jobs?per_page=100&page={page}')
        result.extend(value['jobs'])
        if len(result) == value['total_count']:
            return result
        require(len(result) < value['total_count'] and value['jobs'], 'inconsistent job pagination')
    raise ValueError('job page budget exceeded')


def run_identity(run, run_id, sha, workflow, completed):
    require(type(run_id) is int and run_id > 0 and run['id'] == run_id, 'wrong run ID')
    require(run['repository']['full_name'] == REPO and run['head_repository']['full_name'] == REPO,
            'foreign source repository')
    require(SHA.fullmatch(sha) and run['head_sha'] == sha, 'wrong run source')
    require(run['path'] == workflow and run['event'] in {'push', 'pull_request', 'workflow_dispatch'},
            'wrong workflow/event')
    require(type(run['run_attempt']) is int and run['run_attempt'] > 0, 'wrong run attempt')
    if completed:
        require(run['status'] == 'completed', 'run is not terminal')


def successful_job(values, name, sha, steps=()):
    found = [job for job in values if job['name'] == name]
    require(len(found) == 1, 'missing or ambiguous required job: ' + name)
    job = found[0]
    require(job['head_sha'] == sha and job['status'] == 'completed' and job['conclusion'] == 'success',
            'required job failed: ' + name)
    for step in steps:
        matches = [s for s in job['steps'] if s['name'] == step]
        require(len(matches) == 1 and matches[0]['status'] == 'completed'
                and matches[0]['conclusion'] == 'success', 'required producer step failed: ' + step)
    return job['id']


def source_gate(run_id, expected_sha=None):
    run = api(f'actions/runs/{run_id}')
    sha = expected_sha or run['head_sha']
    run_identity(run, run_id, sha, '.github/workflows/ci.yml', True)
    # Default PR checkout compiles a synthetic merge commit, not head_sha.
    # Such runs need independent checkout-tree receipts before they can qualify inputs.
    require(run['event'] in {'push', 'workflow_dispatch'}, 'PR merge checkout is not exact-source evidence')
    require(run['conclusion'] in {'success', 'failure'}, 'source run cancelled or unqualified')
    values = jobs(run_id, run['run_attempt'])
    gates = {name: successful_job(values, name, sha) for name in ('check', 'arch-package')}
    commit = api(f'git/commits/{sha}')
    require(commit['sha'] == sha and SHA.fullmatch(commit['tree']['sha']), 'source tree identity missing')
    return {'run_id': run_id, 'run_attempt': run['run_attempt'], 'event': run['event'],
            'source_sha': sha, 'tree_sha': commit['tree']['sha'],
            'overall_conclusion': run['conclusion'], 'required_jobs': gates}


def archive_members(data, roots):
    require(len(data) <= LIMIT, 'archive budget exceeded')
    result = {}
    total = 0
    with tarfile.open(fileobj=io.BytesIO(data)) as archive:
        for member in archive:
            require(len(result) < 200000, 'archive member budget exceeded')
            path = PurePosixPath(member.name)
            require(not path.is_absolute() and '..' not in path.parts and path.parts
                    and path.parts[0] in roots and member.name not in result, 'unsafe/duplicate archive member')
            require(member.isdir() or member.isfile() or member.issym(), 'unsupported archive member')
            total += member.size
            require(total <= LIMIT, 'expanded archive budget exceeded')
            if member.issym():
                target = PurePosixPath(member.linkname)
                # No source symlink may escape its archive root.
                depth = len(path.parts) - 1
                require(not target.is_absolute(), 'absolute archive symlink')
                for part in target.parts:
                    depth += -1 if part == '..' else (0 if part == '.' else 1)
                    require(depth >= 1, 'escaping archive symlink')
            value = archive.extractfile(member).read() if member.isfile() else (
                member.linkname.encode() if member.issym() else b'')
            result[member.name] = (member, value)
    return result


def verify_source_archive(data, source, tree):
    require(tree['sha'] == source['tree_sha'] and tree.get('truncated') is False, 'wrong/incomplete source tree')
    prefix = 'tuna-desktop-' + source['source_sha']
    members = archive_members(data, {prefix})
    expected = {prefix + '/' + e['path']: e for e in tree['tree'] if e['type'] == 'blob'}
    expected_dirs = {prefix} | {prefix + '/' + e['path'] for e in tree['tree'] if e['type'] == 'tree'}
    require(all(name.rstrip('/') in expected_dirs for name, (member, _) in members.items() if member.isdir()),
            'foreign source archive directory')
    require(all(e['type'] in {'blob', 'tree'} for e in tree['tree']), 'source submodule requires explicit admission')
    actual = {name: pair for name, pair in members.items() if not pair[0].isdir()}
    require(actual.keys() == expected.keys(), 'source archive does not match exact tree')
    for name, (member, data) in actual.items():
        entry = expected[name]
        require((member.issym() and entry['mode'] == '120000') or
                (member.isfile() and entry['mode'] in {'100644', '100755'}), 'source member mode mismatch')
        if member.isfile():
            require(bool(member.mode & 0o111) == (entry['mode'] == '100755'), 'source execute mode mismatch')
        digest = hashlib.sha1(b'blob ' + str(len(data)).encode() + b'\0' + data).hexdigest()
        require(digest == entry['sha'], 'source member differs from Git blob')
    require(prefix + '/Cargo.lock' in actual, 'source lock missing')
    return actual[prefix + '/Cargo.lock'][1]


def command(args, cwd, log=None):
    result = subprocess.run(args, cwd=cwd, check=True, capture_output=True)
    if log:
        Path(log).write_bytes(result.stderr)
    return result.stdout


def capture(source_run_id, output):
    require(os.environ.get('GITHUB_ACTIONS') == 'true' and os.environ.get('GITHUB_REPOSITORY') == REPO,
            'capture requires owned GitHub CI')
    producer_id = int(os.environ['GITHUB_RUN_ID'])
    producer_sha = os.environ['GITHUB_SHA']
    producer = api(f'actions/runs/{producer_id}')
    run_identity(producer, producer_id, producer_sha, WORKFLOW, False)
    require(producer['event'] == 'workflow_dispatch' and producer['run_attempt'] == int(os.environ['GITHUB_RUN_ATTEMPT']),
            'capture requires exact dispatch attempt')
    source = source_gate(source_run_id)
    out = Path(output).resolve()
    require(not out.exists(), 'fresh output required')
    out.mkdir(parents=True)
    with tempfile.TemporaryDirectory(prefix='tuna-source-') as temp:
        root = Path(temp)
        command(['git', 'init', '-q'], root)
        command(['git', 'fetch', '--depth=1', f'https://github.com/{REPO}.git', source['source_sha']], root)
        command(['git', 'checkout', '--detach', 'FETCH_HEAD'], root)
        require(command(['git', 'rev-parse', 'HEAD^{tree}'], root).decode().strip() == source['tree_sha'],
                'fetched source tree mismatch')
        require(not command(['git', 'status', '--porcelain'], root), 'fetched source dirty')
        url = f'https://github.com/{REPO}/archive/{source["source_sha"]}.tar.gz'
        with urllib.request.urlopen(url, timeout=120) as response:
            archive = response.read(LIMIT + 1)
        tree = api(f'git/trees/{source["tree_sha"]}?recursive=1')
        archive_lock = verify_source_archive(archive, source, tree)
        (out / 'tuna-desktop-source.tar.gz').write_bytes(archive)
        lock = (root / 'Cargo.lock').read_bytes()
        require(lock == archive_lock, 'fetched/archive Cargo.lock mismatch')
        (out / 'Cargo.lock').write_bytes(lock)
        (out / 'rustc.txt').write_bytes(command(['rustc', '-vV'], root))
        (out / 'cargo.txt').write_bytes(command(['cargo', '--version'], root))
        config = command(['cargo', 'vendor', '--locked', '--versioned-dirs', 'vendor'], root, out / 'vendor.log')
        (root / '.cargo').mkdir(exist_ok=True)
        (root / '.cargo/config.toml').write_bytes(config)
        (out / 'vendor-config.toml').write_bytes(config)
        # Use exactly the emitted replacement config; no hand-written approximation.
        metadata = command(['cargo', 'metadata', '--locked', '--offline', '--format-version', '1'], root,
                           out / 'metadata.log')
        require(json.loads(metadata).get('packages'), 'offline metadata returned no packages')
        require((root / 'Cargo.lock').read_bytes() == lock, 'vendor/metadata changed Cargo.lock')
        (out / 'cargo-metadata.json').write_bytes(metadata)
        # Stable tar ownership/time/order; gzip header has no source timestamp.
        tar = command(['tar', '--sort=name', '--mtime=@0', '--owner=0', '--group=0', '--numeric-owner',
                       '-cf', '-', 'vendor', '.cargo'], root)
        require(len(tar) <= LIMIT, 'vendor archive exceeds budget')
        import gzip
        (out / 'tuna-desktop-vendor.tar.gz').write_bytes(gzip.compress(tar, mtime=0))
    artifacts = {name: {'sha256': sha256((out / name).read_bytes()), 'size': (out / name).stat().st_size}
                 for name in sorted(FILES)}
    receipt = {'schema': 1, 'candidate_only': True, 'release_qualified': False, 'published': False,
               'source': source, 'producer': {'run_id': producer_id, 'run_attempt': producer['run_attempt'],
                                             'source_sha': producer_sha, 'workflow': WORKFLOW},
               'lock_sha256': artifacts['Cargo.lock']['sha256'], 'artifacts': artifacts,
               'commands': ['cargo vendor --locked --versioned-dirs vendor',
                            'cargo metadata --locked --offline --format-version 1']}
    (out / 'receipt.json').write_text(json.dumps(receipt, indent=2) + '\n')


def verify_bundle(data, artifact, source, producer_id, producer_sha, attempt, source_tree):
    require(not artifact['expired'] and artifact['size_in_bytes'] == len(data) and len(data) <= LIMIT,
            'artifact unavailable or size mismatch')
    require(artifact.get('digest') == 'sha256:' + sha256(data), 'artifact API digest mismatch')
    expected_name = f'tuna-source-inputs-{producer_id}-{attempt}'
    require(artifact['name'] == expected_name and artifact['workflow_run']['id'] == producer_id
            and artifact['workflow_run']['head_sha'] == producer_sha, 'wrong artifact owner')
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        entries = archive.infolist()
        require(len(entries) == len(FILES) + 1 and {e.filename for e in entries} == FILES | {'receipt.json'},
                'foreign, duplicate or missing artifact entry')
        require(sum(e.file_size for e in entries) <= LIMIT and all(not e.is_dir() for e in entries),
                'expanded artifact budget exceeded')
        blobs = {e.filename: archive.read(e) for e in entries}
    receipt = json.loads(blobs['receipt.json'])
    require(receipt['schema'] == 1 and receipt['candidate_only'] is True
            and receipt['release_qualified'] is False and receipt['published'] is False, 'wrong readiness')
    require(receipt['source'] == source and receipt['producer'] == {
        'run_id': producer_id, 'run_attempt': attempt, 'source_sha': producer_sha, 'workflow': WORKFLOW},
        'receipt provenance mismatch')
    require(set(receipt['artifacts']) == FILES, 'manifest scope mismatch')
    for name in FILES:
        require(receipt['artifacts'][name] == {'sha256': sha256(blobs[name]), 'size': len(blobs[name])},
                'member digest mismatch: ' + name)
    require(receipt['lock_sha256'] == sha256(blobs['Cargo.lock']), 'lock digest mismatch')
    require(json.loads(blobs['cargo-metadata.json']).get('packages'), 'empty offline metadata')
    require(verify_source_archive(blobs['tuna-desktop-source.tar.gz'], source, source_tree) == blobs['Cargo.lock'],
            'archive lock mismatch')
    vendor = archive_members(blobs['tuna-desktop-vendor.tar.gz'], {'vendor', '.cargo'})
    require('.cargo/config.toml' in vendor and vendor['.cargo/config.toml'][0].isfile()
            and vendor['.cargo/config.toml'][1] == blobs['vendor-config.toml'], 'vendor config mismatch')
    require(any(name.startswith('vendor/') and member.isfile() for name, (member, _) in vendor.items()),
            'vendor crate payload missing')
    require(receipt['commands'] == ['cargo vendor --locked --versioned-dirs vendor',
                                  'cargo metadata --locked --offline --format-version 1'], 'wrong offline command receipt')
    return {**receipt, 'source_sha': source['source_sha'], 'source_archive_sha256': sha256(blobs['tuna-desktop-source.tar.gz']),
            'vendor_archive_sha256': sha256(blobs['tuna-desktop-vendor.tar.gz']),
            'vendor_receipt': {'source_sha': source['source_sha'], 'lock_sha256': receipt['lock_sha256'],
                               'archive_sha256': sha256(blobs['tuna-desktop-vendor.tar.gz']),
                               'offline_metadata_success': True, 'run_id': producer_id},
            'admission': {'artifact_id': artifact['id'], 'artifact_sha256': sha256(data),
                                    'artifact_size': len(data), 'producer_terminal_success': True}}


def admit(source_run_id, producer_run_id, producer_sha, artifact_id):
    source = source_gate(source_run_id)
    producer = api(f'actions/runs/{producer_run_id}')
    run_identity(producer, producer_run_id, producer_sha, WORKFLOW, True)
    require(producer['event'] == 'workflow_dispatch' and producer['conclusion'] == 'success',
            'producer did not succeed')
    attempt = producer['run_attempt']
    values = jobs(producer_run_id, attempt)
    producer_jobs = {'unit': successful_job(values, 'unit', producer_sha),
                     'capture': successful_job(values, 'capture', producer_sha,
                                               ('Capture exact offline inputs', 'Upload exact inputs'))}
    artifact = api(f'actions/artifacts/{artifact_id}')
    require(not artifact['expired'] and 0 < artifact['size_in_bytes'] <= LIMIT,
            'artifact exceeds download budget or expired')
    data = api(f'actions/artifacts/{artifact_id}/zip', raw=True)
    tree = api(f'git/trees/{source["tree_sha"]}?recursive=1')
    receipt = verify_bundle(data, artifact, source, producer_run_id, producer_sha, attempt, tree)
    receipt['admission'].update(producer_event=producer['event'], producer_conclusion=producer['conclusion'],
                                producer_required_jobs=producer_jobs)
    return receipt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    subs = parser.add_subparsers(dest='command', required=True)
    capture_args = subs.add_parser('capture')
    capture_args.add_argument('--source-run', required=True, type=int)
    capture_args.add_argument('--output', required=True)
    admission = subs.add_parser('admit')
    admission.add_argument('--source-run', required=True, type=int)
    admission.add_argument('--producer-run', required=True, type=int)
    admission.add_argument('--producer-sha', required=True)
    admission.add_argument('--artifact', required=True, type=int)
    admission.add_argument('--output', required=True)
    args = parser.parse_args()
    if args.command == 'capture':
        capture(args.source_run, args.output)
    else:
        result = admit(args.source_run, args.producer_run, args.producer_sha, args.artifact)
        with Path(args.output).open('x') as stream:
            stream.write(json.dumps(result, indent=2) + '\n')


if __name__ == '__main__':
    main()
