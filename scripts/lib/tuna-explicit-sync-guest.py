#!/usr/bin/python3
"""CI-only guest supervisor for the actual explicit-sync client; root QGA transport.

The original host owns the disposable VM. This helper owns only its one native
client process group, never the compositor/session. Checkpoint acknowledgements
carry the independent host QMP capture digest, not producer pixels.
"""
import base64
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

ROOT = Path('/run/tuna-explicit-sync-proof')
CLIENT = Path('/usr/libexec/tuna-explicit-sync-client')
CASES = ('ordering', 'destroy', 'unmap', 'disconnect', 'surface-exists', 'invalid-timeline',
         'no-surface', 'unsupported-buffer', 'no-buffer', 'no-acquire', 'no-release', 'conflicting-points',
         'failed-dmabuf', 'invalid-dmabuf', 'invalid-dmabuf-destroy-params', 'destroy-sync-shm',
         'legacy-v3', 'mixed-modifiers-v3', 'mixed-modifiers-v5')
CHECKPOINTS = {'red-presented', 'green-blocked', 'green-presented', 'blue-presented'}


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def process(pid):
    path = Path('/proc', str(pid))
    start = int((path / 'stat').read_text().rsplit(')', 1)[1].split()[19])
    return {'pid': pid, 'start_ticks': start, 'uid': path.stat().st_uid,
            'executable': os.readlink(path / 'exe'), 'sha256': digest(path / 'exe')}


def save(path, value):
    temporary = path.with_suffix('.tmp')
    with temporary.open('w') as stream:
        json.dump(value, stream)
    os.chmod(temporary, 0o600)
    temporary.replace(path)


def require_nonce(nonce):
    if re.fullmatch('[0-9a-f]{32}', nonce) is None:
        raise ValueError('exact host-owned nonce required')
    return ROOT / nonce


def run(nonce, compositor_pid, start_ticks, uid, expected_hash):
    directory = require_nonce(nonce)
    ROOT.mkdir(mode=0o711, exist_ok=True)
    directory.mkdir(mode=0o711)
    state_path = directory / 'state.json'
    original = process(compositor_pid)
    if (original['start_ticks'] != start_ticks or original['uid'] != uid or uid == 0
            or original['sha256'] != expected_hash or original['executable'] != '/usr/bin/tuna-compositor'):
        raise RuntimeError('original installed session compositor does not match admission')
    command = Path('/proc', str(compositor_pid), 'cmdline').read_bytes().split(b'\0')
    if not (any(command[i:i + 2] == [b'--backend', b'drm'] for i in range(len(command) - 1))
            or b'--backend=drm' in command):
        raise RuntimeError('native DRM backend required')
    pidfd = os.pidfd_open(compositor_pid)
    executable_stat = Path('/proc', str(compositor_pid), 'exe').stat()
    runtime = Path('/run/user', str(uid))
    sockets = [path for path in runtime.glob('wayland-*') if stat.S_ISSOCK(path.stat().st_mode)]
    devices = list(Path('/dev/dri').glob('renderD*'))
    if len(sockets) != 1 or len(devices) != 1:
        raise RuntimeError('one actual session Wayland socket and DRM render node required')
    account = pwd.getpwuid(uid)
    env = {name: value for name, value in os.environ.items()
           if name not in {'WAYLAND_DISPLAY', 'WAYLAND_SOCKET', 'WAYLAND_DEBUG', 'DISPLAY', 'LIBGL_ALWAYS_SOFTWARE',
                           'GALLIUM_DRIVER', 'LD_PRELOAD', 'LD_LIBRARY_PATH'}}
    env.update(XDG_RUNTIME_DIR=str(runtime), WAYLAND_DISPLAY=sockets[0].name,
               HOME=account.pw_dir, USER=account.pw_name, LOGNAME=account.pw_name)
    state = {'schema': 1, 'nonce': nonce, 'supervisor': process(os.getpid()), 'compositor': original,
             'client_sha256': digest(CLIENT), 'guest_helper_sha256': digest(__file__),
             'device': str(devices[0]), 'complete': False, 'cases': {}, 'checkpoint': None}
    state['protocol_source'] = json.loads(Path('/usr/share/tuna-explicit-sync/protocol-source.json').read_text())
    state['protocol_xml_hash_list_sha256'] = digest('/usr/share/tuna-explicit-sync/protocol-xml.sha256')
    save(state_path, state)
    child = None

    def stop_signal(signum, frame):
        raise InterruptedError('owned native proof interrupted by signal ' + str(signum))

    signal.signal(signal.SIGTERM, stop_signal)
    signal.signal(signal.SIGINT, stop_signal)

    def original_alive():
        path = Path('/proc', str(compositor_pid))
        current_executable = (path / 'exe').stat()
        current_start = int((path / 'stat').read_text().rsplit(')', 1)[1].split()[19])
        if (select.select([pidfd], [], [], 0)[0] or current_start != start_ticks
                or path.stat().st_uid != uid
                or (current_executable.st_dev, current_executable.st_ino)
                   != (executable_stat.st_dev, executable_stat.st_ino)):
            raise RuntimeError('original installed compositor changed/exited')

    def principal():
        os.initgroups(account.pw_name, account.pw_gid)
        os.setgid(account.pw_gid)
        os.setuid(uid)

    try:
        for case in CASES:
            original_alive()
            target = directory / case
            target.mkdir(mode=0o700)
            os.chown(target, uid, account.pw_gid)
            argv = [str(CLIENT), '--device', str(devices[0]), '--artifacts', str(target), '--case', case]
            if case == 'ordering':
                argv.append('--checkpoints')
            rows = []
            with (target / 'stderr.log').open('wb') as error, (target / 'events.jsonl').open('wb') as log:
                child = subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=error,
                                         env=env, preexec_fn=principal, start_new_session=True, bufsize=0)
                child_pidfd = os.pidfd_open(child.pid)
                state['active_client'] = {'pid': child.pid, 'case': case}
                save(state_path, state)
                remaining = b''
                deadline = time.monotonic() + 45
                try:
                    while True:
                        original_alive()
                        left = deadline - time.monotonic()
                        if left <= 0 or not select.select([child.stdout], [], [], left)[0]:
                            raise TimeoutError('native ioctl client deadline: ' + case)
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
                            if row.get('event') == 'wayland-peer' and (row.get('pid') != compositor_pid or row.get('uid') != uid):
                                raise RuntimeError('native consumer connected to a different compositor principal')
                            rows.append(row)
                            if row.get('event') != 'checkpoint':
                                continue
                            name = row.get('name')
                            if name not in CHECKPOINTS:
                                raise RuntimeError('unknown real-client checkpoint')
                            state['checkpoint'] = row
                            save(state_path, state)
                            ack = directory / (name + '.ack')
                            end = time.monotonic() + 4
                            while not ack.exists() and time.monotonic() < end:
                                original_alive()
                                time.sleep(0.01)
                            if not ack.exists():
                                raise TimeoutError('independent host QMP capture acknowledgement')
                            value = json.loads(ack.read_text())
                            if value.get('name') != name or re.fullmatch('[0-9a-f]{64}', value.get('png_sha256', '')) is None:
                                raise RuntimeError('invalid independent capture acknowledgement')
                            state.setdefault('captures', {})[name] = value
                            state['checkpoint'] = None
                            save(state_path, state)
                            child.stdin.write(b'continue\n')
                            child.stdin.flush()
                    if remaining:
                        raise RuntimeError('truncated actual native client event')
                    code = child.wait(timeout=5)
                    if not select.select([child_pidfd], [], [], 0)[0]:
                        raise RuntimeError('original client departure unobserved')
                finally:
                    os.close(child_pidfd)
            child = None
            state['active_client'] = None
            state['cases'][case] = {'exit_code': code, 'event_count': len(rows)}
            save(state_path, state)
            if code != 0 or not any(row == {'event': 'case-pass', 'case': case} for row in rows):
                raise RuntimeError('actual native timeline/dmabuf case failed: ' + case)
        original_alive()
        if digest(Path('/proc', str(compositor_pid), 'exe')) != expected_hash:
            raise RuntimeError('original installed compositor bytes changed')
        if set(state.get('captures', {})) != CHECKPOINTS:
            raise RuntimeError('missing actual QMP pixel captures')
        state['complete'] = True
    except BaseException as error:
        state['failure'] = type(error).__name__ + ': ' + str(error)
        raise
    finally:
        if child is not None:
            if child.poll() is None:
                os.killpg(child.pid, signal.SIGTERM)
                try:
                    child.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    os.killpg(child.pid, signal.SIGKILL)
                    child.wait(timeout=3)
            state['client_cleanup'] = {'pid': child.pid, 'exit_code': child.returncode}
        else:
            state['client_cleanup'] = 'all owned clients exited and were reaped'
        state['active_client'] = None
        state['finished'] = True
        save(state_path, state)
        os.close(pidfd)


