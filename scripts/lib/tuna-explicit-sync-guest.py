#!/usr/bin/python3
"""Finite root actions owning one asynchronous native client at a time.

Only the fixed root router may invoke this CI helper. The original compositor
and VM remain the caller's owners; neither is ever signalled here.
"""
import base64
from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pwd
import re
import select
import signal
import stat
import subprocess
import sys
import time
import traceback
import background_effect_oracle as background

ROOT = Path('/run/tuna-explicit-sync-proof')
CLIENT = Path('/usr/libexec/tuna-explicit-sync-client')
BACKGROUND_CLIENT = Path('/usr/libexec/tuna-background-effect-client')
CASES = ('ordering', 'destroy', 'unmap', 'disconnect', 'surface-exists', 'invalid-timeline',
         'no-surface', 'unsupported-buffer', 'no-buffer', 'no-acquire', 'no-release', 'conflicting-points',
         'failed-dmabuf', 'invalid-dmabuf', 'invalid-dmabuf-destroy-params', 'destroy-sync-shm',
         'legacy-v3', 'mixed-modifiers-v3', 'mixed-modifiers-v5')
CONSUMERS = CASES + ('background-effect',)
CHECKPOINTS = ('red-presented', 'green-blocked', 'green-presented', 'blue-presented')
ARTIFACTS = ('events.jsonl', 'stderr.log', 'default-format-table.bin', 'surface-format-table.bin',
             'producer-red.ppm', 'producer-green.ppm', 'producer-blue.ppm', 'producer-legacy-v3-red.ppm')
SCHEMAS = {'init': {'nonce', 'pid', 'start_ticks', 'uid', 'sha256'},
           'start': {'nonce', 'case'}, 'state': {'nonce'}, 'finish': {'nonce'}, 'cleanup': {'nonce'},
           'ack': {'nonce', 'case', 'attempt', 'name', 'png_sha256'},
           'file': {'nonce', 'case', 'name', 'offset'}}


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def identity(pid):
    path = Path('/proc', str(pid))
    start = int((path / 'stat').read_text().rsplit(')', 1)[1].split()[19])
    executable = (path / 'exe').stat()
    return {'pid': pid, 'start_ticks': start, 'uid': path.stat().st_uid,
            'executable': os.readlink(path / 'exe'),
            'executable_identity': [executable.st_dev, executable.st_ino, executable.st_size,
                                    executable.st_mtime_ns, executable.st_ctime_ns]}


def process(pid):
    original = identity(pid)
    sha256 = digest(Path('/proc', str(pid), 'exe'))
    if identity(pid) != original:
        raise RuntimeError('process source changed during hash observation')
    return dict(original, sha256=sha256)


def same_process(owner):
    # The byte hash is attested once against immutable executable identity.
    # Any inode/size/mtime/ctime change invalidates that attestation. Do not
    # re-read the entire compositor ELF on every 10ms checkpoint poll.
    return identity(owner['pid']) == {key: value for key, value in owner.items() if key != 'sha256'}


def save(path, value):
    temporary = path.with_suffix('.tmp')
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_TRUNC | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'w') as stream:
        json.dump(value, stream)
    temporary.replace(path)


def secure_directory(path, mode):
    info = path.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != 0 or stat.S_IMODE(info.st_mode) != mode:
        raise RuntimeError('proof directory must be root owned with exact permissions')


def require_nonce(nonce):
    if type(nonce) is not str or re.fullmatch('[0-9a-f]{32}', nonce) is None:
        raise ValueError('exact host-owned nonce required')
    return ROOT / nonce


