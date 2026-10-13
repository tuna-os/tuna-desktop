#!/usr/bin/env python3
"""Dump one application's AT-SPI accessible tree as JSON (#71).

Usage: tuna-a11y-dump.py APP_NAME OUT.json [TIMEOUT_S] [--press NAME]
                          [--set NAME VALUE] [--pid PID] [--strict]

With --press, establishes ONE absolute monotonic deadline at press
entry from the TIMEOUT_S duration (no wall-clock state feeds it, so
registration-time adjustment cannot extend it), polls pre-invocation
readiness inside it (fresh desktop/app resolve per observation,
clear_cache before every read), then performs the first action
(click) of the first showing non-label node named NAME with at least
one action, exactly ONCE, as a screen-reader user would. Any gap —
None child, query error, malformed count, tripped bound — invalidates
the whole walk; only a None/retired cause may poll again, everything
else fails at once. The actual doAction bool return plus deadline
gates after resolution, readout, post-owner resolve, and final dump
(the success dump also trips mid-walk past the deadline)
plus a fresh unique-owner re-resolve form the verdict: a False
return, an invocation exception, a late readout, an owner change, or
budget expiry all fail with a bounded post-mortem tree plus every
retained condition; there is never a second invocation.
With --set, sets the value of the first showing node named NAME that
has one (a slider), the way assistive technology moves it.
With --pid, only dumps the tree whose application process matches PID
(and stamps the root node with that process_id).

Each node: role, name, description, depth, showing, checked, selected,
focused, and window-relative bounds (null when the node legitimately
lacks a Component implementation).
Waits up to TIMEOUT_S for the app
to register on the accessibility bus. Exit 1 if it never appears.

With --strict, detected traversal inconsistencies fail closed as
error nodes instead of silently dropped subtrees: a None child for an
advertised index, a malformed or drifting childCount, and
state/read/query exceptions each surface an in-band {"role": "error"}
record. Consumers must reject any tree containing an error node.
Detection is best-effort, not atomic completeness: a constant-count
child replacement mid-walk can still escape notice. Only a
NotImplementedError from queryComponent keeps the legitimate
null-bounds behavior.
"""
import json
import re
import sys
import time

import pyatspi


def bounds(acc, strict=False):
    # Wayland cannot expose global surface origins. Window coordinates
    # retain widget allocation; callers add the compositor-known origin.
    try:
        rect = acc.queryComponent().getExtents(pyatspi.WINDOW_COORDS)
        return [rect.x, rect.y, rect.width, rect.height]
    except NotImplementedError:
        # Accessible applications/text nodes need not implement Component.
        return None
    except Exception:
        if strict:
            raise
        return None


def walk_put(out, node, limit, stats):
    """One bounded append: records never exceed `limit` (None disables).
    Returns True appended, False capped (stats['capped'] set, no record)."""
    if limit is not None and len(out) >= limit:
        if stats is not None:
            stats['capped'] = True
        return False
    out.append(node)
    return True


def walk_capped(out, limit, stats):
    if limit is not None and len(out) >= limit:
        if stats is not None:
            stats['capped'] = True
        return True
    return False


