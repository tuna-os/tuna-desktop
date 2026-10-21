"""Read-only logind user-display/unit join for the published GDM greeter.

GNOME Shell 51.1, 33da06854e229c256afd5872fedd83c28cbfa176, installs the
org.gnome.Shell@.service template with Slice=session.slice and --mode=%i.
GNOME Session 51.0, aed00cf18229a98231c23e14f5798ddb918c4956, gsm-systemd.c
uses logind User.Display because a user-service process need not belong to the
graphical session scope. GDM 51.1, 8043e3659097c4d39b2ace458cace32d94ac7693,
selects gnome-login, whose packaged target requires org.gnome.Shell@gdm.service.

The caller must first observe exactly one active local seat0 Wayland greeter
tuple and its foreground VT. This helper joins that tuple; it does not invent
direct PID/session membership or prove foreground/uniqueness by itself. All
subprocess reads use collector.call and its shared remaining deadline. No unit
is started, settings changed, bus autolaunched, or arbitrary argv exported.
An older preflight lacking the unit's immutable hash/package record is refused.
"""
from pathlib import Path
import pwd
import re

from published_dm_contract import ContractError, Inventory


UNIT = "org.gnome.Shell@gdm.service"
FRAGMENT = "/usr/lib/systemd/user/org.gnome.Shell@.service"
METHOD = "logind-user-display-and-gdm-user-unit"
PROPERTIES = ("MainPID", "ControlGroup", "FragmentPath", "DropInPaths",
              "ActiveState", "SubState")
VERSIONS = {"gdm": "51.1-1", "gnome-shell": "1:51.1-1", "gnome-session": "51.0-1"}
SOURCE_COMMITS = {
    "gdm": "8043e3659097c4d39b2ace458cace32d94ac7693",
    "gnome-shell": "33da06854e229c256afd5872fedd83c28cbfa176",
    "gnome-session": "aed00cf18229a98231c23e14f5798ddb918c4956",
}


def require(condition, reason):
    if not condition:
        raise ContractError(reason)


def account(collector, session):
    """Resolve only the UID of the already selected logind greeter tuple.

    GDM 51.1's launch-environment/dynamic-user-store use gdm-greeter (or a
    numeric suffix), not the static gdm service account. NSS is a consistency
    check, never the authority for selecting a session or accepting its unit.
    Keep the complete selected account snapshot private for repeat comparisons.
    """
    collector.remaining()
    require(type(session) is dict and session.get("Service") == "gdm-launch-environment"
            and session.get("Class") == "greeter" and session.get("Type") == "wayland"
            and session.get("Remote") == "no" and session.get("Seat") == "seat0"
            and session.get("Active") == "yes" and session.get("State") == "active",
            "caller greeter tuple differs")
    raw_uid, name = session.get("User"), session.get("Name")
    require(type(raw_uid) is str and re.fullmatch(r"[1-9][0-9]{0,9}", raw_uid),
            "invalid dynamic greeter UID")
    uid = int(raw_uid)
    require(type(name) is str, "missing dynamic greeter account name")
    match = re.fullmatch(r"gdm-greeter(?:-([1-9][0-9]{0,5}))?", name)
    require(uid < 2**32 - 1 and uid != collector.uid and match is not None
            and (match[1] is None or int(match[1]) >= 2), "wrong dynamic greeter account")
    by_uid, by_name = pwd.getpwuid(uid), pwd.getpwnam(name)
    keys = ("pw_name", "pw_uid", "pw_gid", "pw_dir", "pw_shell")
    first, second = (tuple(getattr(row, key) for key in keys) for row in (by_uid, by_name))
    require(first == second and first[:2] == (name, uid), "dynamic greeter account mapping differs")
    collector.remaining()
    return first


def properties(raw, expected):
    require(type(raw) is str and len(raw) <= 8192, "unbounded greeter unit response")
    result = {}
    for line in raw.splitlines():
        key, separator, value = line.partition("=")
        require(separator and key in expected and key not in result
                and not any(ord(c) < 32 for c in value), "malformed greeter unit response")
        result[key] = value
    require(set(result) == set(expected), "missing greeter unit property")
    return result


def display(collector, uid):
    value = properties(collector.call("/usr/bin/loginctl", "show-user", str(uid),
                                     "--property=Display"), ("Display",))["Display"]
    require(re.fullmatch(r"[A-Za-z0-9_.-]{1,64}", value), "missing logind user display")
    return value


def unit(collector, identity, pid):
    name, uid = identity[:2]
    values = properties(collector.call("/usr/bin/runuser", "-u", name, "--",
        "/usr/bin/env", "-i", "PATH=/usr/bin", "LC_ALL=C",
        f"XDG_RUNTIME_DIR=/run/user/{uid}",
        f"DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/{uid}/bus",
        "/usr/bin/systemctl", "--user", "show", UNIT, "--no-pager",
        *("--property=" + key for key in PROPERTIES)), PROPERTIES)
    expected_group = (f"/user.slice/user-{uid}.slice/user@{uid}.service/"
                      f"session.slice/{UNIT}")
    require(values["MainPID"] == str(pid) and values["ControlGroup"] == expected_group
            and values["FragmentPath"] == FRAGMENT and values["DropInPaths"] == ""
            and values["ActiveState"] == "active" and values["SubState"] == "running",
            "original GDM Shell user unit differs")
    return values


