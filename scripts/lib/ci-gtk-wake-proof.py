#!/usr/bin/python3
"""Build/freeze the current clean source and retain GTK wake proof CI evidence."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--artifacts', type=Path, required=True)
    parser.add_argument('--display', default=':108')
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    artifacts = args.artifacts.resolve()
    if artifacts.is_relative_to(root):
        parser.error('artifacts must be outside the source checkout')
    if artifacts.exists() and any(artifacts.iterdir()):
        parser.error('use a new empty artifact directory; stale receipts are rejected')
    artifacts.mkdir(parents=True, exist_ok=True)
    receipt = {'qualified': False, 'scope': 'nested GTK wake regression',
               'physical_latency_qualified': False, 'gnome_comparison': False,
               'whole_session_cpu_qualified': False, 'power_qualified': False}
    env = dict(os.environ, PYTHONDONTWRITEBYTECODE='1')
    env.setdefault('CARGO_BUILD_JOBS', '2')
    target = Path(env.get('CARGO_TARGET_DIR', root/'target'))
    if not target.is_absolute():
        target = root/target
    env['CARGO_TARGET_DIR'] = str(target)

    def clean_source():
        revision = subprocess.check_output(['git', 'rev-parse', 'HEAD'], cwd=root, text=True).strip()
        dirty = subprocess.check_output(['git', 'status', '--porcelain'], cwd=root, text=True).strip()
        if dirty:
            raise RuntimeError('source checkout is dirty')
        return revision

    def run(command, phase):
        receipt['phase'] = phase
        receipt['command'] = command
        with (artifacts/(phase+'.log')).open('w') as log:
            result = subprocess.run(command, cwd=root, env=env, stdout=log, stderr=subprocess.STDOUT)
        if result.returncode:
            raise RuntimeError(f'{phase} failed with exit {result.returncode}; see retained log')

    try:
        source_revision = clean_source()
        receipt['source_revision'] = source_revision
        if os.environ.get('GITHUB_SHA') and os.environ['GITHUB_SHA'] != source_revision:
            raise RuntimeError('checkout revision does not match GITHUB_SHA')
        # Retain third-party artifacts, rebuild every workspace-owned package.
        run(['cargo', 'clean', '-p', 'tuna-compositor', '-p', 'tuna-greeter', '-p',
             'tuna-shell-control', '-p', 'tuna-shell-gtk', '-p', 'tuna-shell-host',
             '-p', 'tuna-wallpaper'], 'workspace-clean')
        run(['cargo', 'build', '--locked', '-p', 'tuna-compositor', '-p', 'tuna-shell-gtk',
             '-p', 'tuna-shell-host', '--bins', '--example', 'tuna-modal-dim-client'], 'native-build')
        if clean_source() != source_revision:
            raise RuntimeError('source changed during build')
        frozen = artifacts/'frozen'
        frozen.mkdir()
        names = ('tuna-compositor', 'tuna-shell-gtk', 'tuna-shell-host', 'tuna-ibus-bridge',
                 'tuna-modal-dim-client')
        for name in names:
            built = target/'debug'/('examples' if name == 'tuna-modal-dim-client' else '')/name
            shutil.copy2(built, frozen/name)
        shutil.copy2(root/'Cargo.lock', frozen/'Cargo.lock')
        manifest = {'source_revision': source_revision, 'source_dirty': False,
                    'cargo_lock_sha256': digest(frozen/'Cargo.lock'),
                    'binary_sha256': {name: digest(frozen/name) for name in names}}
        # Shared immutable bundle vocabulary used by the minimize proof.
        manifest['source_commit'] = source_revision
        manifest['sha256'] = manifest['binary_sha256']
        (frozen/'manifest.json').write_text(json.dumps(manifest, indent=2)+'\n')
        (frozen/'SOURCE_COMMIT').write_text(source_revision+'\n')
        (frozen/'SHA256SUMS').write_text(''.join(manifest['binary_sha256'][name]+'  '+name+'\n' for name in names))
        receipt['binary_sha256'] = manifest['binary_sha256']
        run(['dbus-run-session', '--', '/usr/bin/python3', str(root/'scripts/tuna-gtk-wake-proof'),
             '--bin-dir', str(frozen), '--artifacts', str(artifacts/'proof'), '--display',
             args.display, '--trace-idle'], 'native-proof')
        if clean_source() != source_revision:
            raise RuntimeError('source changed during native proof')
        receipt.update(qualified=True, phase='complete')
        return 0
    except Exception as error:
        receipt['error'] = str(error)
        print(error, file=sys.stderr)
        return 1
    finally:
        (artifacts/'result.json').write_text(json.dumps(receipt, indent=2)+'\n')


if __name__ == '__main__':
    raise SystemExit(main())
