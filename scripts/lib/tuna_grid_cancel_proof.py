#!/usr/bin/env python3
"""Fail-closed evidence checks used by the real headed grid cancellation gate.

These state receipts are harness-owned observations, not authenticated Wayland
client identity. The caller binds fresh AT-SPI trees to its live GTK PID/start.
"""
import json
import math
from pathlib import Path
import sys


def validate(before, cancelled, before_layout, after_layout):
    for state in (before, cancelled):
        if not isinstance(state, dict):
            raise ValueError("missing compositor state")
        for key, expected in (("locked", False), ("overview_open", True), ("overview_app_grid", True)):
            if state.get(key) is not expected:
                raise ValueError(f"invalid {key}")
    if before.get("client_dnd_active") is not True:
        raise ValueError("no native drag before Escape")
    if cancelled.get("client_dnd_active") is not False:
        raise ValueError("drag did not retire before mouse release")
    a, b = before.get("rendered_frames"), cancelled.get("rendered_frames")
    if type(a) is not int or type(b) is not int or a < 0 or b <= a:
        raise ValueError("missing or stale cancellation frame")
    held_a, held_b = before.get("seat_pressed_key_count"), cancelled.get("seat_pressed_key_count")
    if type(held_a) is not int or type(held_b) is not int or held_a < 0 or held_a != held_b:
        raise ValueError("consumed Escape release did not restore held-key baseline")
    if not isinstance(before_layout, bytes) or not before_layout.strip():
        raise ValueError("missing saved layout")
    if before_layout != after_layout:
        raise ValueError("cancelled drag changed saved layout")


def validate_tree(nodes, pid):
    if type(pid) is not int or pid <= 0 or not isinstance(nodes, list) or not nodes:
        raise ValueError("missing owner-bound accessibility tree")
    if not isinstance(nodes[0], dict) or type(nodes[0].get("process_id")) is not int or nodes[0]["process_id"] != pid:
        raise ValueError("wrong accessibility owner")
    previous_depth = 0
    anchor = False
    for index, node in enumerate(nodes):
        if not isinstance(node, dict):
            raise ValueError("malformed accessibility node")
        if node.get("role") == "error":
            raise ValueError("accessibility transport error")
        for name in ("role", "name", "description"):
            if not isinstance(node.get(name), str):
                raise ValueError(f"malformed accessibility {name}")
        if not node["role"].strip():
            raise ValueError("empty accessibility role")
        for name in ("showing", "checked", "selected", "focused"):
            if type(node.get(name)) is not bool:
                raise ValueError(f"malformed accessibility {name}")
        depth = node.get("depth")
        if type(depth) is not int or depth < 0 or (index == 0 and depth != 0) or (index > 0 and (depth == 0 or depth > previous_depth + 1)):
            raise ValueError("malformed accessibility depth")
        previous_depth = depth
        if "bounds" not in node:
            raise ValueError("missing accessibility bounds")
        bounds = node["bounds"]
        if bounds is not None:
            if not isinstance(bounds, list) or len(bounds) != 4 or any(type(v) not in (int, float) or not math.isfinite(v) for v in bounds) or bounds[2] < 0 or bounds[3] < 0:
                raise ValueError("malformed accessibility bounds")
        if node["showing"] is True and node["role"] in ("push button", "button") and bounds is not None and bounds[2] > 0 and bounds[3] > 0:
            anchor = True
    if not anchor:
        raise ValueError("missing showing live button anchor")


def validate_live_tree(pid, start, path, proc=Path("/proc")):
    def process_start():
        return (proc / str(pid) / "stat").read_text().rsplit(")", 1)[1].split()[19]
    if process_start() != start:
        raise ValueError("GTK owner replaced before tree validation")
    validate_tree(json.loads(Path(path).read_text()), pid)
    if process_start() != start:
        raise ValueError("GTK owner replaced during tree validation")


def grid_geometry(nodes, state, pid):
    """Use actual grid allocation; reject incomplete, ambiguous or oversized trees."""
    validate_tree(nodes, pid)
    if len(nodes) > 4096:
        raise ValueError("oversized accessibility tree")
    for key, expected in (("locked", False), ("overview_open", True), ("overview_app_grid", True)):
        if state.get(key) is not expected:
            raise ValueError(f"invalid {key}")
    raw_layers = state.get("layers")
    if not isinstance(raw_layers, list) or any(not isinstance(v, dict) for v in raw_layers):
        raise ValueError("missing grid layers")
    layers = [v for v in raw_layers if v.get("namespace") == "tuna-shell-appgrid"]
    if len(layers) != 1:
        raise ValueError("ambiguous grid layer")
    rect = layers[0].get("rect")
    if not isinstance(rect, list) or len(rect) != 4 or any(type(v) not in (int, float) or not math.isfinite(v) for v in rect) or min(rect[2:]) <= 0:
        raise ValueError("invalid grid allocation")
    ox, oy, width, height = rect
    tables = [n["bounds"] for n in nodes if n["showing"] and n["role"] == "table" and n["bounds"] and n["bounds"][2] > 0 and n["bounds"][3] > 0]
    # GTK marks adjacent carousel pages SHOWING too. Admit exactly one table
    # wholly within the actual layer; a partly visible second page is unstable.
    intersecting = [r for r in tables if r[0] < width and r[0] + r[2] > 0 and r[1] < height and r[1] + r[3] > 0]
    if len(intersecting) != 1:
        raise ValueError("ambiguous allocated grid table")
    tables = intersecting
    tx, ty, tw, th = tables[0]
    if tx < 0 or ty < 0 or tx + tw > width or ty + th > height:
        raise ValueError("table outside grid allocation")
    # Bounds are local to their GTK layer. Including every showing button is
    # deliberately conservative: extra forbidden rectangles cannot admit a hit.
    forbidden = [n["bounds"] for n in nodes if n["showing"] and n["bounds"] and
                 (n["role"] in ("button", "push button", "table cell") or n["name"] in
                  ("Next page preview", "Previous page preview"))]
    return (ox, oy, width, height), tables[0], forbidden


