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


def main():
    if len(sys.argv) == 5 and sys.argv[1] == "tree":
        validate_live_tree(int(sys.argv[2]), sys.argv[3], Path(sys.argv[4]))
        return
    before, cancelled, before_layout, after_layout = map(Path, sys.argv[1:])
    validate(json.loads(before.read_text()), json.loads(cancelled.read_text()),
             before_layout.read_bytes(), after_layout.read_bytes())
    print("grid-cancel: native drag retired before release, grid stayed open, layout preserved")


if __name__ == "__main__":
    main()
