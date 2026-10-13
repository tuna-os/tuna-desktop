#!/usr/bin/env python3
"""Actual tool-image smoke; synthetic sparse bytes, never a Marlin boot claim."""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import tempfile
import zipfile

spec = importlib.util.spec_from_file_location('worker', '/opt/tuna-tools/corral_node_worker.py')
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)
if os.getuid() != 107:
    raise ValueError('tool smoke must run as UID 107')
qemu_version = subprocess.check_output(['qemu-img', '--version'], timeout=10).decode().splitlines()[0]
qemu = worker.digest('/usr/bin/qemu-img')
if qemu != '250b2b4ee552fdd96e1078525d12bb0b1f2557f33f153c282bfaea1ece5b3fad':
    raise ValueError('retained qemu executable differs')
loader = subprocess.run(['/lib64/ld-linux-x86-64.so.2', '--list', '/usr/bin/qemu-img'], check=True, capture_output=True, text=True, timeout=10)
libraries = {}
for line in loader.stdout.splitlines():
    for token in line.split():
        if token.startswith('/'):
            libraries[token] = worker.digest(token)
if not libraries:raise ValueError('actual ELF library closure absent')
with tempfile.TemporaryDirectory() as directory:
    root = Path(directory)
    raw = root/'original.raw'
    with raw.open('xb') as f:
        f.write(b'TUNA_NODE_STAGE_SMOKE'+b'\xff'*65536)
        f.seek(8*1024**3)
        f.write(b'NONZERO_MIDDLE')
        f.truncate(worker.VIRTUAL)
    qcow = root/'disk.qcow2'
    subprocess.run(['qemu-img', 'convert', '-f', 'raw', '-O', 'qcow2', str(raw), str(qcow)],
                   check=True, timeout=120)
    manifest = {'disk_sha256': worker.digest(qcow), 'raw_sha256': worker.digest(raw)}
    (root/'manifest.json').write_text(json.dumps(manifest))
    converted = worker.convert(root, root/'converted.raw', __import__('time').monotonic()+180)
    if converted['allocated_bytes'] > 16*1024*1024:
        raise ValueError('sparse mixed-extents conversion unexpectedly allocated full disk')
    archive = root/'original.zip'
    with zipfile.ZipFile(archive, 'w', zipfile.ZIP_DEFLATED) as z:
        z.write(qcow, 'disk.qcow2');z.write(root/'manifest.json', 'manifest.json')
    with archive.open('rb') as source:
        worker.receive(source, root/'received.zip', archive.stat().st_size, worker.digest(archive))
    worker.extract(root/'received.zip', root/'extracted')
    if worker.digest(root/'extracted/disk.qcow2') != manifest['disk_sha256']:
        raise ValueError('ZIP roundtrip bytes differ')
    rejected = 0
    for bad in ('wrong-digest', 'wrong-size', 'overwrite'):
        try:
            with archive.open('rb') as source:
                worker.receive(source, root/('bad-'+bad), archive.stat().st_size+(bad=='wrong-size'),
                               '0'*64 if bad=='wrong-digest' else worker.digest(archive))
            if bad=='overwrite':
                worker.convert(root, root/'converted.raw', __import__('time').monotonic()+180)
        except (ValueError, FileExistsError):
            rejected += 1
    if rejected != 3:
        raise ValueError('tool negative controls did not reject')
    backing = root/'backed.qcow2'
    subprocess.run(['qemu-img', 'create', '-f', 'qcow2', '-F', 'qcow2', '-b', str(qcow), str(backing)],
                   check=True, timeout=30)
    try:
        worker.inspect_qcow(backing, manifest, __import__('time').monotonic()+30)
    except ValueError:
        rejected += 1
    else:
        raise ValueError('backing file was admitted')
    print(json.dumps({'schema': 1, 'qemu_version': qemu_version, 'python_version': list(__import__('sys').version_info[:3]), 'smoke_passed': True, 'uid': os.getuid(),
                      'qemu_before_sha256': qemu, 'qemu_after_sha256': worker.digest('/usr/bin/qemu-img'),
                      'worker_sha256': worker.digest('/opt/tuna-tools/corral_node_worker.py'),
                      'qemu_libraries': libraries, 'raw_sha256': manifest['raw_sha256'], 'negative_controls': rejected,
                      'logical_bytes': converted['logical_bytes'], 'allocated_bytes': converted['allocated_bytes'],
                      'scope': 'Synthetic mixed extents and tool runtime only; no image import or boot.'}))
