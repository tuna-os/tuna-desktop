#!/usr/bin/env python3
"""Emulated multitouch clickpad for tuna-drm-smoke's D-TOUCHPAD-SWIPE (P-IN-02, #593).

Creates, through uinput, the evdev device the kernel's rmi4 driver creates
for a Synaptics TM3053-004 clickpad (the capabilities of libinput's own
litest-device-synaptics-rmi4.c): INPUT_PROP_POINTER | INPUT_PROP_BUTTONPAD,
five ABS_MT slots with tracking ids, position, pressure, touch size,
orientation and tool type, the legacy ABS_X/ABS_Y/ABS_PRESSURE pointer,
BTN_TOUCH, BTN_TOOL_FINGER..QUINTTAP and the single BTN_LEFT under the
pad. Positions resolve at 20 units/mm (97 x 53 mm). udev marks it
ID_INPUT_TOUCHPAD and libinput drives it as a clickpad, so it produces
real pointer motion, two-finger scrolling, taps and
LIBINPUT_EVENT_GESTURE_SWIPE_* for three or more fingers, timed by the
kernel as the fingers move.

Usable two ways:

* Standalone, as root (uinput): reads one command per line on stdin and
  replies "ok" or "error ..." per line, so a shell driver stays in
  lockstep (the same protocol as tuna-uinput-tablet.py). Lengths are
  millimetres on the pad, times milliseconds; fingers rest along a row
  through the pad's centre unless placed with X Y (0..1):

    down N [X Y]                N fingers touch (1..5)
    move DX DY MS               the touching fingers travel DX, DY mm
                                (right, down positive) over MS ms
    hold MS                     the fingers rest for MS ms
    up                          every finger lifts
    swipe N DX DY MS [PAUSE]    down N, move, hold PAUSE ms, up
    scroll DX DY MS             a two-finger swipe (libinput scrolls)
    tap [N]                     N fingers (default 1) touch for 40 ms
    click                       press and release the pad's button
    unplug / plug               remove / recreate the device
    quit

* Imported (`importlib.util.spec_from_file_location`, the file name has
  dashes): `create_touchpad(name=NAME)` returns the bare evdev UInput
  device for callers that drive it themselves; `Touchpad(name=NAME)`
  wraps one with `down`, `move`, `hold`, `up`, `swipe`, `scroll`, `tap`
  and `click` methods (the commands above) and `close()` removes it.
"""

import sys
import time

from evdev import AbsInfo, UInput, ecodes as e

NAME = "Synaptics TM3053-004"
VENDOR, PRODUCT = 0x06CB, 0x0000
# BUS_RMI: the rmi4 bus, as an internal laptop clickpad reports it.
BUS_RMI = 0x1D
PHYS = "rmi4-00.fn11/input0"
MAX_X, MAX_Y = 1940, 1062
RES = 20
SLOTS = 5
# A resting finger: well under libinput's palm pressure (130 without a
# quirk) and over its touch threshold.
PRESSURE = 70
TOUCH_MAJOR, TOUCH_MINOR = 6, 5
# Fingers side by side, 15 mm apart, as a hand lands on a pad.
FINGER_SPACING_MM = 15.0
# Seconds between reports while fingers move: the sensor's ~100 Hz.
REPORT_INTERVAL = 0.01
TOOLS = {
    1: e.BTN_TOOL_FINGER,
    2: e.BTN_TOOL_DOUBLETAP,
    3: e.BTN_TOOL_TRIPLETAP,
    4: e.BTN_TOOL_QUADTAP,
    5: e.BTN_TOOL_QUINTTAP,
}


