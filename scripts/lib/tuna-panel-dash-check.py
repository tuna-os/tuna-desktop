#!/usr/bin/env python3
"""Top bar layout and dash checks for tuna-gtk-shell-proof (#1198, #1199).

Usage:
  tuna-panel-dash-check.py panel-points A11Y.json STATE.json --physical-width W
  tuna-panel-dash-check.py panel A11Y.json STATE.json ACTIVITIES.png CLOCK.png
                           SYSTEM.png --physical-width W --direction ltr|rtl
  tuna-panel-dash-check.py dash A11Y.json STATE.json FRAME.png
                           --physical-width W --direction ltr|rtl
                           --expect NAME [NAME ...] --running NAME [NAME ...]

A11Y.json is a tuna-a11y-dump.py tree of the shell (bounds relative to
each layer surface), STATE.json the compositor's TUNA_COMPOSITOR_STATE
(logical layer and output rects), the PNGs captures of the nested output
in physical pixels; W is that output's physical width, so the scale is W
over the output's logical width.

panel-points: where to rest the pointer (physical "NAME X Y" lines) so
each control shows its hover fill, below the row `panel` measures.

panel: GNOME 51's top bar (panel.js: `_leftBox` Activities, `_centerBox`
the date menu, `_rightBox` quick settings; `_allocate` centres the
centre box on the panel). From the accessible tree: the three controls
in reading order and the clock's centre within one physical pixel of
the output's centre. From each control's hover fill (the drawn pill,
one capture per control): Activities at the leading edge, the clock's
pill centred to a physical pixel, the system menu at the trailing edge;
right to left mirrors both ends. GTK reports a padded button's
accessible origin at its content box, so the edges are read from pixels.

dash: GNOME 51's dash (dash.js `_redisplay`): the favourites in
`favorite-apps` order, then the running apps that are not favourites,
then Show Apps, laid out along the reading direction; the running dot
(appDisplay.js AppIcon) is drawn under exactly the running apps.

Prints measurements and exits 0, or names the failure on stderr and
exits 1.
"""
import argparse
import json
import math
import sys

# GNOME's .panel-button carries a 3px transparent border inside its
# allocation (gnome-shell _panel.scss): its pill starts 3px in.
EDGE_PX = 3
# The bar is black; a hovered control's pill is white at 17% (43).
LIT = 8
PANEL_CONTROLS = ("Activities", "Date and Time", "System")
# The running dot: 5px of #fafafb (.app-grid-running-dot) under the icon.
DOT_MIN = 235
BUTTON_ROLES = {"push button", "button", "toggle button"}


class CheckError(Exception):
    pass


def frame_nodes(nodes, title):
    """The nodes inside the first frame (a layer surface) titled TITLE."""
    out, depth = [], None
    for node in nodes:
        if depth is None:
            if node.get("role") == "frame" and node.get("name") == title:
                depth = node.get("depth", 0)
            continue
        if node.get("depth", 0) <= depth:
            break
        out.append(node)
    if depth is None:
        raise CheckError(f"no '{title}' surface in the accessible tree")
    return out


def visible(node):
    bounds = node.get("bounds")
    return bool(node.get("showing") and bounds and bounds[2] > 0 and bounds[3] > 0)


def layer_rect(state, namespace):
    for layer in state.get("layers", []):
        if layer.get("namespace") == namespace:
            return layer["rect"]
    raise CheckError(f"no '{namespace}' layer in the compositor state")


def output_rect(state):
    outputs = state.get("outputs") or []
    primary = [o for o in outputs if o.get("primary")] or outputs
    if not primary:
        raise CheckError("the compositor state lists no output")
    return primary[0]["rect"]


def control(nodes, name, origin):
    """Global logical [left, top, right, bottom] of the outermost showing
    control named NAME."""
    for node in nodes:
        if node.get("name") == name and node.get("role") in BUTTON_ROLES and visible(node):
            x, y, w, h = node["bounds"]
            return [origin[0] + x, origin[1] + y, origin[0] + x + w, origin[1] + y + h]
    raise CheckError(f"no showing control named '{name}' in the top bar")


