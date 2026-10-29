#!/usr/bin/env python3
"""CI-only interactive-user resource observer. No release thresholds."""
import argparse
import base64
import hashlib
import json
import itertools
import os
import pwd
import re
import stat
import subprocess
import tempfile
from pathlib import Path
import time


def global_records(raw, socket_name):
    """Journal-sized chunks; a digest lets the host reject partial captures."""
    if not raw or len(raw) > 2 * 1024 * 1024:
        raise ValueError("Wayland capture is empty or exceeds 2 MiB")
    digest = hashlib.sha256(raw).hexdigest()
    parts = [base64.b64encode(raw[at:at+2048]).decode("ascii")
             for at in range(0, len(raw), 2048)]
    return [{"sha256": digest, "index": index, "count": len(parts),
             "socket": socket_name, "data": part} for index, part in enumerate(parts)]


def capture_globals(uid):
    runtime = Path(f"/run/user/{uid}")
    if not runtime.is_dir():
        return False
    for path in sorted(runtime.iterdir()):
        if not path.name.startswith(("wayland-", "tuna-")):
            continue
        try:
            info = path.stat()
            if info.st_uid != uid or not stat.S_ISSOCK(info.st_mode):
                continue
            with tempfile.TemporaryFile() as output:
                result = subprocess.run([
                    "runuser", "-u", pwd.getpwuid(uid).pw_name, "--", "env",
                    f"XDG_RUNTIME_DIR={runtime}", f"WAYLAND_DISPLAY={path.name}",
                    "wayland-info"], stdout=output, stderr=subprocess.DEVNULL, timeout=15)
                output.seek(0)
                raw = output.read(2 * 1024 * 1024 + 1)
            if result.returncode != 0:
                continue
            if (b"interface: 'wl_compositor'" not in raw
                    or b"interface: 'xdg_wm_base'" not in raw):
                continue  # Ignore a private helper socket rather than mislabel it.
            marker = Path("/run/tuna-perf-desktop.json")
            staged = marker.with_suffix(f".tmp-{os.getpid()}")
            staged.write_text(json.dumps({"uid": uid, "socket": path.name}))
            staged.replace(marker)
            for record in global_records(raw, path.name):
                print("tuna-perf-globals: " + json.dumps(record), flush=True)
            return True
        except (FileNotFoundError, ConnectionError, subprocess.TimeoutExpired):
            continue  # The graphical session may still be starting.
    return False


def diagnostic_counter(fields, index, maximum):
    """Optional bounded decimal metadata; malformed evidence is never accounting."""
    if index >= len(fields):
        return None
    value = fields[index]
    if len(value) > 10 or not value.isascii() or not value.isdecimal():
        return None
    value = int(value)
    return value if value <= maximum else None


def read_process(path):
    status_text = (path / "status").read_text()
    status = dict(line.split(":", 1) for line in status_text.splitlines() if ":" in line)
    # The command field can contain spaces and closing parentheses.
    head, tail = (path / "stat").read_text().rsplit(")", 1)
    fields = tail.split()
    return {
        "pid": int(path.name), "uid": int(status["Uid"].split()[0]), "state": fields[0],
        "ppid": int(fields[1]), "start_ticks": int(fields[19]),
        "cpu_ticks": int(fields[11]) + int(fields[12]),
        # Reuse these reads: no new proc race or diagnostic failure may change
        # accounting. Presence is not a validated memory value; only
        # released_by_exit() reads it, together with PF_EXITING.
        "stat_flags": diagnostic_counter(fields, 6, 0xffffffff),
        "stat_num_threads": diagnostic_counter(fields, 17, 0x7fffffff),
        "status_vmrss_present": int("VmRSS" in status) if len(status_text) <= 16384 else None,
        "status_vmsize_present": int("VmSize" in status) if len(status_text) <= 16384 else None,
        # Names let reports attribute CPU to compositor, shell, Xwayland and
        # portals (#315). comm is the kernel's 15-byte name; cmd is argv[0]'s
        # basename, never the arguments.
        "comm": head.split("(", 1)[1][:15],
    }


# Diagnostic-only limits; ordinary process CPU accounting remains unchanged.
MAX_THREADS_PER_PROCESS = 32
MAX_THREADS_PER_SAMPLE = 128


