"""Evidence checks for the isolated Python runtime and unchanged QEMU closure."""
import hashlib
from pathlib import Path
import subprocess

PRIVATE_ROOT = Path('/opt/tuna-python')


def sha(path):
    with Path(path).open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def qemu_libraries():
    result = subprocess.run(['/lib64/ld-linux-x86-64.so.2', '--list', '/usr/bin/qemu-img'],
                            check=True, capture_output=True, text=True, timeout=10)
    libraries = {}
    for line in result.stdout.splitlines():
        for token in line.split():
            if token.startswith('/'):
                libraries[token] = sha(token)
    if not libraries:
        raise ValueError('actual qemu required ELF libraries absent')
    return libraries


def baseline_libraries(path):
    result = {}
    for line in Path(path).read_text().splitlines():
        digest, name = line.split(None, 1)
        name = name.strip()
        if len(digest) != 64 or not name.startswith('/') or name in result:
            raise ValueError('invalid original qemu library evidence')
        result[name] = digest
    if not result:
        raise ValueError('original qemu library evidence absent')
    return result


def same_qemu_libraries(before, after):
    if not before or before != after:
        raise ValueError('qemu required ELF library paths or bytes changed')


def private_elf_paths(maps, root=PRIVATE_ROOT, resolve=None, is_elf=None):
    """Reject every actually mapped ELF file outside the private runtime."""
    resolve = resolve or (lambda name: Path(name).resolve(strict=True))
    def header(name):
        with Path(name).open('rb') as stream:
            return stream.read(4) == b'\x7fELF'
    is_elf = is_elf or header
    paths = set()
    for line in maps.splitlines():
        fields = line.split(None, 5)
        if len(fields) != 6 or not fields[5].startswith('/'):
            continue
        name = fields[5]
        if name.endswith(' (deleted)'):
            raise ValueError('mapped runtime file was deleted')
        actual = resolve(name)
        if is_elf(actual):
            if not actual.is_relative_to(root):
                raise ValueError('Python mapped a global ELF dependency')
            paths.add(actual)
    if not paths:
        raise ValueError('actual private Python ELF mappings absent')
    return sorted(paths)
