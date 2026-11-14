"""Independent, fixture-bounded readbacks for native system-application journeys.

No operation here performs a claimed GUI action. The only destructive block target
is the separately attached, nonce-serialled, unmounted disposable virtio disk.
"""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
import re
import stat
import time


def _require(condition, message):
    if not condition:
        raise RuntimeError(message)


def _digest(path):
    h = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            h.update(chunk)
    return h.hexdigest()


def _owned(context, relative):
    path = context.path(relative)
    path.mkdir(parents=True, exist_ok=True)
    os.chown(path, context.uid, context.owner.pw_gid)
    return path


def _write(context, path, content):
    _require(context.path(str(path.relative_to(context.fixture))) == path,
             "fixture write escaped its original bounded path")
    path.write_bytes(content)
    os.chown(path, context.uid, context.owner.pw_gid)


def _identity(pid):
    base = Path("/proc") / str(pid)
    raw = (base / "stat").read_text()
    fields = raw[raw.rfind(")") + 2:].split()
    exe = (base / "exe").resolve(strict=True)
    return {"pid": pid, "uid": base.stat().st_uid, "comm": (base / "comm").read_text().strip(),
            "state": fields[0], "nice": int(fields[16]), "start_ticks": int(fields[19]),
            "exe": str(exe), "exe_sha256": _digest(exe),
            "argv": (base / "cmdline").read_bytes().decode().split("\0")[:-1]}


def _package(context, process):
    path = Path(process["exe"])
    mode = path.stat()
    _require(mode.st_uid == 0 and not mode.st_mode & 0o022, "native backend executable is not root-owned immutable installed code")
    _require(_digest(path) == process["exe_sha256"], "native backend executable changed during package admission")
    owner = context.command(["pacman", "-Qqo", str(path)]).stdout.strip()
    _require(re.fullmatch(r"[A-Za-z0-9@_+.-]+", owner) is not None, "native backend has no unique installed Arch package owner")
    package = context.command(["pacman", "-Q", owner]).stdout.strip()
    _require(package.startswith(owner + " ") and len(package.split()) == 2, "installed Arch package version unavailable")
    return {"exe": str(path), "exe_sha256": process["exe_sha256"], "package": package}


def _unit(context, suffix):
    return "tuna-core-" + context.nonce[:16] + "-" + suffix + ".service"


def _unit_pid(context, suffix):
    result = context.user(["systemctl", "--user", "show", "--property=MainPID", "--value", _unit(context, suffix)])
    pid = int(result.stdout.strip())
    _require(pid > 1, "owned fixture service has no live MainPID: " + suffix)
    result = _identity(pid)
    _require(result["uid"] == context.uid, "fixture process changed owner")
    return result


def _unit_members(context, suffix):
    group = context.user(["systemctl", "--user", "show", "--property=ControlGroup", "--value", _unit(context, suffix)]).stdout.strip()
    _require(group.startswith("/user.slice/") and _unit(context, suffix) in group, "owned endpoint cgroup identity mismatch")
    members = []
    for pid in (Path("/sys/fs/cgroup") / group.lstrip("/") / "cgroup.procs").read_text().split():
        try:
            actual = _identity(int(pid))
        except FileNotFoundError:
            # Remote typed commands can naturally exit between the two procfs reads.
            # These are explicitly not live service identities or passing evidence.
            members.append({"pid": int(pid), "exited_during_snapshot": True})
            continue
        _require(actual["uid"] == context.uid, "remote endpoint cgroup changed principal")
        members.append(actual)
    return members


def _service(context, name, user=False):
    run = context.user if user else context.command
    argv = ["systemctl"] + (["--user"] if user else []) + ["show", name, "--property=ActiveState,SubState,MainPID,ExecMainStartTimestampMonotonic"]
    result = run(argv)
    values = dict(line.split("=", 1) for line in result.stdout.splitlines() if "=" in line)
    _require(values.get("ActiveState") == "active", "required native service unavailable: " + name)
    pid = int(values.get("MainPID", "0"))
    if pid:
        values["process"] = _identity(pid)
    return values


def _secrets(context, activate=False):
    bus = ["gdbus", "call", "--session", "--dest", "org.freedesktop.DBus", "--object-path", "/org/freedesktop/DBus", "--method"]
    if activate:
        context.user(bus + ["org.freedesktop.DBus.StartServiceByName", "org.freedesktop.secrets", "0"])
    result = context.user(bus + ["org.freedesktop.DBus.GetConnectionUnixProcessID", "org.freedesktop.secrets"])
    match = re.fullmatch(r"\(uint32 ([0-9]+),\)\s*", result.stdout)
    _require(match is not None, "real session Secret Service owner identity unavailable")
    actual = _identity(int(match[1]))
    _require(actual["uid"] == context.uid, "Secret Service is not owned by the original desktop user")
    return {"name": "org.freedesktop.secrets", "process": actual}


def _original_process(context, prior):
    _require(isinstance(prior, dict) and type(prior.get("pid")) is int and prior["pid"] > 1
             and type(prior.get("start_ticks")) is int and prior["start_ticks"] > 0
             and prior.get("uid") == context.uid and isinstance(prior.get("exe"), str)
             and re.fullmatch(r"[0-9a-f]{64}", prior.get("exe_sha256", "")) is not None,
             "original controlled process authority is incomplete")
    base = Path("/proc", str(prior["pid"]))
    try:
        actual = _identity(prior["pid"])
    except FileNotFoundError:
        _require(not base.exists(), "controlled process exists but its executable identity is unreadable")
        return None
    _require(all(actual[key] == prior[key] for key in ("pid", "uid", "start_ticks", "exe", "exe_sha256")),
             "controlled process PID or executable identity changed")
    return actual


