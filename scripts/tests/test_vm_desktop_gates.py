"""Harness logic of the installed desktop gates (V-CURSOR, V-APP-POPOVER,
V-WINDOW-BUTTONS, V-MINIMIZE-ANIMATION, V-TRAY, V-XWAYLAND, V-MULTI-OUTPUT).

Synthetic compositor snapshots and fake guest transports only: no guest, GTK
or framebuffer qualification; the marlin-vm lane proves the gates.
"""
import ast
import json
import sys
import tempfile
import time
import unittest
from pathlib import Path

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts" / "lib"))
import vm_app_integration as app  # noqa: E402
import vm_desktop_gates as gates  # noqa: E402

PRIMARY = {"name": "Virtual-1", "primary": True, "rect": [0, 0, 1280, 800]}
SECOND = {"name": "Virtual-2", "primary": False, "rect": [1280, 0, 1024, 768]}
BAR = {"namespace": "tuna-panel", "rect": [0, 0, 1280, 32]}


def scene(**extra):
    return {"outputs": [PRIMARY], "layers": [BAR], "windows": [], **extra}


class Geometry(unittest.TestCase):
    def test_widget_centre_is_window_origin_plus_gtk_bounds(self):
        self.assertEqual(gates.widget_point([100, 200, 560, 360], [500, 6, 34, 34]), (617, 223))
        for rect, bounds in (([100, 200, 560, 360], [600, 6, 34, 34]),   # outside the window
                             ([1200, 200, 560, 360], [500, 6, 34, 34]),  # outside the output
                             ([100, 200, 560, 360], None),
                             ([100, 200, 560, 360], [0, 0, 0, 20])):
            with self.subTest(rect=rect, bounds=bounds), self.assertRaises(gates.GateError):
                gates.widget_point(rect, bounds)

    def test_work_area_is_below_the_one_top_bar(self):
        self.assertEqual(gates.work_area(scene()), [0, 32, 1280, 768])
        for broken in (scene(layers=[]), scene(layers=[BAR, BAR]),
                       scene(layers=[{"namespace": "tuna-panel", "rect": [0, 0, 640, 32]}]),
                       scene(outputs=[{**PRIMARY, "primary": False}])):
            with self.subTest(broken=broken), self.assertRaises(gates.GateError):
                gates.work_area(broken)

    def test_fixture_window_needs_exactly_one_nonce_window(self):
        fixture = {"app_id": "org.tuna.VmGates.nabc.main"}
        one = scene(windows=[{"id": 4, "app_id": fixture["app_id"]}, {"id": 5, "app_id": "org.gnome.Nautilus"}])
        self.assertEqual(gates.fixture_window(one, fixture)["id"], 4)
        self.assertIsNone(gates.fixture_window(scene(), fixture))
        self.assertIsNone(gates.fixture_window(scene(windows=one["windows"] * 2), fixture))