def panel_controls(nodes, state):
    out = output_rect(state)
    panel = layer_rect(state, "tuna-panel")
    if panel[0] != out[0] or panel[1] != out[1] or panel[2] != out[2]:
        raise CheckError(f"the bar {panel} does not span the output {out}")
    bar = frame_nodes(nodes, "Top Bar")
    controls = {name: control(bar, name, panel) for name in PANEL_CONTROLS}
    for name, rect in controls.items():
        if rect[1] < panel[1] or rect[3] > panel[1] + panel[3]:
            raise CheckError(f"{name} {rect} leaves the bar {panel}")
    return out, controls


def panel_points(nodes, state, physical_width):
    out, controls = panel_controls(nodes, state)
    scale = physical_width / out[2]
    # The pointer's arrow hangs below and right of its hotspot: rest it
    # 6px under the measured middle row.
    return [(name, round((r[0] + r[2]) / 2 * scale), round(((r[1] + r[3]) / 2 + 6) * scale))
            for name, r in controls.items()]


def pill(image, rect, scale):
    """Physical [left, right) of the lit run through RECT's middle row."""
    y = round((rect[1] + rect[3]) / 2 * scale)
    x = round((rect[0] + rect[2]) / 2 * scale)

    def lit(x):
        return max(image.getpixel((x, y))[:3]) > LIT

    if not lit(x):
        return None
    left, right = x, x
    while left > 0 and lit(left - 1):
        left -= 1
    while right < image.width - 1 and lit(right + 1):
        right += 1
    return [left, right + 1]


def check_panel(nodes, state, images, physical_width, direction):
    out, controls = panel_controls(nodes, state)
    scale = physical_width / out[2]
    centres = {name: (r[0] + r[2]) / 2 for name, r in controls.items()}
    order = sorted(PANEL_CONTROLS, key=centres.get, reverse=direction == "rtl")
    if order != list(PANEL_CONTROLS):
        raise CheckError(f"the bar reads {order} {direction}, not {list(PANEL_CONTROLS)}")
    off = (centres["Date and Time"] - (out[0] + out[2] / 2)) * scale
    if abs(off) > 1:
        raise CheckError(f"the clock's accessible centre is {off:+.2f} physical px "
                         "from the output's centre")
    pills = {}
    for name, image in zip(PANEL_CONTROLS, images):
        rect = controls[name]
        drawn = pill(image, rect, scale)
        width = (rect[2] - rect[0]) * scale
        if drawn is None or drawn[1] - drawn[0] < 0.8 * width:
            raise CheckError(f"no hover fill across {name} {rect}: lit run {drawn}, "
                             f"accessible width {width:g} physical px")
        pills[name] = drawn
    left, right = round(out[0] * scale), round((out[0] + out[2]) * scale)
    edge = math.ceil(EDGE_PX * scale)
    lead, trail = ("Activities", "System") if direction == "ltr" else ("System", "Activities")
    if pills[lead][0] - left > edge:
        raise CheckError(f"{lead}'s pill starts {pills[lead][0] - left} physical px "
                         f"from the left edge (at most {edge})")
    if right - pills[trail][1] > edge:
        raise CheckError(f"{trail}'s pill ends {right - pills[trail][1]} physical px "
                         f"from the right edge (at most {edge})")
    clock = pills["Date and Time"]
    if not pills[lead][1] <= clock[0] or not clock[1] <= pills[trail][0]:
        raise CheckError(f"the clock's pill {clock} is not between {pills[lead]} and {pills[trail]}")
    drawn_off = (clock[0] + clock[1]) / 2 - (left + right) / 2
    if abs(drawn_off) > 1:
        raise CheckError(f"the clock's pill centre is {drawn_off:+.2f} physical px "
                         "from the output's centre")
    return (f"scale {scale:g} {direction}, output {right - left} physical px: pills "
            + ", ".join(f"{name} {pills[name][0]}-{pills[name][1]}" for name in PANEL_CONTROLS)
            + f"; clock centre {drawn_off:+.2f} drawn, {off:+.2f} accessible")


def dot_rows(image, rect, scale):
    """Near-white pixel count per row in the tile's bottom centre band,
    where AppIcon draws its running dot."""
    left, top, right, bottom = (round(v * scale) for v in rect)
    centre = (left + right) // 2
    reach = max(3, round(4 * scale))
    rows = []
    for y in range(max(top, bottom - round(14 * scale)), bottom):
        rows.append(sum(
            1 for x in range(centre - reach, centre + reach + 1)
            if min(image.getpixel((x, y))[:3]) >= DOT_MIN))
    return rows


