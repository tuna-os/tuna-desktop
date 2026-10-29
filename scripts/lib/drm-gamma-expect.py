#!/usr/bin/env python3
"""Expected CRTC gamma ramps for the DRM smoke colour proof (P-DS-03).

  drm-gamma-expect.py vcgt-add SRC.icc DST.icc
      Copy an ICC profile and give it a 256-entry 16-bit vcgt table
      (red sqrt(x), green 0.75x, blue x^2); the profile ID is zeroed
      (not computed), as ICC allows. DST.icc.json keeps the tables.
  drm-gamma-expect.py ramp N KELVIN > RAMP.json
      A raw calibration ramp (warm, distinct from every other case).
  drm-gamma-expect.py check READBACK.json MODE [ARGS]
      MODE "ramp KELVIN": Mutter's uncalibrated ramp, i*65535/(n-1)
      scaled by colord's Planckian blackbody (exact, integer math).
      MODE "vcgt TABLES.json KELVIN": the vcgt interpolated at i/(n-1)
      times the blackbody, with lcms2's 16-bit table interpolation.
      MODE "raw RAMP.json": exactly the calibration ramp.

Expected values follow meta-color-profile.c (generate_gamma_lut*) and
colord 1.4.7 cd-color.c (blackbody_data_d65plankian), written out here
independently of the compositor's implementation.
"""

import json
import struct
import sys

# colord's Planckian table at the temperatures the proof uses.
BLACKBODY = {6500: (1.0, 1.0, 1.0), 3000: (1.0, 0.7198, 0.4286)}


def curves(n=256):
    xs = [i / (n - 1) for i in range(n)]
    return {
        "red": [round(65535 * x ** 0.5) for x in xs],
        "green": [round(65535 * 0.75 * x) for x in xs],
        "blue": [round(65535 * x * x) for x in xs],
    }


def vcgt_add(src, dst):
    data = bytearray(open(src, "rb").read())
    size = struct.unpack_from(">I", data, 0)[0]
    data = data[:size]
    count = struct.unpack_from(">I", data, 128)[0]
    tables = curves()
    tag = bytearray(b"vcgt\0\0\0\0")
    tag += struct.pack(">IHHH", 0, 3, 256, 2)
    for channel in ("red", "green", "blue"):
        tag += struct.pack(">256H", *tables[channel])
    # Insert one tag-table entry; every tag offset moves by 12 bytes.
    table_end = 132 + count * 12
    entries = bytearray()
    for i in range(count):
        sig, offset, length = struct.unpack_from(">4sII", data, 132 + i * 12)
        entries += struct.pack(">4sII", sig, offset + 12, length)
    body = data[table_end:]
    while len(body) % 4:
        body += b"\0"
    vcgt_offset = table_end + 12 + len(body)
    entries += struct.pack(">4sII", b"vcgt", vcgt_offset, len(tag))
    out = data[:128] + struct.pack(">I", count + 1) + entries + body + tag
    while len(out) % 4:
        out += b"\0"
    struct.pack_into(">I", out, 0, len(out))
    out[84:100] = bytes(16)
    open(dst, "wb").write(out)
    json.dump(tables, open(dst + ".json", "w", encoding="utf-8"))


def interpolate(table, x):
    """lcms2 cmsEvalToneCurveFloat on a 16-bit table (LinLerp1D)."""
    d = x * 65535.0 + 0.5
    word = 0 if d <= 0 else 0xFFFF if d >= 65535.0 else int(d)
    domain = len(table) - 1
    if word == 0xFFFF or domain == 0:
        out = table[-1]
    else:
        val3 = domain * word
        val3 += (val3 + 0x7FFF) // 0xFFFF
        cell, rest = val3 >> 16, val3 & 0xFFFF
        low, high = table[cell], table[min(cell + 1, domain)]
        dif = (((high - low) & 0xFFFFFFFF) * rest + 0x8000) & 0xFFFFFFFF
        out = ((dif >> 16) + low) & 0xFFFF
    return out / 65535.0


def f32(value):
    return struct.unpack("f", struct.pack("f", value))[0]


def lut_of(readback):
    crtcs = [c for c in json.load(open(readback, encoding="utf-8")) if c["gamma_lut_size"]]
    assert len(crtcs) == 1, f"expected one CRTC with a gamma LUT: {crtcs}"
    lut = crtcs[0]["gamma_lut"]
    assert lut is not None, "GAMMA_LUT is unset"
    return lut


def check(readback, mode, args):
    lut = lut_of(readback)
    n = len(lut["red"])
    expected = {}
    tolerance = 0
    if mode == "ramp":
        bb = BLACKBODY[int(args[0])]
        for c, channel in enumerate(("red", "green", "blue")):
            expected[channel] = [int(((i * 0xFFFF) // (n - 1)) * bb[c]) for i in range(n)]
    elif mode == "vcgt":
        tables = json.load(open(args[0], encoding="utf-8"))
        bb = BLACKBODY[int(args[1])]
        # meta-color-profile.c: a float input, the float curve value
        # scaled in double precision, truncated to 16 bits.
        for c, channel in enumerate(("red", "green", "blue")):
            expected[channel] = [
                min(65535, int(f32(interpolate(tables[channel], f32(i / (n - 1)))) * bb[c] * 65535.0))
                for i in range(n)
            ]
    elif mode == "raw":
        expected = json.load(open(args[0], encoding="utf-8"))
    else:
        raise SystemExit(f"unknown mode {mode}")
    for channel in ("red", "green", "blue"):
        got = lut[channel]
        want = expected[channel]
        assert len(got) == len(want), f"{channel}: {len(got)} entries, expected {len(want)}"
        worst = max(abs(a - b) for a, b in zip(got, want))
        assert worst <= tolerance, (
            f"{channel} differs by {worst}: got {got[:4]}…{got[-4:]}, want {want[:4]}…{want[-4:]}"
        )
    print(f"{mode} {' '.join(args)}: {n} entries match (tolerance {tolerance})")


def main():
    command = sys.argv[1]
    if command == "vcgt-add":
        vcgt_add(sys.argv[2], sys.argv[3])
    elif command == "ramp":
        n, kelvin = int(sys.argv[2]), int(sys.argv[3])
        bb = BLACKBODY.get(kelvin, (1.0, 0.5, 0.25))
        ramp = {
            channel: [int((i * 0xFFFF) // (n - 1) * bb[c] * 0.9) for i in range(n)]
            for c, channel in enumerate(("red", "green", "blue"))
        }
        json.dump(ramp, sys.stdout)
    elif command == "check":
        check(sys.argv[2], sys.argv[3], sys.argv[4:])
    else:
        raise SystemExit(__doc__)


if __name__ == "__main__":
    main()
