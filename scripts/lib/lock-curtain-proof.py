#!/usr/bin/env python3
"""Check the unlock dialog's traced curtain against GNOME 51.

The GTK shell (TUNA_TRANSIENT_TRACE) logs every ease, frame, swipe and
settle of the lock screen's curtain as `tuna-shell-gtk: unlock-dialog ...`
lines; tuna-gtk-shell-proof drives it with real keys and pointer drags on
the frozen animation clock. This recomputes, independently of the shell,
what gnome-shell 51.0 does (js/ui/unlockDialog.js `_setTransitionProgress`,
js/ui/swipeTracker.js `_getEndProgress`/`_endGesture`) and compares.

  lock-curtain-proof.py line LOG START TEXT     a trace line after START
  lock-curtain-proof.py frame LOG START P MOTION the last frame is at P
  lock-curtain-proof.py distance LOG START      the drag's page distance
  lock-curtain-proof.py follow LOG START TRAVEL STEP
                                                a held drag moved the
                                                curtain with the pointer
  lock-curtain-proof.py release LOG START       the last release matches
                                                GNOME; prints END DURATION
                                                PROGRESS VELOCITY
  lock-curtain-proof.py eased LOG START ELAPSED MOTION
                                                ELAPSED ms after that
                                                release, the frame is on
                                                ease-out cubic

START is a 1-based log line; only lines from it on count. Commands wait
up to --timeout seconds for the shell to log what they check.
"""
import argparse
import math
import re
import sys
import time

PREFIX = "tuna-shell-gtk: unlock-dialog "
CROSSFADE_TIME = 300
FADE_OUT_TRANSLATION = 200
FADE_OUT_SCALE = 0.3
MIN_ANIMATION_DURATION = 100
MAX_ANIMATION_DURATION = 400
VELOCITY_THRESHOLD_TOUCH = 0.3
DECELERATION_TOUCH = 0.998
VELOCITY_CURVE_THRESHOLD = 2
DECELERATION_PARABOLA_MULTIPLIER = 0.35
DURATION_MULTIPLIER = 3
ANIMATION_BASE_VELOCITY = 0.002
# Frames print opacity and scale to 3 places, offsets to 1, progress to 4.
TOLERANCE = {"progress": 0.00015, "opacity": 0.0015, "dy": 0.15, "scale": 0.0015}

FRAME = re.compile(
    r"frame progress=(?P<progress>[-\d.]+) motion=(?P<motion>true|false) "
    r"clock=(?P<clock>[-\d.,]+) prompt=(?P<prompt>[-\d.,]+)$")
FIELD = re.compile(r"(\w+)=(\S+)")


def lerp(a, b, t):
    return a + (b - a) * t


def ease_out_cubic(t):
    t = min(max(t, 0.0), 1.0) - 1.0
    return t * t * t + 1.0


def frames(progress, motion):
    """`_setTransitionProgress`: (opacity, translation_y, scale) of the
    clock and the prompt."""
    p = min(max(progress, 0.0), 1.0)
    if motion:
        clock = (1 - p, -FADE_OUT_TRANSLATION * p, lerp(FADE_OUT_SCALE, 1, 1 - p))
        prompt = (p, FADE_OUT_TRANSLATION * (1 - p), lerp(FADE_OUT_SCALE, 1, p))
    else:
        clock, prompt = (1 - p, 0.0, 1.0), (p, 0.0, 1.0)
    return clock, prompt


def closest(pos):
    """`_findClosestPoint` over [0, 1]; a tie is the first point."""
    return 1 if abs(1 - pos) < abs(pos) else 0


def release(progress, initial, cancel, velocity, distance, cancelled):
    """swipeTracker.js `_getEndProgress` and `_endGesture` for a pan
    (isTouchpad false) over snap points [0, 1]: (end progress, duration
    in ms)."""
    if cancelled:
        end = cancel
    else:
        if abs(velocity) < VELOCITY_THRESHOLD_TOUCH:
            end = closest(progress)
        else:
            slope = DECELERATION_TOUCH / (1.0 - DECELERATION_TOUCH) / 1000.0
            if abs(velocity) > VELOCITY_CURVE_THRESHOLD:
                c = slope / 2 / DECELERATION_PARABOLA_MULTIPLIER
                x = abs(velocity) - VELOCITY_CURVE_THRESHOLD + c
                pos = (slope * VELOCITY_CURVE_THRESHOLD
                       + DECELERATION_PARABOLA_MULTIPLIER * x * x
                       - DECELERATION_PARABOLA_MULTIPLIER * c * c)
            else:
                pos = abs(velocity) * slope
            pos = min(max(pos * math.copysign(1, velocity) + progress, 0.0), 1.0)
            start = closest(initial)
            prev = 1 if pos >= 1 else 0
            nxt = 0 if pos <= 0 else 1
            if (prev if velocity > 0 else nxt) == start:
                end = nxt if velocity > 0 else prev
            else:
                end = closest(pos)
    velocity /= max(distance, 1.0)
    if (end - progress) * velocity <= 0:
        velocity = ANIMATION_BASE_VELOCITY
    points = max(1, math.ceil(abs(progress - end)))
    longest = MAX_ANIMATION_DURATION * math.log2(1 + points)
    duration = abs((progress - end) / velocity * DURATION_MULTIPLIER)
    if duration > 0:
        duration = min(max(duration, MIN_ANIMATION_DURATION), longest)
    return end, duration


def trace(log, start):
    with open(log, encoding="utf-8", errors="replace") as fh:
        lines = fh.read().splitlines()[max(start, 1) - 1:]
    return [line.split(PREFIX, 1)[1] for line in lines if PREFIX in line]


