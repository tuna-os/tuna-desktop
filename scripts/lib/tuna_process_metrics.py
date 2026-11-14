"""Memory and fd accounting of a process tree, for the stress harnesses
(scripts/tuna-stress, scripts/tuna-hotplug-stress): totals a growth gate
compares, and a per-process breakdown that names the owner of a rise."""
from pathlib import Path
import subprocess
import time

ROLLUP = {"Pss:": "pss_kib", "Rss:": "rss_kib", "Pss_Anon:": "pss_anon_kib",
          "Pss_File:": "pss_file_kib", "Pss_Shmem:": "pss_shmem_kib"}


def tree(root_pid):
    """A process and all its descendants."""
    pairs = subprocess.check_output(["ps", "-eo", "pid=,ppid="], text=True)
    children = {root_pid}
    edges = [tuple(map(int, line.split())) for line in pairs.splitlines()]
    while True:
        expanded = children | {pid for pid, parent in edges if parent in children}
        if expanded == children:
            return children
        children = expanded


def mapping_kind(name):
    if not name:
        return "anon"
    if name.startswith(("memfd:", "/memfd:", "/dev/shm/", "/SYSV")):
        return "shm"
    if name.startswith("/dev/dri/"):
        return "gpu"
    if name.startswith("["):
        return name.strip("[]").split(":")[0]
    return "file"


def process_memory(pid):
    """PSS by mapping kind, so growth names its owner: heap, shm pools, GPU or files."""
    kinds, name = {}, ""
    for line in Path(f"/proc/{pid}/smaps").read_text().splitlines():
        head = line.split(None, 1)[0]
        if not head.endswith(":"):
            fields = line.split(None, 5)
            name = fields[5].strip() if len(fields) > 5 else ""
        elif head == "Pss:":
            kind = mapping_kind(name)
            kinds[kind] = kinds.get(kind, 0) + int(line.split()[1])
    return kinds


def rollup(pid):
    """The kernel's totals for one process: PSS, RSS and PSS by backing."""
    values = dict.fromkeys(ROLLUP.values(), 0)
    for line in Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines():
        parts = line.split()
        if parts[0] in ROLLUP:
            values[ROLLUP[parts[0]]] = int(parts[1])
    return values


def tree_rollup(root_pid):
    """Each process of a tree by `comm:pid` with its kernel totals: the
    cheap sample, for every step of a run."""
    per_process = {}
    for pid in tree(root_pid):
        try:
            comm = Path(f"/proc/{pid}/comm").read_text().strip()
            per_process[f"{comm}:{pid}"] = rollup(pid)
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            continue
    return per_process


def metrics(root_pid):
    """PSS, RSS, fds and mappings of a process tree, in total and per
    process (`comm:pid`): the kernel's Pss_Anon/Pss_File/Pss_Shmem split
    and PSS by mapping kind."""
    totals = dict.fromkeys(("pss_kib", "rss_kib", "fds", "maps", "compositor_fds"), 0)
    processes = tree(root_pid)
    per_process = {}
    for pid in processes:
        proc = Path(f"/proc/{pid}")
        try:
            values = rollup(pid)
            count = len(list((proc / "fd").iterdir()))
            mappings = (proc / "maps").read_text().count("\n")
            comm = (proc / "comm").read_text().strip()
            kinds = process_memory(pid)
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            continue
        totals["pss_kib"] += values["pss_kib"]
        totals["rss_kib"] += values.pop("rss_kib")
        totals["fds"] += count
        totals["maps"] += mappings
        if pid == root_pid:
            totals["compositor_fds"] = count
        per_process[f"{comm}:{pid}"] = {**values, "fds": count, "maps": mappings,
                                        "pss_by_kind_kib": kinds}
    return {"elapsed_s": time.monotonic(), **totals, "processes": len(processes),
            "per_process": per_process}


def save_smaps(root_pid, directory, tag):
    """Each process's /proc/<pid>/smaps as `smaps-<tag>-<comm>-<pid>.txt`:
    two of them diff a rise down to the mappings that grew."""
    for pid in tree(root_pid):
        try:
            comm = Path(f"/proc/{pid}/comm").read_text().strip()
            text = Path(f"/proc/{pid}/smaps").read_text()
        except (FileNotFoundError, ProcessLookupError, PermissionError):
            continue
        (Path(directory) / f"smaps-{tag}-{comm}-{pid}.txt").write_text(text)


def largest_rises(before, after, top):
    """The `top` largest PSS changes between two samples, as
    (KiB, process name, mapping kind), largest first."""
    def by_kind(sample):
        totals = {}
        for key, process in sample["per_process"].items():
            comm = key.rsplit(":", 1)[0]
            for kind, kib in process["pss_by_kind_kib"].items():
                totals[(comm, kind)] = totals.get((comm, kind), 0) + kib
        return totals
    old, new = by_kind(before), by_kind(after)
    return [(delta, *key) for delta, key in
            sorted(((new.get(k, 0) - old.get(k, 0), k) for k in new | old), reverse=True)[:top]]
