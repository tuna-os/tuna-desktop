#!/usr/bin/env python3
"""Emulated touchscreen for tuna-drm-smoke's D-TOUCH and D-ORIENT (P-DS-04, #345).

Creates, through uinput, what a laptop's integrated multitouch panel looks
like to libinput: a direct-input (INPUT_PROP_DIRECT) device with the
type-B multitouch protocol (ABS_MT_SLOT, ABS_MT_TRACKING_ID,
ABS_MT_POSITION_X/Y), single-touch ABS_X/Y and BTN_TOUCH, with a
resolution so it reports a physical size. GNOME keeps its settings at
/org/gnome/desktop/peripherals/touchscreens/1234:5678/. Plus a separate
SW_TABLET_MODE switch, as a convertible's tablet-mode switch: with one
present, Mutter's seat touch-mode follows the switch rather than the
pointer devices, which a CI host may have.

Runs as root (uinput) and reads one command per line on stdin, replying
"ok" or "error ..." per line, so the shell driver stays in lockstep:

  down NX NY        one finger (slot 0) down at the normalized panel point
  move NX NY        the finger moves
  up                the finger lifts
  tabletmode 1|0    the tablet-mode switch on / off
  unplug / plug     remove / recreate the touchscreen (the switch stays)
  quit
"""

import sys
import time

from evdev import AbsInfo, UInput, ecodes as e

NAME = "Tuna Proof Touchscreen"
VENDOR, PRODUCT = 0x1234, 0x5678
MAX = 4095
# Units per mm: 4096 units over 256 x 146 mm, a 11.6" 16:9 panel.
RES_X, RES_Y = 16, 28
SLOTS = 10


def touchscreen_device():
    def axis(res):
        return AbsInfo(0, 0, MAX, 0, 0, res)

    return UInput(
        {
            e.EV_KEY: [e.BTN_TOUCH],
            e.EV_ABS: [
                (e.ABS_X, axis(RES_X)),
                (e.ABS_Y, axis(RES_Y)),
                (e.ABS_MT_SLOT, AbsInfo(0, 0, SLOTS - 1, 0, 0, 0)),
                (e.ABS_MT_TRACKING_ID, AbsInfo(0, 0, 65535, 0, 0, 0)),
                (e.ABS_MT_POSITION_X, axis(RES_X)),
                (e.ABS_MT_POSITION_Y, axis(RES_Y)),
            ],
        },
        name=NAME,
        vendor=VENDOR,
        product=PRODUCT,
        version=0x100,
        bustype=e.BUS_USB,
        input_props=[e.INPUT_PROP_DIRECT],
    )


def switch_device():
    return UInput(
        {e.EV_SW: [e.SW_TABLET_MODE]},
        name="Tuna Proof Tablet Mode Switch",
        bustype=e.BUS_HOST,
    )


def units(value):
    """A normalized coordinate in device units: libinput scales
    (v - min) / (max - min + 1), so v = n * (max + 1) maps back to n."""
    return max(0, min(MAX, round(float(value) * (MAX + 1))))


class Touchscreen:
    def __init__(self):
        self.switch = switch_device()
        self.tracking_id = 0
        self.down = False
        self.plug()

    def plug(self):
        self.touch = touchscreen_device()
        self.down = False

    def unplug(self):
        self.touch.close()
        self.touch = None
        self.down = False

    def frame(self, events):
        if self.touch is None:
            raise ValueError("unplugged")
        for etype, code, value in events:
            self.touch.write(etype, code, value)
        self.touch.syn()

    def position(self, nx, ny):
        x, y = units(nx), units(ny)
        return [
            (e.EV_ABS, e.ABS_MT_SLOT, 0),
            (e.EV_ABS, e.ABS_MT_POSITION_X, x),
            (e.EV_ABS, e.ABS_MT_POSITION_Y, y),
            (e.EV_ABS, e.ABS_X, x),
            (e.EV_ABS, e.ABS_Y, y),
        ]

    def run(self, words):
        cmd = words[0]
        if cmd == "down":
            if self.down:
                raise ValueError("already down")
            self.tracking_id = (self.tracking_id + 1) % 65536
            events = self.position(*words[1:3])
            events.insert(1, (e.EV_ABS, e.ABS_MT_TRACKING_ID, self.tracking_id))
            self.frame(events + [(e.EV_KEY, e.BTN_TOUCH, 1)])
            self.down = True
        elif cmd == "move":
            if not self.down:
                raise ValueError("not down")
            self.frame(self.position(*words[1:3]))
        elif cmd == "up":
            if not self.down:
                raise ValueError("not down")
            self.frame(
                [
                    (e.EV_ABS, e.ABS_MT_SLOT, 0),
                    (e.EV_ABS, e.ABS_MT_TRACKING_ID, -1),
                    (e.EV_KEY, e.BTN_TOUCH, 0),
                ]
            )
            self.down = False
        elif cmd == "tabletmode":
            self.switch.write(e.EV_SW, e.SW_TABLET_MODE, int(words[1]))
            self.switch.syn()
        elif cmd == "unplug":
            self.unplug()
        elif cmd == "plug":
            if self.touch is not None:
                raise ValueError("already plugged")
            self.plug()
        else:
            raise ValueError(f"unknown command {cmd}")
        # Leave libinput a frame interval before the next event.
        time.sleep(0.02)


def main():
    screen = Touchscreen()
    print("ready", flush=True)
    for line in sys.stdin:
        words = line.split()
        if not words:
            continue
        if words[0] == "quit":
            break
        try:
            screen.run(words)
            print("ok", flush=True)
        except Exception as error:  # reported to the driver, never fatal
            print(f"error {error}", flush=True)
    if screen.touch is not None:
        screen.unplug()
    screen.switch.close()


if __name__ == "__main__":
    main()
