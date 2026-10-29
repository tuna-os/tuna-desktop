#!/usr/bin/env python3
"""Emulated drawing tablet for tuna-drm-smoke's D-TABLET (P-IN-05, #344).

Creates, through uinput, what the kernel's wacom driver creates for a
Wacom Cintiq 22HD (056a:00fa, DTK-2200; wacom_wac.c WACOM_22HD): a "Pen"
device on the display (pen and eraser tools with pressure, tilt, distance,
serial and two stylus buttons) and a "Pad" device (18 buttons, two touch
strips in two mode groups) sharing one physical path, so libinput groups
them and libwacom knows the model (display-integrated, with the layout
diagram that drives the pad OSD). Plus a plain keyboard.

Runs as root (uinput) and reads one command per line on stdin, replying
"ok" or "error ..." per line, so the shell driver stays in lockstep:

  pen X Y PRESSURE [TILT_X TILT_Y DISTANCE]   tool in proximity, moved (0..1)
  tool pen|eraser|out                        proximity in as pen/eraser, or out
  tip 1|0                                    BTN_TOUCH
  stylus BTN_STYLUS|BTN_STYLUS2 1|0
  pad N 1|0                                  pad button N (libwacom's Nth code)
  strip S P                                  strip S (0, 1) at position P 0..12
                                             (-1 lifts)
  key KEY_NAME                               press and release on the keyboard
  unplug / plug                              remove / recreate pen and pad
  quit
"""

import sys
import time

from evdev import AbsInfo, UInput, ecodes as e

VENDOR, PRODUCT = 0x056A, 0x00FA
PHYS = "usb-0000:00:14.0-9/input0"
# The kernel driver's ranges for this model (wacom_wac.c WACOM_22HD):
# 200 units per mm on 477 x 269 mm, 2048 pressure levels.
MAX_X, MAX_Y = 95440, 53860
RES = 200
MAX_PRESSURE = 2047
# wacom_setup_numbered_buttons(18), libwacom's default codes in order.
PAD_BUTTONS = [e.BTN_0 + n for n in range(10)] + [
    e.BTN_A, e.BTN_B, e.BTN_C, e.BTN_X, e.BTN_Y, e.BTN_Z, e.BTN_BASE, e.BTN_BASE2,
]
STRIPS = [e.ABS_RX, e.ABS_RY]
SERIAL = 0x2F4A1C3
TOOL_ID = {"pen": 0x0802, "eraser": 0x080A}


def pen_device():
    return UInput(
        {
            e.EV_KEY: [
                e.BTN_TOOL_PEN,
                e.BTN_TOOL_RUBBER,
                e.BTN_TOUCH,
                e.BTN_STYLUS,
                e.BTN_STYLUS2,
            ],
            e.EV_ABS: [
                (e.ABS_X, AbsInfo(0, 0, MAX_X, 4, 0, RES)),
                (e.ABS_Y, AbsInfo(0, 0, MAX_Y, 4, 0, RES)),
                (e.ABS_PRESSURE, AbsInfo(0, 0, MAX_PRESSURE, 0, 0, 0)),
                (e.ABS_DISTANCE, AbsInfo(0, 0, 63, 0, 0, 0)),
                (e.ABS_TILT_X, AbsInfo(0, -64, 63, 0, 0, 57)),
                (e.ABS_TILT_Y, AbsInfo(0, -64, 63, 0, 0, 57)),
                (e.ABS_MISC, AbsInfo(0, 0, 0xFFFFFFF, 0, 0, 0)),
            ],
            e.EV_MSC: [e.MSC_SERIAL],
        },
        name="Wacom Cintiq 22HD Pen",
        vendor=VENDOR,
        product=PRODUCT,
        version=0x110,
        bustype=e.BUS_USB,
        phys=PHYS,
        input_props=[e.INPUT_PROP_DIRECT],
    )


def pad_device():
    return UInput(
        {
            e.EV_KEY: PAD_BUTTONS + [e.BTN_STYLUS],
            e.EV_ABS: [
                (e.ABS_X, AbsInfo(0, 0, 1, 0, 0, 0)),
                (e.ABS_Y, AbsInfo(0, 0, 1, 0, 0, 0)),
                (e.ABS_RX, AbsInfo(0, 0, 4096, 0, 0, 0)),
                (e.ABS_RY, AbsInfo(0, 0, 4096, 0, 0, 0)),
                (e.ABS_MISC, AbsInfo(0, 0, 0xFFFFFFF, 0, 0, 0)),
            ],
        },
        name="Wacom Cintiq 22HD Pad",
        vendor=VENDOR,
        product=PRODUCT,
        version=0x110,
        bustype=e.BUS_USB,
        phys=PHYS,
    )


