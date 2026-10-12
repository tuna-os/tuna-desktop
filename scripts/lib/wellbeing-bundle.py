"""Admit a wellbeing example from the existing native freeze build receipt."""
import hashlib
import json
from pathlib import Path
import shutil
import subprocess


def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def admit(root, frozen, build_receipt, destination):
    root, frozen, build_receipt, destination = map(Path, (root, frozen, build_receipt, destination))
    root = root.resolve()
    destination = destination.resolve()
    assert destination != root and root not in destination.parents, 'artifacts must remain outside checkout'
    assert not destination.exists(), 'existing bundle cannot be overwritten'
    revision = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()
    status = subprocess.check_output(['git', 'status', '--porcelain'], cwd=root, text=True).strip()
    assert not status, f'source must remain clean: {status}'
    manifest = json.loads((frozen / 'manifest.json').read_text())
    assert manifest['source_commit'] == revision == (frozen / 'SOURCE_COMMIT').read_text().strip()
    assert manifest['cargo_lock_sha256'] == digest(root / 'Cargo.lock') == digest(frozen / 'Cargo.lock')
    required = {'tuna-compositor', 'tuna-shell-gtk', 'tuna-modal-dim-client', 'tuna-wellbeing-proof-shell'}
    artifacts = {}
    for line in build_receipt.read_text().splitlines():
        message = json.loads(line)
        if message.get('reason') != 'compiler-artifact' or not message.get('executable'):
            continue
        name = message['target']['name']
        if name in required:
            assert name not in artifacts, f'ambiguous executable {name}'
            artifacts[name] = Path(message['executable'])
    assert artifacts.keys() == required, 'same build receipt must contain all frozen members and wellbeing example'
    for name in required - {'tuna-wellbeing-proof-shell'}:
        assert digest(artifacts[name]) == digest(frozen / name) == manifest['sha256'][name]
    records = {name: digest(path) for name, path in artifacts.items()}
    destination.mkdir(parents=True)
    for name in ('tuna-compositor', 'tuna-wellbeing-proof-shell'):
        shutil.copy2(artifacts[name], destination / name)
        assert digest(destination / name) == records[name]
    shutil.copy2(frozen / 'Cargo.lock', destination / 'Cargo.lock')
    result = {'source_commit': revision, 'cargo_lock_sha256': manifest['cargo_lock_sha256'], 'sha256': records,
              'parent_manifest_sha256': digest(frozen / 'manifest.json'), 'build_receipt_sha256': digest(build_receipt),
              'scope': 'production compositor renderer, supervised synthetic control peer; no GTK timer or installed DRM acceptance'}
    (destination / 'manifest.json').write_text(json.dumps(result, indent=2) + '\n')
    (destination / 'SOURCE_COMMIT').write_text(revision + '\n')
    return result
