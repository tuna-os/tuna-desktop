#!/usr/bin/env python3
"""Bounded retirement of the portal fixture shell's active child jobs.

The compositor's graceful exit kills and reaps its GTK-shell child. Wait for
that exit before the fixture relinquishes its bus and finalizes artifact owners.
Never signal a stale PID, a foreign process, or a process group.
"""
import argparse
import json
import os
from pathlib import Path
import select
import signal
import sys
import time


def identity(pid):
    path = Path(f'/proc/{pid}')
    fields = (path / 'stat').read_text().rsplit(')', 1)[1].split()
    return int(fields[1]), fields[19], path.stat().st_uid


def retire(parent, pids, timeout=15):
    handles = []
    try:
        if len(pids) > 64:
            raise RuntimeError('fixture child count exceeds bound')
        # Validate and pin every current job before sending any signal.
        for pid in dict.fromkeys(pids):
            if pid <= 1:
                raise RuntimeError('invalid fixture child PID')
            try:
                before = identity(pid)
                if before[0] != parent or before[2] != os.getuid():
                    raise RuntimeError(f'{pid} is not this fixture shell\'s child')
                fd = os.pidfd_open(pid)
                handles.append((pid, fd))
                if identity(pid) != before:
                    raise RuntimeError(f'{pid} changed identity before retirement')
            except (FileNotFoundError, ProcessLookupError):
                # Bash's jobs snapshot can include a child that just exited.
                continue
        deadline = time.monotonic() + timeout
        # Reverse launch order retires the compositor before its X server.
        for pid, fd in reversed(handles):
            try:
                signal.pidfd_send_signal(fd, signal.SIGTERM)
            except ProcessLookupError:
                continue
            poller = select.poll()
            poller.register(fd, select.POLLIN)
            remaining = max(0, deadline - time.monotonic())
            if not any(events & select.POLLIN for _, events in poller.poll(int(remaining * 1000))):
                raise RuntimeError(f'fixture child {pid} did not retire within {timeout}s')
    finally:
        for _, fd in handles:
            os.close(fd)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('parent', type=int)
    parser.add_argument('pids', type=int, nargs='*')
    args = parser.parse_args()
    if args.parent != os.getppid():
        raise RuntimeError('only the invoking fixture shell may retire its children')
    retire(args.parent, args.pids)
    print(json.dumps({'qualified_retirement': True, 'parent': args.parent,
                      'children': args.pids}))


if __name__ == '__main__':
    try:
        main()
    except (OSError, RuntimeError) as error:
        print(json.dumps({'qualified_retirement': False, 'error': str(error)}))
        print(f'portal fixture retirement failed: {error}', file=sys.stderr)
        sys.exit(1)
