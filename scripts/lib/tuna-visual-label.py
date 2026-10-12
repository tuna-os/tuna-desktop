#!/usr/bin/env python3
"""Label visual-capture frames honestly (never claim wallpaper as proof).

Reads <dir>/frames.jsonl (one JSON object per captured still: file, state,
sha256, epoch_ms) and writes <dir>/frames.json with a label per frame:

  baseline                      the stage's bare-desktop reference frame
  feature                       differs from the baseline above threshold
  indistinguishable-from-baseline
                                a real frame, but visually the bare desktop
                                (e.g. the UI failed to open) -- NOT proof
  duplicate                     byte-identical to an earlier frame
  unverified                    PIL is unavailable so no pixel comparison ran;
                                duplicates are still detected by hash

Threshold: a frame is `feature` when at least 0.2% of its pixels differ
from the baseline in any channel by more than 12 levels. A 1280x800 panel
strip alone is ~4%, so real UI states clear this with margin while noise
and cursor-only movement do not.

Clips (*.webm) are listed, never pixel-compared: label `clip` with the
 recorded duration and byte size.

Usage: tuna-visual-label.py --dir ARTIFACTS --baseline FILE
       tuna-visual-label.py --compare OLD NEW [--threshold PCT]
Exit status is always 0 in labeling mode: labeling never fails a capture
run. In --compare mode the exit status is 0 when the two frames differ
by at least the threshold percent of pixels, 1 when they are visually
the same, and 2 when the comparison cannot run (missing file, size
mismatch, or no PIL): callers fail closed on 2, never treat it as a pass.
"""
import argparse
import hashlib
import json
import os
import sys

THRESHOLD_FRACTION = 0.002
CHANNEL_TOLERANCE = 12


def load_frames(path):
    frames = []
    with open(path, encoding="utf-8") as handle:
        for line in handle:
            line = line.strip()
            if line:
                frames.append(json.loads(line))
    return frames


def diff_fraction(baseline_rgb, other_rgb):
    try:
        from PIL import ImageChops
        import PIL.ImageStat as ImageStat
    except ImportError:
        return None
    diff = ImageChops.difference(baseline_rgb, other_rgb)
    stat = ImageStat.Stat(diff.convert("L"), mask=None)
    # Count pixels above tolerance via a point-thresholded histogram.
    thresholded = diff.convert("L").point(lambda v: 255 if v > CHANNEL_TOLERANCE else 0)
    hist = thresholded.histogram()
    changed = hist[255] if len(hist) > 255 else 0
    total = thresholded.size[0] * thresholded.size[1]
    _ = stat  # kept: mean available for debugging without changing the rule
    return changed / total if total else 0.0


def compare(old_path, new_path, threshold_pct):
    """0 differs, 1 same, 2 cannot compare (fail closed, never a pass)."""
    try:
        from PIL import Image
    except ImportError:
        print("tuna-visual-label: PIL unavailable; cannot compare", file=sys.stderr)
        return 2
    try:
        old = Image.open(old_path).convert("RGB")
        new = Image.open(new_path).convert("RGB")
    except Exception as exc:
        print("tuna-visual-label: cannot open frames: %s" % exc, file=sys.stderr)
        return 2
    if old.size != new.size:
        print("tuna-visual-label: size mismatch %s vs %s" % (old.size, new.size),
              file=sys.stderr)
        return 2
    fraction = diff_fraction(old, new)
    if fraction is None:
        return 2
    pct = fraction * 100
    print("tuna-visual-label: diff %.3f%% (threshold %.3f%%)" % (pct, threshold_pct))
    return 0 if pct >= threshold_pct else 1


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--dir")
    parser.add_argument("--baseline")
    parser.add_argument("--compare", nargs=2, metavar=("OLD", "NEW"))
    parser.add_argument("--threshold", type=float, default=THRESHOLD_FRACTION * 100)
    args = parser.parse_args()
    if args.compare:
        return compare(args.compare[0], args.compare[1], args.threshold)
    if not args.dir or not args.baseline:
        parser.error("--dir and --baseline are required in labeling mode")
    directory = args.dir
    frames_path = os.path.join(directory, "frames.jsonl")
    frames = load_frames(frames_path) if os.path.exists(frames_path) else []

    try:
        from PIL import Image

        pil_available = True
    except ImportError:
        pil_available = False
        Image = None

    baseline_rgb = None
    baseline_path = os.path.join(directory, args.baseline)
    if pil_available and os.path.exists(baseline_path):
        try:
            baseline_rgb = Image.open(baseline_path).convert("RGB")
        except Exception:
            baseline_rgb = None

    seen_hashes = {}
    labeled = []
    for entry in frames:
        name = entry.get("file", "")
        path = os.path.join(directory, name)
        digest = entry.get("sha256", "")
        if not digest and os.path.exists(path):
            with open(path, "rb") as handle:
                digest = hashlib.sha256(handle.read()).hexdigest()
        out = dict(entry)
        out["sha256"] = digest
        if name.endswith((".webm", ".mp4", ".mkv")):
            out["label"] = "clip"
            try:
                out["bytes"] = os.path.getsize(path)
            except OSError:
                out["bytes"] = None
            labeled.append(out)
            continue
        if name == args.baseline:
            out["label"] = "baseline"
            seen_hashes.setdefault(digest, name)
            labeled.append(out)
            continue
        if digest and digest in seen_hashes:
            out["label"] = "duplicate"
            out["duplicate_of"] = seen_hashes[digest]
            out["diff_pct"] = 0.0
            labeled.append(out)
            continue
        if digest:
            seen_hashes.setdefault(digest, name)
        fraction = None
        if baseline_rgb is not None and os.path.exists(path):
            try:
                other = Image.open(path).convert("RGB")
                if other.size == baseline_rgb.size:
                    fraction = diff_fraction(baseline_rgb, other)
            except Exception:
                fraction = None
        out["diff_pct"] = round(fraction * 100, 3) if fraction is not None else None
        if fraction is None:
            out["label"] = "unverified"
        elif fraction >= THRESHOLD_FRACTION:
            out["label"] = "feature"
        else:
            out["label"] = "indistinguishable-from-baseline"
        labeled.append(out)

    summary = {}
    for entry in labeled:
        summary[entry.get("label", "?")] = summary.get(entry.get("label", "?"), 0) + 1
    report = {
        "baseline": args.baseline,
        "threshold_pct": THRESHOLD_FRACTION * 100,
        "channel_tolerance": CHANNEL_TOLERANCE,
        "pil_available": pil_available,
        "summary": summary,
        "frames": labeled,
    }
    with open(os.path.join(directory, "frames.json"), "w", encoding="utf-8") as handle:
        json.dump(report, handle, indent=2)
        handle.write("\n")
    print("tuna-visual-label: %s" % (", ".join("%s=%d" % pair for pair in sorted(summary.items())),))
    return 0


if __name__ == "__main__":
    sys.exit(main())
