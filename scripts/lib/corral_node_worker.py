"""Restricted node staging operations, run only in an admitted owned tool pod."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time
import zipfile

MAX_ARCHIVE = 3 * 1024**3
MAX_EXPANSION = 3 * 1024**3
VIRTUAL = 18 * 1024**3


def digest(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def receive(stream, path, expected_bytes, expected_sha):
    """Only a complete, exact authenticated archive becomes usable."""
    if type(expected_bytes) is not int or not 0 < expected_bytes <= MAX_ARCHIVE:
        raise ValueError('archive byte bound differs')
    total = 0
    with Path(path).open('xb') as target:
        while chunk := stream.read(min(1024*1024, expected_bytes-total+1)):
            total += len(chunk)
            if total > expected_bytes:
                raise ValueError('archive exceeds original API size')
            target.write(chunk)
        target.flush()
        os.fsync(target.fileno())
    if total != expected_bytes or digest(path) != expected_sha:
        raise ValueError('original archive length or SHA differs')


def extract(path, destination):
    """Validate the complete central directory before bounded CRC extraction."""
    destination = Path(destination)
    destination.mkdir(mode=0o700, exist_ok=False)
    with zipfile.ZipFile(path) as archive:
        entries = archive.infolist()
        if not 0 < len(entries) <= 256:
            raise ValueError('archive member count differs')
        names = set()
        total = 0
        for entry in entries:
            name = Path(entry.filename)
            mode = entry.external_attr >> 16
            if (name.is_absolute() or '..' in name.parts or '\\' in entry.filename
                    or name.as_posix() != entry.filename.rstrip('/')
                    or name.as_posix() in names or mode & 0o170000 == 0o120000
                    or entry.flag_bits & 1):
                raise ValueError('unsafe or encrypted archive member')
            names.add(name.as_posix())
            total += entry.file_size
            if entry.file_size > 2*1024**3 or total > MAX_EXPANSION:
                raise ValueError('expanded artifact exceeds bound')
        if not {'manifest.json', 'disk.qcow2'} <= names:
            raise ValueError('original export members missing')
        for entry in entries:
            target = destination / entry.filename
            if entry.is_dir():
                target.mkdir(parents=True, exist_ok=True)
                continue
            target.parent.mkdir(parents=True, exist_ok=True)
            with archive.open(entry) as source, target.open('xb') as output:
                total = 0
                while data := source.read(1024*1024):
                    total += len(data)
                    if total > entry.file_size:
                        raise ValueError('member exceeded central-directory length')
                    output.write(data)
                if total != entry.file_size:
                    raise ValueError('member length differs')
    return total


def inspect_qcow(path, manifest, deadline):
    def command(*args):
        remaining = deadline-time.monotonic()
        if remaining <= 0:
            raise TimeoutError('qemu inspection deadline')
        result = subprocess.run(['qemu-img', *args, str(path)], check=True,
                                capture_output=True, timeout=remaining)
        if len(result.stdout) > 4*1024*1024:
            raise ValueError('qemu evidence exceeds bound')
        return json.loads(result.stdout)
    info = command('info', '--output=json')
    if (info.get('format') != 'qcow2' or info.get('virtual-size') != VIRTUAL
            or info.get('backing-filename') or info.get('data-file')):
        raise ValueError('standalone original 18 GiB QCOW2 required')
    mapping = command('map', '--output=json')
    if not isinstance(mapping, list) or len(mapping) > 65536:
        raise ValueError('bounded QCOW2 extent list required')
    end = 0
    for extent in mapping:
        start, length = extent.get('start'), extent.get('length')
        if (type(start) is not int or type(length) is not int or start != end
                or length <= 0 or start+length > VIRTUAL
                or extent.get('depth') != 0):
            raise ValueError('QCOW2 mapping coverage or backing depth differs')
        end = start+length
    if end != VIRTUAL or digest(path) != manifest['disk_sha256']:
        raise ValueError('original QCOW2 bytes or extent coverage differs')
    return info, mapping


def convert(export, target, deadline):
    export, target = Path(export), Path(target)
    manifest = json.loads((export/'manifest.json').read_text())
    info, mapping = inspect_qcow(export/'disk.qcow2', manifest, deadline)
    if target.exists():
        raise ValueError('refusing final target overwrite')
    remaining = deadline-time.monotonic()
    if remaining <= 0:
        raise TimeoutError('conversion deadline')
    subprocess.run(['qemu-img', 'convert', '-f', 'qcow2', '-O', 'raw', '-S', '4k',
                    str(export/'disk.qcow2'), str(target)], check=True, timeout=remaining)
    result = {'logical_bytes': target.stat().st_size,
              'allocated_bytes': target.stat().st_blocks*512, 'raw_sha256': digest(target),
              'info': info, 'map': mapping}
    if result['logical_bytes'] != VIRTUAL or result['raw_sha256'] != manifest['raw_sha256']:
        raise ValueError('node converted RAW differs from original exporter')
    return result


def usage(roots):
    result = {}
    for root in roots:
        total = 0; seen = set(); count = 0
        for base, dirs, files in os.walk(root, followlinks=False):
            count += len(dirs)+len(files)
            if count > 2048:raise ValueError('owned allocation inventory exceeds bound')
            for name in files:
                path = Path(base)/name
                if path.is_symlink():raise ValueError('owned allocation symlink refused')
                st = path.stat()
                identity = (st.st_dev, st.st_ino)
                if identity not in seen:total += st.st_blocks*512;seen.add(identity)
        result[root] = total
    return result


def main():
    operation = sys.argv[1]
    if operation == 'receive':
        receive(sys.stdin.buffer, '/stage/original-artifact.zip', int(sys.argv[2]), sys.argv[3])
        extract('/stage/original-artifact.zip', '/stage/export')
        manifest_path = Path('/stage/export/manifest.json')
        if manifest_path.stat().st_size > 256*1024:
            raise ValueError('bounded original manifest required')
        manifest = json.loads(manifest_path.read_text())
        evidence = manifest.get('evidence')
        if not isinstance(evidence, dict) or not evidence:
            raise ValueError('export evidence missing')
        for name, expected in evidence.items():
            if Path(name).name != name or digest(Path('/stage/export')/name) != expected:
                raise ValueError('original evidence member differs')
        print(json.dumps({'manifest': manifest, 'manifest_sha256': digest('/stage/export/manifest.json'),
                          'disk_sha256': digest('/stage/export/disk.qcow2')}))
    elif operation == 'convert':
        print(json.dumps(convert('/stage/export', '/target/disk.img',
                                 time.monotonic()+float(sys.argv[2]))))
    elif operation == 'usage':
        print(json.dumps(usage(['/stage', '/target'])))
    elif operation == 'readback':
        print(json.dumps({'raw_sha256': digest('/target/disk.img'),
                          'logical_bytes': Path('/target/disk.img').stat().st_size}))
    else:
        raise ValueError('unknown private staging operation')


if __name__ == '__main__':
    main()