def request(action, raw):
    if action not in SCHEMAS or len(raw.encode()) > 4096:
        raise ValueError('bounded fixed proof action required')
    def unique(pairs):
        value = {}
        for key, item in pairs:
            if key in value:
                raise ValueError('duplicate action key')
            value[key] = item
        return value
    value = json.loads(raw, object_pairs_hook=unique)
    if type(value) is not dict or set(value) != SCHEMAS[action]:
        raise ValueError('exact finite action schema required')
    require_nonce(value['nonce'])
    if 'case' in value and (type(value['case']) is not str or value['case'] not in CONSUMERS):
        raise ValueError('fixed native consumer required')
    if action == 'init':
        if any(type(value[key]) is not int or value[key] <= 0 for key in ('pid', 'start_ticks', 'uid')):
            raise ValueError('original non-root compositor identity required')
    if 'sha256' in value and (type(value['sha256']) is not str or re.fullmatch('[0-9a-f]{64}', value['sha256']) is None):
        raise ValueError('exact compositor hash required')
    if action == 'ack':
        if (value['case'] not in {'ordering', 'background-effect'}
                or type(value['attempt']) is not str or re.fullmatch('[0-9a-f]{32}', value['attempt']) is None
                or type(value['name']) is not str or value['name'] not in
                    (CHECKPOINTS if value['case'] == 'ordering' else tuple(name for _, name in background.STEPS))
                or type(value['png_sha256']) is not str or re.fullmatch('[0-9a-f]{64}', value['png_sha256']) is None):
            raise ValueError('exact independent capture acknowledgement required')
    if action == 'file' and (type(value['name']) is not str or value['name'] not in ARTIFACTS
                             or type(value['offset']) is not int or not 0 <= value['offset'] <= 1048576):
        raise ValueError('bounded owned artifact required')
    return value