def dash_items(nodes):
    """The dash's showing buttons, each with its showing descendants, and
    its separators, in tree order."""
    items, separators, current = [], [], None
    for node in frame_nodes(nodes, "Dash"):
        if current and node.get("depth", 0) <= current[0].get("depth", 0):
            current = None
        if not visible(node):
            continue
        if node.get("role") in BUTTON_ROLES:
            current = (node, [])
            items.append(current)
        elif current:
            current[1].append(node)
        elif node.get("role") == "separator":
            separators.append(node)
    return items, separators


def check_dash(nodes, state, image, physical_width, direction, expect, running,
               separator_after=None):
    out = output_rect(state)
    scale = physical_width / out[2]
    origin = layer_rect(state, "tuna-shell-dash")
    items, separators = dash_items(nodes)
    names = [node["name"] for node, _ in items]
    if names != expect:
        raise CheckError(f"the dash lists {names}, expected {expect}")
    lefts = {node["name"]: origin[0] + node["bounds"][0] for node, _ in items}
    reading = [lefts[n] if direction == "ltr" else -lefts[n] for n in names]
    if reading != sorted(reading) or len(set(reading)) != len(reading):
        raise CheckError(f"the dash is not laid out {direction}: lefts {lefts}")
    if separator_after:
        after = names[names.index(separator_after) + 1]
        low, high = sorted((lefts[separator_after], lefts[after]))
        if not any(low < origin[0] + s["bounds"][0] < high for s in separators):
            raise CheckError(f"no separator between {separator_after} and {after}: "
                             f"{[s['bounds'] for s in separators]}")
    expected_dots = [n for n in expect if n in running]
    # AppIcon's dot is a small unnamed child of the running tile.
    marked = [node["name"] for node, kids in items
              if any(k.get("role") == "panel" and not k.get("name")
                     and k["bounds"][2] <= 8 and k["bounds"][3] <= 8 for k in kids)]
    if marked != expected_dots:
        raise CheckError(f"accessible running dots under {marked}, expected under {expected_dots}")
    widths = {}
    for node, _ in items:
        if node["name"] == "Show Apps":
            continue
        x, y, w, h = node["bounds"]
        rect = [origin[0] + x, origin[1] + y, origin[0] + x + w, origin[1] + y + h]
        widths[node["name"]] = max(dot_rows(image, rect, scale))
    dotted = [name for name, width in widths.items() if width >= max(2, round(3 * scale))]
    if dotted != expected_dots:
        raise CheckError(f"drawn running dots under {dotted}, expected under {expected_dots} "
                         f"(widest near-white run per tile: {widths})")
    return (f"scale {scale:g} {direction}: dash {names}"
            + (f", separator after {separator_after}" if separator_after else "")
            + f"; running dots drawn and accessible under {dotted}")


def load_rgb(path):
    from PIL import Image
    with Image.open(path) as image:
        return image.convert("RGB")


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("what", choices=["panel-points", "panel", "dash"])
    ap.add_argument("a11y")
    ap.add_argument("state")
    ap.add_argument("frames", nargs="*")
    ap.add_argument("--physical-width", type=int, required=True)
    ap.add_argument("--direction", choices=["ltr", "rtl"], default="ltr")
    ap.add_argument("--expect", nargs="+", default=[])
    ap.add_argument("--running", nargs="*", default=[])
    ap.add_argument("--separator-after")
    a = ap.parse_args(argv)
    wants = {"panel-points": 0, "panel": 3, "dash": 1}[a.what]
    if len(a.frames) != wants:
        ap.error(f"{a.what} takes {wants} capture(s)")
    if a.what == "dash" and not a.expect:
        ap.error("dash needs --expect")
    with open(a.a11y, encoding="utf-8") as fh:
        nodes = json.load(fh)
    with open(a.state, encoding="utf-8") as fh:
        state = json.load(fh)
    try:
        if a.what == "panel-points":
            for name, x, y in panel_points(nodes, state, a.physical_width):
                print(f"{name.replace(' ', '-')} {x} {y}")
        elif a.what == "panel":
            images = [load_rgb(frame) for frame in a.frames]
            print(check_panel(nodes, state, images, a.physical_width, a.direction))
        else:
            print(check_dash(nodes, state, load_rgb(a.frames[0]), a.physical_width,
                             a.direction, a.expect, a.running, a.separator_after))
    except CheckError as error:
        print(f"tuna-panel-dash-check: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
