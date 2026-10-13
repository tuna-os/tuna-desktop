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

Break ticks run every 2s (BREAK_POLL) and the press helper sleeps
1.5s after pressing, so timing assertions allow a bounded tick
tolerance instead of exact seconds. No weakening: bounds are fixed
here, and missing dumps or states fail closed.
"""
import argparse
import json
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


def check_retired_absent(dump_path, title, gate_start_epoch=0.0):
    """Assert retired-UI-absence only on an alive, error-free owned tree."""
    nodes = load_nodes(dump_path)
    if not nodes:
        raise HarnessError(f'a11y tree dead in {dump_path}: no nodes')
    errors = [n for n in nodes if n.get('role') == 'error']
    if errors:
        raise HarnessError(
            f'a11y tree untrustworthy in {dump_path}: '
            f'{len(errors)} transport-error nodes')
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


def check_stale_press_fails(press_exit_code, before_cstate, after_cstate,
                            action_label, dump_path, title,
                            gate_start_epoch=0.0, due=None):
    assert press_exit_code != 0, \
        f'stale {action_label} press unexpectedly succeeded (retired resident fired)'
    before = load_cstate(before_cstate)
    after = load_cstate(after_cstate)
    assert before.get('locked') == after.get('locked'), \
        f'stale {action_label} press changed lock state'
    wb_before = before.get('wellbeing_effects') or {}
    wb_after = after.get('wellbeing_effects') or {}
    assert wb_before.get('brightness') == wb_after.get('brightness'), \
        f'stale {action_label} press changed fade state'
    if due is not None:
        kind, old, new, expect = due
        if kind == 'delay':
            check_delay(old, new, expect)
        elif kind == 'skip':
            check_skip(old, new, expect)
        else:
            raise HarnessError(f'unknown due kind {kind!r}')
    check_retired_absent(dump_path, title, gate_start_epoch)
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
                                    args.gate_start, due)
            print(f'stale {args.action} press correctly failed')
    except HarnessError as exc:
        print(f'harness error: {exc}', file=sys.stderr)
        return 2
    return 0


if __name__ == '__main__':
    sys.exit(main() or 0)
