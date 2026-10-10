#!/usr/bin/python3
"""Tap one X key by raw keycode through XTEST, with no keysym lookup.

xdotool(1) `key F1` resolves function keys behind an Alt level on the
proof server's keymap and taps Alt around every press, which poisons a
Super+F1 cycle chord (the compositor logs a near-miss with mods=72 and
no cycle happens). Pressing the level-0 keycode directly keeps the chord
to exactly the modifiers the harness holds down itself.

Usage: xkeycode-tap.py DISPLAY KEYSYM_HEX (e.g. :96 ffbe).
Or DISPLAY --physical=CODE tap|press|release|probe to keep the same physical
XTEST key during compositor-only remapping. Probe reports the host's level-0
keysym without injecting input; this never changes the host X keymap.
Exits nonzero when the display cannot be opened, no level-0 keycode
carries the keysym, or the XTEST tap fails.
"""
import ctypes
import sys
import time


def arguments(values):
    if len(values) == 2:
        display, symbol = values
        try:
            want = int(symbol, 16)
        except ValueError:
            raise ValueError('bad keysym') from None
        if not 0 < want <= 0xffffffff:
            raise ValueError('keysym out of range')
        return display, None, want, 'tap'
    if len(values) == 3 and values[1].startswith('--physical='):
        try:
            code = int(values[1].split('=', 1)[1], 10)
        except ValueError:
            raise ValueError('bad physical X keycode') from None
        if not 8 <= code <= 255 or values[2] not in ('tap', 'press', 'release', 'probe'):
            raise ValueError('physical keycode/action out of range')
        return values[0], code, None, values[2]
    raise ValueError('usage: xkeycode-tap.py DISPLAY KEYSYM_HEX | DISPLAY --physical=CODE tap|press|release|probe')


def main():
    try:
        display, code, want, action = arguments(sys.argv[1:])
    except ValueError as error:
        print(error, file=sys.stderr)
        return 2
    display = display.encode()
    x11 = ctypes.CDLL("libX11.so.6")
    x11.XOpenDisplay.restype = ctypes.c_void_p
    x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
    x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
    x11.XKeycodeToKeysym.restype = ctypes.c_ulong
    x11.XKeycodeToKeysym.argtypes = [ctypes.c_void_p, ctypes.c_ubyte, ctypes.c_int]
    x11.XSync.argtypes = [ctypes.c_void_p, ctypes.c_int]
    xtst = ctypes.CDLL("libXtst.so.6")
    xtst.XTestFakeKeyEvent.restype = ctypes.c_int
    xtst.XTestFakeKeyEvent.argtypes = [
        ctypes.c_void_p,
        ctypes.c_uint,
        ctypes.c_int,
        ctypes.c_ulong,
    ]
    dpy = x11.XOpenDisplay(display)
    if not dpy:
        print("cannot open display %s" % sys.argv[1], file=sys.stderr)
        return 1
    try:
        if code is None:
            code = 0
            for candidate in range(8, 256):
                if x11.XKeycodeToKeysym(dpy, candidate, 0) == want:
                    code = candidate
                    break
        if not code:
            print(
                "keysym 0x%04x has no level-0 keycode" % want, file=sys.stderr
            )
            return 1
        if action == 'probe':
            print(hex(x11.XKeycodeToKeysym(dpy, code, 0)))
            return 0
        events = {'tap': [1, 0], 'press': [1], 'release': [0]}[action]
        for pressed in events:
            if not xtst.XTestFakeKeyEvent(dpy, code, pressed, 0):
                print("XTEST event for keycode %d failed" % code, file=sys.stderr)
                return 1
            x11.XSync(dpy, 0)
            if pressed and action == 'tap':
                time.sleep(0.05)
    finally:
        x11.XCloseDisplay(dpy)
    return 0


if __name__ == "__main__":
    sys.exit(main())
