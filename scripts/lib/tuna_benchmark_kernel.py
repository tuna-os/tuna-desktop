"""Read-only exact Arch split-kernel observation; no version-order selection.

Linux/x86 header offsets: https://docs.kernel.org/arch/x86/boot.html
Package identity comes from pacman query/list/owns, not ABI punctuation guesses.
"""
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import time

NAME = re.compile(r'[A-Za-z0-9._+-]{1,128}')
ROOT = '/usr/lib/modules/'
MEMBERS = ('vmlinuz', 'initramfs.img', 'pkgbase', 'modules.dep')


def require(ok, message):
    if not ok:
        raise ValueError(message)


def kernel_release(data):
    """Decode bounded x86 boot-protocol release; refuse unknown/32-bit layouts."""
    require(len(data) >= 0x238 and data[0x1fe:0x200] == b'\x55\xaa'
            and data[0x202:0x206] == b'HdrS', 'Linux x86 boot header required')
    proto = int.from_bytes(data[0x206:0x208], 'little')
    require(proto >= 0x020c and int.from_bytes(data[0x236:0x238], 'little') & 1,
            '64-bit split-kernel boot protocol required')
    pointer = int.from_bytes(data[0x20e:0x210], 'little')
    setup = (data[0x1f1] or 4) * 512
    require(0 < pointer < setup, 'bounded kernel release pointer required')
    start = pointer + 0x200
    end = data.find(b'\0', start, min(len(data), setup + 512, start + 4096))
    require(end > start, 'bounded terminated kernel release required')
    release = data[start:end].decode('ascii').split()[0]
    require(NAME.fullmatch(release), 'genuine kernel ABI required')
    return release


def protected_hashes(text):
    result = {}
    require(isinstance(text, str) and 0 < len(text) <= 16 * 1024**2,
            'bounded protected file receipt required')
    for line in text.splitlines():
        match = re.fullmatch(r'([0-9a-f]{64})  (/.+)', line)
        require(match is not None, 'malformed protected critical hash receipt')
        sha, path = match.groups()
        require(path not in result, 'duplicate protected file identity')
        result[path] = sha
    return result


def select_kernel(value, critical, expected_package):
    """Actual production admission; caller must bind observation to exact image."""
    require(type(value) is dict and type(value.get('schema')) is int and value['schema'] == 1, 'kernel observation required')
    require(isinstance(expected_package,str) and value.get('package') == expected_package and
            re.fullmatch(r'linux [A-Za-z0-9._+:-]{1,128}', expected_package),
            'exact installed Linux package identity required')
    listed = value.get('package_vmlinuz')
    require(type(listed) is list and len(listed) == 1 and isinstance(listed[0],str), 'one package-listed kernel required')
    match = re.fullmatch(re.escape(ROOT) + r'([A-Za-z0-9._+-]{1,128})/vmlinuz', listed[0])
    require(match is not None, 'canonical package-owned kernel path required')
    version = match[1]
    directories = value.get('directories')
    require(type(directories) is list and 0 < len(directories) <= 256, 'bounded module inventory required')
    seen = set()
    bootable = []
    for row in directories:
        require(type(row) is dict and isinstance(row.get('version'),str) and NAME.fullmatch(row['version'])
                and row['version'] not in seen and row.get('genuine_directory') is True
                and type(row.get('vmlinuz_present')) is bool, 'genuine unique module directory required')
        seen.add(row['version'])
        if row['vmlinuz_present']:
            require(row.get('vmlinuz_regular') is True, 'regular nonlinked boot payload required')
            bootable.append(row)
        else:
            require(row.get('classification') == 'nonboot-no-vmlinuz', 'measured nonboot classification required')
    require(len(bootable) == 1 and bootable[0]['version'] == version
            and bootable[0].get('vmlinuz_owner') == 'linux',
            'one actual package-owned boot kernel required')
    require(value.get('uki_paths') == [], 'unknown UKI boot precedence refused')
    require(value.get('pkgbase') == 'linux' and value.get('kernel_header_release') == version,
            'package/kernel header ABI mismatch')
    module = value.get('module_probe')
    require(type(module) is dict and module.get('owner') == 'linux'
            and isinstance(module.get('path'), str) and module['path'].startswith(ROOT + version + '/kernel/')
            and re.fullmatch(r'/usr/lib/modules/[A-Za-z0-9._+-]+/kernel/[A-Za-z0-9_./+-]+\.ko(?:\.(?:xz|zst|gz))?', module['path'])
            and '..' not in Path(module['path']).parts and isinstance(module.get('vermagic'),str) and module['vermagic'].split()[:1] == [version],
            'actual owned module ABI mismatch')
    initrd = value.get('initramfs')
    require(type(initrd) is dict and initrd.get('module_abis') == [version]
            and initrd.get('bootc_root_setup') is True, 'initramfs ABI/root setup mismatch')
    expected = protected_hashes(critical)
    actual = value.get('members_sha256')
    require(type(actual) is dict and set(actual) == set(MEMBERS), 'complete selected member hashes required')
    for name in MEMBERS:
        path = ROOT + version + '/' + name
        require(path in expected and actual[name] == expected[path], 'selected protected member changed: ' + name)
    require(expected.get(module['path']) == module.get('sha256') and isinstance(module.get('sha256'),str) and
            re.fullmatch(r'[0-9a-f]{64}', module.get('sha256', '')), 'protected module probe changed')
    require(isinstance(value.get('bootc_version'), str) and value['bootc_version'].startswith('bootc ')
            and isinstance(value.get('package_manifest_sha256'),str) and re.fullmatch(r'[0-9a-f]{64}', value['package_manifest_sha256']),
            'actual tool/package manifest observation required')
    return version


