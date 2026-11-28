"""Harness logic of the installed shell gates (scripts/lib/vm_shell_gates.py):
V-OVERVIEW-PICKER, V-APP-GRID, V-OVERVIEW-FOCUS, V-NOTIFY, V-NOTIFY-DND,
V-TILING-KEYS, V-WORKSPACE-KEYS, V-WINDOW-MOVE-RESIZE and V-A11Y-TREE.

The observations are checked on data shaped like the first real marlin-vm
run's (run 43788586136: GNOME_SHELL_SLOWDOWN_FACTOR 0.8, GTK's nested double
AT-SPI exposure, the lifecycle's own windows on workspace 0). The gates then
run end to end against a fake session (World) that follows the compositor's
rules (crates/compositor tests: super_arrows_drive_layouts_and_consume,
super_page_keys_switch_and_shift_moves_focused, dragging_to_an_edge_shows_
gnomes_tile_preview, thumbnails_strip_matches_gnome_51) and the shell's (the
nested proofs' banner queue, Do Not Disturb, date-menu groups and app grid),
once whole and once per broken behaviour each gate must catch. No guest, GTK
or framebuffer qualification: the marlin-vm lane proves the gates.
"""
import ast
import importlib.machinery
import importlib.util
import json
import sys
import tempfile
import threading
import unittest
from pathlib import Path
from unittest import mock

sys.dont_write_bytecode = True
ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts" / "lib"))
import vm_shell_gates as shell  # noqa: E402

PRIMARY = {"name": "Virtual-1", "primary": True, "rect": [0, 0, 1280, 800]}
BAR = {"namespace": "tuna-panel", "rect": [0, 0, 1280, 32]}
WORK = [0, 32, 1280, 768]
# The lifecycle's own windows as the first real run's snapshot listed them.
LIFECYCLE = [(1, "org.gnome.DiskUtility", [190, 116, 900, 600]), (2, "org.gnome.SystemMonitor", [240, 56, 800, 720]),
             (3, "org.gnome.Nautilus", [195, 141, 890, 550]), (4, "org.tuna.VmInput", [300, 200, 400, 300])]


def golden(stage):
    return (shell.GOLDEN / shell.STAGES[stage][0]).read_text()


def nodes_from(text, extra=()):
    """AT-SPI nodes as the guest dump reports them, from golden lines."""
    nodes, frame = [], None
    for line in text.splitlines():
        role, name = line.split(": ", 1)
        if role == "frame":
            frame = name
        nodes.append({"role": "button" if role == "push button" else role, "name": name, "toplevel": frame,
                      "checked": False, "focused": False, "description": "", "extents": [0, 0, 20, 20]})
    return nodes + list(extra)


# ---- observations ----------------------------------------------------------

class A11y(unittest.TestCase):
    def test_the_goldens_themselves_pass(self):
        for stage in shell.STAGES:
            with self.subTest(stage):
                self.assertEqual(shell.a11y_problems(stage, nodes_from(golden(stage))), [])

    def test_normalisation_is_the_golden_tools(self):
        loader = importlib.machinery.SourceFileLoader("golden_tool", str(ROOT / "scripts" / "tuna-a11y-golden"))
        spec = importlib.util.spec_from_loader(loader.name, loader)
        tool = importlib.util.module_from_spec(spec)
        loader.exec_module(tool)
        for name in ("12", "Thursday October 1 2026", "Date and Time", "3 notifications, collapsed"):
            self.assertEqual(shell.stable(name), tool.stable(name), name)
        for role in ("button", "push button", "toggle button", "menu"):
            self.assertEqual(shell.canonical_role(role), tool.role(role), role)

    def test_gtk_double_exposure_counts_once(self):
        # Real data: GTK's MenuButton showed the tray item as `button` then `toggle button`;
        # the same control exposed twice in a row with one role reads once.
        nodes = nodes_from(golden("panel"))
        doubled = []
        for node in nodes:
            doubled.append(node)
            if node["name"] == "Activities":
                doubled.append(dict(node))
        self.assertEqual(shell.a11y_problems("panel", doubled), [])

    def test_hardware_tiles_are_whole_or_absent(self):
        nodes = nodes_from(golden("quick-settings"))
        no_bluetooth = [n for n in nodes if not n["name"].startswith("Bluetooth")]
        self.assertEqual(shell.a11y_problems("quick-settings", no_bluetooth), [])
        no_audio = [n for n in nodes if n["name"] not in ("Mute", "Volume", "Open sound output menu")]
        self.assertEqual(shell.a11y_problems("quick-settings", no_audio), [])
        # One output device: GNOME 51 hides the output menu button, the slider stays (volume.js:173).
        one_output = [n for n in nodes if n["name"] != "Open sound output menu"]
        self.assertEqual(shell.a11y_problems("quick-settings", one_output), [])
        no_slider = [n for n in nodes if n["name"] != "Volume"]
        self.assertTrue(shell.a11y_problems("quick-settings", no_slider))
        half = [n for n in nodes if n["name"] != "Bluetooth Menu"]
        self.assertTrue(shell.a11y_problems("quick-settings", half))

    def test_dropped_renamed_retyped_or_reordered_controls_fail(self):
        nodes = nodes_from(golden("quick-settings"))
        dropped = [n for n in nodes if n["name"] != "Do Not Disturb"]
        renamed = [dict(n, name="Quiet") if n["name"] == "Dark Style" else n for n in nodes]
        retyped = [dict(n, role="check box") if n["name"] == "Night Light" else n for n in nodes]
        order = [n["name"] for n in nodes]
        swapped = list(nodes)
        i, j = order.index("Take Screenshot"), order.index("Lock Screen")
        swapped[i], swapped[j] = swapped[j], swapped[i]
        for label, broken in (("dropped", dropped), ("renamed", renamed), ("retyped", retyped), ("reordered", swapped)):
            with self.subTest(label):
                self.assertTrue(shell.a11y_problems("quick-settings", broken))
        no_frame = [n for n in nodes_from(golden("overview")) if n["toplevel"] != "Dash"]
        self.assertTrue(shell.a11y_problems("overview", no_frame))

    def test_dash_apps_and_preview_close_depend_on_the_machine(self):
        nodes = [n for n in nodes_from(golden("overview")) if not n["name"].startswith("tuna-test-")
                 and n["name"] != "Close"]
        extra = [{"role": "button", "name": "Files", "toplevel": "Dash"}]
        self.assertEqual(shell.a11y_problems("overview", nodes + extra), [])
        without_show_apps = [n for n in nodes if n["name"] != "Show Apps"]
        self.assertTrue(shell.a11y_problems("overview", without_show_apps))


