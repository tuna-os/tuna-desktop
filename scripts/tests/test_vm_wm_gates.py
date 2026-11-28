"""Harness logic of the installed window-manager gates (V-WINDOW-MENU,
V-X11-INPUT-FOCUS, V-X11-ICONIC, V-X11-RESTART, V-X11-CLEANUP;
scripts/lib/vm_wm_gates.py).

The checkers are tested against observations of the behaviour each gate
proves and of the behaviour before it existed (X input focus left at
PointerRoot, WM_STATE never set to IconicState, no Xwayland restart, no
window menu, X socket and lock left behind). The four lifecycle gates then
run end to end against a simulated session, once as Tuna Desktop behaves
and once per old behaviour: each old behaviour fails its own gate and
leaves the others passing (the window menu also serves V-X11-ICONIC's Hide
step). The guest's Xlib client and reader run against a real X server
(Xvfb) when one is installed.
"""
import contextlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts" / "lib"))
import vm_desktop_gates as base  # noqa: E402
import vm_wm_gates as wm  # noqa: E402

GUEST = ROOT / "packaging" / "marlin" / "vm-lane" / "tuna-vm-wm-gates"
PRIMARY = {"name": "Virtual-1", "primary": True, "rect": [0, 0, 1280, 800]}
BAR = {"namespace": "tuna-panel", "rect": [0, 0, 1280, 32]}
LAYER = [0, 0, 1280, 800]
ROWS = ("Take Screenshot", "Hide", "Maximize", "Move", "Resize", "Always on Top",
        "Always on Visible Workspace", "Move to Workspace Right", "Close")


def menu_nodes(x, y, maximized=False, labels=ROWS, insensitive=()):
    """The shell's window menu as AT-SPI shows it: each row a button and the
    label inside it, both named by the row."""
    nodes = []
    for index, label in enumerate(labels):
        if label == "Maximize" and maximized:
            label = "Restore"
        sensitive = label not in insensitive and not (label == "Always on Top" and maximized)
        rect = [x, y + 7 + index * 30, 220, 30]
        nodes.append({"name": label, "role": "button", "showing": True, "sensitive": sensitive, "extents": rect})
        nodes.append({"name": label, "role": "label", "showing": True, "sensitive": True,
                      "extents": [rect[0] + 30, rect[1] + 6, 120, 18]})
    return {"nodes": nodes}


class Menu(unittest.TestCase):
    def test_rows_nest_button_and_label_and_take_the_buttons_state(self):
        rows = wm.menu_rows(menu_nodes(100, 100, maximized=True))
        self.assertEqual(rows["Hide"]["rect"], [100, 137, 220, 30])
        self.assertFalse(rows["Always on Top"]["sensitive"])
        self.assertTrue(rows["Restore"]["sensitive"])
        apart = menu_nodes(100, 100)
        apart["nodes"].append({**apart["nodes"][2], "extents": [600, 600, 220, 30]})
        with self.assertRaises(base.GateError):
            wm.menu_rows(apart)

    def test_gnome_51_rows_in_order(self):
        self.assertEqual(wm.menu_problems(wm.menu_rows(menu_nodes(100, 100)), False), [])
        self.assertEqual(wm.menu_problems(wm.menu_rows(menu_nodes(100, 100, maximized=True)), True), [])

    def test_wrong_menus_are_refused(self):
        normal = wm.menu_rows(menu_nodes(100, 100))
        # A menu without GNOME's rows: no window menu at all, or an app's own.
        self.assertTrue(wm.menu_problems({}, False))
        self.assertTrue(wm.menu_problems(wm.menu_rows(menu_nodes(100, 100, labels=ROWS[1:])), False))
        # Maximized but still offering Maximize, Always on Top not greyed out.
        self.assertTrue(wm.menu_problems(normal, True))
        self.assertTrue(wm.menu_problems(wm.menu_rows(menu_nodes(100, 100, maximized=True)), False))
        # Out of order, or a row greyed out that GNOME offers.
        shuffled = dict(normal, Close={"rect": [100, 50, 220, 30], "sensitive": True})
        self.assertTrue(any("order" in p for p in wm.menu_problems(shuffled, False)))
        greyed = wm.menu_rows(menu_nodes(100, 100, insensitive=("Hide",)))
        self.assertTrue(any("Hide is greyed out" in p for p in wm.menu_problems(greyed, False)))

    def test_row_point_is_the_overlay_origin_plus_extents(self):
        rows = wm.menu_rows(menu_nodes(100, 100))
        self.assertEqual(wm.menu_point(rows, [1280, 0, 1280, 800], "Hide"), (1490, 152))
        with self.assertRaises(base.GateError):
            wm.menu_point(wm.menu_rows(menu_nodes(100, 100, maximized=True)), LAYER, "Always on Top")
        with self.assertRaises(base.GateError):
            wm.menu_point(rows, [0, 0, 200, 200], "Close")
        self.assertEqual(wm.menu_layer({"layers": [BAR, {"namespace": wm.MENU_NAMESPACE, "rect": LAYER}]}), LAYER)
        self.assertIsNone(wm.menu_layer({"layers": [BAR]}))


