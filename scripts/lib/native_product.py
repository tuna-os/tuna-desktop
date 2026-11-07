"""Authenticated, source-equivalent native CI product capsules (not releases).

The outer Actions ZIP is authenticated before inspecting any member. The OCI
archive remains opaque here; only ordinary native CI's literal Podman load
interprets it. Retained verification JSON is provenance, not a new signature
verification or a claim of GUI qualification.
"""
import argparse
import base64
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import subprocess
import tempfile
import zipfile

from maintained_media import required_source_inputs
from vm_gnome_reference import validate_platform_verification

REPOSITORY = 'tuna-os/tuna-desktop'
ORIGIN = 'https://github.com/' + REPOSITORY + '.git'
WORKFLOW_PATH = '.github/workflows/ci.yml'
PROFILES = ('distribution', 'upstream51')
IMAGE_TAG = 'localhost/marlin-tuna-vm:ci'
SCHEMA = 'tuna-native-product-v1'
SELECTION_SCHEMA = 'tuna-native-product-selection-v1'
MAX_RUNS = 30
CHUNK = 1024 * 1024
MAX_METADATA = 16 * CHUNK
MAX_POINTER = 2 * CHUNK
MAX_IMAGE = 24 * 1024 * CHUNK
MAX_PACKAGE = 2 * 1024 * CHUNK
MAX_ZIP = MAX_IMAGE + MAX_PACKAGE + 256 * CHUNK
MAX_MEMBERS = 128
BASE_FILES = ('index.json', 'platform-cosign.json', 'platform-verification.json')
GDM_FILES = ('manifest.json', 'build-provenance.json', 'install-provenance.json',
             'installed-packages.txt', 'package-info.txt', 'prior-image-id',
             'final-image-id', 'final-image-inspect.json')
ORCA_FILES = ('base-atspi.txt', 'base-orca.txt', 'coherent-atspi.txt',
              'package-sha256.txt', 'installed-packages.txt', 'launcher-sha256.txt',
              'source-sha256.txt', 'upstream-tag.txt', 'build-runtime-packages.txt')


def require(condition, message):
    if not condition:
        raise ValueError(message)


def positive(value):
    return type(value) is int and value > 0


def hex_value(value, length):
    return isinstance(value, str) and re.fullmatch('[0-9a-f]{' + str(length) + '}', value) is not None


def image_digest(value):
    return isinstance(value, str) and re.fullmatch(r'sha256:[0-9a-f]{64}', value) is not None


def artifact_name(profile, data=False):
    require(profile in PROFILES, 'unsupported native product profile')
    return 'native-product-' + ('DATA-' if data else '') + profile


def package_basename(value):
    require(isinstance(value, str) and re.fullmatch(
        r'tuna-desktop-[0-9][A-Za-z0-9.+_~:-]*-x86_64\.pkg\.tar\.zst', value) is not None,
        'original native package basename is unsafe or not an x86_64 tuna package')
    return value


def json_object(raw):
    def unique(pairs):
        result = {}
        for key, value in pairs:
            require(key not in result, 'duplicate JSON key: ' + key)
            result[key] = value
        return result
    result = json.loads(raw, object_pairs_hook=unique)
    require(isinstance(result, dict), 'expected JSON object')
    return result


def write_json(path, value):
    path = Path(path)
    require(not path.is_symlink(), 'refusing JSON symlink destination')
    path.write_text(json.dumps(value, sort_keys=True, indent=2) + '\n')
    path.chmod(0o600)


def file_identity(path, bound=MAX_IMAGE):
    """Hash incrementally through a regular, non-symlink descriptor."""
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        before = os.fstat(stream.fileno())
        require(stat.S_ISREG(before.st_mode) and 0 < before.st_size <= bound,
                'file is empty, oversized or not regular: ' + str(path))
        checksum = hashlib.sha256()
        for chunk in iter(lambda: stream.read(CHUNK), b''):
            checksum.update(chunk)
        after = os.fstat(stream.fileno())
        require((before.st_dev, before.st_ino, before.st_size, before.st_mtime_ns, before.st_ctime_ns)
                == (after.st_dev, after.st_ino, after.st_size, after.st_mtime_ns, after.st_ctime_ns),
                'file changed during hashing')
        return {'sha256': checksum.hexdigest(), 'bytes': before.st_size}


def command(root, *args):
    return subprocess.check_output(list(map(str, args)), cwd=root, timeout=120,
                                   stderr=subprocess.PIPE).decode().strip()


def git(root, *args):
    return command(root, 'git', *args)


def validate_origin(root):
    require(git(root, 'remote', 'get-url', '--all', 'origin') in
            (ORIGIN, ORIGIN.removesuffix('.git')), 'checkout origin is not the fixed trusted repository')


