#!/usr/bin/env python3
"""Validate an uncropped scale frame and return the real close-button click.

The fixture's four-logical-pixel colored footer exposes its actual bottom edge.
Metadata alone is insufficient: missing/clipped marker pixels reject the frame.
"""
import hashlib
import json
import math
import sys
from pathlib import Path
from PIL import Image


def validate(image, state, nodes, scale=1.25):
    windows = [w for w in state['windows'] if w['app_id'] == 'tuna-test-gamma']
    if len(windows) != 1:
        raise ValueError('expected exactly one Gamma')
    x, y, w, h = windows[0]['rect']
    left, top = round(x * scale), round(y * scale)
    right, bottom = round((x + w) * scale), round((y + h) * scale)
    width, height = image.size
    if not (0 <= left < right <= width and 40 <= top < bottom <= height):
        raise ValueError('Gamma geometry outside actual frame/workarea')
    rgb = image.convert('RGB')
    marker_y = bottom - 4
    marker_count = sum(rgb.getpixel((px, marker_y)) == (230, 97, 0)
                       for px in range(left, right))
    if marker_count < right - left - 24:
        raise ValueError('actual Gamma bottom marker absent, occluded or clipped')
    closes = [n for n in nodes if n.get('showing') and n.get('name') == 'Close'
              and n.get('bounds', [0, 0, 0, 0])[2] > 0
              and n['bounds'][3] > 0]
    if len(closes) != 1:
        raise ValueError('expected exactly one visible Close button')
    bx, by, bw, bh = closes[0]['bounds']
    bounds = ((x + bx) * scale, (y + by) * scale,
              (x + bx + bw) * scale, (y + by + bh) * scale)
    if not (left <= bounds[0] < bounds[2] <= right
            and top <= bounds[1] < bounds[3] <= bottom):
        raise ValueError('Close button outside actual window/frame')
    return {'scale': scale, 'frame_size': [width, height],
            'window_id': windows[0]['id'], 'logical_rect': [x, y, w, h],
            'physical_rect': [left, top, right - left, bottom - top],
            'bottom_marker_pixels': marker_count, 'close_physical_bounds': bounds,
            'close_physical_center': [math.floor((bounds[0] + bounds[2]) / 2),
                                      math.floor((bounds[1] + bounds[3]) / 2)]}


if __name__ == '__main__':
    frame, state_path, nodes_path, receipt = map(Path, sys.argv[1:])
    result = validate(Image.open(frame), json.loads(state_path.read_text()),
                      json.loads(nodes_path.read_text()))
    result['input_sha256'] = {p.name: hashlib.sha256(p.read_bytes()).hexdigest()
                              for p in (frame, state_path, nodes_path)}
    result['input_close_delivered'] = False  # shell must deliver and verify removal
    Path(receipt).write_text(json.dumps(result, indent=2) + '\n')