def main():
    if os.geteuid() != 0:
        raise RuntimeError('CI guest-agent root principal required')
    action, nonce, *arguments = sys.argv[1:]
    directory = require_nonce(nonce)
    if action == 'run':
        if len(arguments) != 4 or re.fullmatch('[0-9a-f]{64}', arguments[3]) is None:
            raise ValueError('original compositor identity/hash required')
        run(nonce, int(arguments[0]), int(arguments[1]), int(arguments[2]), arguments[3])
        return
    if action == 'state':
        path = directory / 'state.json'
        print(path.read_text() if path.exists() else '{}')
    elif action == 'ack':
        name, sha256 = arguments
        if name not in CHECKPOINTS or re.fullmatch('[0-9a-f]{64}', sha256) is None:
            raise ValueError('fixed real-pixel checkpoint required')
        save(directory / (name + '.ack'), {'name': name, 'png_sha256': sha256})
        print('{}')
    elif action == 'stop':
        state = json.loads((directory / 'state.json').read_text())
        owner = state['supervisor']
        try:
            fd = os.pidfd_open(owner['pid'])
        except ProcessLookupError:
            print('{"departed":true}')
            return
        try:
            if not select.select([fd], [], [], 0)[0]:
                if process(owner['pid']) != owner:
                    raise RuntimeError('original owned supervisor changed')
                signal.pidfd_send_signal(fd, signal.SIGTERM)
            if not select.select([fd], [], [], 5)[0]:
                raise RuntimeError('owned guest client supervisor did not clean up')
        finally:
            os.close(fd)
        print('{"departed":true}')
    elif action == 'file':
        case, name, offset = arguments
        allowed = {'events.jsonl', 'stderr.log', 'default-format-table.bin', 'surface-format-table.bin',
                   'producer-red.ppm', 'producer-green.ppm', 'producer-blue.ppm'}
        if case not in CASES or name not in allowed or not offset.isdecimal():
            raise ValueError('bounded owned artifact required')
        path = directory / case / name
        if not path.exists():
            print('{"missing":true}')
            return
        with path.open('rb') as stream:
            stream.seek(int(offset))
            chunk = stream.read(32768)
        print(json.dumps({'data': base64.b64encode(chunk).decode(), 'size': path.stat().st_size}))
    else:
        raise ValueError('unknown fixed guest proof action')


if __name__ == '__main__':
    main()