class X11(unittest.TestCase):
    def test_x_focus_is_the_window_itself(self):
        self.assertEqual(wm.x_focus_problems({"focus": 0x200001}, 0x200001), [])
        # Before: PointerRoot, keys go to whatever X window is under the pointer.
        self.assertIn("PointerRoot", wm.x_focus_problems({"focus": 1}, 0x200001)[0])
        self.assertIn("None", wm.x_focus_problems({"focus": 0}, 0x200001)[0])
        self.assertTrue(wm.x_focus_problems({"focus": 0x400001}, 0x200001))

    def test_iconic_state_follows_minimize(self):
        iconic = {"wm_state": "Iconic", "net_wm_state": ["_NET_WM_STATE_HIDDEN"]}
        normal = {"wm_state": "Normal", "net_wm_state": ["_NET_WM_STATE_FOCUSED"]}
        self.assertEqual(wm.iconic_problems(iconic, True), [])
        self.assertEqual(wm.iconic_problems(normal, False), [])
        # Before #1078: a minimized X11 window was never told so.
        self.assertEqual(len(wm.iconic_problems(normal, True)), 2)
        self.assertTrue(wm.iconic_problems({"wm_state": "Iconic", "net_wm_state": []}, True))
        self.assertTrue(wm.iconic_problems({"wm_state": "Normal", "net_wm_state": ["_NET_WM_STATE_HIDDEN"]}, False))
        self.assertTrue(wm.iconic_problems({"wm_state": None, "net_wm_state": []}, False))

    def observed(self, xwayland, windows, ready=True, display=":0", xim=()):
        return {"xwayland": list(xwayland), "xim": list(xim),
                "scene": {"x11_ready": ready, "x11_display": display, "windows": windows}}

    def test_restart_replaces_the_server_and_keeps_native_windows(self):
        native = [{"id": 1, "x11_window_id": None}, {"id": 2, "x11_window_id": None}]
        before = self.observed([500], native + [{"id": 7, "x11_window_id": 77}], xim=[501])
        good = self.observed([600], native, xim=[601])
        self.assertEqual(wm.restart_problems(before, good, 77), [])
        cases = {
            "no restart": self.observed([], native, ready=False, xim=[]),
            "same server": self.observed([500], native, xim=[601]),
            "stale X11 window": self.observed([600], native + [{"id": 7, "x11_window_id": 77}], xim=[601]),
            "native window lost": self.observed([600], native[:1], xim=[601]),
            "display moved": self.observed([600], native, display=":1", xim=[601]),
            "XIM not restarted": self.observed([600], native, xim=[]),
            "old XIM survived": self.observed([600], native, xim=[501, 601]),
        }
        for name, after in cases.items():
            with self.subTest(name):
                self.assertTrue(wm.restart_problems(before, after, 77))
        # No XIM helper before: none is required after.
        self.assertEqual(wm.restart_problems({**before, "xim": []}, self.observed([600], native), 77), [])


