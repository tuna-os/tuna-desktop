#!/usr/bin/env python3
"""Emulated keyboard for tuna-drm-smoke's D-OVERVIEW-EXIT.

Creates, through uinput, a plain USB keyboard (every key code below 256),
so udev marks it ID_INPUT_KEYBOARD and libinput hands its keys to the
compositor as a real keyboard's.

Run as root (uinput): reads one command per line on stdin and replies
"ok" or "error ..." per line, so a shell driver stays in lockstep (the
protocol of tuna-uinput-touchpad.py). Keys are evdev names without the
KEY_ prefix, case-insensitive (esc, leftmeta, l):

    tap KEY...      press the keys in order, release them in reverse
    quit
"""

import sys
import time

from evdev import UInput, ecodes as e

NAME = "Tuna Smoke Keyboard"
# A key held for a human-length moment, so nothing reads it as a bounce.
HOLD = 0.03


def code(name):
    value = getattr(e, "KEY_" + name.upper(), None)
    if not isinstance(value, int):
        raise ValueError(f"unknown key {name}")
    return value


def main():
    keys = sorted({v for k, v in vars(e).items() if k.startswith("KEY_") and isinstance(v, int) and v < 256})
    keyboard = UInput({e.EV_KEY: keys}, name=NAME, vendor=0x1, product=0x1, bustype=e.BUS_USB)
    # libinput opens the node once udev announces it.
    time.sleep(0.5)
    print("ready", flush=True)
    for line in sys.stdin:
        words = line.split()
        if not words:
            continue
        if words[0] == "quit":
            break
        try:
            if words[0] != "tap" or len(words) < 2:
                raise ValueError(f"unknown command {line.strip()}")
            codes = [code(word) for word in words[1:]]
            for value in codes:
                keyboard.write(e.EV_KEY, value, 1)
                keyboard.syn()
                time.sleep(HOLD)
            for value in reversed(codes):
                keyboard.write(e.EV_KEY, value, 0)
                keyboard.syn()
                time.sleep(HOLD)
            print("ok", flush=True)
        except Exception as error:  # reported to the driver, never fatal
            print(f"error {error}", flush=True)
    keyboard.close()


if __name__ == "__main__":
    main()
