#!/usr/bin/env python3
"""Observe denial of an admitted installed bridge on the public session socket.

This is denial evidence only; the caller must independently prove real typing.
The expected digest comes from the caller's existing package admission receipt.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import time

SOURCE = Path('/usr/bin/tuna-ibus-bridge')
BUDGET_SECONDS = 15
DENIAL = 'tuna-ibus-bridge: the compositor offers no seat or input-method-v2'
CONTEXT = re.compile(r'tuna-ibus-bridge: input context (/[^\s]+)')
GLOBAL = re.compile(r"^interface: '([^']+)', version: [0-9]+, name: [0-9]+\s*$", re.MULTILINE)


def copy_bridge(out, expected, receipt):
    destination = out / 'bridge' / 'tuna-ibus-bridge'
    destination.parent.mkdir(mode=0o700)
    receipt['source'] = {'path': str(SOURCE)}
    receipt['copy'] = {'path': str(destination)}
    fd = os.open(SOURCE, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as source:
        before = os.fstat(source.fileno())
        if not stat.S_ISREG(before.st_mode) or not before.st_mode & 0o111:
            raise ValueError('installed bridge is not an executable regular ELF')
        digest = hashlib.sha256()
        with destination.open('xb') as copied:
            header = source.read(4)
            if header != b'\x7fELF':
                raise ValueError('installed bridge is not ELF')
            digest.update(header)
            copied.write(header)
            while block := source.read(65536):
                digest.update(block)
                copied.write(block)
        identity = lambda s: (s.st_dev, s.st_ino, s.st_mode, s.st_size, s.st_mtime_ns, s.st_ctime_ns)
        if (identity(before) != identity(os.fstat(source.fileno())) or
                identity(before) != identity(SOURCE.stat(follow_symlinks=False))):
            raise ValueError('installed bridge changed during copy')
    receipt['source']['sha256'] = digest.hexdigest()
    copied_digest = hashlib.sha256()
    with destination.open('rb') as copied:
        while block := copied.read(65536):
            copied_digest.update(block)
    receipt['copy']['sha256'] = copied_digest.hexdigest()
    if receipt['source']['sha256'] != expected or receipt['copy']['sha256'] != expected:
        raise ValueError('source/copy does not match already admitted bridge SHA256')
    destination.chmod(0o700)
    return destination


def observe_command(command, env, out, stdout_name, stderr_name, deadline, observation):
    observation['command'] = [str(arg) for arg in command]
    # Files retain actual partial diagnostics even when the shared deadline expires.
    with (out / stdout_name).open('xb') as stdout, (out / stderr_name).open('xb') as stderr:
        try:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise subprocess.TimeoutExpired(command, BUDGET_SECONDS)
            result = subprocess.run(command, env=env, stdin=subprocess.DEVNULL,
                                    stdout=stdout, stderr=stderr, timeout=remaining,
                                    check=False)
            observation['exit_status'] = result.returncode
        except subprocess.TimeoutExpired:
            observation['timed_out'] = True
            raise
        finally:
            observation['stdout'] = (out / stdout_name).read_bytes().decode('utf-8', errors='replace')
            observation['stderr'] = (out / stderr_name).read_bytes().decode('utf-8', errors='replace')
    if time.monotonic() > deadline:
        raise subprocess.TimeoutExpired(command, BUDGET_SECONDS)


def prove(out, display, admitted_sha256):
    out = Path(out).absolute()
    out.mkdir(mode=0o700, parents=True, exist_ok=True)
    owned = out.lstat()
    if not stat.S_ISDIR(owned.st_mode) or owned.st_uid != os.getuid() or owned.st_mode & 0o022:
        raise ValueError('output directory must be owned and not group/world writable')
    # Refuse stale evidence rather than overwriting a previous successful receipt.
    with (out / 'receipt.json').open('x') as receipt_file:
        receipt = {'verified': False, 'scope': 'public-input-method-denial-only',
                   'admitted_bridge_sha256': admitted_sha256, 'wayland_display': display,
                   'uid': os.getuid(), 'observation_budget_seconds': BUDGET_SECONDS}
        started = time.monotonic()
        try:
            if os.getuid() == 0 or os.geteuid() != os.getuid():
                raise ValueError('ordinary nonroot session required')
            if not re.fullmatch(r'[0-9a-f]{64}', admitted_sha256):
                raise ValueError('already admitted bridge SHA256 required')
            if not display or not os.environ.get('XDG_RUNTIME_DIR') or not os.environ.get('DBUS_SESSION_BUS_ADDRESS'):
                raise ValueError('actual public display and existing runtime/session bus required')
            env = os.environ.copy()
            for key in ('WAYLAND_SOCKET', 'DISPLAY', 'IBUS_ADDRESS', 'WAYLAND_DEBUG'):
                env.pop(key, None)
            env['WAYLAND_DISPLAY'] = display
            copied = copy_bridge(out, admitted_sha256, receipt)
            deadline = started + BUDGET_SECONDS
            registry = receipt['registry'] = {}
            observe_command(['/usr/bin/wayland-info'], env, out, 'wayland-info.txt',
                            'wayland-info.stderr', deadline, registry)
            interfaces = set(GLOBAL.findall(registry['stdout']))
            registry['wl_seat'] = 'wl_seat' in interfaces
            registry['zwp_input_method_manager_v2'] = 'zwp_input_method_manager_v2' in interfaces
            if registry['exit_status'] != 0 or not registry['wl_seat'] or registry['zwp_input_method_manager_v2']:
                raise ValueError('public registry must advertise wl_seat and hide input-method-v2')
            helper = receipt['copied_helper'] = {}
            observe_command([str(copied)], env, out, 'copied-helper.stdout',
                            'copied-helper.stderr', deadline, helper)
            lines = helper['stderr'].splitlines()
            contexts = [(index, CONTEXT.fullmatch(line)) for index, line in enumerate(lines)]
            contexts = [(index, match.group(1)) for index, match in contexts if match]
            denials = [index for index, line in enumerate(lines) if line == DENIAL]
            if (helper['exit_status'] != 1 or len(contexts) != 1 or len(denials) != 1 or
                    contexts[0][0] >= denials[0] or denials[0] != len(lines) - 1):
                raise ValueError('copied helper did not exit 1 after input context and exact admission denial')
            helper['input_context'] = contexts[0][1]
            helper['denial'] = DENIAL
            receipt['verified'] = True
        except Exception as error:
            receipt['error'] = str(error)
            for name in ('registry', 'copied_helper'):
                for stream in ('stdout', 'stderr'):
                    diagnostic = receipt.get(name, {}).get(stream, '')
                    if diagnostic:
                        print(name + '.' + stream + ':\n' + diagnostic, file=sys.stderr,
                              end='' if diagnostic.endswith('\n') else '\n')
            raise
        finally:
            receipt['elapsed_seconds'] = time.monotonic() - started
            json.dump(receipt, receipt_file, indent=2, sort_keys=True)
            receipt_file.write('\n')
    return receipt


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--out-dir', required=True)
    parser.add_argument('--wayland-display', required=True)
    parser.add_argument('--bridge-sha256', required=True)
    args = parser.parse_args(argv)
    try:
        receipt = prove(args.out_dir, args.wayland_display, args.bridge_sha256)
    except Exception as error:
        print('tuna-ime-admission-proof: ' + str(error), file=sys.stderr)
        return 1
    print(json.dumps(receipt, sort_keys=True))
    return 0


if __name__ == '__main__':
    sys.exit(main())