def walk(acc, depth, out, strict=False, limit=None, stats=None,
         deadline=None):
    """Tree dump shared by every caller. With limit set (press dumps
    only), traversal is bounded INDEPENDENT of emitted records: typed
    child-count, depth, total visits (nodes + child slots share one
    counter), and an optional monotonic deadline trip the walk with
    stats['capped'] / stats['deadline_tripped'] instead of half-reading
    past the budget. With limit None (all pre-existing callers) every
    new branch is inert and behavior is byte-identical to before."""
    if limit is not None:
        if stats is not None:
            stats['visits'] = stats.get('visits', 0) + 1
            if stats['visits'] > limit:
                stats['capped'] = True
                return
        if depth > PRESS_DEPTH_MAX:
            if stats is not None:
                stats['capped'] = True
            return
        if deadline is not None and time.monotonic() >= deadline:
            if stats is not None:
                stats['deadline_tripped'] = True
            return
    if walk_capped(out, limit, stats):
        return
    try:
        if not walk_put(out, {
            "role": acc.getRoleName(),
            "name": acc.name or "",
            "description": getattr(acc, "description", None) or "",
            "depth": depth,
            "showing": acc.getState().contains(pyatspi.STATE_SHOWING),
            "checked": acc.getState().contains(pyatspi.STATE_CHECKED)
            or acc.getState().contains(pyatspi.STATE_PRESSED),
            "selected": acc.getState().contains(pyatspi.STATE_SELECTED),
            "focused": acc.getState().contains(pyatspi.STATE_FOCUSED),
            "bounds": bounds(acc, strict),
        }, limit, stats):
            return
        child_count = acc.childCount
        if limit is not None and (type(child_count) is not int
                                  or child_count < 0
                                  or child_count > PRESS_CHILDREN_MAX):
            if stats is not None:
                stats['capped'] = True
            return
        if strict and (type(child_count) is not int or child_count < 0):
            walk_put(out, {"role": "error",
                           "name": f"malformed childCount {child_count!r} "
                                   f"at depth {depth}",
                           "depth": depth, "showing": False}, limit, stats)
        else:
            for i in range(child_count):
                if limit is not None and stats is not None:
                    stats['visits'] = stats.get('visits', 0) + 1
                    if stats['visits'] > limit:
                        stats['capped'] = True
                        return
                if walk_capped(out, limit, stats):
                    return
                child = acc.getChildAtIndex(i)
                if child is None:
                    if strict:
                        if not walk_put(out, {"role": "error",
                                             "name": f"missing child {i} of "
                                                     f"{child_count} "
                                                     f"at depth {depth + 1}",
                                             "depth": depth + 1,
                                             "showing": False}, limit, stats):
                            return
                    continue
                walk(child, depth + 1, out, strict, limit, stats,
                     deadline)
            if strict:
                end_count = acc.childCount
                if type(end_count) is not int or end_count < 0:
                    walk_put(out, {"role": "error",
                                   "name": f"malformed childCount {end_count!r} "
                                           f"at depth {depth}",
                                   "depth": depth, "showing": False},
                             limit, stats)
                elif end_count != child_count:
                    walk_put(out, {"role": "error",
                                   "name": f"childCount drifted {child_count} -> "
                                           f"{end_count} at depth {depth}",
                                   "depth": depth, "showing": False},
                             limit, stats)
    except Exception as exc:  # a node can die mid-walk; keep the rest
        walk_put(out, {"role": "error", "name": str(exc), "depth": depth,
                       "showing": False}, limit, stats)


PRESS_SEEN_MAX = 200
PRESS_SEEN_BYTES = 240
PRESS_DEPTH_MAX = 64
PRESS_NODES_MAX = 2048
PRESS_CHILDREN_MAX = 1024


def press_note(seen, text):
    """Bounded diagnostic append: count, bytes, then a single marker."""
    if len(seen) >= PRESS_SEEN_MAX:
        if seen[-1] != '...truncated':
            seen.append('...truncated')
        return
    seen.append(text[:PRESS_SEEN_BYTES])


class _InvalidWalk(Exception):
    """An owned press walk cannot complete honestly. `transient` is True
    ONLY for a None child or the precise retired-object signal — the only
    causes that may poll a fresh observation before invocation. Every
    other cause fails the press immediately with its cause retained."""

    def __init__(self, transient, cause):
        super().__init__(cause)
        self.transient = transient
        self.cause = cause


_GLIB = None
_GLIB_TRIED = False


def _gi_glib():
    global _GLIB, _GLIB_TRIED
    if not _GLIB_TRIED:
        _GLIB_TRIED = True
        try:
            from gi.repository import GLib as _g
            _GLIB = _g
        except Exception:
            _GLIB = None
    return _GLIB


_ATSPI_GONE = re.compile(
    r'Object does not exist at path (?:“/[^”\n\x00]+”|"/[^"\n\x00]+")')


