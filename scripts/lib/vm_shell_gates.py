"""Installed shell gates on the marlin-vm lifecycle boot.

The installed-image proof of rows the nested session already passes, each
checking the same user-visible claim on the real Marlin session:

  V-OVERVIEW-PICKER   P-OV-02    the overview's window picker previews the
                                 workspace's windows, three workspaces show
                                 GNOME 51's thumbnail strip (43x26 at 1280x800,
                                 an accent ring on the active one), a thumbnail
                                 click switches, and a window preview dragged
                                 onto a thumbnail moves the window there
  V-APP-GRID          P-OV-05    Show Apps opens the app grid, thirty more apps
                                 make it page, Next Page turns it, and a click
                                 on an app launches it and closes the overview
  V-OVERVIEW-FOCUS    P-OV-06    Escape out of the overview returns keyboard
                                 focus to the window that had it, at its
                                 rectangle, for two different windows
  V-NOTIFY            P-NT-01    notify-send shows a top-centre banner, the
                                 pointer on it shows its action and holds it,
                                 one banner at a time, an action press
                                 reaches the app, Delete dismisses (reason 2),
                                 Escape expires (reason 1), low urgency never
                                 banners
  V-NOTIFY-DND        P-NT-02    the date menu lists held notifications grouped
                                 per app (collapsed, expand, collapse, Clear);
                                 Do Not Disturb from quick settings holds banners
                                 back but not critical ones; turning it off
                                 restores them
  V-TILING-KEYS       P-WM-02    Super+Up maximizes, Super+Left and Super+Right
                                 tile halves of the work area, repeats and
                                 Super+Down restore, and the client never sees
                                 the arrows
  V-WORKSPACE-KEYS    P-WM-03    Super+Page Down/Up switch workspace over
                                 GNOME's 250 ms slide (times the session's
                                 slow-down factor) with the switcher popup;
                                 with Shift they move the window and follow it
  V-WINDOW-MOVE-RESIZE P-WM-04   real pointer drags through QMP: the header bar
                                 moves a window, its invisible shadow border
                                 resizes it from the right, bottom-right and
                                 left edges, Super+drag moves it without the
                                 client, and the screen edges show GNOME's tile
                                 preview and tile or maximize on drop
  V-A11Y-TREE         P-A11Y-01  the shell's AT-SPI tree at the panel, the
                                 overview and quick settings has the nested
                                 goldens' (tests/a11y/*.txt) frames, controls
                                 and order, minus the hardware-dependent tiles

Every key, click and drag is a QMP input event on the VM's own tablet and
keyboard; the guest probe (packaging/marlin/vm-lane/tuna-vm-desktop-gates)
only launches fixtures, runs the session user's notify-send, writes the app
grid's desktop entries and reads the shell's AT-SPI tree, the compositor's
state snapshot and the notification bus. Observations are pure functions,
tested against real session data in scripts/tests/test_vm_shell_gates.py.
"""
import re
import time
from pathlib import Path

from vm_desktop_gates import (GateError, Gates, HEIGHT, WIDTH, contains, fixture_window, inside, primary_output,
                              work_area)
from vm_strip_acceptance import exposed_point

GATES = ("V-OVERVIEW-PICKER", "V-APP-GRID", "V-OVERVIEW-FOCUS", "V-NOTIFY", "V-NOTIFY-DND",
         "V-TILING-KEYS", "V-WORKSPACE-KEYS", "V-WINDOW-MOVE-RESIZE", "V-A11Y-TREE")
GOLDEN = Path(__file__).resolve().parents[2] / "tests" / "a11y"
# The compositor snapshot's layer namespace of each AT-SPI top-level frame
# the lane clicks in (overview.rs, notify.rs, the panel).
FRAME_LAYERS = {"Top Bar": "tuna-panel", "Search": "tuna-shell-overview", "Dash": "tuna-shell-dash",
                "App Grid": "tuna-shell-appgrid", "Notifications": "tuna-shell-banners"}
# GNOME Shell 51 at 1280x800 (overview.rs thumbnails_strip_matches_gnome_51).
THUMBNAIL, CARD, STRIP_Y, CARD_Y, PITCH = (43, 26), (849, 509), 102, 152, 49
# GNOME Shell 51 workspacesView.js WORKSPACE_ANIMATION_TIME.
SLIDE_MS = 250
SNAP_EDGE_PX = 2
TOP_EDGE_Y = 33
# What a screen-reader user reaches in the nested goldens, minus what depends on the
# hardware and services of the machine (the comparison.md "Intended
# differences"): these tiles show only when their service is on the bus.
OPTIONAL_GROUPS = (
    ("push button: Mute", "slider: Volume"),
    # The output menu button shows only with more than one output device
    # (gnome-shell 51 volume.js:173 `menuEnabled`, quickSettings.js:353).
    ("toggle button: Open sound output menu",),
    ("slider: Brightness",),
    ("toggle button: Wi-Fi", "toggle button: Wi-Fi Menu"),
    ("toggle button: Wired", "toggle button: Wired Menu"),
    ("toggle button: Bluetooth", "toggle button: Bluetooth Menu"),
    ("toggle button: Printers", "toggle button: Printers Menu"),
    ("toggle button: Mobile Network", "toggle button: Mobile Network Menu"),
    ("toggle button: Power Mode", "toggle button: Power Mode Menu"),
)
# Per stage: the golden file, and per frame the roles whose names depend on
# what is installed or open (the dash's app buttons, a hovered preview's Close).
STAGES = {
    "panel": ("panel.txt", {}),
    "overview": ("overview.txt", {"Dash": ("push button",), "Window Previews": ("push button",)}),
    "quick-settings": ("quick-settings.txt", {}),
}
DAY = re.compile(r"^\d{1,2}$")
DATE = re.compile(r"^(Monday|Tuesday|Wednesday|Thursday|Friday|Saturday|Sunday) [A-Z][a-z]+ \d{1,2} \d{4}$")


# ---- observations (scripts/tests/test_vm_shell_gates.py) -------------------

def slowdown_ms(scene, base):
    """GNOME's adjustAnimationTime: `base` ms times the session's
    GNOME_SHELL_SLOWDOWN_FACTOR, which the compositor reports as
    `animation_slowdown` (TunaOS images set 0.8)."""
    factor = scene.get("animation_slowdown")
    if not isinstance(factor, (int, float)) or factor <= 0:
        factor = 1.0
    return round(base * factor)


def canonical_role(role):
    """Newer AT-SPI reports GNOME's `button` where the goldens say `push button`."""
    return "push button" if role == "button" else role


def stable(name):
    """scripts/tuna-a11y-golden: names that change with the date read the same."""
    if DAY.match(name):
        return "#"
    if DATE.match(name):
        return "<date>"
    return name


def observed_groups(nodes):
    """The showing, named, non-label nodes as `role: name` lines in tree order,
    grouped by AT-SPI top-level frame. GTK's outer and inner exposure of one
    control (the same line twice in a row) counts once."""
    groups = {}
    for node in nodes:
        if not node.get("name") or node.get("role") == "label" or node.get("showing") is False:
            continue
        lines = groups.setdefault(node.get("toplevel"), [])
        line = f"{canonical_role(node['role'])}: {stable(node['name'])}"
        if not lines or lines[-1] != line:
            lines.append(line)
    return groups


def golden_groups(text):
    """A golden's lines grouped by the `frame:` line that opens each top level."""
    groups = []
    for line in text.splitlines():
        if line.startswith("frame: "):
            groups.append((line[len("frame: "):], []))
        if groups:
            groups[-1][1].append(line)
    return groups


