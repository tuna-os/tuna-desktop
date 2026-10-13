#!/usr/bin/env python3
"""Bind the owned wellbeing probe window to its harness-owned client (#354).

CSTATE windows expose id/app_id/application_window_id (+x11_window_id,
geometry) but NOT pid/start (runtime.rs), and no script-readable
authenticated window/process inventory exposes the kernel client PID, so
PID-from-CSTATE is unprovable here and is never asserted.

Binding is instead exact before/after unique new-map identity plus owned
native-client proof, following scripts/lib/tuna_window_menu_fixture.py:

- bind: the after-scene minus the before-scene id set must be exactly
  ONE new window; it must carry the expected app_id, must be native
  (x11_window_id None), and must be the ONLY same-app native window in
  the after scene. A decoy same-app window (pre-existing or second new
  map) fails closed.
- the harness holds the client child PID; bind records its /proc
  identity (pid, uid, start_ticks, exe link). check re-reads /proc and
  fails on any lifetime change (exit, PID reuse, binary swap).
- the pointer-enter handshake (client logs {"enter":...} on enter) is
  performed shell-side after bind: only the owned client writes that
  log, and an enter at the bound rect proves the compositor routes the
  bound surface to the owned client.

check additionally requires stable owned geometry (rect equality) and
the unchanged native identity pair (id + application_window_id).

Gatefix hardening (issue #354 stays OPEN): the enter handshake is
fresh-only. The shell parks the pointer away (explicit leave),
records the client-log byte offset, moves onto the bound center, and
fresh_enter requires a NEW complete {"enter":...} line strictly after
that offset; pre-seeded old enters never qualify and whole-log
matching is gone. pointer_zone classifies the live pointer against
a rect, rejecting malformed shapes and mistyped or non-finite
coordinates (jq null-ordering would accept [] as outside).
"""
import json
import math
import sys
from pathlib import Path

sys.dont_write_bytecode = True


def process_identity(pid):
    """Live /proc identity of a harness-owned process (no PID reuse)."""
    root = Path('/proc') / str(pid)
    try:
        stat = (root / 'stat').read_text().rsplit(')', 1)[1].split()
        exe = (root / 'exe').readlink().__str__()
    except FileNotFoundError:
        raise RuntimeError(f'probe process {pid} is gone')
    return {'pid': pid, 'uid': root.stat().st_uid,
            'start_ticks': int(stat[19]), 'exe': exe}


def native_same_app(scene, app_id):
    return [w for w in scene.get('windows', [])
            if w.get('app_id') == app_id and w.get('x11_window_id') is None]


def bind(before_scene, after_scene, identity, app_id):
    before_ids = {w.get('id') for w in before_scene.get('windows', [])}
    new = [w for w in after_scene.get('windows', [])
           if w.get('id') not in before_ids]
    if len(new) != 1:
        raise RuntimeError(
            f'probe map is ambiguous or missing ({len(new)} new windows)')
    window = new[0]
    if window.get('app_id') != app_id:
        raise RuntimeError(
            f"new window app_id {window.get('app_id')!r} is not the owned {app_id!r}")
    if window.get('x11_window_id') is not None:
        raise RuntimeError('new probe window is X11, not the owned native client')
    same = native_same_app(after_scene, app_id)
    if len(same) != 1 or same[0].get('id') != window.get('id'):
        raise RuntimeError(
            f'decoy: {len(same)} same-app native windows, owned probe not unique')
    return {**identity, 'id': window['id'],
            'application_window_id': window.get('application_window_id'),
            'app_id': app_id, 'rect': list(window['rect'])}


def check(live_scene, expected, live_identity):
    for key in ('pid', 'uid', 'start_ticks', 'exe'):
        if expected[key] != live_identity[key]:
            raise RuntimeError(
                f'owned probe process lifetime changed ({key}: '
                f'{expected[key]!r} -> {live_identity[key]!r})')
    same = native_same_app(live_scene, expected['app_id'])
    if len(same) != 1:
        raise RuntimeError(
            f"owned probe identity is ambiguous or missing ({len(same)} same-app native windows)")
    window = same[0]
    if (window.get('id') != expected['id']
            or window.get('application_window_id') != expected['application_window_id']):
        raise RuntimeError('owned probe native identity changed')
    if list(window['rect']) != list(expected['rect']):
        raise RuntimeError(
            f"owned probe geometry changed ({expected['rect']!r} -> {window['rect']!r})")
    return list(window['rect'])