class Cursor(unittest.TestCase):
    TEXT = {"kind": "named", "name": "text", "source": "client", "size": 48,
            "image": [48, 48], "logical": [48, 48], "image_theme": "Adwaita"}

    def test_client_shape(self):
        self.assertTrue(gates.cursor_shows(self.TEXT, "text"))
        self.assertFalse(gates.cursor_shows({**self.TEXT, "kind": "surface"}, "text"))
        self.assertFalse(gates.cursor_shows({**self.TEXT, "name": "default"}, "text"))
        self.assertFalse(gates.cursor_shows(None, "text"))

    def test_cursor_size_rescales_from_the_previous_size(self):
        xcursor24 = {**self.TEXT, "size": 24, "image": [24, 24], "logical": [24, 24]}
        self.assertTrue(gates.cursor_rescaled(xcursor24, self.TEXT, "text", 48))
        # Adwaita's scalable text cursor on the installed Marlin session
        # (#1153 marlin-vm): a 32 px canvas at nominal 24 draws 64 at size 48.
        svg24 = {**self.TEXT, "size": 24, "image": [32, 32], "logical": [32.0, 32.0]}
        svg48 = {**self.TEXT, "size": 48, "image": [64, 64], "logical": [64.0, 64.0]}
        self.assertTrue(gates.cursor_rescaled(svg24, svg48, "text", 48))
        cases = {"size not applied": {**svg48, "size": 24},
                 "not redrawn": {**svg48, "logical": [32.0, 32.0]},
                 "wrong scale": {**svg48, "logical": [48.0, 48.0]},
                 "other shape": {**svg48, "name": "default"},
                 "server-drawn": {**svg48, "source": "compositor"}}
        for name, after in cases.items():
            with self.subTest(name):
                self.assertFalse(gates.cursor_rescaled(svg24, after, "text", 48))
        self.assertFalse(gates.cursor_rescaled(None, svg48, "text", 48))

    def ripple(self, frames=((1, [400.0, 300.0]), (2, [400.0, 300.0])), count=4, active=False):
        samples = [{"enabled": True, "active": True, "count": 4, "frames": f, "at": at} for f, at in frames]
        return {"baseline_locate_count": 3, "samples": samples,
                "scene": {"cursor": {"locate_pointer": {"enabled": True, "count": count, "active": active}}}}

    def test_one_finished_ripple_at_the_pointer(self):
        self.assertEqual(gates.ripple_problems(self.ripple(), [400.4, 299.6]), [])
        cases = {"elsewhere": (self.ripple(frames=((1, [10.0, 10.0]), (2, [10.0, 10.0]))), [400, 300]),
                 "two ripples": (self.ripple(count=5), [400, 300]),
                 "unfinished": (self.ripple(active=True), [400, 300]),
                 "unobserved": (self.ripple(frames=()), [400, 300]),
                 "one frame": (self.ripple(frames=((1, [400.0, 300.0]),)), [400, 300]),
                 "no pointer": (self.ripple(), None)}
        for name, (watch, pointer) in cases.items():
            with self.subTest(name):
                self.assertTrue(gates.ripple_problems(watch, pointer))