class Cleanup(unittest.TestCase):
    RUNNING = ["/tmp/.X11-unix/X2", "/tmp/.X2-lock"]

    def test_the_sessions_socket_and_lock_go_with_it(self):
        self.assertEqual(wm.cleanup_problems(self.RUNNING, []), [])
        # Before G-X11-CLEANUP: the socket, the lock, or both outlive the session.
        for left in (self.RUNNING, self.RUNNING[:1], self.RUNNING[1:]):
            with self.subTest(left):
                self.assertTrue(wm.cleanup_problems(self.RUNNING, left))
        # Nothing reserved while it ran proves nothing about its end.
        self.assertTrue(wm.cleanup_problems([], []))
        self.assertTrue(wm.cleanup_problems(self.RUNNING[:1], []))

    def record(self, during, ended, afters):
        replies = iter(afters)
        agent = type("Agent", (), {"run": lambda self, action: {"owned": next(replies)}})()
        records = {}
        with tempfile.TemporaryDirectory() as evidence, simulated_time():
            wm.record_x11_cleanup(agent, during, ended, lambda n, ok, d: records.__setitem__(n, (ok, d)), evidence)
            saved = json.loads(Path(evidence, "x11-cleanup.json").read_text())
        return records[wm.CLEANUP_GATE], saved

    def test_recorded_after_log_out(self):
        (ok, _), saved = self.record({"owned": self.RUNNING}, True, [self.RUNNING[1:], []])
        self.assertTrue(ok)
        self.assertEqual(saved["after"], [])
        (ok, detail), _ = self.record({"owned": self.RUNNING}, True, [self.RUNNING] * 40)
        self.assertFalse(ok)
        self.assertIn("survived", detail)
        (ok, detail), _ = self.record({"owned": self.RUNNING}, False, [])
        self.assertFalse(ok)
        self.assertIn("never ended", detail)
        (ok, detail), _ = self.record({"gates_error": "RuntimeError: no tmp"}, True, [])
        self.assertFalse(ok)
        self.assertIn("no tmp", detail)


# ---- a simulated session ---------------------------------------------------