@contextmanager
def locked(directory):
    secure_directory(ROOT, 0o711)
    secure_directory(directory, 0o711)
    fd = os.open(directory / 'lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    try:
        fcntl.flock(fd, fcntl.LOCK_EX)
        yield
    finally:
        os.close(fd)


def load(directory):
    return json.loads((directory / 'state.json').read_text())


def backend(pid):
    """Whether the process runs the DRM backend, as the compositor resolves it
    (BackendChoice::resolve): an explicit drm/kms choice, or the default auto
    with no host Wayland/X11 display in its exec-time environment. The
    installed session (tuna-session) passes no --backend, so auto is the
    production launch."""
    command = Path('/proc', str(pid), 'cmdline').read_bytes().split(b'\0')
    choice = b'auto'
    for index, argument in enumerate(command):
        if argument == b'--backend' and index + 1 < len(command):
            choice = command[index + 1]
        elif argument.startswith(b'--backend='):
            choice = argument[len(b'--backend='):]
    if choice in (b'drm', b'kms'):
        return True
    if choice != b'auto':
        return False
    environment = Path('/proc', str(pid), 'environ').read_bytes().split(b'\0')
    return not any(entry.partition(b'=')[0] in (b'WAYLAND_DISPLAY', b'DISPLAY')
                   and entry.partition(b'=')[2] for entry in environment)


def original_alive(state, fd):
    owner = state['compositor']
    if select.select([fd], [], [], 0)[0] or not same_process(owner) or not backend(owner['pid']):
        raise RuntimeError('original installed native compositor changed/exited')


def check_owner(state):
    fd = os.pidfd_open(state['compositor']['pid'])
    try:
        original_alive(state, fd)
    finally:
        os.close(fd)


def initialize(value):
    try:
        ROOT.mkdir(mode=0o711)
    except FileExistsError:
        pass
    else:
        ROOT.chmod(0o711)
    secure_directory(ROOT, 0o711)
    directory = require_nonce(value['nonce'])
    directory.mkdir(mode=0o711)  # Never reuse an old attempt, even after failure.
    directory.chmod(0o711)
    owner = process(value['pid'])
    if (owner['start_ticks'] != value['start_ticks'] or owner['uid'] != value['uid']
            or owner['sha256'] != value['sha256'] or owner['executable'] != '/usr/bin/tuna-compositor'):
        raise RuntimeError('original installed session compositor does not match admission')
    packages = dict(line.split(' ', 1) for line in subprocess.check_output(
        ['pacman', '-Q', 'mesa', 'libdrm', 'wayland', 'tuna-desktop'],
        text=True, timeout=1).splitlines())
    if set(packages) != {'mesa', 'libdrm', 'wayland', 'tuna-desktop'}:
        raise RuntimeError('actual installed native graphics/package facts unavailable')
    protocol_source = Path('/usr/share/tuna-explicit-sync/protocol-source.json').read_bytes()
    state = {'schema': 1, 'nonce': value['nonce'], 'compositor': owner,
             'backend': 'drm', 'client_sha256': digest(CLIENT), 'guest_helper_sha256': digest(__file__),
             'background_client_sha256': digest(BACKGROUND_CLIENT),
             'background_oracle_sha256': digest(background.__file__),
             'protocol_source': json.loads(protocol_source),
             'protocol_source_sha256': hashlib.sha256(protocol_source).hexdigest(),
             'protocol_xml_hash_list_sha256': digest('/usr/share/tuna-explicit-sync/protocol-xml.sha256'),
             'system': {'kernel_release': os.uname().release, 'machine': os.uname().machine,
                        'packages': packages},
             'deadline': time.monotonic() + 180, 'cases': {}, 'captures': {}, 'active': None,
             'checkpoint': None, 'complete': False}
    check_owner(state)
    save(directory / 'state.json', state)
    return state


def update(directory, **changes):
    with locked(directory):
        state = load(directory)
        state.update(changes)
        save(directory / 'state.json', state)
        return state


def worker(directory, case, attempt, gate, ready):
    child = None
    child_fd = None
    owner_fd = None
    result = {'attempt': attempt, 'finished': False, 'client_reaped': False}
    def interrupted(signum, frame):
        raise InterruptedError('owned native worker interrupted')
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    try:
        os.write(ready, b'R')
        os.close(ready)
        if os.read(gate, 1) != b'1':
            raise RuntimeError('native worker ownership handshake failed')
        os.close(gate)
        state = load(directory)
        active = state['active']
        if active['attempt'] != attempt or active['case'] != case or not same_process(active['worker']):
            raise RuntimeError('native worker reservation changed')
        owner = state['compositor']
        owner_fd = os.pidfd_open(owner['pid'])
        deadline = min(state['deadline'], active['deadline'])
        def alive():
            original_alive(state, owner_fd)
            if (child_fd is not None and 'client' in result
                    and not select.select([child_fd], [], [], 0)[0]
                    and not same_process(result['client'])):
                raise RuntimeError('owned native client identity changed')
            if time.monotonic() >= deadline:
                raise TimeoutError('native ioctl case/aggregate deadline: ' + case)
        alive()
        uid = owner['uid']
        account = pwd.getpwuid(uid)
        runtime = Path('/run/user', str(uid))
        # The session compositor listens on its default socket,
        # tuna-nested-<pid> (tuna-session passes no --socket); a display
        # manager's wayland-N is the other shape.
        sockets = sorted({path for pattern in ('wayland-*', 'tuna-nested-' + str(owner['pid']))
                          for path in runtime.glob(pattern) if stat.S_ISSOCK(path.stat().st_mode)})
        devices = list(Path('/dev/dri').glob('renderD*'))
        if len(sockets) != 1:
            raise RuntimeError('one actual session Wayland socket required')
        if len(devices) != 1:
            raise RuntimeError('one DRM render node required')
        client = BACKGROUND_CLIENT if case == 'background-effect' else CLIENT
        client_hash = state['background_client_sha256'] if case == 'background-effect' else state['client_sha256']
        if (digest(client) != client_hash or digest(__file__) != state['guest_helper_sha256']
                or digest(background.__file__) != state['background_oracle_sha256']):
            raise RuntimeError('native helper/client/oracle bytes changed')
        env = {name: value for name, value in os.environ.items()
               if name not in {'WAYLAND_DISPLAY', 'WAYLAND_SOCKET', 'WAYLAND_DEBUG', 'DISPLAY',
                               'LIBGL_ALWAYS_SOFTWARE', 'GALLIUM_DRIVER', 'LD_PRELOAD', 'LD_LIBRARY_PATH'}}
        env.update(XDG_RUNTIME_DIR=str(runtime), WAYLAND_DISPLAY=sockets[0].name,
                   HOME=account.pw_dir, USER=account.pw_name, LOGNAME=account.pw_name)
        def principal():
            os.initgroups(account.pw_name, account.pw_gid)
            os.setgid(account.pw_gid)
            os.setuid(uid)
        target = directory / case
        target.mkdir(mode=0o700)
        os.chown(target, uid, account.pw_gid)
        argv = ([str(client), '--attest-peer'] if case == 'background-effect' else
                [str(client), '--device', str(devices[0]), '--artifacts', str(target), '--case', case])
        if case == 'ordering':
            argv.append('--checkpoints')
        checkpoints = (tuple(name for _, name in background.STEPS) if case == 'background-effect'
                       else CHECKPOINTS if case == 'ordering' else ())
        rows = []
        seen = []
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW
        with os.fdopen(os.open(target / 'stderr.log', flags, 0o600), 'wb') as error, \
                os.fdopen(os.open(target / 'events.jsonl', flags, 0o600), 'wb') as log:
            child = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=error,
                                     env=env, preexec_fn=principal, start_new_session=True, bufsize=0)
            child_fd = os.pidfd_open(child.pid)
            result['client'] = process(child.pid)
            if (result['client']['uid'] != uid or result['client']['executable'] != str(client)
                    or result['client']['sha256'] != client_hash):
                raise RuntimeError('spawned native client does not match exact installed fixture')
            update(directory, active_client=result['client'], device=str(devices[0]))
            remaining = b''
            while True:
                alive()
                if not select.select([child.stdout], [], [], min(0.05, deadline - time.monotonic()))[0]:
                    continue
                chunk = os.read(child.stdout.fileno(), 65536)
                if not chunk:
                    break
                log.write(chunk)
                remaining += chunk
                if len(remaining) > 1048576:
                    raise RuntimeError('oversized native client event')
                while b'\n' in remaining:
                    line, remaining = remaining.split(b'\n', 1)
                    row = json.loads(line)
                    if row.get('event') == 'wayland-peer' and (row.get('pid') != owner['pid'] or row.get('uid') != uid):
                        raise RuntimeError('native consumer connected to different compositor principal')
                    if row.get('event') == 'device':
                        if (row.get('path') != str(devices[0]) or row.get('driver') != 'virtio_gpu'
                                or row.get('timeline') is not True or row.get('eventfd') is not True):
                            raise RuntimeError('actual native device differs from the named virtio profile')
                        result['device'] = row
                    if row.get('event') == 'graphics':
                        if (any(type(row.get(key)) is not str or not row[key] for key in
                                ('egl_vendor', 'egl_version', 'gl_vendor', 'gl_renderer',
                                 'gl_version', 'client_compiler'))
                                or 'virgl' not in row['gl_renderer'].lower()):
                            raise RuntimeError('named native virgl profile requires actual virgl rendering')
                        result['graphics'] = row
                    rows.append(row)
                    if case == 'background-effect' and 'result' in row:
                        index = len(seen)
                        if index >= len(background.STEPS):
                            raise RuntimeError('unexpected extra background consumer receipt')
                        operation, name = background.STEPS[index]
                        if (row.get('result') != ('command-applied' if operation else 'ready')
                                or (operation and (row.get('command') != operation or row.get('sequence') != index))):
                            raise RuntimeError('actual background lifecycle receipt missing or out of order')
                        row = {'event': 'checkpoint', 'name': name}
                        # The client's frame callback may precede the KMS flip;
                        # retain the existing standalone consumer's settle time.
                        time.sleep(0.15)
                        alive()
                    if row.get('event') != 'checkpoint':
                        continue
                    name = row.get('name')
                    if len(seen) >= len(checkpoints) or name != checkpoints[len(seen)]:
                        raise RuntimeError('unexpected or replayed native checkpoint')
                    seen.append(name)
                    checkpoint = dict(row, case=case, attempt=attempt)
                    update(directory, checkpoint=checkpoint)
                    ack = directory / (attempt + '-' + name + '.ack')
                    end = min(deadline, time.monotonic() + 4)
                    while not ack.exists():
                        alive()
                        if time.monotonic() >= end:
                            raise TimeoutError('independent host QMP capture acknowledgement')
                        time.sleep(0.01)
                    value = json.loads(ack.read_text())
                    if (value['case'], value['attempt'], value['name']) != (case, attempt, name):
                        raise RuntimeError('capture acknowledgement ownership changed')
                    with locked(directory):
                        current = load(directory)
                        current['captures'][name] = value
                        current['checkpoint'] = None
                        save(directory / 'state.json', current)
                    if case == 'background-effect':
                        command = background.STEPS[len(seen)][0] if len(seen) < len(background.STEPS) else 'quit'
                        child.stdin.write((command + '\n').encode())
                    else:
                        child.stdin.write(b'continue\n')
                    child.stdin.flush()
            if remaining:
                raise RuntimeError('truncated actual native client event')
            alive()
            code = child.wait(timeout=min(5, max(0, deadline - time.monotonic())))
            result.update(exit_code=code, event_count=len(rows))
            if not select.select([child_fd], [], [], 0)[0]:
                raise RuntimeError('exact native client departure unobserved')
            result['client_reaped'] = True
            alive()
            sync_pass = (any(row == {'event': 'case-pass', 'case': case} for row in rows)
                         and 'device' in result and 'graphics' in result)
            if (code != 0 or not any(row.get('event') == 'wayland-peer' for row in rows)
                    or (case != 'background-effect' and not sync_pass)
                    or tuple(seen) != checkpoints):
                raise RuntimeError('actual native timeline/dmabuf case failed: ' + case)
    except BaseException as error:
        result['failure'] = type(error).__name__ + ': ' + str(error)
    finally:
        # Ignore further cancellation only during bounded exact-client retirement.
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        signal.signal(signal.SIGINT, signal.SIG_IGN)
        try:
            if child is not None:
                if child.poll() is None:
                    if child_fd is None:
                        child_fd = os.pidfd_open(child.pid)
                    signal.pidfd_send_signal(child_fd, signal.SIGTERM)
                    try:
                        child.wait(timeout=2)
                    except subprocess.TimeoutExpired:
                        signal.pidfd_send_signal(child_fd, signal.SIGKILL)
                        child.wait(timeout=2)
                result['client_reaped'] = True
                result['exit_code'] = child.returncode
                if child_fd is not None and not select.select([child_fd], [], [], 0)[0]:
                    raise RuntimeError('owned client departure not proved')
            else:
                result['client_reaped'] = True  # No native client was ever spawned.
        except BaseException as error:
            result['cleanup_failure'] = type(error).__name__ + ': ' + str(error)
        finally:
            if child_fd is not None:
                os.close(child_fd)
            if owner_fd is not None:
                os.close(owner_fd)
        result['finished'] = True
        with locked(directory):
            state = load(directory)
            state['cases'][case] = result
            state['active_client'] = None
            state['checkpoint'] = None
            save(directory / 'state.json', state)