def removed_accessible(error):
    """Only AT-SPI's concrete retired-object error is retryable readiness:
    GLib.Error, domain atspi_error, code 1, exact object-gone message
    (mirrors the 2bc classifier). With gi present the type gate is
    isinstance; the hermetic fallback (gi absent, fake bus) matches the
    same triple by attribute so one code path serves both."""
    glib = _gi_glib()
    if glib is not None and not isinstance(error, glib.Error):
        return False
    return (getattr(error, 'domain', None) == 'atspi_error'
            and getattr(error, 'code', None) == 1
            and _ATSPI_GONE.fullmatch(
                str(getattr(error, 'message', ''))) is not None)


def retired_or_fatal(exc, what):
    """Classify a query failure: retired-signal goes transient, anything
    else is immediately fatal. Always raises _InvalidWalk."""
    if removed_accessible(exc):
        raise _InvalidWalk(True, '%s retired: %s:%r'
                           % (what, type(exc).__name__, exc))
    raise _InvalidWalk(False, '%s err=%s:%r'
                       % (what, type(exc).__name__, exc))


def press_clear(node):
    """Drop cached node data before reading it; a missing clear_cache or
    any failure out of it invalidates the walk (fatal unless the precise
    retired signal)."""
    try:
        clearer = node.clear_cache
    except AttributeError as exc:
        raise _InvalidWalk(False, 'clear_cache missing: %r' % (exc,))
    except Exception as exc:
        retired_or_fatal(exc, 'clear_cache attr')
    try:
        clearer()
    except Exception as exc:
        retired_or_fatal(exc, 'clear_cache')


def press_candidate(acc, target, seen, depth, stats):
    """Pre-invocation readiness probe: find a fully-pressable node WITHOUT
    invoking anything. Returns (node, action, count, name) for the first
    node named `target` that is not a label, is showing, and exposes at
    least one action; otherwise None. Honest unreadiness (label,
    unshowing, zero actions) records one short observation and the walk
    continues to later siblings. ANY gap — a None child, a query error, a
    malformed count, a tripped diagnostic bound — raises _InvalidWalk and
    invalidates the ENTIRE walk: no later candidate is taken from it, and
    only a None/retired cause may poll a fresh observation. Cached data
    is dropped via clear_cache before any node is read."""
    if depth > PRESS_DEPTH_MAX:
        raise _InvalidWalk(False, 'max depth %d' % (PRESS_DEPTH_MAX,))
    stats['nodes'] += 1
    if stats['nodes'] > PRESS_NODES_MAX:
        raise _InvalidWalk(False, 'node budget %d exceeded'
                           % (PRESS_NODES_MAX,))
    press_clear(acc)
    try:
        name = acc.name
    except Exception as exc:
        retired_or_fatal(exc, 'name')
    if name == target:
        try:
            role = acc.getRoleName()
        except Exception as exc:
            retired_or_fatal(exc, 'role')
        try:
            showing = acc.getState().contains(pyatspi.STATE_SHOWING)
        except Exception as exc:
            retired_or_fatal(exc, 'state')
        if role == 'label':
            # Labels can expose a do-nothing action: press the control.
            press_note(seen, 'label (never pressed)')
        elif not showing:
            press_note(seen, 'role=%s unshowing' % (role,))
        else:
            try:
                action = acc.queryAction()
            except NotImplementedError:
                raise _InvalidWalk(
                    False, 'role=%s showing no-action-interface' % (role,))
            except Exception as exc:
                retired_or_fatal(exc, 'action')
            try:
                count = action.nActions
            except Exception as exc:
                retired_or_fatal(exc, 'count')
            if type(count) is not int or count < 0:
                # Malformed action count: the interface disagrees with
                # itself, so the WHOLE walk is invalid (a later sibling
                # must not be invoked past a corrupt readout).
                raise _InvalidWalk(False, 'malformed actions=%r' % (count,))
            if count == 0:
                press_note(seen, 'role=%s showing actions=%r' % (role, count))
            else:
                try:
                    namer = action.getName
                except AttributeError:
                    # The binding itself lacks action names: record that
                    # precise stable fact, keep the verified candidate.
                    press_note(seen, 'action-name unsupported by binding')
                    aname = None
                    return (acc, action, count, aname)
                except Exception as exc:
                    retired_or_fatal(exc, 'action-name attr')
                try:
                    aname = namer(0)
                except Exception as exc:
                    retired_or_fatal(exc, 'action-name')
                return (acc, action, count, aname)
    try:
        child_count = acc.childCount
    except Exception as exc:
        retired_or_fatal(exc, 'childcount')
    if type(child_count) is not int or child_count < 0:
        raise _InvalidWalk(False, 'childcount malformed %r' % (child_count,))
    if child_count > PRESS_CHILDREN_MAX:
        raise _InvalidWalk(False, 'children %d exceeds %d'
                           % (child_count, PRESS_CHILDREN_MAX))
    for i in range(child_count):
        try:
            child = acc.getChildAtIndex(i)
        except Exception as exc:
            retired_or_fatal(exc, 'child %d' % (i,))
        if child is None:
            raise _InvalidWalk(True, 'child %d None' % (i,))
        found = press_candidate(child, target, seen, depth + 1, stats)
        if found is not None:
            return found
    return None


