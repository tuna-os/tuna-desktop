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

Frames are sampled with a 10 ms pause, only within SECONDS of `xdotool key`
returning. Monotonic command begin/end bound the command, not input delivery,
drawing or presentation. Every capture records begin/end relative to command
return, without rounding. A first-lit capture's END bounds onset above; the
last-lit capture's BEGIN and first-unlit capture's END conservatively bracket
expiry. Neither capture endpoint is a presentation timestamp. The panel strip
before the key, half a second after it, at the first lit frame and at the first
expired frame are saved next to OUT.json as PNG evidence.
"""
import json
import pathlib
import subprocess
import sys
import time


PANEL_HEIGHT = 32
SAMPLE_PAUSE_S = 0.01


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


def summarize_control(frames, name, rect, lit_before):
    first_lit = last_lit = first_unlit = None
    for frame in frames:
        bounds = {field: frame[field] for field in ("capture_begin_s", "capture_end_s")}
        if frame["lit"][name]:
            if first_lit is None:
                first_lit = bounds
            last_lit = bounds
        elif first_lit is not None:
            first_unlit = bounds
            break
    return {"rect": rect, "lit_before": lit_before, "first_lit": first_lit,
            "last_lit": last_lit, "first_unlit": first_unlit}


def main():
    from PIL import ImageGrab

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
        begin = time.monotonic()
        frame = ImageGrab.grab(bbox=(0, 0, width, PANEL_HEIGHT), xdisplay=display)
        end = time.monotonic()
        return frame.convert("RGB"), begin, end

    def lit(pills):
        return {name: any(a <= c <= b for a, b in pills) for name, c in centres.items()}

    before, before_begin, before_end = grab()
    saved = {"before": before}
    command_begin = time.monotonic()
    subprocess.run(["xdotool", "key", key], check=True, timeout=5)
    command_end = time.monotonic()
    origin = command_end
    deadline = origin + seconds
    frames = []
    while time.monotonic() < deadline:
        frame, capture_begin, capture_end = grab()
        # A capture that finishes outside the original window cannot qualify.
        if capture_end > deadline:
            break
        pills = lit_pills(frame, band)
        frames.append({"capture_begin_s": capture_begin - origin,
                       "capture_end_s": capture_end - origin,
                       "pills": pills, "lit": lit(pills)})
        if pills and "lit" not in saved:
            saved["lit"] = frame
        if capture_end - origin >= 0.5 and "early" not in saved:
            saved["early"] = frame
        if not pills and "lit" in saved and "expired" not in saved:
            saved["expired"] = frame
        remaining = deadline - time.monotonic()
        if remaining > 0:
            time.sleep(min(SAMPLE_PAUSE_S, remaining))
    lit_before = lit(lit_pills(before, band))
    summary = {name: summarize_control(frames, name, rect, lit_before[name])
               for name, rect in rects.items()}
    for label, frame in saved.items():
        frame.save(out.with_name(f"{out.stem}-{label}.png"))
    out.write_text(json.dumps({"key": key, "seconds": seconds, "clock": "monotonic",
                               "origin": "xdotool-command-return",
                               "origin_monotonic_s": origin,
                               "command": {"begin_monotonic_s": command_begin,
                                           "end_monotonic_s": command_end},
                               "before_capture": {"capture_begin_s": before_begin - origin,
                                                  "capture_end_s": before_end - origin},
                               "sample_pause_s": SAMPLE_PAUSE_S,
                               "controls": summary, "frames": frames}, indent=1) + "\n")
    print(json.dumps({"key": key, "controls": summary}))
    return 0


if __name__ == "__main__":
    sys.exit(main())
