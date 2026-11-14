#!/usr/bin/env python3
"""Press one real key and time the panel focus indicator in X frames (#351).

Usage: tuna-focus-ring-timeline.py DISPLAY OUT.json SECONDS KEY
           NAME=X,Y,W,H [NAME=X,Y,W,H ...]

KEY goes to `xdotool key` (XTEST, so the nested compositor receives a
physical-looking key, never a synthetic shell state change). Each NAME
is a panel control's accessible rectangle in screen coordinates; only
its centre is trusted (GTK 4.14 offsets a padded button's accessible x
by its padding).

GNOME 51 marks a focused panel button (`.panel-button:focus`,
_drawing.scss panel_button) with the hover fill, white at 17% over the
black bar, and draws no ring. Each frame is read straight from the X
server; in the pills' middle band a column is fill when its darkest
pixel is that neutral grey (labels and icons only brighten it) and
content when brighter. A lit pill is a run of fill and content columns
with at least six fill columns (its padding), so antialiased label
edges on the black bar never count. A control is lit when a lit pill
covers its centre.

Frames are sampled every ~40 ms for SECONDS after the key. The JSON
records the lit pills per frame and, per control, `lit_before` (the
frame before the key), `lit_from` (first lit frame, seconds after the
key) and `lit_until` (first unlit frame after that, or null if it stayed
lit). The panel strip before the key, half a second after it, at the
first lit frame and at the first expired frame are saved next to
OUT.json as PNG evidence.
"""
import json
import pathlib
import subprocess
import sys
import time

from PIL import ImageGrab

PANEL_HEIGHT = 32


def fill(pixel):
    # 17% white over black is 43; Adwaita's blue ring or the bare bar is not.
    return 30 <= sum(pixel) / 3 <= 60 and max(pixel) - min(pixel) <= 12


def lit_pills(frame, band):
    pixels = frame.load()
    pills, run, fills = [], None, 0
    for x in range(frame.width):
        darkest = min((pixels[x, y] for y in band), key=sum)
        if fill(darkest) or sum(darkest) / 3 > 60:
            run = run or [x, x]
            run[1] = x
            fills += fill(darkest)
            continue
        if run and fills >= 6:
            pills.append(run)
        run, fills = None, 0
    if run and fills >= 6:
        pills.append(run)
    return pills


def main():
    display, out, seconds, key = sys.argv[1], pathlib.Path(sys.argv[2]), float(sys.argv[3]), sys.argv[4]
    rects = {}
    for arg in sys.argv[5:]:
        name, _, coords = arg.partition("=")
        rects[name] = [int(v) for v in coords.split(",")]
    width = max(x + w for x, _, w, _ in rects.values()) + 16
    _, y, _, h = next(iter(rects.values()))
    band = range(y + h // 2 - 3, y + h // 2 + 4)
    centres = {name: x + w // 2 for name, (x, _, w, _) in rects.items()}

    def grab():
        return ImageGrab.grab(bbox=(0, 0, width, PANEL_HEIGHT), xdisplay=display).convert("RGB")

    def lit(pills):
        return {name: any(a <= c <= b for a, b in pills) for name, c in centres.items()}

    before = grab()
    saved = {"before": before}
    subprocess.run(["xdotool", "key", key], check=True, timeout=5)
    start = time.monotonic()
    frames = []
    while (now := time.monotonic() - start) < seconds:
        frame = grab()
        pills = lit_pills(frame, band)
        frames.append({"t": round(now, 3), "pills": pills, "lit": lit(pills)})
        if pills and "lit" not in saved:
            saved["lit"] = frame
        if now >= 0.5 and "early" not in saved:
            saved["early"] = frame
        if not pills and "lit" in saved and "expired" not in saved:
            saved["expired"] = frame
        time.sleep(0.04)
    lit_before = lit(lit_pills(before, band))
    summary = {}
    for name in rects:
        lit_from = next((f["t"] for f in frames if f["lit"][name]), None)
        lit_until = None
        if lit_from is not None:
            lit_until = next((f["t"] for f in frames if f["t"] > lit_from and not f["lit"][name]), None)
        summary[name] = {"rect": rects[name], "lit_before": lit_before[name],
                         "lit_from": lit_from, "lit_until": lit_until}
    for label, frame in saved.items():
        frame.save(out.with_name(f"{out.stem}-{label}.png"))
    out.write_text(json.dumps({"key": key, "seconds": seconds, "controls": summary,
                               "frames": frames}, indent=1) + "\n")
    print(json.dumps({"key": key, "controls": summary}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
