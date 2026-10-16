#!/usr/bin/python3
"""Shared assertions for the P-ST-01 live-settings gates (#594).

G-SET-WALLPAPER, G-SET-CLOCK and G-SET-FONTS call the subcommands below
so the same checks run in the nested proof and in the self-test
(`tuna-set-gates-tests.py`), which needs no compositor:

  clock-check A11Y_JSON --format {12h,24h} --date {on,off}
      Assert the panel clock's accessible text matches GNOME's
      clock-format / clock-show-date policy (see
      `crates/shell-gtk/src/logic.rs::clock_text`).
  wallpaper-check CSTATE_JSON --uri URI [--identity {file,none}] [--decoded]
      Assert the compositor's wallpaper diagnostics track URI: a file
      identity snapshot (`FileIdentity`, for readable images, or `None`
      for ignored non-file URIs) and/or a decoded buffer with pixels.
  fonts-check FIXTURE_JSON --document FONT --monospace FONT
      Assert the GTK fixture app observed the GNOME document/monospace
      fonts through its own Gio.Settings read, as GNOME apps do (no
      Gtk.Settings properties exist for these keys, no Tuna code).
  desktop-pixel CSTATE_JSON --width W --height H
      Print "X Y" for a desktop pixel outside every window rect and
      below the 32 px panel, for visible wallpaper sampling.
  pixel-diff BEFORE AFTER X Y W H --min-fraction F
      Assert at least fraction F of the cropped pixels changed
      (per-channel threshold 25, like G-SETTINGS-FONT).

Exit 0 with a one-line report on success, exit 1 with the reason on
failure. Only pixel-diff needs PIL; everything else is stdlib.
"""

import argparse
import json
import re
import sys

MONTHS = ("Jan", "Feb", "Mar", "Apr", "May", "Jun",
          "Jul", "Aug", "Sep", "Oct", "Nov", "Dec")

TIME_RE = re.compile(r"\d{1,2}:\d{2}")
AMP_PM_RE = re.compile(r"\d{1,2}:\d{2}(:\d{2})? [AP]M")


def load_json(path):
    with open(path, encoding="utf-8") as handle:
        return json.load(handle)


def clock_labels(nodes):
    """Accessible label texts containing a clock-like time."""
    return [node.get("name", "") for node in nodes
            if node.get("role") == "label"
            and TIME_RE.search(node.get("name", ""))]


def check_clock(nodes, fmt, date):
    """Return (ok, report) for the clock-format / clock-show-date policy."""
    labels = clock_labels(nodes)
    if not labels:
        return False, "no accessible label shows a time"
    meridiem = [text for text in labels if AMP_PM_RE.search(text)]
    dated = [text for text in labels
             if any(month in text for month in MONTHS)]
    if fmt == "12h" and not meridiem:
        sample = " / ".join(labels[:3])
        return False, f"no 12h AM/PM clock among: {sample}"
    if fmt == "24h" and meridiem:
        return False, f"24h clock unexpectedly shows AM/PM: {meridiem[0]!r}"
    if date == "on" and not dated:
        sample = " / ".join(labels[:3])
        return False, f"no date in the panel clock among: {sample}"
    if date == "off" and dated:
        return False, f"date still shown with clock-show-date off: {dated[0]!r}"
    shown = meridiem[0] if meridiem else labels[0]
    return True, f"clock shows {shown!r} (format={fmt} date={date})"


def prefixed(entry, key):
    """Diagnostics `text()` helper: the entry's truncated prefix string."""
    value = (entry or {}).get(key) or {}
    return value.get("prefix", "")


def decode_uri(entry):
    """Decode-result URIs are render keys (`uri`, settings, geometry and
    file identity joined by newlines); the first line is the URI."""
    return prefixed(entry, "request_key").split("\n", 1)[0]


def check_wallpaper(state, uri, identity, decoded):
    """Return (ok, report) for the compositor wallpaper diagnostics."""
    diag = (state or {}).get("wallpaper_diagnostics") or {}
    if identity:
        snapshots = diag.get("identities") or []
        match = [item for item in snapshots
                 if prefixed(item, "uri") == uri]
        if not match:
            known = [prefixed(item, "uri") for item in snapshots][:4]
            return False, f"no identity snapshot for {uri!r} (have {known})"
        seen = prefixed(match[0], "identity")
        if identity == "file" and not seen.startswith("Some(FileIdentity"):
            return False, f"identity for {uri!r} is {seen!r}, not a file"
        if identity == "none" and seen != "None":
            return False, f"identity for {uri!r} is {seen!r}, not ignored"
    if decoded:
        results = diag.get("decode_results") or []
        match = [item for item in results if decode_uri(item) == uri]
        if not match:
            known = [decode_uri(item) for item in results][:4]
            return False, f"no decode result for {uri!r} (have {known})"
        result = match[0]
        pixels = result.get("pixel_bytes") or 0
        if result.get("buffer_available") is not True or pixels <= 0:
            return False, (f"no decoded buffer for {uri!r}: "
                           f"buffer_available={result.get('buffer_available')} "
                           f"pixel_bytes={result.get('pixel_bytes')}")
    parts = [f"uri={uri}"]
    if identity:
        parts.append(f"identity={identity}")
    if decoded:
        parts.append("decoded")
    return True, "wallpaper diagnostics track " + " ".join(parts)


