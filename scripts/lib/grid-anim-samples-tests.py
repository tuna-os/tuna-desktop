#!/usr/bin/env python3
"""No GUI: the app grid's trace samples checked against GNOME 51's curves."""
import importlib.util
from pathlib import Path
import sys
import tempfile
import unittest

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location(
    'samples', Path(__file__).with_name('grid-anim-samples.py'))
samples = importlib.util.module_from_spec(spec)
spec.loader.exec_module(samples)

PREFIX = 'tuna-shell-gtk: grid-anim '


def dots_line(position, n=2, hover=(), pressed=(), skew=0.0):
    items = []
    for i in range(n):
        scale, opacity = samples.dot_look(position, i, i in hover, i in pressed)
        items.append(f'{i}:{scale + skew:.6f}:{opacity:.6f}:{int(i in hover)}:{int(i in pressed)}')
    return f'{PREFIX}page-dots position={position:.6f} dots={",".join(items)}'


def switch_log(start=0.0, end=1.0, times=(0.0, 0.0, 16.7, 33.4, 150.0, 283.5, 300.2),
               curve=None, ms=300.0, dots=True):
    curve = curve or (lambda e: samples.expected_position(start, end, e))
    lines = []
    for elapsed in times:
        position = curve(elapsed)
        lines.append(f'{PREFIX}page-switch frame elapsed={elapsed:.3f} position={position:.6f} '
                     f'from={start:.6f} to={end:.6f} ms={ms:.3f}')
        if dots:
            lines.append(dots_line(position))
    return '\n'.join(lines) + '\n'


def check(text, start=0.0, end=1.0):
    frames, dots = samples.parse(text)
    return samples.check_switch(frames, dots, start, end)


class CurveTests(unittest.TestCase):
    def test_gnome_constants(self):
        self.assertEqual(samples.PAGE_SWITCH_MS, 300.0)
        self.assertAlmostEqual(samples.expected_position(0.0, 1.0, 150.0), 0.875)
        self.assertAlmostEqual(samples.expected_position(2.0, 1.0, 150.0), 1.125)
        self.assertEqual(samples.expected_position(0.0, 1.0, 400.0), 1.0)
        self.assertEqual(samples.dot_look(0.0, 0, False, False), (1.0, 1.0))
        self.assertEqual(samples.dot_look(0.0, 1, False, False), (2.0 / 3.0, 128.0 / 255.0))
        self.assertEqual(samples.dot_look(0.0, 1, True, True), (0.5, 1.0))


class SwitchTests(unittest.TestCase):
    def test_gnome_switch_passes(self):
        self.assertEqual(check(switch_log()), [])
        self.assertEqual(check(switch_log(start=1.0, end=0.0), start=1.0, end=0.0), [])

    def test_a_linear_slide_fails(self):
        errors = check(switch_log(curve=lambda e: min(e / 300.0, 1.0)))
        self.assertTrue(any('ease-out-cubic' in e for e in errors), errors)

    def test_another_duration_fails(self):
        times = (0.0, 50.0, 100.0, 200.2)
        log = switch_log(times=times, ms=200.0,
                         curve=lambda e: samples.out_cubic(e / 200.0))
        errors = check(log)
        self.assertTrue(any("not GNOME's 300 ms" in e for e in errors), errors)

    def test_a_jump_fails(self):
        errors = check(switch_log(times=(0.0, 300.0)))
        self.assertTrue(any('frames between the pages' in e for e in errors), errors)

    def test_an_unfinished_switch_fails(self):
        errors = check(switch_log(times=(0.0, 16.7, 33.4, 150.0)))
        self.assertTrue(any('last frame' in e for e in errors), errors)

    def test_dots_must_follow_the_pages(self):
        errors = check(switch_log(dots=False))
        self.assertIn('no page-dots samples', errors)
        log = switch_log(dots=False) + dots_line(0.0) + '\n' + dots_line(1.0) + '\n'
        errors = check(log)
        self.assertTrue(any('page-dots samples between' in e for e in errors), errors)
        skewed = switch_log() + dots_line(1.0, skew=0.01) + '\n'
        errors = check(skewed)
        self.assertTrue(any('dot 0 at position 1.0' in e for e in errors), errors)

    def test_hover_and_pressed_dots_are_gnome_looks(self):
        log = switch_log() + dots_line(1.0, hover=(0,), pressed=(0,)) + '\n'
        self.assertEqual(check(log), [])


class InstantTests(unittest.TestCase):
    def test_off_jumps(self):
        frames, dots = samples.parse(dots_line(0.0) + '\n')
        self.assertEqual(samples.check_instant(frames, dots, 0.0), [])

    def test_off_with_frames_or_passing_dots_fails(self):
        frames, dots = samples.parse(switch_log(start=1.0, end=0.0))
        errors = samples.check_instant(frames, dots, 0.0)
        self.assertTrue(any('animations off' in e for e in errors), errors)
        frames, dots = samples.parse('')
        self.assertIn('no page-dots samples', samples.check_instant(frames, dots, 0.0))


class CommandTests(unittest.TestCase):
    def test_reads_from_the_offset(self):
        with tempfile.NamedTemporaryFile('w', suffix='.log', delete=False) as log:
            log.write(switch_log(times=(0.0, 300.0)))
            offset = log.tell()
            log.write(switch_log())
        self.assertEqual(samples.main(['--log', log.name, '--from', '0', '--to', '1']), 1)
        self.assertEqual(samples.main(['--log', log.name, '--offset', str(offset),
                                       '--from', '0', '--to', '1']), 0)
        Path(log.name).unlink()


if __name__ == '__main__':
    unittest.main()
