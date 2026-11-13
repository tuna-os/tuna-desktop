#!/usr/bin/env python3
"""Count the resources a session process holds, for leak gates (#1084).

    tuna-resource-sample.py sample NAME=PID [NAME=PID ...]
    tuna-resource-sample.py compare BEFORE.json AFTER.json KEY=ALLOWED ...
    tuna-resource-sample.py diff BEFORE.json AFTER.json

`sample` prints one JSON object: for each named process its open
descriptors (total and by kind of target), its shared-memory mappings by
name (`memfd:gdk-wayland` is one GDK shm buffer) and its anonymous
proportional set size.

`compare` exits 1 when a counter grew by more than its allowance between
two samples. KEY is NAME.COUNTER, where COUNTER is `fds`, `pss_anon_kib`
or `shm:<mapping name>` (the number of mappings of that name).

`diff` prints every counter that differs between two samples, one per
line (what a failed `compare` leaves to explain).
"""
import json
import os
import pathlib
import re
import sys


def fd_kind(target):
    """Group a /proc/PID/fd link target so that counts compare across samples."""
    if target.startswith("/memfd:"):
        return "memfd:" + target[len("/memfd:"):].removesuffix(" (deleted)")
    if target.startswith(("socket:", "pipe:", "anon_inode:")):
        return re.sub(r":\[?\d+\]?$", "", target)
    if target.startswith("/dev/"):
        return target
    if target.endswith(" (deleted)"):
        return "deleted file"
    return "file"


def shm_name(path):
    """The name of a shared-memory mapping in /proc/PID/maps, or None."""
    if path.startswith("/memfd:"):
        return "memfd:" + path[len("/memfd:"):].removesuffix(" (deleted)")
    if path.startswith("/dev/shm/"):
        return "shm:" + path.removesuffix(" (deleted)")
    return None


def sample_process(pid):
    proc = pathlib.Path("/proc") / str(pid)
    kinds = {}
    total = 0
    for entry in os.listdir(proc / "fd"):
        try:
            target = os.readlink(proc / "fd" / entry)
        except OSError:
            continue  # closed between the listing and the read
        total += 1
        kind = fd_kind(target)
        kinds[kind] = kinds.get(kind, 0) + 1
    mappings = {}
    for line in (proc / "maps").read_text().splitlines():
        fields = line.split(None, 5)
        if len(fields) < 6:
            continue
        name = shm_name(fields[5])
        if name is None:
            continue
        start, end = (int(part, 16) for part in fields[0].split("-"))
        entry = mappings.setdefault(name, {"count": 0, "kib": 0})
        entry["count"] += 1
        entry["kib"] += (end - start) // 1024
    pss_anon = None
    for line in (proc / "smaps_rollup").read_text().splitlines():
        if line.startswith("Pss_Anon:"):
            pss_anon = int(line.split()[1])
    return {
        "pid": int(pid),
        "fds": total,
        "fd_kinds": dict(sorted(kinds.items())),
        "shm": dict(sorted(mappings.items())),
        "pss_anon_kib": pss_anon,
    }


def counter(sample, key):
    name, _, what = key.partition(".")
    process = sample[name]
    if what.startswith("shm:"):
        return process["shm"].get(what[len("shm:"):], {"count": 0})["count"]
    return process[what]


def compare(before, after, allowances):
    """Lines describing each counter that grew past its allowance."""
    grown = []
    for key, allowed in allowances:
        was, now = counter(before, key), counter(after, key)
        if now - was > allowed:
            grown.append(f"{key} grew {was} -> {now} (allowed +{allowed})")
    return grown


def differences(before, after):
    """One line for each counter that differs, in a stable order."""
    lines = []
    for name in sorted(set(before) | set(after)):
        was, now = before.get(name, {}), after.get(name, {})
        for key in ("fds", "pss_anon_kib"):
            if was.get(key) != now.get(key):
                lines.append(f"{name}.{key} {was.get(key)} -> {now.get(key)}")
        kinds = set(was.get("fd_kinds", {})) | set(now.get("fd_kinds", {}))
        for kind in sorted(kinds):
            a, b = was.get("fd_kinds", {}).get(kind, 0), now.get("fd_kinds", {}).get(kind, 0)
            if a != b:
                lines.append(f"{name}.fd[{kind}] {a} -> {b}")
        none = {"count": 0, "kib": 0}
        for shm in sorted(set(was.get("shm", {})) | set(now.get("shm", {}))):
            a, b = was.get("shm", {}).get(shm, none), now.get("shm", {}).get(shm, none)
            if a != b:
                lines.append(
                    f"{name}.shm[{shm}] {a['count']} ({a['kib']} KiB) -> {b['count']} ({b['kib']} KiB)"
                )
    return lines


def main(argv):
    if len(argv) >= 3 and argv[1] == "sample":
        result = {}
        for pair in argv[2:]:
            name, _, pid = pair.partition("=")
            result[name] = sample_process(pid)
        json.dump(result, sys.stdout, indent=1)
        print()
        return 0
    if len(argv) >= 5 and argv[1] == "compare":
        before = json.loads(pathlib.Path(argv[2]).read_text())
        after = json.loads(pathlib.Path(argv[3]).read_text())
        allowances = []
        for pair in argv[4:]:
            key, _, allowed = pair.rpartition("=")
            allowances.append((key, int(allowed)))
        grown = compare(before, after, allowances)
        for line in grown:
            print(line)
        return 1 if grown else 0
    if len(argv) == 4 and argv[1] == "diff":
        before = json.loads(pathlib.Path(argv[2]).read_text())
        after = json.loads(pathlib.Path(argv[3]).read_text())
        for line in differences(before, after):
            print(line)
        return 0
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