def optional_group(line):
    return next((group for group in OPTIONAL_GROUPS if line in group), None)


def a11y_problems(stage, nodes, golden=None):
    """The shell's tree against the nested golden of the same stage: every
    frame shows, every control of the golden is there in the golden's order,
    except what the machine's hardware decides (OPTIONAL_GROUPS: all of a
    group or none) and the names that depend on what is installed or open."""
    name, dynamic = STAGES[stage]
    text = golden if golden is not None else (GOLDEN / name).read_text()
    seen = observed_groups(nodes)
    problems = []
    for frame, want in golden_groups(text):
        got = seen.get(frame)
        if got is None:
            problems.append(f"{stage}: frame {frame!r} is not showing")
            continue
        present = set(got)
        required = []
        for line in want:
            role = line.split(": ", 1)[0]
            if role in dynamic.get(frame, ()):
                continue
            group = optional_group(line)
            if group is not None and not present & set(group):
                continue
            required.append(line)
        missing = [line for line in required if line not in present]
        if missing:
            problems.append(f"{stage}: {frame!r} lacks {missing}")
        found = [line for line in required if line in present]
        order = list(dict.fromkeys(line for line in got if line in set(found)))
        if order != found:
            problems.append(f"{stage}: {frame!r} lists {order}, the golden's order is {found}")
    return problems


def find_nodes(nodes, name, toplevel=None, roles=None):
    return [n for n in nodes if n.get("name") == name and (toplevel is None or n.get("toplevel") == toplevel)
            and (roles is None or canonical_role(n.get("role")) in roles)]


def has(nodes, name, toplevel=None):
    return bool(find_nodes(nodes, name, toplevel))


def layer_rect(scene, namespace):
    rects = [layer["rect"] for layer in scene.get("layers") or [] if layer.get("namespace") == namespace]
    if len(rects) != 1:
        raise GateError(f"{len(rects)} {namespace} layers in the compositor snapshot, not one")
    return rects[0]


