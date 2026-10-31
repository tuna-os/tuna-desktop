#!/usr/bin/env python3
"""Assert a cursor's actual solid pixels and bounds in the X host capture.

SIDE and the hotspot are physical pixels. At an integer scale the drawn
square is exact. With --tolerance N (fractional scales, where Mutter 51's
cursor renderer downscales the ceiled-scale image) its edges may blend
into the background: the solid square's bounds may then sit up to N
pixels off, and only pixels at least N inside it must be the colour.
"""
import argparse
import json
from pathlib import Path
import subprocess
import time
from PIL import Image


def verify(image, x, y, side, hotspot, color):
    left, top = x - hotspot[0], y - hotspot[1]
    assert side > 0 and left >= 2 and top >= 2
    assert left + side + 2 <= image.width and top + side + 2 <= image.height
    region = image.crop((left - 2, top - 2, left + side + 2, top + side + 2)).convert("RGB")
    matches = [(px, py) for py in range(region.height) for px in range(region.width)
               if all(abs(a - b) <= 1 for a, b in zip(region.getpixel((px, py)), color))]
    assert len(matches) == side * side, (len(matches), side * side)
    assert (min(px for px, _ in matches), min(py for _, py in matches),
            max(px for px, _ in matches), max(py for _, py in matches)) == (2, 2, side + 1, side + 1)
    return {"pointer_root": [x, y], "cursor_root": [left, top], "side": side,
            "hotspot": hotspot, "rgb": color, "matching_pixels": len(matches)}


def verify_scaled(image, x, y, side, hotspot, color, tolerance):
    """The solid square of SIDE at the hotspot, edges within TOLERANCE."""
    left, top = round(x - hotspot[0]), round(y - hotspot[1])
    margin = tolerance + 2
    assert side > 2 * tolerance and left >= margin and top >= margin
    assert left + side + margin <= image.width and top + side + margin <= image.height
    origin = (left - margin, top - margin)
    region = image.crop((origin[0], origin[1], left + side + margin, top + side + margin)).convert("RGB")
    matches = [(px, py) for py in range(region.height) for px in range(region.width)
               if all(abs(a - b) <= 1 for a, b in zip(region.getpixel((px, py)), color))]
    assert matches, "no pixel of the cursor colour"
    box = (min(px for px, _ in matches), min(py for _, py in matches),
           max(px for px, _ in matches) + 1, max(py for _, py in matches) + 1)
    expected = (margin, margin, margin + side, margin + side)
    assert all(abs(a - b) <= tolerance for a, b in zip(box, expected)), (box, expected)
    inner = [(px, py) for py in range(box[1] + tolerance, box[3] - tolerance)
             for px in range(box[0] + tolerance, box[2] - tolerance)]
    solid = set(matches)
    stray = [p for p in inner if p not in solid]
    assert inner and not stray, ("interior not solid", stray[:5])
    return {"pointer_root": [x, y], "cursor_root": [origin[0] + box[0], origin[1] + box[1]],
            "drawn": [box[2] - box[0], box[3] - box[1]], "side": side, "hotspot": hotspot,
            "rgb": color, "tolerance": tolerance, "matching_pixels": len(matches)}


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("png", type=Path)
    parser.add_argument("side", type=int)
    parser.add_argument("hotspot_x", type=float)
    parser.add_argument("hotspot_y", type=float)
    parser.add_argument("rgb", help="comma-separated RGB bytes")
    parser.add_argument("--tolerance", type=int, default=0)
    args = parser.parse_args()
    color = [int(v) for v in args.rgb.split(",")]
    assert len(color) == 3 and all(0 <= v <= 255 for v in color)
    location = dict(line.split("=", 1) for line in subprocess.check_output(
        ["xdotool", "getmouselocation", "--shell"], text=True, timeout=5).splitlines())
    deadline = time.monotonic() + 10
    while True:
        subprocess.run(["scrot", "-o", str(args.png)], check=True, timeout=5)
        try:
            with Image.open(args.png) as image:
                x, y = int(location["X"]), int(location["Y"])
                if args.tolerance:
                    receipt = verify_scaled(image, x, y, args.side, [args.hotspot_x, args.hotspot_y],
                                            color, args.tolerance)
                else:
                    assert args.hotspot_x.is_integer() and args.hotspot_y.is_integer()
                    receipt = verify(image, x, y, args.side,
                                     [int(args.hotspot_x), int(args.hotspot_y)], color)
            break
        except AssertionError:
            if time.monotonic() >= deadline:
                raise
            time.sleep(.05)
    args.png.with_suffix(".pixels.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt))