def start(directory, state, case):
    if (state['active'] is not None or state.get('failure') or case in state['cases']
            or len(state['cases']) >= len(CONSUMERS) or case != CONSUMERS[len(state['cases'])]
            or time.monotonic() >= state['deadline']):
        raise RuntimeError('native cases must start once, serially, within aggregate deadline')
    attempt = os.urandom(16).hex()
    state['cases'][case] = {'attempt': attempt, 'finished': False, 'client_reaped': False}
    state['active'] = {'case': case, 'attempt': attempt, 'deadline': min(state['deadline'], time.monotonic() + 45)}
    save(directory / 'state.json', state)
    descriptors = []
    try:
        read_fd, write_fd = os.pipe()
        descriptors.extend((read_fd, write_fd))
        ready_read, ready_write = os.pipe()
        descriptors.extend((ready_read, ready_write))
        pid = os.fork()
    except BaseException as error:
        for fd in descriptors:
            os.close(fd)
        state['cases'][case].update(finished=True, client_reaped=True, worker_departed=True,
                                    failure=type(error).__name__ + ': ' + str(error))
        state['active'] = None
        state['failure'] = 'native worker could not be started'
        save(directory / 'state.json', state)
        raise
    if pid == 0:
        os.close(write_fd)
        os.close(ready_read)
        os.setsid()
        null = os.open('/dev/null', os.O_RDWR)
        for fd in (0, 1, 2):
            os.dup2(null, fd)
        os.close(null)
        # Fork inherited the finite action's flock and QGA descriptors. Keep
        # only stdio and the ownership/readiness gates, never the state lock.
        for name in os.listdir('/proc/self/fd'):
            fd = int(name)
            if fd > 2 and fd not in (read_fd, ready_write):
                try:
                    os.close(fd)
                except OSError:
                    pass
        try:
            worker(directory, case, attempt, read_fd, ready_write)
        finally:
            os._exit(0)
    os.close(read_fd)
    os.close(ready_write)
    try:
        state['active']['worker'] = process(pid)
        save(directory / 'state.json', state)
        if not select.select([ready_read], [], [], 1)[0] or os.read(ready_read, 1) != b'R':
            raise RuntimeError('native worker readiness not observed')
        os.write(write_fd, b'1')
    except BaseException as error:
        # The gate has not been released: no native client can exist yet.
        fd = os.pidfd_open(pid)
        try:
            signal.pidfd_send_signal(fd, signal.SIGKILL)
            os.waitpid(pid, 0)
        finally:
            os.close(fd)
        state['cases'][case].update(finished=True, client_reaped=True, worker_departed=True,
                                    failure=type(error).__name__ + ': ' + str(error))
        state['active'] = None
        state['failure'] = 'native worker ownership handshake failed'
        save(directory / 'state.json', state)
        raise
    finally:
        os.close(write_fd)
        os.close(ready_read)
    return state