def resolve_named_app(name, expected_pid, seen):
    """Fresh top-level resolve returning ALL fully-verified matches. EVERY
    read is freshness-first: the desktop handle is cleared before its
    childCount is read, and each candidate is cleared before its name
    (then optional typed PID) is read — no cached datum feeds matching.
    Raises _InvalidWalk on any gap (transient only for None children and
    the precise retired signal); every cause is recorded, nothing
    swallowed. Callers enforce typed unique ownership on the list."""
    try:
        desktop = pyatspi.Registry.getDesktop(0)
    except Exception as exc:
        retired_or_fatal(exc, 'desktop')
    press_clear(desktop)
    try:
        top = desktop.childCount
    except Exception as exc:
        retired_or_fatal(exc, 'desktop childcount')
    if type(top) is not int or top < 0:
        raise _InvalidWalk(False, 'desktop childcount malformed %r' % (top,))
    if top > PRESS_CHILDREN_MAX:
        raise _InvalidWalk(False, 'desktop children %d exceeds %d'
                           % (top, PRESS_CHILDREN_MAX))
    matches = []
    for j in range(top):
        try:
            cand = desktop.getChildAtIndex(j)
        except Exception as exc:
            retired_or_fatal(exc, 'desktop child %d' % (j,))
        if cand is None:
            raise _InvalidWalk(True, 'desktop child %d None' % (j,))
        press_clear(cand)
        try:
            same = cand.name == name
        except Exception as exc:
            retired_or_fatal(exc, 'app name')
        if not same:
            continue
        if expected_pid is not None:
            try:
                pid = cand.get_process_id()
            except Exception as exc:
                retired_or_fatal(exc, 'app pid')
            if type(pid) is not int or pid != expected_pid:
                press_note(seen, 'app pid %r not expected %r (skipped)'
                           % (pid, expected_pid))
                continue
        matches.append(cand)
    return matches


def _cap_field(value):
    if isinstance(value, str) and len(value) > PRESS_SEEN_BYTES:
        return value[:PRESS_SEEN_BYTES] + '...truncated'
    return value