class Minimize(unittest.TestCase):
    ICON = [600, 740, 48, 48]

    def watch(self, effect="minimize", samples=2, history=("started-live", "completed"), duration=400,
              in_flight=False, minimized=True, icon=None):
        entry = "minimizing" if effect == "minimize" else "restoring"
        frames = [{"window": 7, "icon": icon or self.ICON, "rect": [100 + i, 100, 300, 200],
                   "alpha": 0.9 - i * 0.3, "elapsed_ms": 40 * i, "duration_ms": duration} for i in range(samples)]
        rows = [{"window": 7, "effect": effect, "outcome": o, "at_ms": 2000 + i, "duration_ms": duration}
                for i, o in enumerate(history)]
        rows.insert(0, {"window": 7, "effect": effect, "outcome": "completed", "at_ms": 900, "duration_ms": 400})
        return {"baseline_at_ms": 1000, "samples": frames,
                "scene": {"minimized": [7] if minimized else [],
                          "window_lifecycle": {entry: frames[:1] if in_flight else [], "history": rows}}}

    def test_minimize_runs_frame_by_frame_and_finishes(self):
        self.assertEqual(gates.transition_problems(self.watch(), 7, "minimize"), [])
        cases = {"no frames": self.watch(samples=1), "never finished": self.watch(in_flight=True),
                 "not completed": self.watch(history=("started-live",)),
                 "not started": self.watch(history=("completed",)),
                 "reduced motion": self.watch(duration=0), "still shown": self.watch(minimized=False)}
        for name, watch in cases.items():
            with self.subTest(name):
                self.assertTrue(gates.transition_problems(watch, 7, "minimize"))
        # An earlier minimize of the same window (before the baseline) is not this one.
        self.assertTrue(gates.transition_problems(self.watch(history=()), 7, "minimize"))

    def test_the_session_slow_down_factor_scales_the_400_ms(self):
        # TunaOS images export GNOME_SHELL_SLOWDOWN_FACTOR=0.8; GNOME's
        # adjustAnimationTime then runs the minimize for 320 ms.
        fast = self.watch(duration=320)
        fast["scene"]["animation_slowdown"] = 0.8
        self.assertEqual(gates.transition_problems(fast, 7, "minimize"), [])
        wrong = self.watch(duration=400)
        wrong["scene"]["animation_slowdown"] = 0.8
        self.assertTrue(gates.transition_problems(wrong, 7, "minimize"))
        self.assertTrue(gates.transition_problems(self.watch(duration=320), 7, "minimize"))

    def test_restore_runs_and_ends_shown(self):
        good = self.watch("unminimize", history=("started",), minimized=False)
        self.assertEqual(gates.transition_problems(good, 7, "unminimize"), [])
        self.assertTrue(gates.transition_problems({**good, "samples": good["samples"][:1]}, 7, "unminimize"))
        self.assertTrue(gates.transition_problems(self.watch("unminimize", history=("started",)), 7, "unminimize"))

    CORNER = [0, 0, 0, 0]

    def test_one_drawn_target(self):
        self.assertEqual(gates.minimize_target(self.watch()["samples"]), self.ICON)
        mixed = self.watch()["samples"] + self.watch(icon=self.CORNER)["samples"]
        self.assertIsNone(gates.minimize_target(mixed))
        self.assertIsNone(gates.minimize_target([]))

    def test_gnome_51_leading_corner_of_the_windows_output(self):
        self.assertEqual(gates.leading_corner(PRIMARY), self.CORNER)
        self.assertEqual(gates.leading_corner(SECOND), [1280, 0, 0, 0])
        self.assertEqual(gates.window_output(scene(outputs=[PRIMARY, SECOND]), [1400, 100, 300, 200]), SECOND)
        with self.assertRaises(gates.GateError):
            gates.window_output(scene(outputs=[PRIMARY, SECOND]), [2400, 900, 300, 200])

    def test_the_published_dash_icon(self):
        published = scene(dash_icon_targets=[[3, 100, 740, 48, 48], [7] + self.ICON])
        self.assertEqual(gates.published_dash_icon(published, 7, PRIMARY), self.ICON)
        self.assertIsNone(gates.published_dash_icon(published, 9, PRIMARY))
        for rows in ([[7, 600, 40, 48, 48]], [[7, 600, 740, 0, 0]], [[7] + self.ICON, [7] + self.ICON], []):
            with self.subTest(rows):
                self.assertIsNone(gates.published_dash_icon(scene(dash_icon_targets=rows), 7, PRIMARY))

    def test_dash_hidden_goes_to_the_corner_not_the_published_icon(self):
        self.assertEqual(gates.target_problems(self.watch(icon=self.CORNER)["samples"], self.CORNER, "corner"), [])
        cases = {
            # Before #894 Tuna went to the hidden dash's icon; GNOME 51 never does on Wayland.
            "hidden dash icon": self.watch()["samples"],
            "the trailing corner": self.watch(icon=[1280, 0, 0, 0])["samples"],
            "nowhere": [],
            "two targets": self.watch(icon=self.CORNER)["samples"] + self.watch()["samples"],
        }
        for name, samples in cases.items():
            with self.subTest(name):
                self.assertTrue(gates.target_problems(samples, self.CORNER, "corner"))

    def instant(self, **scene_extra):
        watch = self.watch(samples=0, history=())
        watch["scene"].update({"overview_open": True, **scene_extra})
        return watch

    def test_overview_open_minimizes_at_once_without_a_frame(self):
        # windowManager.js 51.1 _shouldAnimate() is false while the overview is
        # open: _minimizeWindow calls completed_minimize, nothing is drawn.
        self.assertEqual(gates.instant_problems(self.instant(), 7), [])
        animated = self.watch()
        animated["scene"]["overview_open"] = True
        cases = {
            # What the gate asked for before: 400 ms toward the dash icon.
            "animated toward the dash icon": animated,
            "frames drawn": {**self.instant(), "samples": self.watch()["samples"]},
            "started": {**self.instant(), "scene": {**self.instant()["scene"], "window_lifecycle": {
                "history": [{"window": 7, "effect": "minimize", "outcome": "started-live", "at_ms": 2000}]}}},
            "still running": self.instant(window_lifecycle={"minimizing": [{"window": 7}], "history": []}),
            "overview closed": self.instant(overview_open=False),
            "still shown": self.instant(minimized=[]),
        }
        for name, watch in cases.items():
            with self.subTest(name):
                self.assertTrue(gates.instant_problems(watch, 7))
        # An earlier animated minimize of the window (before the baseline) is not this one.
        self.assertEqual(gates.instant_problems(self.instant(window_lifecycle={"history": [
            {"window": 7, "effect": "minimize", "outcome": "started-live", "at_ms": 900}]}), 7), [])


