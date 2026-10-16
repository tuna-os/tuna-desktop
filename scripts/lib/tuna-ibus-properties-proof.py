#!/usr/bin/python3
"""Actual daemon property traffic through a PID-bound GTK accessible menu.

Run inside the existing private GTK proof session. The parent timeout bounds
AT-SPI calls as well as polling. No unrelated desktop tree is accepted.
"""
import faulthandler
import hashlib
import json
import os
import sys
import shutil
import re
import time
from pathlib import Path
import signal
import subprocess
# The harness's own phase deadline can expire while this driver still waits;
# its SIGTERM then dumps where it waited into the lifecycle log.
faulthandler.register(signal.SIGTERM, chain=True)
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


def live(node):
    """A reference libatspi can still query.

    libatspi disposes an application object when a NameOwnerChanged names its
    bus name as the old owner, but keeps the disposed object in its process-wide
    table: every later reference resolved through it fails for the rest of that
    process ('The application no longer exists', with 'hash_table != NULL'
    warnings). gtk-shell 116720252865 retried such a walk until its deadline,
    and gtk-shell 116769499648 saw the shell's own application disposed this
    way right after the popover opened. So every query and press runs in a
    fresh short-lived process (atspi_child), like tuna-a11y-dump.py, and this
    long-lived driver never holds libatspi state. A defunct reference is not
    part of the live tree. The owned shell's own exit or replacement still
    fails through identity() and the unique live application check.
    """
    try:
        node.get_role()
        node.get_child_count()
        return True
    except GLib.Error:
        return False


def walk(node, depth=0, budget=None):
    if budget is None:
        budget = [4096]
    budget[0] -= 1
    if depth > 32 or budget[0] < 0:
        raise RuntimeError("accessible depth limit")
    yield node
    for i in range(node.get_child_count()):
        child = node.get_child_at_index(i)
        if child is not None and live(child):
            yield from walk(child, depth + 1, budget)


def validate_traffic(before, after, positive):
    if after != before + [positive]:
        raise RuntimeError("old property emitted traffic after engine change")


# gtk::MenuButton exposes an outer "button" wrapping an inner "toggle button",
# both carrying the indicator's name (the clock and System menus have the same
# shape); the inner toggle button is the actionable control.
INDICATOR = ("中", "toggle button")


def matching(nodes, name, role=None):
    return [n for n in nodes if n.get_name() == name
            and n.get_state_set().contains(Atspi.StateType.SHOWING)
            and n.get_role_name() != "label"
            and (role is None or n.get_role_name() == role)]


class Transient(Exception):
    """An AT-SPI walk met a node disposed under it; the caller may retry."""


def atspi_child(pid, op, name, role):
    """One fresh-process AT-SPI query or press against the owned GTK app.

    Prints one JSON object. `matches` are the showing nodes named `name`
    (any node for op "named"), each with its accessible address so the parent
    can later call a held, possibly stale action over the bus directly.
    """
    def describe(n):
        states = n.get_state_set()
        return {"bus": n.app.bus_name, "path": n.path, "role": n.get_role_name(),
                "showing": states.contains(Atspi.StateType.SHOWING),
                "checked": states.contains(Atspi.StateType.CHECKED),
                "sensitive": states.contains(Atspi.StateType.SENSITIVE)}
    try:
        desktop = Atspi.get_desktop(0)
        apps = [desktop.get_child_at_index(i) for i in range(desktop.get_child_count())]
        apps = [a for a in apps if a is not None and live(a) and a.get_process_id() == pid]
        if len(apps) != 1:
            return {"error": f"GTK accessible application not unique ({len(apps)} live)"}
        if op == "named":
            found = [n for n in walk(apps[0]) if n.get_name() == name]
        else:
            found = matching(walk(apps[0]), name, role)
        result = {"matches": [describe(n) for n in found]}
        if op == "press":
            if len(found) != 1:
                return {"error": "ambiguous property action"}
            action = found[0].get_action_iface()
            if action is None or action.get_n_actions() < 1 or not action.do_action(0):
                return {"error": "accessible action rejected"}
        return result
    except GLib.Error as error:
        return {"transient": error.message}


