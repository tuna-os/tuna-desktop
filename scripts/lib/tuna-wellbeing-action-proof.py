#!/usr/bin/env python3
"""Wellbeing reachable-action proofs for the nested GTK shell (#354).

Predicates for ACTUAL banner presses (tuna-a11y-dump.py --press):
Take-with-lock observes the session lock; Take-with-fade observes a
faded frame; Delay observes a live due moved by delay-seconds; Skip
observes an interval restart at the press time.

Stale-press is NARROWED (w3-354pixel-proof4f decision, do not expand):
a stale press of a retired resident must FAIL (nothing to press, no
state change), and retired-UI-absence is asserted ONLY with a
mandatory alive owned AT-SPI tree plus zero transport errors. A dead
bus, a timed-out dump, or any transport-error node FAILS the gate;
absence is never inferred from a missing or dead tree. Delay/Skip
stale checks also assert the due schedule the live press set is
undisturbed. Callback-retirement stays explicitly UNQUALIFIED at e2e
(qualified at unit/GTK level elsewhere); no captured-callback
evidence is claimed here.

Gatefix hardening (issue #354 stays OPEN): stale-press snapshots are
presence-first (an actual bool locked plus a complete typed finite
effect state in BOTH docs before any equality; {} vs {} errors), and
the retired dump must carry the stamped root process_id of the live
expected owner (PID/start identity), a gate/attempt-tied observation
stamp (mtime alone never qualifies), and a live showing anchor. The
stamp binds the exact observation (expected gate/nonce/seq) and the
exact dump bytes (dump_sha256 over the actual current file); a
replayed, swapped, or tampered dump never qualifies.

Break ticks run every 2s (BREAK_POLL) and the press helper sleeps
1.5s after pressing, so timing assertions allow a bounded tick
tolerance instead of exact seconds. No weakening: bounds are fixed
here, and missing dumps or states fail closed.
"""
import argparse
import hashlib
import json
import math
import sys
from pathlib import Path

sys.dont_write_bytecode = True

TICK_TOLERANCE_SECS = 5.0
PRESS_SETTLE_SECS = 1.5


class HarnessError(Exception):
    """Malformed/untrustworthy input: broken harness, never a verdict."""


def load_nodes(path):
    try:
        nodes = json.loads(Path(path).read_text())
    except FileNotFoundError:
        raise HarnessError(f'a11y dump missing {path}: bus dead or dump never ran')
    except (OSError, json.JSONDecodeError) as exc:
        raise HarnessError(f'a11y dump unreadable {path}: {exc}')
    if not isinstance(nodes, list):
        raise HarnessError(f'a11y dump {path} is not a node list')
    return nodes


def showing(nodes, name):
    return any(n.get('name') == name and n.get('showing') for n in nodes)


def banner_present(a11y_path, title):
    nodes = load_nodes(a11y_path)
    assert showing(nodes, title), f'no showing banner named {title!r} in {a11y_path}'
    return True


def banner_absent(a11y_path, title):
    nodes = load_nodes(a11y_path)
    assert not showing(nodes, title), f'banner {title!r} still showing in {a11y_path}'
    return True


def proc_start_ticks(pid, label):
    """Live /proc start-time of an owned process (no PID reuse, no dead PID)."""
    if type(pid) is not int or pid <= 0:
        raise HarnessError(f'{label}: owner pid must be a positive int, got {pid!r}')
    root = Path('/proc') / str(pid)
    try:
        stat = (root / 'stat').read_text().rsplit(')', 1)[1].split()
    except FileNotFoundError:
        raise HarnessError(f'{label}: owned process {pid} is gone (dead PID)')
    except OSError as exc:
        raise HarnessError(f'{label}: cannot read /proc/{pid}: {exc}')
    try:
        return int(stat[19])
    except (IndexError, ValueError):
        raise HarnessError(f'{label}: /proc/{pid}/stat has no start time')


def load_stamp(stamp_path, label):
    try:
        stamp = json.loads(Path(stamp_path).read_text())
    except FileNotFoundError:
        raise HarnessError(f'{label}: observation stamp missing: {stamp_path}')
    except (OSError, json.JSONDecodeError) as exc:
        raise HarnessError(f'{label}: observation stamp unreadable {stamp_path}: {exc}')
    if not isinstance(stamp, dict):
        raise HarnessError(f'{label}: observation stamp {stamp_path} is not an object')
    return stamp


