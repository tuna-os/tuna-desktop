#!/usr/bin/env python3
"""Check the workspace switcher popup and the Activities dots against GNOME 51.

The GTK shell (TUNA_TRANSIENT_TRACE) logs every display, fade, changed
frame and hide of the workspace switcher popup (`tuna-shell-gtk: ws-popup
...`) and every set, swipe phase and changed frame of the Activities dots
(`tuna-shell-gtk: ws-dots ...`); tuna-gtk-shell-proof drives them with real
keys and synthetic three-finger swipes on the frozen animation clock. This
recomputes, independently of the shell, what gnome-shell 51.0 draws
(js/ui/workspaceSwitcherPopup.js:10-11,93-121 for the popup;
js/ui/panel.js:44-196 `WorkspaceDot`/`WorkspaceIndicators` for the dots,
whose value is the swipe progress, js/ui/workspaceAnimation.js:255-268)
and compares.

  ws-anim-proof.py line LOG START TEXT     a trace line containing TEXT
  ws-anim-proof.py absent LOG START TEXT   no such line within --timeout
  ws-anim-proof.py popup LOG START OPACITY the popup's latest frame
  ws-anim-proof.py dots LOG START POSITION the dots' latest frame is
                                           GNOME's at adjustment POSITION
  ws-anim-proof.py release LOG START       prints the last swipe release's
                                           POSITION DURATION and the held
                                           position it starts from
  ws-anim-proof.py eased LOG START ELAPSED the dots' latest frame is
                                           ELAPSED ms into that release
                                           (ease-out cubic)
  ws-anim-proof.py settled LOG START       the dots' latest frame shows
                                           exactly the last set's count,
                                           GNOME's at its active index
                                           (nothing scaling in or out)

START is a 1-based log line; only lines from it on count. Commands wait
up to --timeout seconds for the shell to log what they check.
"""
import argparse
import re
import sys
import time

PREFIX = "tuna-shell-gtk: "
# panel.js:44 and the .workspace-dot size (8px).
INACTIVE_WORKSPACE_DOT_SCALE = 0.75
DOT = 8.0
# Frames print sizes to 3 places, opacities and positions to 4.
TOLERANCE = {"position": 0.00015, "size": 0.0015, "opacity": 0.00015}

DOTS_FRAME = re.compile(
    r"ws-dots frame position=(?P<position>[-\d.]+) slots=(?P<slots>[-\d.,]+) "
    r"widths=(?P<widths>[-\d.,]+) heights=(?P<heights>[-\d.,]+) "
    r"opacities=(?P<opacities>[-\d.,]+)$")
POPUP_FRAME = re.compile(r"ws-popup frame opacity=(?P<opacity>[-\d.]+)$")
FOLLOW = re.compile(r"ws-dots follow position=(?P<position>[-\d.]+) duration=(?P<duration>[-\d.]+) ")
SET = re.compile(r"ws-dots set count=(?P<count>\d+) active=(?P<active>\d+) ")


def lerp(a, b, t):
    return a + (b - a) * t


def ease_out_cubic(t):
    t = min(max(t, 0.0), 1.0) - 1.0
    return t * t * t + 1.0


def width_multiplier(n):
    """`_updateExpansion` (panel.js:182-188)."""
    if n <= 2:
        return 3.625
    if n <= 5:
        return 3.25
    return 2.75


def gnome_dots(n, value):
    """Each dot of `n` at adjustment `value`: (slot, width, height,
    opacity). `expansion = clamp(1 - |index - value|)` (panel.js:190-194);
    the slot is the dot width times lerp(1, multiplier, expansion)
    (:91-94), the visible dot scales by lerp(0.75, 1, expansion) and fades
    by lerp(0.5, 1, expansion) (:81-89)."""
    multiplier = width_multiplier(n)
    out = []
    for index in range(n):
        expansion = min(max(1 - abs(index - value), 0.0), 1.0)
        slot = DOT * lerp(1.0, multiplier, expansion)
        scale = lerp(INACTIVE_WORKSPACE_DOT_SCALE, 1.0, expansion)
        out.append((slot, slot * scale, DOT * scale, lerp(0.5, 1.0, expansion)))
    return out


def trace(log, start):
    with open(log, encoding="utf-8", errors="replace") as fh:
        lines = fh.read().splitlines()[max(start, 1) - 1:]
    return [line.split(PREFIX, 1)[1] for line in lines if PREFIX in line]


def wait(timeout, check):
    """Poll `check` until it returns a non-None value or time runs out."""
    deadline = time.monotonic() + timeout
    while True:
        result = check()
        if result is not None or time.monotonic() >= deadline:
            return result
        time.sleep(0.1)


def last_match(entries, pattern):
    for entry in reversed(entries):
        match = pattern.search(entry)
        if match:
            return match
    return None