class Points(unittest.TestCase):
    def test_a_control_is_its_layer_origin_plus_window_extents(self):
        # The real tray item: outer `button` and inner `toggle button`, nested.
        nodes = [{"name": "Tray", "role": "button", "toplevel": "Top Bar", "extents": [1124, 0, 48, 32]},
                 {"name": "Tray", "role": "toggle button", "toplevel": "Top Bar", "extents": [1127, 3, 42, 26]}]
        scene = {"layers": [BAR]}
        self.assertEqual(shell.node_point(nodes, "Tray", scene, "Top Bar"), (1148, 16))
        grid = {"layers": [BAR, {"namespace": "tuna-shell-appgrid", "rect": [0, 120, 1280, 600]}]}
        tile = [{"name": "App", "role": "button", "toplevel": "App Grid", "extents": [100, 50, 96, 96]}]
        self.assertEqual(shell.node_point(tile, "App", grid, "App Grid"), (148, 218))

    def test_ambiguous_missing_or_unplaceable_controls_are_refused(self):
        scene = {"layers": [BAR]}
        apart = [{"name": "X", "role": "button", "toplevel": "Top Bar", "extents": [0, 0, 20, 20]},
                 {"name": "X", "role": "button", "toplevel": "Top Bar", "extents": [100, 0, 20, 20]}]
        for nodes, frame, sc in ((apart, "Top Bar", scene), ([], "Top Bar", scene),
                                 (apart[:1], "Unknown", scene), (apart[:1], "Top Bar", {"layers": [BAR, BAR]})):
            with self.subTest(frame=frame), self.assertRaises(shell.GateError):
                shell.node_point(nodes, "X", sc, frame)


def thumbs(n=3, active=0):
    left = round((1280 - (n * 43 + (n - 1) * 6)) / 2)
    return [{"workspace": i, "active": i == active, "alpha": 1.0, "rect": [left + 49 * i, 102, 43, 26]} for i in range(n)]


class Overview(unittest.TestCase):
    def scene(self, **extra):
        windows = [{"id": i, "app_id": a, "rect": r, "workspace": 0} for i, a, r in LIFECYCLE]
        windows.append({"id": 11, "app_id": "fixture", "rect": [360, 150, 560, 360], "workspace": 1})
        previews = [{"id": i, "rect": [100 + 200 * k, 300, 180, 120]} for k, (i, _, _) in enumerate(LIFECYCLE)]
        return {"outputs": [PRIMARY], "layers": [BAR], "windows": windows, "minimized": [], "previews": previews,
                "thumbnails": thumbs(), "active_workspace": 0,
                "workspace_cards": [{"workspace": 0, "active": True, "rect": [215, 152, 849, 509]}], **extra}

    def test_gnome_51_strip_at_1280x800(self):
        self.assertEqual([t["rect"][0] for t in thumbs()], [570, 619, 668])
        self.assertEqual(shell.strip_problems(self.scene(), {0, 1}), [])
        self.assertEqual(shell.picker_problems(self.scene(), 0), [])

    def test_strip_breakage(self):
        cases = {
            "two": self.scene(thumbnails=thumbs(2)),
            "size": self.scene(thumbnails=[dict(t, rect=t["rect"][:2] + [48, 30]) for t in thumbs()]),
            "row": self.scene(thumbnails=[dict(t, rect=[t["rect"][0], 140, 43, 26]) for t in thumbs()]),
            "active": self.scene(thumbnails=thumbs(active=1)),
            "no trailing empty": self.scene(thumbnails=thumbs()),
            "card": self.scene(workspace_cards=[{"workspace": 0, "active": True, "rect": [0, 32, 1280, 768]}]),
        }
        for label, scene in cases.items():
            occupied = {0, 1, 2} if label == "no trailing empty" else {0, 1}
            with self.subTest(label):
                self.assertTrue(shell.strip_problems(scene, occupied))

    def test_picker_breakage(self):
        missing = self.scene(previews=self.scene()["previews"][1:])
        unscaled = self.scene(previews=[{"id": 1, "rect": [190, 116, 900, 600]}] + self.scene()["previews"][1:])
        outside = self.scene(previews=[{"id": 1, "rect": [1200, 300, 180, 120]}] + self.scene()["previews"][1:])
        for label, scene in (("missing", missing), ("unscaled", unscaled), ("outside", outside)):
            with self.subTest(label):
                self.assertTrue(shell.picker_problems(scene, 0))

    def test_the_ring_is_vivid(self):
        self.assertTrue(shell.vivid((53, 132, 228)))   # GNOME blue accent
        self.assertTrue(shell.vivid((233, 84, 32)))    # Ubuntu orange accent
        self.assertFalse(shell.vivid((60, 60, 60)))


class Workspaces(unittest.TestCase):
    RECORD = {"from": 0, "to": 1, "motion": "slide", "gesture": False, "duration_ms": 200.0, "elapsed_ms": 201.0,
              "frames": 13, "retargets": 0, "reversed": False, "velocity": None}

    def test_the_slide_is_250_ms_times_the_slowdown(self):
        scene = {"animation_slowdown": 0.8}
        self.assertEqual(shell.slide_problems(self.RECORD, 0, 1, scene), [])
        self.assertTrue(shell.slide_problems(dict(self.RECORD, duration_ms=250.0), 0, 1, scene))
        self.assertEqual(shell.slide_problems(dict(self.RECORD, duration_ms=250.0), 0, 1, {}), [])
        for broken in (dict(self.RECORD, motion="off", frames=0), dict(self.RECORD, frames=1),
                       dict(self.RECORD, to=2), None):
            with self.subTest(broken=broken):
                self.assertTrue(shell.slide_problems(broken, 0, 1, scene))

    def test_a_new_record_is_one_finished_since(self):
        old = [self.RECORD]
        self.assertIsNone(shell.slide_record(old, {"workspace_slide": {"history": old}}))
        new = old + [dict(self.RECORD, **{"from": 1, "to": 0})]
        self.assertEqual(shell.slide_record(old, {"workspace_slide": {"history": new}})["to"], 0)

    def test_popup(self):
        self.assertEqual(shell.popup_problems({"samples": [{"rect": [540, 340, 200, 120]}], "gone": True}), [])
        self.assertTrue(shell.popup_problems({"samples": [], "gone": False}))
        self.assertTrue(shell.popup_problems({"samples": [{"rect": [540, 340, 200, 120]}], "gone": False}))