def check_retired_absent(dump_path, title, gate_start_epoch=0.0, *,
                         expected_pid, expected_start_ticks,
                         stamp_path, expected_gate, expected_nonce,
                         expected_seq):
    """Assert retired-UI-absence only on a live, error-free OWNED tree.

    The dump must carry the stamped root process_id of the expected
    owner, that PID must be alive with the expected start time (no
    dead PID, no PID reuse), a gate/attempt-tied observation stamp
    must bind this exact dump to this gate (mtime alone never
    qualifies), and a live showing anchor must prove the tree is the
    true owner's, not an empty or replayed husk. The stamp's
    gate/nonce/seq must equal the expected observation, and its
    dump_sha256 must equal the hash of the actual current dump bytes.
    """
    if type(expected_pid) is not int or expected_pid <= 0:
        raise HarnessError(f'expected owner pid must be a positive int, got {expected_pid!r}')
    if type(expected_start_ticks) is not int or expected_start_ticks <= 0:
        raise HarnessError(
            f'expected owner start time must be a positive int, got {expected_start_ticks!r}')
    if not expected_gate or not expected_nonce:
        raise HarnessError('expected gate and nonce are required to qualify a retired dump')
    if type(expected_seq) is not int or expected_seq <= 0:
        raise HarnessError(
            f'expected observation seq must be a positive int, got {expected_seq!r}')
    nodes = load_nodes(dump_path)
    if not nodes:
        raise HarnessError(f'a11y tree dead in {dump_path}: no nodes')
    errors = [n for n in nodes if n.get('role') == 'error']
    if errors:
        raise HarnessError(
            f'a11y tree untrustworthy in {dump_path}: '
            f'{len(errors)} transport-error nodes')
    root_pid = nodes[0].get('process_id') if isinstance(nodes[0], dict) else None
    if type(root_pid) is not int or root_pid != expected_pid:
        raise HarnessError(
            f'a11y tree in {dump_path} is not stamped to the owned shell '
            f'(root process_id {root_pid!r} != owner {expected_pid})')
    live_start = proc_start_ticks(expected_pid, str(dump_path))
    if live_start != expected_start_ticks:
        raise HarnessError(
            f'owned shell PID {expected_pid} was replaced '
            f'(start {expected_start_ticks} -> {live_start})')
    stamp = load_stamp(stamp_path, str(dump_path))
    for key in ('gate', 'nonce', 'seq', 'pid', 'start_ticks', 'epoch',
                'dump_sha256'):
        if key not in stamp:
            raise HarnessError(f'{stamp_path}: observation stamp missing {key!r}')
    if stamp['gate'] != expected_gate or stamp['nonce'] != expected_nonce:
        raise HarnessError(
            f'{stamp_path}: observation stamp is not for this gate/attempt '
            f'(got {stamp["gate"]!r}/{stamp["nonce"]!r})')
    if type(stamp['seq']) is not int or stamp['seq'] != expected_seq:
        raise HarnessError(
            f'{stamp_path}: observation stamp is not for observation '
            f'{expected_seq} (got {stamp["seq"]!r})')
    try:
        dump_bytes = Path(dump_path).read_bytes()
    except OSError as exc:
        raise HarnessError(f'{dump_path}: retired dump unreadable for binding: {exc}')
    if not isinstance(stamp['dump_sha256'], str) \
            or stamp['dump_sha256'] != hashlib.sha256(dump_bytes).hexdigest():
        raise HarnessError(
            f'{stamp_path}: observation stamp does not bind the actual '
            f'current dump bytes of {dump_path} (replayed, swapped, or tampered)')
    if type(stamp['pid']) is not int or stamp['pid'] != expected_pid:
        raise HarnessError(
            f'{stamp_path}: observation stamp is not for the owned shell PID')
    if type(stamp['start_ticks']) is not int \
            or stamp['start_ticks'] != expected_start_ticks:
        raise HarnessError(
            f'{stamp_path}: observation stamp is not for the owned shell lifetime')
    epoch = stamp['epoch']
    if isinstance(epoch, bool) or not isinstance(epoch, (int, float)) \
            or not math.isfinite(epoch) or epoch < gate_start_epoch:
        raise HarnessError(
            f'{stamp_path}: observation stamp is stale or predates the gate start')
    if not any(n.get('showing') for n in nodes if isinstance(n, dict)):
        raise HarnessError(
            f'a11y tree in {dump_path} has no live showing anchor: '
            f'not the true owner tree')
    mtime = Path(dump_path).stat().st_mtime
    assert mtime + 1 >= gate_start_epoch, \
        f'stale a11y dump {dump_path} (mtime {mtime} < gate {gate_start_epoch})'
    assert not showing(nodes, title), \
        f'retired banner {title!r} still showing in {dump_path}'
    return True