def clear_point(point, table, forbidden):
    x, y = point
    tx, ty, tw, th = table
    # Keep away from tile/hint borders, rather than accept their exact edge.
    return (tx + 3 < x < tx + tw - 3 and ty + 3 < y < ty + th - 3 and
            all(not (a - 3 <= x <= a + w + 3 and b - 3 <= y <= b + h + 3)
                for a, b, w, h in forbidden if w > 0 and h > 0))


def neutral_point(nodes, state, pid):
    layer, table, forbidden = grid_geometry(nodes, state, pid)
    names = {n["name"] for n in nodes if n["showing"]}
    if "Next Page" not in names or "Previous Page" in names or any(n in names for n in ("Next page preview", "Previous page preview")):
        raise ValueError("source is not normal first page")
    tx, ty, tw, th = table
    def axis(start, extent, index):
        edges = sorted({start, start + extent, *(v[index] for v in forbidden),
                        *(v[index] + v[index + 2] for v in forbidden)})
        if len(edges) > 256:
            raise ValueError("excessive geometry search")
        return sorted({int((a + b) / 2) for a, b in zip(edges, edges[1:])
                       if start + 3 < (a + b) / 2 < start + extent - 3},
                      key=lambda v: (abs(v - start - extent / 2), v))
    for y in axis(ty, th, 1):
        for x in axis(tx, tw, 0):
            if clear_point((x, y), table, forbidden):
                return [int(layer[0] + x), int(layer[1] + y)]
    raise ValueError("no observed neutral grid point")


def page_content(nodes, state, pid):
    _, table, _ = grid_geometry(nodes, state, pid)
    tx, ty, tw, th = table
    cells = [(n["name"], n["bounds"]) for n in nodes if n["showing"] and
             n["role"] == "table cell" and n["bounds"] and
             tx <= n["bounds"][0] < tx + tw and ty <= n["bounds"][1] < ty + th]
    if not cells or any(not name.strip() for name, _ in cells):
        raise ValueError("missing actual first-page cells")
    return cells


def validate_neutral(nodes, state, pid, point, source_nodes):
    layer, table, forbidden = grid_geometry(nodes, state, pid)
    if (not isinstance(point, list) or len(point) != 2 or
            any(type(v) is not int for v in point)):
        raise ValueError("invalid neutral pointer")
    # Arrow/preview actors alone do not establish the carousel page. Compare
    # its actual cells with the freshly admitted normal first-page tree.
    neutral_point(source_nodes, state, pid)
    if page_content(nodes, state, pid) != page_content(source_nodes, state, pid):
        raise ValueError("actual carousel page changed before Escape")
    names = {n["name"] for n in nodes if n["showing"]}
    if "Next page preview" not in names or any(n in names for n in
            ("Previous page preview", "Next Page", "Previous Page")):
        raise ValueError("active drag is not on first page")
    if state.get("client_dnd_active") is not True or state.get("pointer_position") != point:
        raise ValueError("native drag pointer not delivered to neutral point")
    if not clear_point((point[0] - layer[0], point[1] - layer[1]), table, forbidden):
        raise ValueError("pointer overlaps actual tile or page hint")


def main():
    if len(sys.argv) in (7, 10) and sys.argv[1] in ("neutral", "neutral-check"):
        mode, pid, start, tree, state = sys.argv[1:6]
        pid = int(pid)
        validate_live_tree(pid, start, Path(tree))
        nodes, snapshot = json.loads(Path(tree).read_text()), json.loads(Path(state).read_text())
        if mode == "neutral" and len(sys.argv) == 7:
            point = neutral_point(nodes, snapshot, pid)
            output = Path(sys.argv[6])
            result = point
        elif mode == "neutral-check" and len(sys.argv) == 10:
            point = [int(sys.argv[6]), int(sys.argv[7])]
            validate_live_tree(pid, start, Path(sys.argv[8]))
            source_nodes = json.loads(Path(sys.argv[8]).read_text())
            validate_neutral(nodes, snapshot, pid, point, source_nodes)
            output = Path(sys.argv[9])
            result = {"pid": pid, "start": start, "point": point,
                "grid_layer": grid_geometry(nodes, snapshot, pid)[0],
                "first_page_observed": True, "native_drag_pointer_observed": True}
        else:
            raise ValueError("invalid neutral command")
        validate_live_tree(pid, start, Path(tree))
        output.write_text(json.dumps(result) + "\n")
        if mode == "neutral":
            print(*point)
        return
    if len(sys.argv) == 5 and sys.argv[1] == "tree":
        validate_live_tree(int(sys.argv[2]), sys.argv[3], Path(sys.argv[4]))
        return
    before, cancelled, before_layout, after_layout = map(Path, sys.argv[1:])
    validate(json.loads(before.read_text()), json.loads(cancelled.read_text()),
             before_layout.read_bytes(), after_layout.read_bytes())
    print("grid-cancel: native drag retired before release, grid stayed open, layout preserved")


if __name__ == "__main__":
    main()
