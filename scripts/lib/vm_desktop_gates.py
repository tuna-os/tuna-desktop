"""Installed desktop gates on the marlin-vm lifecycle boot.

The installed-image proof of rows the nested session already passes:

  V-CURSOR             P-IN-03  a GTK entry's text cursor shape, cursor-size
                                48 live (redrawn scaled by 48/24 as Mutter
                                does), and locate-pointer's ripple at the
                                pointer after a lone Ctrl tap
  V-APP-POPOVER        P-WM-06  an app's menu popover (an xdg_popup) opens on
                                a click and an outside click on another
                                client dismisses it; that press is consumed
  V-WINDOW-BUTTONS     P-WM-05  GTK's own maximize, restore, minimize and
                                close title buttons; Alt+Tab restores
  V-MINIMIZE-ANIMATION P-AN-03  Super+H runs GNOME's 400 ms minimize (times
                                the session's GNOME_SHELL_SLOWDOWN_FACTOR)
                                toward the window's dash icon, frame by
                                frame, and it finishes; activation runs the
                                restore
  V-TRAY               P-TR-01  a StatusNotifierItem shows in the top bar with
                                its tooltip, a click opens its menu, and it
                                leaves when the item exits

The guest probe packaging/marlin/vm-lane/tuna-vm-desktop-gates (gates-*
actions through tuna-vm-lifecycle) launches real GTK clients and the nested
proof's StatusNotifierItem stub as the session user, and reads their state
files, the compositor's state snapshot and the shell's AT-SPI tree. Every
pointer move, click and key below is a QMP input event on the VM's own tablet
and keyboard; the guest never synthesizes input.
"""
import json
import os
import secrets
import threading
import time

from vm_strip_acceptance import exposed_point

GATES = ("V-CURSOR", "V-APP-POPOVER", "V-WINDOW-BUTTONS", "V-MINIMIZE-ANIMATION", "V-TRAY")
WIDTH, HEIGHT = 1280, 800
# GNOME Shell 51 windowManager.js MINIMIZE_WINDOW_ANIMATION_TIME.
MINIMIZE_MS = 400
TRAY_TITLE, TRAY_ROW = "Tuna Desktop Tray Probe", "Open Probe"


class GateError(RuntimeError):
    pass


# ---- observations (scripts/tests/test_vm_desktop_gates.py) -----------------

def inside(point, rect):
    return rect[0] <= point[0] < rect[0] + rect[2] and rect[1] <= point[1] < rect[1] + rect[3]


def fixture_window(scene, fixture):
    """The one compositor window carrying the fixture's nonce app ID."""
    rows = [w for w in scene.get("windows") or [] if w.get("app_id") == fixture.get("app_id")]
    return rows[0] if len(rows) == 1 else None


