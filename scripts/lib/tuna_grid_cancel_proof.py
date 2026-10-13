#!/usr/bin/env python3
"""Fail-closed evidence checks used by the real headed grid cancellation gate.

These state receipts are harness-owned observations, not authenticated Wayland
client identity. The caller binds fresh AT-SPI trees to its live GTK PID/start.
"""
import json
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
    if not isinstance(before_layout, bytes) or not before_layout.strip():
        raise ValueError("missing saved layout")
    if before_layout != after_layout:
        raise ValueError("cancelled drag changed saved layout")


def main():
    before, cancelled, before_layout, after_layout = map(Path, sys.argv[1:])
    validate(json.loads(before.read_text()), json.loads(cancelled.read_text()),
             before_layout.read_bytes(), after_layout.read_bytes())
    print("grid-cancel: native drag retired before release, grid stayed open, layout preserved")


if __name__ == "__main__":
    main()
