"""Fail-closed tray absence observation bound to the admitted GTK publisher.

Exit 0 once a fresh owner-bound tree shows the probe item gone; 2 when it
is still there (or still being torn down) after every observation; 1 when
an observation is refused.
"""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

ITEM = 'Tuna Desktop Tray Probe'
# The proof's deadline: up to 30 fresh observations, 0.5 s apart.
ATTEMPTS = 30
INTERVAL_S = 0.5
# A child AT-SPI unexported while the walk reached it: the item is being
# torn down, not a broken tree.
TEARDOWN_ERROR = 'No such interface'


class TearingDown(Exception):
    """The tree is partial only inside the departing item's subtree."""


def process_identity(pid):
    proc = Path('/proc') / str(pid)
    if proc.stat().st_uid != os.getuid():
        raise ValueError('shell owner uid changed')
    return {'pid': pid,
            'start_ticks': proc.joinpath('stat').read_text().rsplit(')', 1)[1].split()[19],
            'boot_id': Path('/proc/sys/kernel/random/boot_id').read_text().strip(),
            'sha256': hashlib.sha256(proc.joinpath('exe').read_bytes()).hexdigest()}


def teardown_errors(nodes):
    """Error nodes that are teardown races inside the probe item's subtree,
    or None when some error node lies elsewhere or is of another kind."""
    errors, item_depth = [], None
    for node in nodes:
        depth = node.get('depth')
        if item_depth is not None and (not isinstance(depth, int) or depth <= item_depth):
            item_depth = None
        if node.get('role') == 'error':
            if item_depth is None or TEARDOWN_ERROR not in str(node.get('name', '')):
                return None
            errors.append(node)
        elif node.get('name') == ITEM and isinstance(depth, int):
            item_depth = depth
    return errors


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
            or not all(isinstance(node, dict) for node in nodes)):
        raise ValueError('missing, empty or partial accessibility tree')
    races = teardown_errors(nodes)
    if races is None:
        raise ValueError('missing, empty or partial accessibility tree')
    root = nodes[0]
    if (root.get('role') != 'application' or root.get('name') != 'tuna-shell-gtk'
            or root.get('process_id') != identity['pid']
            or not any(node.get('name') == 'Activities' and node.get('showing') is True
                       for node in nodes)):
        raise ValueError('tree lacks admitted shell identity or live panel anchor')
    if races:
        raise TearingDown(races[0]['name'])
    return not any(node.get('name') == ITEM
                   and node.get('showing') is True for node in nodes)


def await_absence(observe, attempts=ATTEMPTS, interval=INTERVAL_S, sleep=None):
    """Observe until the item is absent: None then, else why it stayed
    after the last attempt. Refusals propagate at once."""
    reason = None
    for _ in range(attempts):
        try:
            if observe():
                return None
            reason = 'the item is still showing'
        except TearingDown as race:
            reason = f'the item is still being torn down: {race}'
        (sleep or time.sleep)(interval)
    return reason


def main():
    receipt, output, helper = map(Path, sys.argv[1:4])
    admitted = json.loads(receipt.read_text())
    if admitted.get('complete') is not True:
        raise ValueError('GTK publisher receipt incomplete')
    expected = admitted['publisher']
    reason = await_absence(lambda: observe_absence(output, expected, lambda: subprocess.run(
        [sys.executable, str(helper), 'tuna-shell-gtk', str(output), '20',
         '--pid', str(expected['pid'])], check=False).returncode))
    if reason is None:
        return 0
    print(f'tray absence not observed after {ATTEMPTS} observations: {reason}', file=sys.stderr)
    return 2


if __name__ == '__main__':
    try:
        sys.exit(main())
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(f'tray absence observation refused: {error}', file=sys.stderr)
        sys.exit(1)
