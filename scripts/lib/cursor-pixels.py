#!/usr/bin/env python3
"""Assert a cursor's actual solid pixels and bounds in the X host capture."""
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


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("png", type=Path)
    parser.add_argument("side", type=int)
    parser.add_argument("hotspot_x", type=int)
    parser.add_argument("hotspot_y", type=int)
    parser.add_argument("rgb", help="comma-separated RGB bytes")
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
                receipt = verify(image, int(location["X"]), int(location["Y"]),
                                 args.side, [args.hotspot_x, args.hotspot_y], color)
            break
        except AssertionError:
            if time.monotonic() >= deadline:
                raise
            time.sleep(.05)
    args.png.with_suffix(".pixels.json").write_text(json.dumps(receipt, indent=2) + "\n")
    print(json.dumps(receipt))
