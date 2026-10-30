#!/usr/bin/env python3
"""Emulated pointer devices for tuna-drm-smoke's D-POINTER-SETTINGS (#343).

Creates, through uinput (ADR 0008 emulation), the device classes Mutter 51
applies GNOME's peripherals settings to, each as the kernel and udev
present the real thing:

  mouse           relative motion, wheel, left/right/middle and side buttons
  trackball       the same, named "... Trackball"; tuna-drm-smoke's udev
                  rule gives it the ID_INPUT_TRACKBALL that systemd's
                  70-mouse.hwdb (`mouse:*:name:*Trackball*:*`) gives real
                  USB trackballs, since a uinput device has no USB parent
  pointing-stick  relative motion and three buttons with
                  INPUT_PROP_POINTING_STICK: udev's input_id builtin sets
                  ID_INPUT_POINTINGSTICK, libinput defaults it to
                  on-button-down scrolling
  touchpad        tuna-uinput-touchpad.py's Synaptics RMI4 clickpad

Runs as root (uinput) and reads one command per line on stdin, replying
"ok" or "error ..." per line, so the shell driver stays in lockstep:

  add KIND        create the device (KIND as above)
  remove KIND     destroy it
  quit
"""

import importlib.util
import pathlib
import sys

from evdev import UInput, ecodes as e

_SPEC = importlib.util.spec_from_file_location(
    "tuna_uinput_touchpad", pathlib.Path(__file__).with_name("tuna-uinput-touchpad.py"))
touchpad = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(touchpad)

BUTTONS = {
    "mouse": [e.BTN_LEFT, e.BTN_RIGHT, e.BTN_MIDDLE, e.BTN_SIDE, e.BTN_EXTRA],
    "trackball": [e.BTN_LEFT, e.BTN_RIGHT, e.BTN_MIDDLE, e.BTN_SIDE, e.BTN_EXTRA],
    "pointing-stick": [e.BTN_LEFT, e.BTN_RIGHT, e.BTN_MIDDLE],
}
NAMES = {
    "mouse": "Tuna D-POINTER Mouse",
    "trackball": "Tuna D-POINTER Trackball",
    "pointing-stick": "Tuna D-POINTER TrackPoint",
    "touchpad": touchpad.NAME,
}


def device(kind):
    if kind == "touchpad":
        return touchpad.create_touchpad()
    relative = [e.REL_X, e.REL_Y]
    if kind != "pointing-stick":
        relative += [e.REL_WHEEL, e.REL_HWHEEL]
    return UInput(
        {e.EV_KEY: BUTTONS[kind], e.EV_REL: relative},
        name=NAMES[kind],
        bustype=e.BUS_USB if kind != "pointing-stick" else e.BUS_I8042,
        input_props=[e.INPUT_PROP_POINTING_STICK] if kind == "pointing-stick" else [],
    )


def main():
    devices = {}
    for line in sys.stdin:
        words = line.split()
        try:
            if not words:
                continue
            if words == ["quit"]:
                break
            if len(words) != 2 or words[1] not in NAMES:
                raise ValueError(f"unknown command {line.strip()!r}")
            command, kind = words
            if command == "add":
                if kind in devices:
                    raise ValueError(f"{kind} already present")
                devices[kind] = device(kind)
            elif command == "remove":
                devices.pop(kind).close()
            else:
                raise ValueError(f"unknown command {line.strip()!r}")
            print("ok", flush=True)
        except (ValueError, KeyError, OSError) as error:
            print(f"error {error}", flush=True)
    for dev in devices.values():
        dev.close()


if __name__ == "__main__":
    main()