def load_cstate(path):
    try:
        doc = json.loads(Path(path).read_text())
    except FileNotFoundError:
        raise HarnessError(f'cstate missing: {path}')
    except (OSError, json.JSONDecodeError) as exc:
        raise HarnessError(f'cstate unreadable {path}: {exc}')
    if not isinstance(doc, dict):
        raise HarnessError(f'cstate {path} is not an object')
    return doc


def check_locked(cstate_path, expect_locked=True):
    doc = load_cstate(cstate_path)
    assert doc.get('locked') is expect_locked, (cstate_path, doc.get('locked'), expect_locked)
    return doc


def check_fade_sample(cstate_path, expect_faded=True):
    doc = load_cstate(cstate_path)
    wb = doc.get('wellbeing_effects')
    if not isinstance(wb, dict) or 'brightness' not in wb:
        raise HarnessError(f'{cstate_path}: wellbeing_effects.brightness missing')
    if expect_faded:
        assert wb['brightness'] < 1.0 - 1e-9, (cstate_path, wb['brightness'])
    else:
        assert abs(wb['brightness'] - 1.0) <= 1e-3, (cstate_path, wb['brightness'])
    return doc, wb


def check_delay(old_due_epoch, new_due_epoch, delay_secs,
                tolerance=TICK_TOLERANCE_SECS):
    low = delay_secs - tolerance
    high = delay_secs + tolerance
    delta = new_due_epoch - old_due_epoch
    assert low <= delta <= high, \
        f'Delay must move live due by delay-seconds ({delay_secs}s): got {delta:.1f}s'
    return delta


def check_skip(press_epoch, new_due_epoch, interval_secs,
               tolerance=TICK_TOLERANCE_SECS):
    low = interval_secs - tolerance
    high = interval_secs + tolerance
    delta = new_due_epoch - press_epoch
    assert low <= delta <= high, \
        f'Skip must restart the interval at press ({interval_secs}s): got {delta:.1f}s'
    return delta


def require_effect_state(doc, label):
    """Presence-first typed effect state: bool locked + complete finite wb.

    Both stale-press snapshots must actually carry the state before any
    equality is meaningful; missing or mistyped fields are harness
    errors, never equal-by-absence ({} vs {} must error, not pass).
    """
    locked = doc.get('locked')
    if type(locked) is not bool:
        raise HarnessError(f'{label}: locked must be an actual bool, got {locked!r}')
    wb = doc.get('wellbeing_effects')
    if not isinstance(wb, dict):
        raise HarnessError(f'{label}: wellbeing_effects must be an object, got {wb!r:.60}')
    for key in ('desaturation', 'brightness'):
        value = wb.get(key)
        if isinstance(value, bool) or not isinstance(value, (int, float)) \
                or not math.isfinite(value):
            raise HarnessError(
                f'{label}: wellbeing_effects.{key} must be a finite number, '
                f'got {value!r}')
    if type(wb.get('animating')) is not bool:
        raise HarnessError(
            f"{label}: wellbeing_effects.animating must be an actual bool, "
            f'got {wb.get("animating")!r}')
    return locked, wb