def bounded_text(path, limit):
    with path.open(errors="replace") as stream:
        text = stream.read(limit + 1)
    if len(text) > limit:
        raise ValueError("oversized proc record")
    return text


def read_thread(path):
    """Numeric scheduling counters only: never retain comm, argv or environment."""
    status = dict(line.split(":", 1) for line in
                  bounded_text(path / "status", 16384).splitlines() if ":" in line)
    head, tail = bounded_text(path / "stat", 4096).rsplit(")", 1)
    fields = tail.split()
    tid = int(head.split("(", 1)[0])
    if tid != int(path.name):
        raise ValueError("thread identity mismatch")
    row = {
        "tid": tid, "start_ticks": int(fields[19]),
        "user_ticks": int(fields[11]), "system_ticks": int(fields[12]),
        "voluntary_context_switches": int(status["voluntary_ctxt_switches"].strip()),
        "involuntary_context_switches": int(status["nonvoluntary_ctxt_switches"].strip()),
    }
    if any(value < 0 for value in row.values()):
        raise ValueError("negative proc counter")
    return int(status["Uid"].split()[0]), fields[0], row


def thread_snapshot(path, uid, limit):
    """Bound work and report races/limits without invalidating process totals.

    Context switches are not wakeup counts or causal stack traces. Match both
    parent process identity and (tid, start_ticks) before deriving any deltas.
    """
    limit = min(MAX_THREADS_PER_PROCESS, max(0, limit))
    result = {"schema": 1, "threads": [], "errors": [], "truncated": False,
              "status": "unavailable", "counter_kind": "context-switches-not-wakeups"}
    if limit == 0:
        result.update(status="incomplete", truncated=True)
        return result, 0
    try:
        with os.scandir(path / "task") as entries:
            # Bound enumeration too, including unexpected directory entries.
            names = [entry.name for entry in itertools.islice(entries, limit + 1)]
    except OSError as error:
        result["errors"].append({"error": type(error).__name__})
        return result, 0
    result["truncated"] = len(names) > limit
    attempted = min(len(names), limit)
    for name in names[:limit]:
        if not name.isdecimal():
            result["errors"].append({"error": "InvalidThreadEntry"})
            continue
        task = path / "task" / name
        try:
            owner, state, row = read_thread(task)
            owner_after, state_after, after = read_thread(task)
            if (owner != uid or owner_after != uid or state in ("Z", "X", "x")
                    or state_after in ("Z", "X", "x")
                    or row["start_ticks"] != after["start_ticks"]):
                raise ValueError("thread lifetime changed")
            result["threads"].append(row)
        except (OSError, ValueError, KeyError, IndexError) as error:
            result["errors"].append({"tid": int(name), "error": type(error).__name__})
    result["threads"].sort(key=lambda row: row["tid"])
    result["status"] = ("incomplete" if result["errors"] or result["truncated"]
                        else "available" if result["threads"] else "unavailable")
    return result, attempted


def command_name(path):
    """Basename of argv[0], bounded; None for kernel threads or a racing exit."""
    try:
        argv0 = (path / "cmdline").read_bytes().split(b"\0", 1)[0]
    except OSError:
        return None
    name = os.path.basename(argv0.decode("utf-8", errors="replace"))[:64]
    return name or None


def drm_clients(path):
    """Retain documented memory counters only, never arbitrary fdinfo text.

    https://www.kernel.org/doc/html/v6.9/gpu/drm-usage-stats.html
    Counters describe client accounting, not unique physical GPU allocations.
    """
    clients, errors = [], []
    try:
        files = list((path / "fdinfo").iterdir())
    except FileNotFoundError:
        return clients, errors
    except OSError as error:
        return clients, [type(error).__name__]
    for info in files:
        try:
            fields = dict(line.split(":", 1) for line in info.read_text().splitlines() if ":" in line)
        except FileNotFoundError:
            continue  # Closed FD during this non-atomic observation.
        except OSError as error:
            errors.append(type(error).__name__)
            continue
        driver = fields.get("drm-driver", "").strip()
        if not driver:
            continue
        counters = {}
        for key, value in fields.items():
            if not re.fullmatch(r"drm-(?:memory|total|shared|resident|purgeable|active)-\S+", key):
                continue
            match = re.fullmatch(r"\s*(\d+)\s*(KiB|MiB)?\s*", value)
            if not match:
                errors.append("InvalidMemoryCounter")
                continue
            counters[key] = int(match[1]) * {None: 1, "KiB": 1024, "MiB": 1024**2}[match[2]]
        client = fields.get("drm-client-id", "").strip()
        if not client.isdecimal():
            errors.append("MissingClientIdentity")
            continue  # Duplicate handles cannot safely be accounted.
        clients.append({"driver": driver, "device": fields.get("drm-pdev", "").strip(),
                        "client_id": client, "memory_bytes": counters})
    return clients, errors