def _processes(context, prepare=False):
    if prepare:
        _require(not context.control_exists("system-processes"), "controlled process preparation already has original authority")
        expected = {}
        context.save_control("system-processes", expected)
        for suffix, prefix in (("selected", "tcsel"), ("unselected", "tcneg")):
            name = prefix + context.nonce.replace("-", "")[:8]
            code = ("import ctypes,time; ctypes.CDLL(None).prctl(15," + repr(name.encode()) + ",0,0,0); time.sleep(1800)")
            context.user(["systemd-run", "--user", "--collect", "--unit=" + _unit(context, suffix), "python3", "-c", code])
            deadline = time.monotonic() + 5
            while True:
                actual = _unit_pid(context, suffix)
                if actual["comm"] == name:
                    break
                _require(time.monotonic() < deadline, "controlled process did not establish exact kernel identity")
                time.sleep(0.05)
            expected[suffix] = actual
            context.save_control("system-processes", expected)
    expected = context.read_control("system-processes")
    _require(isinstance(expected, dict) and set(expected) == {"selected", "unselected"},
             "original controlled process authority is incomplete")
    result = {}
    for suffix in ("selected", "unselected"):
        result[suffix] = _original_process(context, expected[suffix])
    _require(result["unselected"] is not None, "unselected negative process died")
    context.save_control("system-process-effect", result)
    return result


def _tree(context, prepare=False):
    root = _owned(context, "system/usage")
    scanned, unscanned = root / "scanned", root / "unscanned"
    if prepare:
        for directory in (scanned, scanned / "nested", unscanned):
            directory.mkdir(exist_ok=True)
            os.chown(directory, context.uid, context.owner.pw_gid)
        for path, count, byte in ((scanned / "alpha.bin", 4 * 1024 * 1024, b"A"),
                                   (scanned / "nested" / "beta.bin", 2 * 1024 * 1024, b"B"),
                                   (unscanned / "untouched.bin", 3 * 1024 * 1024, b"N")):
            _write(context, path, byte * count)
    def inventory(directory):
        records = []
        for path in sorted(directory.rglob("*")):
            _require(not path.is_symlink(), "tree fixture became a symlink")
            if path.is_file():
                s = path.stat()
                records.append({"path": str(path.relative_to(directory)), "bytes": s.st_size,
                                "allocated_bytes": s.st_blocks * 512, "sha256": _digest(path), "mtime_ns": s.st_mtime_ns})
        directories = [directory] + [p for p in directory.rglob("*") if p.is_dir()]
        return {"path": str(directory), "files": records,
                "bytes": sum(item["bytes"] for item in records),
                "allocated_bytes": sum(item["allocated_bytes"] for item in records),
                "recursive_allocated_bytes": sum(item["allocated_bytes"] for item in records)
                + sum(p.stat().st_blocks * 512 or p.stat().st_size for p in directories)}
    return {"scanned": inventory(scanned), "unscanned": inventory(unscanned), "identity": context.identity()}


def _journal(context, prepare=False):
    tag = "tuna-core-" + context.nonce[:16]
    token = "CORE_JOURNAL_" + context.nonce.replace("-", "")
    absent = "CORE_ABSENT_" + context.nonce.replace("-", "")
    if prepare:
        _require(not context.control_exists("system-journal"), "journal producers already have original authority")
        context.user(["systemd-cat", "--identifier=" + tag, "--priority=info"], input=token + " producer-payload\n")
        context.user(["systemd-cat", "--identifier=" + tag + "-negative", "--priority=warning"], input="UNSELECTED_" + token + "\n")
        context.command(["journalctl", "--sync"])
    command = ["journalctl", "--no-pager", "--output=json", "_UID=" + str(context.uid), "SYSLOG_IDENTIFIER=" + tag]
    read = context.user(command)
    entries = [json.loads(line) for line in read.stdout.splitlines() if line.startswith("{")]
    _require(len(entries) == 1 and entries[0].get("MESSAGE") == token + " producer-payload"
             and entries[0].get("_COMM") and entries[0].get("__CURSOR"),
             "ordinary user cannot read a unique actual journal producer identity/payload")
    negative_read = context.user(["journalctl", "--no-pager", "--output=json", "_UID=" + str(context.uid), "SYSLOG_IDENTIFIER=" + tag + "-negative"])
    negative_entries = [json.loads(line) for line in negative_read.stdout.splitlines() if line.startswith("{")]
    _require(len(negative_entries) == 1 and negative_entries[0].get("MESSAGE") == "UNSELECTED_" + token
             and negative_entries[0].get("__CURSOR"), "real unique independent negative producer payload unavailable")
    missing = context.user(["journalctl", "--no-pager", "--output=json", "_UID=" + str(context.uid), "SYSLOG_IDENTIFIER=" + absent])
    _require(not any(line.startswith("{") for line in missing.stdout.splitlines()), "negative journal tag unexpectedly exists")
    receipt = {"tag": tag, "token": token, "absent": absent, "entries": entries,
               "negative_tag": tag + "-negative", "negative_entries": negative_entries,
               "producer_exe": "/usr/bin/systemd-cat", "producer_exe_sha256": _digest(Path("/usr/bin/systemd-cat")),
               "journal": _service(context, "systemd-journald.service"), "identity": context.identity()}
    if prepare:
        receipt["journal_package"] = _package(context, receipt["journal"]["process"])
        context.save_control("system-journal", receipt)
    else:
        original = context.read_control("system-journal")
        _require(all(receipt[key] == original[key] for key in ("entries", "negative_entries", "producer_exe_sha256")),
                 "original selected or unselected journal producer payload changed")
        _require(all(receipt["journal"]["process"][key] == original["journal"]["process"][key]
                     for key in ("pid", "uid", "start_ticks", "exe", "exe_sha256")),
                 "actual journal service identity changed across original consumer operation")
        receipt["journal_package"] = original["journal_package"]
    context.save_control("system-journal-effect", receipt)
    return receipt