class Windows(unittest.TestCase):
    def test_half_tiles(self):
        self.assertEqual(shell.half_rects(WORK), ([0, 32, 640, 768], [640, 32, 640, 768]))
        self.assertEqual(shell.half_rects([0, 32, 1281, 768]), ([0, 32, 640, 768], [641, 32, 640, 768]))

    def test_moves_and_resizes(self):
        self.assertEqual(shell.move_problems([360, 150, 560, 360], [460, 210, 560, 360], (100, 60), [560, 360]), [])
        self.assertEqual(shell.move_problems([360, 150, 560, 360], [310, 120, 560, 360], (-100, -60), [560, 360]), [])
        for after in ([400, 150, 560, 360], [460, 210, 600, 360], [570, 210, 560, 360]):
            with self.subTest(after=after):
                self.assertTrue(shell.move_problems([360, 150, 560, 360], after, (100, 60), [560, 360]))
        self.assertEqual(shell.resize_problems([360, 150, 560, 360], [310, 150, 610, 360], (50, 0), (-50, 0)), [])
        self.assertTrue(shell.resize_problems([360, 150, 560, 360], [360, 150, 610, 360], (50, 0), (-50, 0)))
        self.assertTrue(shell.resize_problems([360, 150, 560, 360], [360, 150, 580, 360], (60, 0)))

    def test_the_invisible_border(self):
        self.assertEqual(shell.shadow_problems([586, 386], [360, 150, 560, 360]), [])
        self.assertTrue(shell.shadow_problems([560, 360], [360, 150, 560, 360]))


class Notifications(unittest.TestCase):
    def test_banner_is_top_centre(self):
        self.assertEqual(shell.banner_problems([400, 32, 480, 100], PRIMARY), [])
        for rect in ([0, 32, 480, 100], [400, 300, 480, 100], [1000, 32, 480, 100]):
            with self.subTest(rect=rect):
                self.assertTrue(shell.banner_problems(rect, PRIMARY))

    def test_closing_reasons_come_from_the_bus(self):
        events = [{"signal": "ready"}, {"signal": "ActionInvoked", "args": [7, "open"]},
                  {"signal": "NotificationClosed", "args": [7, 2]}, {"signal": "NotificationClosed", "args": [8, 1]}]
        self.assertEqual(shell.action_invoked(events, 7), "open")
        self.assertEqual((shell.closed_reason(events, 7), shell.closed_reason(events, 8)), (2, 1))
        self.assertIsNone(shell.closed_reason(events, 9))

    def test_one_banner_at_a_time(self):
        self.assertEqual(shell.queue_problems([{"a"}, {"a"}], "a", "b"), [])
        self.assertTrue(shell.queue_problems([{"a"}, {"a", "b"}], "a", "b"))
        self.assertTrue(shell.queue_problems([set(), {"b"}], "a", "b"))

    def test_collapsed_group(self):
        nodes = [{"name": "Tuna VM Gates", "description": "2 notifications, collapsed"}]
        self.assertIsNotNone(shell.group_node(nodes, "Tuna VM Gates"))
        self.assertIsNone(shell.group_node([{"name": "Tuna VM Gates", "description": "expanded"}], "Tuna VM Gates"))


# ---- a fake session the gates drive ----------------------------------------

class Clock:
    def __init__(self):
        self.now = 1000.0

    def sleep(self, seconds):
        self.now += seconds

    def monotonic(self):
        self.now += 0.01
        return self.now


def inside(point, rect):
    return rect[0] <= point[0] < rect[0] + rect[2] and rect[1] <= point[1] < rect[1] + rect[3]


def golden_lines(stage, frame):
    return dict(shell.golden_groups(golden(stage)))[frame]


ABSENT = ("Wired", "Bluetooth", "Printers", "Mobile Network", "Power Mode")