class Session:
    """A compositor, shell and X server as the gates observe them through
    the guest probe, driven only by the lane's QMP input. `old` names one
    behaviour from before the gated feature."""
    def __init__(self, old=None):
        self.old = old
        self.native = [{"id": i, "app_id": f"native{i}", "x11_window_id": None, "workspace": 0,
                        "rect": [40 * i, 60, 400, 300]} for i in (1, 2, 3)]
        self.windows = list(self.native)
        self.focused, self.mru = 3, [3, 2, 1]
        self.minimized, self.above = set(), set()
        self.overview = False
        self.menu = None  # (window id, x, y)
        self.x11 = {}  # role -> fixture
        self.xwayland, self.next_pid, self.next_id = [], 900, 20
        self.ready = True
        self.gtk_alive = False
        self.pointer = (0, 0)
        self.keys_sent = []

    # compositor model
    def scene(self):
        previews = [{"id": w["id"], "rect": [100 + 200 * n, 300, 180, 120]}
                    for n, w in enumerate(self.windows)] if self.overview else []
        layers = [BAR] + ([{"namespace": wm.MENU_NAMESPACE, "rect": LAYER}] if self.menu else [])
        return {"windows": [dict(w) for w in self.windows], "focused": self.focused,
                "minimized": sorted(self.minimized), "above": sorted(self.above), "outputs": [PRIMARY],
                "layers": layers, "overview_open": self.overview, "overview_progress": 1 if self.overview else 0,
                "previews": previews, "x11_ready": self.ready, "x11_display": ":0", "locked": False}

    def window(self, ident):
        return next(w for w in self.windows if w["id"] == ident)

    def focus(self, ident):
        self.focused = ident
        if ident is not None:
            self.mru = [ident] + [i for i in self.mru if i != ident]

    def minimize(self, ident):
        self.minimized.add(ident)
        rest = [i for i in self.mru if i != ident and i not in self.minimized]
        self.focus(rest[0] if rest else None)

    def activate(self, ident):
        self.minimized.discard(ident)
        self.focus(ident)

    def area(self):
        return [0, 32, 1280, 768]

    def map(self, ident, **window):
        self.windows.append({"id": ident, "workspace": 0, "x11_window_id": None, **window})
        self.focus(ident)

    def act(self, ident, row):
        window = self.window(ident)
        if row == "Hide":
            self.minimize(ident)
        elif row == "Maximize":
            window["restore"], window["rect"] = window["rect"], self.area()
        elif row == "Restore":
            window["rect"] = window.pop("restore")
        elif row == "Always on Top":
            self.above ^= {ident}
        elif row == "Close":
            self.windows.remove(window)
            self.mru.remove(ident)
            self.focus(self.mru[0])
            self.gtk_alive = False

    def menu_labels(self):
        ident = self.menu[0]
        maximized = "restore" in self.window(ident)
        return maximized, ROWS

    def menu_rects(self):
        maximized, labels = self.menu_labels()
        observed = menu_nodes(self.menu[1], self.menu[2], maximized=maximized, labels=labels)
        return {n["name"]: n["extents"] for n in observed["nodes"] if n["role"] == "button"}

    def open_menu(self, ident, x, y):
        if self.old != "no window menu":
            self.menu = (ident, x, y)

    # QMP input
    def click(self, x, y, button):
        self.pointer = (x, y)
        if self.menu:
            for name, (rx, ry, rw, rh) in self.menu_rects().items():
                if rx <= x < rx + rw and ry <= y < ry + rh:
                    ident = self.menu[0]
                    self.menu = None
                    if not (name == "Always on Top" and "restore" in self.window(ident)):
                        self.act(ident, name)
                    return
            self.menu = None
            return
        if self.overview:
            for preview in self.scene()["previews"]:
                px, py, pw, ph = preview["rect"]
                if px <= x < px + pw and py <= y < py + ph:
                    self.overview = False
                    self.activate(preview["id"])
                    return
            return
        for window in reversed(self.windows):
            wx, wy, ww, wh = window["rect"]
            if window["id"] not in self.minimized and wx <= x < wx + ww and wy <= y < wy + wh:
                if button == "right" and y < wy + 46 and window["app_id"].startswith("org.tuna.VmGates"):
                    self.open_menu(window["id"], x, y)
                elif button == "left":
                    self.focus(window["id"])
                return

    def keys(self, *names):
        self.keys_sent.append(names)
        if names == ("meta_l",):
            self.overview = not self.overview
        elif names == ("meta_l", "h"):
            self.minimize(self.focused)
        elif names == ("alt", "tab"):
            candidates = [i for i in self.mru if i != self.focused]
            self.activate(candidates[0])
        elif names == ("alt", "spc"):
            self.open_menu(self.focused, *self.pointer)
        elif names == ("x",):
            target = self.x_focus()
            for fixture in self.x11.values():
                if fixture["alive"] and fixture["xid"] == target:
                    fixture["keys"] += 1

    # X server model
    def x_focus(self):
        if self.old == "pointer root":
            return 1
        for fixture in self.x11.values():
            if fixture["alive"] and fixture["ident"] == self.focused:
                return fixture["xid"]
        return 1

    def x11_read(self, role):
        fixture = self.x11[role]
        hidden = fixture["ident"] in self.minimized and self.old != "never iconic"
        return {"focus": self.x_focus(), "wm_state": "Iconic" if hidden else "Normal",
                "net_wm_state": ["_NET_WM_STATE_HIDDEN"] if hidden else [], "x_errors": 0}


