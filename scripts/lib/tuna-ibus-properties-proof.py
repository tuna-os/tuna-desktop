#!/usr/bin/python3
"""Actual daemon property traffic through a PID-bound GTK accessible menu.

Run inside the existing private GTK proof session. The parent timeout bounds
AT-SPI calls as well as polling. No unrelated desktop tree is accepted.
"""
import hashlib
import json
import os
import sys
import shutil
import time
from pathlib import Path
import gi

gi.require_version("Atspi", "2.0")
gi.require_version("Gio", "2.0")
gi.require_version("IBus", "1.0")
from gi.repository import Atspi, Gio, GLib, IBus


def file_hash(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def identity(pid, with_hash=True):
    stat = Path(f"/proc/{pid}/stat").read_text().rsplit(")", 1)[1].split()
    exe = Path(f"/proc/{pid}/exe").stat()
    value = {"pid": pid, "start": int(stat[19]), "exe_dev": exe.st_dev, "exe_ino": exe.st_ino}
    if with_hash:
        value["exe_sha256"] = file_hash(Path(f"/proc/{pid}/exe"))
    return value


def walk(node, depth=0, budget=None):
    if budget is None:
        budget = [4096]
    budget[0] -= 1
    if depth > 32 or budget[0] < 0:
        raise RuntimeError("accessible depth limit")
    yield node
    for i in range(node.get_child_count()):
        child = node.get_child_at_index(i)
        if child is not None:
            yield from walk(child, depth + 1, budget)


def validate_traffic(before, after, positive):
    if after != before + [positive]:
        raise RuntimeError("old property emitted traffic after engine change")


def run(pid, log_path, output, expected_shell, engine_pid):
    expected_hash = file_hash(expected_shell)
    origin = identity(pid)
    if origin["exe_sha256"] != expected_hash:
        raise RuntimeError("GTK executable does not match admitted harness path")
    engine_origin = identity(engine_pid)
    engine_fixture = Path(__file__).with_name("tuna-ibus-test-engine.py")
    if str(engine_fixture).encode() not in Path(f"/proc/{engine_pid}/cmdline").read_bytes().split(b"\0"):
        raise RuntimeError("engine process is not the owned fixture")
    engine_fixture_hash = file_hash(engine_fixture)
    basic_origin = {k: v for k, v in origin.items() if k != "exe_sha256"}
    deadline = time.monotonic() + 30
    seen = []

    def node(name):
        if identity(pid, False) != basic_origin:
            raise RuntimeError("GTK process changed")
        desktop = Atspi.get_desktop(0)
        apps = [desktop.get_child_at_index(i) for i in range(desktop.get_child_count())]
        apps = [a for a in apps if a is not None and a.get_process_id() == pid]
        if len(apps) != 1:
            raise RuntimeError("GTK accessible application not unique")
        matches = [n for n in walk(apps[0]) if n.get_name() == name
                   and n.get_state_set().contains(Atspi.StateType.SHOWING)
                   and n.get_role_name() != "label"]
        return matches

    def wait(predicate):
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(.05)
        raise RuntimeError("property traffic deadline")

    def press(name):
        wait(lambda: len(node(name)) == 1)
        selected = node(name)
        if len(selected) != 1:
            raise RuntimeError("ambiguous property action")
        action = selected[0].get_action_iface()
        if action is None or action.get_n_actions() < 1 or not action.do_action(0):
            raise RuntimeError("accessible action rejected")
        seen.append(name)

    def heard(line):
        return line in log_path.read_text().splitlines()

    press("中")
    wait(lambda: len(node("Input Mode")) == 1)
    if node("Proof Hidden"):
        raise RuntimeError("hidden property exposed")
    disabled = node("Proof Disabled")
    if len(disabled) != 1 or disabled[0].get_state_set().contains(Atspi.StateType.SENSITIVE):
        raise RuntimeError("disabled property is actionable")
    press("Input Mode")
    wait(lambda: heard("tuna-ibus-test-engine: property activated InputMode 1 engine=tuna-test"))
    press("Proof Radio B")
    wait(lambda: heard("tuna-ibus-test-engine: property activated proof-radio-a 0 engine=tuna-test")
         and heard("tuna-ibus-test-engine: property activated proof-radio-b 1 engine=tuna-test"))
    press("Proof Submenu")
    press("Proof Action")
    wait(lambda: heard("tuna-ibus-test-engine: property activated proof-action 0 engine=tuna-test"))

    # A real second bus peer cannot forge the daemon's reserved sender. This
    # calls the production registered object (including its shared dispatcher).
    IBus.init()
    bus = IBus.Bus()
    if not bus.is_connected():
        raise RuntimeError("owned IBus bus absent")
    conn = bus.get_connection()
    stream = conn.get_stream()
    if not isinstance(stream, Gio.SocketConnection):
        raise RuntimeError("IBus transport has no measured socket")
    credentials = stream.get_socket().get_credentials()
    if credentials.get_unix_user() != os.geteuid():
        raise RuntimeError("IBus peer UID mismatch")
    daemon_origin = identity(credentials.get_unix_pid())
    expected_daemon = shutil.which("ibus-daemon")
    if expected_daemon is None or file_hash(Path(expected_daemon)) != daemon_origin["exe_sha256"]:
        raise RuntimeError("IBus peer executable mismatch")
    owner = conn.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus",
                          "org.freedesktop.DBus", "GetNameOwner",
                          GLib.Variant("(s)", ("org.freedesktop.IBus.Panel",)),
                          GLib.VariantType.new("(s)"), Gio.DBusCallFlags.NONE, 2000, None).unpack()[0]
    for method, params in [
            ("FocusOut", GLib.Variant("(o)", ("/forged/context",))),
            ("RegisterProperties", GLib.Variant("(v)", (GLib.Variant("s", "forged"),)))]:
        try:
            conn.call_sync(owner, "/org/freedesktop/IBus/Panel", "org.freedesktop.IBus.Panel",
                           method, params, None, Gio.DBusCallFlags.NONE, 2000, None)
        except GLib.Error as exc:
            if Gio.DBusError.get_remote_error(exc) != "org.freedesktop.DBus.Error.AccessDenied":
                raise
        else:
            raise RuntimeError("forged panel method accepted")
    stale = node("Input Mode")
    if len(stale) != 1:
        raise RuntimeError("old property control missing")
    stale_action = stale[0].get_action_iface()
    if stale_action is None:
        raise RuntimeError("old property action missing")
    before = [line for line in log_path.read_text().splitlines() if "property activated" in line]

    def engine(name):
        complete = []
        def done(bus, result):
            complete.append(bus.set_global_engine_async_finish(result))
        bus.set_global_engine_async(name, 2000, None, done)
        context = GLib.MainContext.default()
        while not complete and time.monotonic() < deadline:
            while context.pending():
                context.iteration(False)
            time.sleep(.01)
        if complete != [True]:
            raise RuntimeError("engine switch did not complete")

    engine("tuna-test-alternate")
    wait(lambda: any(n.get_name() == "tuna-test-alternate" for n in walk(
        next(a for a in [Atspi.get_desktop(0).get_child_at_index(i)
            for i in range(Atspi.get_desktop(0).get_child_count())]
            if a is not None and a.get_process_id() == pid))))
    # The old handler may report handled even though its epoch gate suppresses
    # effects. Follow it with a fresh action through the SAME actual GTK action
    # producer and await the daemon engine's ordered callback/log, not an
    # unrelated D-Bus round trip or an arbitrary quiet sleep.
    stale_result = None
    stale_error = None
    try:
        stale_result = bool(stale_action.do_action(0))
    except GLib.Error as exc:
        stale_error = type(exc).__name__
    press("Proof Submenu")
    press("Proof Action")
    positive = "tuna-ibus-test-engine: property activated proof-action 0 engine=tuna-test-alternate"
    wait(lambda: heard(positive))
    after = [line for line in log_path.read_text().splitlines() if "property activated" in line]
    validate_traffic(before, after, positive)
    current = node("Input Mode")
    if len(current) != 1 or current[0].get_state_set().contains(Atspi.StateType.CHECKED):
        raise RuntimeError("old property mutated current context state")
    engine("tuna-test")
    if (identity(pid) != origin or file_hash(expected_shell) != expected_hash
            or identity(engine_pid) != engine_origin
            or file_hash(engine_fixture) != engine_fixture_hash
            or identity(daemon_origin["pid"]) != daemon_origin):
        raise RuntimeError("owned process/source changed at receipt")
    output.write_text(json.dumps({"scope": "nested live IBus daemon + actual GTK accessibility actions",
        "gtk": origin, "expected_shell_sha256": expected_hash,
        "daemon": daemon_origin, "engine": engine_origin,
        "engine_fixture_sha256": engine_fixture_hash,
        "bus_guid": conn.get_guid(), "panel_owner": owner,
        "actions": seen, "forged_sender_denied": True,
        "engine_change_old_accessible_no_effect": True,
        "old_accessible_return": stale_result, "old_accessible_error": stale_error,
        "ordered_positive_callback": positive, "actual_property_traffic": after,
        "hidden_absent": True, "disabled_not_sensitive": True,
        "source_sha": os.environ["TUNA_PROOF_SOURCE_SHA"],
        "installed_or_all_ime_qualified": False}, indent=2) + "\n")


if __name__ == "__main__":
    run(int(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3]), Path(sys.argv[4]), int(sys.argv[5]))
