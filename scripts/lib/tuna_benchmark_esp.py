"""Repair composefs ESP bytes from one exact source image, using owned resources."""
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import signal
import tempfile
import time
import uuid
from tuna_benchmark_disk import digest, require


class Commands:
    """Finite owned process groups; files bound output without PIPE deadlocks."""
    def __init__(self, log):
        self.log = log
        self.containers = []
        self.interrupted_build = False
        self.process_cleanup_uncertain = False
        self.events = []

    def _signal(self, process, signum, privileged):
        try:
            os.killpg(process.pid, signum)
        except ProcessLookupError:
            return
        except PermissionError:
            if not privileged: raise
        if privileged:
            # sudo children can change real UID, preventing the caller's signal.
            # The group ID comes solely from this executor's own new session.
            helper = subprocess.Popen(['sudo','-n','/bin/kill','-s',signal.Signals(signum).name,
                                       '--', '-'+str(process.pid)],
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                      start_new_session=True)
            try: helper.wait(timeout=3)
            except subprocess.TimeoutExpired:
                try: os.killpg(helper.pid, signal.SIGKILL)
                except ProcessLookupError: pass
                helper.wait(timeout=3)
                raise RuntimeError('privileged owned-group signal deadline')
            # ESRCH means the group is already gone. Verify independently.
            if signum == signal.SIGKILL:
                try: os.killpg(process.pid, 0)
                except ProcessLookupError: return
                except PermissionError: pass
                if helper.returncode:
                    raise RuntimeError('privileged owned-group kill unconfirmed')

    def _verify_group_stopped(self, group):
        # Linux process state distinguishes a dead, awaiting-adoption grandchild
        # from an executing descendant. We can reap only our direct child.
        deadline = time.monotonic()+1
        while True:
            active = False
            count = 0
            for entry in Path('/proc').iterdir():
                if not entry.name.isdigit(): continue
                count += 1
                require(count <= 65536, 'process group query bound')
                try: fields = (entry/'stat').read_text().rsplit(')',1)[1].split()
                except (FileNotFoundError, ProcessLookupError): continue
                if int(fields[2]) == group and fields[0] != 'Z':
                    active = True; break
            if not active: return
            if time.monotonic() >= deadline:
                raise RuntimeError('owned process group still executing after KILL')
            time.sleep(.02)

    def call(self, args, check=True, timeout=60, max_output=4*1024*1024):
        args = list(args)
        if args[:3] == ['sudo','podman','rm'] or args[:2] == ['sudo','umount'] or '--detach' in args:
            timeout = min(timeout, 30)
        # Podman/conmon can survive their CLI process group. Name every run
        # before launch so even a lost create response has an owned cleanup key.
        if args[:3] == ['sudo', 'podman', 'run']:
            name = 'tuna-export-run-'+uuid.uuid4().hex
            args[3:3] = ['--name', name]
            self.containers.append(name)
        started = time.monotonic()
        failure = None
        with self.log.open('a') as log, tempfile.TemporaryFile() as out, tempfile.TemporaryFile() as err:
            log.write(json.dumps(args)+'\n'); log.flush()
            process = subprocess.Popen(args, stdout=out, stderr=err, start_new_session=True)
            try:
                while process.poll() is None:
                    if time.monotonic()-started >= timeout:
                        failure = 'deadline exceeded'; break
                    if out.tell()+err.tell() > max_output:
                        failure = 'output bound exceeded'; break
                    time.sleep(.02)
                if out.tell()+err.tell() > max_output:
                    failure = failure or 'output bound exceeded'
            finally:
                # Always retire this session's group, including a child whose
                # parent exited successfully. Never signal another session.
                try:
                    self._signal(process, signal.SIGTERM, args[0] == 'sudo')
                    try: process.wait(timeout=2)
                    except subprocess.TimeoutExpired: pass
                    self._signal(process, signal.SIGKILL, args[0] == 'sudo')
                    process.wait(timeout=3)
                    self._verify_group_stopped(process.pid)
                except Exception as error:
                    self.process_cleanup_uncertain = True
                    self.events.append({'command': args, 'deadline_seconds': timeout,
                        'output_limit_bytes': max_output, 'elapsed_seconds': time.monotonic()-started,
                        'returncode': process.returncode, 'failure': failure,
                        'leader_reaped': process.returncode is not None,
                        'group_execution_stopped': False, 'cleanup_error': str(error)})
                    log.write('owned process cleanup failed: '+str(error)+'\n'); log.flush()
                    raise
                out.seek(0); err.seek(0)
                output = out.read(max_output).decode(errors='replace')
                error = err.read(max_output).decode(errors='replace')
                log.write(output); log.write(error); log.flush()
                self.events.append({'command': args, 'deadline_seconds': timeout,
                    'output_limit_bytes': max_output, 'elapsed_seconds': time.monotonic()-started,
                    'returncode': process.returncode, 'failure': failure, 'leader_reaped': True,
                    'group_execution_stopped': True})
            if failure:
                if args[:3] == ['sudo','podman','build']:
                    self.interrupted_build = True
                raise RuntimeError('owned command '+failure+': '+args[0])
        if check and process.returncode:
            raise RuntimeError('command failed: '+args[0])
        return process.returncode, output.strip()

    def cleanup_containers(self):
        errors = []
        for name in self.containers:
            try:
                self.call(['sudo','podman','rm','--force','--ignore',name], timeout=30)
                status, _ = self.call(['sudo','podman','container','exists',name],
                                      check=False, timeout=15)
                require(status == 1, 'owned container absence not confirmed')
            except Exception as error: errors.append(name+': '+str(error))
        if errors: raise RuntimeError('owned run cleanup failed: '+'; '.join(errors))