def _block(context):
    serial = "TC397-" + context.nonce[:14]
    data = json.loads(context.command(["lsblk", "--json", "--bytes", "--paths", "--output", "NAME,TYPE,SIZE,SERIAL,RM,TRAN,START,PTTYPE,MAJ:MIN,MOUNTPOINTS,PKNAME,FSTYPE,LABEL,UUID"]).stdout)
    def flatten(nodes):
        for node in nodes:
            yield node
            yield from flatten(node.get("children", []))
    devices = list(flatten(data["blockdevices"]))
    matches = [node for node in devices if node.get("serial") == serial and node["type"] == "disk"]
    _require(len(matches) == 1, "exactly one separately attached nonce-serialled disposable disk is required")
    disk = matches[0]
    _require(256 * 1024 * 1024 <= int(disk["size"]) <= 2 * 1024 ** 3, "disposable disk size outside 256MiB..2GiB")
    _require(disk.get("rm") is True, "disposable storage must be a genuinely removable emulated device, not an internal disk/policy override")
    path = Path(disk["name"])
    _require(stat.S_ISBLK(path.stat().st_mode), "disposable fixture is not a block device")
    # Root/boot/swap ancestry must never intersect the admitted disk, including dm stacks.
    topology = json.loads(context.command(["lsblk", "--json", "--inverse", "--paths", "--output", "NAME,MOUNTPOINTS"]).stdout)
    def protected(nodes, ancestors=()):
        for node in nodes:
            chain = ancestors + (node["name"],)
            mounts = node.get("mountpoints") or []
            if any(m == "/" or m == "[SWAP]" or (m and m.startswith("/boot")) for m in mounts):
                backing = set(chain) | {item["name"] for item in flatten(node.get("children", []))}
                _require(str(path) not in backing, "disposable serial aliases protected system storage")
            protected(node.get("children", []), chain)
    protected(topology["blockdevices"])
    descendants = [node for node in flatten([disk])]
    mounted = [m for node in descendants for m in node.get("mountpoints", []) or [] if m]
    with path.open("rb", buffering=0) as stream:
        first = stream.read(1024 * 1024)
        stream.seek(int(disk["size"]) - 2 * 1024 * 1024)
        tail = stream.read(1024 * 1024)
    details = context.user(["udisksctl", "info", "--block-device", str(path)]).stdout
    _require(re.search(r"HintSystem:\s+false\b", details) is not None, "disposable storage is classified as a protected system device")
    return {"device": str(path), "serial": serial, "disk": disk, "mounted": mounted,
            "first_mib_sha256": hashlib.sha256(first).hexdigest(),
            "unallocated_mib_sha256": hashlib.sha256(tail).hexdigest(),
            "negative_block_offset": int(disk["size"]) - 2 * 1024 * 1024, "negative_block_bytes": 1024 * 1024,
            "device_sha256": _digest(path), "udisks_info": details, "udisks": _service(context, "udisks2.service")}


def _disk_prepare(context):
    context.user(["udisksctl", "status"])
    disk = _block(context)
    _require(not disk["mounted"], "disposable device already mounted before fixture admission")
    _require(not disk["disk"].get("children") and not disk["disk"].get("fstype") and not disk["disk"].get("pttype"), "parent disposable device must start blank/unpartitioned")
    _require(not context.control_exists("system-disk"), "disposable disk already has original authority")
    payload = _owned(context, "system/disk/payloads")
    token = ("TUNA_DISPOSABLE_PAYLOAD_" + context.nonce + "\n").encode()
    _write(context, payload / "qualification.txt", token)
    _write(context, payload / "negative.bin", b"independent-negative-control\n" * 1024)
    info = {"source": str(payload), "label": "TC" + context.nonce.replace("-", "")[:12],
            "payload_sha256": hashlib.sha256(token).hexdigest(),
            "negative_sha256": _digest(payload / "negative.bin"), "before": disk}
    context.save_control("system-disk", info)
    return info


def _disk_read(context):
    expected = context.read_control("system-disk")
    actual = _block(context)
    _require(actual["device"] == expected["before"]["device"] and actual["serial"] == expected["before"]["serial"]
             and actual["disk"]["size"] == expected["before"]["disk"]["size"], "disposable disk changed since original admission")
    actual["filesystems"] = [node for node in actual["disk"].get("children", []) if node.get("fstype")]
    actual["payloads"] = []
    for mount in actual["mounted"]:
        base = Path(mount)
        p, n = base / "qualification.txt", base / "negative.bin"
        if p.is_file() and n.is_file():
            # Ordinary-user access is independently required, not just privileged bytes.
            content = context.user(["cat", str(p)]).stdout.encode()
            actual["payloads"].append({"mount": mount, "sha256": hashlib.sha256(content).hexdigest(),
                                       "negative_sha256": context.user(["sha256sum", str(n)]).stdout.split()[0], "uid": p.stat().st_uid})
    actual["source"] = {"payload_sha256": _digest(context.path("system/disk/payloads/qualification.txt")),
                        "negative_sha256": _digest(context.path("system/disk/payloads/negative.bin"))}
    _require(actual["source"]["payload_sha256"] == expected["payload_sha256"]
             and actual["source"]["negative_sha256"] == expected["negative_sha256"], "native disk operation modified its independent source controls")
    actual["raw_payloads"] = []
    if not actual["mounted"]:
        for fs in actual["filesystems"]:
            if fs.get("fstype") != "ext4" or fs.get("label") != expected["label"]:
                continue
            readbacks = [context.command(["debugfs", "-R", "cat /" + filename, fs["name"]], check=False)
                         for filename in ("qualification.txt", "negative.bin")]
            _require(all(item.returncode == 0 for item in readbacks), "independent read-only ext4 consumer failed")
            if any("File not found" in item.stderr for item in readbacks):
                actual["raw_payload_absence"] = [item.stderr for item in readbacks]
                continue
            _require(not any("Filesystem not open" in item.stderr or "Bad magic" in item.stderr for item in readbacks),
                     "native formatted filesystem is not readable by independent ext4 consumer")
            actual["raw_payloads"].append({"device": fs["name"], "sha256": hashlib.sha256(readbacks[0].stdout.encode()).hexdigest(),
                                            "negative_sha256": hashlib.sha256(readbacks[1].stdout.encode()).hexdigest(),
                                            "consumer": "debugfs read-only", "stderr": [item.stderr for item in readbacks]})
    actual["expected"] = expected
    context.save_control("system-disk-effect", actual)
    return actual

