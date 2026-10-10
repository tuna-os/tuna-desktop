"""Repair composefs ESP bytes from one exact source image, using owned resources."""
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import uuid
from tuna_benchmark_disk import digest, require


class Commands:
    def __init__(self, log):
        self.log = log

    def call(self, args, check=True):
        with self.log.open('a') as log:
            log.write(json.dumps(args)+'\n'); log.flush()
            result = subprocess.run(args, stdout=subprocess.PIPE, stderr=log, text=True)
            log.write(result.stdout); log.flush()
        if check and result.returncode:
            raise RuntimeError('ESP command failed: '+args[1])
        return result.returncode, result.stdout.strip()


def fat_partition(value, loop):
    require(type(value) is dict and type(value.get('blockdevices')) is list
            and len(value['blockdevices']) == 1, 'one owned loop tree required')
    device = value['blockdevices'][0]
    require(device.get('path') == loop and device.get('type') == 'loop', 'unexpected loop tree')
    children = device.get('children', [])
    require(type(children) is list and children and len(children) <= 16, 'partition count bound')
    matches = []
    for child in children:
        path = child.get('path', '')
        require(child.get('type') == 'part' and re.fullmatch(re.escape(loop)+r'p[1-9][0-9]*', path),
                'partition is not owned by this loop')
        if child.get('fstype') in ('vfat', 'fat', 'fat32'):
            matches.append(path)
    require(len(matches) == 1, 'unique FAT ESP required; refusing first-partition selection')
    return matches[0]


def critical_hashes(text, version):
    expected = {}
    wanted = {f'/usr/lib/modules/{version}/vmlinuz', f'/usr/lib/modules/{version}/initramfs.img'}
    for line in text.splitlines():
        match = re.fullmatch(r'([0-9a-f]{64})  (/.+)', line)
        require(match is not None, 'malformed protected critical hash receipt')
        sha, path = match.groups()
        if path in wanted:
            require(path not in expected, 'duplicate protected kernel member')
            expected[path] = sha
    require(set(expected) == wanted, 'protected kernel/initramfs hashes missing')
    return expected