def retire_backing(command, raw):
    """Reconcile lost loop allocation responses; detach only exact unmounted backing."""
    _, output = command.call(['sudo','losetup','--json','--output','NAME,BACK-FILE'])
    devices = json.loads(output)['loopdevices']
    require(type(devices) is list and len(devices) <= 4096, 'loop query bound')
    for device in devices:
        if device.get('back-file') != str(raw): continue
        loop = device.get('name')
        require(re.fullmatch(r'/dev/loop[0-9]+', loop or ''), 'invalid backing loop identity')
        _, tree = command.call(['sudo','lsblk','--json','--tree','--output','PATH,MOUNTPOINTS',loop])
        def unmounted(nodes):
            require(type(nodes) is list and len(nodes) <= 32, 'loop mount tree bound')
            for node in nodes:
                require(not any(node.get('mountpoints') or []), 'retaining mounted raw backing')
                unmounted(node.get('children', []))
        unmounted(json.loads(tree)['blockdevices'])
        _, current = command.call(['sudo','losetup','--json','--output','NAME,BACK-FILE',loop])
        identities = json.loads(current)['loopdevices']
        require(len(identities) == 1 and identities[0].get('name') == loop
                and identities[0].get('back-file') == str(raw), 'refusing changed backing detach')
        command.call(['sudo','losetup','--detach',loop])
    _, output = command.call(['sudo','losetup','--json','--output','NAME,BACK-FILE'])
    require(not any(item.get('back-file') == str(raw) for item in json.loads(output)['loopdevices']),
            'raw backing loop absence not confirmed')


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
    loop_attempted = False
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
        loop_attempted = True
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
        if loop_attempted and loop is None:
            try: retire_backing(command, raw)
            except Exception as error: cleanup_errors.append('unresolved loop: '+str(error))
        if isinstance(command, Commands) and loop_attempted and not cleanup_errors:
            try: retire_backing(command, raw)
            except Exception as error: cleanup_errors.append('loop absence: '+str(error))
        if container_attempted:
            try:
                command.call(['sudo','podman','rm','--force','--ignore',container])
                status, _ = command.call(['sudo','podman','container','exists',container], check=False)
                require(status == 1, 'source container absence not confirmed')
            except Exception as error: cleanup_errors.append('container: '+str(error))
        if hasattr(command, 'cleanup_containers'):
            try: command.cleanup_containers()
            except Exception as error: cleanup_errors.append('run containers: '+str(error))
        if getattr(command, 'process_cleanup_uncertain', False):
            cleanup_errors.append('owned command process cleanup unconfirmed')
        if hasattr(command, 'events'):
            record['command_deadlines'] = command.events
        if not cleanup_errors:
            try: shutil.rmtree(resources)
            except Exception as error: cleanup_errors.append('scratch: '+str(error))
        record['owned_resources_removed'] = not cleanup_errors
        if cleanup_errors:
            record['cleanup_errors'] = cleanup_errors
            raise RuntimeError('owned ESP cleanup failed: '+'; '.join(cleanup_errors))