class World:
    """The compositor, the shell and the guest probe, by their documented rules."""
    APP = "Tuna VM Gates"
    BANNER = [400, 32, 480, 100]
    DASH = [520, 730, 240, 64]
    GRID = [0, 140, 1280, 560]

    def __init__(self, quirks=()):
        self.quirks = set(quirks)
        self.windows = {i: {"id": i, "app_id": a, "rect": list(r), "workspace": 0, "layout": "floating",
                            "stash": None} for i, a, r in LIFECYCLE}
        self.stack, self.mru, self.focused, self.active = [1, 2, 3, 4], [4, 3, 2, 1], 4, 0
        self.minimized, self.fixtures, self.next_id = set(), {}, 10
        self.overview = self.grid_open = False
        self.prev_focus = None
        self.pointer, self.down, self.grab, self.super_held, self.preview = [640.0, 400.0], False, None, False, None
        self.slides, self.layer_log = [], {}
        self.layer_seen = threading.Event()
        self.qs = self.date = self.expanded = False
        self.ui_focus, self.banner_focus = 0, False
        self.notes, self.events, self.dnd, self.next_note = [], [], False, 1
        self.apps, self.page, self.launch_name = [f"Base App {i:02d}" for i in range(10)], 0, None

    # -- compositor ---------------------------------------------------------
    def occupied(self):
        return {w["workspace"] for w in self.windows.values()}

    def workspaces(self):
        return list(range(max(self.occupied() | {self.active}) + 2 - (1 if self.active > max(self.occupied()) else 0)))

    def shown(self, ws=None):
        ws = self.active if ws is None else ws
        return [i for i in self.stack if self.windows[i]["workspace"] == ws and i not in self.minimized]

    def focus(self, ident):
        self.focused = ident
        if ident is not None:
            self.stack = [i for i in self.stack if i != ident] + [ident]
            self.mru = [ident] + [i for i in self.mru if i != ident]

    def role_of(self, ident):
        return next((role for role, f in self.fixtures.items() if f["id"] == ident), None)

    def set_rect(self, ident, rect):
        self.windows[ident]["rect"] = list(rect)
        role = self.role_of(ident)
        if role:
            fixture = self.fixtures[role]
            fixture["size"] = list(rect[2:])
            fixture["surface"] = [rect[2] + 26, rect[3] + 26]
            fixture["maximized"] = self.windows[ident]["layout"] == "maximized"
            fixture["tiled"] = self.windows[ident]["layout"] in ("left", "right")

    def layout(self, ident, kind):
        window = self.windows[ident]
        if window["layout"] == "floating" and kind != "floating":
            window["stash"] = list(window["rect"])
        left, right = shell.half_rects(WORK)
        target = {"maximized": WORK, "left": left, "right": right}.get(kind) or window["stash"] or window["rect"]
        window["layout"] = kind
        self.set_rect(ident, target)

    def thumbnails(self):
        spaces = self.workspaces()
        if not self.overview or len(spaces) < 3 or "no-strip" in self.quirks:
            return []
        n = len(spaces)
        left = round((1280 - (n * 43 + (n - 1) * 6)) / 2)
        return [{"workspace": ws, "active": ws == self.active, "alpha": 1.0, "rect": [left + 49 * k, 102, 43, 26]}
                for k, ws in enumerate(spaces)]

    def previews(self):
        if not self.overview or self.grid_open:
            return []
        return [{"id": i, "app_id": self.windows[i]["app_id"], "rect": [80 + 190 * k, 300, 170, 110]}
                for k, i in enumerate(sorted(self.shown()))]

    def layers(self):
        rows = [dict(BAR)]
        if self.overview:
            rows += [{"namespace": "tuna-shell-overview", "rect": [440, 40, 400, 40]},
                     {"namespace": "tuna-shell-dash", "rect": self.DASH}]
        if self.grid_open:
            rows.append({"namespace": "tuna-shell-appgrid", "rect": self.GRID})
        if self.banners():
            rows.append({"namespace": "tuna-shell-banners", "rect": self.BANNER})
        return rows

    def scene(self):
        windows = [{"id": i, "app_id": w["app_id"], "rect": list(w["rect"]), "workspace": w["workspace"]}
                   for i, w in sorted(self.windows.items())]
        return {"windows": windows, "focused": self.focused, "minimized": sorted(self.minimized),
                "stacking": list(self.stack), "outputs": [PRIMARY], "layers": self.layers(),
                "pointer_position": list(self.pointer), "overview_open": self.overview,
                "overview_progress": 1.0 if self.overview else 0.0, "overview_app_grid": self.grid_open,
                "overview_state": 2 if self.grid_open else (1 if self.overview else 0),
                "previews": self.previews(), "thumbnails": self.thumbnails(),
                "workspace_cards": [{"workspace": self.active, "active": True, "rect": [215, 152, 849, 509]}]
                if self.overview else [],
                "workspaces": self.workspaces(), "active_workspace": self.active, "session_mode": "gnome",
                "tile_preview": {"window": self.grab["id"], "rect": self.preview} if self.preview else None,
                "workspace_slide": {"in_flight": None, "history": list(self.slides[-4:])},
                "animation_slowdown": 0.8, "animations_enabled": True, "locked": False}

    def switch(self, target, carry=None):
        source = self.active
        if carry is not None:
            self.windows[carry]["workspace"] = target
        self.active = target
        self.slides.append({"from": source, "to": target, "motion": "slide", "gesture": False,
                            "duration_ms": 250.0 if "slide-unscaled" in self.quirks else 200.0,
                            "elapsed_ms": 201.0, "frames": 13, "retargets": 0, "reversed": False,
                            "velocity": None, "n": len(self.slides)})
        if "no-popup" not in self.quirks:
            self.layer_log["tuna-workspace-popup"] = {"samples": [{"rect": [540, 340, 200, 120], "t": 1.0}],
                                                       "gone": True}
            self.layer_seen.set()
        shown = self.shown()
        keep = self.focused if "focus-stays-on-empty" in self.quirks else None
        self.focused = carry if carry is not None else next((i for i in self.mru if i in shown), keep)
        if self.focused is not None:
            self.focus(self.focused)

    def key_to_client(self, name):
        role = self.role_of(self.focused)
        if role:
            self.fixtures[role]["keys"] += 1
            if name in ("up", "down", "left", "right", "pgup", "pgdn"):
                self.fixtures[role]["nav_keys"] += 1

    def chord(self, names):
        names = tuple(names)
        if names == ("meta_l",):
            if self.overview:
                self.close_overview(restore=True)
            else:
                self.overview, self.prev_focus = True, self.focused
            return
        if self.qs or self.date or self.banner_focus:
            return self.ui_key(names)
        if self.overview and names == ("esc",):
            return self.close_overview(restore="escape-drops-focus" not in self.quirks)
        if names[0] == "meta_l" and names[-1] in ("up", "down", "left", "right") and self.focused is not None:
            if "arrows-leak" in self.quirks:
                self.key_to_client(names[-1])
            kind = self.windows[self.focused]["layout"]
            key = names[-1]
            new = {"up": "maximized", "down": "floating"}.get(key)
            if key in ("left", "right"):
                new = "floating" if kind == key else key
            return self.layout(self.focused, new)
        if names[0] == "meta_l" and names[-1] in ("pgdn", "pgup"):
            step = 1 if names[-1] == "pgdn" else -1
            target = self.active + step
            if target < 0:
                return
            return self.switch(target, carry=self.focused if "shift" in names else None)
        if names == ("meta_l", "s"):
            self.qs, self.ui_focus = True, 0
            return
        if names == ("meta_l", "v"):
            self.date, self.ui_focus, self.expanded = True, 0, False
            return
        if names == ("meta_l", "n"):
            self.banner_focus = bool(self.banners())
            return
        if names == ("alt", "tab"):
            others = [i for i in self.mru[1:] if i in self.shown()]
            if others:
                self.focus(others[0])
            return
        if len(names) == 1:
            self.key_to_client(names[0])

    def close_overview(self, restore):
        self.overview = self.grid_open = False
        shown = self.shown()
        if restore and self.prev_focus in shown:
            self.focus(self.prev_focus)
        elif restore:
            self.focus(next((i for i in self.mru if i in shown), None))
        else:  # the bug V-OVERVIEW-FOCUS catches: focus lands on another window
            self.focus(next((i for i in shown if i != self.prev_focus), None))

    # -- pointer ------------------------------------------------------------
    def window_at(self, point):
        for ident in reversed(self.shown()):
            rect = self.windows[ident]["rect"]
            border = 10 if self.role_of(ident) == "main" and "no-border" not in self.quirks else 0
            grown = [rect[0] - border, rect[1] - border, rect[2] + 2 * border, rect[3] + 2 * border]
            if inside(point, grown):
                return ident
        return None

    def press(self):
        self.down = True
        p = tuple(self.pointer)
        for rect, action in self.targets():
            if inside(p, rect):
                return action()
        if self.overview:
            return
        ident = self.window_at(p)
        if ident is None:
            return
        self.focus(ident)
        rect = list(self.windows[ident]["rect"])
        if self.super_held:
            self.grab = {"kind": "move", "id": ident, "start": p, "rect": rect, "snap": False}
        elif not inside(p, rect):
            edges = {"left": p[0] < rect[0], "right": p[0] >= rect[0] + rect[2],
                     "top": p[1] < rect[1], "bottom": p[1] >= rect[1] + rect[3]}
            self.grab = {"kind": "resize", "id": ident, "start": p, "rect": rect, "edges": edges}
        elif self.role_of(ident) == "main" and p[1] < rect[1] + 46 and p[0] < rect[0] + rect[2] - 200:
            self.grab = {"kind": "move", "id": ident, "start": p, "rect": rect, "snap": True}
        else:
            self.fixtures[self.role_of(ident)]["presses"] += 1 if self.role_of(ident) else 0

    def motion(self):
        if self.grab is None or self.grab["kind"] == "preview":
            return
        g, p = self.grab, self.pointer
        dx, dy = round(p[0] - g["start"][0]), round(p[1] - g["start"][1])
        x, y, w, h = g["rect"]
        if g["kind"] == "move":
            if self.windows[g["id"]]["layout"] != "floating":
                self.windows[g["id"]]["layout"] = "floating"
            self.set_rect(g["id"], [x + dx, y + dy, w, h])
            left, _ = shell.half_rects(WORK)
            self.preview = None
            if g["snap"] and "no-edge-preview" not in self.quirks:
                if p[0] <= 2:
                    self.preview = left
                elif p[1] <= 34:
                    self.preview = list(WORK)
        elif g["kind"] == "resize":
            e = g["edges"]
            nx, nw = (x + dx, w - dx) if e["left"] else (x, w + dx if e["right"] else w)
            ny, nh = (y + dy, h - dy) if e["top"] else (y, h + dy if e["bottom"] else h)
            self.set_rect(g["id"], [nx, ny, nw, nh])
        elif g["kind"] == "preview":
            pass

    def release(self):
        self.down = False
        g, self.grab = self.grab, None
        if g is None:
            return
        if g["kind"] == "move" and self.preview is not None:
            ident = g["id"]
            self.windows[ident]["stash"] = g["rect"]
            self.windows[ident]["layout"] = "floating"
            kind = "maximized" if self.preview == list(WORK) else "left"
            self.windows[ident]["stash"] = list(g["rect"])
            self.windows[ident]["layout"] = kind
            self.set_rect(ident, self.preview)
        if g["kind"] == "preview":
            for t in self.thumbnails():
                if inside(self.pointer, t["rect"]):
                    self.windows[g["id"]]["workspace"] = t["workspace"]
        self.preview = None

    def targets(self):
        """Pointer targets the shell and the overview draw, screen rectangles first."""
        rows = []
        for note in self.banners():
            if note.get("action"):
                rows.append(([self.BANNER[0] + 20, self.BANNER[1] + 60, 120, 30], lambda n=note: self.invoke(n)))
        if self.overview:
            rows.append(([self.DASH[0] + 190, self.DASH[1] + 10, 40, 40], self.show_apps))
            if self.grid_open:
                for name, rect in self.grid_buttons().items():
                    rows.append(([self.GRID[0] + rect[0], self.GRID[1] + rect[1], rect[2], rect[3]],
                                 lambda name=name: self.grid_press(name)))
            for t in self.thumbnails():
                rows.append((t["rect"], lambda t=t: setattr(self, "active", t["workspace"])))
            for p in self.previews():
                rows.append((p["rect"], lambda p=p: setattr(self, "grab", {"kind": "preview", "id": p["id"]})))
        return rows

    # -- the shell's overview, grid and notifications ------------------------
    def show_apps(self):
        self.grid_open, self.page = True, 0

    def grid_apps(self):
        return sorted(self.apps)

    def pages(self):
        apps = self.grid_apps()
        return [apps[i:i + 24] for i in range(0, len(apps), 24)] or [[]]

    def grid_buttons(self):
        rows = {}
        pages = self.pages()
        if self.page + 1 < len(pages):
            rows["Next Page"] = [1220, 250, 40, 60]
        if self.page > 0:
            rows["Previous Page"] = [20, 250, 40, 60]
        for k, name in enumerate(pages[self.page]):
            rows[name] = [100 + (k % 8) * 130, 20 + (k // 8) * 130, 96, 96]
        return rows

    def grid_press(self, name):
        if name == "Next Page":
            self.page += 1
        elif name == "Previous Page":
            self.page -= 1
        elif name == self.launch_name and "grid-no-launch" not in self.quirks:
            self.overview = self.grid_open = False
            self.start("launched")

    def banners(self):
        shown = [n for n in self.notes if n["state"] == "banner"]
        return shown

    def send(self, title, urgency, action=None):
        note = {"id": self.next_note, "title": title, "urgency": urgency, "action": action, "state": "list"}
        self.next_note += 1
        self.notes.append(note)
        held = (urgency == "low" and "low-banners" not in self.quirks) or \
            (self.dnd and urgency != "critical" and "dnd-leaks" not in self.quirks)
        if not held:
            note["state"] = "queued"
            self.advance()
        return note["id"]

    def advance(self):
        if self.banners() and "two-banners" not in self.quirks:
            return
        for note in self.notes:
            if note["state"] == "queued":
                note["state"] = "banner"
                if "two-banners" not in self.quirks:
                    return

    def close(self, note, reason):
        self.events.append({"signal": "NotificationClosed", "args": [note["id"], reason]})
        note["state"] = "list" if reason == 1 else "closed"
        self.banner_focus = False
        self.advance()

    def invoke(self, note):
        """intake.rs invoke_action: ActionInvoked, the banner goes, history stays."""
        self.events.append({"signal": "ActionInvoked", "args": [note["id"], note["action"].split("=")[0]]})
        note["state"] = "list"
        self.banner_focus = False
        self.advance()

    def listed(self):
        return [n for n in self.notes if n["state"] in ("list", "queued", "banner")]

    def date_items(self):
        listed = self.listed()
        items = ["Previous month", "Next month"]
        if len(listed) > 1:
            items += [self.APP] + (["Collapse"] if self.expanded else [])
        items.append("Clear")
        return items

    def qs_items(self):
        return [line.split(": ", 1)[1] for line in golden_lines("quick-settings", "Top Bar")[6:]
                if not any(line.split(": ", 1)[1].startswith(a) for a in ABSENT)]

    def ui_key(self, names):
        if self.banner_focus:
            banner = self.banners()[0]
            if names == ("delete",):
                self.close(banner, 2)
            elif names == ("esc",):
                self.close(banner, 1)
            return
        items = self.qs_items() if self.qs else self.date_items()
        if names == ("tab",):
            self.ui_focus = (self.ui_focus + 1) % len(items)
        elif names == ("shift", "tab"):
            self.ui_focus = (self.ui_focus - 1) % len(items)
        elif names == ("ret",):
            item = items[self.ui_focus]
            if item == "Do Not Disturb":
                self.dnd = not self.dnd
            elif item == self.APP:
                self.expanded = True
                self.ui_focus = self.date_items().index("Collapse")
            elif item == "Clear":
                for note in self.listed():
                    note["state"] = "closed"
                self.ui_focus = self.date_items().index("Clear")
        elif names == ("esc",):
            if self.date and self.expanded:
                self.expanded = False
                self.ui_focus = self.date_items().index(self.APP)
            else:
                self.qs = self.date = False

    def a11y(self, frames):
        nodes = []

        def add(frame, role, name, extents=(0, 0, 20, 20), **extra):
            nodes.append({"role": role, "name": name, "toplevel": frame, "description": "", "checked": False,
                          "focused": False, "extents": list(extents), **extra})

        def lines(stage, frame, skip=()):
            for line in golden_lines(stage, frame):
                role, name = line.split(": ", 1)
                if not any(name.startswith(a) for a in skip):
                    add(frame, role, name, checked=(name == "Do Not Disturb" and self.dnd),
                        focused=self.qs and name in self.qs_items() and self.qs_items()[self.ui_focus] == name)
        if self.qs:
            lines("quick-settings", "Top Bar", ABSENT + (("Night Light",) if "qs-drops-night-light" in self.quirks else ()))
        else:
            lines("panel", "Top Bar")
        if self.date:
            items = self.date_items()
            for name in ("Previous month", "Next month"):
                add("Top Bar", "button", name, focused=items[self.ui_focus] == name)
            listed = self.listed()
            if len(listed) > 1:
                add("Top Bar", "button", self.APP, focused=items[self.ui_focus] == self.APP,
                    description=f"{len(listed)} notifications, {'expanded' if self.expanded else 'collapsed'}")
                shown = listed if self.expanded else listed[-1:]
                for note in shown:
                    add("Top Bar", "label", note["title"])
                if self.expanded:
                    add("Top Bar", "button", "Collapse", focused=items[self.ui_focus] == "Collapse")
            elif listed:
                add("Top Bar", "label", listed[0]["title"])
            else:
                add("Top Bar", "label", "No Notifications")
            add("Top Bar", "button", "Clear", focused=items[self.ui_focus] == "Clear")
        if self.overview:
            hovered = any(inside(self.pointer, p["rect"]) for p in self.previews())
            for frame in ("Search", "Dash") + (("Window Previews",) if hovered else ()):
                for line in golden_lines("overview", frame):
                    role, name = line.split(": ", 1)
                    add(frame, role, name, extents=(190, 10, 40, 40) if name == "Show Apps" else (0, 0, 20, 20))
        if self.grid_open:
            add("App Grid", "frame", "App Grid", extents=(0, 0) + tuple(self.GRID[2:]))
            for k in range(len(self.pages())):
                add("App Grid", "button", f"Page {k + 1}")
            for name, rect in self.grid_buttons().items():
                add("App Grid", "button", name, extents=rect)
            # GTK reports the other pages' tiles showing too, off the layer.
            for k, page in enumerate(self.pages()):
                for i, name in enumerate(page if k != self.page else ()):
                    add("App Grid", "button", name,
                        extents=(100 + (i % 8) * 130 + 1280 * (k - self.page), 20 + (i // 8) * 130, 96, 96))
        if self.banners():
            add("Notifications", "frame", "Notifications", extents=(0, 0) + tuple(self.BANNER[2:]))
            for note in self.banners():
                add("Notifications", "label", note["title"], focused=self.banner_focus)
                if note.get("action"):
                    add("Notifications", "button", note["action"].split("=")[1], extents=(20, 60, 120, 30))
        return [n for n in nodes if not frames or n["toplevel"] in frames]

    # -- the guest probe ----------------------------------------------------
    def start(self, role):
        ident = self.next_id
        self.next_id += 1
        rect = {"main": [360, 150, 560, 360], "bystander": list(WORK), "launched": [320, 180, 640, 420]}[role]
        app = f"org.tuna.VmGates.nkey.{role}"
        self.windows[ident] = {"id": ident, "app_id": app, "rect": rect, "workspace": self.active,
                               "layout": "maximized" if role == "bystander" else "floating", "stash": None}
        self.stack.append(ident)
        self.fixtures[role] = {"role": role, "id": ident, "app_id": app, "mapped": True, "alive": True,
                               "size": rect[2:], "surface": [rect[2] + 26, rect[3] + 26], "maximized": role == "bystander",
                               "tiled": False, "presses": 0, "releases": 0, "keys": 0, "nav_keys": 0,
                               "bounds": {"menu": [378, 6, 36, 34], "entry": [24, 302, 512, 34],
                                          "minimize": [427, 0, 40, 46], "maximize": [473, 0, 40, 46],
                                          "close": [519, 0, 41, 46]} if role == "main" else {}}
        self.focus(ident)
        return {"fixture": self.view()[role], "window": {"id": ident, "app_id": app}, "scene": self.scene()}

    def view(self):
        return {role: {k: v for k, v in f.items() if k != "id"} for role, f in self.fixtures.items()}


class FakeQmp:
    def __init__(self, world):
        self.world = world

    def keys(self, *names, hold_ms=80):
        self.world.chord(names)

    def click(self, x, y, button="left"):
        self.world.pointer = [float(x), float(y)]
        self.world.press()
        self.world.release()

    def cmd(self, name, **args):
        if name == "screendump":
            Path(args["filename"]).write_bytes(b"")
            return {}
        assert name == "input-send-event", name
        events = args["events"]
        if events[0]["type"] == "abs":
            self.world.pointer = [events[0]["data"]["value"] * 1280 / 32767, events[1]["data"]["value"] * 800 / 32767]
            self.world.motion()
        elif events[0]["type"] == "btn":
            self.world.press() if events[0]["data"]["down"] else self.world.release()
        elif events[0]["type"] == "key":
            self.world.super_held = events[0]["data"]["down"]
        return {}

    def frame(self, path):
        Path(path).write_bytes(b"")


class FakeAgent:
    def __init__(self, world):
        self.world, self.calls = world, []

    def run(self, action, *args, timeout=30):
        self.calls.append((action, args))
        w = self.world
        if action == "gates-state":
            return {"fixtures": w.view(), "scene": w.scene()}
        if action == "gates-start":
            return w.start(args[0])
        if action == "gates-a11y":
            return {"nodes": w.a11y(args), "visited": 100, "scene": w.scene()}
        if action == "gates-banners":
            if args[0] == "set":
                w.dnd = args[1] == "false"
            return {"show-banners": "false" if w.dnd else "true"}
        if action == "gates-watch-layer":
            w.layer_seen.wait(5)
            w.layer_seen.clear()
            return {"namespace": args[0], "scene": w.scene(),
                    **w.layer_log.pop(args[0], {"samples": [], "gone": False})}
        if action == "gates-notify":
            sub = args[0]
            if sub == "monitor-start":
                return {"monitor": [99]}
            if sub == "monitor-read":
                return {"events": [{"signal": "ready"}] + list(w.events)}
            if sub == "send":
                title, _body, urgency, *rest = args[2:]
                return {"id": w.send(title, urgency, rest[0] if rest else None)}
        if action == "gates-apps":
            if args[0] == "start":
                w.launch_name = f"Tuna VM Gates Zlaunch {args[1][:8]}"
                w.apps += [f"Tuna VM Gates Page {n:02d}" for n in range(1, 31)] + [w.launch_name]
                return {"launch_name": w.launch_name, "count": 31}
            return {"removed": 31}
        if action == "gates-stop":
            return {"confirmed": True}
        raise AssertionError(f"unexpected guest action {action}")


def accent(image_world):
    """A screendump: black, with the overview's accent ring left of the active thumbnail."""
    width, height = 1280, 800
    pixels = bytearray(width * height * 3)
    for t in image_world.thumbnails():
        if t["active"] and "no-ring" not in image_world.quirks:
            x, y = t["rect"][0] - 2, t["rect"][1] + t["rect"][3] // 2
            pixels[(y * width + x) * 3:(y * width + x) * 3 + 3] = bytes((53, 132, 228))
    return width, height, bytes(pixels)


def run_world(quirks=()):
    world, clock, records = World(quirks), Clock(), {}
    with tempfile.TemporaryDirectory() as out, mock.patch("time.sleep", clock.sleep), \
            mock.patch("time.monotonic", clock.monotonic):
        shell.run(FakeQmp(world), FakeAgent(world), out,
                  lambda name, ok, detail: records.__setitem__(name, (ok, detail)), lambda _path: accent(world))
        steps = json.loads(Path(out, "shell-gates", "steps.json").read_text())
    return records, world, steps


class EndToEnd(unittest.TestCase):
    def test_every_gate_passes_on_a_session_that_behaves(self):
        records, world, steps = run_world()
        self.assertEqual(set(records), set(shell.GATES))
        failed = {name: detail for name, (ok, detail) in records.items() if not ok}
        self.assertEqual(failed, {})
        self.assertFalse(world.dnd, "banners are back on at the end")
        self.assertEqual(steps[-1]["action"], "gates-stop")

    def test_each_gate_catches_its_own_breakage(self):
        cases = {
            "no-border": ("V-WINDOW-MOVE-RESIZE", "invisible border"),
            "arrows-leak": ("V-TILING-KEYS", "arrow press"),
            "slide-unscaled": ("V-WORKSPACE-KEYS", "not 200"),
            "no-popup": ("V-WORKSPACE-KEYS", "popup never appeared"),
            "escape-drops-focus": ("V-OVERVIEW-FOCUS", "did not return focus"),
            "no-strip": ("V-OVERVIEW-PICKER", "thumbnails"),
            "no-ring": ("V-OVERVIEW-PICKER", "accent ring"),
            "two-banners": ("V-NOTIFY", "two banners showed at once"),
            "dnd-leaks": ("V-NOTIFY-DND", "with Do Not Disturb on"),
            "qs-drops-night-light": ("V-A11Y-TREE", "Night Light"),
            "grid-no-launch": ("V-APP-GRID", "did not launch"),
            "no-edge-preview": ("V-WINDOW-MOVE-RESIZE", "no tile preview"),
            "low-banners": ("V-NOTIFY", "low-urgency"),
            "focus-stays-on-empty": ("V-WORKSPACE-KEYS", "focus on None"),
        }
        for quirk, (gate, message) in cases.items():
            with self.subTest(quirk):
                records, _, _ = run_world({quirk})
                ok, detail = records[gate]
                self.assertFalse(ok, detail)
                self.assertIn(message, detail)
                # A failed gate puts the desktop back: the others still pass.
                self.assertEqual([name for name, (ok, _) in records.items() if not ok], [gate])

    def test_a_failed_fixture_start_fails_every_gate_and_still_cleans_up(self):
        world, clock, records = World(), Clock(), {}
        agent = FakeAgent(world)
        original = agent.run

        def failing(action, *args, timeout=30):
            if action == "gates-start":
                return {"gates_error": "RuntimeError: no map"}
            return original(action, *args, timeout=timeout)
        agent.run = failing
        with tempfile.TemporaryDirectory() as out, mock.patch("time.sleep", clock.sleep), \
                mock.patch("time.monotonic", clock.monotonic):
            shell.run(FakeQmp(world), agent, out, lambda n, ok, d: records.__setitem__(n, (ok, d)), None)
        self.assertEqual(set(records), set(shell.GATES))
        self.assertTrue(all(not ok and "no map" in detail for ok, detail in records.values()))
        self.assertEqual([a for a, _ in agent.calls][-2:], ["gates-banners", "gates-stop"])

    def test_unconfirmed_cleanup_raises_after_recording(self):
        world, clock, records = World(), Clock(), {}
        agent = FakeAgent(world)
        original = agent.run
        agent.run = lambda action, *args, timeout=30: ({"gates_error": "RuntimeError: windows remain"}
                                                      if action in ("gates-start", "gates-stop")
                                                      else original(action, *args, timeout=timeout))
        with tempfile.TemporaryDirectory() as out, mock.patch("time.sleep", clock.sleep), \
                mock.patch("time.monotonic", clock.monotonic), self.assertRaises(RuntimeError) as raised:
            shell.run(FakeQmp(world), agent, out, lambda n, ok, d: records.__setitem__(n, (ok, d)), None)
        self.assertIn("windows remain", str(raised.exception))
        self.assertEqual(set(records), set(shell.GATES))


# ---- the guest probe's new actions -------------------------------------------

def guest_scope(*names, **extra):
    source = ROOT / "packaging" / "marlin" / "vm-lane" / "tuna-vm-desktop-gates"
    tree = ast.parse(source.read_text())
    body = [node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name in names]
    scope = {"json": json, "re": __import__("re"), "time": __import__("time"),
             "SCENE_KEYS": ("layers",), **extra}
    exec(compile(ast.Module(body=body, type_ignores=[]), str(source), "exec"), scope)
    return scope


class Scripted:
    def __init__(self, snapshots):
        self.snapshots = [json.dumps(s) for s in snapshots]

    def read_text(self):
        return self.snapshots.pop(0) if len(self.snapshots) > 1 else self.snapshots[0]


class Guest(unittest.TestCase):
    def test_the_layer_watch_ends_once_the_popup_has_come_and_gone(self):
        popup = {"namespace": "tuna-workspace-popup", "rect": [540, 340, 200, 120]}
        scope = guest_scope("watch_layer", SCENE=Scripted([{"layers": [BAR]}, {"layers": [BAR]},
                                                           {"layers": [BAR, popup]}, {"layers": [BAR, popup]},
                                                           {"layers": [BAR]}]))
        result = scope["watch_layer"]("tuna-workspace-popup", 3)
        self.assertEqual([s["rect"] for s in result["samples"]], [popup["rect"]])
        self.assertTrue(result["gone"])
        with self.assertRaises(ValueError):
            scope["watch_layer"]("../../etc", 3)

    def test_scene_keys_carry_what_the_shell_gates_read(self):
        source = (ROOT / "packaging" / "marlin" / "vm-lane" / "tuna-vm-desktop-gates").read_text()
        keys = ast.literal_eval(next(node.value for node in ast.parse(source).body
                                     if isinstance(node, ast.Assign)
                                     and getattr(node.targets[0], "id", None) == "SCENE_KEYS"))
        # animation_slowdown included: V-MINIMIZE-ANIMATION's 400 ms times the factor needs it too.
        for key in ("animation_slowdown", "previews", "thumbnails", "workspace_cards", "workspaces",
                    "active_workspace", "tile_preview", "workspace_slide", "session_mode", "overview_state",
                    "overview_app_grid", "overview_progress", "pointer_position", "layers"):
            self.assertIn(key, keys)
        self.assertEqual(set(World().scene()) - set(keys), set())

    def test_notification_text_is_fixed_vocabulary(self):
        source = (ROOT / "packaging" / "marlin" / "vm-lane" / "tuna-vm-desktop-gates").read_text()
        text = ast.literal_eval(next(node.value.args[0] for node in ast.parse(source).body
                                     if isinstance(node, ast.Assign) and getattr(node.targets[0], "id", None) == "TEXT"))
        import re
        self.assertTrue(re.fullmatch(text, "Tuna VM Gates first banner 0123abcd"))
        self.assertIsNone(re.fullmatch(text, "-u critical"))
        self.assertIsNone(re.fullmatch(text, "x" * 80))

    def test_every_shell_gate_is_defaulted_by_the_lane(self):
        lane = (ROOT / "scripts" / "tuna-vm-lane").read_text()
        self.assertIn("for gate in SHELL_GATES:", lane)
        self.assertIn('on_tablet("shell-gates"', lane)


if __name__ == "__main__":
    unittest.main()