def press_fail(target, app_name, app, observations, seen, actions, aname,
               returned, error, invocations, expected_pid, path, strict):
    """Retain everything a failed press observed: the bounded post-mortem
    tree to OUT, and one JSON record with all conditions to stderr. Never
    invokes. `invocations` is the explicit doAction call count (0 means
    readiness never completed); every repr field is length-bounded."""
    try:
        app.clear_cache()
    except Exception as exc:
        press_note(seen, 'postmortem handle stale: %s:%r'
                         % (type(exc).__name__, exc))
    try:
        pid = app.get_process_id()
    except Exception as exc:
        pid = 'err=%s:%r' % (type(exc).__name__, exc)
    nodes = []
    dump_stats = {}
    try:
        walk(app, 0, nodes, strict, PRESS_NODES_MAX, dump_stats)
    except Exception as exc:
        nodes.append({'role': 'error',
                      'name': 'post-mortem walk: %r' % (exc,),
                      'depth': 0, 'showing': False})
    if dump_stats.get('capped'):
        press_note(seen, 'postmortem tree capped at %d nodes'
                         % (PRESS_NODES_MAX,))
    if nodes and expected_pid is not None and type(pid) is int:
        nodes[0]['process_id'] = pid
    with open(path, 'w') as fh:
        json.dump(nodes, fh, indent=1)
    if len(seen) > 50:
        seen = seen[:50] + ['...%d more' % (len(seen) - 50)]
    if isinstance(returned, (bool, int, str)) or returned is None:
        kept_returned = returned
    else:
        kept_returned = repr(returned)
    if isinstance(error, str):
        failed_at = error
    else:
        failed_at = 'non-True action return %r' % (kept_returned,)
    record = {
        'event': 'press-failed', 'target': target, 'app': app_name,
        'pid': _cap_field(pid), 'observations': observations,
        'conditions': seen, 'actions': actions, 'action': _cap_field(aname),
        'returned': _cap_field(kept_returned), 'error': _cap_field(error),
        'invocations': invocations,
        'tree_nodes': len(nodes), 'tree': path,
        'scope': ('checks performed on this attempt: one absolute '
                  'monotonic entry deadline; fresh unique-owner resolve '
                  'per observation; one doAction only after typed '
                  'owner/pre-PID match, with its actual return honored; '
                  'deadline gates after resolution, readout, post-owner, '
                  'and dump; bounded traversal. This record failed at: '
                  '%s. Owner/pid/conditions above are the last observed '
                  'values, not proof of any earlier state; an earlier '
                  'due dump is separate evidence, and cross-dump '
                  'identity belongs to the gate artifacts, never to '
                  'this record.' % (failed_at,)),
    }
    print('tuna-a11y-dump: ' + json.dumps(record), file=sys.stderr)


def set_value(acc, target, value):
    try:
        if acc.name == target and acc.getState().contains(pyatspi.STATE_SHOWING):
            try:
                acc.queryValue().currentValue = value
                return True
            except NotImplementedError:
                pass
        for i in range(acc.childCount):
            child = acc.getChildAtIndex(i)
            if child is not None and set_value(child, target, value):
                return True
    except Exception:
        pass
    return False


