"""Installed window-manager gates on the marlin-vm lifecycle boot.

The installed-image proof of the parts of two `pass (nested)` rows that the
desktop gates (scripts/lib/vm_desktop_gates.py) leave nested-only:

  V-WINDOW-MENU      P-WM-05  GNOME 51's window menu on a header-bar right
                              click (G-WINDOW-MENU): its rows in GNOME's order,
                              Always on Top keeps the window above, Maximize
                              fills the work area and the menu then offers
                              Restore with Always on Top greyed out, Hide
                              minimizes and the overview brings it back,
                              Close ends the app
  V-X11-INPUT-FOCUS  P-SY-04  the X server's input focus is the focused X11
                              window (G-X11-INPUT-FOCUS, Mutter's
                              meta_x11_display_set_input_focus), so a real key
                              reaches it with the pointer elsewhere
  V-X11-ICONIC       P-SY-04  a minimized X11 window is WM_STATE IconicState
                              with _NET_WM_STATE_HIDDEN, by Super+H, the window
                              menu's Hide and its own WM_CHANGE_STATE; the
                              overview and Alt+Tab make it NormalState without
                              (G-X11-ICONIC)
  V-X11-RESTART      P-SY-04  Xwayland's loss removes its windows, keeps every
                              native window, restarts a new server on the same
                              display (and its XIM helper), which takes a new
                              client (G-X11-RESTART)
  V-X11-CLEANUP      P-SY-04  recorded at the lifecycle's Log Out (V-LOGOUT):
                              the X socket and display lock the session owned
                              while it ran are gone once it ended
                              (G-X11-CLEANUP)

The guest probe packaging/marlin/vm-lane/tuna-vm-wm-gates (wm-* actions
through tuna-vm-lifecycle) runs a plain Xlib window as the session user and
reads the X server's focus and window properties, the compositor's state
snapshot and the shell's AT-SPI tree; the GTK window is the desktop gates'
main fixture. Every pointer move, click and key is a QMP input event on the
VM's own tablet and keyboard; the guest never synthesizes input.
"""
import json
import os
import time

from vm_desktop_gates import GateError, Gates, inside, work_area
from vm_strip_acceptance import exposed_point

GATES = ("V-WINDOW-MENU", "V-X11-INPUT-FOCUS", "V-X11-ICONIC", "V-X11-RESTART")
CLEANUP_GATE = "V-X11-CLEANUP"
MENU_NAMESPACE = "tuna-window-menu"
# GNOME 51 windowMenu.js, top to bottom, for a window that is not on every
# workspace (the Move to Workspace rows depend on the workspace layout).
MENU_ORDER = ("Take Screenshot", "Hide", "Maximize", "Move", "Resize", "Always on Top",
              "Always on Visible Workspace", "Close")
X_FOCUS_NAMES = {0: "None", 1: "PointerRoot"}


# ---- observations (scripts/tests/test_vm_wm_gates.py) ----------------------

def contains(outer, inner):
    ox, oy, ow, oh = outer
    ix, iy, iw, ih = inner
    return ox <= ix and oy <= iy and ix + iw <= ox + ow and iy + ih <= oy + oh


def menu_layer(scene):
    """The shell's window-menu overlay: one layer, the whole output."""
    rects = [layer["rect"] for layer in scene.get("layers") or [] if layer.get("namespace") == MENU_NAMESPACE]
    return rects[0] if len(rects) == 1 else None


def menu_rows(observed):
    """Showing rows by label. GTK exposes a row as its button and the label
    inside it under one name; those nest and count as one row, its extents
    and sensitivity the outermost's (GTK marks only the button itself
    disabled). Two showing rows of one label that don't nest are refused."""
    rows = {}
    for name in {n.get("name") for n in observed.get("nodes") or []}:
        nodes = [n for n in observed["nodes"] if n.get("name") == name and n.get("showing")
                 and len(n.get("extents") or ()) == 4 and n["extents"][2] > 0 and n["extents"][3] > 0]
        if not nodes:
            continue
        outer = max(nodes, key=lambda n: n["extents"][2] * n["extents"][3])
        if not all(contains(outer["extents"], n["extents"]) for n in nodes):
            raise GateError(f"two separate showing {name!r} rows")
        rows[name] = {"rect": outer["extents"], "sensitive": bool(outer.get("sensitive"))}
    return rows