def _disk_policy(context, arguments):
    actor = arguments.get("actor")
    _require(isinstance(actor, dict) and actor.get("id") == "org.gnome.DiskUtility",
             "actual original native Disks actor authority required")
    app = _original_process(context, actor)
    _require(app is not None, "original Disks frontend retired before its policy observation")
    _require(app["uid"] == context.uid and Path(app["exe"]).name == "gnome-disks", "policy subject is not the actual original native Disks consumer")
    package = _package(context, app)
    disk = _block(context)
    action = "org.freedesktop.udisks2.modify-device"
    policy = context.user(["pkaction", "--action-id", action, "--verbose"]).stdout
    def check(subject, run, caller_uid):
        observed_uid = run(["id", "--user"]).stdout.strip()
        _require(observed_uid == str(caller_uid) == str(subject["uid"]),
                 "Polkit checker must run as the actual unprivileged subject UID")
        process = str(subject["pid"]) + "," + str(subject["start_ticks"]) + "," + str(subject["uid"])
        result = run(["pkcheck", "--action-id", action, "--process", process, "--detail", "device", disk["device"]], check=False)
        _require(result.returncode in (0, 1)
                 and result.stderr.strip() in ("", "Not authorized."),
                 "Polkit backend/query error is not an operation denial")
        return {"subject": subject, "caller_uid": int(observed_uid), "returncode": result.returncode,
                "stdout": result.stdout, "stderr": result.stderr}
    positive = check(app, context.user, context.uid)
    _require(positive["returncode"] == 0, "ordinary active UID1000 is not authorized for supported removable-device modification")
    # A real non-session process under a different unprivileged UID is the
    # independent denied subject. Same-UID user services can inherit logind's
    # display-session fallback and are not a valid inactive-user negative.
    context.command(["systemd-run", "--collect", "--uid=65534", "--property=Type=exec", "--unit=" + _unit(context, "disk-policy"), "sleep", "60"])
    try:
        negative_pid = int(context.command(["systemctl", "show", "--property=MainPID", "--value", _unit(context, "disk-policy")]).stdout.strip())
        negative_subject = _identity(negative_pid)
        _require(negative_subject["uid"] == 65534, "independent inactive policy subject changed UID")
        import pwd
        negative_owner = pwd.getpwuid(65534)
        def negative_run(argv, **options):
            return context.command(["runuser", "-u", negative_owner.pw_name, "--"] + argv, **options)
        negative = check(negative_subject, negative_run, 65534)
        _require(negative["returncode"] == 1, "real inactive-session process unexpectedly authorized for removable-device modification")
    finally:
        context.command(["systemctl", "stop", _unit(context, "disk-policy")])
    receipt = {"action": action, "policy": policy, "positive": positive, "inactive_subject_denied": negative, "app_package": package,
               "polkit": _service(context, "polkit.service"), "device": disk}
    context.save_control("system-disk-policy", receipt)
    return receipt


def _rfb_socket_rows(raw, process):
    rows = []
    for line in raw.splitlines():
        if not line.strip():
            continue
        endpoints = [word for word in line.split() if word.endswith(":5991")]
        _require(len(endpoints) == 1 and endpoints[0] in ("127.0.0.1:5991", "[::1]:5991", "::1:5991"),
                 "RFB socket is not the admitted loopback-only endpoint")
        owners = {int(value) for value in re.findall(r"\bpid=([0-9]+)", line)}
        _require(owners == {process["pid"]}, "RFB socket does not belong exclusively to the original packaged Xvnc process")
        rows.append({"line": line, "endpoint": endpoints[0], "pid": process["pid"]})
    return rows


def _rfb_sockets(context, process, listening):
    argv = ["ss", "--tcp", "--no-header", "--numeric", "--processes"]
    argv += ["--listening"] if listening else ["state", "established"]
    raw = context.user(argv + ["sport = :5991"]).stdout
    rows = _rfb_socket_rows(raw, process)
    if listening:
        _require(rows, "original packaged Xvnc endpoint is not listening")
    return {"stdout": raw, "rows": rows}


