#!/usr/bin/env python3
"""Dump one application's AT-SPI accessible tree as JSON (#71).

Usage: tuna-a11y-dump.py APP_NAME OUT.json [TIMEOUT_S] [--press NAME]
                          [--when-showing NAME] [--set NAME VALUE]
                          [--pid PID] [--strict]

With --press, also performs the first action (click) of the first
showing node named NAME, as a screen-reader user would, then dumps.
With --when-showing NAME (requires --press), dumps first and presses in
the same pass only if a node named NAME is showing; otherwise exits 3
without pressing. The dump is the tree the press acted on.
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


def walk(acc, depth, out, strict=False):
    try:
        out.append({
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
        })
        child_count = acc.childCount
        if strict and (type(child_count) is not int or child_count < 0):
            out.append({"role": "error",
                        "name": f"malformed childCount {child_count!r} "
                                f"at depth {depth}",
                        "depth": depth, "showing": False})
        else:
            for i in range(child_count):
                child = acc.getChildAtIndex(i)
                if child is None:
                    if strict:
                        out.append({"role": "error",
                                    "name": f"missing child {i} of "
                                            f"{child_count} "
                                            f"at depth {depth + 1}",
                                    "depth": depth + 1, "showing": False})
                    continue
                walk(child, depth + 1, out, strict)
            if strict:
                end_count = acc.childCount
                if type(end_count) is not int or end_count < 0:
                    out.append({"role": "error",
                                "name": f"malformed childCount {end_count!r} "
                                        f"at depth {depth}",
                                "depth": depth, "showing": False})
                elif end_count != child_count:
                    out.append({"role": "error",
                                "name": f"childCount drifted {child_count} -> "
                                        f"{end_count} at depth {depth}",
                                "depth": depth, "showing": False})
    except Exception as exc:  # a node can die mid-walk; keep the rest
        out.append({"role": "error", "name": str(exc), "depth": depth, "showing": False})


def press(acc, target):
    try:
        # Labels can expose a do-nothing action: press the control.
        if (acc.name == target and acc.getRoleName() != "label"
                and acc.getState().contains(pyatspi.STATE_SHOWING)):
            action = acc.queryAction()
            if action.nActions > 0:
                action.doAction(0)
                return True
        for i in range(acc.childCount):
            child = acc.getChildAtIndex(i)
            if child is not None and press(child, target):
                return True
    except Exception:
        pass
    return False


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
    when = None
    if "--when-showing" in args:
        i = args.index("--when-showing")
        when = args[i + 1]
        del args[i:i + 2]
        if target is None:
            print("tuna-a11y-dump: --when-showing needs --press", file=sys.stderr)
            return 2
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
                if when is not None:
                    nodes = []
                    walk(app, 0, nodes, strict)
                    if expected_pid is not None:
                        nodes[0]["process_id"] = app.get_process_id()
                    with open(path, "w") as fh:
                        json.dump(nodes, fh, indent=1)
                    print(f"tuna-a11y-dump: {len(nodes)} nodes from {name}")
                    if not any(n.get("name") == when and n.get("showing") for n in nodes):
                        print(f"tuna-a11y-dump: {when} not showing; nothing pressed", file=sys.stderr)
                        return 3
                    if not press(app, target):
                        print(f"tuna-a11y-dump: nothing named {target} to press", file=sys.stderr)
                        return 1
                    return 0
                if target is not None:
                    if not press(app, target):
                        print(f"tuna-a11y-dump: nothing named {target} to press", file=sys.stderr)
                        return 1
                    time.sleep(1.5)
                if setting is not None:
                    if not set_value(app, *setting):
                        print(f"tuna-a11y-dump: nothing named {setting[0]} to set", file=sys.stderr)
                        return 1
                    time.sleep(1.5)
                nodes = []
                walk(app, 0, nodes, strict)
                if expected_pid is not None:
                    nodes[0]["process_id"] = app.get_process_id()
                with open(path, "w") as fh:
                    json.dump(nodes, fh, indent=1)
                print(f"tuna-a11y-dump: {len(nodes)} nodes from {name}")
                return 0
        time.sleep(0.5)
    print(f"tuna-a11y-dump: {name} never registered on the a11y bus", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