class FakeQmp:
    def __init__(self, on_keys):
        self.on_keys, self.pressed = on_keys, []

    def keys(self, *names):
        self.pressed.append(names)
        self.on_keys(names)


class Settle(unittest.TestCase):
    MAIN = {"app_id": "org.tuna.VmGates.nk.main"}
    BYSTANDER = {"app_id": "org.tuna.VmGates.nk.bystander"}

    def desktop(self, overview, minimized):
        windows = [{"id": 11, "app_id": self.MAIN["app_id"]}, {"id": 10, "app_id": self.BYSTANDER["app_id"]}]
        return {"fixtures": {"main": self.MAIN, "bystander": self.BYSTANDER},
                "scene": scene(windows=windows, overview_open=overview, minimized=minimized)}

    def gates_on(self, desktop, out):
        """Gates whose guest state follows the keys the fake QMP receives."""
        state = {"value": desktop}

        def on_keys(names):
            scene_ = state["value"]["scene"]
            if names == ("esc",):
                state["value"] = self.desktop(False, scene_["minimized"])
            elif names == ("alt", "tab"):
                state["value"] = self.desktop(scene_["overview_open"], scene_["minimized"][1:])
        agent = FakeAgent({"gates-state": lambda *_: state["value"]})
        g = gates.Gates(FakeQmp(on_keys), agent, out, lambda *a: None, read_ppm=None)
        return g, state

    def test_a_failed_gate_leaves_the_overview_closed_and_the_window_restored(self):
        with tempfile.TemporaryDirectory() as out:
            g, state = self.gates_on(self.desktop(True, [11]), out)
            records = {}
            g.record = lambda name, ok, detail: records.__setitem__(name, ok)

            def stops_mid_overview():
                raise gates.GateError("no minimize started")
            self.assertFalse(g.gate("V-MINIMIZE-ANIMATION", stops_mid_overview))
            self.assertEqual(records, {"V-MINIMIZE-ANIMATION": False})
            self.assertEqual(g.qmp.pressed, [("esc",), ("alt", "tab")])
            self.assertFalse(state["value"]["scene"]["overview_open"])
            self.assertEqual(state["value"]["scene"]["minimized"], [])

    def test_a_settled_desktop_gets_no_input(self):
        with tempfile.TemporaryDirectory() as out:
            g, _ = self.gates_on(self.desktop(False, []), out)
            self.assertTrue(g.gate("V-CURSOR", lambda: "ok"))
            self.assertEqual(g.qmp.pressed, [])

    def test_settle_never_raises_and_logs_what_it_could_not_do(self):
        with tempfile.TemporaryDirectory() as out:
            g = gates.Gates(FakeQmp(lambda names: None), FakeAgent({"gates-state": {"gates_error": "gone"}}),
                            out, lambda *a: None, read_ppm=None)
            g.settle()
            self.assertEqual(g.log[-1]["action"], "settle")
            self.assertIn("gone", g.log[-1]["error"])