def menu_problems(rows, maximized):
    """GNOME 51's rows for a normal window, in order; Maximize becomes
    Restore on a maximized window, whose Always on Top is greyed out."""
    order = [("Restore" if maximized else "Maximize") if name == "Maximize" else name for name in MENU_ORDER]
    problems = []
    missing = [name for name in order if name not in rows]
    if missing:
        problems.append(f"the window menu lacks {missing}; it shows {sorted(rows)}")
    other = "Maximize" if maximized else "Restore"
    if other in rows:
        problems.append(f"a {'maximized' if maximized else 'normal'} window's menu offers {other}")
    shown = [name for name in order if name in rows]
    if [rows[name]["rect"][1] for name in shown] != sorted(rows[name]["rect"][1] for name in shown):
        problems.append(f"rows are not in GNOME's order: {sorted(shown, key=lambda n: rows[n]['rect'][1])}")
    above = rows.get("Always on Top")
    if above is not None and above["sensitive"] == maximized:
        problems.append(f"Always on Top is {'sensitive' if above['sensitive'] else 'greyed out'} "
                        f"on a {'maximized' if maximized else 'normal'} window")
    for name in shown:
        if name != "Always on Top" and not rows[name]["sensitive"]:
            problems.append(f"{name} is greyed out")
    return problems