def node_point(nodes, name, scene, toplevel, roles=None):
    """The control's centre on screen: its top-level frame's layer origin plus
    its window-relative AT-SPI extents (Wayland exposes no global ones). GTK
    exposes some controls twice, nested, under one name; those count as one,
    clicked at the innermost. Anything else ambiguous is refused."""
    found = [n for n in find_nodes(nodes, name, toplevel, roles) if len(n.get("extents") or ()) == 4]
    if not found:
        raise GateError(f"no {name!r} control showing in {toplevel!r}")
    inner = min(found, key=lambda n: n["extents"][2] * n["extents"][3])
    if not all(contains(n["extents"], inner["extents"]) for n in found):
        raise GateError(f"{len(found)} un-nested {name!r} controls in {toplevel!r}")
    namespace = FRAME_LAYERS.get(toplevel)
    if namespace is None:
        raise GateError(f"no layer known for frame {toplevel!r}")
    layer = layer_rect(scene, namespace)
    x, y, w, h = inner["extents"]
    point = (layer[0] + x + w // 2, layer[1] + y + h // 2)
    if w <= 0 or h <= 0 or not inside(point, layer) or not inside(point, (0, 0, WIDTH, HEIGHT)):
        raise GateError(f"{name!r} centre {point} is outside its layer {layer} or the output")
    return point


def on_screen(nodes, name, scene, toplevel):
    """node_point, or None while the control is off its layer (another page)."""
    try:
        return node_point(nodes, name, scene, toplevel)
    except GateError:
        return None


def focused_node(nodes, matches):
    return any(n.get("focused") and matches(n) for n in nodes)


def window_of(scene, ident):
    rows = [w for w in scene.get("windows") or [] if w.get("id") == ident]
    return rows[0] if len(rows) == 1 else None


def half_rects(area):
    """Mutter's half tiles of a work area (windows.rs tile_area)."""
    x, y, w, h = area
    half = w // 2
    return [x, y, half, h], [x + w - half, y, half, h]


def vivid(pixel):
    """The overview's active-workspace ring follows the session accent: a vivid
    colour rather than one hue (tuna-gtk-shell-proof G-WS-THUMBS)."""
    high, low = max(pixel), min(pixel)
    return high - low > 100 and high > 150


def pixel_at(image, x, y):
    width = image[0]
    offset = (y * width + x) * 3
    return tuple(image[2][offset:offset + 3])


def strip_problems(scene, occupied):
    """Three workspaces put GNOME 51's thumbnail strip in the overview: one
    43x26 thumbnail per workspace in a row 6 px apart, centred at y 102, the
    active one marked, an empty trailing workspace, and the active card
    shrunk to 849x509 at y 152. `occupied` is the set of workspaces with windows."""
    problems = []
    thumbs = scene.get("thumbnails") or []
    order = sorted(thumbs, key=lambda t: t["workspace"])
    if len(thumbs) < 3:
        return [f"{len(thumbs)} thumbnails, not the strip of three or more"]
    if [t["workspace"] for t in order] != list(range(order[0]["workspace"], order[0]["workspace"] + len(order))):
        problems.append(f"thumbnail workspaces {[t['workspace'] for t in order]} are not consecutive")
    if not set(occupied) <= {t["workspace"] for t in order} or order[-1]["workspace"] in occupied:
        problems.append(f"thumbnails {[t['workspace'] for t in order]} do not end with one empty workspace "
                        f"after {sorted(occupied)}")
    if any(tuple(t["rect"][2:]) != THUMBNAIL for t in order):
        problems.append(f"thumbnail sizes {sorted({tuple(t['rect'][2:]) for t in order})}, not {THUMBNAIL}")
    if any(abs(t["rect"][1] - STRIP_Y) > 1 for t in order):
        problems.append(f"thumbnail rows {sorted({t['rect'][1] for t in order})}, not {STRIP_Y}")
    left = (WIDTH - (len(order) * THUMBNAIL[0] + (len(order) - 1) * (PITCH - THUMBNAIL[0]))) / 2
    if any(abs(t["rect"][0] - (left + PITCH * i)) > 2 for i, t in enumerate(order)):
        problems.append(f"thumbnail columns {[t['rect'][0] for t in order]}, not {PITCH} px apart from {left:.0f}")
    active = [t for t in order if t.get("active")]
    if len(active) != 1 or active[0]["workspace"] != scene.get("active_workspace"):
        problems.append(f"active thumbnails {[t['workspace'] for t in active]} for workspace "
                        f"{scene.get('active_workspace')}")
    cards = [c for c in scene.get("workspace_cards") or [] if c.get("active")]
    if len(cards) != 1 or (cards[0]["rect"][2], cards[0]["rect"][3]) != CARD or abs(cards[0]["rect"][1] - CARD_Y) > 1:
        problems.append(f"active card {[c['rect'] for c in cards]}, not {CARD[0]}x{CARD[1]} at y {CARD_Y}")
    return problems


def picker_problems(scene, workspace):
    """The window picker previews every shown window of the workspace, each
    scaled down inside the output."""
    problems = []
    minimized = set(scene.get("minimized") or [])
    wanted = {w["id"]: w for w in scene.get("windows") or []
              if w.get("workspace") == workspace and w["id"] not in minimized}
    previews = {p["id"]: p for p in scene.get("previews") or []}
    if set(previews) != set(wanted):
        problems.append(f"previews of windows {sorted(previews)}, the workspace shows {sorted(wanted)}")
    output = primary_output(scene)["rect"]
    for ident, preview in previews.items():
        rect, window = preview["rect"], wanted.get(ident)
        if not contains(output, rect) or rect[2] <= 0 or rect[3] <= 0:
            problems.append(f"preview {ident} at {rect} is not inside the output")
        elif window is not None and (rect[2] >= window["rect"][2] or rect[3] >= window["rect"][3]):
            problems.append(f"preview {ident} {rect[2:]} is not scaled down from its window {window['rect'][2:]}")
    return problems


def slide_record(history, scene):
    """The workspace switch that finished since `history` was read, if any."""
    now = (scene.get("workspace_slide") or {}).get("history") or []
    return now[-1] if now and now != history else None


def slide_problems(record, source, target, scene):
    """GNOME's key switch: both workspaces slide over 250 ms times the
    session's slow-down factor, drawn over more than one frame."""
    if record is None:
        return [f"no workspace switch {source} -> {target} was recorded"]
    problems = []
    if record.get("motion") != "slide" or (record.get("from"), record.get("to")) != (source, target):
        problems.append(f"switch recorded {record.get('motion')} {record.get('from')} -> {record.get('to')}, "
                        f"not a slide {source} -> {target}")
    expected = slowdown_ms(scene, SLIDE_MS)
    if record.get("duration_ms") != expected:
        problems.append(f"the slide lasted {record.get('duration_ms')} ms, not {expected}")
    if not isinstance(record.get("frames"), int) or record["frames"] < 2:
        problems.append(f"the slide drew {record.get('frames')} frame(s), not at least two")
    return problems


def popup_problems(watch):
    """The workspace switcher popup showed during the switch, inside the output,
    and went again."""
    samples = watch.get("samples") or []
    if not samples:
        return ["the workspace switcher popup never appeared"]
    problems = []
    if any(not contains((0, 0, WIDTH, HEIGHT), s["rect"]) or s["rect"][2] <= 0 for s in samples):
        problems.append(f"the popup at {[s['rect'] for s in samples][:2]} is not inside the output")
    if watch.get("gone") is not True:
        problems.append("the workspace switcher popup never went away")
    return problems


def banner_problems(rect, output):
    """GNOME's banner: top centre, just under the top bar."""
    problems = []
    if not contains(output["rect"], rect) or rect[2] <= 0 or rect[3] <= 0:
        problems.append(f"banner {rect} is not inside the output")
    elif abs(rect[0] + rect[2] / 2 - (output["rect"][0] + output["rect"][2] / 2)) > 3:
        problems.append(f"banner {rect} is not centred on the output")
    if rect[1] > 80:
        problems.append(f"banner {rect} is not at the top of the output")
    return problems


def events_for(events, signal, ident):
    return [e for e in events if e.get("signal") == signal and e.get("args") and e["args"][0] == ident]


def closed_reason(events, ident):
    """NotificationClosed's reason for one notification: 1 expired, 2 dismissed."""
    rows = events_for(events, "NotificationClosed", ident)
    return rows[-1]["args"][1] if rows else None


def action_invoked(events, ident):
    rows = events_for(events, "ActionInvoked", ident)
    return rows[-1]["args"][1] if rows else None


def group_node(nodes, app):
    """The date menu's collapsed notification group for an app."""
    return next((n for n in nodes if n.get("name") == app
                 and (n.get("description") or "").endswith("notifications, collapsed")), None)


def near(point, target, tolerance=2):
    return point is not None and all(abs(a - b) <= tolerance for a, b in zip(point, target))


def move_problems(before, after, delta, size):
    """The header-bar drag moved the window by the drag distance (the nested
    G-APP-DRAG bound: at least half of it, at most all, on each axis) and left
    its size alone."""
    problems = []
    for axis, name in ((0, "x"), (1, "y")):
        moved = after[axis] - before[axis]
        low, high = sorted((delta[axis] / 2, delta[axis] + (2 if delta[axis] >= 0 else -2)))
        if not low <= moved <= high:
            problems.append(f"the window moved {moved} px in {name} for a {delta[axis]} px drag")
    if list(after[2:]) != list(size):
        problems.append(f"the window's size changed from {size} to {after[2:]}")
    return problems


def resize_problems(before, after, grow, origin_shift=(0, 0)):
    """An edge drag changed the size by exactly the drag (within 2 px) and moved
    the origin only for a left or top edge."""
    problems = []
    for axis, name in ((0, "width"), (1, "height")):
        got = after[2 + axis] - before[2 + axis]
        if abs(got - grow[axis]) > 2:
            problems.append(f"{name} changed by {got}, not {grow[axis]}")
    for axis, name in ((0, "x"), (1, "y")):
        got = after[axis] - before[axis]
        if abs(got - origin_shift[axis]) > 2:
            problems.append(f"{name} moved {got}, not {origin_shift[axis]}")
    return problems


def shadow_problems(surface, rect):
    """GTK draws its resize handles in the invisible border outside the window
    geometry: the surface must be larger than the geometry on both axes, by
    more than the 4 px the lane grabs from."""
    extra = [surface[0] - rect[2], surface[1] - rect[3]]
    if min(extra) < 12:
        return [f"GTK's surface {surface} leaves no invisible border around the geometry {rect[2:]}"]
    return []


def queue_problems(samples, first, second):
    """GNOME's message tray shows one banner at a time: never both, the first
    alone, and the second only once the first is gone."""
    problems = []
    if any(first in s and second in s for s in samples):
        problems.append("two banners showed at once")
    if not any(first in s and second not in s for s in samples):
        problems.append("the first banner was never seen alone")
    return problems


# ---- the gates on a live guest ---------------------------------------------

class ShellGates(Gates):
    def __init__(self, qmp, agent, out, record, read_ppm):
        super().__init__(qmp, agent, out, record, read_ppm, name="shell-gates")
        self.tag = self.key[:8]
        self.home = None

    def gate(self, name, body, *args):
        """Any failure of a gate's own steps is that gate's recorded failure;
        the desktop is put back to rest for the next one."""
        try:
            detail = body(*args)
        except Exception as error:  # noqa: BLE001 - the lane records it; cleanup still runs
            self.record(name, False, f"{type(error).__name__}: {error}"[:900])
            self.recorded.add(name)
            self.rest()
            return False
        self.record(name, True, detail)
        self.recorded.add(name)
        return True

    def rest(self):
        """Best effort after a failure: no held keys or buttons, no menu or
        overview, the main window and the session on their workspace, banners on."""
        for step in (lambda: self.button(False), lambda: self.hold("meta_l", False), lambda: self.qmp.keys("esc"),
                     self.idle_desktop, lambda: self.put_back(self.main(self.state())[1]["id"], self.home),
                     lambda: self.guest("gates-banners", "set", "true")):
            try:
                step()
            except Exception:  # noqa: BLE001 - the failed gate's own message is the record
                continue

    # Input the base lane helpers lack: held buttons and keys.
    def button(self, down, name="left"):
        self.qmp.cmd("input-send-event", events=[{"type": "btn", "data": {"down": down, "button": name}}])
        time.sleep(0.1)

    def hold(self, qcode, down):
        self.qmp.cmd("input-send-event", events=[
            {"type": "key", "data": {"down": down, "key": {"type": "qcode", "data": qcode}}}])
        time.sleep(0.1)

    def drag(self, start, end, during=None, steps=12, keys=()):
        """Press at `start`, walk to `end`, run `during` with the button held,
        release. Super and friends in `keys` are held across the whole drag."""
        for qcode in keys:
            self.hold(qcode, True)
        try:
            self.move(*start)
            self.button(True)
            for step in range(1, steps + 1):
                self.move(start[0] + (end[0] - start[0]) * step / steps, start[1] + (end[1] - start[1]) * step / steps)
            time.sleep(0.3)
            if during is not None:
                during()
        finally:
            self.button(False)
            for qcode in reversed(keys):
                self.hold(qcode, False)
        time.sleep(0.4)

    def armed(self, action, args, act, seconds=6):
        """Arm a guest watcher, then send `act`'s real input (Gates.watched for
        the guest's other watchers)."""
        import threading
        box = {}

        def watch():
            try:
                box["value"] = self.guest(action, *args, seconds, timeout=seconds + 20)
            except Exception as error:  # noqa: BLE001 - re-raised on the lane's thread below
                box["error"] = error
        thread = threading.Thread(target=watch, daemon=True)
        thread.start()
        time.sleep(1.0)
        act()
        thread.join(seconds + 25)
        if thread.is_alive() or "error" in box:
            raise GateError(f"{action} watch failed: {box.get('error', 'still running')}")
        return box["value"]

    # The shell's accessibility tree.
    def dump(self, *frames):
        return self.guest("gates-a11y", *frames, timeout=60)

    def see(self, predicate, message, *frames, timeout=20, interval=0.4):
        deadline = time.monotonic() + timeout
        last = None
        while time.monotonic() < deadline:
            last = self.dump(*frames)
            if predicate(last["nodes"]):
                return last
            time.sleep(interval)
        self.save(f"a11y-timeout-{len(self.log):03d}.json", last)
        raise GateError(message)

    def settled_absence(self, name, message, *frames, samples=3, interval=0.8):
        """`name` shows in none of several consecutive trees."""
        for _ in range(samples):
            last = self.dump(*frames)
            if has(last["nodes"], name):
                self.save(f"a11y-present-{len(self.log):03d}.json", last)
                raise GateError(message)
            time.sleep(interval)

    def focus_node(self, matches, schedule, message):
        """Walk the keyboard focus onto the control `matches` accepts: each
        (keys, count) step sends its keys up to `count` times, reading the
        shell's focus after each."""
        last = None
        for keys, count in schedule:
            for _ in range(count):
                self.qmp.keys(*keys)
                time.sleep(0.3)
                last = self.dump()
                if focused_node(last["nodes"], matches):
                    return last
        self.save(f"focus-timeout-{len(self.log):03d}.json", last)
        raise GateError(message)

    # Desktop states.
    def idle_desktop(self):
        value = self.state()
        scene = value["scene"]
        if scene.get("session_mode") != "gnome":
            raise GateError(f"the session is in {scene.get('session_mode')!r} mode, not the floating desktop")
        if scene["overview_open"]:
            self.qmp.keys("esc")
            value = self.wait(lambda v: not v["scene"]["overview_open"], 10, "Escape did not close the overview")
        return value

    def open_overview(self):
        self.qmp.keys("meta_l")
        return self.wait(lambda v: v["scene"]["overview_open"] and (v["scene"].get("overview_progress") or 0) >= 1,
                         15, "Super did not open the settled overview")

    def close_overview(self):
        self.qmp.keys("esc")
        return self.wait(lambda v: not v["scene"]["overview_open"], 10, "Escape did not close the overview")

    def banners_state(self):
        return self.guest("gates-banners", "get")["show-banners"].strip()

    def events(self):
        return self.guest("gates-notify", "monitor-read", self.key)["events"]

    def send(self, title, urgency="normal", action=None):
        args = [title, "Sent with notify-send by the lane", urgency] + ([action] if action else [])
        return self.guest("gates-notify", "send", self.key, *args, timeout=40)

    def title(self, word):
        return f"Tuna VM Gates {word} {self.tag}"

    # ---- V-A11Y-TREE ------------------------------------------------------
    def a11y_tree(self):
        self.idle_desktop()
        counts, problems = {}, []

        def stage(name, dump):
            self.save(f"a11y-{name}.json", dump["nodes"])
            counts[name] = len(observed_groups(dump["nodes"]).get("Top Bar", [])) + sum(
                len(lines) for frame, lines in observed_groups(dump["nodes"]).items() if frame != "Top Bar")
            problems.extend(a11y_problems(name, dump["nodes"]))

        stage("panel", self.see(lambda n: has(n, "Activities", "Top Bar") and has(n, "System", "Top Bar"),
                                "the top bar never showed its controls"))
        # The golden was taken with a window preview hovered: its chrome is a frame of its own.
        previews = self.open_overview()["scene"].get("previews") or []
        if not previews:
            raise GateError("the overview shows no window preview to hover")
        hovered = previews[0]["rect"]
        self.move(hovered[0] + hovered[2] // 2, hovered[1] + hovered[3] // 2)
        stage("overview", self.see(lambda n: has(n, "Show Apps", "Dash") and has(n, "Search", "Search")
                                   and has(n, "Window Previews"),
                                   "the overview never showed its dash, search and a hovered preview's chrome"))
        self.close_overview()
        self.qmp.keys("meta_l", "s")
        stage("quick-settings", self.see(lambda n: has(n, "Do Not Disturb") and has(n, "Take Screenshot"),
                                         "Super+S never showed quick settings"))
        self.qmp.keys("esc")
        self.see(lambda n: not has(n, "Do Not Disturb"), "Escape did not close quick settings")
        if problems:
            raise GateError("; ".join(problems))
        return (f"the shell's AT-SPI tree matches the nested goldens (tests/a11y/*.txt) at the panel "
                f"({counts['panel']} named nodes), the overview ({counts['overview']}) and quick settings "
                f"({counts['quick-settings']}): every frame shows, every control in the golden's order, "
                "hardware-dependent tiles whole or absent (shell-gates/a11y-*.json)")

    # ---- V-NOTIFY ---------------------------------------------------------
    def notify(self):
        self.idle_desktop()
        if self.banners_state() != "true":
            raise GateError("show-banners is off before the gate starts")
        self.guest("gates-notify", "monitor-start", self.key)
        output = primary_output(self.state()["scene"])
        first, second, third, quiet = (self.title(w) for w in ("first banner", "queued", "keyboard", "low"))
        one = self.send(first, action="open=Open Probe")
        shown = self.see(lambda n: has(n, first), "no banner for notify-send", "Notifications")
        banner = layer_rect(shown["scene"], "tuna-shell-banners")
        placement = banner_problems(banner, output)
        self.frame("notify-banner")
        # GNOME 51's banner shows its action buttons only expanded (messageList.js:628-649), which the
        # pointer coming onto it does, holding it there too (messageTray.js:977-997, :1105-1108).
        self.move(banner[0] + banner[2] // 2, min(HEIGHT - 1, banner[1] + banner[3] + 120))
        time.sleep(0.3)
        self.move(*node_point(shown["nodes"], first, shown["scene"], "Notifications"))
        self.see(lambda n: has(n, "Open Probe", "Notifications"), "the pointer on the banner never showed its action",
                 "Notifications")
        two = self.send(second)
        time.sleep(1.5)
        together = self.dump("Notifications")
        samples = [{first, second} & {n["name"] for n in shown["nodes"]},
                   {first, second} & {n["name"] for n in together["nodes"]}]
        problems = placement + queue_problems(samples, first, second)
        self.save("notify-queue.json", {"shown": shown["nodes"], "together": together["nodes"]})
        press = node_point(together["nodes"], "Open Probe", together["scene"], "Notifications")
        self.qmp.click(*press)
        self.see(lambda n: not has(n, first) and has(n, second), "the action press did not advance the banner queue",
                 "Notifications")
        events = self.wait_events(lambda e: action_invoked(e, one["id"]) is not None, "the action never reached the app")
        if action_invoked(events, one["id"]) != "open":
            problems.append(f"the banner's action reached the app as {action_invoked(events, one['id'])!r}")
        # Keyboard: Super+N focuses the banner, Delete dismisses it, Escape expires one.
        self.qmp.keys("meta_l", "n")
        self.see(lambda n: focused_node(n, lambda x: x.get("name") == second), "Super+N did not focus the banner",
                 "Notifications")
        self.qmp.keys("delete")
        self.see(lambda n: not has(n, second), "Delete did not close the focused banner", "Notifications")
        three_sent = self.send(third)
        self.see(lambda n: has(n, third), "no banner for the third notification", "Notifications")
        self.qmp.keys("meta_l", "n")
        self.see(lambda n: focused_node(n, lambda x: x.get("name") == third), "Super+N did not focus the second banner",
                 "Notifications")
        self.qmp.keys("esc")
        self.see(lambda n: not has(n, third), "Escape did not expire the focused banner", "Notifications")
        events = self.wait_events(lambda e: closed_reason(e, three_sent["id"]) is not None,
                                  "the banner's expiry never reached the app")
        if closed_reason(events, two["id"]) != 2:
            problems.append(f"Delete closed the banner with reason {closed_reason(events, two['id'])}, not 2 (dismissed)")
        if closed_reason(events, three_sent["id"]) != 1:
            problems.append(f"Escape closed the banner with reason {closed_reason(events, three_sent['id'])}, not 1 (expired)")
        # Low urgency is for the list only.
        self.send(quiet, "low")
        self.settled_absence(quiet, "a low-urgency notification showed a banner", "Notifications")
        self.save("notify-events.json", events)
        if problems:
            raise GateError("; ".join(problems))
        return ("notify-send's banner shows top centre under the bar, the pointer on it shows its action, the next "
                "waits for it "
                "(one banner at a time) and shows when the action press reaches the app; Super+N then Delete "
                "dismisses (reason 2), Escape expires (reason 1); low urgency never banners "
                "(shell-gates/notify-*.json)")

    def wait_events(self, predicate, message, timeout=15):
        deadline = time.monotonic() + timeout
        events = []
        while time.monotonic() < deadline:
            events = self.events()
            if predicate(events):
                return events
            time.sleep(0.4)
        self.save(f"events-timeout-{len(self.log):03d}.json", events)
        raise GateError(message)

    # ---- V-NOTIFY-DND -----------------------------------------------------
    def toggle_dnd(self, want):
        """Do Not Disturb from the quick-settings tile, by keys: Shift+Tab wraps
        to the last stop, the tile."""
        self.idle_desktop()
        self.qmp.keys("meta_l", "s")
        self.see(lambda n: has(n, "Do Not Disturb"), "Super+S never showed quick settings")
        self.focus_node(lambda n: n.get("name") == "Do Not Disturb", [(("shift", "tab"), 40), (("tab",), 40)],
                        "the keyboard never reached the Do Not Disturb tile")
        self.qmp.keys("ret")
        seen = self.see(lambda n: any(x["name"] == "Do Not Disturb" and x["checked"] == want for x in n),
                        f"the Do Not Disturb tile never read {'on' if want else 'off'}")
        shown = self.banners_state()
        self.qmp.keys("esc")
        self.see(lambda n: not has(n, "Do Not Disturb"), "Escape did not close quick settings")
        if shown != ("false" if want else "true"):
            raise GateError(f"show-banners is {shown} with the tile {'on' if want else 'off'}")
        return seen

    def open_list(self):
        self.qmp.keys("meta_l", "v")
        return self.see(lambda n: has(n, "Previous month"), "Super+V never opened the date menu")

    def dnd(self):
        self.idle_desktop()
        if self.banners_state() != "true":
            raise GateError("show-banners is off before the gate starts")
        app, held_one, held_two, critical, after = "Tuna VM Gates", self.title("held one"), self.title("held two"), \
            self.title("critical"), self.title("after")
        self.toggle_dnd(True)
        self.send(held_one)
        self.send(held_two)
        self.settled_absence(held_one, "a banner showed with Do Not Disturb on", "Notifications")
        self.settled_absence(held_two, "a banner showed with Do Not Disturb on", "Notifications")
        # The held notifications are in the date menu, grouped per app: collapsed the newest card shows
        # alone, a press (Enter) expands the group under a Collapse button, Escape collapses it again
        # before the menu closes.
        listed = self.open_list()
        problems = []
        group = group_node(listed["nodes"], app)
        if group is None:
            problems.append(f"no collapsed {app!r} group in the date menu")
        if not has(listed["nodes"], held_two) or has(listed["nodes"], held_one):
            problems.append("the collapsed group does not show only its newest card")
        if has(listed["nodes"], "No Notifications"):
            problems.append("the empty state shows next to notifications")
        self.focus_node(lambda n: n.get("name") == app and (n.get("description") or "").endswith("notifications, collapsed"),
                        [(("shift", "tab"), 12), (("tab",), 80)], "the keyboard never reached the collapsed group")
        self.qmp.keys("ret")
        expanded = self.see(lambda n: has(n, "Collapse") and has(n, held_one) and has(n, held_two),
                            "Enter did not expand the group")
        self.qmp.keys("esc")
        collapsed = self.see(lambda n: not has(n, "Collapse") and has(n, "Previous month"),
                             "Escape did not collapse the group inside the open date menu")
        if has(collapsed["nodes"], held_one):
            problems.append("the older card still shows after collapsing")
        self.save("dnd-list.json", {"collapsed": listed["nodes"], "expanded": expanded["nodes"]})
        self.focus_node(lambda n: n.get("name") == "Clear", [(("shift", "tab"), 12), (("tab",), 80)],
                        "the keyboard never reached Clear")
        self.qmp.keys("ret")
        self.see(lambda n: has(n, "No Notifications"), "Clear did not empty the list")
        self.qmp.keys("esc")
        self.see(lambda n: not has(n, "Previous month"), "Escape did not close the date menu")
        # Critical notifications are not held.
        self.send(critical, "critical")
        self.see(lambda n: has(n, critical), "Do Not Disturb held a critical banner", "Notifications")
        self.qmp.keys("meta_l", "n")
        self.see(lambda n: focused_node(n, lambda x: x.get("name") == critical),
                 "Super+N did not focus the critical banner", "Notifications")
        self.qmp.keys("delete")
        self.see(lambda n: not has(n, critical), "Delete did not close the critical banner", "Notifications")
        # And off again: banners come back.
        self.toggle_dnd(False)
        self.send(after)
        self.see(lambda n: has(n, after), "no banner after turning Do Not Disturb off", "Notifications")
        self.qmp.keys("meta_l", "n")
        self.see(lambda n: focused_node(n, lambda x: x.get("name") == after), "Super+N did not focus the banner",
                 "Notifications")
        self.qmp.keys("delete")
        self.see(lambda n: not has(n, after), "Delete did not close the banner", "Notifications")
        if problems:
            raise GateError("; ".join(problems))
        return ("Do Not Disturb from quick settings (tile on, show-banners false) holds two notify-send banners back "
                "but lists them in the date menu as one collapsed per-app group that expands, collapses and clears; "
                "a critical one still banners; off restores banners (shell-gates/dnd-list.json)")

    # ---- V-TILING-KEYS ----------------------------------------------------
    def tiling(self):
        value = self.focus_main()
        fixture, window = self.main(value)
        original, ident, nav = window["rect"], window["id"], fixture["nav_keys"]
        area = work_area(value["scene"])
        left, right = half_rects(area)
        steps = (("Super+Up", ("meta_l", "up"), area), ("Super+Left", ("meta_l", "left"), left),
                 ("Super+Left again", ("meta_l", "left"), original), ("Super+Right", ("meta_l", "right"), right),
                 ("Super+Down", ("meta_l", "down"), original))
        reached = []
        try:
            for label, keys, expect in steps:
                self.qmp.keys(*keys)
                state = self.wait(lambda v: self.main(v)[1]["rect"] == expect
                                  and v["fixtures"]["main"]["size"] == expect[2:], 10,
                                  f"{label} did not put the window at {expect}")
                reached.append({"keys": label, "rect": expect, "maximized": state["fixtures"]["main"]["maximized"],
                                "tiled": state["fixtures"]["main"]["tiled"]})
        finally:
            self.save("tiling.json", reached)
            self.restore_floating(ident, original)
        problems = []
        if reached[0]["maximized"] is not True:
            problems.append("GTK never reported the window maximized after Super+Up")
        if any(step["maximized"] for step in reached[1:]):
            problems.append("GTK still reports the window maximized after tiling or restoring")
        leaked = self.state()["fixtures"]["main"]["nav_keys"] - nav
        if leaked:
            problems.append(f"{leaked} arrow press(es) reached the client")
        if problems:
            raise GateError("; ".join(problems))
        return (f"Super+Up fills the work area {area}, Super+Left tiles {left}, again returns {original}, "
                f"Super+Right tiles {right}, Super+Down restores; no arrow reaches the client (shell-gates/tiling.json)")

    def restore_floating(self, ident, original):
        """Best effort after a failed tiling step: the next gates need the
        window floating where it was."""
        for _ in range(2):
            current = window_of(self.state()["scene"], ident)
            if current is None or current["rect"] == original:
                return
            self.qmp.keys("meta_l", "down")
            time.sleep(1.0)

    # ---- V-WORKSPACE-KEYS -------------------------------------------------
    def workspaces(self):
        value = self.focus_main()
        fixture, window = self.main(value)
        ident, nav = window["id"], fixture["nav_keys"]
        home = value["scene"]["active_workspace"]
        if window["workspace"] != home:
            raise GateError(f"the focused window is on workspace {window['workspace']}, not the active {home}")
        original = list(value["scene"]["workspaces"] or [])
        problems, log = [], []

        def switch(label, keys, target, with_window, focused, popup=False):
            """One chord: the active workspace (and with Shift the window) reach
            `target`, focus lands on `focused` (None on an empty workspace),
            and the compositor records one finished slide."""
            before_state = self.state()["scene"]
            history = (before_state.get("workspace_slide") or {}).get("history") or []
            source = before_state["active_workspace"]

            def act():
                self.qmp.keys(*keys)
            watch = self.armed("gates-watch-layer", ["tuna-workspace-popup"], act) if popup else act()
            state = self.wait(lambda v: v["scene"]["active_workspace"] == target
                              and (window_of(v["scene"], ident) or {}).get("workspace") == (target if with_window else home)
                              and v["scene"]["focused"] == focused
                              and slide_record(history, v["scene"]) is not None, 10,
                              f"{label} did not leave workspace {target} active with focus on {focused} "
                              "after one recorded slide")
            record = slide_record(history, state["scene"])
            problems.extend(f"{label}: {p}" for p in slide_problems(record, source, target, state["scene"]))
            if popup:
                problems.extend(f"{label}: {p}" for p in popup_problems(watch))
            log.append({"keys": label, "record": record, "popup": watch})
            return state

        switch("Super+PageDown", ("meta_l", "pgdn"), home + 1, False, None, popup=True)
        switch("Super+PageUp", ("meta_l", "pgup"), home, False, ident)
        switch("Super+Shift+PageDown", ("meta_l", "shift", "pgdn"), home + 1, True, ident)
        switch("Super+Shift+PageUp", ("meta_l", "shift", "pgup"), home, True, ident)
        settled = self.wait(lambda v: list(v["scene"]["workspaces"] or []) == original, 10,
                            f"the workspaces did not settle back to {original}")
        self.save("workspaces.json", log)
        leaked = settled["fixtures"]["main"]["nav_keys"] - nav
        if leaked:
            problems.append(f"{leaked} Page key press(es) reached the client")
        if problems:
            raise GateError("; ".join(problems))
        return (f"Super+PageDown/PageUp slide workspace {home} <-> {home + 1} over "
                f"{slowdown_ms(settled['scene'], SLIDE_MS)} ms (250 ms times the slow-down factor) with the "
                "switcher popup, the empty workspace unfocused; Shift moves the focused window and follows it, then "
                f"back; workspaces end as {original}; no Page key reaches the client (shell-gates/workspaces.json)")

    # ---- V-WINDOW-MOVE-RESIZE ---------------------------------------------
    def move_resize(self):
        value = self.focus_main()
        fixture, window = self.main(value)
        rect, area = window["rect"], work_area(value["scene"])
        problems = shadow_problems(fixture["surface"], rect)
        if problems:
            raise GateError("; ".join(problems))
        done = []

        def settle(predicate, message):
            return self.wait(lambda v: predicate(self.main(v)[1]["rect"], v), 10, message)

        # Header-bar drag: GTK sends xdg_toplevel.move after its drag threshold.
        start = (rect[0] + 30, rect[1] + 22)
        delta = (100 if rect[0] + rect[2] + 160 < WIDTH else -100, 60 if rect[1] + rect[3] + 100 < HEIGHT else -40)
        end = (start[0] + delta[0], start[1] + delta[1])
        self.drag(start, end)
        value = settle(lambda r, v: near(v["scene"]["pointer_position"], end) and r != rect,
                       "dragging the header bar did not move the window")
        after = self.main(value)[1]["rect"]
        problems += move_problems(rect, after, delta, rect[2:])
        done.append({"drag": "header", "from": rect, "to": after, "delta": delta})
        # Resizes from the invisible border, 4 px outside the geometry.
        for label, grab, grow, shift in (
                ("right edge", lambda r: (r[0] + r[2] + 4, r[1] + r[3] // 2), (60, 0), (0, 0)),
                ("bottom-right corner", lambda r: (r[0] + r[2] + 4, r[1] + r[3] + 4), (40, 40), (0, 0)),
                ("left edge", lambda r: (r[0] - 4, r[1] + r[3] // 2), (50, 0), (-50, 0))):
            self.focus_main()  # a missed border press lands on the window below and raises it
            before = self.main(self.state())[1]["rect"]
            origin = grab(before)
            target = (origin[0] + (grow[0] if shift[0] == 0 else shift[0]), origin[1] + grow[1])
            self.drag(origin, target)
            try:
                value = settle(lambda r, v: r != before and near(v["scene"]["pointer_position"], target),
                               f"dragging the invisible border at the {label} did not resize the window")
            except GateError as error:
                problems.append(str(error))
                done.append({"drag": label, "from": before, "failed": str(error)})
                continue
            now = self.main(value)[1]["rect"]
            gtk = self.main(value)[0]["size"]
            found = resize_problems(before, now, grow, shift)
            if gtk != now[2:]:
                found.append(f"GTK's size {gtk} is not the compositor's {now[2:]}")
            problems += [f"{label}: {p}" for p in found]
            done.append({"drag": label, "from": before, "to": now})
        # Super+drag moves without the client.
        value = self.focus_main()
        before = self.main(value)[1]["rect"]
        presses = value["fixtures"]["main"]["presses"]
        centre = (before[0] + before[2] // 2, before[1] + before[3] // 2 + 30)
        target = (centre[0] - 40, centre[1] + 30)
        self.drag(centre, target, keys=("meta_l",))
        value = settle(lambda r, v: r != before and near(v["scene"]["pointer_position"], target),
                       "Super+drag did not move the window")
        now = self.main(value)[1]["rect"]
        if abs(now[0] - (before[0] - 40)) > 2 or abs(now[1] - (before[1] + 30)) > 2:
            problems.append(f"Super+drag moved the window {[now[0] - before[0], now[1] - before[1]]}, not [-40, 30]")
        if value["fixtures"]["main"]["presses"] != presses:
            problems.append("the Super+drag press reached the client")
        if value["scene"]["overview_open"]:
            problems.append("Super held across a drag opened the overview")
        done.append({"drag": "Super+left", "from": before, "to": now})
        # Screen edges: GNOME's tile preview over the snap area, tile or maximize on drop.
        left, _ = half_rects(area)
        for label, edge, preview in (("left edge", lambda s: (1, s[1] + 40), left),
                                     ("top edge", lambda s: (s[0], TOP_EDGE_Y), area)):
            self.focus_main()
            before = self.main(self.state())[1]["rect"]
            origin = (before[0] + 30, before[1] + 22)
            seen = {}

            def hold_at_edge(preview=preview, seen=seen):
                try:
                    self.wait(lambda v: (v["scene"].get("tile_preview") or {}).get("rect") == preview, 5,
                              f"no tile preview {preview}")
                except GateError as error:
                    seen["error"] = str(error)
            self.drag(origin, edge(origin), during=hold_at_edge)
            if "error" in seen:
                problems.append(f"{label}: {seen['error']}")
            try:
                self.wait(lambda v: self.main(v)[1]["rect"] == preview, 10,
                          f"dropping at the {label} did not put the window at {preview}")
            except GateError as error:
                problems.append(f"{label}: {error}")
                done.append({"drag": label, "preview": preview, "landed": False})
                continue
            self.qmp.keys("meta_l", "down")
            try:
                self.wait(lambda v: self.main(v)[1]["rect"] != preview and v["scene"].get("tile_preview") is None, 10,
                          "Super+Down did not restore the window after the edge drop")
            except GateError as error:
                problems.append(f"{label}: {error}")
            done.append({"drag": label, "preview": preview, "landed": True})
        # Back off the edge: the preview goes and the drop tiles nothing.
        self.focus_main()
        before = self.main(self.state())[1]["rect"]
        origin = (before[0] + 30, before[1] + 22)
        gone = {}

        def leave_edge():
            self.move(*origin)
            time.sleep(0.5)
            gone["preview"] = self.state()["scene"].get("tile_preview")
        self.drag(origin, (1, origin[1] + 40), during=leave_edge)
        if gone.get("preview") is not None:
            problems.append("the tile preview stayed after leaving the edge")
        end_rect = self.main(self.state())[1]["rect"]
        if end_rect[2:] != before[2:]:
            problems.append(f"the window tiled after leaving the edge: {end_rect}")
        done.append({"drag": "left edge and back", "rect": end_rect})
        self.save("move-resize.json", done)
        if problems:
            raise GateError("; ".join(problems))
        return ("real QMP pointer drags: the header bar moves the window by the drag, its invisible shadow border "
                "resizes it from the right edge, bottom-right corner and left edge, Super+drag moves it without the "
                "client, the left and top screen edges show the tile preview and tile or maximize on drop, and leaving "
                "the edge cancels it (shell-gates/move-resize.json)")

    # ---- V-OVERVIEW-FOCUS -------------------------------------------------
    def overview_focus(self):
        problems = []
        for label, role, focus in (("main", "main", self.focus_main), ("bystander", "bystander", self.focus_bystander)):
            value = focus()
            ident = value["scene"]["focused"]
            window = window_of(value["scene"], ident)
            if window is None or window["app_id"] != value["fixtures"][role]["app_id"]:
                raise GateError(f"the {label} fixture does not have focus before the overview opens")
            rect, keys = window["rect"], value["fixtures"][role]["keys"]
            opened = self.open_overview()
            self.save(f"overview-focus-{label}.json", opened["scene"])
            self.close_overview()
            back = self.wait(lambda v: v["scene"]["focused"] == ident, 10,
                             f"Escape did not return focus to the {label} window {ident}")
            after = window_of(back["scene"], ident)
            if after is None or after["rect"] != rect:
                problems.append(f"the {label} window is at {after and after['rect']} after Escape, not {rect}")
            if (back["scene"]["stacking"] or [ident])[-1] != ident:
                problems.append(f"the {label} window is not on top after Escape")
            self.qmp.keys("x")
            self.wait(lambda v: v["fixtures"][role]["keys"] > keys, 10,
                      f"a key typed after Escape never reached the {label} window")
        if problems:
            raise GateError("; ".join(problems))
        return ("Escape out of the overview restores keyboard focus to the window that had it, first the main "
                "fixture then the maximized one, each at its rectangle and on top, and a key typed afterwards "
                "reaches it (shell-gates/overview-focus-*.json)")

    def focus_bystander(self):
        value = self.state()
        _, main = self.main(value)
        fixture, bystander = self.bystander(value)
        ident = bystander["id"]
        if value["scene"]["focused"] == ident:
            return value
        self.qmp.click(*exposed_point(bystander["rect"], [main["rect"]]))
        return self.wait(lambda v: v["scene"]["focused"] == ident, 10, "a click on the other client did not focus it")

    # ---- V-OVERVIEW-PICKER ------------------------------------------------
    def picker(self):
        value = self.focus_main()
        fixture, window = self.main(value)
        ident, home = window["id"], value["scene"]["active_workspace"]
        # A window one workspace over makes the strip of three (the trailing one empty).
        self.qmp.keys("meta_l", "shift", "pgdn")
        self.wait(lambda v: (window_of(v["scene"], ident) or {}).get("workspace") == home + 1
                  and v["scene"]["active_workspace"] == home + 1, 10, "Super+Shift+PageDown did not move the window")
        self.qmp.keys("meta_l", "pgup")
        self.wait(lambda v: v["scene"]["active_workspace"] == home, 10, "Super+PageUp did not return home")
        problems = []
        self.open_overview()
        time.sleep(1.0)  # the strip and cards finish their entrance
        scene = self.state()["scene"]
        occupied = {w["workspace"] for w in scene["windows"] or []}
        problems += picker_problems(scene, home) + strip_problems(scene, occupied)
        image = self.frame("overview-strip")
        ring = next((t for t in scene["thumbnails"] if t.get("active")), None)
        if ring is not None:
            at = (ring["rect"][0] - 2, ring["rect"][1] + ring["rect"][3] // 2)
            pixel = pixel_at(image, *at)
            if not vivid(pixel):
                problems.append(f"no accent ring at {at} around the active thumbnail (pixel {pixel})")
        self.save("overview-picker.json", {k: scene.get(k) for k in ("previews", "thumbnails", "workspace_cards",
                                                                       "windows", "active_workspace")})

        def thumbnail(rows, workspace):
            found = [t["rect"] for t in rows or [] if t["workspace"] == workspace]
            if len(found) != 1:
                raise GateError(f"no thumbnail of workspace {workspace} to press")
            return found[0]
        try:
            # A click on the next workspace's thumbnail switches there, the overview staying open.
            target = thumbnail(scene["thumbnails"], home + 1)
            self.qmp.click(target[0] + target[2] // 2, target[1] + target[3] // 2)
            there = self.wait(lambda v: v["scene"]["overview_open"] and v["scene"]["active_workspace"] == home + 1
                              and any(p["id"] == ident for p in v["scene"]["previews"] or []), 10,
                              "a thumbnail click did not switch workspace with the overview open")
            # Drag the window's preview onto the first workspace's thumbnail: the window moves there.
            scene = there["scene"]
            preview = next(p for p in scene["previews"] if p["id"] == ident)["rect"]
            home_thumb = thumbnail(scene["thumbnails"], home)
            start = (preview[0] + preview[2] // 2, preview[1] + preview[3] // 2)
            drop = (home_thumb[0] + home_thumb[2] // 2, home_thumb[1] + home_thumb[3] // 2)
            self.drag(start, drop, during=lambda: self.frame("overview-drag"))
            moved = self.wait(lambda v: v["scene"]["overview_open"]
                              and (window_of(v["scene"], ident) or {}).get("workspace") == home, 10,
                              "dropping the preview on a workspace thumbnail did not move the window there")
        except GateError as error:
            self.put_back(ident, home)
            raise GateError("; ".join(problems + [str(error)])) from None
        self.save("overview-drop.json", moved["scene"])
        self.close_overview()
        if self.state()["scene"]["active_workspace"] != home:
            self.qmp.keys("meta_l", "pgup")
        self.wait(lambda v: v["scene"]["active_workspace"] == home, 10, "the session did not return to its workspace")
        if problems:
            raise GateError("; ".join(problems))
        return ("the overview's window picker previews every shown window scaled down; with a window one workspace "
                f"over the strip is three 43x26 thumbnails 49 px apart at y {STRIP_Y} with an accent ring on the "
                f"active one and the card shrunk to 849x509; a click on a thumbnail switches with the overview open; "
                "a window preview dragged onto a thumbnail moves its window (shell-gates/overview-*.json)")

    def put_back(self, ident, home):
        """Best effort after a failed step: the window and the session back on
        workspace `home` for the next gates."""
        if home is None:
            return
        try:
            self.idle_desktop()
            for _ in range(4):
                scene = self.state()["scene"]
                window = window_of(scene, ident)
                if window is None or (window["workspace"] == home and scene["active_workspace"] == home):
                    return
                away = window["workspace"] != home and scene["active_workspace"] == window["workspace"]
                direction = "pgup" if scene["active_workspace"] > home else "pgdn"
                if away and scene["focused"] == ident:
                    self.qmp.keys("meta_l", "shift", direction)
                else:
                    self.qmp.keys("meta_l", direction)
                time.sleep(1.0)
        except Exception:  # noqa: BLE001 - the gate's own failure is the record
            return

    # ---- V-APP-GRID -------------------------------------------------------
    def app_grid(self):
        self.idle_desktop()
        made = self.guest("gates-apps", "start", self.key)
        name = made["launch_name"]
        self.open_overview()
        dash = self.see(lambda n: has(n, "Show Apps", "Dash"), "the dash never showed Show Apps", "Dash")
        self.qmp.click(*node_point(dash["nodes"], "Show Apps", dash["scene"], "Dash"))
        self.wait(lambda v: v["scene"]["overview_app_grid"] and v["scene"].get("overview_state") == 2
                  and any(layer["namespace"] == "tuna-shell-appgrid" for layer in v["scene"]["layers"] or []), 15,
                  "Show Apps did not open the app grid")
        grid = self.see(lambda n: has(n, "Next Page", "App Grid") and has(n, "Page 2", "App Grid"),
                        "the app grid never paged: no Next Page and Page 2 with thirty more apps", "App Grid", timeout=40)
        self.frame("app-grid")
        problems = []
        pages = len({n["name"] for n in grid["nodes"] if re.fullmatch(r"Page \d+", n["name"])})
        if has(grid["nodes"], "Previous Page", "App Grid"):
            problems.append("Previous Page shows on the first page")
        if pages < 2:
            problems.append(f"{pages} page indicator(s), not at least two")
        self.qmp.click(*node_point(grid["nodes"], "Next Page", grid["scene"], "App Grid"))
        time.sleep(0.8)
        turned = self.see(lambda n: has(n, "Previous Page", "App Grid"), "Next Page did not turn the grid", "App Grid")
        found = None
        for _ in range(2 * pages):
            current = self.dump("App Grid")
            # Tiles of other pages may still read as showing; only an on-screen tile counts.
            found = on_screen(current["nodes"], name, current["scene"], "App Grid")
            if found is not None:
                break
            direction = "Next Page" if has(current["nodes"], "Next Page", "App Grid") else "Previous Page"
            self.qmp.click(*node_point(current["nodes"], direction, current["scene"], "App Grid"))
            time.sleep(0.8)
        if found is None:
            raise GateError(f"the launch fixture {name!r} is on no page of the grid")
        self.save("app-grid.json", {"pages": pages, "grid": grid["nodes"], "turned": turned["nodes"]})
        self.qmp.click(*found)
        launched = self.wait(lambda v: not v["scene"]["overview_open"] and v["fixtures"].get("launched")
                             and fixture_window(v["scene"], v["fixtures"]["launched"]) is not None, 30,
                             "the grid tile did not launch its app and close the overview")
        window = fixture_window(launched["scene"], launched["fixtures"]["launched"])
        if launched["scene"]["focused"] != window["id"]:
            problems.append("the launched app's window is not focused")
        if problems:
            raise GateError("; ".join(problems))
        return (f"Show Apps opens the app grid; {pages} pages with thirty more apps, Next Page turns it (Previous Page "
                "appears), and a click on a tile launches its app, a mapped focused window, and closes the overview "
                "(shell-gates/app-grid.json)")


def run(qmp, agent, out, record, read_ppm):
    """All nine gates; each records pass or fail on its own. Raises only when
    fixture cleanup is unconfirmed, which would leave the lifecycle's own
    window baseline changed."""
    gates = ShellGates(qmp, agent, out, record, read_ppm)
    setup_error = None
    banners = None
    try:
        banners = gates.banners_state()
        gates.guest("gates-banners", "set", "true")
        gates.guest("gates-start", "bystander", gates.key)
        gates.guest("gates-start", "main", gates.key)
        gates.home = gates.state()["scene"]["active_workspace"]
        gates.gate("V-A11Y-TREE", gates.a11y_tree)
        gates.gate("V-NOTIFY", gates.notify)
        gates.gate("V-NOTIFY-DND", gates.dnd)
        gates.gate("V-TILING-KEYS", gates.tiling)
        gates.gate("V-WORKSPACE-KEYS", gates.workspaces)
        gates.gate("V-WINDOW-MOVE-RESIZE", gates.move_resize)
        gates.gate("V-OVERVIEW-FOCUS", gates.overview_focus)
        gates.gate("V-OVERVIEW-PICKER", gates.picker)
        gates.gate("V-APP-GRID", gates.app_grid)
    except (GateError, RuntimeError, ValueError, KeyError, TypeError, OSError) as error:
        setup_error = f"{type(error).__name__}: {error}"
    finally:
        cleanup_error = None
        try:
            if banners is not None:
                gates.guest("gates-banners", "set", banners if banners in ("true", "false") else "true")
            stopped = gates.guest("gates-stop", gates.key, timeout=60)
            if stopped.get("confirmed") is not True:
                raise GateError("fixture cleanup unconfirmed")
        except (GateError, RuntimeError, ValueError, KeyError, TypeError, OSError) as error:
            cleanup_error = f"{type(error).__name__}: {error}"
        gates.save("steps.json", gates.log)
    for name in GATES:
        if name not in gates.recorded:
            record(name, False, f"not reached: {setup_error or cleanup_error}"[:900])
    if cleanup_error:
        raise RuntimeError(f"shell gate fixtures not retired: {cleanup_error}")