def retire(directory, state, cancel):
    active = state['active']
    if active is None:
        return state
    owner = active['worker']
    fd = None
    try:
        try:
            fd = os.pidfd_open(owner['pid'])
        except ProcessLookupError:
            pass
        if fd is not None and not select.select([fd], [], [], 0)[0]:
            if not same_process(owner):
                raise RuntimeError('owned native worker identity changed')
            if cancel:
                signal.pidfd_send_signal(fd, signal.SIGTERM)
            elif not state['cases'][active['case']].get('finished'):
                raise RuntimeError('native worker has not finished')
            # No state lock while waiting: worker needs it to publish reap proof.
            if not select.select([fd], [], [], 5)[0]:
                raise RuntimeError('owned native worker departure unconfirmed')
    finally:
        if fd is not None:
            os.close(fd)
    with locked(directory):
        state = load(directory)
        row = state['cases'][active['case']]
        if not row.get('finished') or not row.get('client_reaped') or row.get('cleanup_failure'):
            raise RuntimeError('owned native client reap proof absent')
        row['worker_departed'] = True
        state['active'] = None
        if cancel or row.get('failure') or row.get('exit_code') != 0:
            state['failure'] = row.get('failure', 'native case cancelled or failed')
        if len(state['cases']) == len(CONSUMERS) and not state.get('failure'):
            check_owner(state)
            if process(state['compositor']['pid']) != state['compositor']:
                raise RuntimeError('original installed compositor bytes changed')
            if set(state['captures']) != set(CHECKPOINTS) | {name for _, name in background.STEPS}:
                raise RuntimeError('missing independent native QMP captures')
            state['complete'] = True
        save(directory / 'state.json', state)
        return state


