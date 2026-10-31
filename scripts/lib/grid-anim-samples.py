#!/usr/bin/env python3
"""Check the app grid's per-frame animation samples against GNOME 51.

With TUNA_GRID_TRACE the GTK shell logs every animated frame of a page
switch (its elapsed time on the GDK frame clock and where the pages are
drawn) and every page-dot update (pageIndicators.js _updateIndicator).
The expected values here come from GNOME 51's own constants, not from
the shell: iconGrid.js PAGE_SWITCH_TIME (300 ms, ease-out-cubic) and
pageIndicators.js's inactive scale/opacity. Each sample's elapsed time
is the frame clock's, so the check does not depend on wall-clock timing.
"""
import argparse
import re
import sys

PAGE_SWITCH_MS = 300.0
DOT_INACTIVE_SCALE = 2.0 / 3.0
DOT_INACTIVE_SCALE_PRESSED = 0.5
DOT_INACTIVE_OPACITY = 128.0 / 255.0
DOT_INACTIVE_OPACITY_HOVER = 1.0
# The shell prints positions and looks to 6 decimals and elapsed time to
# 3 (at most 3/300 page per ms, so 5e-6 of a page): 1e-4 is rounding
# headroom, not slack in the curve.
TOLERANCE = 1e-4
# Frames strictly between the start and the end of a 300 ms switch; at
# 60 Hz there are about 17, and two still show the curve was followed.
MIN_INTERMEDIATE = 2

NUMBER = r'(-?[0-9]+(?:\.[0-9]+)?)'
FRAME = re.compile(
    r'grid-anim page-switch frame elapsed=' + NUMBER + r' position=' + NUMBER
    + r' from=' + NUMBER + r' to=' + NUMBER + r' ms=' + NUMBER)
DOTS = re.compile(r'grid-anim page-dots position=' + NUMBER + r' dots=(\S+)')
TICK = re.compile(r'grid-anim page-switch tick (.*)$')


def out_cubic(t):
    t = min(max(t, 0.0), 1.0)
    return 1.0 - (1.0 - t) ** 3


def expected_position(start, end, elapsed):
    return start + (end - start) * out_cubic(elapsed / PAGE_SWITCH_MS)


def dot_look(position, index, hover, pressed):
    progress = max(1.0 - abs(position - index), 0.0)
    inactive_scale = DOT_INACTIVE_SCALE_PRESSED if pressed else DOT_INACTIVE_SCALE
    inactive_opacity = DOT_INACTIVE_OPACITY_HOVER if hover else DOT_INACTIVE_OPACITY
    return (inactive_scale + (1.0 - inactive_scale) * progress,
            inactive_opacity + (1.0 - inactive_opacity) * progress)


def parse(text):
    frames, dots = [], []
    for line in text.splitlines():
        match = FRAME.search(line)
        if match:
            elapsed, position, start, end, ms = map(float, match.groups())
            frames.append({'elapsed': elapsed, 'position': position, 'from': start,
                           'to': end, 'ms': ms})
            continue
        match = DOTS.search(line)
        if match:
            samples = []
            for item in match.group(2).split(','):
                fields = item.split(':')
                if len(fields) != 5:
                    raise ValueError(f'malformed page-dots sample {item!r}')
                samples.append({'index': int(fields[0]), 'scale': float(fields[1]),
                                'opacity': float(fields[2]), 'hover': fields[3] == '1',
                                'pressed': fields[4] == '1'})
            dots.append({'position': float(match.group(1)), 'dots': samples})
    return frames, dots


def between(value, start, end):
    low, high = min(start, end), max(start, end)
    return low + TOLERANCE < value < high - TOLERANCE


def check_dots(dots):
    errors = []
    for line in dots:
        for dot in line['dots']:
            scale, opacity = dot_look(line['position'], dot['index'], dot['hover'], dot['pressed'])
            if abs(dot['scale'] - scale) > TOLERANCE or abs(dot['opacity'] - opacity) > TOLERANCE:
                errors.append(
                    f"dot {dot['index']} at position {line['position']}: "
                    f"{dot['scale']}/{dot['opacity']}, GNOME {scale:.6f}/{opacity:.6f}")
    return errors


