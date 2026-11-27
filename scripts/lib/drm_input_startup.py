#!/usr/bin/env python3
"""D-INPUT-AT-STARTUP: input devices present at login are added at once.

Usage:
  drm_input_startup.py hold NAME          (root) create a uinput mouse NAME,
                                          print `ready`, keep it until stdin
                                          closes; it never sends an event
  drm_input_startup.py check STATE NAME   exit 0 when the compositor state
                                          (TUNA_COMPOSITOR_STATE) lists the
                                          input device NAME, else 1

libinput queues the devices already present when the compositor assigns
its seat without making its fd readable; a compositor that only
dispatches libinput on readiness adds them, and applies their settings,
at the first later input or hotplug. tuna-drm-smoke holds a mouse from
before the compositor starts and requires it in the state within a
deadline while nothing produces input.
"""
import json
import sys
import time


def problem(state, name):
    """None when `state` lists input device `name`, else why not."""
    devices = state.get("input_devices")
    if devices is None:
        return "the state has no input_devices"
    names = [d.get("name") for d in devices]
    if name not in names:
        return f"{name!r} is not among the input devices {names}"
    return None


def hold(name):
    from evdev import UInput, ecodes as e
    mouse = UInput({e.EV_KEY: [e.BTN_LEFT, e.BTN_RIGHT, e.BTN_MIDDLE],
                    e.EV_REL: [e.REL_X, e.REL_Y]},
                   name=name, bustype=e.BUS_VIRTUAL)
    # Let udev tag the node before anyone enumerates it.
    time.sleep(0.5)
    print("ready", flush=True)
    sys.stdin.read()
    mouse.close()


def check(path, name):
    try:
        with open(path, encoding="utf-8") as stream:
            state = json.load(stream)
    except (OSError, json.JSONDecodeError) as error:
        print(f"no compositor state: {error}")
        return 1
    why = problem(state, name)
    if why:
        print(why)
        return 1
    kind = next(d.get("kind") for d in state["input_devices"] if d.get("name") == name)
    print(f"{name!r} listed as {kind} among {len(state['input_devices'])} input devices")
    return 0


def main():
    if len(sys.argv) == 3 and sys.argv[1] == "hold":
        hold(sys.argv[2])
        return 0
    if len(sys.argv) == 4 and sys.argv[1] == "check":
        return check(sys.argv[2], sys.argv[3])
    print(__doc__, file=sys.stderr)
    return 2


if __name__ == "__main__":
    sys.exit(main())