def atspi(pid, op, name, role=None):
    try:
        done = subprocess.run(
            [sys.executable, __file__, "--atspi", str(pid), op, name, role or ""],
            capture_output=True, text=True, timeout=15)
    except subprocess.TimeoutExpired:
        raise Transient(f"AT-SPI {op} {name!r} timed out")
    try:
        reply = json.loads(done.stdout.strip().splitlines()[-1])
    except (IndexError, json.JSONDecodeError):
        raise RuntimeError(f"AT-SPI child {op} {name!r} exit={done.returncode}: {done.stderr[-2000:]}")
    if "transient" in reply:
        raise Transient(reply["transient"])
    if "error" in reply:
        raise RuntimeError(reply["error"])
    return reply["matches"]


def run(pid, log_path, output, expected_shell, engine_pid, phase_file=None, gtk_log=None):
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
    # The lifecycle drives two real lock/PAM unlock cycles (each phase is also
    # bounded by the harness); with a fresh AT-SPI process per query the whole
    # proof ran past 70 s at its final close (gtk-shell 116808314378).
    deadline = time.monotonic() + (150 if phase_file else 30)
    seen = []

    def node(name, role=None):
        if identity(pid, False) != basic_origin:
            raise RuntimeError("GTK process changed")
        return atspi(pid, "query", name, role)

    def wait(predicate):
        last_error = None
        while time.monotonic() < deadline:
            try:
                if predicate():
                    return
            except Transient as error:
                # GTK rebuilds the menu on every property update; a walk can
                # meet a child disposed under it ("The application no longer
                # exists", gtk-shell 38757056152). node() rechecks the GTK
                # process identity first, so a real exit still fails.
                last_error = str(error)
            except GLib.Error as error:
                # Bus predicates in this process, e.g. the panel name briefly
                # unowned between the challenger's release and our reclaim
                # (journey 116793877779: NameHasNoOwner).
                last_error = error.message
            time.sleep(.05)
        raise RuntimeError(f"property traffic deadline after presses {seen}; "
                           f"last AT-SPI error: {last_error}")

    def press(name, role=None):
        wait(lambda: len(node(name, role)) == 1)
        if identity(pid, False) != basic_origin:
            raise RuntimeError("GTK process changed")
        atspi(pid, "press", name, role)
        seen.append(name)

    def heard(line):
        return line in log_path.read_text().splitlines()

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
    # The same bus libatspi resolves: AT_SPI_BUS_ADDRESS, else org.a11y.Bus.
    a11y_address = os.environ.get("AT_SPI_BUS_ADDRESS") or Gio.bus_get_sync(
        Gio.BusType.SESSION, None).call_sync("org.a11y.Bus", "/org/a11y/bus", "org.a11y.Bus",
        "GetAddress", None, GLib.VariantType.new("(s)"), Gio.DBusCallFlags.NONE, 2000,
        None).unpack()[0]
    a11y = Gio.DBusConnection.new_for_address_sync(a11y_address,
        Gio.DBusConnectionFlags.AUTHENTICATION_CLIENT
        | Gio.DBusConnectionFlags.MESSAGE_BUS_CONNECTION, None, None)

    def stale_call(held):
        # The held address's own Action, as a held libatspi reference would
        # call it; no lookup can redirect it to the fresh menu.
        try:
            returned = a11y.call_sync(held["bus"], held["path"], "org.a11y.atspi.Action",
                "DoAction", GLib.Variant("(i)", (0,)), GLib.VariantType.new("(b)"),
                Gio.DBusCallFlags.NONE, 2000, None).unpack()[0]
            return {"returned": bool(returned), "error": None}
        except GLib.Error as exc:
            return {"returned": None, "error": type(exc).__name__}

    if phase_file is not None:
        def traffic():
            return [line for line in log_path.read_text().splitlines() if "property activated" in line]
        def trace():
            return [line for line in gtk_log.read_text().splitlines()
                    if line.startswith("tuna-shell-gtk: ibus properties applied ")]
        def fresh_menu():
            if not node("Input Mode"):
                press(*INDICATOR)
            wait(lambda: len(node("Input Mode")) == 1)
        def ordered_fresh(before):
            fresh_menu()
            press("Proof Submenu")
            press("Proof Action")
            positive = "tuna-ibus-test-engine: property activated proof-action 0 engine=tuna-test"
            wait(lambda: len(traffic()) >= len(before) + 1)
            validate_traffic(before, traffic(), positive)
        def instance():
            return [line for line in log_path.read_text().splitlines()
                    if line.startswith("tuna-ibus-test-engine: property instance engine=tuna-test ")][-1]
        session = Gio.bus_get_sync(Gio.BusType.SESSION, None)
        ss_owner = session.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus",
            "org.freedesktop.DBus", "GetNameOwner", GLib.Variant("(s)", ("org.gnome.ScreenSaver",)),
            GLib.VariantType.new("(s)"), Gio.DBusCallFlags.NONE, 2000, None).unpack()[0]
        ss_pid = session.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus",
            "org.freedesktop.DBus", "GetConnectionUnixProcessID", GLib.Variant("(s)", (ss_owner,)),
            GLib.VariantType.new("(u)"), Gio.DBusCallFlags.NONE, 2000, None).unpack()[0]
        if ss_pid != pid:
            raise RuntimeError("lock service not owned by admitted GTK")
        def screensaver(method):
            return session.call_sync(ss_owner, "/org/gnome/ScreenSaver", "org.gnome.ScreenSaver",
                method, None, None, Gio.DBusCallFlags.NONE, 2000, None)
        def active():
            return screensaver("GetActive").unpack()[0]
        def panel_owner():
            return conn.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus",
                "org.freedesktop.DBus", "GetNameOwner", GLib.Variant("(s)", ("org.freedesktop.IBus.Panel",)),
                GLib.VariantType.new("(s)"), Gio.DBusCallFlags.NONE, 2000, None).unpack()[0]
        outcomes = []
        fresh_menu()
        ordered_fresh(traffic())
        original_instance = instance()

        # The same context object is used twice. These are real daemon Focus
        # methods; a fresh consumer RegisterProperties trace is mandatory.
        context = bus.create_input_context("tuna-property-lifecycle")
        context.set_capabilities(IBus.Capabilite.FOCUS)
        context_sha = hashlib.sha256(context.get_object_path().encode()).hexdigest()
        def registered(since):
            focus = None
            for line in trace()[since:]:
                match = re.fullmatch(r"tuna-shell-gtk: ibus properties applied (\w+) epoch=(\d+) focus=(\d+) context_sha=(\w+)", line)
                if not match or match[4] != context_sha:
                    continue
                if match[1] == "FocusIn":
                    focus = match[3]
                if match[1] == "RegisterProperties" and focus is not None and match[3] == focus:
                    return True
            return False
        before_trace = len(trace())
        context.focus_in()
        wait(lambda: registered(before_trace))
        fresh_menu()
        held = node("Input Mode")[0]
        before = traffic(); before_trace = len(trace())
        context.focus_out(); context.focus_in()
        wait(lambda: registered(before_trace))
        result = stale_call(held); ordered_fresh(before)
        outcomes.append({"edge": "same-context reentry", "old_action": result,
                         "traffic": traffic(), "consumer_trace": trace()[before_trace:]})
        before_trace = len(trace())
        context.focus_out(); context.destroy()
        wait(lambda: len(trace()) > before_trace and "RegisterProperties" in trace()[-1])
        fresh_menu()

        for case in ("normal", "other-panel"):
            held = node("Input Mode")[0]
            checked = held["checked"]
            before = traffic(); before_trace = len(trace())
            screensaver("Lock"); wait(active)
            if node("中") or node("Input Mode"):
                raise RuntimeError("property menu exposed while locked")
            challenger = None
            if case == "other-panel":
                # This is the driver's real second bus peer. Its takeover is
                # deliberate, and production refresh must NOT replace it.
                challenger = conn.get_unique_name()
                code = conn.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus",
                    "org.freedesktop.DBus", "RequestName",
                    GLib.Variant("(su)", ("org.freedesktop.IBus.Panel", 2 | 4)),
                    GLib.VariantType.new("(u)"), Gio.DBusCallFlags.NONE, 2000, None).unpack()[0]
                if code != 1 or panel_owner() != challenger:
                    raise RuntimeError("challenger did not acquire owned test panel")
            # The parent drives the existing real nested lock UI using its
            # private pam_wrapper credential; the helper never sees that value.
            phase_file.write_text(json.dumps({"phase": "locked-" + case, "pid": os.getpid(),
                "gtk": basic_origin, "source": os.environ["TUNA_PROOF_SOURCE_SHA"]}) + "\n")
            wait(lambda: not active())
            if case == "other-panel":
                wait(lambda: any("another panel owns" in line for line in gtk_log.read_text().splitlines()))
                if panel_owner() != challenger or node("中") or node("Input Mode"):
                    raise RuntimeError("refresh stole another panel or exposed its properties")
            result = stale_call(held)
            if challenger is not None:
                code = conn.call_sync("org.freedesktop.DBus", "/org/freedesktop/DBus",
                    "org.freedesktop.DBus", "ReleaseName",
                    GLib.Variant("(s)", ("org.freedesktop.IBus.Panel",)),
                    GLib.VariantType.new("(u)"), Gio.DBusCallFlags.NONE, 2000, None).unpack()[0]
                if code != 1:
                    raise RuntimeError("challenger release not confirmed")
                wait(lambda: panel_owner() == owner)
            fresh_menu(); ordered_fresh(before)
            if node("Input Mode")[0]["checked"] != checked:
                raise RuntimeError("old action mutated restored checked state")
            if instance() != original_instance:
                raise RuntimeError("lock recovery recreated or changed the engine")
            outcomes.append({"edge": "lock-unlock-" + case, "old_action": result,
                "same_engine_instance": original_instance, "other_panel_not_replaced": bool(challenger),
                "traffic": traffic(), "consumer_trace": trace()[before_trace:]})
        # The menu stays open: pressing the indicator's toggle while its own
        # popover is open never matched a unique showing node (gtk-shell
        # 116828879015). The harness closes it with real Escape afterward.
        if (identity(pid) != origin or file_hash(expected_shell) != expected_hash
                or identity(daemon_origin["pid"]) != daemon_origin
                or identity(engine_pid) != engine_origin
                or file_hash(engine_fixture) != engine_fixture_hash):
            raise RuntimeError("process/source changed during lifecycle proof")
        output.write_text(json.dumps({"scope": "nested real daemon context/owner and real GTK lock lifecycle",
            "gtk": origin, "daemon": daemon_origin, "engine": engine_origin,
            "source_sha": os.environ["TUNA_PROOF_SOURCE_SHA"], "outcomes": outcomes,
            "installed_or_all_ime_qualified": False}, indent=2) + "\n")
        return

    press(*INDICATOR)
    wait(lambda: len(node("Input Mode")) == 1)
    if node("Proof Hidden"):
        raise RuntimeError("hidden property exposed")
    disabled = node("Proof Disabled")
    if len(disabled) != 1 or disabled[0]["sensitive"]:
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
    stale_action = stale[0]
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
    wait(lambda: bool(atspi(pid, "named", "tuna-test-alternate")))
    # The old handler may report handled even though its epoch gate suppresses
    # effects. Follow it with a fresh action through the SAME actual GTK action
    # producer and await the daemon engine's ordered callback/log, not an
    # unrelated D-Bus round trip or an arbitrary quiet sleep.
    stale_result = stale_call(stale_action)
    stale_error = stale_result["error"]
    stale_result = stale_result["returned"]
    press("Proof Submenu")
    press("Proof Action")
    positive = "tuna-ibus-test-engine: property activated proof-action 0 engine=tuna-test-alternate"
    wait(lambda: heard(positive))
    after = [line for line in log_path.read_text().splitlines() if "property activated" in line]
    validate_traffic(before, after, positive)
    current = node("Input Mode")
    if len(current) != 1 or current[0]["checked"]:
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
    if sys.argv[1] == "--atspi":
        print(json.dumps(atspi_child(int(sys.argv[2]), sys.argv[3], sys.argv[4], sys.argv[5] or None)))
        sys.exit(0)
    run(int(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3]), Path(sys.argv[4]), int(sys.argv[5]),
        Path(sys.argv[6]) if len(sys.argv) > 6 else None,
        Path(sys.argv[7]) if len(sys.argv) > 7 else None)