def fresh_enter(log_path, offset):
    """A NEW complete enter strictly after the byte offset, or fail.

    Old enters at or before the offset never qualify (a pre-seeded
    log with no fresh enter FAILS); torn or non-JSON lines are
    skipped, and only a complete {"enter": <int serial>} object
    counts.
    """
    if isinstance(offset, bool) or not isinstance(offset, int) or offset < 0:
        raise RuntimeError(f'fresh-enter offset must be a non-negative int, got {offset!r}')
    try:
        with open(log_path, 'rb') as stream:
            at_boundary = True
            if offset:
                stream.seek(offset - 1)
                at_boundary = stream.read(1) == b'\n'
            stream.seek(offset)
            tail = stream.read()
    except FileNotFoundError:
        raise RuntimeError(f'probe log missing: {log_path}')
    except OSError as exc:
        raise RuntimeError(f'probe log unreadable {log_path}: {exc}')
    # A byte offset can bisect an in-flight old record. Its remainder
    # is not a fresh record, even when it happens to be valid JSON.
    if not at_boundary:
        _, delimiter, tail = tail.partition(b'\n')
        if not delimiter:
            tail = b''
    # The producer's newline commits the record. Never qualify a
    # syntactically valid but still unfinished final write.
    tail = tail.rpartition(b'\n')[0]
    try:
        text = tail.decode('utf-8')
    except UnicodeDecodeError as exc:
        raise RuntimeError(f'probe log tail is not UTF-8 after {offset}: {exc}')
    for line in text.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            obj = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(obj, dict) and type(obj.get('enter')) is int:
            return obj
    raise RuntimeError(
        f'no fresh pointer enter strictly after byte offset {offset} in {log_path}')


def pointer_zone(cstate_path, rect):
    """'inside' or 'outside' for a well-formed finite pointer, else fail.

    The pointer must be a present, non-null [x, y] pair of finite
    numbers; missing, null, misshapen, mistyped (bools, strings),
    or non-finite (NaN/Inf) positions are harness failures, never a
    zone. rect is [x, y, w, h] ints with positive width/height; a
    zero- or negative-area rect would classify every position outside.
    """
    if (not isinstance(rect, list) or len(rect) != 4
            or not all(type(v) is int for v in rect)):
        raise RuntimeError(f'zone rect must be 4 ints, got {rect!r:.80}')
    if rect[2] <= 0 or rect[3] <= 0:
        raise RuntimeError(
            f'zone rect width/height must be positive, got {rect!r:.80}')
    try:
        doc = json.loads(Path(cstate_path).read_bytes().decode('utf-8'))
    except FileNotFoundError:
        raise RuntimeError(f'compositor state missing: {cstate_path}')
    except (OSError, UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise RuntimeError(
            f'compositor state unreadable {cstate_path}: {exc}')
    if not isinstance(doc, dict):
        raise RuntimeError(
            f'compositor state {cstate_path} is not an object')
    if 'pointer_position' not in doc or doc['pointer_position'] is None:
        raise RuntimeError(f'{cstate_path}: pointer_position missing or null')
    pos = doc['pointer_position']
    if (not isinstance(pos, list) or len(pos) != 2
            or any(isinstance(v, bool) or not isinstance(v, (int, float))
                   or not math.isfinite(v) for v in pos)):
        raise RuntimeError(
            f'{cstate_path}: pointer_position must be a finite [x, y] '
            f'pair, got {pos!r:.80}')
    x, y, w, h = rect
    px, py = pos
    if px < x or px >= x + w or py < y or py >= y + h:
        return 'outside'
    return 'inside'


def main(argv=None):
    import argparse
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)
    p = sub.add_parser('bind')
    p.add_argument('--before', type=Path, required=True)
    p.add_argument('--after', type=Path, required=True)
    p.add_argument('--pid', type=int, required=True)
    p.add_argument('--app-id', required=True)
    p.add_argument('--out', type=Path, required=True)
    p = sub.add_parser('check')
    p.add_argument('--cstate', type=Path, required=True)
    p.add_argument('--owner', type=Path, required=True)
    p.add_argument('--pid', type=int, required=True)
    p = sub.add_parser('fresh-enter')
    p.add_argument('--log', type=Path, required=True)
    p.add_argument('--offset', type=int, required=True)
    p = sub.add_parser('pointer-zone')
    p.add_argument('--cstate', type=Path, required=True)
    p.add_argument('--rect', required=True)
    args = parser.parse_args(argv)
    if args.command == 'bind':
        before = json.loads(args.before.read_text())
        after = json.loads(args.after.read_text())
        owner = bind(before, after, process_identity(args.pid), args.app_id)
        args.out.write_text(json.dumps(owner, indent=2) + '\n')
        print(json.dumps({'id': owner['id'], 'rect': owner['rect']}))
    elif args.command == 'check':
        expected = json.loads(args.owner.read_text())
        if args.pid != expected['pid']:
            raise SystemExit(
                f"harness pid {args.pid} does not match bound owner {expected['pid']}")
        rect = check(json.loads(args.cstate.read_text()), expected,
                     process_identity(expected['pid']))
        print(json.dumps({'id': expected['id'], 'rect': rect}))
    elif args.command == 'fresh-enter':
        entered = fresh_enter(args.log, args.offset)
        print(json.dumps(entered))
    elif args.command == 'pointer-zone':
        try:
            rect = [int(v) for v in args.rect.split(',')]
        except ValueError:
            raise RuntimeError(
                f'zone rect must be 4 ints, got {args.rect!r:.80}')
        print(pointer_zone(args.cstate, rect))


if __name__ == '__main__':
    main()