def _endpoint(context, prepare=False):
    root = _owned(context, "system/remote")
    descriptor = root / "owned-endpoint.vnc"
    if not descriptor.exists():
        _write(context, descriptor, b"[Connection]\nHost=127.0.0.1\nPort=5991\n")
    if prepare:
        if context.control_exists("system-remote"):
            _require(context.read_control("system-remote")["stopped"], "previous admitted endpoint has not retired")
        vacant = context.user(["ss", "--tcp", "--no-header", "--listening", "--numeric", "sport = :5991"]).stdout
        _require(not vacant.strip(), "separate RFB fixture endpoint already belongs to another listener")
        admission = {"stopped": False}
        context.save_control("system-remote", admission)
        # Preserve packaged vncpasswd's binary stdout before entering the JSON/text transport.
        context.user(["python3", "-c",
                      "import os,pathlib,subprocess,sys; p=pathlib.Path(sys.argv[1]); "
                      "r=subprocess.run(['vncpasswd','-f'],input=b'TunaVnc7\\n',stdout=subprocess.PIPE,check=True); "
                      "p.write_bytes(r.stdout); os.chmod(p,0o600)", str(root / "passwd")])
        context.user(["systemd-run", "--user", "--collect", "--property=Type=exec", "--unit=" + _unit(context, "xvnc"),
                      "Xvnc", ":91", "-geometry", "1024x640", "-depth", "24", "-localhost=1", "-nolisten", "tcp",
                      "-rfbport", "5991", "-SecurityTypes", "VncAuth", "-PasswordFile", str(root / "passwd"), "-AlwaysShared"])
        deadline = time.monotonic() + 5
        while True:
            sockets = context.user(["ss", "--tcp", "--no-header", "--listening", "--numeric", "sport = :5991"]).stdout
            if sockets.strip():
                break
            _require(time.monotonic() < deadline, "packaged Xvnc did not bind its separate endpoint")
            time.sleep(0.1)
        admission["xvnc"] = _unit_pid(context, "xvnc")
        _rfb_sockets(context, admission["xvnc"], True)
        context.save_control("system-remote", admission)
        context.user(["systemd-run", "--user", "--collect", "--property=Type=exec", "--unit=" + _unit(context, "xterm"),
                      "env", "-u", "WAYLAND_DISPLAY", "-u", "DBUS_SESSION_BUS_ADDRESS", "DISPLAY=:91",
                      "dbus-run-session", "--", "xterm", "-geometry", "110x32+0+0", "-title", "TUNA-REMOTE-" + context.nonce[:8],
                      "-e", "/bin/bash", "--noprofile", "--norc"])
        admission["terminal_session"] = _unit_pid(context, "xterm")
        context.save_control("system-remote", admission)
        deadline = time.monotonic() + 5
        while not any(Path(item.get("exe", "")).name == "xterm" for item in _unit_members(context, "xterm")):
            _require(time.monotonic() < deadline, "packaged endpoint xterm did not become ready")
            time.sleep(0.1)
    result = {"xvnc": _unit_pid(context, "xvnc"), "terminal_session": _unit_pid(context, "xterm"),
              "uri": "vnc://127.0.0.1:5991", "display": ":91", "password": "TunaVnc7", "output": str(root / "typed-result"),
              "token": "REMOTE_TYPED_" + context.nonce.replace("-", ""), "descriptor": str(descriptor), "identity": context.identity()}
    result["members"] = _unit_members(context, "xterm")
    _require(any(Path(item.get("exe", "")).name == "xterm" for item in result["members"]), "real packaged remote xterm consumer missing")
    _require(any(Path(item.get("exe", "")).name == "dbus-daemon" for item in result["members"]), "real separate endpoint bus daemon missing")
    terminals = [item for item in result["members"] if Path(item.get("exe", "")).name == "xterm"]
    buses = [item for item in result["members"] if Path(item.get("exe", "")).name == "dbus-daemon"]
    _require(len(terminals) == len(buses) == 1, "remote endpoint has ambiguous terminal or bus ownership")
    terminal, endpoint_bus = terminals[0], buses[0]
    terminal_env = dict(item.split(b"=", 1) for item in Path("/proc", str(terminal["pid"]), "environ").read_bytes().split(b"\0") if b"=" in item)
    _require(terminal_env.get(b"DISPLAY") == b":91" and terminal_env.get(b"DBUS_SESSION_BUS_ADDRESS")
             and terminal_env[b"DBUS_SESSION_BUS_ADDRESS"] != os.environ.get("DBUS_SESSION_BUS_ADDRESS", "").encode(),
             "remote endpoint does not have its separate display/session bus")
    result["terminal_bus"] = terminal_env[b"DBUS_SESSION_BUS_ADDRESS"].decode()
    _require(result["terminal_bus"] != "unix:path=" + str(context.runtime / "bus"), "endpoint reused desktop session bus")
    result["secrets"] = _secrets(context, activate=prepare)
    if prepare:
        result["packages"] = [_package(context, item) for item in
                              (result["xvnc"], result["terminal_session"], terminal, endpoint_bus, result["secrets"]["process"])]
        context.save_control("system-remote", {"packages": result["packages"], "stopped": False,
                             "xvnc": result["xvnc"], "terminal": terminal, "terminal_session": result["terminal_session"],
                             "bus": endpoint_bus, "secrets": result["secrets"]["process"]})
    else:
        original = context.read_control("system-remote")
        _require(not original["stopped"], "remote read attempted after original endpoint retirement")
        result["packages"] = original["packages"]
        for name, item in (("xvnc", result["xvnc"]), ("terminal", terminal),
                           ("terminal_session", result["terminal_session"]), ("bus", endpoint_bus),
                           ("secrets", result["secrets"]["process"])):
            _require(all(original[name][key] == item[key] for key in ("pid", "uid", "start_ticks", "exe", "exe_sha256")),
                     "packaged endpoint/Secret Service process changed after admission")
            _require(any(p["exe"] == item["exe"] and p["exe_sha256"] == item["exe_sha256"] for p in result["packages"]),
                     "packaged endpoint/Secret Service executable changed after admission")
    result["socket"] = _rfb_sockets(context, result["xvnc"], True)
    result["established"] = _rfb_sockets(context, result["xvnc"], False)["rows"]
    output = context.path("system/remote/typed-result")
    if output.exists():
        metadata = output.lstat()
        _require(stat.S_ISREG(metadata.st_mode) and metadata.st_uid == context.uid and metadata.st_size <= 4096,
                 "remote output is not a bounded ordinary-user-owned regular consumer file")
        result["result"] = context.user(["cat", str(output)]).stdout
        result["result_sha256"] = hashlib.sha256(result["result"].encode()).hexdigest()
    else:
        result["result"] = None
        result["result_sha256"] = None
    clipboard = context.user(["env", "DISPLAY=:91", "xclip", "-selection", "clipboard", "-o"], check=False)
    _require(clipboard.returncode == 0 or (clipboard.returncode == 1
             and "target" in clipboard.stderr and "not available" in clipboard.stderr),
             "remote clipboard backend is unavailable, not an empty selection")
    result["clipboard"] = clipboard.stdout
    result["clipboard_status"] = {"returncode": clipboard.returncode, "stderr": clipboard.stderr}
    context.save_control("system-remote-effect", result)
    return result


