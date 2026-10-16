#!/usr/bin/env python3
"""Bounded fixture observations; never injects input or changes audio/settings."""
import json
import os
from pathlib import Path
import sys
import time

COMMANDS = {'get-volume', 'set-volume', 'set-mute', 'set-default', 'status'}
MARKERS = {'begin', 'amplification-reset-before', 'amplification-reset-after',
           'baseline-written', 'static-disable-before', 'static-disable-after',
           'disabled-key-before', 'disabled-key-after', 'disabled-check'}


def record(root, stage, actor_pid, args):
    root = Path(root)
    if not (root / 'media-diagnostics-enabled').is_file():
        return None
    if stage == 'marker':
        if len(args) != 1 or args[0] not in MARKERS:
            raise ValueError('unknown marker')
    elif stage in {'wpctl-enter', 'wpctl-exit'}:
        if not args or args[0] not in COMMANDS or len(args) > 3:
            raise ValueError('unbounded wpctl invocation')
        for value in args[1:]:
            if value not in {'@DEFAULT_AUDIO_SINK@', '@DEFAULT_AUDIO_SOURCE@', 'toggle'}:
                try:
                    if not float(value) == float(value) or len(value) > 32:
                        raise ValueError('non-finite or unbounded audio value')
                    if abs(float(value)) == float('inf'):
                        raise ValueError('non-finite audio value')
                except ValueError as exc:
                    raise ValueError('unexpected audio argument') from exc
    else:
        raise ValueError('unknown stage')
    proc = Path('/proc') / str(actor_pid)
    identity = {'pid': actor_pid}
    try:
        stat = (proc / 'stat').read_text().rsplit(')', 1)[1].split()
        identity.update(parent_pid=int(stat[1]), start_ticks=int(stat[19]))
        exe = proc / 'exe'
        identity['exe_name'] = Path(os.readlink(exe)).name
        if stage.startswith('wpctl'):
            parent = Path('/proc') / str(identity['parent_pid'])
            pstat = (parent / 'stat').read_text().rsplit(')', 1)[1].split()
            pexe = parent / 'exe'
            identity['parent_start_ticks'] = int(pstat[19])
            name = Path(os.readlink(pexe)).name
            identity['parent_exe_name'] = name if name == 'tuna-shell-gtk' else 'other'
            info = pexe.stat()
            identity['parent_exe_dev_ino'] = [info.st_dev, info.st_ino]
    except OSError:
        identity['identity_unavailable'] = True
    values = {}
    for name in ['volume', 'volume-muted', 'mic-volume', 'mic-muted', 'audio-route', 'default-sink']:
        try:
            with (root / name).open() as value_file:
                values[name] = value_file.read(64).strip()
        except OSError:
            values[name] = None
    row = {'monotonic_ns': time.monotonic_ns(), 'stage': stage,
           'actor': identity, 'args': args, 'fixture': values}
    payload = (json.dumps(row, separators=(',', ':')) + '\n').encode()
    fd = os.open(root / 'media-causal-trace.jsonl', os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o600)
    try:
        if os.write(fd, payload) != len(payload):
            raise OSError('short diagnostic write')
    finally:
        os.close(fd)
    return row


if __name__ == '__main__':
    record(sys.argv[1], sys.argv[2], int(sys.argv[3]), sys.argv[4:])