def repair(raw, image, scratch, critical, record, log, commands=None):
    """Always clean our container/mount/loop; failure retains unsafe raw backing."""
    require(re.fullmatch(r'(?:sha256:)?[0-9a-f]{64}', image), 'exact source image ID required')
    raw = raw.resolve(); scratch = scratch.resolve()
    require(raw.is_file() and raw.parent == scratch, 'raw must belong to owned scratch')
    resources = scratch/'esp-repair'
    resources.mkdir()
    source = resources/'source'
    mount = resources/'mount'
    command = commands or Commands(log)
    container = 'tuna-esp-'+uuid.uuid4().hex
    loop = None
    mount_attempted = False
    container_attempted = False
    cleanup_errors = []
    record.update(source_image_id=image, repaired=False, owned_resources_removed=False,
                  container_name=container, mount_target=str(mount), resource_directory=str(resources),
                  boot_qualified=False)
    try:
        source.mkdir(); mount.mkdir()
        record['protected_critical_sha256'] = digest(critical)
        container_attempted = True
        command.call(['sudo','podman','create','--name',container,image])
        _, versions = command.call(['sudo','podman','run','--rm','--entrypoint','',image,'sh','-c',
            'for d in /usr/lib/modules/*; do test -d "$d" || continue; test ! -L "$d" || exit 2; printf "%s\\n" "${d##*/}"; done'])
        versions = versions.splitlines()
        require(len(versions) == 1 and re.fullmatch(r'[A-Za-z0-9._+-]{1,128}', versions[0]),
                'exactly one source kernel version required')
        version = versions[0]; record['kernel_version'] = version
        expected = critical_hashes(critical.read_text(), version)
        for name in ('vmlinuz','initramfs.img'):
            path = f'/usr/lib/modules/{version}/{name}'
            command.call(['sudo','podman','run','--rm','--entrypoint','',image,'sh','-c',
                          'test -f "$1" && test ! -L "$1"', 'sh', path])
            command.call(['sudo','podman','cp',container+':'+path,str(source/name)])
            command.call(['sudo','chown',f'{os.getuid()}:{os.getgid()}',str(source/name)])
            require((source/name).is_file() and not (source/name).is_symlink()
                    and (source/name).stat().st_size > 0, 'source kernel member missing/linked/empty')
            require(digest(source/name) == expected[path], 'source differs from protected baseline: '+name)
        _, loop = command.call(['sudo','losetup','--find','--show','--partscan',str(raw)])
        require(re.fullmatch(r'/dev/loop[0-9]+',loop), 'unexpected owned loop identity')
        record['loop_device'] = loop
        _, ownership = command.call(['sudo','losetup','--json','--output','NAME,BACK-FILE',loop])
        identities = json.loads(ownership).get('loopdevices',[])
        require(len(identities) == 1 and identities[0].get('name') == loop
                and identities[0].get('back-file') == str(raw), 'loop backing identity mismatch')
        command.call(['sudo','udevadm','settle','--timeout=20'])
        _, tree = command.call(['sudo','lsblk','--json','--tree','--output','PATH,TYPE,FSTYPE',loop])
        esp = fat_partition(json.loads(tree),loop); record['esp_partition'] = esp
        mount_attempted = True
        command.call(['sudo','mount','-t','vfat','-o','rw,nosuid,nodev,noexec',esp,str(mount)])
        # Glob only our mounted ESP and reject links/traversal or ambiguity.
        efi = mount/'EFI'; linux = efi/'Linux'
        require(efi.is_dir() and not efi.is_symlink() and linux.is_dir() and not linux.is_symlink(),
                'mounted FAT partition has no genuine EFI/Linux directory')
        targets = list(linux.glob('bootc_composefs-*'))
        require(len(targets) == 1 and targets[0].is_dir() and not targets[0].is_symlink(),
                'unique bootc_composefs target required')
        target = targets[0]; record['esp_target'] = str(target.relative_to(mount))
        record['members'] = {}
        for src, dest in [('vmlinuz','vmlinuz'),('initramfs.img','initrd')]:
            destination = target/dest
            require(destination.is_file() and not destination.is_symlink(), 'ESP member missing/linked')
            before = digest(destination)
            command.call(['sudo','cp','--',str(source/src),str(destination)])
            expected_sha = expected[f'/usr/lib/modules/{version}/{src}']
            require(digest(destination) == expected_sha, 'materialized ESP hash mismatch')
            record['members'][dest] = {'source_path':f'/usr/lib/modules/{version}/{src}',
                'source_sha256':expected_sha,'esp_before_sha256':before,
                'esp_after_sha256':digest(destination),'bytes':destination.stat().st_size}
        command.call(['sudo','sync','-f',str(mount)])
        record['repaired'] = True
    finally:
        if mount_attempted:
            try:
                status, _ = command.call(['mountpoint','-q',str(mount)],check=False)
                require(status in (0,32), 'mount state query failed')
                if status == 0:
                    command.call(['sudo','umount',str(mount)])
            except Exception as error: cleanup_errors.append('mount: '+str(error))
        if loop is not None:
            try:
                require(re.fullmatch(r'/dev/loop[0-9]+',loop), 'cannot detach malformed loop identity')
                if not cleanup_errors:
                    _, ownership = command.call(['sudo','losetup','--json','--output','NAME,BACK-FILE',loop])
                    identities = json.loads(ownership).get('loopdevices',[])
                    require(len(identities) == 1 and identities[0].get('name') == loop
                            and identities[0].get('back-file') == str(raw), 'refusing foreign loop detach')
                    command.call(['sudo','losetup','--detach',loop])
                else:
                    raise RuntimeError('retaining loop while owned mount cleanup failed')
            except Exception as error: cleanup_errors.append('loop: '+str(error))
        if container_attempted:
            try: command.call(['sudo','podman','rm','--force','--ignore',container])
            except Exception as error: cleanup_errors.append('container: '+str(error))
        if not cleanup_errors:
            try: shutil.rmtree(resources)
            except Exception as error: cleanup_errors.append('scratch: '+str(error))
        record['owned_resources_removed'] = not cleanup_errors
        if cleanup_errors:
            record['cleanup_errors'] = cleanup_errors
            raise RuntimeError('owned ESP cleanup failed: '+'; '.join(cleanup_errors))