def _endpoint_stop(context):
    original = context.read_control("system-remote")
    _require(not original["stopped"], "endpoint was already retired")
    before = _unit_pid(context, "xvnc")
    _require(all(original["xvnc"][key] == before[key] for key in ("pid", "uid", "start_ticks", "exe", "exe_sha256")),
             "endpoint process differs from original admission")
    terminal_session = _unit_pid(context, "xterm")
    _require(all(original["terminal_session"][key] == terminal_session[key]
                 for key in ("pid", "uid", "start_ticks", "exe", "exe_sha256")),
             "endpoint terminal service differs from original admission")
    context.user(["systemctl", "--user", "stop", _unit(context, "xterm"), _unit(context, "xvnc")])
    _require(all(_original_process(context, original[name]) is None
                 for name in ("xvnc", "terminal", "terminal_session", "bus")),
             "original remote endpoint, terminal or separate bus did not stop")
    original["stopped"] = True
    context.save_control("system-remote", original)
    socket = context.user(["ss", "--tcp", "--no-header", "--listening", "--numeric", "sport = :5991"]).stdout
    _require(not socket.strip(), "retired Xvnc endpoint still has a listening socket")
    receipt = {"stopped": before, "socket": socket}
    context.save_control("system-remote-retirement", receipt)
    return receipt


def _flatpak_configuration(context, arguments):
    app = arguments.get("app_id")
    _require(app == "org.gnome.gitg", "Software fixture requires the genuine org.gnome.gitg application")
    branch = arguments.get("branch", "stable")
    _require(branch == "stable", "Software fixture requires the genuine stable branch")
    remote = "flathub"
    return app, branch, remote


def _flatpak_remote_facts(context):
    import configparser
    config = context.home / ".local/share/flatpak/repo/config"
    parsed = configparser.ConfigParser(interpolation=None)
    with config.open() as stream:
        parsed.read_file(stream)
    section = 'remote "flathub"'
    url = parsed.get(section, "url")
    _require(url.rstrip("/") == "https://dl.flathub.org/repo", "Flathub remote is not the supported public repository")
    _require(parsed.getboolean(section, "gpg-verify"), "Flathub commit GPG verification is disabled")
    _require(parsed.getboolean(section, "gpg-verify-summary"), "Flathub summary GPG verification is disabled")
    keyring = config.parent / "flathub.trustedkeys.gpg"
    _require(keyring.is_file() and keyring.stat().st_size > 0, "trusted Flathub public keyring missing")
    return {"url": url, "gpg_verify": True, "gpg_verify_summary": True,
            "trusted_keyring_sha256": _digest(keyring)}


def _flatpak_plan(context, arguments):
    app, branch, remote = _flatpak_configuration(context, arguments)
    plan = context.read_control("system-software-plan")
    _require((plan["app_id"], plan["branch"], plan["remote"]) == (app, branch, remote),
             "Software request differs from original preparation plan")
    _require(_flatpak_remote_facts(context) == plan["remote_facts"],
             "Software remote URL, verification policy or trusted keyring changed after admission")
    return plan


def _flatpak_remote_prepare(context, arguments):
    app, branch, remote = _flatpak_configuration(context, arguments)
    _require(not context.control_exists("system-software-plan"), "Software already has an original preparation plan")
    # Each preparation action owns one finite network command, leaving time for
    # the original 30s QGA identity/transport/retirement envelope.
    context.user(["flatpak", "remote-add", "--user", "--if-not-exists", remote,
                  "https://flathub.org/repo/flathub.flatpakrepo"], timeout=18)
    facts = _flatpak_remote_facts(context)
    remotes = context.user(["flatpak", "remotes", "--user", "--show-details"], timeout=2).stdout
    installed = context.user(["flatpak", "info", "--user", app], timeout=2, check=False)
    _require(installed.returncode != 0 and "not installed" in installed.stderr.lower(),
             "Software install target must initially be absent, not an unavailable backend")
    root = _owned(context, "system/software")
    descriptor = root / (app + ".flatpakref")
    # Consumer input only; the authoritative ref/commits never come from this file.
    _write(context, descriptor, ("[Flatpak Ref]\nVersion=1\nName=" + app + "\nBranch=" + branch
        + "\nUrl=https://dl.flathub.org/repo/\nIsRuntime=false\nTitle=gitg\n").encode())
    plan = {"app_id": app, "branch": branch, "remote": remote, "remote_facts": facts,
            "remotes": remotes, "flatpakref": str(descriptor), "stage": "remote",
            "required_external_prerequisites": [
                "Original gnome-software with its supported Flatpak plugin; ordinary UID1000 user installation",
                "DNS and HTTPS to flathub.org, dl.flathub.org and their actual repository/CDN/runtime endpoints",
                "Public Flathub AppStream for org.gnome.gitg/stable and trusted summary/commit GPG verification",
                "Current and previous signed org.gnome.gitg/stable commits plus their complete runtime and content objects retained by the real remote",
                "Actual downloads/install/update complete within the unchanged bounded native UI and QGA actions",
            ],
            "trusted_keyring_sha256": facts["trusted_keyring_sha256"], "identity": context.identity()}
    context.save_control("system-software-plan", plan)
    return plan


