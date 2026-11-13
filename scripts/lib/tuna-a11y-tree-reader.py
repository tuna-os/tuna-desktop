#!/usr/bin/env python3
"""Read one application's whole AT-SPI tree again and again (#1077).

Usage: tuna-a11y-tree-reader.py APP_NAME PID STOP OUT.json [NAME...]

Until the file STOP exists, walks every accessible of the application
named APP_NAME whose process is PID, from the application root: each
node's child count, then each child by index, its name and its states.
That is how a screen reader's flat review and the proof's own dumps read
a tree, and the count-then-index pair is what races a window being
hidden (GNOME/gtk#8444: GTK before 4.25 answers a stale root index from
a hidden toplevel's missing object path and crashes).

A node that goes away mid-walk raises on the bus; those are counted, not
fatal. OUT.json gets {"passes", "nodes", "errors", "seen", "lost"}:
"seen" counts, per NAME, the passes in which a showing node had that
name (so a caller can require that the reading overlapped a surface
being up), and "lost" is true when the application left the bus, which
also exits 1.
"""
import json
import os
import sys

import pyatspi


def walk(acc, wanted, found, counters, depth=0):
    counters["nodes"] += 1
    try:
        name = acc.name
        if name in wanted and acc.getState().contains(pyatspi.STATE_SHOWING):
            found.add(name)
        count = acc.childCount
    except Exception:
        counters["errors"] += 1
        return
    if depth >= 64:
        return
    for index in range(count):
        try:
            child = acc.getChildAtIndex(index)
        except Exception:
            counters["errors"] += 1
            continue
        if child is not None:
            walk(child, wanted, found, counters, depth + 1)


def application(name, pid):
    desktop = pyatspi.Registry.getDesktop(0)
    for index in range(desktop.childCount):
        try:
            app = desktop.getChildAtIndex(index)
            if app is not None and app.name == name and app.get_process_id() == pid:
                return app
        except Exception:
            continue
    return None


def main():
    name, pid, stop, out = sys.argv[1], int(sys.argv[2]), sys.argv[3], sys.argv[4]
    wanted = set(sys.argv[5:])
    counters = {"passes": 0, "nodes": 0, "errors": 0}
    seen = {key: 0 for key in sorted(wanted)}
    lost = False
    while not os.path.exists(stop):
        app = application(name, pid)
        if app is None:
            lost = True
            break
        found = set()
        walk(app, wanted, found, counters)
        counters["passes"] += 1
        for key in found:
            seen[key] += 1
    with open(out, "w") as fh:
        json.dump(dict(counters, seen=seen, lost=lost), fh, indent=1)
    print(f"tuna-a11y-tree-reader: {counters['passes']} passes, {counters['nodes']} nodes, "
          f"{counters['errors']} bus errors{', application lost' if lost else ''}")
    return 1 if lost else 0


if __name__ == "__main__":
    sys.exit(main())