def artifact(directory, value):
    # The client owns its artifact directory. Neither it nor a symlink may turn
    # this root read into an arbitrary path/file read.
    try:
        fd = os.open(directory / value['case'], os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    except FileNotFoundError:
        return {'missing': True}
    try:
        try:
            file_fd = os.open(value['name'], os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=fd)
        except FileNotFoundError:
            return {'missing': True}
        with os.fdopen(file_fd, 'rb') as stream:
            info = os.fstat(stream.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_size > 1048576 or info.st_nlink != 1:
                raise RuntimeError('owned native artifact exceeds bound or is not a single regular file')
            stream.seek(value['offset'])
            return {'data': base64.b64encode(stream.read(32768)).decode(), 'size': info.st_size}
    finally:
        os.close(fd)


def dispatch(action, value):
    if action == 'init':
        return initialize(value)
    directory = require_nonce(value['nonce'])
    with locked(directory):
        state = load(directory)
        if action not in {'cleanup', 'file'}:
            check_owner(state)
        if action == 'start':
            return start(directory, state, value['case'])
        if action == 'state':
            return state
        if action == 'ack':
            active = state['active']
            checkpoint = state['checkpoint']
            if (active is None or checkpoint is None
                    or any(value[key] != checkpoint[key] for key in ('case', 'attempt', 'name'))
                    or value['attempt'] != active['attempt'] or time.monotonic() >= active['deadline']):
                raise RuntimeError('no matching live native checkpoint')
            path = directory / (value['attempt'] + '-' + value['name'] + '.ack')
            temporary = path.with_suffix('.pending')
            fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
            try:
                with os.fdopen(fd, 'w') as stream:
                    json.dump(value, stream)
                os.link(temporary, path)  # Atomic publication, never overwrite an ACK.
            finally:
                temporary.unlink()
            return {'acknowledged': value['name'], 'attempt': value['attempt']}
        if action == 'file':
            if state['active'] is not None:
                raise RuntimeError('native artifacts require worker retirement')
            return artifact(directory, value)
    return retire(directory, state, action == 'cleanup')


def failure_record(error):
    """Fixed type plus this helper's own (function, line) frames; never
    exception text, argv, paths, subprocess output or locals."""
    oserrors = {'OSError', 'FileNotFoundError', 'PermissionError', 'ProcessLookupError'}
    types = oserrors | {'RuntimeError', 'ValueError', 'TypeError', 'KeyError', 'IndexError',
                        'AttributeError', 'StopIteration', 'JSONDecodeError', 'UnicodeDecodeError',
                        'CalledProcessError', 'TimeoutExpired'}
    kind = type(error).__name__
    result = {'stage': 'unstaged', 'exception_type': kind if kind in types else 'OtherError',
              'code': 'observation-exception'}
    number = getattr(error, 'errno', None)
    if kind in oserrors and type(number) is int and 0 <= number <= 4095:
        result['errno'] = number
    returncode = getattr(error, 'returncode', None)
    if kind == 'CalledProcessError' and type(returncode) is int and -255 <= returncode <= 255:
        result['returncode'] = returncode
    source = failure_record.__code__.co_filename
    result['frames'] = [[frame.name, frame.lineno]
                        for frame in traceback.extract_tb(error.__traceback__)
                        if frame.filename == source and type(frame.lineno) is int
                        and 0 < frame.lineno <= 1000000 and len(frame.name) <= 64
                        and frame.name.isascii() and frame.name.isidentifier()][-16:]
    return result


def main():
    try:
        if os.geteuid() != 0:
            raise RuntimeError('CI guest-agent root principal required')
        if len(sys.argv) != 3:
            raise ValueError('one fixed operation and one bounded JSON argument required')
        action, raw = sys.argv[1:]
        print(json.dumps(dispatch(action, request(action, raw))))
    except Exception as error:
        # One bounded record on stdout for the host's failure diagnostics.
        print(json.dumps({'explicit_sync_error': failure_record(error)}))
        raise SystemExit(1) from None


if __name__ == '__main__':
    main()