def _flatpak_catalog_prepare(context, arguments):
    plan = _flatpak_plan(context, arguments)
    _require(plan["stage"] == "remote", "Software catalog preparation is out of order")
    result = context.user(["flatpak", "update", "--user", "--appstream", plan["remote"]], timeout=18)
    _require(_flatpak_remote_facts(context) == plan["remote_facts"], "Software catalog refresh changed remote trust")
    plan["catalog"] = {"stdout": result.stdout, "stderr": result.stderr, "returncode": result.returncode}
    plan["stage"] = "catalog"
    plan["identity"] = context.identity()
    context.save_control("system-software-plan", plan)
    return plan


def _flatpak_history_prepare(context, arguments):
    plan = _flatpak_plan(context, arguments)
    _require(plan["stage"] == "catalog", "Software history preparation is out of order")
    result = context.user(["flatpak", "remote-info", "--user", "--log", plan["remote"],
                           plan["app_id"] + "//" + plan["branch"]], timeout=18)
    _require(_flatpak_remote_facts(context) == plan["remote_facts"], "Software history lookup changed remote trust")
    commits = list(dict.fromkeys(re.findall(r"^\s*Commit:\s*([0-9a-f]{64})\s*$", result.stdout, re.MULTILINE)))
    _require(len(commits) >= 2, "real signed remote requires current and previous app commits for native UI update")
    plan.update({"current_commit": commits[0], "previous_commit": commits[1],
                 "history": result.stdout, "history_stderr": result.stderr, "stage": "history",
                 "identity": context.identity()})
    context.save_control("system-software-plan", plan)
    return plan


def _flatpak_old_baseline(context, arguments):
    plan = _flatpak_plan(context, arguments)
    _require(plan["stage"] == "history", "genuine remote history must precede update baseline preparation")
    commit = arguments.get("commit")
    _require(commit == plan["previous_commit"], "update baseline differs from original real remote history")
    installed = context.read_control("system-software-installed")
    _require(installed["commit"] == plan["current_commit"], "native UI current install must precede update baseline preparation")
    current = context.user(["flatpak", "info", "--user", "--show-commit", plan["app_id"]], timeout=2).stdout.strip()
    _require(current == installed["commit"], "Software current installation changed before historical precondition")
    result = context.user(["flatpak", "update", "--user", "--noninteractive", "--commit=" + commit,
                           plan["app_id"]], timeout=18)
    receipt = {"app_id": plan["app_id"], "requested_commit": commit, "stdout": result.stdout,
               "stderr": result.stderr, "returncode": result.returncode, "identity": context.identity()}
    context.save_control("system-software-old-baseline", receipt)
    # Actual deployment/signature readback is a separate finite host request.
    return receipt


def _flatpak_read(context, arguments):
    app, branch, remote = _flatpak_configuration(context, arguments)
    plan = _flatpak_plan(context, arguments)
    _require(plan["stage"] == "history", "Software readback requires original signed remote history")
    listing = context.user(["flatpak", "list", "--user", "--app", "--columns=application,branch,arch,origin,active,installation"]).stdout
    apps = [line.split("\t") for line in listing.splitlines() if line.split("\t")[0] == app]
    if not apps:
        receipt = {"app_id": app, "installed": False, "user_app_list": listing, "identity": context.identity()}
        context.save_control("system-software-effect", receipt)
        return receipt
    _require(len(apps) == 1, "ambiguous user installed application ref")
    _require(len(apps[0]) == 6 and apps[0][1] == branch and apps[0][3] == remote, "native Software installed a different user branch/origin")
    info = context.user(["flatpak", "info", "--user", app]).stdout
    commit = context.user(["flatpak", "info", "--user", "--show-commit", app]).stdout.strip()
    runtime = context.user(["flatpak", "info", "--user", "--show-runtime", app]).stdout.strip()
    runtime_read = context.user(["flatpak", "info", "--user", "--show-commit", runtime], check=False)
    runtime_scope = "user"
    runtime_repo = context.home / ".local/share/flatpak/repo"
    if runtime_read.returncode:
        _require("not installed" in runtime_read.stderr.lower(), "user runtime deployment lookup failed, not a supported shared-runtime resolution")
        # Flatpak supports a user application consuming an already installed,
        # signed system runtime. This is a readback, never a root/system install.
        runtime_scope = "system"
        runtime_repo = Path("/var/lib/flatpak/repo")
        runtime_read = context.user(["flatpak", "info", "--system", "--show-commit", runtime])
    runtime_commit = runtime_read.stdout.strip()
    runtime_info = context.user(["flatpak", "info", "--" + runtime_scope, runtime]).stdout
    runtime_origin = context.user(["flatpak", "info", "--" + runtime_scope, "--show-origin", runtime]).stdout.strip()
    runtime_location = context.user(["flatpak", "info", "--" + runtime_scope, "--show-location", runtime]).stdout.strip()
    permissions = context.user(["flatpak", "info", "--user", "--show-permissions", app]).stdout
    _require(re.fullmatch("[0-9a-f]{64}", commit) and re.fullmatch("[0-9a-f]{64}", runtime_commit), "Flatpak exact app/runtime commit unavailable")
    repository = str(context.home / ".local/share/flatpak/repo")
    signatures = {}
    for kind, value, repo, origin in (("app", commit, repository, remote), ("runtime", runtime_commit, str(runtime_repo), runtime_origin)):
        signature = context.user(["ostree", "--repo=" + repo, "show", "--gpg-verify-remote=" + origin, value])
        verification = signature.stdout + signature.stderr
        _require("Good signature" in verification, "installed " + kind + " commit has no verified trusted remote signature")
        signatures[kind] = verification
    receipt = {"app_id": app, "installed": True, "commit": commit, "runtime": runtime, "runtime_commit": runtime_commit,
               "runtime_scope": runtime_scope, "runtime_info": runtime_info, "runtime_origin": runtime_origin, "runtime_location": runtime_location,
               "permissions": permissions, "permissions_sha256": hashlib.sha256(permissions.encode()).hexdigest(),
               "info": info, "ref": apps[0], "verified_signatures": signatures, "identity": context.identity()}
    _require(commit in (plan["current_commit"], plan["previous_commit"]), "Software installed a commit outside original remote plan")
    if commit == plan["current_commit"]:
        if context.control_exists("system-software-installed"):
            original = context.read_control("system-software-installed")
            _require(all(receipt[key] == original[key] for key in ("commit", "runtime", "runtime_commit", "permissions_sha256")),
                     "Software current deployment or permissions changed after original native install")
        else:
            context.save_control("system-software-installed", receipt)
    context.save_control("system-software-effect", receipt)
    return receipt