class Pixels(unittest.TestCase):
    def test_changed_pixels_counts_only_the_region(self):
        width, height = 8, 4
        left = (width, height, bytes(width * height * 3))
        changed = bytearray(left[2])
        for x, y in ((1, 1), (2, 1), (7, 3)):
            changed[(y * width + x) * 3] = 200
        right = (width, height, bytes(changed))
        self.assertEqual(gates.changed_pixels(left, right, [0, 0, 4, 2]), 2)
        self.assertEqual(gates.changed_pixels(left, right, [0, 0, 8, 4]), 3)
        self.assertEqual(gates.clip([-10, 790, 100, 100]), [0, 790, 90, 10])
        with self.assertRaises(gates.GateError):
            gates.changed_pixels(left, (4, 4, bytes(48)), [0, 0, 1, 1])


class Tray(unittest.TestCase):
    def test_item_point_is_the_top_bar_origin_plus_window_extents(self):
        item = {"name": gates.TRAY_TITLE, "showing": True, "toplevel": "Top Bar", "extents": [1000, 2, 28, 28]}
        self.assertEqual(gates.tray_point({"nodes": [item]}, scene()), (1014, 16))
        other = {**item, "extents": [900, 2, 28, 28]}
        for nodes in ([], [{**item, "showing": False}], [item, other], [{**item, "extents": [1000, 2, 0, 0]}]):
            with self.subTest(nodes=nodes):
                self.assertIsNone(gates.tray_point({"nodes": nodes}, scene()))
        self.assertIsNone(gates.tray_point({"nodes": [item]}, scene(layers=[])))

    def test_gtk_menu_button_reports_one_item_twice_and_the_inner_one_is_clicked(self):
        # Measured on the installed Marlin session (#1153 marlin-vm): the
        # MenuButton's outer button and its inner toggle button share the name.
        outer = {"name": gates.TRAY_TITLE, "role": "button", "showing": True, "toplevel": "Top Bar",
                 "extents": [1124, 0, 48, 32]}
        inner = {**outer, "role": "toggle button", "extents": [1127, 3, 42, 26]}
        self.assertEqual(gates.tray_point({"nodes": [outer, inner]}, scene()), (1148, 16))


class MultiOutput(unittest.TestCase):
    def test_gnome_keeps_one_top_bar_on_the_primary_output(self):
        plugged = scene(outputs=[PRIMARY, SECOND])
        self.assertEqual(app.top_bar_problems(plugged, 2), [])
        self.assertEqual(app.top_bar_problems(scene(), 1), [])
        self.assertTrue(app.top_bar_problems(scene(), 2))
        self.assertTrue(app.top_bar_problems(scene(outputs=[PRIMARY, SECOND], layers=[]), 2))
        both = scene(outputs=[PRIMARY, SECOND], layers=[BAR, {"namespace": "tuna-panel", "rect": [1280, 0, 1024, 32]}])
        self.assertTrue(app.top_bar_problems(both, 2))
        moved = scene(outputs=[{**PRIMARY, "primary": False}, {**SECOND, "primary": True}])
        self.assertTrue(app.top_bar_problems(moved, 2))

    def test_placed_window_and_containment(self):
        fixture = {"app_id": "org.tuna.VmGates.nab.placed"}
        window = {"id": 9, "app_id": fixture["app_id"], "rect": SECOND["rect"]}
        self.assertEqual(app.placed_window(scene(windows=[window]), fixture), window)
        self.assertIsNone(app.placed_window(scene(), fixture))
        self.assertTrue(app.contains(PRIMARY["rect"], [0, 32, 1280, 768]))
        self.assertFalse(app.contains(PRIMARY["rect"], SECOND["rect"]))

    def test_every_app_integration_gate_is_defaulted_by_the_lane(self):
        self.assertEqual(set(app.GATES), {"V-XWAYLAND", "V-PICKER", "V-INTROSPECT-SCREEN",
                                          "V-MULTI-OUTPUT", "V-INTROSPECT-SANDBOX"})
        lane = (ROOT / "scripts" / "tuna-vm-lane").read_text()
        self.assertEqual(lane.count("for gate in app_integration.GATES:"), 2)
        self.assertIn("for gate in DESKTOP_GATES:", lane)


