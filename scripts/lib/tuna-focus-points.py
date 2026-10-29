#!/usr/bin/env python3
"""Pick pointer targets for the focus-policy proof from the compositor state.

Usage:
  tuna-focus-points.py STATE beneath ID...    -> "ID X Y": a point where one
      of the given windows, neither focused nor on top, is the topmost window
  tuna-focus-points.py STATE visible ID [br]  -> "X Y": a point where window
      ID is topmost (br: inside its bottom-right third)
  tuna-focus-points.py STATE desktop W H      -> "X Y": a point on no window

Points avoid header bars and window edges, and the shell's layer surfaces
(a layer spanning the whole output is input-transparent chrome, as the
overview's, and does not count). Exit status 1 when no such point exists.
"""
import json
import sys
from pathlib import Path

STEP = 10
EDGE = 24
HEADER = 60


def inside(rect, x, y):
    rx, ry, rw, rh = rect
    return rx <= x < rx + rw and ry <= y < ry + rh


def scene(state):
    """Windows on the active workspace, bottom to top, and blocking layers."""
    minimized = set(state.get("minimized", []))
    by_id = {
        w["id"]: w
        for w in state["windows"]
        if w["workspace"] == state["active_workspace"] and w["id"] not in minimized
    }
    stack = [by_id[i] for i in state["stacking"] if i in by_id]
    outputs = [o for o in state.get("layers", [])]
    width = max((l["rect"][0] + l["rect"][2] for l in outputs), default=0)
    height = max((l["rect"][1] + l["rect"][3] for l in outputs), default=0)
    layers = [
        l["rect"]
        for l in outputs
        if not (l["rect"][0] <= 0 and l["rect"][1] <= 0 and l["rect"][2] >= width and l["rect"][3] >= height)
    ]
    return stack, layers


def topmost(stack, layers, x, y):
    """The window id under (x, y), None for no window, False over chrome."""
    if any(inside(rect, x, y) for rect in layers):
        return False
    for window in reversed(stack):
        if inside(window["rect"], x, y):
            return window["id"]
    return None


def visible_point(stack, layers, window, third=None):
    x, y, w, h = window["rect"]
    left, top = x + EDGE, y + HEADER
    if third == "br":
        left, top = max(left, x + 2 * w // 3 + 1), max(top, y + 2 * h // 3 + 1)
    for py in range(top, y + h - EDGE, STEP):
        for px in range(left, x + w - EDGE, STEP):
            if topmost(stack, layers, px, py) == window["id"]:
                return px, py
    return None


def desktop_point(stack, layers, width, height):
    for py in range(40, height - EDGE, STEP):
        for px in range(EDGE, width - EDGE, STEP):
            near = [
                topmost(stack, layers, px + dx, py + dy)
                for dx in (-EDGE, 0, EDGE)
                for dy in (-EDGE, 0, EDGE)
            ]
            if all(hit is None for hit in near):
                return px, py
    return None


def main(argv):
    state = json.loads(Path(argv[1]).read_text())
    mode = argv[2]
    stack, layers = scene(state)
    if mode == "beneath":
        candidates = {int(arg) for arg in argv[3:]}
        top = stack[-1]["id"] if stack else None
        for window in reversed(stack):
            if window["id"] not in candidates or window["id"] in (state.get("focused"), top):
                continue
            point = visible_point(stack, layers, window)
            if point:
                print(window["id"], *point)
                return 0
    elif mode == "visible":
        wanted = int(argv[3])
        third = argv[4] if len(argv) > 4 else None
        for window in stack:
            if window["id"] == wanted:
                point = visible_point(stack, layers, window, third)
                if point:
                    print(*point)
                    return 0
    elif mode == "desktop":
        width, height = (int(v) for v in argv[3:5])
        point = desktop_point(stack, layers, width, height)
        if point:
            print(*point)
            return 0
    else:
        raise SystemExit(f"unknown mode {mode}")
    return 1


if __name__ == "__main__":
    sys.exit(main(sys.argv))
