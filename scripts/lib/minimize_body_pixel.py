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


def switcher_absent(scene):
    """Exact namespace in the isolated compositor scene; no per-layer PID exists."""
    layers = scene.get('layers')
    assert isinstance(layers, list), 'scene layer inventory is missing'
    for layer in layers:
        assert isinstance(layer, dict) and isinstance(layer.get('namespace'), str), 'invalid layer identity'
    return not any(layer['namespace'] == 'tuna-shell-switcher' for layer in layers)


def wait_clear_restore(wait, reference, window_id, normal_rect):
    def ready(scene):
        require_restore_bracket(reference, scene, window_id, normal_rect)
        return switcher_absent(scene)
    return wait(ready, 'restore-close-switcher-absent')


def clear_capture(capture, read_scene, validate):
    """Fail closed if an overlay reappears or actor/clock identity changes."""
    before = read_scene()
    validate(before)
    assert switcher_absent(before), 'switcher present before capture'
    image = capture()
    after = read_scene()
    validate(after)
    assert switcher_absent(after), 'switcher present after capture'
    return image


def require_destroy_bracket(reference, scene, window_id):
    assert scene['animation_clock_ms'] == reference['animation_clock_ms'], 'destroy clock changed during capture'
    assert not scene['windows'], 'live window appeared during retained capture'
    expected = [e for e in reference['window_lifecycle']['destroying'] if e['window'] == window_id]
    actual = [e for e in scene['window_lifecycle']['destroying'] if e['window'] == window_id]
    assert len(expected) == len(actual) == 1 and actual == expected, 'retained actor identity/presentation changed'
    assert scene['window_lifecycle']['retained_destroy_bytes'] > 0, 'retained actor pixels disappeared'


def wait_clock_frames(wait, requested_clock, label):
    """Anchor rendering after acknowledgment, never on pre-poll frames.

    Runtime publishes the clock before rendering refreshes lifecycle samples.
    Two subsequent frames at that exact clock make those samples observable.
    The existing bounded wait owns polling; this adds no sleep or clock step.
    """
    acknowledged = wait(lambda scene: scene['animation_clock_ms'] == requested_clock,
                        label + '-ack')
    frame = acknowledged['rendered_frames']

    def settled(scene):
        assert scene['animation_clock_ms'] == requested_clock, 'acknowledged clock changed before settling'
        return scene['rendered_frames'] >= frame + 2

    return wait(settled, label)


def minimize_quarter(scene, window_id, icon):
    actors = scene['window_lifecycle']['minimizing']
    return (len(actors) == 1 and actors[0]['window'] == window_id
            and actors[0]['icon'] == icon and actors[0]['duration_ms'] == 400
            and abs(actors[0]['alpha'] - 45 / 255) < 1e-5)


def require_minimize_bracket(reference, scene, window_id, icon):
    assert scene['animation_clock_ms'] == reference['animation_clock_ms'], 'minimize clock changed during capture'
    assert minimize_quarter(reference, window_id, icon), 'reference minimize quarter missing or ambiguous'
    assert minimize_quarter(scene, window_id, icon), 'minimize quarter identity/timing changed'
    expected = reference['window_lifecycle']['minimizing'][0]
    actual = scene['window_lifecycle']['minimizing'][0]
    for field in ('rect', 'alpha', 'icon', 'duration_ms', 'live', 'monitor_sized'):
        assert actual[field] == expected[field], 'minimize actor presentation changed: ' + field


def minimize_capture(capture, read_scene, reference, window_id, icon):
    """Bind screenshot pixels to the same quarter actor on both sides."""
    require_minimize_bracket(reference, read_scene('before'), window_id, icon)
    image = capture()
    require_minimize_bracket(reference, read_scene('after'), window_id, icon)
    return image
