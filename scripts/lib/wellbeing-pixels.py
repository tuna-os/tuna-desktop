#!/usr/bin/env python3
"""Check actual scene pixels against a frozen neutral baseline; no settings acceptance."""
import argparse
import hashlib
import json
from pathlib import Path
from PIL import Image


def transform(rgb, desaturation, brightness):
    gray = sum(c * weight for c, weight in zip(rgb, (.299, .587, .114)))
    return tuple((c + (gray - c) * desaturation) * brightness for c in rgb)


def verify(baseline, current, regions, desaturation, brightness):
    assert 0 <= desaturation <= 1 and .6 <= brightness <= 1
    assert baseline.size == current.size
    assert {region['kind'] for region in regions} >= {'texture', 'solid', 'cursor'}, 'texture, solid and cursor negative control required'
    names = [region['name'] for region in regions]
    assert len(set(names)) == len(names) and len(names) >= 3
    results = []
    for region in regions:
        assert region['kind'] in ('texture', 'solid', 'cursor')
        x, y, width, height = region['rect']
        assert all(type(value) is int for value in region['rect'])
        assert width >= 4 and height >= 4 and x >= 0 and y >= 0
        assert x + width <= baseline.width and y + height <= baseline.height
        count, max_error, chroma = 0, 0, 0
        for py in range(y, y + height):
            for px in range(x, x + width):
                before = baseline.getpixel((px, py))[:3]
                after = current.getpixel((px, py))[:3]
                chroma = max(chroma, max(before) - min(before))
                expected = before if region['kind'] == 'cursor' else transform(before, desaturation, brightness)
                error = max(abs(actual - expected) for actual, expected in zip(after, expected))
                max_error = max(error, max_error)
                assert error <= 2, (region['name'], px, py, before, after, expected, error)
                count += 1
        assert chroma >= 20, (region['name'], 'neutral fixture cannot prove desaturation/bypass')
        results.append({'name': region['name'], 'kind': region['kind'], 'rect': region['rect'], 'pixels': count, 'maximum_error': max_error})
    return {'desaturation': desaturation, 'brightness': brightness, 'regions': results}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('baseline', type=Path)
    parser.add_argument('current', type=Path)
    parser.add_argument('--regions', type=Path, required=True)
    parser.add_argument('--desaturation', type=float, required=True)
    parser.add_argument('--brightness', type=float, required=True)
    parser.add_argument('--receipt', type=Path, required=True)
    args = parser.parse_args()
    with Image.open(args.baseline) as first, Image.open(args.current) as second:
        receipt = verify(first.convert('RGB'), second.convert('RGB'), json.loads(args.regions.read_text()), args.desaturation, args.brightness)
    receipt['baseline_sha256'] = hashlib.sha256(args.baseline.read_bytes()).hexdigest()
    receipt['current_sha256'] = hashlib.sha256(args.current.read_bytes()).hexdigest()
    args.receipt.write_text(json.dumps(receipt, indent=2) + '\n')
    print(json.dumps(receipt))
