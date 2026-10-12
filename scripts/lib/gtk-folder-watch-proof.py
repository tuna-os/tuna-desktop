#!/usr/bin/env python3
"""Observe a live folder mutation in the owned GTK process, without UI retries."""
import argparse
import hashlib
import json
from pathlib import Path
import time

import gi

gi.require_version("Atspi", "2.0")
from gi.repository import Atspi, GLib


def identity(pid, parent, expected):
    proc = Path(f"/proc/{pid}")
    fields = (proc / "stat").read_text().rsplit(")", 1)[1].split()
    if int(fields[1]) != parent:
        raise RuntimeError("GTK process is not the owned compositor child")
    digest = hashlib.sha256((proc / "exe").read_bytes()).hexdigest()
    if digest != hashlib.sha256(Path(expected).read_bytes()).hexdigest():
        raise RuntimeError("GTK executable does not match admitted proof binary")
    return {"pid": pid, "parent": parent, "start_ticks": fields[19], "exe_sha256": digest,
            "boot_id": Path("/proc/sys/kernel/random/boot_id").read_text().strip()}


def observe(pid):
    desktop = Atspi.get_desktop(0)
    desktop.clear_cache()
    apps = [desktop.get_child_at_index(i) for i in range(desktop.get_child_count())]
    apps = [app for app in apps if app and app.get_process_id() == pid]
    if len(apps) != 1:
        return []
    pending, result = [apps[0]], []
    while pending:
        acc = pending.pop()
        acc.clear_cache()
        result.append({"name": acc.get_name(), "role": acc.get_role_name(),
                       "showing": acc.get_state_set().contains(Atspi.StateType.SHOWING)})
        if len(result) > 20000:
            raise RuntimeError("accessibility tree exceeded proof bound")
        pending.extend(acc.get_child_at_index(i) for i in range(acc.get_child_count()))
        pending = [child for child in pending if child is not None]
    return result


def qualifies(nodes, present, absent):
    names = {n["name"] for n in nodes if n["showing"]}
    return set(present) <= names and not (set(absent) & names)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--pid", type=int, required=True)
    parser.add_argument("--parent", type=int, required=True)
    parser.add_argument("--binary", required=True)
    parser.add_argument("--output", required=True)
    parser.add_argument("--present", action="append", default=[])
    parser.add_argument("--absent", action="append", default=[])
    args = parser.parse_args()
    if not args.present:
        raise RuntimeError("positive observed state is required")
    before = identity(args.pid, args.parent, args.binary)
    end = time.monotonic() + 5
    nodes, error = [], None
    while time.monotonic() < end:
        try:
            nodes = observe(args.pid)
            if qualifies(nodes, args.present, args.absent):
                after = identity(args.pid, args.parent, args.binary)
                if after != before:
                    raise RuntimeError("GTK identity changed during observation")
                Path(args.output).write_text(json.dumps({"identity": before, "nodes": nodes,
                    "present": args.present, "absent": args.absent, "qualified": True}, indent=2))
                return
        except GLib.Error as exc:
            error = str(exc)
        time.sleep(0.05)
    Path(args.output).write_text(json.dumps({"identity": before, "nodes": nodes,
        "present": args.present, "absent": args.absent, "qualified": False, "error": error}, indent=2))
    raise RuntimeError("live folder state did not match within bounded observation")


if __name__ == "__main__":
    main()
