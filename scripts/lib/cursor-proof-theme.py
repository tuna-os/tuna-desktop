#!/usr/bin/env python3
"""Distinct solid cursor fixtures for actual size/hotspot/frame proofs."""
import json
from pathlib import Path
import struct
import sys

LEGACY_COLORS = (0xffb51d5b, 0xff19ad7d)
SVG_COLORS = ("#315fc7", "#d5a121")
DELAYS = (100, 250)


def xcursor(frames):
    header = struct.pack("<4sIII", b"Xcur", 16, 0x10000, len(frames))
    toc, chunks, offset = b"", b"", 16 + 12 * len(frames)
    for side, argb, delay in frames:
        toc += struct.pack("<III", 0xfffd0002, side, offset)
        chunk = struct.pack("<IIIIIIIII", 36, 0xfffd0002, side, 1, side, side, 1, 1, delay)
        chunk += struct.pack("<I", argb) * (side * side)
        chunks += chunk
        offset += len(chunk)
    return header + toc + chunks


def generate(root):
    root = Path(root)
    for theme in ("TunaProof", "TunaProofSvg"):
        directory = root / theme
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "index.theme").write_text(f"[Icon Theme]\nName={theme}\nInherits=TunaProofBase\n")
    legacy = root / "TunaProofBase/cursors"
    legacy.mkdir(parents=True, exist_ok=True)
    for name in ("default", "crosshair"):
        (legacy / name).write_bytes(xcursor([(24, LEGACY_COLORS[0], 0), (48, LEGACY_COLORS[1], 0)]))
    (legacy / "wait").write_bytes(xcursor([(24, color, delay) for color, delay in zip(LEGACY_COLORS, DELAYS)]))
    for name, colors, delays in (("default", SVG_COLORS[:1], (0,)), ("wait", SVG_COLORS, DELAYS)):
        directory = root / "TunaProofSvg/cursors_scalable" / name
        directory.mkdir(parents=True, exist_ok=True)
        descriptions = []
        for index, (color, delay) in enumerate(zip(colors, delays)):
            filename = f"frame-{index}.svg"
            (directory / filename).write_text(f'<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24"><rect width="24" height="24" fill="{color}"/></svg>')
            descriptions.append({"filename": filename, "nominal_size": 24,
                                 "hotspot_x": 4, "hotspot_y": 6, "delay": delay})
        (directory / "metadata.json").write_text(json.dumps(descriptions))


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: cursor-proof-theme.py ICON_DIRECTORY")
    generate(sys.argv[1])