class FakeAgent:
    def __init__(self, replies):
        self.replies, self.calls = replies, []

    def run(self, action, *args, timeout=30):
        self.calls.append((action, args))
        reply = self.replies.get(action, {})
        return reply(*args) if callable(reply) else reply


class Run(unittest.TestCase):
    def run_gates(self, replies):
        records = {}
        agent = FakeAgent({"gates-settings": {"cursor-size": "24", "locate-pointer": "false"},
                           "gates-stop": {"confirmed": True}, **replies})
        with tempfile.TemporaryDirectory() as out:
            try:
                gates.run(None, agent, out, lambda name, ok, detail: records.__setitem__(name, (ok, detail)),
                          read_ppm=None)
                error = None
            except RuntimeError as raised:
                error = raised
            steps = json.loads(Path(out, "desktop-gates", "steps.json").read_text())
        return records, agent, error, steps

    def test_a_failed_fixture_start_fails_every_gate_and_still_cleans_up(self):
        records, agent, error, steps = self.run_gates({"gates-start": {"gates_error": "RuntimeError: no map"}})
        self.assertIsNone(error)
        self.assertEqual(set(records), set(gates.GATES))
        self.assertTrue(all(not ok and "no map" in detail for ok, detail in records.values()))
        actions = [action for action, _ in agent.calls]
        self.assertEqual(actions[-2:], ["gates-settings", "gates-stop"])
        self.assertEqual(agent.calls[-2][1], ("set", "24", "false"))
        self.assertTrue(steps)

    def test_unconfirmed_cleanup_raises_after_recording(self):
        records, _, error, _ = self.run_gates({"gates-start": {"gates_error": "RuntimeError: no map"},
                                               "gates-stop": {"gates_error": "RuntimeError: windows remain"}})
        self.assertIn("windows remain", str(error))
        self.assertEqual(set(records), set(gates.GATES))


def guest_scope(*names):
    """Functions of the guest probe, run against a scripted state file."""
    source = ROOT / "packaging" / "marlin" / "vm-lane" / "tuna-vm-desktop-gates"
    tree = ast.parse(source.read_text())
    body = [node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name in names]
    scope = {"json": json, "time": time, "SCENE_KEYS": ("cursor", "window_lifecycle", "minimized"),
             "fixtures": lambda key: {}}
    exec(compile(ast.Module(body=body, type_ignores=[]), str(source), "exec"), scope)
    return scope


class Scripted:
    """A state file whose successive reads are the given snapshots (the last repeats)."""
    def __init__(self, snapshots):
        self.snapshots = [json.dumps(s) for s in snapshots]

    def read_text(self):
        return self.snapshots.pop(0) if len(self.snapshots) > 1 else self.snapshots[0]