def parse_frame(text):
    match = FRAME.match(text)
    if not match:
        return None
    clock = tuple(float(v) for v in match["clock"].split(","))
    prompt = tuple(float(v) for v in match["prompt"].split(","))
    return float(match["progress"]), match["motion"] == "true", clock, prompt


def frame_matches(frame, progress, motion):
    got_progress, got_motion, clock, prompt = frame
    want_clock, want_prompt = frames(progress, motion)
    if got_motion != motion or abs(got_progress - progress) > TOLERANCE["progress"]:
        return False
    for got, want in ((clock, want_clock), (prompt, want_prompt)):
        for value, expected, kind in zip(got, want, ("opacity", "dy", "scale")):
            if abs(value - expected) > TOLERANCE[kind]:
                return False
    return True


def fields(text):
    return dict(FIELD.findall(text))


def last(entries, prefix):
    found = [entry for entry in entries if entry.startswith(prefix)]
    return found[-1] if found else None


def wait(timeout, check):
    """Poll `check` until it returns a non-None value or time runs out."""
    deadline = time.monotonic() + timeout
    while True:
        result = check()
        if result is not None or time.monotonic() >= deadline:
            return result
        time.sleep(0.1)


def swipe_end(entries):
    text = last(entries, "swipe end ")
    if text is None:
        return None
    f = fields(text)
    return {
        "progress": float(f["progress"]),
        "initial": float(f["initial"]),
        "velocity": float(f["velocity"]),
        "distance": float(f["distance"]),
        "cancelled": f["cancelled"] == "true",
        "end": int(f["end"]),
        "duration": float(f["duration"]),
    }


def check_release(entry):
    # `Math.round` (half up), not Python's half-to-even.
    cancel = math.floor(entry["initial"] + 0.5)
    end, duration = release(entry["progress"], entry["initial"], cancel, entry["velocity"],
                            entry["distance"], entry["cancelled"])
    if end != entry["end"] or abs(duration - entry["duration"]) > 0.5:
        raise SystemExit(f"release {entry} differs from GNOME 51: end={end} duration={duration:.3f}")
    return end, duration


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("command", choices=["line", "frame", "distance", "follow", "release", "eased"])
    ap.add_argument("log")
    ap.add_argument("start", type=int)
    ap.add_argument("args", nargs="*")
    ap.add_argument("--timeout", type=float, default=10.0)
    a = ap.parse_args(argv)
    entries = lambda: trace(a.log, a.start)  # noqa: E731

    if a.command == "line":
        text = " ".join(a.args)
        if wait(a.timeout, lambda: True if any(text in e for e in entries()) else None) is None:
            raise SystemExit(f"no unlock-dialog line '{text}' (last: {entries()[-3:]})")
        return 0

    if a.command == "frame":
        progress, motion = float(a.args[0]), a.args[1] == "true"

        def matched():
            frame = parse_frame(last(entries(), "frame ") or "")
            return True if frame and frame_matches(frame, progress, motion) else None
        if wait(a.timeout, matched) is None:
            raise SystemExit(f"last frame {last(entries(), 'frame ')} is not GNOME's at "
                             f"progress {progress} motion {motion}: {frames(progress, motion)}")
        return 0

    if a.command == "distance":
        text = wait(a.timeout, lambda: last(entries(), "swipe begin "))
        if text is None:
            raise SystemExit("no swipe began")
        print(int(float(fields(text)["distance"])))
        return 0

    if a.command == "follow":
        travel, step = float(a.args[0]), float(a.args[1])
        begin = wait(a.timeout, lambda: last(entries(), "swipe begin "))
        if begin is None:
            raise SystemExit("the drag never began a swipe")
        start = fields(begin)
        distance, initial = float(start["distance"]), float(start["progress"])

        def followed():
            frame = parse_frame(last(entries(), "frame ") or "")
            if frame is None:
                return None
            moved = abs(frame[0] - initial) * distance
            # The threshold travel (under one step) moves nothing; GTK may
            # merge events, so the last step can count whole.
            if travel - step - 1 <= moved <= travel + 1 and 0 < frame[0] < 1 \
                    and frame_matches(frame, frame[0], frame[1]):
                return frame
            return None
        frame = wait(a.timeout, followed)
        if frame is None:
            raise SystemExit(f"curtain at {last(entries(), 'frame ')} after {travel} px "
                             f"over a {distance} px page from {initial}")
        print(f"{frame[0]:.4f}")
        return 0

    if a.command == "release":
        entry = wait(a.timeout, lambda: swipe_end(entries()))
        if entry is None:
            raise SystemExit("no swipe ended")
        end, duration = check_release(entry)
        print(f"{end} {duration:.3f} {entry['progress']:.6f} {entry['velocity']:.6f}")
        return 0

    elapsed, motion = float(a.args[0]), a.args[1] == "true"
    entry = swipe_end(entries())
    if entry is None:
        raise SystemExit("no swipe ended")
    end, duration = check_release(entry)
    t = 1.0 if duration <= 0 else min(elapsed / duration, 1.0)
    progress = lerp(entry["progress"], end, ease_out_cubic(t))

    def matched():
        frame = parse_frame(last(entries(), "frame ") or "")
        return True if frame and frame_matches(frame, progress, motion) else None
    if wait(a.timeout, matched) is None:
        raise SystemExit(f"{elapsed} ms into the release ease the frame is "
                         f"{last(entries(), 'frame ')}, not ease-out cubic {progress:.4f}")
    print(f"{progress:.4f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
