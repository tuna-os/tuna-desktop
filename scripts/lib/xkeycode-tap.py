#!/usr/bin/python3
"""Tap one X key by raw keycode through XTEST, with no keysym lookup.

xdotool(1) `key F1` resolves function keys behind an Alt level on the
proof server's keymap and taps Alt around every press, which poisons a
Super+F1 cycle chord (the compositor logs a near-miss with mods=72 and
no cycle happens). Pressing the level-0 keycode directly keeps the chord
to exactly the modifiers the harness holds down itself.

Usage: xkeycode-tap.py DISPLAY KEYSYM_HEX (e.g. :96 ffbe), or
       xkeycode-tap.py DISPLAY --keycode DECIMAL_CODE (8..255).
The explicit physical mode never resolves or synthesizes host modifiers.
Exits nonzero when the display cannot be opened, no level-0 keycode
carries the keysym, or the XTEST tap fails.
"""
import ctypes
import sys
import time


def parse_request(args):
    if len(args) == 3 and args[1] == '--keycode':
        if not args[2].isascii() or not args[2].isdecimal():
            raise ValueError('physical keycode must be decimal')
        code = int(args[2])
        if not 8 <= code <= 255:
            raise ValueError('physical keycode must be in 8..255')
        return args[0].encode(), None, code
    if len(args) == 2:
        return args[0].encode(), int(args[1], 16), None
    raise ValueError('usage: xkeycode-tap.py DISPLAY KEYSYM_HEX or DISPLAY --keycode CODE')


def tap_key(x11, xtst, dpy, code):
    if not xtst.XTestFakeKeyEvent(dpy, code, 1, 0):
        return False
    x11.XSync(dpy, 0)
    time.sleep(0.05)
    released = bool(xtst.XTestFakeKeyEvent(dpy, code, 0, 0))
    x11.XSync(dpy, 0)
    return released


def main():
    try:
        display, want, physical = parse_request(sys.argv[1:])
    except ValueError as error:
        print(error, file=sys.stderr)
        return 2
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
        code = physical
        if code is None:
            code = next((candidate for candidate in range(8, 256)
                         if x11.XKeycodeToKeysym(dpy, candidate, 0) == want), None)
        if code is None:
            print("keysym 0x%04x has no level-0 keycode" % want, file=sys.stderr)
            return 1
        if not tap_key(x11, xtst, dpy, code):
            print("XTEST tap for keycode %d failed" % code, file=sys.stderr)
            return 1
    finally:
        x11.XCloseDisplay(dpy)
    return 0


if __name__ == "__main__":
    sys.exit(main())