def _interfaces(context, arguments):
    app = arguments.get("app_id")
    allowed = {"org.gnome.Connections", "org.gnome.Software", "org.gnome.DiskUtility",
               "org.gnome.SystemMonitor", "org.gnome.Logs", "org.gnome.baobab"}
    _require(app in allowed, "unknown system application interface request")
    path = Path("/usr/share/applications") / (app + ".desktop")
    _require(path.is_file() and path.stat().st_uid == 0 and not path.stat().st_mode & 0o022,
             "root-owned non-writable native desktop entry missing")
    import configparser
    parsed = configparser.ConfigParser(interpolation=None)
    parsed.read(path)
    entry = dict(parsed["Desktop Entry"])
    mime = [value for value in entry.get("mimetype", "").split(";") if value]
    handlers = {value: context.user(["xdg-mime", "query", "default", value]).stdout.strip() for value in mime}
    return {"desktop_file": str(path), "sha256": _digest(path), "entry": entry,
            "declared_mime": mime, "actual_default_handlers": handlers, "identity": context.identity()}


def dispatch(operation, arguments, context):
    _require(context.uid == 1000 and context.owner.pw_uid == 1000, "system journeys require the actual original ordinary UID1000")
    if operation == "interfaces":
        return _interfaces(context, arguments)
    if operation == "process-prepare":
        return _processes(context, True)
    if operation == "process-read":
        return _processes(context)
    if operation == "tree-prepare":
        return _tree(context, True)
    if operation == "tree-read":
        return _tree(context)
    if operation == "journal-prepare":
        return _journal(context, True)
    if operation == "journal-read":
        return _journal(context)
    if operation == "disk-prepare":
        return _disk_prepare(context)
    if operation == "disk-read":
        return _disk_read(context)
    if operation == "disk-policy":
        return _disk_policy(context, arguments)
    if operation == "remote-prepare":
        return _endpoint(context, True)
    if operation == "remote-read":
        return _endpoint(context)
    if operation == "remote-stop":
        return _endpoint_stop(context)
    if operation == "flatpak-remote-prepare":
        return _flatpak_remote_prepare(context, arguments)
    if operation == "flatpak-catalog-prepare":
        return _flatpak_catalog_prepare(context, arguments)
    if operation == "flatpak-history-prepare":
        return _flatpak_history_prepare(context, arguments)
    if operation == "flatpak-read":
        return _flatpak_read(context, arguments)
    if operation == "flatpak-old-baseline":
        return _flatpak_old_baseline(context, arguments)
    if operation == "cleanup":
        storage = None
        if context.control_exists("system-disk"):
            storage = _disk_read(context)
            if storage["mounted"]:
                for node in storage["disk"].get("children", []):
                    if any(node.get("mountpoints") or []):
                        context.user(["udisksctl", "unmount", "--block-device", node["name"]])
                storage = _disk_read(context)
                _require(not storage["mounted"], "fixture storage stayed mounted during cleanup")
        originals = {}
        if context.control_exists("system-processes"):
            originals.update(context.read_control("system-processes"))
        if context.control_exists("system-remote"):
            remote = context.read_control("system-remote")
            for suffix, key in (("xterm", "terminal_session"), ("xvnc", "xvnc")):
                if key in remote:
                    originals[suffix] = remote[key]
        stopped = []
        for suffix in ("selected", "unselected", "xterm", "xvnc"):
            unit = _unit(context, suffix)
            prior = originals.get(suffix)
            state = context.user(["systemctl", "--user", "show", "--property=LoadState", "--value", unit], check=False)
            if state.stdout.strip() == "not-found":
                _require(prior is None or _original_process(context, prior) is None,
                         "fixture unit disappeared but its original process survived")
                stopped.append({"unit": unit, "load_state": "not-found", "original": prior})
                continue
            _require(state.returncode == 0 and state.stdout.strip() == "loaded", "cannot establish owned fixture unit state")
            pid = int(context.user(["systemctl", "--user", "show", "--property=MainPID", "--value", unit]).stdout.strip())
            current = _identity(pid) if pid > 1 else None
            if current is not None:
                _require(prior is not None and all(current[key] == prior[key]
                         for key in ("pid", "uid", "start_ticks", "exe", "exe_sha256")),
                         "fixture unit MainPID is not its originally admitted process")
            context.user(["systemctl", "--user", "stop", unit])
            _require(current is None or _original_process(context, current) is None, "owned fixture process survived cleanup")
            _require(prior is None or _original_process(context, prior) is None, "original fixture process survived cleanup")
            stopped.append({"unit": unit, "retired_pid": pid, "original": prior})
        receipt = {"identity": context.identity(), "fixture": str(context.fixture), "stopped": stopped, "storage": storage}
        context.save_control("system-cleanup", receipt)
        return receipt
    raise ValueError("unknown system operation: " + str(operation))
