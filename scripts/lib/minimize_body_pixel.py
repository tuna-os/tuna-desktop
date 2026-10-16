"""Deterministic client-body probes that exclude observed switcher overlays."""
import math


def rect(value):
    if (not isinstance(value, (list, tuple)) or len(value) != 4
            or any(type(n) not in (int, float) or not math.isfinite(n) for n in value)
            or value[2] <= 0 or value[3] <= 0):
        raise AssertionError('missing or invalid probe/overlay geometry')
    return value


def unobscured_body_point(body, image_size, scenes):
    x, y, width, height = rect(body)
    if len(scenes) != 2:
        raise AssertionError('both screenshot scene brackets are required')
    overlays = []
    for scene in scenes:
        if not isinstance(scene.get('layers'), list):
            raise AssertionError('scene layer geometry is missing')
        for layer in scene['layers']:
            if layer.get('namespace') == 'tuna-shell-switcher':
                overlays.append(rect(layer.get('rect')))
    for fraction in (0.7, 0.3):
        point = (round(x + width * fraction), round(y + height * 0.8))
        px, py = point
        # The solid blue lower body is the lower half of the owned client.
        # Require an interior client point and an actual framebuffer pixel.
        if not (x + 1 < px < x + width - 1 and y + height / 2 + 1 < py < y + height - 1
                and 0 <= px < image_size[0] and 0 <= py < image_size[1]):
            continue
        if any(ox <= px <= ox + ow and oy <= py <= oy + oh for ox, oy, ow, oh in overlays):
            continue
        return point
    raise AssertionError('no unobscured in-frame lower-body probe')


def require_rgb(actual, expected, context):
    assert all(abs(actual[i] - expected[i]) <= 4 for i in range(3)), (actual, expected, context)


def require_restore_bracket(reference, scene, window_id, normal_rect):
    assert scene['animation_clock_ms'] == reference['animation_clock_ms'], 'restore clock changed during capture'
    expected = [e for e in reference['window_lifecycle']['restoring'] if e['window'] == window_id]
    actual = [e for e in scene['window_lifecycle']['restoring'] if e['window'] == window_id]
    assert len(expected) == len(actual) == 1, 'restore identity missing or ambiguous'
    for effect in (expected[0], actual[0]):
        assert effect['elapsed_ms'] == 100 and effect['duration_ms'] == 400, 'restore quarter timing changed'
    assert actual[0]['icon'] == expected[0]['icon'], 'restore icon geometry changed during capture'
    windows = [w for w in scene['windows'] if w['id'] == window_id]
    assert len(windows) == 1 and windows[0]['rect'] == normal_rect, 'restore body identity/geometry changed'