class Agent:
    """The guest probe's actions on the simulated session."""
    def __init__(self, session):
        self.s, self.calls = session, []

    def run(self, action, *args, timeout=30):
        self.calls.append((action, args))
        s = self.s
        if action == "gates-start":
            s.gtk_alive = True
            s.map(10, app_id=f"org.tuna.VmGates.n{args[1]}.main", rect=[300, 200, 560, 360])
            return {"fixture": {"app_id": f"org.tuna.VmGates.n{args[1]}.main"}, "window": dict(s.window(10)),
                    "scene": s.scene()}
        if action == "gates-state":
            return {"fixtures": {"main": {"alive": s.gtk_alive}}, "scene": s.scene()}
        if action in ("gates-stop", "wm-stop"):
            return {"confirmed": True}
        if action == "wm-state":
            value = {"fixtures": {r: {k: f[k] for k in ("xid", "keys", "alive", "display")}
                                  for r, f in s.x11.items()},
                     "scene": s.scene(), "xwayland": list(s.xwayland), "xim": []}
            if len(args) == 2:
                if not s.x11.get(args[1], {}).get("alive"):
                    return {"gates_error": f"RuntimeError: the {args[1]} X11 fixture is not running"}
                value["x11"] = s.x11_read(args[1])
            return value
        if action == "wm-menu":
            if not s.menu:
                return {"nodes": []}
            maximized, labels = s.menu_labels()
            return menu_nodes(s.menu[1], s.menu[2], maximized=maximized, labels=labels)
        if action == "wm-x11-start":
            if not s.ready:
                return {"gates_error": "RuntimeError: the X server is gone"}
            if not s.xwayland:
                s.xwayland = [s.next_pid]
                s.next_pid += 1
            ident, xid = s.next_id, 0x200001 + s.next_id
            s.next_id += 1
            s.x11[args[0]] = {"xid": xid, "ident": ident, "keys": 0, "alive": True, "display": ":0"}
            s.map(ident, app_id="TunaVmWm", x11_window_id=xid, rect=[500, 300, 480, 300])
            return {"fixture": dict(s.x11[args[0]]), "window": dict(s.window(ident)), "scene": s.scene()}
        if action == "wm-x11-iconify":
            s.minimize(s.x11[args[0]]["ident"])
            return {"sent": True}
        if action == "wm-x11-kill-server":
            killed = s.xwayland[0]
            for fixture in s.x11.values():
                if fixture["alive"]:
                    fixture["alive"] = False
                    s.windows = [w for w in s.windows if w["id"] != fixture["ident"]]
                    s.mru.remove(fixture["ident"])
            s.focus(s.mru[0])
            if s.old == "no restart":
                s.xwayland, s.ready = [], False
            else:
                s.xwayland = [s.next_pid]
                s.next_pid += 1
            return {"killed": killed, "xim": []}
        raise AssertionError(f"unexpected guest action {action}")


class Qmp:
    def __init__(self, session):
        self.s = session

    def click(self, x, y, button="left"):
        self.s.click(x, y, button)

    def keys(self, *names):
        self.s.keys(*names)

    def cmd(self, command, **arguments):
        if command == "input-send-event":
            axes = {e["data"]["axis"]: e["data"]["value"] for e in arguments["events"] if e["type"] == "abs"}
            if axes:
                self.s.pointer = (round(axes["x"] * 1280 / 32767), round(axes["y"] * 800 / 32767))