def keyboard_device():
    keys = [code for code in range(e.KEY_ESC, e.KEY_MICMUTE + 1)]
    return UInput({e.EV_KEY: keys}, name="Tuna D-TABLET keyboard", bustype=e.BUS_VIRTUAL)


class Tablet:
    def __init__(self):
        self.keyboard = keyboard_device()
        self.plug()

    def plug(self):
        self.pen = pen_device()
        self.pad = pad_device()
        self.tool = None

    def unplug(self):
        self.pen.close()
        self.pad.close()
        self.tool = None

    def pen_frame(self, events):
        for etype, code, value in events:
            self.pen.write(etype, code, value)
        if self.tool:
            self.pen.write(e.EV_MSC, e.MSC_SERIAL, SERIAL)
        self.pen.syn()

    def set_tool(self, tool):
        if self.tool == tool:
            return
        if self.tool:
            key = e.BTN_TOOL_PEN if self.tool == "pen" else e.BTN_TOOL_RUBBER
            self.pen_frame([(e.EV_ABS, e.ABS_PRESSURE, 0), (e.EV_ABS, e.ABS_MISC, 0), (e.EV_KEY, key, 0)])
            self.tool = None
            time.sleep(0.05)
        if tool in TOOL_ID:
            key = e.BTN_TOOL_PEN if tool == "pen" else e.BTN_TOOL_RUBBER
            self.tool = tool
            self.pen_frame(
                [
                    (e.EV_ABS, e.ABS_DISTANCE, 20),
                    (e.EV_ABS, e.ABS_MISC, TOOL_ID[tool]),
                    (e.EV_KEY, key, 1),
                ]
            )

    def move(self, x, y, pressure, tilt_x=0, tilt_y=0, distance=10):
        if not self.tool:
            self.set_tool("pen")
        self.pen_frame(
            [
                (e.EV_ABS, e.ABS_X, round(float(x) * MAX_X)),
                (e.EV_ABS, e.ABS_Y, round(float(y) * MAX_Y)),
                (e.EV_ABS, e.ABS_PRESSURE, round(float(pressure) * MAX_PRESSURE)),
                (e.EV_ABS, e.ABS_TILT_X, int(tilt_x)),
                (e.EV_ABS, e.ABS_TILT_Y, int(tilt_y)),
                (e.EV_ABS, e.ABS_DISTANCE, int(distance)),
            ]
        )

    def pad_frame(self, events):
        for etype, code, value in events:
            self.pad.write(etype, code, value)
        # The kernel's pad reports ABS_MISC with the pad id while in use.
        self.pad.write(e.EV_ABS, e.ABS_MISC, 15 if any(v for _, _, v in events) else 0)
        self.pad.syn()

    def run(self, words):
        cmd = words[0]
        if cmd == "pen":
            self.move(*words[1:])
        elif cmd == "tool":
            self.set_tool(words[1])
        elif cmd == "tip":
            self.pen_frame([(e.EV_KEY, e.BTN_TOUCH, int(words[1]))])
        elif cmd == "stylus":
            self.pen_frame([(e.EV_KEY, e.ecodes[words[1]], int(words[2]))])
        elif cmd == "pad":
            self.pad_frame([(e.EV_KEY, PAD_BUTTONS[int(words[1])], int(words[2]))])
        elif cmd == "strip":
            # The kernel reports a strip position as one set bit; 0 lifts.
            position = int(words[2])
            self.pad_frame([(e.EV_ABS, STRIPS[int(words[1])], 0 if position < 0 else 1 << position)])
        elif cmd == "key":
            code = e.ecodes[words[1]]
            self.keyboard.write(e.EV_KEY, code, 1)
            self.keyboard.syn()
            self.keyboard.write(e.EV_KEY, code, 0)
            self.keyboard.syn()
        elif cmd == "unplug":
            self.unplug()
        elif cmd == "plug":
            self.plug()
        else:
            raise ValueError(f"unknown command {cmd}")


def main():
    tablet = Tablet()
    print("ready", flush=True)
    for line in sys.stdin:
        words = line.split()
        if not words:
            continue
        if words[0] == "quit":
            break
        try:
            tablet.run(words)
            print("ok", flush=True)
        except Exception as error:  # reported to the driver, never fatal
            print(f"error {error}", flush=True)
    tablet.unplug()
    tablet.keyboard.close()


if __name__ == "__main__":
    main()