def unique_drm_clients(rows):
    clients = {}
    for row in rows:
        for client in row["drm_clients"]:
            key = (client["driver"], client["device"], client["client_id"])
            if key not in clients:
                clients[key] = dict(client, memory_bytes=dict(client["memory_bytes"]))
            else:
                # Shared/duplicated FDs expose one client. Sampling is not
                # atomic, so preserve the larger observed value per counter.
                values = clients[key]["memory_bytes"]
                for name, value in client["memory_bytes"].items():
                    values[name] = max(value, values.get(name, 0))
    return list(clients.values())


def read_smaps_rollup(path):
    """Bound a transient ESRCH while a process changes or releases its mm.

    The caller still rechecks uid/start time and rejects a persistent read
    failure. Permission, malformed-data and missing-file errors are not retried.
    """
    for attempt in range(3):
        try:
            return (path / "smaps_rollup").read_text()
        except ProcessLookupError:
            if attempt == 2:
                raise
            time.sleep(0.0005)


def numeric_identity(row):
    if row is None:
        return None
    return {"pid": row["pid"], "uid": row["uid"], "start_ticks": row["start_ticks"],
            "state_code": ord(row["state"]) if len(row["state"]) == 1 else None,
            "stat_flags": row.get("stat_flags"),
            "stat_num_threads": row.get("stat_num_threads"),
            "status_vmrss_present": row.get("status_vmrss_present"),
            "status_vmsize_present": row.get("status_vmsize_present")}


# include/linux/sched.h: set by exit_signals() at the start of do_exit() and
# never cleared for the task.
PF_EXITING = 0x00000004


def released_by_exit(error, before, after):
    """The same task is inside do_exit() and has already released its mm.

    smaps_rollup fails with ESRCH once exit_mm() cleared the task's mm, and
    /proc/<pid>/status then omits every Vm* field. Before exit_notify() the
    state is still R/S/D, not Z (Marlin stock runs 40887247970 and
    40890717926: PF_EXITING set, no VmRSS/VmSize, unchanged start ticks). Its
    memory is gone exactly as for an observed zombie, so it departs like one.
    Unknown flags or presence (None), a live mm, another identity or another
    error never do.
    """
    return bool(isinstance(error, ProcessLookupError) and after is not None
                and (after["uid"], after["start_ticks"]) == (before["uid"], before["start_ticks"])
                and after.get("stat_flags") is not None and after["stat_flags"] & PF_EXITING
                and after.get("status_vmrss_present") == 0
                and after.get("status_vmsize_present") == 0)


def failure_record(stage, error, before, after, after_error=None):
    """Fixed call-site enum and numeric identity/errno; no exception text/paths."""
    return {"stage": stage, "errno": getattr(error, "errno", None),
            "before": numeric_identity(before), "after": numeric_identity(after),
            "after_errno": getattr(after_error, "errno", None)}