def kernel_group(collector, pid):
    collector.remaining()
    with (Path("/proc") / str(pid) / "cgroup").open("rb") as stream:
        raw = stream.read(4097)
    collector.remaining()
    require(len(raw) <= 4096, "unbounded greeter cgroup")
    lines = raw.decode("ascii").splitlines()
    require(len(lines) == 1 and lines[0].startswith("0::"),
            "greeter requires an observed unified cgroup")
    return lines[0][3:]


def mode_verified(collector, pid):
    collector.remaining()
    with (Path("/proc") / str(pid) / "cmdline").open("rb") as stream:
        raw = stream.read(1025)
    collector.remaining()
    require(raw == b"/usr/bin/gnome-shell\0--mode=gdm\0", "unreviewed greeter launch arguments")
    return True


def immutable_fragment(collector):
    expected = collector.baseline.get("files", {}).get(FRAGMENT)
    require(type(expected) is dict and expected.get("type") == "file"
            and expected.get("owner") == "gnome-shell"
            and re.fullmatch(r"[0-9a-f]{64}", expected.get("sha256", "")),
            "greeter unit missing from immutable preflight")
    # A fresh reader avoids reusing Collector.observer's cached file snapshot.
    reader = Inventory("/", lambda args: collector.call("/usr/bin/pacman", *args))
    reader.packages = collector.baseline["packages"]
    collector.remaining()
    actual = reader.required(FRAGMENT, "gnome-shell")
    collector.remaining()
    require(actual == expected, "greeter unit differs from immutable preflight")
    return {"path": FRAGMENT, "sha256": actual["sha256"], "package": "gnome-shell",
            "package_version": reader.packages["gnome-shell"], "uid": actual["uid"],
            "mode": actual["mode"]}


def join_greeter(collector, process, session):
    """Return a copy with an explicitly indirect, independently observed join.

    collector.uid is the fixture user; it must never substitute for the actual
    greeter UID. collector.process(pid, environment=True) supplies an exact shipped
    executable hash and stable kernel identity. Only allowed environment keys
    from that existing collector are compared; this module reads no environment.
    """
    require(type(process) is dict and type(session) is dict, "missing greeter join inputs")
    identity = account(collector, session)
    name, uid = identity[:2]
    pid = process.get("pid")
    sid = session.get("Id")
    require(type(uid) is int and uid > 0 and uid != collector.uid
            and type(pid) is int and 0 < pid < 2**31
            and type(process.get("start_ticks")) is int and process["start_ticks"] > 0
            and process.get("uid") == uid and process.get("executable") == "/usr/bin/gnome-shell",
            "wrong original GDM Shell principal")
    require(type(sid) is str and re.fullmatch(r"[A-Za-z0-9_.-]{1,64}", sid)
            and session.get("User") == str(uid)
            and session.get("Service") == "gdm-launch-environment"
            and session.get("Class") == "greeter" and session.get("Type") == "wayland"
            and session.get("Remote") == "no" and session.get("Seat") == "seat0"
            and session.get("Active") == "yes" and session.get("State") == "active"
            and re.fullmatch(r"[1-9][0-9]{0,3}", session.get("VTNr", "")),
            "caller greeter tuple differs")
    direct = process.get("logind_session")
    require(direct in (None, sid), "direct greeter session contradicts user display")
    require(all(collector.baseline.get("packages", {}).get(k) == v for k, v in VERSIONS.items()),
            "unreviewed greeter source versions")
    require(collector.process(pid, environment=True) == process, "original greeter process changed")
    initial_display = display(collector, uid)
    initial_unit = unit(collector, identity, pid)
    require(account(collector, session) == identity, "dynamic greeter account changed")
    initial_fragment = immutable_fragment(collector)
    require(initial_display == sid and kernel_group(collector, pid) == initial_unit["ControlGroup"],
            "greeter user-display or kernel cgroup join differs")
    mode_verified(collector, pid)
    # Take another complete independent snapshot around all file/process reads.
    final_display = display(collector, uid)
    require(account(collector, session) == identity, "dynamic greeter account changed")
    final_unit = unit(collector, identity, pid)
    require(final_display == initial_display and final_unit == initial_unit
            and immutable_fragment(collector) == initial_fragment
            and kernel_group(collector, pid) == final_unit["ControlGroup"],
            "greeter display or user unit changed")
    mode_verified(collector, pid)
    require(collector.process(pid, environment=True) == process, "original greeter process changed")
    require(account(collector, session) == identity, "dynamic greeter account changed")
    return {**process, "direct_logind_session": direct, "logind_session": initial_display,
            "logind_join_method": METHOD,
            "greeter_join": {"account": {"name": name, "uid": uid, "source": "observed-logind-greeter"},
                "display": {"uid": uid, "session_id": initial_display},
                "unit": {"name": UNIT, "main_pid": pid, "control_group": initial_unit["ControlGroup"],
                         "fragment": initial_fragment, "drop_in_paths": [], "active_state": "active",
                         "sub_state": "running"},
                "launch_mode_verified": True, "stable_observation": True,
                "source_commits": dict(SOURCE_COMMITS)}}