class Clock:
    """Waits in simulated time: a failing gate times out at once."""
    def __init__(self):
        self.now = 1000.0

    def monotonic(self):
        return self.now

    def time(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


@contextlib.contextmanager
def simulated_time():
    clock = Clock()
    with mock.patch.object(base, "time", clock), mock.patch.object(wm, "time", clock):
        yield clock


class Run(unittest.TestCase):
    def run_gates(self, old=None):
        session = Session(old)
        records = {}
        with tempfile.TemporaryDirectory() as out, simulated_time():
            wm.run(Qmp(session), Agent(session), out, lambda name, ok, detail: records.__setitem__(name, (ok, detail)),
                   read_ppm=None)
            steps = json.loads(Path(out, "wm-gates", "steps.json").read_text())
        return records, session, steps

    def test_tuna_desktop_passes_every_gate_and_leaves_nothing_behind(self):
        records, session, steps = self.run_gates()
        self.assertEqual(set(records), set(wm.GATES))
        for name, (ok, detail) in records.items():
            self.assertTrue(ok, f"{name}: {detail}")
        self.assertEqual([w["id"] for w in session.windows if w["x11_window_id"] is None], [1, 2, 3])
        self.assertEqual(session.above, set())
        self.assertTrue(steps)
        self.assertIn(("alt", "spc"), session.keys_sent)

    def test_each_old_behaviour_fails_its_own_gate(self):
        expected = {
            # X input focus left at PointerRoot (before G-X11-INPUT-FOCUS).
            "pointer root": {"V-X11-INPUT-FOCUS"},
            # WM_STATE never set (before #1078, G-X11-ICONIC).
            "never iconic": {"V-X11-ICONIC"},
            # Xwayland's loss is final (before G-X11-RESTART).
            "no restart": {"V-X11-RESTART"},
            # No window menu: V-X11-ICONIC's Hide step uses it too.
            "no window menu": {"V-WINDOW-MENU", "V-X11-ICONIC"},
        }
        for old, failing in expected.items():
            with self.subTest(old):
                records, _, _ = self.run_gates(old)
                self.assertEqual(set(records), set(wm.GATES))
                self.assertEqual({name for name, (ok, _) in records.items() if not ok}, failing, records)

    def test_unconfirmed_cleanup_raises_after_recording(self):
        session = Session()
        agent = Agent(session)
        original = agent.run

        def run(action, *args, timeout=30):
            if action == "wm-stop":
                return {"gates_error": "RuntimeError: X11 fixture windows remain"}
            return original(action, *args, timeout=timeout)
        agent.run = run
        records = {}
        with tempfile.TemporaryDirectory() as out, simulated_time():
            with self.assertRaises(RuntimeError) as raised:
                wm.run(Qmp(session), agent, out, lambda n, ok, d: records.__setitem__(n, (ok, d)), read_ppm=None)
        self.assertIn("windows remain", str(raised.exception))
        self.assertEqual(set(records), set(wm.GATES))
        self.assertEqual(agent.calls[-1][0], "gates-stop")


class Wiring(unittest.TestCase):
    def test_the_lane_lifecycle_and_images_carry_the_gates(self):
        lane = (ROOT / "scripts" / "tuna-vm-lane").read_text()
        self.assertIn('on_tablet("wm-gates"', lane)
        self.assertIn("for gate in WM_GATES:", lane)
        self.assertIn("results.setdefault(CLEANUP_GATE,", lane)
        self.assertIn('x11_sockets = agent.run("wm-x11-sockets")', lane)
        self.assertIn("record_x11_cleanup(agent, x11_sockets, greeter_visible and product_logout, record, evidence)",
                      lane)
        lifecycle = (ROOT / "packaging" / "marlin" / "vm-lane" / "tuna-vm-lifecycle").read_text()
        self.assertIn('os.execv("/usr/libexec/tuna-vm-wm-gates"', lifecycle)
        for name in ("Containerfile", "Containerfile.published"):
            image = (ROOT / "packaging" / "marlin" / "vm-lane" / name).read_text()
            self.assertIn("COPY packaging/marlin/vm-lane/tuna-vm-wm-gates /usr/libexec/tuna-vm-wm-gates", image)
            self.assertIn("/usr/libexec/tuna-vm-wm-gates \\", image)
        self.assertTrue(os.access(GUEST, os.X_OK))


@unittest.skipUnless(shutil.which("Xvfb"), "no Xvfb to run the guest's Xlib client against")
class GuestXlib(unittest.TestCase):
    """The guest's ctypes Xlib client and reader against a real X server."""
    def setUp(self):
        self.runtime = tempfile.TemporaryDirectory()
        # Xvfb picks a free display and reports it once it accepts clients;
        # SIGTERM (not SIGKILL) lets it remove its socket and lock.
        ready, report = os.pipe()
        self.server = subprocess.Popen(["Xvfb", "-displayfd", str(report), "-screen", "0", "1280x800x24",
                                        "-nolisten", "tcp"], pass_fds=(report,),
                                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        os.close(report)
        self.addCleanup(self.server.wait)
        self.addCleanup(self.server.terminate)
        self.addCleanup(self.runtime.cleanup)
        with os.fdopen(ready) as stream:
            number = stream.readline().strip()
        self.assertTrue(number.isdigit(), "Xvfb did not start")
        self.display = f":{number}"

    def guest(self, *args):
        result = subprocess.run([sys.executable, str(GUEST), *args], capture_output=True, text=True, timeout=20,
                                env={**os.environ, "DISPLAY": self.display, "XDG_RUNTIME_DIR": self.runtime.name})
        return json.loads(result.stdout)

    def test_the_window_reports_focus_keys_and_window_manager_state(self):
        key = "0123456789abcdef"
        env = {**os.environ, "DISPLAY": self.display, "XDG_RUNTIME_DIR": self.runtime.name}
        # Its stderr carries Xlib's fatal-IO line when the server goes away.
        client = subprocess.Popen([sys.executable, str(GUEST), "--x11-window", "first", key, self.display], env=env,
                                  stderr=subprocess.DEVNULL)
        self.addCleanup(client.kill)
        path = Path(self.runtime.name, f"tuna-vm-wm-x11-first-{key}.json")
        deadline = time.monotonic() + 10
        while not (path.exists() and json.loads(path.read_text())["mapped"]):
            self.assertLess(time.monotonic(), deadline, "the Xlib window never mapped")
            time.sleep(0.05)
        state = json.loads(path.read_text())
        xid = state["xid"]
        self.assertEqual(state["wm_class"], "TunaVmWmFirst0123456789ab")
        # No window manager, the old behaviour as the X server shows it:
        # focus PointerRoot and no WM_STATE, so both checkers refuse it.
        read = self.guest("--x11-read", self.display, str(xid))
        self.assertEqual((read["focus"], read["wm_state"], read["net_wm_state"]), (1, None, []))
        self.assertTrue(wm.x_focus_problems(read, xid))
        self.assertTrue(wm.iconic_problems(read, False))
        if shutil.which("xdotool") and shutil.which("xprop"):
            subprocess.run(["xdotool", "windowfocus", "--sync", str(xid), "key", "x"], env=env, check=True)
            read = self.guest("--x11-read", self.display, str(xid))
            self.assertEqual(wm.x_focus_problems(read, xid), [])
            deadline = time.monotonic() + 5
            while json.loads(path.read_text())["keys"] < 1:
                self.assertLess(time.monotonic(), deadline, "the key never reached the window")
                time.sleep(0.05)
            for prop in (["-f", "WM_STATE", "32c", "-set", "WM_STATE", "3,0"],
                         ["-f", "_NET_WM_STATE", "32a", "-set", "_NET_WM_STATE", "_NET_WM_STATE_HIDDEN"]):
                subprocess.run(["xprop", "-id", str(xid), *prop], env=env, check=True)
            self.assertEqual(wm.iconic_problems(self.guest("--x11-read", self.display, str(xid)), True), [])
        self.assertEqual(self.guest("--x11-iconify", self.display, str(xid)), {"sent": True, "x_errors": 0})
        # A destroyed window is an X error, counted, not a probe exit.
        self.assertEqual(self.guest("--x11-read", self.display, "4194303")["x_errors"], 2)
        # The client exits with its server.
        self.server.terminate()
        client.wait(timeout=10)


if __name__ == "__main__":
    unittest.main()
