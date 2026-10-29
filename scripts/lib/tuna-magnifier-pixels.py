#!/usr/bin/python3
"""Pixel measurements on live output frames for tuna-magnifier-proof (#349).

Frames are binary PPM (P6) files, as ffmpeg writes them from an
x11grab of the nested compositor window (the real output, magnified
pointer sprite included).

  run FILE ROW R G B TOL          first and last column of the longest
                                  run of pixels near R G B on ROW
  footprint FILE X Y R G B DIST   bounding box W H of pixels farther
                                  than DIST from R G B in the 128 px box
                                  from (X-32, Y-32): the pointer sprite
                                  over a flat fill
  count FILE R G B TOL            pixels near R G B in the whole frame

"Near" means every channel within TOL; "farther" means some channel
differs by more than DIST.
"""
import sys


def load(path):
    data = open(path, "rb").read()
    fields = []
    pos = 0
    while len(fields) < 4:
        while data[pos : pos + 1].isspace():
            pos += 1
        if data[pos : pos + 1] == b"#":
            while data[pos : pos + 1] not in (b"\n", b""):
                pos += 1
            continue
        start = pos
        while not data[pos : pos + 1].isspace():
            pos += 1
        fields.append(data[start:pos])
    if fields[0] != b"P6" or fields[3] != b"255":
        raise SystemExit(f"{path}: not an 8-bit P6 PPM")
    width, height = int(fields[1]), int(fields[2])
    pixels = data[pos + 1 :]
    if len(pixels) < width * height * 3:
        raise SystemExit(f"{path}: truncated PPM")
    return width, height, pixels


def near(pixels, offset, rgb, tol):
    return all(abs(pixels[offset + i] - rgb[i]) <= tol for i in range(3))


def main(argv):
    cmd, path = argv[1], argv[2]
    width, height, pixels = load(path)
    nums = [int(v) for v in argv[3:]]
    if cmd == "run":
        row, rgb, tol = nums[0], nums[1:4], nums[4]
        if not 0 <= row < height:
            raise SystemExit(f"row {row} outside {height}")
        best, start = (-1, -1), None
        for x in range(width + 1):
            hit = x < width and near(pixels, (row * width + x) * 3, rgb, tol)
            if hit and start is None:
                start = x
            elif not hit and start is not None:
                if x - 1 - start > best[1] - best[0]:
                    best = (start, x - 1)
                start = None
        print(best[0], best[1])
    elif cmd == "footprint":
        cx, cy, rgb, dist = nums[0], nums[1], nums[2:5], nums[5]
        xs, ys = [], []
        for y in range(max(0, cy - 32), min(height, cy + 96)):
            for x in range(max(0, cx - 32), min(width, cx + 96)):
                if not near(pixels, (y * width + x) * 3, rgb, dist):
                    xs.append(x)
                    ys.append(y)
        if not xs:
            print(0, 0)
        else:
            print(max(xs) - min(xs) + 1, max(ys) - min(ys) + 1)
    elif cmd == "count":
        rgb, tol = nums[0:3], nums[3]
        print(sum(near(pixels, i * 3, rgb, tol) for i in range(width * height)))
    else:
        raise SystemExit(f"unknown command {cmd}")


if __name__ == "__main__":
    main(sys.argv)
