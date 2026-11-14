#!/usr/bin/env python3
"""Host pixels of one visual-bell case (G-VISUAL-BELL, G-X11-BELL).

Usage: tuna-bell-pixels.py OUT_DIR CASE FIFO LINE ORIGIN RECT EXPECT [--top N]

ORIGIN "x,y" is the nested output's top-left on the host X display (the
nested compositor's window), RECT "x,y,w,h" the ringing window in output
coordinates. The case grabs a baseline, writes LINE to the bell client's
FIFO (one real ring), then grabs the host display as fast as it can for
700 ms. Rows above --top (the black panel, which a black flash cannot
change, and its clock) are ignored.

EXPECT is one of:
  frame  a flash darkens the window's rectangle and nothing outside it
  full   a flash darkens the whole output below the panel
  none   no pixel changes at all (visual bell off)
Every case must end on the baseline pixels again (Mutter removes the
flash after its last 50 ms leg). Writes OUT_DIR/CASE.json and the
baseline/peak/last PNGs, exits 1 on a failed expectation.
"""
import json
import os
import sys
import time
from pathlib import Path

from PIL import Image, ImageChops, ImageGrab

THRESHOLD = 24
WINDOW_MS = 700


def main():
    args = sys.argv[1:]
    top = 0
    if '--top' in args:
        at = args.index('--top')
        top = int(args[at + 1])
        del args[at:at + 2]
    out, case, fifo, line, origin, rect, expect = args
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    ox, oy = (int(v) for v in origin.split(','))
    rx, ry, rw, rh = (int(v) for v in rect.split(','))
    width = int(os.environ.get('WIDTH', '1280'))
    height = int(os.environ.get('HEIGHT', '800'))
    box = (ox, oy + top, ox + width, oy + height)
    display = os.environ['DISPLAY']

    def grab():
        return ImageGrab.grab(bbox=box, xdisplay=display).convert('RGB')

    baseline = grab()
    frames = []
    with open(fifo, 'w') as control:
        control.write(line + '\n')
    rung = time.monotonic()
    while time.monotonic() - rung < WINDOW_MS / 1000:
        frames.append((time.monotonic() - rung, grab()))
    # Let any last leg finish, then the settled frame.
    time.sleep(0.3)
    settled = grab()

    def changed(image):
        diff = ImageChops.difference(baseline, image).convert('L')
        return diff.point(lambda v: 255 if v > THRESHOLD else 0)

    darkest = None
    report_frames = []
    union = None
    for at, image in frames:
        mask = changed(image)
        bbox = mask.getbbox()
        count = sum(mask.histogram()[255:])
        report_frames.append({'ms': round(at * 1000, 1), 'changed': count,
                              'bbox': [bbox[0], bbox[1] + top, bbox[2], bbox[3] + top] if bbox else None})
        if bbox:
            union = bbox if union is None else (min(union[0], bbox[0]), min(union[1], bbox[1]),
                                                max(union[2], bbox[2]), max(union[3], bbox[3]))
        if darkest is None or count > darkest[0]:
            darkest = (count, image)
    settled_bbox = changed(settled).getbbox()
    # The flash is black: a pixel already near black (another window's dark
    # content) cannot change by THRESHOLD, so coverage counts only pixels
    # bright enough to show half their level going.
    darkenable = baseline.convert('L').point(lambda v: 255 if v > 2 * THRESHOLD else 0)
    # The window's rectangle in grabbed coordinates (rows above top cut).
    win = (rx, max(ry, top) - top, rx + rw, ry + rh - top)

    def bright(region=None):
        mask = darkenable.crop(region) if region else darkenable
        return sum(mask.histogram()[255:])
    area = bright()
    win_area = bright(win) if win[2] > win[0] and win[3] > win[1] else 0
    peak = darkest[0] if darkest else 0
    flashed = sum(1 for f in report_frames if f['changed'])
    failures = []
    if settled_bbox:
        failures.append(f'pixels did not return to the baseline: {settled_bbox}')
    if expect == 'none':
        if union:
            failures.append(f'pixels changed with the visual bell off: {union}')
    elif not union:
        failures.append('no grabbed frame showed a flash')
    elif expect == 'frame':
        slack = 2
        if (union[0] < win[0] - slack or union[1] < win[1] - slack
                or union[2] > win[2] + slack or union[3] > win[3] + slack):
            failures.append(f'the frame flash left the window {win}: {union}')
        if peak < 0.9 * win_area:
            failures.append(f'the frame flash covered {peak} of {win_area} window pixels')
    elif expect == 'full':
        if peak < 0.9 * area:
            failures.append(f'the full-screen flash covered {peak} of {area} pixels')
    else:
        failures.append(f'unknown expectation {expect}')
    if darkest:
        darkest[1].save(out / f'{case}-peak.png')
        # Mutter's flash is black: the window's centre must get darker.
        cx, cy = (win[0] + win[2]) // 2, (win[1] + win[3]) // 2
        before = baseline.getpixel((cx, cy))
        after = darkest[1].getpixel((cx, cy))
        if expect != 'none' and not all(a < b for a, b in zip(after, before)):
            failures.append(f'the flash did not darken {before} at the window centre: {after}')
    baseline.save(out / f'{case}-baseline.png')
    settled.save(out / f'{case}-settled.png')
    report = {'case': case, 'line': line, 'expect': expect, 'origin': [ox, oy], 'top': top,
              'window_rect': [rx, ry, rw, rh], 'frames': report_frames,
              'flashed_frames': flashed, 'union': list(union) if union else None,
              'peak_changed': peak, 'settled_bbox': settled_bbox, 'failures': failures}
    (out / f'{case}.json').write_text(json.dumps(report, indent=1) + '\n')
    if failures:
        print(f'{case}: ' + '; '.join(failures), file=sys.stderr)
        sys.exit(1)
    print(f'{case}: {expect} ok, {flashed}/{len(report_frames)} frames flashed, peak {peak}')


if __name__ == '__main__':
    main()
