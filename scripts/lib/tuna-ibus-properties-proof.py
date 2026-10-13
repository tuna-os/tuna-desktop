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
import re
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


def walk_with_ancestors(node, depth=0, budget=None, ancestors=()):
    """Yield (node, root-first ancestor tuple) under the walk limits.

    Ancestors are the traversal's own wrapper objects, so identity (`is`)
    between a match and another match's ancestors proves an actual
    parent/descendant chain without re-querying AT-SPI.
    """
    if budget is None:
        budget = [4096]
    budget[0] -= 1
    if depth > 32 or budget[0] < 0:
        raise RuntimeError("accessible depth limit")
    yield node, ancestors
    for i in range(node.get_child_count()):
        child = node.get_child_at_index(i)
        if child is not None:
            yield from walk_with_ancestors(child, depth + 1, budget, ancestors + (node,))


def validate_traffic(before, after, positive):
    if after != before + [positive]:
        raise RuntimeError("old property emitted traffic after engine change")


def component_bounds(node):
    """Screen extents of one accessible match, or None when unavailable."""
    try:
        component = node.get_component_iface()
        if component is None:
            return None
        extents = component.get_extents(Atspi.CoordType.SCREEN)
    except GLib.Error:
        return None
    return (extents.x, extents.y, extents.width, extents.height)


def action_count(node):
    """Exposed action count: 0 without an action interface, None when unreadable."""
    try:
        action = node.get_action_iface()
        if action is None:
            return 0
        return action.get_n_actions()
    except GLib.Error:
        return None


def collapse_known_menu_pair(records):
    """Resolve ONLY the known MenuButton+inner-toggle exposed pair.

    `records` holds one (node, bounds, role, n_actions, ancestors) tuple
    per same-name showing non-label match from a single bounded
    traversal; `ancestors` is that node's root-first ancestor tuple by
    traversal identity. The caller pre-filters name/showing/label.

    Returns [inner_toggle] only when the records are exactly two nodes
    with equal positive extents where one ordering is the parent outer
    button (role "button"/"push button", zero actions) above its actual
    descendant inner toggle (role "toggle button", at least one action).
    Both button spellings are the same outer widget across AT-SPI
    versions (the harness already accepts both). Every other shape —
    unknown bounds, non-positive extents, extra matches, unrelated
    equal-bounds nodes, reversed or missing ancestry, duplicate, zero,
    or unknown actions, other roles — returns all nodes unchanged so
    downstream uniqueness checks still fail ambiguous.
    """
    parent_roles = ("button", "push button")
    child_role = "toggle button"
    nodes = [node for node, _bounds, _role, _actions, _ancestors in records]
    if len(records) != 2:
        return nodes
    bounds0 = records[0][1]
    bounds1 = records[1][1]
    if bounds0 is None or bounds1 is None or bounds0 != bounds1:
        return nodes
    if len(bounds0) != 4 or bounds0[2] <= 0 or bounds0[3] <= 0:
        return nodes
    orders = ((records[0], records[1]), (records[1], records[0]))
    for parent, child in orders:
        parent_node, _pb, parent_role, parent_actions, _pa = parent
        child_node, _cb, child_role_name, child_actions, child_ancestors = child
        if parent_role not in parent_roles:
            continue
        if child_role_name != child_role:
            continue
        if parent_actions != 0:
            continue
        if child_actions is None or child_actions < 1:
            continue
        if not any(ancestor is parent_node for ancestor in child_ancestors):
            continue
        return [child_node]
    return nodes


def wait_until(predicate, deadline, clock, sleep):
    """Poll until the predicate holds while the absolute deadline holds.

    A predicate that turns true only after the deadline has passed is a
    late positive and fails: the deadline is rechecked after every
    success before returning. Raises RuntimeError on expiry.
    """
    while clock() < deadline:
        if predicate():
            if clock() >= deadline:
                break
            return
        sleep(0.05)
    raise RuntimeError("property traffic deadline")


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
    deadline = time.monotonic() + (70 if phase_file else 30)
    seen = []

    def node(name):
        if identity(pid, False) != basic_origin:
            raise RuntimeError("GTK process changed")
        desktop = Atspi.get_desktop(0)
        apps = [desktop.get_child_at_index(i) for i in range(desktop.get_child_count())]
        apps = [a for a in apps if a is not None and a.get_process_id() == pid]
        if len(apps) != 1:
            raise RuntimeError("GTK accessible application not unique")
        records = []
        for found, ancestors in walk_with_ancestors(apps[0]):
            if found.get_name() != name:
                continue
            role = found.get_role_name()
            if role == "label":
                continue
            if not found.get_state_set().contains(Atspi.StateType.SHOWING):
                continue
            records.append((found, component_bounds(found), role,
                            action_count(found), ancestors))
        return collapse_known_menu_pair(records)

    def wait(predicate):
        wait_until(predicate, deadline, time.monotonic, time.sleep)

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
    if phase_file is not None:
        def traffic():
            return [line for line in log_path.read_text().splitlines() if "property activated" in line]
        def trace():
            return [line for line in gtk_log.read_text().splitlines()
                    if line.startswith("tuna-shell-gtk: ibus properties applied ")]
        def fresh_menu():
            if not node("Input Mode"):
                press("中")
            wait(lambda: len(node("Input Mode")) == 1)
        def stale_call(action):
            try:
                return {"returned": bool(action.do_action(0)), "error": None}
            except GLib.Error as exc:
                return {"returned": None, "error": type(exc).__name__}
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
        held = node("Input Mode")[0].get_action_iface()
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
            held = node("Input Mode")[0].get_action_iface()
            checked = node("Input Mode")[0].get_state_set().contains(Atspi.StateType.CHECKED)
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
            if node("Input Mode")[0].get_state_set().contains(Atspi.StateType.CHECKED) != checked:
                raise RuntimeError("old action mutated restored checked state")
            if instance() != original_instance:
                raise RuntimeError("lock recovery recreated or changed the engine")
            outcomes.append({"edge": "lock-unlock-" + case, "old_action": result,
                "same_engine_instance": original_instance, "other_panel_not_replaced": bool(challenger),
                "traffic": traffic(), "consumer_trace": trace()[before_trace:]})
        if node("Input Mode"):
            press("中")
            wait(lambda: not node("Input Mode"))
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
    run(int(sys.argv[1]), Path(sys.argv[2]), Path(sys.argv[3]), Path(sys.argv[4]), int(sys.argv[5]),
        Path(sys.argv[6]) if len(sys.argv) > 6 else None,
        Path(sys.argv[7]) if len(sys.argv) > 7 else None)