def source_identity(root, ref='HEAD'):
    validate_origin(root)
    if ref != 'HEAD':
        require(hex_value(ref, 40), 'source reference must be a full commit SHA')
        present = subprocess.run(['git', 'cat-file', '-e', ref + '^{commit}'], cwd=root,
                                 stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode == 0
        if not present:
            # Public fixed origin, no personal credential helper or interactive fallback.
            env = dict(os.environ, GIT_TERMINAL_PROMPT='0', GIT_CONFIG_GLOBAL='/dev/null',
                       GIT_CONFIG_NOSYSTEM='1', GIT_ASKPASS='/bin/false', SSH_ASKPASS='/bin/false')
            fetched = subprocess.run(['git', '-c', 'credential.helper=', '-c', 'http.extraHeader=',
                                      'fetch', '--no-tags', '--depth=1', ORIGIN, ref], cwd=root,
                                     env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=180)
            require(fetched.returncode == 0, 'cannot fetch exact trusted source commit')
    commit = git(root, 'rev-parse', '--verify', ref + '^{commit}')
    tree = git(root, 'rev-parse', '--verify', ref + '^{tree}')
    require(hex_value(commit, 40) and hex_value(tree, 40) and (ref == 'HEAD' or commit == ref),
            'actual Git commit/tree does not match strict source identity')
    value = required_source_inputs(root, ref=ref)
    require(hex_value(value, 64), 'invalid actual required source inputs hash')
    return {'commit': commit, 'tree': tree, 'required_source_inputs_sha256': value}


def validate_source(value):
    require(isinstance(value, dict) and set(value) ==
            {'commit', 'tree', 'required_source_inputs_sha256'} and
            hex_value(value.get('commit'), 40) and hex_value(value.get('tree'), 40) and
            hex_value(value.get('required_source_inputs_sha256'), 64), 'invalid source identity')


def metadata_names(profile):
    return ({'native-base/' + name for name in BASE_FILES} |
            {'gdm51/' + name for name in GDM_FILES} |
            ({'orca51-package-provenance/' + name for name in ORCA_FILES}
             if profile == 'upstream51' else set()))


def allowed_file(name, profile):
    if name in {'product-image.oci.tar', 'tuna.pkg.tar.zst', 'product-provenance.json', 'strip-image.json'} | metadata_names(profile):
        return True
    return profile == 'upstream51' and re.fullmatch(
        r'orca51-package-provenance/(?:orca|at-spi2-core)-[A-Za-z0-9.+_~:-]+-(?:x86_64|any)\.pkg\.tar\.zst\.(?:buildinfo|pkginfo)',
        name) is not None


def member_bound(name):
    return MAX_IMAGE if name == 'product-image.oci.tar' else MAX_PACKAGE if name == 'tuna.pkg.tar.zst' else MAX_METADATA


def extract_zip(archive, target, profile, pointer=False):
    """Validate ALL paths/types/bounds/aliases before creating a destination."""
    target = Path(target)
    require(not target.exists(), 'refusing existing artifact extraction destination')
    require(file_identity(archive, MAX_POINTER if pointer else MAX_ZIP)['bytes'] > 0, 'empty ZIP')
    with zipfile.ZipFile(archive) as source:
        members = source.infolist()
        require(0 < len(members) <= MAX_MEMBERS, 'artifact ZIP member count exceeds bound')
        seen, regular, total = set(), [], 0
        for member in members:
            name = member.filename
            path = PurePosixPath(name)
            canonical = name[:-1] if member.is_dir() else name
            require(name and member.orig_filename == name and '\\' not in name and '\x00' not in name and not path.is_absolute()
                    and str(path) == canonical and all(p not in ('', '.', '..') for p in path.parts)
                    and ':' not in name and canonical not in seen, 'unsafe or aliased ZIP member path')
            seen.add(canonical)
            mode = member.external_attr >> 16
            kind = stat.S_IFMT(mode)
            require(not (member.flag_bits & 1) and
                    (kind in (0, stat.S_IFDIR) if member.is_dir() else kind in (0, stat.S_IFREG)),
                    'encrypted, symlink or nonregular ZIP member')
            require(not (mode & 0o111) or member.is_dir(), 'executable artifact member is forbidden')
            if member.is_dir():
                require(not pointer and canonical in ('native-base', 'gdm51', 'orca51-package-provenance'),
                        'unexpected artifact directory')
                continue
            require(name == 'product-identity.json' if pointer else allowed_file(name, profile),
                    'unexpected artifact member: ' + name)
            bound = MAX_POINTER if pointer else member_bound(name)
            require(0 < member.file_size <= bound, 'artifact member exceeds finite bound')
            total += member.file_size
            require(total <= (MAX_POINTER if pointer else MAX_ZIP), 'expanded artifact exceeds finite bound')
            regular.append(member)
        require(not pointer or len(regular) == 1, 'pointer must contain exactly one identity file')
        # Parent/file aliases are rejected independently of entry ordering.
        files = {m.filename for m in regular}
        require(all(not any(str(p) in files for p in PurePosixPath(n).parents if str(p) != '.')
                    for n in files), 'ZIP file/directory collision')
        target.mkdir(parents=True, mode=0o700)
        for member in regular:
            output = target / member.filename
            output.parent.mkdir(parents=True, exist_ok=True)
            with source.open(member) as stream, output.open('xb') as dest:
                copied = 0
                while chunk := stream.read(CHUNK):
                    copied += len(chunk)
                    require(copied <= member.file_size, 'ZIP member exceeded declared size')
                    dest.write(chunk)
                require(copied == member.file_size, 'truncated ZIP member')
            output.chmod(0o600)


class ArtifactUnavailable(ValueError):
    """Only an explicit API 404/410 means missing or expired, never auth failure."""


class GitHub:
    """Only the supplied Actions token; never gh's saved personal credentials."""
    def __init__(self):
        require(bool(os.environ.get('GH_TOKEN')), 'GH_TOKEN from the native Actions job is required')
        self.env = dict(os.environ, GH_HOST='github.com', GH_PROMPT_DISABLED='1')

    def api(self, path):
        response = subprocess.run(['gh', 'api', '--hostname', 'github.com',
                                   'repos/' + REPOSITORY + '/' + path], env=self.env, timeout=120,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        if response.returncode:
            if re.search(rb'\(HTTP (?:404|410)\)', response.stderr):
                raise ArtifactUnavailable('GitHub object unavailable or expired')
            raise ValueError('GitHub API authentication/transport/request failed')
        require(len(response.stdout) <= 8 * CHUNK, 'GitHub JSON response exceeds bound')
        return json_object(response.stdout)

    def download(self, artifact, destination, bound):
        # gh follows the API redirect without copying the ZIP into Python memory.
        with Path(destination).open('xb') as output, tempfile.TemporaryFile() as errors:
            process = subprocess.Popen(['gh', 'api', '--hostname', 'github.com',
                                        'repos/' + REPOSITORY + '/actions/artifacts/' + str(artifact['id']) + '/zip'],
                                       env=self.env, stdout=subprocess.PIPE, stderr=errors)
            try:
                total = 0
                while chunk := process.stdout.read(CHUNK):
                    total += len(chunk)
                    require(total <= bound, 'download exceeds finite artifact bound')
                    output.write(chunk)
                if process.wait(timeout=120) != 0:
                    errors.seek(0)
                    if re.search(rb'\(HTTP (?:404|410)\)', errors.read(MAX_METADATA)):
                        raise ArtifactUnavailable('artifact unavailable or expired')
                    raise ValueError('artifact download/authentication/transport failed')
            finally:
                process.stdout.close()
                if process.poll() is None:
                    process.kill()
                    process.wait()
        actual = file_identity(destination, bound)
        require('sha256:' + actual['sha256'] == artifact['digest'], 'API artifact ZIP digest mismatch')
        require(actual['bytes'] == artifact['size_in_bytes'], 'API artifact ZIP byte count mismatch')


def workflow_identity(api):
    workflow = api.api('actions/workflows/ci.yml')
    require(positive(workflow.get('id')) and workflow.get('name') == 'ci'
            and workflow.get('path') == WORKFLOW_PATH and workflow.get('state') == 'active',
            'unexpected trusted workflow identity')
    return workflow


def inspect_run(run, workflow, run_id=None, attempt=None):
    require(positive(run.get('id')) and positive(run.get('run_attempt'))
            and (run_id is None or run['id'] == run_id)
            and (attempt is None or run['run_attempt'] == attempt)
            and run.get('workflow_id') == workflow['id'] and run.get('path') == WORKFLOW_PATH
            and run.get('name') == 'ci' and run.get('event') == 'push'
            and run.get('head_branch') == 'main' and hex_value(run.get('head_sha'), 40)
            and run.get('status') == 'completed' and run.get('conclusion') == 'success'
            and run.get('repository', {}).get('full_name') == REPOSITORY
            and run.get('head_repository', {}).get('full_name') == REPOSITORY
            and positive(run['repository'].get('id')) and positive(run['head_repository'].get('id'))
            and run['repository']['id'] == run['head_repository']['id'],
            'run is not the exact successful trusted Main workflow/attempt')


def inspect_artifact(artifact, run, name):
    binding = artifact.get('workflow_run', {})
    require(positive(artifact.get('id')) and artifact.get('name') == name
            and image_digest(artifact.get('digest')) and type(artifact.get('size_in_bytes')) is int
            and artifact['size_in_bytes'] > 0 and binding.get('id') == run['id']
            and binding.get('head_sha') == run['head_sha'] and binding.get('head_branch') == 'main'
            and binding.get('repository_id') == run['repository']['id']
            and binding.get('head_repository_id') == run['head_repository']['id'],
            'artifact API identity/digest/run binding differs')


def run_artifacts(api, run):
    result = []
    for page in range(1, 5):
        response = api.api('actions/runs/' + str(run['id']) + '/artifacts?per_page=100&page=' + str(page))
        rows = response.get('artifacts')
        require(isinstance(rows, list) and type(response.get('total_count')) is int
                and 0 <= response['total_count'] <= 400, 'unbounded or invalid run artifact list')
        result.extend(rows)
        if len(result) >= response['total_count']:
            require(len(result) == response['total_count'] and
                    len({a.get('id') for a in result}) == len(result), 'inconsistent artifact pagination')
            return result
    raise ValueError('incomplete bounded artifact list')


def validate_manifest(value, profile):
    require(profile in PROFILES and value.get('schema') == SCHEMA and value.get('profile') == profile
            and image_digest(value.get('image_id')), 'invalid capsule schema/profile/image')
    for key in ('product_source', 'qualification_source'):
        validate_source(value.get(key))
    require(value['product_source']['required_source_inputs_sha256'] ==
            value['qualification_source']['required_source_inputs_sha256'], 'capsule source inputs differ')
    package = value.get('package', {})
    require(package.get('file') == 'tuna.pkg.tar.zst' and hex_value(package.get('sha256'), 64),
            'invalid original package identity')
    package_basename(package.get('basename'))
    run = value.get('qualification_run', {})
    require(positive(run.get('id')) and positive(run.get('attempt')), 'invalid qualification run/attempt')
    files = value.get('files')
    require(isinstance(files, dict) and
            {'product-image.oci.tar', 'tuna.pkg.tar.zst', 'strip-image.json'} | metadata_names(profile) <= set(files),
            'capsule is missing fixed metadata/package/OCI members')
    for name, identity in files.items():
        require(allowed_file(name, profile) and name != 'product-provenance.json'
                and isinstance(identity, dict) and set(identity) == {'sha256', 'bytes'}
                and hex_value(identity.get('sha256'), 64) and positive(identity.get('bytes'))
                and identity['bytes'] <= member_bound(name), 'invalid capsule file identity')
    require(files['tuna.pkg.tar.zst']['sha256'] == package['sha256'], 'package and files hash differ')
    if profile == 'upstream51':
        require(any(n.startswith('orca51-package-provenance/orca-') and n.endswith('.buildinfo') for n in files)
                and any(n.startswith('orca51-package-provenance/orca-') and n.endswith('.pkginfo') for n in files),
                'missing actual upstream Orca package metadata')
    require(image_digest(value.get('base_index')) and image_digest(value.get('base_platform')),
            'capsule is missing authenticated immutable base index/platform')


def validate_base(directory):
    directory = Path(directory)
    for name in BASE_FILES:
        file_identity(directory / name, MAX_METADATA)
    raw = (directory / 'index.json').read_bytes()
    index = json_object(raw)
    require(index.get('schemaVersion') == 2 and isinstance(index.get('manifests'), list), 'invalid actual base index')
    platforms = [m for m in index['manifests'] if m.get('platform', {}).get('os') == 'linux'
                 and m.get('platform', {}).get('architecture') == 'amd64']
    require(len(platforms) == 1 and image_digest(platforms[0].get('digest')), 'ambiguous native base platform')
    platform = platforms[0]['digest']
    validate_platform_verification((directory / 'platform-cosign.json').read_bytes(),
                                   (directory / 'platform-verification.json').read_bytes(), platform)
    return {'platform': platform, 'index_digest': 'sha256:' + hashlib.sha256(raw).hexdigest()}


def validate_data(directory, value):
    validate_manifest(value, value['profile'])
    actual = {p.relative_to(directory).as_posix() for p in Path(directory).rglob('*') if p.is_file()}
    require(actual == set(value['files']) | {'product-provenance.json'}, 'capsule member inventory differs')
    for name, identity in value['files'].items():
        require(file_identity(Path(directory) / name, member_bound(name)) == identity,
                'retained inner file SHA/size differs: ' + name)
    base = validate_base(Path(directory) / 'native-base')
    require(value['base_index'] == base['index_digest'] and value['base_platform'] == base['platform'],
            'authenticated base pair differs from actual retained index/platform bytes')
    receipt = json_object((Path(directory) / 'strip-image.json').read_bytes())
    require(receipt.get('image_id') == value['image_id'] and receipt.get('commit') == value['product_source']['commit']
            and receipt.get('package_sha256') == value['package']['sha256']
            and receipt.get('built_tree') == value['product_source']['tree']
            and receipt.get('required_source_inputs_sha256') == value['product_source']['required_source_inputs_sha256']
            and receipt.get('base_image') == 'ghcr.io/tuna-os/marlin:gnome@' + base['platform'],
            'actual strip receipt differs from product image/package/source/base')
    require((Path(directory) / 'gdm51/final-image-id').read_text().strip() == value['image_id'],
            'final GDM image differs from retained product')
    return base, receipt


def podman(root, *args):
    return command(root, 'sudo', 'podman', *args)


def restore_metadata(root, directory, files):
    for name in files:
        if name.startswith('gdm51/'):
            destination = Path(root) / 'vm-lane-artifacts' / name
        elif name.startswith('orca51-package-provenance/'):
            destination = Path(root) / name
        else:
            continue
        destination.parent.mkdir(parents=True, exist_ok=True)
        require(not destination.is_symlink(), 'refusing restored metadata symlink')
        shutil.copyfile(Path(directory) / name, destination)
        destination.chmod(0o600)


def select(root, profile, out, current_package, *, expected_base_index, expected_base_platform, base_evidence, api=None):
    artifact_name(profile)
    root, out, current_package = Path(root), Path(out).resolve(), Path(current_package).resolve()
    require(not out.exists(), 'selection output must be a new owned directory')
    require(image_digest(expected_base_index) and image_digest(expected_base_platform),
            'current independently verified immutable base index/platform required')
    current_base = validate_base(base_evidence)
    require(current_base == {'platform': expected_base_platform, 'index_digest': expected_base_index},
            'current base evidence differs from independently resolved expected pair')
    current = source_identity(root)
    fresh_package = {'file': str(current_package), 'basename': package_basename(current_package.name),
                     'sha256': file_identity(current_package, MAX_PACKAGE)['sha256']}
    api = api or GitHub()
    workflow = workflow_identity(api)
    response = api.api('actions/workflows/' + str(workflow['id']) +
                       '/runs?branch=main&event=push&status=success&per_page=' + str(MAX_RUNS))
    runs = response.get('workflow_runs')
    require(isinstance(runs, list) and len(runs) <= MAX_RUNS, 'invalid bounded latest workflow lookup')
    reason = 'no eligible retained product in bounded successful Main runs; ordinary fresh build required'
    for listed in runs:
        inspect_run(listed, workflow)
        try:
            run = api.api('actions/runs/' + str(listed['id']))
        except ArtifactUnavailable:
            reason = 'qualified run unavailable; ordinary fresh build required'
            continue
        inspect_run(run, workflow, listed['id'], listed['run_attempt'])
        require(run['head_sha'] == listed['head_sha'], 'run head changed across API lookup')
        try:
            attempt = api.api('actions/runs/' + str(run['id']) + '/attempts/' + str(run['run_attempt']))
        except ArtifactUnavailable:
            reason = 'qualified attempt unavailable; ordinary fresh build required'
            continue
        inspect_run(attempt, workflow, run['id'], run['run_attempt'])
        require(attempt['head_sha'] == run['head_sha'], 'attempt head differs')
        try:
            artifacts = run_artifacts(api, run)
        except ArtifactUnavailable:
            reason = 'qualified run artifacts unavailable; ordinary fresh build required'
            continue
        pointers = [a for a in artifacts if a.get('name') == artifact_name(profile)]
        pointers = [a for a in pointers if not a.get('expired')]
        if not pointers:
            reason = 'no unexpired profile pointer in bounded successful Main runs; ordinary fresh build required'
            continue
        # Retries may retain earlier attempts. Only exact qualification_run can qualify.
        for pointer_artifact in sorted(pointers, key=lambda a: a['id'], reverse=True)[:5]:
            inspect_artifact(pointer_artifact, run, artifact_name(profile))
            require(pointer_artifact['size_in_bytes'] <= MAX_POINTER, 'oversized identity pointer')
            with tempfile.TemporaryDirectory(prefix='tuna-product-pointer-') as scratch:
                scratch = Path(scratch)
                try:
                    api.download(pointer_artifact, scratch / 'pointer.zip', MAX_POINTER)
                except ArtifactUnavailable:
                    reason = 'identity pointer unavailable/expired; ordinary fresh build required'
                    continue
                extract_zip(scratch / 'pointer.zip', scratch / 'pointer', profile, pointer=True)
                pointer = json_object((scratch / 'pointer/product-identity.json').read_bytes())
                require(pointer.get('schema') == SCHEMA and pointer.get('profile') == profile,
                        'identity pointer schema/profile differs')
                validate_source(pointer.get('product_source'))
                if pointer['product_source']['required_source_inputs_sha256'] != current['required_source_inputs_sha256']:
                    reason = 'retained product has different required source inputs; ordinary fresh build required'
                    continue
                require(image_digest(pointer.get('base_index')) and image_digest(pointer.get('base_platform')),
                        'matching identity pointer lacks authenticated immutable base pair')
                if pointer['base_index'] != expected_base_index or pointer['base_platform'] != expected_base_platform:
                    reason = 'published native base index/platform changed; ordinary current fresh build required'
                    continue
                validate_manifest(pointer, profile)
                if pointer['qualification_run'] != {'id': run['id'], 'attempt': run['run_attempt']}:
                    reason = 'retained pointer belongs to an older run attempt; ordinary fresh build required'
                    continue
                require(pointer['qualification_source']['commit'] == run['head_sha'], 'matching pointer qualification head differs')
                original = source_identity(root, pointer['product_source']['commit'])
                qualification = source_identity(root, pointer['qualification_source']['commit'])
                require(original == pointer['product_source'] and qualification == pointer['qualification_source']
                        and original['required_source_inputs_sha256'] == current['required_source_inputs_sha256'],
                        'matching capsule does not equal actual Git source trees/inputs')
                data_binding = pointer.get('data_artifact', {})
                require(positive(data_binding.get('id')) and data_binding.get('name') == artifact_name(profile, True)
                        and image_digest(data_binding.get('digest')), 'invalid data artifact pointer binding')
                matches = [a for a in artifacts if a.get('id') == data_binding['id']]
                if not matches or matches[0].get('expired'):
                    reason = 'original retained data artifact unavailable/expired; ordinary fresh build required'
                    continue
                require(len(matches) == 1, 'duplicate data artifact ID')
                try:
                    data_artifact = api.api('actions/artifacts/' + str(data_binding['id']))
                except ArtifactUnavailable:
                    reason = 'retained data artifact unavailable/expired; ordinary fresh build required'
                    continue
                if data_artifact.get('expired'):
                    reason = 'retained data artifact expired; ordinary fresh build required'
                    continue
                inspect_artifact(data_artifact, run, artifact_name(profile, True))
                require(data_artifact['digest'] == data_binding['digest']
                        and matches[0].get('digest') == data_binding['digest']
                        and data_artifact['size_in_bytes'] <= MAX_ZIP, 'data artifact digest/size binding differs')
                # Stage on the output filesystem; rename, never copy an image-sized archive.
                out.parent.mkdir(parents=True, exist_ok=True)
                with tempfile.TemporaryDirectory(prefix='.native-product-', dir=out.parent) as data_scratch:
                    stage = Path(data_scratch)
                    try:
                        api.download(data_artifact, stage / 'data.zip', MAX_ZIP)
                    except ArtifactUnavailable:
                        reason = 'retained data artifact unavailable/expired; ordinary fresh build required'
                        continue
                    extract_zip(stage / 'data.zip', stage / 'data', profile)
                    directory = stage / 'data'
                    provenance_identity = file_identity(directory / 'product-provenance.json', MAX_METADATA)
                    require(pointer.get('provenance') == provenance_identity, 'product provenance hash/size differs from pointer')
                    provenance = json_object((directory / 'product-provenance.json').read_bytes())
                    require(provenance == {k: v for k, v in pointer.items() if k not in ('data_artifact', 'provenance')},
                            'pointer identities differ from data provenance')
                    base, receipt = validate_data(directory, provenance)
                    # OCI bytes are never parsed by this process or a privileged promoter.
                    require(os.environ.get('GITHUB_WORKFLOW') == 'ci'
                            and os.environ.get('GITHUB_JOB') == 'marlin-vm'
                            and os.environ.get('GITHUB_REPOSITORY') == REPOSITORY
                            and os.environ.get('GITHUB_EVENT_NAME') in ('push', 'pull_request', 'workflow_dispatch', 'schedule'),
                            'OCI load is allowed only in the ordinary native CI job')
                    podman(root, 'load', '--input', directory / 'product-image.oci.tar')
                    loaded = podman(root, 'image', 'inspect', provenance['image_id'], '--format', '{{.Id}}')
                    require(loaded == provenance['image_id'], 'actual loaded Podman image ID differs')
                    podman(root, 'tag', provenance['image_id'], IMAGE_TAG)
                    require(podman(root, 'image', 'inspect', IMAGE_TAG, '--format', '{{.Id}}') == provenance['image_id'],
                            'actual selected Podman tag differs')
                    directory.rename(out)
                restore_metadata(root, out, provenance['files'])
                raw_receipt = (out / 'strip-image.json').read_bytes()
                selection = {'schema': SELECTION_SCHEMA, 'mode': 'reused', 'profile': profile,
                             'reason': 'authenticated exact product with equal actual Git required source inputs',
                             'current_capture_source': current, 'product_source': original,
                             'image_id': provenance['image_id'], 'package': dict(provenance['package'], file=str(out / 'tuna.pkg.tar.zst')),
                             'base': base, 'strip_image': provenance['files']['strip-image.json'],
                             'base_index': expected_base_index, 'base_platform': expected_base_platform,
                             'original_image_receipt': dict(provenance['files']['strip-image.json'], receipt=receipt,
                                                            raw_base64=base64.b64encode(raw_receipt).decode()),
                             'authenticated_origin': {'repository': REPOSITORY, 'workflow_id': workflow['id'],
                                 'workflow_path': WORKFLOW_PATH, 'run_id': run['id'], 'run_attempt': run['run_attempt'],
                                 'head_sha': run['head_sha'], 'pointer_artifact':
                                 {k: pointer_artifact[k] for k in ('id', 'name', 'digest')},
                                 'data_artifact': data_binding, 'provenance': provenance_identity}}
                write_json(out / 'selection.json', selection)
                return selection
    out.mkdir(parents=True, mode=0o700)
    selection = {'schema': SELECTION_SCHEMA, 'mode': 'fresh', 'profile': profile, 'reason': reason,
                 'current_capture_source': current, 'product_source': current, 'package': fresh_package,
                 'image_id': '', 'base': current_base,
                 'base_index': expected_base_index, 'base_platform': expected_base_platform}
    write_json(out / 'selection.json', selection)
    return selection


def copy_regular(source, destination, bound):
    source, destination = Path(source), Path(destination)
    identity = file_identity(source, bound)
    if source.resolve() != destination.resolve():
        require(not destination.is_symlink(), 'refusing retained file symlink')
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, destination)
        destination.chmod(0o600)
        require(file_identity(destination, bound) == identity, 'retained copy differs')
    return identity


def qualification_environment(current):
    require(os.environ.get('GITHUB_REPOSITORY') == REPOSITORY and os.environ.get('GITHUB_EVENT_NAME') == 'push'
            and os.environ.get('GITHUB_REF') == 'refs/heads/main' and os.environ.get('GITHUB_SHA') == current['commit']
            and os.environ.get('GITHUB_WORKFLOW') == 'ci', 'retention requires actual ordinary Main CI checkout')
    run, attempt = os.environ.get('GITHUB_RUN_ID', ''), os.environ.get('GITHUB_RUN_ATTEMPT', '')
    require(re.fullmatch(r'[1-9][0-9]*', run) and re.fullmatch(r'[1-9][0-9]*', attempt), 'missing actual qualification run/attempt')
    return {'id': int(run), 'attempt': int(attempt)}


def retain(root, profile, out, image_id, package, basename, product_commit):
    artifact_name(profile)
    require(image_digest(image_id), 'retention requires exact canonical image ID')
    package_basename(basename)
    root, out = Path(root), Path(out).resolve()
    current = source_identity(root)
    original = source_identity(root, product_commit)
    require(original['required_source_inputs_sha256'] == current['required_source_inputs_sha256'],
            'original product source is not equivalent to qualification source')
    run = qualification_environment(current)
    require(podman(root, 'image', 'inspect', image_id, '--format', '{{.Id}}') == image_id,
            'retained actual Podman image differs')
    out.mkdir(parents=True, exist_ok=True)
    selection_path = out / 'selection.json'
    selected = json_object(selection_path.read_bytes()) if selection_path.exists() else None
    existing_provenance = out / 'product-provenance.json'
    reuse_archive = False
    if selected and selected.get('mode') == 'reused':
        require(selected.get('schema') == SELECTION_SCHEMA and selected.get('profile') == profile
                and selected.get('product_source') == original and selected.get('current_capture_source') == current
                and selected.get('image_id') == image_id, 'retention selection differs from actual product')
        previous = json_object(existing_provenance.read_bytes())
        validate_manifest(previous, profile)
        require(previous['image_id'] == image_id and previous['product_source'] == original
                and file_identity(out / 'product-image.oci.tar') == previous['files']['product-image.oci.tar'],
                'authenticated retained OCI archive changed')
        reuse_archive = True
    package_identity = copy_regular(package, out / 'tuna.pkg.tar.zst', MAX_PACKAGE)
    if selected:
        require(selected['package']['sha256'] == package_identity['sha256'] and selected['package']['basename'] == basename,
                'retained original package differs from selection')
    current_receipt_path = root / 'vm-lane-artifacts/strip-image.json'
    file_identity(current_receipt_path, MAX_METADATA)
    current_receipt = json_object(current_receipt_path.read_bytes())
    require(current_receipt.get('commit') == original['commit'] and current_receipt.get('image_id') == image_id
            and current_receipt.get('package_sha256') == package_identity['sha256'],
            'current actual strip admission differs from retained product')
    require(current_receipt.get('capture_commit') == current['commit']
            and current_receipt.get('capture_tree') == current['tree']
            and current_receipt.get('built_tree') == original['tree']
            and current_receipt.get('required_source_inputs_sha256') == original['required_source_inputs_sha256'],
            'current strip admission does not bind actual product/capture Git sources')
    if selected:
        from vm_strip_image import validate_product_origin
        require(current_receipt.get('product_origin') == selected,
                'current admission did not retain the selected product origin')
        validate_product_origin(selected, current_receipt, root)
    for name in metadata_names(profile):
        source = root / name if name.startswith('orca51-package-provenance/') else root / 'vm-lane-artifacts' / name
        copy_regular(source, out / name, MAX_METADATA)
    if reuse_archive:
        original_receipt = selected.get('original_image_receipt', {})
        raw = base64.b64decode(original_receipt.get('raw_base64', ''), validate=True)
        require(len(raw) == original_receipt.get('bytes')
                and hashlib.sha256(raw).hexdigest() == original_receipt.get('sha256')
                and json_object(raw) == original_receipt.get('receipt')
                and previous['files']['strip-image.json'] ==
                {k: original_receipt[k] for k in ('sha256', 'bytes')},
                'authenticated original strip receipt changed')
        (out / 'strip-image.json').write_bytes(raw)
        (out / 'strip-image.json').chmod(0o600)
    else:
        copy_regular(current_receipt_path, out / 'strip-image.json', MAX_METADATA)
    if profile == 'upstream51':
        for source in (root / 'orca51-package-provenance').iterdir():
            name = 'orca51-package-provenance/' + source.name
            if allowed_file(name, profile) and name not in metadata_names(profile):
                copy_regular(source, out / name, MAX_METADATA)
    if not reuse_archive:
        require(not (out / 'product-image.oci.tar').exists(), 'refusing untrusted preexisting OCI archive')
        subprocess.run(['sudo', 'podman', 'save', '--format', 'oci-archive', '--output',
                        str(out / 'product-image.oci.tar'), image_id], cwd=root, check=True, timeout=1800)
        subprocess.run(['sudo', 'chown', str(os.getuid()) + ':' + str(os.getgid()), str(out / 'product-image.oci.tar')],
                       cwd=root, check=True, timeout=30)
        (out / 'product-image.oci.tar').chmod(0o600)
    files = {}
    for path in out.rglob('*'):
        name = path.relative_to(out).as_posix()
        if path.is_dir():
            continue
        if name in ('selection.json', 'product-provenance.json', 'product-identity.json'):
            continue
        require(allowed_file(name, profile), 'unexpected retained output file: ' + name)
        files[name] = file_identity(path, member_bound(name))
    base = validate_base(out / 'native-base')
    if selected:
        require(selected.get('base_index') == base['index_digest']
                and selected.get('base_platform') == base['platform'],
                'current retained base evidence changed after product selection')
    provenance = {'schema': SCHEMA, 'profile': profile, 'image_id': image_id,
                  'package': {'file': 'tuna.pkg.tar.zst', 'basename': basename, 'sha256': package_identity['sha256']},
                  'product_source': original, 'qualification_source': current, 'qualification_run': run, 'files': files,
                  'base_index': base['index_digest'], 'base_platform': base['platform']}
    validate_manifest(provenance, profile)
    write_json(existing_provenance, provenance)
    # No OCI copy: validate the explicit inventory and metadata in place.
    require(current_receipt.get('base_image') == 'ghcr.io/tuna-os/marlin:gnome@' + base['platform'],
            'current actual strip admission base differs')
    receipt = json_object((out / 'strip-image.json').read_bytes())
    require(receipt.get('commit') == original['commit'] and receipt.get('image_id') == image_id
            and receipt.get('package_sha256') == package_identity['sha256'], 'retained actual strip receipt differs')
    require((out / 'gdm51/final-image-id').read_text().strip() == image_id, 'retained GDM final image differs')
    return provenance


def pointer(out, artifact_id, artifact_digest):
    out = Path(out)
    require(positive(artifact_id), 'actual uploaded data artifact ID required')
    if hex_value(artifact_digest, 64):
        artifact_digest = 'sha256:' + artifact_digest
    require(image_digest(artifact_digest), 'actual uploaded artifact SHA256 required')
    provenance = json_object((out / 'product-provenance.json').read_bytes())
    validate_manifest(provenance, provenance.get('profile'))
    identity = dict(provenance, provenance=file_identity(out / 'product-provenance.json', MAX_METADATA),
                    data_artifact={'id': artifact_id, 'name': artifact_name(provenance['profile'], True), 'digest': artifact_digest})
    write_json(out / 'product-identity.json', identity)
    return identity


def github_outputs(selection, out):
    product, package = selection['product_source'], selection['package']
    return {'restored': str(selection['mode'] == 'reused').lower(), 'product_commit': product['commit'],
            'product_tree': product['tree'], 'product_source_inputs_sha256': product['required_source_inputs_sha256'],
            'package_path': package['file'], 'package_basename': package['basename'], 'package_sha256': package['sha256'],
            'image_id': selection['image_id'], 'base_platform': selection['base']['platform'],
            'base_index': selection['base']['index_digest'], 'product_origin_path': str(Path(out).resolve() / 'selection.json'),
            'reason': selection['reason']}


def main(argv=None, root=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    select_parser = sub.add_parser('select', help='select authenticated equivalent product or explicit ordinary fresh build')
    select_parser.add_argument('--profile', choices=PROFILES, required=True)
    select_parser.add_argument('--out', type=Path, required=True)
    select_parser.add_argument('--current-package', type=Path, required=True)
    select_parser.add_argument('--github-output', type=Path, required=True)
    select_parser.add_argument('--expected-base-index', required=True)
    select_parser.add_argument('--expected-base-platform', required=True)
    select_parser.add_argument('--base-evidence', type=Path, required=True)
    retain_parser = sub.add_parser('retain', help='export exact final product before image-store cleanup')
    retain_parser.add_argument('--profile', choices=PROFILES, required=True)
    retain_parser.add_argument('--out', type=Path, required=True)
    retain_parser.add_argument('--image-id', required=True)
    retain_parser.add_argument('--package', type=Path, required=True)
    retain_parser.add_argument('--package-basename', required=True)
    retain_parser.add_argument('--product-commit', required=True)
    pointer_parser = sub.add_parser('pointer', help='bind actual data upload ID/digest after successful gates')
    pointer_parser.add_argument('--out', type=Path, required=True)
    pointer_parser.add_argument('--data-artifact-id', type=int, required=True)
    pointer_parser.add_argument('--data-artifact-digest', required=True)
    args = parser.parse_args(argv)
    root = Path(root) if root else Path(__file__).resolve().parents[2]
    if args.command == 'select':
        selection = select(root, args.profile, args.out, args.current_package,
                           expected_base_index=args.expected_base_index,
                           expected_base_platform=args.expected_base_platform, base_evidence=args.base_evidence)
        outputs = github_outputs(selection, args.out)
        with args.github_output.open('a') as stream:
            for key, value in outputs.items():
                require(isinstance(value, str) and not any(c in value for c in '\r\n\x00'), 'unsafe GitHub output value')
                stream.write(key + '=' + value + '\n')
        print(json.dumps(outputs, sort_keys=True))
    elif args.command == 'retain':
        result = retain(root, args.profile, args.out, args.image_id, args.package, args.package_basename, args.product_commit)
        print(json.dumps({'provenance_path': str(args.out / 'product-provenance.json'), 'image_id': result['image_id']}, sort_keys=True))
    else:
        result = pointer(args.out, args.data_artifact_id, args.data_artifact_digest)
        print(json.dumps({'pointer_path': str(args.out / 'product-identity.json'), 'data_artifact': result['data_artifact']}, sort_keys=True))
    return 0
