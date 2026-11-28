#!/usr/bin/env python3
"""Assert locate-pointer ripples' actual pixels in X host captures.

GNOME Shell 51's locatePointer.js plays ripples.js's three rings of
`.ripple-pointer-location` (a 50 px disc with a 2 px glow, the accent
lightened) centred on the pointer. Given a capture BEFORE the tap and one
DURING the ripples at a frozen animation time of 415 ms, the outermost
changed pixel along each of the four rays from the centre must sit at
the largest ring's radius (32.0625 logical pixels times SCALE, within
TOLERANCE), the disc must move the scene towards its fill, and nothing
may change beyond it. With --settled, a capture after the ripples ended
must match BEFORE.
"""
import argparse
import colorsys
import json
from pathlib import Path
from PIL import Image

# A channel moving by this much is the ripple, not capture noise.
CHANGED = 6
# GNOME's default accent (blue, #3584e4).
ACCENT = (0x35, 0x84, 0xe4)
# ripples.js: at 415/830 of the first ring's duration, EASE_OUT_QUAD
# has progressed 0.75 of the way from scale 0.25 to 1.5, not to scale
# 1.375. Include the disc's 2 px glow on each side in the base radius.
RADIUS_AT_415_MS = (50 + 2 * 2) / 2 * (
    0.25 + (1.5 - 0.25) * (1 - (1 - 415 / 830) ** 2))


def lighten(rgb, amount):
    """Sass's lighten(), as cursor.rs's: HSL lightness plus AMOUNT."""
    h, l, s = colorsys.rgb_to_hls(*(c / 255 for c in rgb))
    return tuple(round(c * 255) for c in colorsys.hls_to_rgb(h, min(1.0, l + amount), s))


def distance(a, b):
    return sum((x - y) ** 2 for x, y in zip(a, b))


def changed(a, b):
    return max(abs(x - y) for x, y in zip(a, b)) >= CHANGED


def reach_of(radius, tolerance):
    return int(radius + tolerance + 12)


def verify(before, during, cx, cy, radius, tolerance):
    reach = reach_of(radius, tolerance)
    assert cx - reach >= 0 and cy - reach >= 0
    assert cx + reach < before.width and cy + reach < before.height
    edges = {}
    for name, (dx, dy) in {"left": (-1, 0), "right": (1, 0), "up": (0, -1), "down": (0, 1)}.items():
        moved = [step for step in range(reach + 1)
                 if changed(before.getpixel((cx + dx * step, cy + dy * step)),
                            during.getpixel((cx + dx * step, cy + dy * step)))]
        assert moved, f"nothing changed along the {name} ray"
        edges[name] = max(moved) + 1
        assert abs(edges[name] - radius) <= tolerance, (name, edges[name], radius)
    # Half way out on the left and up rays (clear of the cursor, which
    # hangs down-right of the hotspot) the disc moves the scene towards
    # its fill, the accent lightened by 30%.
    fill = lighten(ACCENT, 0.3)
    for dx, dy in ((-1, 0), (0, -1)):
        x, y = cx + dx * int(radius / 2), cy + dy * int(radius / 2)
        b, d = before.getpixel((x, y)), during.getpixel((x, y))
        assert distance(d, fill) < distance(b, fill), ("not towards the lightened accent", (x, y), b, d, fill)
    return edges


def settled(before, after, cx, cy, radius, tolerance):
    reach = reach_of(radius, tolerance)
    for y in range(cy - reach, cy + reach + 1):
        for x in range(cx - reach, cx + reach + 1):
            assert not changed(before.getpixel((x, y)), after.getpixel((x, y))), ("residue", (x, y))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before", type=Path)
    parser.add_argument("during", type=Path)
    parser.add_argument("cx", type=int)
    parser.add_argument("cy", type=int)
    parser.add_argument("scale", type=float, help="physical pixels per logical pixel")
    parser.add_argument("tolerance", type=float)
    parser.add_argument("--settled", type=Path)
    args = parser.parse_args()
    radius = RADIUS_AT_415_MS * args.scale
    with Image.open(args.before) as b, Image.open(args.during) as d:
        before, during = b.convert("RGB"), d.convert("RGB")
    receipt = {"centre": [args.cx, args.cy], "radius": radius, "tolerance": args.tolerance,
               "edges": verify(before, during, args.cx, args.cy, radius, args.tolerance)}
    if args.settled:
        with Image.open(args.settled) as s:
            settled(before, s.convert("RGB"), args.cx, args.cy, radius, args.tolerance)
        receipt["settled"] = True
    args.during.with_suffix(".ripple.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt))