def collect(expected_package, *, rootfs=Path('/'), run=None):
    # Trusted Python fixtures may supply a private root and inert query function.
    # The actual image entrypoint exposes neither argument.
    run = subprocess.run if run is None else run
    def locate(path):
        logical=Path(path)
        require(logical.is_absolute() and '..' not in logical.parts,'canonical source path required')
        return rootfs / logical.relative_to('/')
    deadline = time.monotonic() + 45
    def query(*args, optional=False):
        left = deadline - time.monotonic()
        require(left > 0, 'kernel observation deadline')
        result = run(args, capture_output=True, timeout=min(5, left),
                                env={**os.environ, 'LC_ALL': 'C'})
        require(len(result.stdout) + len(result.stderr) <= 2 * 1024**2, 'kernel query output bound')
        if optional and result.returncode == 1:
            return None
        require(result.returncode == 0, 'kernel source query failed')
        return result.stdout.decode('utf-8').strip()
    def regular(path):
        require(path.is_absolute(), 'absolute source member required')
        for parent in [path, *path.parents]:
            require(not parent.is_symlink(), 'linked source member refused')
        require(stat.S_ISREG(path.lstat().st_mode) and path.stat().st_size > 0, 'nonempty regular source member required')
    def sha(path):
        regular(path)
        require(path.stat().st_size <= 512 * 1024**2, 'kernel member size bound')
        h = hashlib.sha256()
        with path.open('rb') as stream:
            while True:
                require(time.monotonic() < deadline, 'kernel hash observation deadline')
                block = stream.read(1024**2)
                if not block:
                    break
                h.update(block)
        return h.hexdigest()
    package = query('pacman', '-Q', 'linux')
    require(package == expected_package, 'installed kernel package changed')
    files = query('pacman', '-Qlq', 'linux')
    listed = [p for p in files.splitlines() if re.fullmatch(re.escape(ROOT) + r'[^/]+/vmlinuz', p)]
    require(len(listed) == 1, 'one installed package kernel required')
    path = Path(listed[0]);version = path.parent.name
    require(NAME.fullmatch(version), 'canonical kernel ABI required')
    root = locate(ROOT)
    require(root.is_dir() and root.resolve() == root, 'genuine modules root required')
    children = list(root.iterdir());require(len(children) <= 256, 'module directory count bound')
    directories = []
    for child in children:
        require(child.is_dir() and not child.is_symlink() and NAME.fullmatch(child.name), 'genuine module directory required')
        kernel = child / 'vmlinuz';present = kernel.exists() or kernel.is_symlink()
        row = {'version': child.name, 'genuine_directory': True, 'vmlinuz_present': present,
               'directory_owner': query('pacman', '-Qqo', ROOT+child.name, optional=True),
               'contents_ownership': 'not queried'}
        if present:
            regular(kernel);row.update(vmlinuz_regular=True, vmlinuz_owner=query('pacman', '-Qqo', ROOT+child.name+'/vmlinuz', optional=True))
        else:
            row['classification'] = 'nonboot-no-vmlinuz'
        directories.append(row)
    uki_root = locate('/boot/EFI/Linux');ukis = []
    require(all(not parent.is_symlink() for parent in [uki_root,*uki_root.parents]),'linked UKI layout refused')
    if uki_root.exists():
        require(uki_root.is_dir() and not uki_root.is_symlink(), 'genuine source UKI directory required')
        ukis = ['/boot/EFI/Linux/'+p.name for p in uki_root.iterdir() if p.suffix.lower() == '.efi']
        require(len(ukis) <= 128, 'source UKI count bound')
    regular(locate(path))
    with locate(path).open('rb') as stream:
        release = kernel_release(stream.read(128 * 1024))
    pkgbase = locate(path.parent / 'pkgbase');regular(pkgbase)
    require(pkgbase.stat().st_size <= 4096, 'pkgbase bound')
    modules = [p for p in files.splitlines() if p.startswith(str(path.parent) + '/kernel/') and re.search(r'\.ko(?:\.(?:xz|zst|gz))?$', p)]
    require(modules, 'actual owned module probe required')
    module = Path(sorted(modules)[0])
    module_probe = {'path': str(module), 'owner': query('pacman', '-Qqo', str(module)),
                    'sha256': sha(locate(module)), 'vermagic': query('modinfo', '-F', 'vermagic', str(module))}
    initrd_path = path.parent / 'initramfs.img'
    listing = query('lsinitrd', str(initrd_path))
    abis = sorted(set(re.findall(r'(?:usr/)?lib/modules/([^/\s]+)/', listing)))
    return {'schema': 1, 'package': package, 'package_vmlinuz': listed,
            'package_manifest_sha256': hashlib.sha256(files.encode()).hexdigest(),
            'bootc_version': query('bootc', '--version'), 'directories': directories, 'uki_paths': ukis,
            'pkgbase': pkgbase.read_text().strip(), 'kernel_header_release': release,
            'members_sha256': {name: sha(locate(path.parent / name)) for name in MEMBERS},
            'module_probe': module_probe,
            'initramfs': {'module_abis': abis, 'bootc_root_setup': 'bootc-root-setup.service' in listing,
                          'listing_sha256': hashlib.sha256(listing.encode()).hexdigest()}}


if __name__ == '__main__':
    require(len(sys.argv) == 2, 'one exact expected package required')
    print(json.dumps(collect(sys.argv[1]), sort_keys=True))