class GuestWatch(unittest.TestCase):
    def life(self, minimizing=(), restoring=(), history=()):
        return {"window_lifecycle": {"minimizing": list(minimizing), "restoring": list(restoring),
                                     "history": list(history)}}

    def watch(self, snapshots, effect, target=7):
        scope = guest_scope("in_flight", "watch")
        scope["SCENE"] = Scripted(snapshots)
        started = time.monotonic()
        result = scope["watch"]("k", effect, target, 3)
        return result, time.monotonic() - started

    def test_minimize_frames_are_collected_until_this_run_completes(self):
        old = {"window": 7, "effect": "minimize", "outcome": "completed", "at_ms": 500}
        frame = lambda alpha: {"window": 7, "alpha": alpha, "icon": [1, 2, 3, 4]}
        snapshots = [self.life(history=[old]), self.life(history=[old]),
                     self.life([frame(0.8)], history=[old]), self.life([frame(0.8)], history=[old]),
                     self.life([frame(0.3), {"window": 8, "alpha": 0.5}], history=[old]),
                     self.life(history=[old, {"window": 7, "effect": "minimize", "outcome": "completed",
                                              "at_ms": 900}])]
        result, elapsed = self.watch(snapshots, "minimize")
        self.assertLess(elapsed, 2)
        self.assertEqual([s["alpha"] for s in result["samples"]], [0.8, 0.3])
        self.assertEqual(result["baseline_at_ms"], 500)

    def test_an_earlier_completed_minimize_does_not_end_the_watch(self):
        old = {"window": 7, "effect": "minimize", "outcome": "completed", "at_ms": 500}
        result, elapsed = self.watch([self.life(history=[old])], "minimize")
        self.assertGreaterEqual(elapsed, 3)
        self.assertEqual(result["samples"], [])

    def test_restore_ends_when_its_frames_stop(self):
        run = lambda ms: {"window": 7, "elapsed_ms": ms, "duration_ms": 400}
        result, elapsed = self.watch([self.life(), self.life(restoring=[run(16)]),
                                      self.life(restoring=[run(33)]), self.life()], "unminimize")
        self.assertLess(elapsed, 2)
        self.assertEqual([s["elapsed_ms"] for s in result["samples"]], [16, 33])

    def test_locate_ends_after_one_new_ripple_finishes(self):
        locate = lambda count, active, frames: {"cursor": {"locate_pointer": {
            "count": count, "active": active, "frames": frames, "at": [5, 6] if active else None}}}
        result, elapsed = self.watch([locate(2, False, 10), locate(3, True, 11), locate(3, True, 12),
                                      locate(3, False, 13)], "locate", 0)
        self.assertLess(elapsed, 2)
        self.assertEqual(result["baseline_locate_count"], 2)
        self.assertEqual([s["frames"] for s in result["samples"]], [11, 12])
        with self.assertRaises(ValueError):
            self.watch([locate(2, False, 10)], "maximize")

    def test_a_requested_minimize_is_sent_by_the_armed_watch(self):
        # The guest agent runs one action at a time, so the watch itself sends
        # the main fixture's own minimize request (SIGUSR1) once armed.
        scope = guest_scope("in_flight", "watch")
        sent = []
        scope["os"] = type("Os", (), {"kill": staticmethod(lambda pid, sig: sent.append((pid, sig)))})
        scope["signal"] = type("Signal", (), {"SIGUSR1": 10})
        scope["fixtures"] = lambda key: {"main": {"pid": 4242, "alive": True}}
        done = {"window": 7, "effect": "minimize", "outcome": "completed", "at_ms": 900}
        scope["SCENE"] = Scripted([self.life(), self.life([{"window": 7, "alpha": 0.5, "icon": [1, 2, 3, 4]}]),
                                   self.life(history=[done])])
        result = scope["watch"]("k", "minimize", 7, 3, "main")
        self.assertEqual(sent, [(4242, 10)])
        self.assertEqual(len(result["samples"]), 1)
        for args in (("unminimize", 7, 3, "main"), ("minimize", 7, 3, "bystander")):
            with self.assertRaises(ValueError):
                scope["watch"]("k", *args)
        scope["fixtures"] = lambda key: {"main": {"pid": 4242, "alive": False}}
        with self.assertRaises(RuntimeError):
            scope["watch"]("k", "minimize", 7, 3, "main")
        self.assertEqual(len(sent), 1)


if __name__ == "__main__":
    unittest.main()