def sample(uid, proc=Path("/proc"), observer=None, thread_diagnostics=False):
    processes, errors = {}, []
    for path in proc.iterdir():
        if not path.name.isdigit():
            continue
        try:
            row = read_process(path)
            if row["uid"] == uid and row["state"] not in ("Z", "X", "x"):
                processes[row["pid"]] = row
        except (OSError, ValueError, KeyError, IndexError):
            continue  # A process can exit between directory and status reads.
    excluded = {observer} if observer is not None else set()
    while True:
        children = {pid for pid, row in processes.items() if row["ppid"] in excluded}
        if children.issubset(excluded):
            break
        excluded.update(children)
    rows = []
    failures = []
    failure_overflow = 0
    thread_budget = MAX_THREADS_PER_SAMPLE
    for pid, row in processes.items():
        if pid in excluded:
            continue
        path = proc / str(pid)
        stage = "smaps_rollup_read"
        try:
            memory = dict(line.split(":", 1) for line in read_smaps_rollup(path).splitlines() if ":" in line)
            stage = "smaps_rollup_parse"
            row.update(pss_kib=int(memory["Pss"].split()[0]), rss_kib=int(memory["Rss"].split()[0]))
            stage = "fd_enumeration"
            row["fds"] = len(list((path / "fd").iterdir()))
            stage = "drm_read"
            row["drm_clients"], row["drm_errors"] = drm_clients(path)
            stage = "command_read"
            row["cmd"] = command_name(path)
            if thread_diagnostics:
                stage = "thread_read"
                row["thread_diagnostics"], attempted = thread_snapshot(path, uid, thread_budget)
                thread_budget -= attempted
            stage = "identity_recheck"
            current_identity = read_process(path)
            if current_identity["start_ticks"] != row["start_ticks"] or current_identity["uid"] != uid:
                if len(failures) < 128:
                    failures.append(failure_record(stage, None, row, current_identity))
                else:
                    failure_overflow += 1
                errors.append({"pid": pid, "error": "ProcessIdentityChanged"})
                continue
            rows.append(row)
        except (OSError, ValueError, KeyError) as error:
            identity = None
            identity_error = None
            exited = False
            try:
                identity = read_process(path)
                exited = (identity["state"] in ("Z", "X", "x")
                          or released_by_exit(error, row, identity))
            except (FileNotFoundError, ProcessLookupError) as observed:
                identity_error = observed
                exited = True
            except (OSError, ValueError, KeyError, IndexError) as observed:
                identity_error = observed
            if len(failures) < 128:
                failures.append(failure_record(stage, error, row, identity, identity_error))
            else:
                failure_overflow += 1
            if exited:
                continue  # Preserve the existing explicit exit accounting rule.
            errors.append({"pid": pid, "error": type(error).__name__})
    drm = unique_drm_clients(rows)
    drm_errors = [{"pid": row["pid"], "error": error} for row in rows for error in row["drm_errors"]]
    gpu = {"clients": drm, "errors": drm_errors,
           "status": "incomplete" if drm_errors else ("available" if any(c["memory_bytes"] for c in drm) else "unavailable")}
    return {"uid": uid, "processes": rows, "unreadable": errors, "drm_memory": gpu,
            "failure_evidence": failures, "failure_evidence_overflow": failure_overflow,
            "pss_kib": sum(row["pss_kib"] for row in rows),
            "rss_kib": sum(row["rss_kib"] for row in rows),
            "fds": sum(row["fds"] for row in rows), "process_count": len(rows)}


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--interval", type=float, default=1)
    ap.add_argument("--user", help="target interactive user; observer may run as root to read protected smaps")
    ap.add_argument("--thread-diagnostics", action="store_true",
                    help="bounded numeric per-thread CPU/context-switch counters; no wakeup claim")
    args = ap.parse_args()
    if args.interval <= 0:
        ap.error("interval must be positive")
    uid = pwd.getpwnam(args.user).pw_uid if args.user else os.getuid()
    previous, previous_at, previous_boot_at = {}, None, None
    globals_captured = False
    ticks = os.sysconf("SC_CLK_TCK")
    while True:
        start = time.monotonic()
        boot_start = time.clock_gettime(time.CLOCK_BOOTTIME)
        row = sample(uid, observer=os.getpid(), thread_diagnostics=args.thread_diagnostics)
        current = {(p["pid"], p["start_ticks"]): p["cpu_ticks"] for p in row["processes"]}
        delta = sum(max(0, count - previous.get(key, count)) for key, count in current.items())
        row.update(monotonic_s=start, collection_s=time.monotonic()-start, boottime_s=time.clock_gettime(time.CLOCK_BOOTTIME),
                   hz=ticks, cpu_observed_percent=(100 * delta / ticks / (start - previous_at)
                                                  if previous_at is not None else None),
                   departed_processes=len(previous.keys() - current.keys()))
        row["cpu_interval_start_boottime_s"] = previous_boot_at
        print("tuna-perf-sample: " + json.dumps(row, separators=(",", ":")), flush=True)
        previous, previous_at, previous_boot_at = current, start, boot_start
        if not globals_captured:
            globals_captured = capture_globals(uid)
        time.sleep(max(0, args.interval - (time.monotonic() - start)))


if __name__ == "__main__":
    main()