def widget_point(rect, bounds):
    """A widget's centre on screen: the compositor's window-geometry origin
    plus GTK's window-relative bounds. Refuses a point outside the window or
    the output."""
    if not bounds or bounds[2] <= 0 or bounds[3] <= 0:
        raise GateError("the widget has no allocated bounds")
    point = (rect[0] + bounds[0] + bounds[2] // 2, rect[1] + bounds[1] + bounds[3] // 2)
    if not inside(point, rect) or not inside(point, (0, 0, WIDTH, HEIGHT)):
        raise GateError(f"widget centre {point} is outside window {rect} or the output")
    return point


def primary_output(scene):
    outputs = [o for o in scene.get("outputs") or [] if o.get("primary")]
    if len(outputs) != 1:
        raise GateError(f"no single primary output: {scene.get('outputs')}")
    return outputs[0]


def panel_layers(scene):
    return [layer for layer in scene.get("layers") or [] if layer.get("namespace") == "tuna-panel"]


def top_bar(scene, output):
    """The shell's top bar on `output`: one tuna-panel layer across its top."""
    rect = output["rect"]
    bars = [layer["rect"] for layer in panel_layers(scene)
            if layer["rect"][:3] == rect[:3] and 0 < layer["rect"][3] < rect[3]]
    return bars[0] if len(bars) == 1 else None


def work_area(scene):
    """Mutter's work area of the primary output: below the top bar."""
    output = primary_output(scene)["rect"]
    bar = top_bar(scene, primary_output(scene))
    if bar is None:
        raise GateError("the top bar is not one full-width layer on the primary output")
    return [output[0], output[1] + bar[3], output[2], output[3] - bar[3]]


def cursor_shows(cursor, name):
    """The compositor draws the client's requested named shape (cursor-shape-v1)."""
    if not isinstance(cursor, dict):
        return False
    return cursor.get("kind") == "named" and cursor.get("name") == name and cursor.get("source") == "client"


def cursor_rescaled(before, after, name, size):
    """`cursor-size` changed to `size` redraws the shape scaled by size/old,
    as Mutter does. The drawn logical size need not equal the theme size: a
    scalable (SVG) theme's canvas may be larger than its nominal size, as
    Adwaita's text cursor is (a 32 px canvas at nominal 24), so compare the
    drawn size with the one before the change (within a pixel of rounding)."""
    if not (cursor_shows(before, name) and cursor_shows(after, name)) or after.get("size") != size:
        return False
    old, new = before.get("size"), after.get("logical")
    was = before.get("logical")
    if not (isinstance(old, int) and old > 0 and isinstance(was, list) and isinstance(new, list)
            and len(was) == 2 and len(new) == 2):
        return False
    k = size / old
    return all(abs(n - w * k) <= 1 for n, w in zip(new, was)) and new != was


def ripple_problems(watch, pointer):
    """GNOME's locate-pointer after one lone Ctrl tap: exactly one more
    ripple, drawn at the pointer over new frames, then finished."""
    problems = []
    final = ((watch.get("scene") or {}).get("cursor") or {}).get("locate_pointer") or {}
    before = watch.get("baseline_locate_count")
    if type(before) is not int or final.get("count") != before + 1:
        problems.append(f"ripple count went {before} -> {final.get('count')}, not one more")
    if final.get("active") is not False:
        problems.append("the ripple never finished")
    drawn = [s for s in watch.get("samples") or [] if s.get("active") and isinstance(s.get("at"), list)]
    if not drawn:
        problems.append("no in-flight ripple frame was observed")
    elif pointer is None or not all(abs(s["at"][i] - pointer[i]) <= 1 for s in drawn for i in (0, 1)):
        problems.append(f"ripples at {[s['at'] for s in drawn][:3]}, pointer at {pointer}")
    if len({s.get("frames") for s in drawn}) < 2:
        problems.append("the ripple drew fewer than two distinct frames")
    return problems


def minimize_ms(scene):
    """GNOME's 400 ms minimize as the session runs it: St's adjustAnimationTime
    multiplies it by GNOME_SHELL_SLOWDOWN_FACTOR, which the compositor reports
    as `animation_slowdown` (TunaOS images set 0.8)."""
    factor = scene.get("animation_slowdown")
    if not isinstance(factor, (int, float)) or factor <= 0:
        factor = 1.0
    return round(MINIMIZE_MS * factor)


def transition_problems(watch, window, effect):
    """GNOME's minimize (or its restore) for `window`, started by the lane's
    own input: history records it, at least two distinct in-flight frames were
    observed, it ran the 400 ms duration scaled by the session's slow-down
    factor, and it ended."""
    problems = []
    scene = watch.get("scene") or {}
    expected = minimize_ms(scene)
    life = scene.get("window_lifecycle") or {}
    since = watch.get("baseline_at_ms")
    rows = [h for h in life.get("history") or [] if h.get("window") == window
            and h.get("effect") == effect and (h.get("at_ms") or 0) > since]
    if not any(str(h.get("outcome", "")).startswith("started") for h in rows):
        problems.append(f"no {effect} started for the window")
    samples = watch.get("samples") or []
    if len(samples) < 2:
        problems.append(f"{len(samples)} in-flight {effect} frame(s) observed, not at least two")
    if any(s.get("duration_ms") != expected for s in samples):
        problems.append(f"{effect} durations {sorted({s.get('duration_ms') for s in samples})}, not {expected} ms")
    entry = "minimizing" if effect == "minimize" else "restoring"
    if any(run.get("window") == window for run in life.get(entry) or []):
        problems.append(f"the {effect} never finished")
    if effect == "minimize":
        completed = [h for h in rows if h.get("outcome") == "completed"]
        if not completed or any(h.get("duration_ms") != expected for h in completed):
            problems.append(f"no {expected} ms minimize completed: {rows}")
        if window not in (scene.get("minimized") or []):
            problems.append("the window is not minimized afterwards")
        alphas = [s.get("alpha") for s in samples]
        if alphas and not any(isinstance(a, (int, float)) and 0 < a < 1 for a in alphas):
            problems.append(f"no partially faded frame: alphas {alphas[:6]}")
    elif window in (scene.get("minimized") or []):
        problems.append("the window is still minimized afterwards")
    return problems


def dash_target(samples, output):
    """The minimize target the shell published: the window's dash icon, a
    real rectangle in the bottom half of the output (GNOME's dash), not the
    missing-icon fallback (a zero-size point at the monitor's corner)."""
    icons = {tuple(s.get("icon") or ()) for s in samples}
    if len(icons) != 1:
        return None
    icon = list(icons.pop())
    if len(icon) != 4 or icon[2] <= 0 or icon[3] <= 0:
        return None
    centre = (icon[0] + icon[2] / 2, icon[1] + icon[3] / 2)
    rect = output["rect"]
    if not inside(centre, rect) or centre[1] < rect[1] + rect[3] / 2:
        return None
    return icon


def changed_pixels(left, right, rect):
    """Pixels of `rect` that differ by more than 24 in any channel."""
    if left[:2] != right[:2]:
        raise GateError("frame dimensions changed")
    width, height = left[:2]
    x, y, w, h = rect
    changed = 0
    for row in range(max(0, y), min(height, y + h)):
        for column in range(max(0, x), min(width, x + w)):
            offset = (row * width + column) * 3
            changed += max(abs(a - b) for a, b in zip(left[2][offset:offset + 3], right[2][offset:offset + 3])) > 24
    return changed


def clip(rect):
    x0, y0 = max(0, rect[0]), max(0, rect[1])
    x1, y1 = min(WIDTH, rect[0] + rect[2]), min(HEIGHT, rect[1] + rect[3])
    return [x0, y0, max(0, x1 - x0), max(0, y1 - y0)]


def tray_nodes(observed, name):
    return [n for n in observed.get("nodes") or [] if n.get("name") == name]


def contains(outer, inner):
    ox, oy, ow, oh = outer
    ix, iy, iw, ih = inner
    return ox <= ix and oy <= iy and ix + iw <= ox + ow and iy + ih <= oy + oh


def tray_point(observed, scene):
    """The item's centre on screen: the top bar layer's origin plus the
    button's window-relative AT-SPI extents (Wayland exposes no global ones).
    GTK's MenuButton exposes the item twice under one name (the outer
    `button` and its inner `toggle button`); those nest and count as one
    item, clicked at the innermost. Two items that don't nest are refused."""
    items = [n for n in tray_nodes(observed, TRAY_TITLE)
             if n.get("showing") and len(n.get("extents") or ()) == 4]
    bar = top_bar(scene, primary_output(scene))
    if not items or bar is None:
        return None
    inner = min(items, key=lambda n: n["extents"][2] * n["extents"][3])
    if not all(contains(n["extents"], inner["extents"]) for n in items):
        return None
    x, y, w, h = inner["extents"]
    point = (bar[0] + x + w // 2, bar[1] + y + h // 2)
    return point if w > 0 and h > 0 and inside(point, bar) else None


# ---- the gates on a live guest ---------------------------------------------

class Gates:
    def __init__(self, qmp, agent, out, record, read_ppm, name="desktop-gates"):
        self.qmp, self.agent, self.record, self.read_ppm = qmp, agent, record, read_ppm
        self.evidence = os.path.join(out, name)
        os.makedirs(self.evidence, exist_ok=True)
        self.key = secrets.token_hex(16)
        self.log = []
        self.recorded = set()

    def guest(self, action, *args, timeout=40):
        value = self.agent.run(action, *args, timeout=timeout)
        self.log.append({"action": action, "args": [str(a) for a in args], "result": value, "t": time.time()})
        if isinstance(value, dict) and "gates_error" in value:
            raise GateError(f"{action}: {value['gates_error']}")
        return value

    def state(self):
        return self.guest("gates-state", self.key)

    def wait(self, predicate, timeout, message):
        deadline = time.monotonic() + timeout
        last = None
        while time.monotonic() < deadline:
            last = self.state()
            if predicate(last):
                return last
            time.sleep(0.15)
        self.save(f"timeout-{len(self.log):03d}.json", {"message": message, "last": last})
        raise GateError(message)

    def watched(self, effect, target, act, seconds=6):
        """Arm the guest's state-file watcher, then send `act`'s real input."""
        box = {}

        def watch():
            try:
                box["value"] = self.guest("gates-watch", self.key, effect, target, seconds, timeout=seconds + 20)
            except Exception as error:  # re-raised on the lane's thread below
                box["error"] = error
        thread = threading.Thread(target=watch, daemon=True)
        thread.start()
        time.sleep(1.0)
        act()
        thread.join(seconds + 25)
        if thread.is_alive() or "error" in box:
            raise GateError(f"{effect} watch failed: {box.get('error', 'still running')}")
        return box["value"]

    def save(self, name, value):
        with open(os.path.join(self.evidence, name), "w") as stream:
            json.dump(value, stream, indent=2)

    def frame(self, label):
        self.qmp.frame(os.path.join(self.evidence, f"{label}.png"))
        ppm = os.path.join(self.evidence, f"{label}.ppm")
        self.qmp.cmd("screendump", filename=ppm, format="ppm")
        image = self.read_ppm(ppm)
        os.remove(ppm)
        return image

    def move(self, x, y):
        self.qmp.cmd("input-send-event", events=[
            {"type": "abs", "data": {"axis": "x", "value": int(x * 32767 / WIDTH)}},
            {"type": "abs", "data": {"axis": "y", "value": int(y * 32767 / HEIGHT)}}])
        time.sleep(0.15)

    def gate(self, name, body, *args):
        try:
            detail = body(*args)
        except (GateError, RuntimeError, ValueError, KeyError, TypeError, OSError) as error:
            self.record(name, False, f"{type(error).__name__}: {error}"[:900])
            self.recorded.add(name)
            return False
        self.record(name, True, detail)
        self.recorded.add(name)
        return True

    # Window lookups on one gates-state result.
    def main(self, value):
        fixture = value["fixtures"].get("main")
        window = fixture and fixture_window(value["scene"], fixture)
        if not window:
            raise GateError("the main fixture window is gone")
        return fixture, window

    def bystander(self, value):
        fixture = value["fixtures"]["bystander"]
        window = fixture_window(value["scene"], fixture)
        if not window:
            raise GateError("the bystander window is gone")
        return fixture, window

    def focus_main(self):
        """Bring the main fixture back to the front. A click cannot: once the
        maximized bystander was clicked it covers the main window, which is
        then the most recent other app, so Alt+Tab activates and raises it."""
        value = self.state()
        _, window = self.main(value)
        ident = window["id"]
        if value["scene"]["focused"] == ident:
            return value
        self.qmp.keys("alt", "tab")
        return self.wait(lambda v: v["scene"]["focused"] == ident
                         and (v["scene"]["stacking"] or [ident])[-1] == ident, 10,
                         "Alt+Tab did not bring the main fixture back to the front")

    # ---- V-CURSOR ------------------------------------------------------
    def cursor(self, original):
        value = self.state()
        fixture, window = self.main(value)
        _, bystander = self.bystander(value)
        entry = widget_point(window["rect"], fixture["bounds"]["entry"])
        self.move(*entry)
        self.wait(lambda v: cursor_shows(v["scene"]["cursor"], "text"), 10,
                  "hovering the GTK entry never drew the client's text cursor shape")
        self.move(*exposed_point(bystander["rect"], [window["rect"]]))
        self.wait(lambda v: cursor_shows(v["scene"]["cursor"], "default"), 10,
                  "leaving the entry for another client never drew its default shape")
        self.move(*entry)
        before = self.wait(lambda v: cursor_shows(v["scene"]["cursor"], "text"), 10,
                           "returning to the entry never drew the text shape again")["scene"]["cursor"]
        self.guest("gates-settings", "set", "48", "false")
        sized = self.wait(lambda v: cursor_rescaled(before, v["scene"]["cursor"], "text", 48), 10,
                          f"cursor-size 48 did not redraw the text shape scaled from {before.get('size')} px")
        self.save("cursor-48.json", {"before": before, "after": sized["scene"]["cursor"]})
        self.guest("gates-settings", "set", original["cursor-size"], "true")
        located = self.wait(lambda v: ((v["scene"]["cursor"] or {}).get("locate_pointer") or {}).get("enabled") is True
                            and not v["scene"]["cursor"]["locate_pointer"].get("active"), 10,
                            "locate-pointer true never reached the compositor")
        ripple = self.watched("locate", 0, lambda: self.qmp.keys("ctrl"))
        self.save("cursor-locate.json", ripple)
        problems = ripple_problems(ripple, located["scene"]["pointer_position"])
        if problems:
            raise GateError("; ".join(problems))
        theme = sized["scene"]["cursor"].get("image_theme")
        return (f"GTK entry's cursor-shape text drawn from theme {theme}, the other client's default; "
                f"cursor-size 48 redraws it scaled by 48/{before.get('size')} live; a lone Ctrl tap ripples once at the pointer "
                f"over {len(ripple['samples'])} observed frames and finishes (desktop-gates/cursor-*.json)")

    # ---- V-APP-POPOVER --------------------------------------------------
    def popover(self):
        value = self.focus_main()
        fixture, window = self.main(value)
        _, bystander = self.bystander(value)
        mx, my = widget_point(window["rect"], fixture["bounds"]["menu"])
        # Below the menu button, where GTK places the popover with its
        # painted swatch: the label sits at the window's top left and the
        # entry at its bottom, both outside this region.
        region = clip([mx - 130, my + 20, 260, 120])
        before = self.frame("popover-closed")
        shows = fixture["popover_shows"]
        self.qmp.click(mx, my)
        self.wait(lambda v: v["fixtures"]["main"]["popover_visible"]
                  and v["fixtures"]["main"]["popover_shows"] == shows + 1, 10,
                  "clicking the app's menu button did not show its popover")
        time.sleep(0.5)
        drawn = changed_pixels(before, self.frame("popover-open"), region)
        guard = [mx - 260, my - 10, 520, 420]
        outside = exposed_point(bystander["rect"], [window["rect"], guard])
        presses = self.state()["fixtures"]["bystander"]["presses"]
        self.qmp.click(*outside)
        closed = self.wait(lambda v: not v["fixtures"]["main"]["popover_visible"], 10,
                           "a click outside the popover, on another client, did not dismiss it")
        time.sleep(0.5)
        settled = self.state()
        residue = changed_pixels(before, self.frame("popover-dismissed"), region)
        _, after_window = self.main(settled)
        consumed = settled["fixtures"]["bystander"]["presses"] == presses
        # Control: the same point does reach that client once nothing is open.
        self.qmp.click(*outside)
        reached = self.wait(lambda v: v["fixtures"]["bystander"]["presses"] == presses + 1, 10,
                            "a second click on the other client never reached it")
        self.save("popover.json", {"menu": [mx, my], "region": region, "outside": outside, "drawn": drawn,
                                   "residue": residue, "closed": closed, "settled": settled,
                                   "reached": reached["fixtures"]["bystander"]})
        area = region[2] * region[3]
        problems = []
        if drawn < area // 10:
            problems.append(f"the open popover changed only {drawn}/{area} pixels below its button")
        if residue > area // 100:
            problems.append(f"{residue}/{area} pixels still differ after dismissal")
        if not consumed:
            problems.append("the dismissing press reached the other client")
        if after_window["rect"] != window["rect"] or after_window["id"] != window["id"]:
            problems.append("the app window changed while its popover was dismissed")
        if problems:
            raise GateError("; ".join(problems))
        return (f"the app's menu popover (xdg_popup) opens on a click ({drawn}/{area} pixels drawn below the "
                "button) and a click on another client dismisses it without reaching that client; the next "
                "click there does (desktop-gates/popover.json, popover-*.png)")

    # ---- V-WINDOW-BUTTONS (maximize, restore, minimize, Alt+Tab) ----------
    def buttons(self):
        value = self.focus_main()
        fixture, window = self.main(value)
        original, ident = window["rect"], window["id"]
        if fixture["size"] != original[2:]:
            raise GateError(f"GTK's window size {fixture['size']} is not the compositor's window geometry {original}")
        area = work_area(value["scene"])
        self.qmp.click(*widget_point(original, fixture["bounds"]["maximize"]))
        maxed = self.wait(lambda v: v["fixtures"]["main"]["maximized"] and v["fixtures"]["main"]["size"] == area[2:]
                          and self.main(v)[1]["rect"] == area, 10,
                          f"the maximize button did not fill the work area {area}")
        fixture, window = self.main(maxed)
        self.qmp.click(*widget_point(window["rect"], fixture["bounds"]["maximize"]))
        self.wait(lambda v: not v["fixtures"]["main"]["maximized"] and v["fixtures"]["main"]["size"] == original[2:]
                  and self.main(v)[1]["rect"] == original, 10,
                  f"the restore button did not return the window to {original}")
        restored = self.state()
        fixture, window = self.main(restored)
        self.qmp.click(*widget_point(window["rect"], fixture["bounds"]["minimize"]))
        self.wait(lambda v: ident in (v["scene"]["minimized"] or []) and v["scene"]["focused"] != ident, 10,
                  "the minimize button did not minimize the window")
        self.qmp.keys("alt", "tab")
        self.wait(lambda v: ident not in (v["scene"]["minimized"] or []) and v["scene"]["focused"] == ident
                  and self.main(v)[1]["rect"] == original, 10,
                  "Alt+Tab did not restore and focus the minimized window at its rectangle")
        return original, area

    def close(self):
        value = self.focus_main()
        fixture, window = self.main(value)
        self.qmp.click(*widget_point(window["rect"], fixture["bounds"]["close"]))
        gone = self.wait(lambda v: not v["fixtures"]["main"]["alive"]
                         and fixture_window(v["scene"], fixture) is None, 15,
                         "the close button did not close the window and end the app")
        self.save("window-buttons.json", gone)

    # ---- V-MINIMIZE-ANIMATION --------------------------------------------
    def minimize(self):
        value = self.state()
        if value["scene"].get("animations_enabled") is not True:
            raise GateError("animations are off in the installed session")
        # The overview's dash publishes each window's icon rectangle, the
        # minimize target (GNOME's Dash._updateIconGeometry).
        self.qmp.keys("meta_l")
        self.wait(lambda v: v["scene"]["overview_open"], 10, "Super did not open the overview")
        time.sleep(1.5)
        self.qmp.keys("esc")
        self.wait(lambda v: not v["scene"]["overview_open"], 10, "Escape did not close the overview")
        time.sleep(0.6)
        value = self.focus_main()
        _, window = self.main(value)
        ident, original = window["id"], window["rect"]
        gone = self.watched("minimize", ident, lambda: self.qmp.keys("meta_l", "h"))
        self.save("minimize.json", gone)
        problems = transition_problems(gone, ident, "minimize")
        icon = dash_target(gone["samples"], primary_output(value["scene"]))
        if icon is None:
            problems.append(f"the minimize target {[s.get('icon') for s in gone['samples']][:1]} is not a dash icon")
        if problems:
            raise GateError("; ".join(problems))
        back = self.watched("unminimize", ident, lambda: self.qmp.keys("alt", "tab"))
        self.save("unminimize.json", back)
        problems = transition_problems(back, ident, "unminimize")
        final = self.main(back)[1] if back["fixtures"].get("main") else None
        if final is None or final["rect"] != original:
            problems.append(f"the restored window is at {final and final['rect']}, not {original}")
        if problems:
            raise GateError("; ".join(problems))
        return (f"Super+H minimizes toward the window's dash icon {icon} over {len(gone['samples'])} observed "
                f"frames and the {MINIMIZE_MS} ms run completes; Alt+Tab restores it over "
                f"{len(back['samples'])} frames to its rectangle (desktop-gates/minimize.json, unminimize.json)")

    # ---- V-TRAY ---------------------------------------------------------
    def tray(self):
        self.guest("gates-tray-start", self.key)
        shown = self.wait_tray(lambda o: tray_point(o, o["scene"]) is not None, "never showed in the top bar")
        item = tray_nodes(shown, TRAY_TITLE)[0]
        point = tray_point(shown, shown["scene"])
        before = self.frame("tray-shown")
        self.qmp.click(*point)
        opened = self.wait_tray(lambda o: any(n["showing"] for n in tray_nodes(o, TRAY_ROW)),
                                "a click on the item did not open its menu")
        menu = self.frame("tray-menu")
        self.qmp.keys("esc")
        self.wait_tray(lambda o: not any(n["showing"] for n in tray_nodes(o, TRAY_ROW)),
                       "Escape left the item's menu open")
        self.guest("gates-tray-stop", self.key)
        gone = self.wait_tray(lambda o: not tray_nodes(o, TRAY_TITLE) and not o["stub"],
                              "stayed in the top bar after the item exited")
        self.save("tray.json", {"shown": shown, "point": point, "opened": opened, "gone": gone})
        drawn = changed_pixels(before, menu, clip([point[0] - 200, point[1] + 12, 400, 200]))
        problems = []
        if not item.get("tooltip"):
            problems.append("the item showed without its tooltip")
        if item.get("toplevel") != "Top Bar":
            problems.append(f"the item is in {item.get('toplevel')!r}, not the top bar")
        if drawn < 400:
            problems.append(f"the open menu changed only {drawn} pixels below the item")
        if problems:
            raise GateError("; ".join(problems))
        return (f"a StatusNotifierItem shows in the top bar at {list(point)} with its tooltip, a click opens "
                f"its menu ({drawn} pixels drawn), Escape closes it, and it leaves when the item exits "
                "(desktop-gates/tray.json, tray-*.png)")

    def wait_tray(self, predicate, message):
        deadline = time.monotonic() + 30
        last = None
        while time.monotonic() < deadline:
            last = self.guest("gates-tray-observe", self.key, timeout=40)
            if predicate(last):
                return last
            time.sleep(0.3)
        self.save(f"tray-timeout-{len(self.log):03d}.json", last)
        raise GateError(f"the tray item {message}")


def run(qmp, agent, out, record, read_ppm):
    """All five gates; each records pass or fail on its own. Raises only when
    fixture cleanup is unconfirmed, which would leave the lifecycle's own
    window baseline changed."""
    gates = Gates(qmp, agent, out, record, read_ppm)
    setup_error = None
    original = None
    try:
        original = gates.guest("gates-settings", "get")
        gates.guest("gates-start", "bystander", gates.key)
        gates.guest("gates-start", "main", gates.key)
        gates.gate("V-CURSOR", gates.cursor, original)
        gates.guest("gates-settings", "set", original["cursor-size"], original["locate-pointer"])
        gates.gate("V-APP-POPOVER", gates.popover)
        buttons_error = None
        try:
            original_rect, area = gates.buttons()
        except (GateError, RuntimeError, ValueError, KeyError, TypeError, OSError) as error:
            buttons_error = f"{type(error).__name__}: {error}"
        gates.gate("V-MINIMIZE-ANIMATION", gates.minimize)
        try:
            gates.close()
        except (GateError, RuntimeError, ValueError, KeyError, TypeError, OSError) as error:
            buttons_error = buttons_error or f"close: {type(error).__name__}: {error}"
        if buttons_error:
            record("V-WINDOW-BUTTONS", False, buttons_error[:900])
        else:
            record("V-WINDOW-BUTTONS", True,
                   f"GTK's own title buttons: maximize fills the work area {area}, restore returns {original_rect}, "
                   "minimize hides it and Alt+Tab restores it there, close ends the app "
                   "(desktop-gates/window-buttons.json)")
        gates.recorded.add("V-WINDOW-BUTTONS")
        gates.gate("V-TRAY", gates.tray)
    except (GateError, RuntimeError, ValueError, KeyError, TypeError, OSError) as error:
        setup_error = f"{type(error).__name__}: {error}"
    finally:
        cleanup_error = None
        try:
            if original is not None:
                gates.guest("gates-settings", "set", original["cursor-size"], original["locate-pointer"])
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
        raise RuntimeError(f"desktop gate fixtures not retired: {cleanup_error}")
