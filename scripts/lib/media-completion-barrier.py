#!/usr/bin/env python3
"""Owned MediaKeys callback/scheduler proof; volume equality never means idle."""
import argparse
import json
from pathlib import Path
import time

PREFIX = 'tuna-media-diagnostic: '
STAGES = {'media-accepted', 'media-complete', 'media-scheduler'}


def owner(pid):
    proc = Path('/proc') / str(pid)
    fields = (proc / 'stat').read_text().rsplit(')', 1)[1].split()
    return {'pid': pid, 'start_ticks': int(fields[19])}


def integer(value, minimum=0):
    return type(value) is int and minimum <= value < 2**64-1


def validate(rows, expected_owner, since, kind, policy):
    if (not isinstance(expected_owner, dict)
            or not integer(expected_owner.get('pid'), 1)
            or not integer(expected_owner.get('start_ticks'), 1)
            or not integer(since) or kind not in {'key', 'clamp', 'idle'}
            or type(policy) is not bool):
        raise ValueError('malformed barrier intent or owner')
    relevant = []
    for row in rows:
        if row.get('stage') not in STAGES:
            continue
        if not integer(row.get('pid'), 1) or not integer(row.get('start_ticks'), 1):
            raise ValueError('malformed completion owner')
        if row.get('pid') != expected_owner['pid']:
            raise ValueError('unrelated shell completion producer')
        if row.get('start_ticks') != expected_owner['start_ticks']:
            raise ValueError('shell lifetime changed')
        if not integer(row.get('monotonic_ns')) or not isinstance(row.get('details'), dict):
            raise ValueError('malformed completion record')
        if row['monotonic_ns'] >= since:
            relevant.append(row)
    if not relevant:
        return None
    instances = {r['details'].get('instance') for r in relevant}
    if len(instances) != 1 or not all(integer(i, 1) for i in instances):
        raise ValueError('ambiguous or malformed MediaKeys instance')
    accepted, completed = {}, {}
    snapshots = []
    last = 0
    for r in relevant:
        if r['monotonic_ns'] < last:
            raise ValueError('out-of-order observer records')
        last = r['monotonic_ns']
        d = r['details']
        if r['stage'] == 'media-accepted':
            token = d.get('token')
            if (not integer(token, 1) or token in accepted or token in completed
                    or d.get('kind') not in {'key', 'clamp'}):
                raise ValueError('invalid/duplicate accepted token')
            accepted[token] = d['kind']
        elif r['stage'] == 'media-complete':
            token = d.get('token')
            if (not integer(token, 1) or token in completed
                    or d.get('kind') not in {'key', 'clamp'}
                    or d.get('outcome') not in {'success', 'failure', 'superseded'}):
                raise ValueError('invalid/duplicate completion token')
            if token in accepted and d.get('kind') != accepted[token]:
                raise ValueError('completion kind changed')
            if d.get('outcome') == 'superseded' and d.get('kind') != 'clamp':
                raise ValueError('key intent cannot be superseded')
            completed[token] = d['outcome']
        else:
            for name in ('accepted', 'failed', 'commands'):
                if not integer(d.get(name)):
                    raise ValueError('malformed scheduler count')
            for name in ('busy', 'clamp_pending', 'command_busy', 'allow_amplified'):
                if type(d.get(name)) is not bool:
                    raise ValueError('malformed scheduler boolean')
            if not isinstance(d.get('pending'), list) or not all(integer(t, 1) for t in d['pending']):
                raise ValueError('malformed scheduler token list')
            snapshots.append(r)
    keys = [token for token, category in accepted.items() if category == 'key']
    clamps = [token for token, category in accepted.items() if category == 'clamp']
    if kind == 'clamp':
        if keys:
            raise ValueError('unexpected key intent during a policy change')
        if not clamps:
            return None
    if kind == 'key' and len(keys) != 1:
        if len(keys) > 1:
            raise ValueError('multiple key intents for one hardware press')
        return None
    if any(outcome == 'failure' for outcome in completed.values()):
        raise ValueError('observed key or clamp completion failed')
    for token in accepted:
        if token not in completed:
            return None
        if completed[token] == 'failure':
            raise ValueError('accepted mutation failed')
    for token, outcome in completed.items():
        if outcome == 'superseded' and not any(t > token and category == 'clamp' and completed.get(t) == 'success' for t, category in accepted.items()):
            return None
    if not snapshots:
        return None
    r = snapshots[-1]; d = r['details']
    if any(r['monotonic_ns'] < c['monotonic_ns'] for c in relevant if c['stage'] == 'media-complete'):
        return None
    if (d['pending'] or d['busy'] or d['clamp_pending'] or d['command_busy'] or d['commands']
            or d['allow_amplified'] is not policy):
        return None
    if d['failed'] > d['accepted'] or (completed and max(completed) > d['accepted']):
        raise ValueError('scheduler highwater predates completion/failure')
    if accepted and d['accepted'] < max(accepted):
        raise ValueError('scheduler predates accepted token')
    if kind == 'key' and (completed.get(keys[0]) != 'success' or d['failed'] >= keys[0]):
        raise ValueError('key terminal completion failed')
    if accepted and d['failed'] >= min(accepted):
        raise ValueError('mutation failed in this barrier')
    return {'owner': expected_owner, 'since': since, 'kind': kind, 'snapshot': r,
            'accepted': accepted, 'completed': completed}


def read_rows(path):
    rows = []
    for line in Path(path).read_text().splitlines():
        if PREFIX not in line:
            continue
        suffix = line.split(PREFIX, 1)[1]
        try:
            row = json.loads(suffix)
        except json.JSONDecodeError as exc:
            raise ValueError('interleaved/malformed observer record') from exc
        if not isinstance(row, dict):
            raise ValueError('observer is not an object')
        rows.append(row)
    return rows


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check-once', action='store_true')
    parser.add_argument('--log', type=Path, required=True)
    parser.add_argument('--pid', type=int, required=True)
    parser.add_argument('--owner', type=Path, required=True)
    parser.add_argument('--since', type=int, required=True)
    parser.add_argument('--kind', choices=('key', 'clamp', 'idle'), required=True)
    parser.add_argument('--policy', choices=('true', 'false'), required=True)
    parser.add_argument('--receipt', type=Path, required=True)
    args = parser.parse_args()
    expected = json.loads(args.owner.read_text())
    # A volume key completes four serialized wpctl subprocesses, each with
    # a five-second production timeout. Allow that chain plus callback
    # dispatch headroom; acceptance/terminal/scheduler assertions stay strict.
    end = time.monotonic() + 25
    while True:
        if owner(args.pid) != expected:
            raise ValueError('owned shell process changed')
        receipt = validate(read_rows(args.log), expected, args.since, args.kind, args.policy == 'true')
        if receipt is not None:
            args.receipt.write_text(json.dumps(receipt, indent=2) + '\n')
            return
        if args.check_once:
            raise SystemExit(1)
        if time.monotonic() >= end:
            raise ValueError('actual callback/scheduler barrier did not complete')
        time.sleep(.05)


if __name__ == '__main__':
    main()