def check_stale_press_fails(press_exit_code, before_cstate, after_cstate,
                            action_label, dump_path, title,
                            gate_start_epoch=0.0, due=None, *,
                            expected_pid, expected_start_ticks,
                            stamp_path, expected_gate, expected_nonce,
                            expected_seq):
    assert press_exit_code != 0, \
        f'stale {action_label} press unexpectedly succeeded (retired resident fired)'
    before = load_cstate(before_cstate)
    after = load_cstate(after_cstate)
    locked_before, wb_before = require_effect_state(before, str(before_cstate))
    locked_after, wb_after = require_effect_state(after, str(after_cstate))
    assert locked_before == locked_after, \
        f'stale {action_label} press changed lock state'
    assert wb_before['brightness'] == wb_after['brightness'], \
        f'stale {action_label} press changed fade state'
    if due is not None:
        kind, old, new, expect = due
        if kind == 'delay':
            check_delay(old, new, expect)
        elif kind == 'skip':
            check_skip(old, new, expect)
        else:
            raise HarnessError(f'unknown due kind {kind!r}')
    check_retired_absent(dump_path, title, gate_start_epoch,
                         expected_pid=expected_pid,
                         expected_start_ticks=expected_start_ticks,
                         stamp_path=stamp_path, expected_gate=expected_gate,
                         expected_nonce=expected_nonce,
                         expected_seq=expected_seq)
    return True


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)

    p = sub.add_parser('banner-present')
    p.add_argument('a11y', type=Path)
    p.add_argument('title')

    p = sub.add_parser('banner-absent')
    p.add_argument('a11y', type=Path)
    p.add_argument('title')

    p = sub.add_parser('locked')
    p.add_argument('cstate', type=Path)
    p.add_argument('--expect', choices=('true', 'false'), default='true')

    p = sub.add_parser('fade-sample')
    p.add_argument('cstate', type=Path)
    p.add_argument('--expect', choices=('faded', 'neutral'), default='faded')

    p = sub.add_parser('delay')
    p.add_argument('--old-due', type=float, required=True)
    p.add_argument('--new-due', type=float, required=True)
    p.add_argument('--delay-secs', type=float, required=True)

    p = sub.add_parser('skip')
    p.add_argument('--press', type=float, required=True)
    p.add_argument('--new-due', type=float, required=True)
    p.add_argument('--interval-secs', type=float, required=True)

    p = sub.add_parser('stale-press')
    p.add_argument('--exit-code', type=int, required=True)
    p.add_argument('--before', type=Path, required=True)
    p.add_argument('--after', type=Path, required=True)
    p.add_argument('--action', required=True)
    p.add_argument('--dump', type=Path, required=True)
    p.add_argument('--title', required=True)
    p.add_argument('--gate-start', type=float, default=0.0)
    p.add_argument('--expect-pid', type=int, required=True)
    p.add_argument('--expect-start', type=int, required=True)
    p.add_argument('--stamp', type=Path, required=True)
    p.add_argument('--expect-gate', required=True)
    p.add_argument('--expect-nonce', required=True)
    p.add_argument('--expect-seq', type=int, required=True)
    p.add_argument('--due-kind', choices=('delay', 'skip'), default=None)
    p.add_argument('--due-old', type=float, default=None)
    p.add_argument('--due-new', type=float, default=None)
    p.add_argument('--due-expect', type=float, default=None)

    args = parser.parse_args(argv)
    try:
        if args.command == 'banner-present':
            banner_present(args.a11y, args.title)
            print(f'banner showing: {args.title}')
        elif args.command == 'banner-absent':
            banner_absent(args.a11y, args.title)
            print(f'banner absent: {args.title}')
        elif args.command == 'locked':
            check_locked(args.cstate, args.expect == 'true')
            print(f'locked == {args.expect}')
        elif args.command == 'fade-sample':
            _, wb = check_fade_sample(args.cstate, args.expect == 'faded')
            print(json.dumps(wb))
        elif args.command == 'delay':
            delta = check_delay(args.old_due, args.new_due, args.delay_secs)
            print(f'delay delta {delta:.1f}s for delay-secs {args.delay_secs:g}')
        elif args.command == 'skip':
            delta = check_skip(args.press, args.new_due, args.interval_secs)
            print(f'skip delta {delta:.1f}s for interval {args.interval_secs:g}')
        elif args.command == 'stale-press':
            due_args = [args.due_kind, args.due_old, args.due_new, args.due_expect]
            if any(v is not None for v in due_args):
                if not all(v is not None for v in due_args):
                    raise HarnessError(
                        'due check needs --due-kind, --due-old, --due-new and --due-expect together')
                due = (args.due_kind, args.due_old, args.due_new, args.due_expect)
            else:
                due = None
            check_stale_press_fails(args.exit_code, args.before, args.after,
                                    args.action, args.dump, args.title,
                                    args.gate_start, due,
                                    expected_pid=args.expect_pid,
                                    expected_start_ticks=args.expect_start,
                                    stamp_path=args.stamp,
                                    expected_gate=args.expect_gate,
                                    expected_nonce=args.expect_nonce,
                                    expected_seq=args.expect_seq)
            print(f'stale {args.action} press correctly failed')
    except HarnessError as exc:
        print(f'harness error: {exc}', file=sys.stderr)
        return 2
    return 0


if __name__ == '__main__':
    sys.exit(main() or 0)