def check_fonts(report, document, monospace):
    """Return (ok, report) for the GTK fixture app's observed fonts."""
    if "error" in report:
        return False, f"fixture reports an error: {report['error']}"
    seen_document = report.get("app_document")
    seen_monospace = report.get("app_monospace")
    if seen_document != document or seen_monospace != monospace:
        return False, (
            f"GTK app observes document={seen_document!r} "
            f"monospace={seen_monospace!r}, want {document!r} / {monospace!r}")
    return True, (f"GTK app follows GNOME: document={document!r} "
                  f"monospace={monospace!r}")


def uncovered_desktop_pixel(state, width, height):
    """Return (x, y) outside every window rect and below the panel."""
    rects = [window.get("rect", []) for window in (state or {}).get("windows", [])]
    candidates = [(10, height - 10), (width - 10, height - 10),
                  (width - 10, 100), (10, 100),
                  (width // 2, height - 10),
                  (width // 4, height // 2), (3 * width // 4, height // 2),
                  (width // 2, height // 2)]
    for x, y in candidates:
        if y < 40:
            continue
        if all(not (rx - 2 <= x < rx + rw + 2 and ry - 2 <= y < ry + rh + 2)
               for rx, ry, rw, rh in (r for r in rects if len(r) == 4)):
            return x, y
    return None


def check_pixel_diff(before_path, after_path, box, min_fraction):
    """Return (ok, report); at least min_fraction of the crop changed."""
    from PIL import Image, ImageChops
    before = Image.open(before_path).convert("RGB").crop(box)
    after = Image.open(after_path).convert("RGB").crop(box)
    width, height = before.size
    histogram = ImageChops.difference(before, after).convert("L").histogram()
    changed = sum(histogram[25:]) / max(1, width * height)
    if changed < min_fraction:
        return False, (f"only {changed:.3%} of {width}x{height} changed, "
                       f"want at least {min_fraction:.3%}")
    return True, f"{changed:.3%} of {width}x{height} changed"


def main(argv):
    parser = argparse.ArgumentParser()
    sub = parser.add_subparsers(dest="command", required=True)
    clock = sub.add_parser("clock-check")
    clock.add_argument("a11y")
    clock.add_argument("--format", choices=("12h", "24h"), required=True)
    clock.add_argument("--date", choices=("on", "off"), required=True)
    wallpaper = sub.add_parser("wallpaper-check")
    wallpaper.add_argument("cstate")
    wallpaper.add_argument("--uri", required=True)
    wallpaper.add_argument("--identity", choices=("file", "none"))
    wallpaper.add_argument("--decoded", action="store_true")
    fonts = sub.add_parser("fonts-check")
    fonts.add_argument("report")
    fonts.add_argument("--document", required=True)
    fonts.add_argument("--monospace", required=True)
    pixel = sub.add_parser("desktop-pixel")
    pixel.add_argument("cstate")
    pixel.add_argument("--width", type=int, required=True)
    pixel.add_argument("--height", type=int, required=True)
    diff = sub.add_parser("pixel-diff")
    diff.add_argument("before")
    diff.add_argument("after")
    diff.add_argument("x", type=int)
    diff.add_argument("y", type=int)
    diff.add_argument("w", type=int)
    diff.add_argument("h", type=int)
    diff.add_argument("--min-fraction", type=float, required=True)
    args = parser.parse_args(argv)
    if args.command == "clock-check":
        ok, report = check_clock(load_json(args.a11y), args.format, args.date)
    elif args.command == "wallpaper-check":
        ok, report = check_wallpaper(load_json(args.cstate), args.uri,
                                     args.identity, args.decoded)
    elif args.command == "fonts-check":
        ok, report = check_fonts(load_json(args.report),
                                 args.document, args.monospace)
    elif args.command == "desktop-pixel":
        found = uncovered_desktop_pixel(load_json(args.cstate),
                                        args.width, args.height)
        if found is None:
            print("tuna-set-gates: no desktop pixel outside every window",
                  file=sys.stderr)
            return 1
        print(f"{found[0]} {found[1]}")
        return 0
    elif args.command == "pixel-diff":
        ok, report = check_pixel_diff(args.before, args.after,
                                      (args.x, args.y,
                                       args.x + args.w, args.y + args.h),
                                      args.min_fraction)
    print(f"tuna-set-gates: {report}")
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