def dots_frame(match):
    floats = lambda name: [float(v) for v in match[name].split(",")]  # noqa: E731
    return {
        "position": float(match["position"]),
        "dots": list(zip(floats("slots"), floats("widths"), floats("heights"),
                         floats("opacities"))),
    }


def dots_mismatch(frame, position, count=None):
    """Why `frame` is not GNOME's dots at `position`, or None. With
    `count`, the frame must hold exactly that many dots: none still
    scaling in or out (`scaleIn`/`scaleOutAndDestroy`, panel.js:101-127)."""
    if count is not None and len(frame["dots"]) != count:
        return f"{len(frame['dots'])} dots drawn, not the {count} workspaces"
    if abs(frame["position"] - position) > TOLERANCE["position"]:
        return f"position {frame['position']} != {position:.4f}"
    want = gnome_dots(len(frame["dots"]), position)
    for index, (got, expected) in enumerate(zip(frame["dots"], want)):
        for value, wanted, kind in zip(got, expected, ("size", "size", "size", "opacity")):
            if abs(value - wanted) > TOLERANCE[kind]:
                return f"dot {index} {got} != GNOME {tuple(round(v, 4) for v in expected)}"
    return None


def releases(entries):
    """(held position, release position, duration) of the last swipe
    release: the last follow with a duration, and the follow before it."""
    follows = [FOLLOW.search(e) for e in entries]
    follows = [(float(m["position"]), float(m["duration"])) for m in follows if m]
    for index in range(len(follows) - 1, -1, -1):
        position, duration = follows[index]
        if duration > 0:
            held = follows[index - 1][0] if index > 0 else None
            return held, position, duration
    return None


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("command",
                    choices=["line", "absent", "popup", "dots", "release", "eased", "settled"])
    ap.add_argument("log")
    ap.add_argument("start", type=int)
    ap.add_argument("args", nargs="*")
    ap.add_argument("--timeout", type=float, default=10.0)
    a = ap.parse_args(argv)
    entries = lambda: trace(a.log, a.start)  # noqa: E731

    if a.command == "line":
        text = " ".join(a.args)
        if wait(a.timeout, lambda: True if any(text in e for e in entries()) else None) is None:
            raise SystemExit(f"no line '{text}' (last: {entries()[-4:]})")
        return 0
    if a.command == "absent":
        text = " ".join(a.args)
        time.sleep(a.timeout)
        found = [e for e in entries() if text in e]
        if found:
            raise SystemExit(f"unexpected line '{found[0]}'")
        return 0
    if a.command == "popup":
        want = float(a.args[0])

        def popup():
            match = last_match(entries(), POPUP_FRAME)
            if match and abs(float(match["opacity"]) - want) <= TOLERANCE["opacity"]:
                return True
            return None
        if wait(a.timeout, popup) is None:
            match = last_match(entries(), POPUP_FRAME)
            raise SystemExit(f"popup opacity {match['opacity'] if match else None} != {want}")
        return 0
    if a.command in ("dots", "eased"):
        if a.command == "dots":
            position = float(a.args[0])
        else:
            release = releases(entries())
            if release is None or release[0] is None:
                raise SystemExit("no swipe release after a held swipe")
            held, end, duration = release
            elapsed = float(a.args[0])
            position = lerp(held, end, ease_out_cubic(elapsed / duration))

        def dots():
            match = last_match(entries(), DOTS_FRAME)
            if match and dots_mismatch(dots_frame(match), position) is None:
                return True
            return None
        if wait(a.timeout, dots) is None:
            match = last_match(entries(), DOTS_FRAME)
            why = dots_mismatch(dots_frame(match), position) if match else "no frame"
            raise SystemExit(f"dots are not GNOME 51's at {position:.4f}: {why}")
        print(f"{position:.4f}")
        return 0
    if a.command == "settled":
        def settled():
            entries_now = entries()
            last_set = last_match(entries_now, SET)
            match = last_match(entries_now, DOTS_FRAME)
            if last_set is None or match is None:
                return None
            count, active = int(last_set["count"]), float(last_set["active"])
            if dots_mismatch(dots_frame(match), active, count) is None:
                return True
            return None
        if wait(a.timeout, settled) is None:
            entries_now = entries()
            last_set = last_match(entries_now, SET)
            match = last_match(entries_now, DOTS_FRAME)
            if last_set is None or match is None:
                raise SystemExit("no dots set or frame traced")
            why = dots_mismatch(dots_frame(match), float(last_set["active"]),
                                int(last_set["count"]))
            raise SystemExit(f"the dots never settled: {why}")
        return 0
    if a.command == "release":
        release = wait(a.timeout, lambda: releases(entries()))
        if release is None:
            raise SystemExit("no swipe release")
        held, end, duration = release
        print(f"{end:.4f} {duration:.3f} {held if held is not None else 'none'}")
        return 0
    return 2


if __name__ == "__main__":
    sys.exit(main())