def menu_point(rows, layer, name):
    """A row's centre on screen: the overlay's origin plus the row's
    window-relative extents."""
    row = rows.get(name)
    if row is None or not row["sensitive"]:
        raise GateError(f"the window menu has no sensitive {name!r} row")
    x, y, w, h = row["rect"]
    point = (layer[0] + x + w // 2, layer[1] + y + h // 2)
    if not inside(point, layer):
        raise GateError(f"{name!r} at {point} is outside the menu overlay {layer}")
    return point


def x_focus_problems(read, xid):
    """The X server's input focus is the focused X11 window itself, not
    PointerRoot (keys to whatever X window is under the pointer) or None."""
    focus = read.get("focus")
    if focus != xid:
        return [f"X input focus is {X_FOCUS_NAMES.get(focus, focus)}, not the focused X11 window {xid}"]
    return []


def iconic_problems(read, iconic):
    """Mutter's meta_window_x11_set_wm_state and set_net_wm_state: a hidden
    window is IconicState with _NET_WM_STATE_HIDDEN, a shown one NormalState
    without."""
    wanted = "Iconic" if iconic else "Normal"
    hidden = "_NET_WM_STATE_HIDDEN" in (read.get("net_wm_state") or [])
    problems = []
    if read.get("wm_state") != wanted:
        problems.append(f"WM_STATE is {read.get('wm_state')}, not {wanted}State")
    if hidden != iconic:
        problems.append(f"_NET_WM_STATE {'lacks' if iconic else 'has'} _NET_WM_STATE_HIDDEN")
    return problems


def native_ids(scene):
    return sorted(w["id"] for w in scene.get("windows") or [] if w.get("x11_window_id") is None)


def restart_problems(before, after, xid):
    """G-X11-RESTART on the installed session: after the server's loss its
    window is gone, every native window keeps its ID, a new Xwayland (and a
    new XIM helper, when one ran) serves the same display."""
    problems = []
    old_server = before["xwayland"]
    if any(w.get("x11_window_id") == xid for w in after["scene"].get("windows") or []):
        problems.append("the lost server's X11 window is still in the window model")
    if after["scene"].get("x11_ready") is not True:
        problems.append("the compositor's X11 window manager is not ready again")
    if len(after["xwayland"]) != 1 or after["xwayland"] == old_server:
        problems.append(f"Xwayland {old_server} was not replaced by one new server: {after['xwayland']}")
    if native_ids(after["scene"]) != native_ids(before["scene"]):
        problems.append(f"native windows changed across the restart: {native_ids(before['scene'])} -> "
                        f"{native_ids(after['scene'])}")
    if after["scene"].get("x11_display") != before["scene"].get("x11_display"):
        problems.append(f"the X display moved from {before['scene'].get('x11_display')} to "
                        f"{after['scene'].get('x11_display')}")
    if before["xim"] and (not after["xim"] or set(after["xim"]) & set(before["xim"])):
        problems.append(f"the XIM helper {before['xim']} was not replaced: {after['xim']}")
    return problems


def window_of(scene, ident):
    rows = [w for w in scene.get("windows") or [] if w.get("id") == ident]
    return rows[0] if len(rows) == 1 else None


def cleanup_problems(during, after):
    """G-X11-CLEANUP on the installed session: while it ran, the session
    user owned an X socket and its display lock (the compositor reserves
    them at startup); once it ended, the session user owns none."""
    sockets = [p for p in during if p.startswith("/tmp/.X11-unix/X")]
    locks = {f"/tmp/.X{p.removeprefix('/tmp/.X11-unix/X')}-lock" for p in sockets}
    problems = []
    if not sockets or not locks <= set(during):
        problems.append(f"the running session did not own an X socket with its display lock: {during}")
    if after:
        problems.append(f"{after} survived the session's end")
    return problems


def record_x11_cleanup(agent, during, ended, record, evidence):
    """V-X11-CLEANUP, after the lifecycle's Log Out: `during` is the guest's
    wm-x11-sockets answer (or its error) from before it, `ended` whether
    V-LOGOUT saw the session end."""
    result = {"during": during, "ended": ended}
    try:
        if not ended:
            raise GateError("the session never ended")
        if "owned" not in during:
            raise GateError("the running session's X sockets were not read: "
                            f"{during.get('error') or during.get('gates_error')}")
        # The compositor removes them in its orderly shutdown, before it exits.
        deadline = time.monotonic() + 10
        while True:
            result["after"] = agent.run("wm-x11-sockets")["owned"]
            if not result["after"] or time.monotonic() >= deadline:
                break
            time.sleep(0.5)
        problems = cleanup_problems(during["owned"], result["after"])
        if problems:
            raise GateError("; ".join(problems))
        record(CLEANUP_GATE, True,
               f"the session's X socket and display lock {during['owned']} are gone once Log Out ended it "
               "(lifecycle/x11-cleanup.json)")
    except (GateError, RuntimeError, ValueError, KeyError, TypeError, OSError) as error:
        result["error"] = f"{type(error).__name__}: {error}"
        record(CLEANUP_GATE, False, result["error"][:900])
    with open(os.path.join(evidence, "x11-cleanup.json"), "w") as stream:
        json.dump(result, stream, indent=2)


# ---- the gates on a live guest ---------------------------------------------

class WmGates(Gates):
    def __init__(self, qmp, agent, out, record, read_ppm):
        super().__init__(qmp, agent, out, record, read_ppm)
        self.evidence = os.path.join(out, "wm-gates")
        os.makedirs(self.evidence, exist_ok=True)

    def state(self, role=None):
        return self.guest("wm-state", self.key, *([role] if role else []))

    def wait_x11(self, role, predicate, timeout, message):
        return self.wait_on(lambda: self.state(role), predicate, timeout, message)

    def wait_on(self, observe, predicate, timeout, message):
        """Gates.wait over another observation than the gates' state."""
        deadline = time.monotonic() + timeout
        last = None
        while time.monotonic() < deadline:
            last = observe()
            if predicate(last):
                return last
            time.sleep(0.15)
        self.save(f"timeout-{len(self.log):03d}.json", {"message": message, "last": last})
        raise GateError(message)

    def open_menu(self, act, maximized):
        """Send `act` (a header-bar right click or Alt+Space), then the menu's
        showing rows with the overlay they are drawn on."""
        act()
        found = {}

        def shown(observed):
            rows = menu_rows(observed)
            if "Hide" in rows and "Close" in rows:
                found["rows"] = rows
                return True
            return False
        self.wait_on(lambda: self.guest("wm-menu", timeout=40), shown, 12, "the window menu never showed")
        layer = menu_layer(self.state()["scene"])
        if layer is None:
            raise GateError("the window menu's overlay layer is not mapped once")
        problems = menu_problems(found["rows"], maximized)
        if problems:
            raise GateError("; ".join(problems))
        return found["rows"], layer

    def choose(self, rows, layer, name):
        self.qmp.click(*menu_point(rows, layer, name))
        self.wait_on(lambda: self.guest("wm-menu", timeout=40), lambda o: "Hide" not in menu_rows(o), 10,
                     f"choosing {name!r} left the window menu showing")

    def pick_from_overview(self, ident):
        """Super, then a click on the window's preview, as G-WINDOW-MENU's
        and G-X11-ICONIC's pick does."""
        self.qmp.keys("meta_l")
        self.wait(lambda v: v["scene"]["overview_open"] and (v["scene"]["overview_progress"] or 0) >= 1
                  and any(p.get("id") == ident for p in v["scene"]["previews"] or []), 10,
                  "the overview never showed the window's preview")
        previews = [p for p in self.state()["scene"]["previews"] or [] if p.get("id") == ident]
        if len(previews) != 1:
            raise GateError("the window's preview is not one")
        x, y, w, h = previews[0]["rect"]
        self.qmp.click(x + w // 2, y + h // 2)
        return self.wait(lambda v: not v["scene"]["overview_open"] and v["scene"]["overview_progress"] == 0
                         and v["scene"]["focused"] == ident, 10,
                         "picking the preview did not close the overview onto the window")

    # ---- V-WINDOW-MENU --------------------------------------------------
    def window_menu(self):
        started = self.guest("gates-start", "main", self.key)
        ident, original = started["window"]["id"], started["window"]["rect"]
        value = self.wait(lambda v: v["scene"]["focused"] == ident, 10, "the GTK window never took focus")
        area = work_area(value["scene"])

        def right_click():
            rect = window_of(self.state()["scene"], ident)["rect"]
            # The header bar's empty left end (decoration layout
            # ":minimize,maximize,close"), where G-WINDOW-MENU clicks.
            self.qmp.click(rect[0] + 60, rect[1] + 20, button="right")

        rows, layer = self.open_menu(right_click, maximized=False)
        self.save("window-menu-rows.json", {"rows": rows, "layer": layer})
        self.choose(rows, layer, "Always on Top")
        self.wait(lambda v: ident in (v["scene"]["above"] or []), 10, "Always on Top did not keep the window above")
        rows, layer = self.open_menu(right_click, maximized=False)
        self.choose(rows, layer, "Maximize")
        self.wait(lambda v: (window_of(v["scene"], ident) or {}).get("rect") == area, 10,
                  f"Maximize did not fill the work area {area}")
        rows, layer = self.open_menu(right_click, maximized=True)
        self.save("window-menu-maximized-rows.json", {"rows": rows, "layer": layer})
        self.choose(rows, layer, "Restore")
        self.wait(lambda v: (window_of(v["scene"], ident) or {}).get("rect") == original, 10,
                  f"Restore did not return the window to {original}")
        rows, layer = self.open_menu(right_click, maximized=False)
        self.choose(rows, layer, "Always on Top")
        self.wait(lambda v: ident not in (v["scene"]["above"] or []), 10,
                  "Always on Top chosen again did not release the window")
        rows, layer = self.open_menu(right_click, maximized=False)
        self.choose(rows, layer, "Hide")
        self.wait(lambda v: ident in (v["scene"]["minimized"] or []) and v["scene"]["focused"] != ident, 10,
                  "Hide did not take the window away")
        self.pick_from_overview(ident)
        self.wait(lambda v: ident not in (v["scene"]["minimized"] or []), 10,
                  "picking the hidden window did not bring it back")
        rows, layer = self.open_menu(right_click, maximized=False)
        self.choose(rows, layer, "Close")
        gone = self.wait(lambda v: window_of(v["scene"], ident) is None, 15, "Close did not close the window")
        fixture = self.guest("gates-state", self.key)["fixtures"].get("main") or {}
        if fixture.get("alive"):
            raise GateError("Close removed the window but the app kept running")
        self.save("window-menu.json", gone)
        return (f"a header-bar right click opens GNOME 51's window menu ({', '.join(MENU_ORDER)} in order); "
                f"Always on Top keeps the window above and releases it; Maximize fills the work area {area} and the "
                f"menu then offers Restore with Always on Top greyed out, Restore returns {original}; Hide minimizes "
                "and the overview brings it back; Close ends the app (wm-gates/window-menu*.json)")

    # ---- V-X11-INPUT-FOCUS ----------------------------------------------
    def x11_focus(self):
        started = self.guest("wm-x11-start", "first", self.key)
        xid, ident, rect = started["fixture"]["xid"], started["window"]["id"], started["window"]["rect"]
        value = self.wait_x11("first", lambda v: v["scene"]["focused"] == ident and not x_focus_problems(v["x11"], xid),
                              10, "the mapped X11 window never took model and X input focus")
        # Off the X11 window, on the desktop: with PointerRoot focus the key
        # would go to whatever X window lies under the pointer, here none.
        away = exposed_point(work_area(value["scene"]), [rect])
        self.move(*away)
        keys = value["fixtures"]["first"]["keys"]
        self.qmp.keys("x")
        typed = self.wait_x11("first", lambda v: v["fixtures"]["first"]["keys"] > keys, 10,
                              "a real key never reached the focused X11 window with the pointer away from it")
        problems = x_focus_problems(typed["x11"], xid)
        if problems:
            raise GateError("; ".join(problems))
        self.save("x11-focus.json", typed)
        return (f"the X server's input focus is the focused X11 window {xid}, and a real key reaches it with the "
                f"pointer at {list(away)}, off it (wm-gates/x11-focus.json)")

    # ---- V-X11-ICONIC ---------------------------------------------------
    def x11_iconic(self):
        value = self.state("first")
        xid = value["fixtures"]["first"]["xid"]
        windows = [w for w in value["scene"]["windows"] or [] if w.get("x11_window_id") == xid]
        if len(windows) != 1:
            raise GateError("the X11 window is not in the window model once")
        ident = windows[0]["id"]
        steps = {}

        def expect(step, iconic, extra, message):
            try:
                seen = self.wait_x11("first", lambda v: extra(v) and not iconic_problems(v["x11"], iconic), 10,
                                     message)
            except GateError:
                raise GateError(f"{message}: {'; '.join(iconic_problems(self.state('first')['x11'], iconic))}")
            steps[step] = seen["x11"]

        hidden = lambda v: ident in (v["scene"]["minimized"] or [])
        shown = lambda v: ident not in (v["scene"]["minimized"] or [])
        expect("mapped", False, shown, "the mapped X11 window is not NormalState")
        self.qmp.keys("meta_l", "h")
        expect("super-h", True, hidden, "Super+H did not make the X11 window IconicState")
        self.pick_from_overview(ident)
        expect("overview", False, shown, "the overview's restore left the X11 window iconic")
        rows, layer = self.open_menu(lambda: self.qmp.keys("alt", "spc"), maximized=False)
        self.choose(rows, layer, "Hide")
        expect("window-menu", True, hidden, "the window menu's Hide did not make the X11 window IconicState")
        self.qmp.keys("alt", "tab")
        expect("alt-tab", False, lambda v: shown(v) and v["scene"]["focused"] == ident,
               "Alt+Tab's restore left the X11 window iconic")
        sent = self.guest("wm-x11-iconify", "first", self.key)
        if not sent.get("sent"):
            raise GateError("XIconifyWindow did not send WM_CHANGE_STATE")
        expect("wm-change-state", True, hidden, "the client's own WM_CHANGE_STATE did not make it IconicState")
        self.pick_from_overview(ident)
        expect("restored", False, shown, "restoring the iconified X11 window left it iconic")
        self.save("x11-iconic.json", steps)
        return ("Super+H, the window menu's Hide and the client's own WM_CHANGE_STATE make an X11 window "
                "WM_STATE IconicState with _NET_WM_STATE_HIDDEN; the overview and Alt+Tab make it NormalState "
                "without (wm-gates/x11-iconic.json)")

    # ---- V-X11-RESTART --------------------------------------------------
    def x11_restart(self):
        before = self.state()
        first = before["fixtures"].get("first")
        if not first or not first["alive"] or len(before["xwayland"]) != 1:
            raise GateError(f"no running X11 client on one Xwayland: {before['xwayland']}")
        xid = first["xid"]
        killed = self.guest("wm-x11-kill-server")
        if [killed["killed"]] != before["xwayland"]:
            raise GateError(f"the probe stopped Xwayland {killed['killed']}, not {before['xwayland']}")
        after = self.wait(lambda v: not restart_problems(before, v, xid)
                          and not (v["fixtures"].get("first") or {}).get("alive"), 20,
                          "Xwayland's loss did not end in a restarted server with native windows intact")
        again = self.guest("wm-x11-start", "again", self.key)
        new_ident = again["window"]["id"]
        if again["fixture"]["display"] != before["scene"]["x11_display"]:
            raise GateError("the new client did not connect on the reserved display")
        # As G-X11-RESTART: the reconnected client maps and takes focus.
        reconnected = self.wait(lambda v: v["scene"]["focused"] == new_ident and v["scene"]["x11_ready"]
                                and v["xwayland"] == after["xwayland"], 10,
                                "a new X11 client on the restarted server never took focus")
        self.save("x11-restart.json", {"before": before, "killed": killed, "after": after,
                                       "reconnected": reconnected})
        return (f"Xwayland {killed['killed']}'s loss removes its window and keeps native windows "
                f"{native_ids(before['scene'])}; a new server {after['xwayland']} on "
                f"{before['scene']['x11_display']} takes a new client, which gets focus "
                "(wm-gates/x11-restart.json)")


def run(qmp, agent, out, record, read_ppm):
    """All four gates; each records pass or fail on its own. The X11 gates
    share one client, so a failed start fails all three. Raises only when
    fixture cleanup is unconfirmed, which would leave the lifecycle's own
    window baseline changed."""
    gates = WmGates(qmp, agent, out, record, read_ppm)
    setup_error = None
    try:
        gates.gate("V-WINDOW-MENU", gates.window_menu)
        gates.gate("V-X11-INPUT-FOCUS", gates.x11_focus)
        gates.gate("V-X11-ICONIC", gates.x11_iconic)
        gates.gate("V-X11-RESTART", gates.x11_restart)
    except (GateError, RuntimeError, ValueError, KeyError, TypeError, OSError) as error:
        setup_error = f"{type(error).__name__}: {error}"
    finally:
        cleanup_error = None
        for action in ("wm-stop", "gates-stop"):
            try:
                stopped = gates.guest(action, gates.key, timeout=60)
                if stopped.get("confirmed") is not True:
                    raise GateError(f"{action}: fixture cleanup unconfirmed")
            except (GateError, RuntimeError, ValueError, KeyError, TypeError, OSError) as error:
                cleanup_error = cleanup_error or f"{type(error).__name__}: {error}"
        gates.save("steps.json", gates.log)
    for name in GATES:
        if name not in gates.recorded:
            record(name, False, f"not reached: {setup_error or cleanup_error}"[:900])
    if cleanup_error:
        raise RuntimeError(f"window-manager gate fixtures not retired: {cleanup_error}")