def create_touchpad(name=NAME):
    return UInput(
        {
            e.EV_KEY: [e.BTN_LEFT, e.BTN_TOUCH, *TOOLS.values()],
            e.EV_ABS: [
                (e.ABS_X, AbsInfo(0, 0, MAX_X, 0, 0, RES)),
                (e.ABS_Y, AbsInfo(0, 0, MAX_Y, 0, 0, RES)),
                (e.ABS_PRESSURE, AbsInfo(0, 0, 255, 0, 0, 0)),
                (e.ABS_MT_SLOT, AbsInfo(0, 0, SLOTS - 1, 0, 0, 0)),
                (e.ABS_MT_TOUCH_MAJOR, AbsInfo(0, 0, 15, 0, 0, 0)),
                (e.ABS_MT_TOUCH_MINOR, AbsInfo(0, 0, 15, 0, 0, 0)),
                (e.ABS_MT_ORIENTATION, AbsInfo(0, 0, 1, 0, 0, 0)),
                (e.ABS_MT_POSITION_X, AbsInfo(0, 0, MAX_X, 0, 0, RES)),
                (e.ABS_MT_POSITION_Y, AbsInfo(0, 0, MAX_Y, 0, 0, RES)),
                (e.ABS_MT_TOOL_TYPE, AbsInfo(0, 0, 2, 0, 0, 0)),
                (e.ABS_MT_TRACKING_ID, AbsInfo(0, 0, 65535, 0, 0, 0)),
                (e.ABS_MT_PRESSURE, AbsInfo(0, 0, 255, 0, 0, 0)),
            ],
        },
        name=name,
        vendor=VENDOR,
        product=PRODUCT,
        version=0x1,
        bustype=BUS_RMI,
        phys=PHYS,
        input_props=[e.INPUT_PROP_POINTER, e.INPUT_PROP_BUTTONPAD],
    )


