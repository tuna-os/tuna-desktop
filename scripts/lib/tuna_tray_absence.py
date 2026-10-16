"""Fail-closed tray absence observation bound to the admitted GTK publisher."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys


def process_identity(pid):
    proc = Path('/proc') / str(pid)
    if proc.stat().st_uid != os.getuid():
        raise ValueError('shell owner uid changed')
    return {'pid': pid,
            'start_ticks': proc.joinpath('stat').read_text().rsplit(')', 1)[1].split()[19],
            'boot_id': Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
            'sha256': hashlib.sha256(proc.joinpath('exe').read_bytes()).hexdigest()}


def observe_absence(output, expected, dump):
    # Delete old data even if identity admission or the dump subsequently fails.
    output.unlink(missing_ok=True)
    keys = ('pid', 'start_ticks', 'boot_id', 'sha256')
    identity = {key: expected[key] for key in keys}
    if process_identity(identity['pid']) != identity:
        raise ValueError('admitted shell lifetime changed before dump')
    if dump() != 0:
        raise ValueError('fresh owner-bound dump failed')
    if process_identity(identity['pid']) != identity:
        raise ValueError('admitted shell lifetime changed during dump')
    nodes = json.loads(output.read_text())
    if (not isinstance(nodes, list) or not nodes
            or not all(isinstance(node, dict) for node in nodes)
            or any(node.get('role') == 'error' for node in nodes)):
        raise ValueError('missing, empty or partial accessibility tree')
    root = nodes[0]
    if (root.get('role') != 'application' or root.get('name') != 'tuna-shell-gtk'
            or root.get('process_id') != identity['pid']
            or not any(node.get('name') == 'Activities' and node.get('showing') is True
                       for node in nodes)):
        raise ValueError('tree lacks admitted shell identity or live panel anchor')
    return not any(node.get('name') == 'Tuna Desktop Tray Probe'
                   and node.get('showing') is True for node in nodes)


def main():
    receipt, output, helper = map(Path, sys.argv[1:4])
    admitted = json.loads(receipt.read_text())
    if admitted.get('complete') is not True:
        raise ValueError('GTK publisher receipt incomplete')
    expected = admitted['publisher']
    absent = observe_absence(output, expected, lambda: subprocess.run(
        [sys.executable, str(helper), 'tuna-shell-gtk', str(output), '20',
         '--pid', str(expected['pid'])], check=False).returncode)
    return 0 if absent else 2


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f'tray absence observation refused: {error}', file=sys.stderr)
        sys.exit(1)
