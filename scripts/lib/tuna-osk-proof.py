#!/usr/bin/python3
"""Helpers for scripts/tuna-osk-proof (#349).

  tuna-osk-proof.py watch STATE OUT
      Poll the compositor state snapshot every 5 ms until killed (exit 1
      after 1800 s so an orphaned sampler cannot run forever) and
      append one JSON line to OUT whenever what the proof follows
      changes: {"t": CLOCK_MONOTONIC seconds, "osk": [x, y, w, h] of the
      mapped tuna-osk layer or null, "locked", "text_input",
      "focused_app_id"}.
  tuna-osk-proof.py key DUMP WINDOW OX OY NAME
      From a tuna-a11y-dump.py tree, print the global centre "X Y" of
      the showing key button NAME inside the top-level window titled
      WINDOW, whose surface origin is (OX, OY). Exit 1 when absent.
  tuna-osk-proof.py first WATCH SINCE PRESENT
      Print the CLOCK_MONOTONIC time of the first watched change at or
      after SINCE where the keyboard layer is mapped (PRESENT=1) or
      unmapped (PRESENT=0); exit 1 when none.
  tuna-osk-proof.py steady WATCH FROM TO
      Exit 0 when the keyboard layer stayed mapped from FROM to TO
      (every change in between keeps it mapped, and it was mapped at
      FROM); otherwise print the first gap and exit 1.
  tuna-osk-proof.py never WATCH FROM TO
      Exit 0 when no change in [FROM, TO] maps the keyboard layer.
"""
import json
import sys
import time

NAMESPACE = "tuna-osk"
BUTTON_ROLES = {"push button", "button", "toggle button"}


def sample(path):
    try:
        with open(path, encoding="utf-8") as fh:
            state = json.load(fh)
    except (OSError, ValueError):
        return None
    osk = next((layer["rect"] for layer in state.get("layers") or []
                if layer.get("namespace") == NAMESPACE), None)
    return {
        "osk": osk,
        "locked": state.get("locked"),
        "text_input": state.get("text_input"),
        "focused_app_id": state.get("focused_app_id"),
    }


def watch(state, out):
    last = None
    end = time.monotonic() + 1800
    while time.monotonic() < end:
        now = sample(state)
        if now is not None and now != last:
            with open(out, "a", encoding="utf-8") as fh:
                fh.write(json.dumps(dict(now, t=time.monotonic())) + "\n")
            last = now
        time.sleep(0.005)
    sys.exit("watch: not stopped within 1800 s")


def key(dump, window, ox, oy, name):
    with open(dump, encoding="utf-8") as fh:
        nodes = json.load(fh)
    inside = None
    for node in nodes:
        depth = node.get("depth", 0)
        if inside is not None and depth <= inside:
            inside = None
        if inside is None:
            if node.get("name") == window and node.get("role") in {"frame", "window"}:
                inside = depth
            continue
        if (node.get("name") == name and node.get("role") in BUTTON_ROLES
                and node.get("showing") and node.get("bounds")):
            x, y, w, h = node["bounds"]
            if w > 0 and h > 0:
                print(f"{int(ox) + x + w // 2} {int(oy) + y + h // 2}")
                return 0
    return 1


def changes(path):
    with open(path, encoding="utf-8") as fh:
        return [json.loads(line) for line in fh if line.strip()]


def first(path, since, present):
    want = present == "1"
    for change in changes(path):
        if change["t"] >= float(since) and (change["osk"] is not None) == want:
            print(f"{change['t']:.6f}")
            return 0
    return 1


def steady(path, start, end):
    start, end = float(start), float(end)
    mapped = False
    for change in changes(path):
        if change["t"] <= start:
            mapped = change["osk"] is not None
        elif change["t"] <= end and change["osk"] is None:
            print(f"unmapped at {change['t']:.6f}")
            return 1
    if not mapped:
        print(f"not mapped at {start:.6f}")
        return 1
    return 0


def never(path, start, end):
    for change in changes(path):
        if float(start) <= change["t"] <= float(end) and change["osk"] is not None:
            print(f"mapped at {change['t']:.6f}: {change['osk']}")
            return 1
    return 0


def main(argv):
    commands = {"watch": (watch, 2), "key": (key, 5), "first": (first, 3),
                "steady": (steady, 3), "never": (never, 3)}
    if len(argv) < 2 or argv[1] not in commands or len(argv) - 2 != commands[argv[1]][1]:
        print(__doc__, file=sys.stderr)
        return 2
    fn, _ = commands[argv[1]]
    return fn(*argv[2:]) or 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