class Touchpad:
    def __init__(self, name=NAME):
        self.name = name
        self.next_id = 1
        self.plug()

    def plug(self):
        self.ui = create_touchpad(self.name)
        # Per slot: (x, y) in device units while the finger touches.
        self.fingers = {}
        # libinput opens the node once udev announces it.
        time.sleep(0.5)

    def unplug(self):
        self.ui.close()
        self.fingers = {}

    close = unplug

    def frame(self, events):
        for etype, code, value in events:
            self.ui.write(etype, code, value)
        self.ui.syn()

    def positions(self):
        return [(slot, *self.fingers[slot]) for slot in sorted(self.fingers)]

    def legacy(self):
        """The single-touch pointer the kernel reports beside the slots:
        the first touching finger."""
        if not self.fingers:
            return []
        _, x, y = self.positions()[0]
        return [
            (e.EV_ABS, e.ABS_X, x),
            (e.EV_ABS, e.ABS_Y, y),
            (e.EV_ABS, e.ABS_PRESSURE, PRESSURE),
        ]

    def down(self, count, x=0.5, y=0.5):
        count = int(count)
        if self.fingers:
            raise ValueError("fingers already down")
        if count not in TOOLS:
            raise ValueError(f"{count} fingers: 1 to 5")
        cx, cy = float(x) * MAX_X, float(y) * MAX_Y
        events = []
        for slot in range(count):
            fx = round(cx + (slot - (count - 1) / 2) * FINGER_SPACING_MM * RES)
            fy = round(cy)
            if not (0 <= fx <= MAX_X and 0 <= fy <= MAX_Y):
                raise ValueError("fingers off the pad")
            self.fingers[slot] = (fx, fy)
            events += [
                (e.EV_ABS, e.ABS_MT_SLOT, slot),
                (e.EV_ABS, e.ABS_MT_TRACKING_ID, self.next_id),
                (e.EV_ABS, e.ABS_MT_TOOL_TYPE, 0),
                (e.EV_ABS, e.ABS_MT_POSITION_X, fx),
                (e.EV_ABS, e.ABS_MT_POSITION_Y, fy),
                (e.EV_ABS, e.ABS_MT_PRESSURE, PRESSURE),
                (e.EV_ABS, e.ABS_MT_TOUCH_MAJOR, TOUCH_MAJOR),
                (e.EV_ABS, e.ABS_MT_TOUCH_MINOR, TOUCH_MINOR),
                (e.EV_ABS, e.ABS_MT_ORIENTATION, 0),
            ]
            self.next_id = self.next_id % 65535 + 1
        events += self.legacy()
        events += [(e.EV_KEY, e.BTN_TOUCH, 1), (e.EV_KEY, TOOLS[count], 1)]
        self.frame(events)

    def move(self, dx, dy, ms):
        """Every touching finger travels (dx, dy) mm in steps of one
        report interval, at a constant speed over `ms`."""
        if not self.fingers:
            raise ValueError("no finger down")
        dx, dy = float(dx) * RES, float(dy) * RES
        steps = max(1, round(float(ms) / 1000 / REPORT_INTERVAL))
        start = dict(self.fingers)
        began = time.monotonic()
        for step in range(1, steps + 1):
            # Report on the sensor's clock, not after however long the
            # previous write took.
            delay = began + step * REPORT_INTERVAL - time.monotonic()
            if delay > 0:
                time.sleep(delay)
            events = []
            for slot, (x, y) in start.items():
                fx = round(x + dx * step / steps)
                fy = round(y + dy * step / steps)
                if not (0 <= fx <= MAX_X and 0 <= fy <= MAX_Y):
                    raise ValueError("fingers ran off the pad")
                self.fingers[slot] = (fx, fy)
                events += [
                    (e.EV_ABS, e.ABS_MT_SLOT, slot),
                    (e.EV_ABS, e.ABS_MT_POSITION_X, fx),
                    (e.EV_ABS, e.ABS_MT_POSITION_Y, fy),
                ]
            self.frame(events + self.legacy())

    def hold(self, ms):
        # Unchanged axes are never reported (the input core drops
        # repeated values), so resting fingers are silent.
        time.sleep(float(ms) / 1000)

    def up(self):
        if not self.fingers:
            raise ValueError("no finger down")
        count = len(self.fingers)
        events = []
        for slot in sorted(self.fingers):
            events += [(e.EV_ABS, e.ABS_MT_SLOT, slot), (e.EV_ABS, e.ABS_MT_TRACKING_ID, -1)]
        events += [
            (e.EV_ABS, e.ABS_PRESSURE, 0),
            (e.EV_KEY, TOOLS[count], 0),
            (e.EV_KEY, e.BTN_TOUCH, 0),
        ]
        self.fingers = {}
        self.frame(events)

    def swipe(self, count, dx, dy, ms, pause=0):
        self.down(count)
        # Fingers land and settle before moving, as a hand does.
        self.hold(30)
        self.move(dx, dy, ms)
        self.hold(pause)
        self.up()

    def scroll(self, dx, dy, ms):
        self.swipe(2, dx, dy, ms)

    def tap(self, count=1):
        self.down(count)
        self.hold(40)
        self.up()

    def click(self):
        self.frame([(e.EV_KEY, e.BTN_LEFT, 1)])
        self.frame([(e.EV_KEY, e.BTN_LEFT, 0)])

    def run(self, words):
        cmd, args = words[0], words[1:]
        commands = {
            "down": self.down,
            "move": self.move,
            "hold": self.hold,
            "up": self.up,
            "swipe": self.swipe,
            "scroll": self.scroll,
            "tap": self.tap,
            "click": self.click,
            "unplug": self.unplug,
            "plug": self.plug,
        }
        if cmd not in commands:
            raise ValueError(f"unknown command {cmd}")
        commands[cmd](*args)


def main():
    touchpad = Touchpad()
    print("ready", flush=True)
    for line in sys.stdin:
        words = line.split()
        if not words:
            continue
        if words[0] == "quit":
            break
        try:
            touchpad.run(words)
            print("ok", flush=True)
        except Exception as error:  # reported to the driver, never fatal
            print(f"error {error}", flush=True)
    touchpad.close()


if __name__ == "__main__":
    main()