def check_switch(frames, dots, start, end):
    """An animated switch from page `start` to `end`: [] when it follows
    GNOME's 300 ms ease-out-cubic and the dots follow it."""
    errors = []
    if not frames:
        return ['no animated page-switch frames']
    for frame in frames:
        if abs(frame['from'] - start) > TOLERANCE or abs(frame['to'] - end) > TOLERANCE:
            errors.append(f"frame from {frame['from']} to {frame['to']}, not {start} to {end}")
        if abs(frame['ms'] - PAGE_SWITCH_MS) > TOLERANCE:
            errors.append(f"frame of a {frame['ms']} ms switch, not GNOME's {PAGE_SWITCH_MS:.0f} ms")
        want = expected_position(start, end, frame['elapsed'])
        if abs(frame['position'] - want) > TOLERANCE:
            errors.append(f"at {frame['elapsed']} ms the pages were at {frame['position']}, "
                          f"GNOME's ease-out-cubic {want:.6f}")
    direction = 1.0 if end >= start else -1.0
    for before, after in zip(frames, frames[1:]):
        if after['elapsed'] < before['elapsed']:
            errors.append(f"frame time went back from {before['elapsed']} to {after['elapsed']}")
        if (after['position'] - before['position']) * direction < -TOLERANCE:
            errors.append(f"the pages moved back from {before['position']} to {after['position']}")
    middle = [f for f in frames
              if 0.0 < f['elapsed'] < PAGE_SWITCH_MS and between(f['position'], start, end)]
    if len(middle) < MIN_INTERMEDIATE:
        errors.append(f'{len(middle)} frames between the pages, fewer than {MIN_INTERMEDIATE}')
    last = frames[-1]
    if last['elapsed'] < PAGE_SWITCH_MS or abs(last['position'] - end) > TOLERANCE:
        errors.append(f"the last frame ({last['elapsed']} ms, {last['position']}) is not "
                      f"page {end} after {PAGE_SWITCH_MS:.0f} ms")
    if not dots:
        errors.append('no page-dots samples')
    else:
        errors += check_dots(dots)
        moving = [d for d in dots if between(d['position'], start, end)]
        if len(moving) < MIN_INTERMEDIATE:
            errors.append(f'{len(moving)} page-dots samples between the pages, '
                          f'fewer than {MIN_INTERMEDIATE}')
        if abs(dots[-1]['position'] - end) > TOLERANCE:
            errors.append(f"the dots settled at {dots[-1]['position']}, not page {end}")
    return errors


def check_instant(frames, dots, end):
    """A switch to page `end` with animations off: [] when no frame was
    animated and the dots jumped there."""
    errors = [f"animated frame at {f['elapsed']} ms with animations off" for f in frames]
    if not dots:
        errors.append('no page-dots samples')
    for line in dots:
        if abs(line['position'] - end) > TOLERANCE:
            errors.append(f"the dots passed through {line['position']} with animations off")
    errors += check_dots(dots)
    return errors


def switch_diagnostics(frames, dots, start, end):
    """Describe available samples, without changing the gate's verdict.

    A correct curve at sparse callback times is not proof of smooth
    presentation, nor evidence of which process delayed those callbacks.
    """
    middle = sum(0.0 < f['elapsed'] < PAGE_SWITCH_MS
                 and between(f['position'], start, end) for f in frames)
    moving_dots = sum(between(d['position'], start, end) for d in dots)
    lines = [
        f'sample coverage: {middle}/{MIN_INTERMEDIATE} required intermediate page frames; '
        f'{moving_dots}/{MIN_INTERMEDIATE} required intermediate page-dot updates',
    ]
    gaps = [(after['elapsed'] - before['elapsed'], before['elapsed'], after['elapsed'])
            for before, after in zip(frames, frames[1:])]
    positive = [gap for gap in gaps if gap[0] > 0.0]
    repeated = sum(gap[0] == 0.0 for gap in gaps)
    if positive:
        gap, before, after = max(positive)
        lines.append(f'largest logged frame-time gap: {gap:.3f} ms '
                     f'({before:.3f} -> {after:.3f} ms); repeated frame times: {repeated}')
    else:
        lines.append(f'no advancing frame-time samples; repeated frame times: {repeated}')
    if frames:
        error = max(abs(f['position'] - expected_position(start, end, f['elapsed']))
                    for f in frames)
        lines.append(f'maximum GNOME position error: {error:.9f} page '
                     f'(tolerance {TOLERANCE:.9f})')
    lines.append('trace scope: page positions are callback samples, not presentation; '
                 'elapsed=0 includes initialization and the first tick')
    return lines


def tick_diagnostics(text):
    return [match.group(1) for line in text.splitlines() if (match := TICK.search(line))]


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--log', required=True)
    parser.add_argument('--offset', type=int, default=0, help='byte offset of the first line to read')
    parser.add_argument('--from', dest='start', type=float, help='page the switch starts on')
    parser.add_argument('--to', dest='end', type=float, required=True, help='page it ends on')
    parser.add_argument('--instant', action='store_true', help='animations are off')
    args = parser.parse_args(argv)
    with open(args.log, 'rb') as log:
        log.seek(args.offset)
        text = log.read().decode('utf-8', errors='replace')
    frames, dots = parse(text)
    if args.instant:
        errors = check_instant(frames, dots, args.end)
    elif args.start is None:
        parser.error('--from is required unless --instant')
    else:
        errors = check_switch(frames, dots, args.start, args.end)
    print(f'grid-anim-samples: source {args.log}, byte offset {args.offset}')
    print(f'grid-anim-samples: {len(frames)} page-switch frames, {len(dots)} page-dots samples')
    for frame in frames:
        print(f"  frame {frame['elapsed']:.3f} ms position {frame['position']:.6f} "
              f"(GNOME {expected_position(frame['from'], frame['to'], frame['elapsed']):.6f})")
    if not args.instant:
        for line in switch_diagnostics(frames, dots, args.start, args.end):
            print(f'grid-anim-samples: {line}')
        ticks = tick_diagnostics(text)
        if ticks:
            for tick in ticks:
                print(f'grid-anim-samples: tick observation: {tick}')
            print('grid-anim-samples: scheduler scope: thread CPU includes observer work; '
                  'runqueue wait is meaningful only with schedstats=enabled; '
                  'blocked time alone does not identify compositor or presentation delay')
        else:
            print('grid-anim-samples: main-thread scheduler observations unavailable in this trace')
    for error in errors:
        print(f'grid-anim-samples: {error}', file=sys.stderr)
    return 1 if errors else 0


if __name__ == '__main__':
    sys.exit(main())