def main():
    args = sys.argv[1:]
    expected_pid = None
    strict = "--strict" in args
    if strict:
        args.remove("--strict")
    if "--pid" in args:
        i = args.index("--pid")
        expected_pid = int(args[i + 1])
        del args[i:i + 2]
    target = None
    setting = None
    if "--press" in args:
        i = args.index("--press")
        target = args[i + 1]
        del args[i:i + 2]
    if "--set" in args:
        i = args.index("--set")
        setting = (args[i + 1], float(args[i + 2]))
        del args[i:i + 3]
    name, path = args[0], args[1]
    timeout = float(args[2]) if len(args) > 2 else 20
    end = time.time() + timeout
    while time.time() < end:
        desktop = pyatspi.Registry.getDesktop(0)
        for i in range(desktop.childCount):
            app = desktop.getChildAtIndex(i)
            if app is not None and app.name == name:
                if expected_pid is not None and app.get_process_id() != expected_pid:
                    continue
                if target is not None:
                    # Dedicated press path on ONE absolute monotonic
                    # deadline established AT PRESS ENTRY from the
                    # original duration: no wall-clock state feeds it,
                    # so registration-time adjustment cannot extend it.
                    # Exactly ONE doAction(0) ever runs, and only an
                    # actual True inside the deadline with a stable
                    # unique owner can pass. Deadline gates stand after
                    # candidate resolution, after the readout, after the
                    # post-owner resolve, and after the final dump: async
                    # or blocking readouts may fail past the deadline,
                    # but nothing late is ever accepted.
                    press_end = time.monotonic() + max(0.0, timeout)
                    observations = 0
                    last_seen = []
                    last_fresh = None
                    invoke = None
                    no_budget = ['no press budget']
                    while time.monotonic() < press_end:
                        observations += 1
                        seen = []
                        try:
                            matches = resolve_named_app(name, expected_pid,
                                                        seen)
                        except _InvalidWalk as bad:
                            press_note(seen, bad.cause)
                            last_seen = seen
                            if not bad.transient:
                                press_fail(target, name,
                                           last_fresh
                                           if last_fresh is not None else app,
                                           observations, last_seen, None,
                                           None, None, None, 0, expected_pid,
                                           path, strict)
                                return 1
                            time.sleep(0.5)
                            continue
                        if len(matches) != 1:
                            press_note(seen, 'owner matches: %d (unique '
                                             'required)' % (len(matches),))
                            press_fail(target, name,
                                       last_fresh
                                       if last_fresh is not None else app,
                                       observations, seen, None, None, None,
                                       None, 0, expected_pid, path, strict)
                            return 1
                        last_fresh = matches[0]
                        stats = {'nodes': 0}
                        try:
                            found = press_candidate(last_fresh, target,
                                                    seen, 0, stats)
                        except _InvalidWalk as bad:
                            press_note(seen, bad.cause)
                            last_seen = seen
                            if not bad.transient:
                                press_fail(target, name, last_fresh,
                                           observations, last_seen, None,
                                           None, None, None, 0, expected_pid,
                                           path, strict)
                                return 1
                            time.sleep(0.5)
                            continue
                        last_seen = seen
                        if found is not None:
                            if time.monotonic() >= press_end:
                                press_fail(target, name, app, observations,
                                           last_seen, None, None, None,
                                           'late-ready: candidate past '
                                           'deadline', 0, expected_pid,
                                           path, strict)
                                return 1
                            invoke = (matches[0],) + found
                            break
                        time.sleep(0.5)
                    if invoke is None:
                        if observations == 0:
                            last_seen = no_budget
                        press_fail(target, name,
                                   last_fresh
                                   if last_fresh is not None else app,
                                   observations, last_seen, None, None, None,
                                   None, 0, expected_pid, path, strict)
                        return 1
                    fire_app, _, action, actions, aname = invoke
                    try:
                        pre_pid = fire_app.get_process_id()
                    except Exception as exc:
                        press_fail(target, name, fire_app, observations,
                                   last_seen, actions, aname, None,
                                   'pre-pid err=%s:%r'
                                   % (type(exc).__name__, exc), 0,
                                   expected_pid, path, strict)
                        return 1
                    if type(pre_pid) is not int or pre_pid <= 0:
                        press_fail(target, name, fire_app, observations,
                                   last_seen, actions, aname, None,
                                   'pre-pid non-positive %r' % (pre_pid,), 0,
                                   expected_pid, path, strict)
                        return 1
                    if (expected_pid is not None
                            and pre_pid != expected_pid):
                        press_fail(target, name, fire_app, observations,
                                   last_seen, actions, aname, None,
                                   'pre-pid owner mismatch %r != expected %r'
                                   % (pre_pid, expected_pid), 0,
                                   expected_pid, path, strict)
                        return 1
                    if time.monotonic() >= press_end:
                        press_fail(target, name, fire_app, observations,
                                   last_seen, actions, aname, None,
                                   'late-ready refused: invocation gate '
                                   'past deadline', 0, expected_pid, path,
                                   strict)
                        return 1
                    invocations = 0
                    try:
                        invoked = action.doAction(0)
                        invocations = 1
                    except Exception as exc:
                        press_fail(target, name, fire_app, observations,
                                   last_seen, actions, aname, None,
                                   '%s:%r' % (type(exc).__name__, exc), 1,
                                   expected_pid, path, strict)
                        return 1
                    if invoked is not True:
                        press_fail(target, name, fire_app, observations,
                                   last_seen, actions, aname, invoked, None,
                                   invocations, expected_pid, path, strict)
                        return 1
                    if time.monotonic() >= press_end:
                        press_fail(target, name, fire_app, observations,
                                   last_seen, actions, aname, invoked,
                                   'late-positive refused: readout past '
                                   'deadline', invocations, expected_pid,
                                   path, strict)
                        return 1
                    post_seen = []
                    try:
                        post_matches = resolve_named_app(name, expected_pid,
                                                         post_seen)
                    except _InvalidWalk as bad:
                        press_fail(target, name, fire_app, observations,
                                   last_seen, actions, aname, invoked,
                                   'post-owner resolve: ' + bad.cause,
                                   invocations, expected_pid, path, strict)
                        return 1
                    if len(post_matches) != 1:
                        press_fail(target, name, fire_app, observations,
                                   last_seen, actions, aname, invoked,
                                   'post-owner unique: %d matches'
                                   % (len(post_matches),), invocations,
                                   expected_pid, path, strict)
                        return 1
                    try:
                        post_pid = post_matches[0].get_process_id()
                    except Exception as exc:
                        post_pid = 'err=%s:%r' % (type(exc).__name__, exc)
                    if (type(pre_pid) is not int
                            or type(post_pid) is not int
                            or pre_pid != post_pid):
                        press_fail(target, name, fire_app, observations,
                                   last_seen, actions, aname, invoked,
                                   'owner identity pre=%r post=%r'
                                   % (pre_pid, post_pid), invocations,
                                   expected_pid, path, strict)
                        return 1
                    if time.monotonic() >= press_end:
                        press_fail(target, name, fire_app, observations,
                                   last_seen, actions, aname, invoked,
                                   'late-positive refused: post-owner past '
                                   'deadline', invocations, expected_pid,
                                   path, strict)
                        return 1
                    time.sleep(1.5)
                if setting is not None:
                    if not set_value(app, *setting):
                        print(f"tuna-a11y-dump: nothing named {setting[0]} to set", file=sys.stderr)
                        return 1
                    time.sleep(1.5)
                dump_app = app
                dump_limit = None
                dump_stats = None
                dump_deadline = None
                if target is not None:
                    dump_app = post_matches[0]
                    dump_limit = PRESS_NODES_MAX
                    dump_stats = {}
                    dump_deadline = press_end
                nodes = []
                walk(dump_app, 0, nodes, strict, dump_limit, dump_stats,
                     dump_deadline)
                if expected_pid is not None and nodes:
                    nodes[0]["process_id"] = dump_app.get_process_id()
                with open(path, "w") as fh:
                    json.dump(nodes, fh, indent=1)
                if target is not None and dump_stats.get('deadline_tripped'):
                    press_fail(target, name, dump_app, observations,
                               last_seen, actions, aname, invoked,
                               'success dump deadline tripped; evidence '
                               'incomplete', invocations, expected_pid,
                               path, strict)
                    return 1
                if target is not None and dump_stats.get('capped'):
                    press_fail(target, name, dump_app, observations,
                               last_seen, actions, aname, invoked,
                               'success dump capped at %d nodes; evidence '
                               'incomplete' % (PRESS_NODES_MAX,),
                               invocations, expected_pid, path, strict)
                    return 1
                if target is not None and time.monotonic() >= press_end:
                    press_fail(target, name, dump_app, observations,
                               last_seen, actions, aname, invoked,
                               'late-success refused: dump completed past '
                               'deadline; one True invocation already fired',
                               invocations, expected_pid, path, strict)
                    return 1
                print(f"tuna-a11y-dump: {len(nodes)} nodes from {name}")
                return 0
        time.sleep(0.5)
    print(f"tuna-a11y-dump: {name} never registered on the a11y bus", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
