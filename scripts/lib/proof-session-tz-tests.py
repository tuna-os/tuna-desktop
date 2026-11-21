#!/usr/bin/env python3
"""The proof session's zone keeps every gate inside one screen-time day,
and a real mid-bracket divergence still fails the wellbeing pixel gates."""
import calendar
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import unittest

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, HERE / file)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


TZ = load('proof_session_tz', 'proof-session-tz.py')
FRAME = load('wb_frame', 'tuna-wellbeing-frame-proof.py')
HOUR = 3600
# The longest gtk-shell proof CI admits is far below this.
PROOF_WINDOW = 6 * HOUR


def utc(hour, minute, second=0):
    return calendar.timegm((2026, 11, 21, hour, minute, second))


def wellbeing_day(seconds, offset):
    """The screen-time day `seconds` falls in, for a zone `offset` hours
    east: days start at DAY_START_HOUR local (start_of_today_in)."""
    return (seconds + (offset - TZ.DAY_START_HOUR) * HOUR) // (24 * HOUR)


class SessionZone(unittest.TestCase):
    def test_failed_run_reproduces_in_utc_and_not_in_the_session_zone(self):
        # PR #1214's gtk-shell job (136693448079) began at 02:34 UTC; its
        # both-fade-2000 bracket ran 02:59:59-03:00:01 and the day turned
        # inside it. The shell's own test of this zone is in wellbeing.rs
        # (proof_session_zone_keeps_the_day_across_the_utc_3am_boundary).
        start, before, after = utc(2, 34), utc(2, 59, 59), utc(3, 0, 1)
        self.assertNotEqual(wellbeing_day(before, 0), wellbeing_day(after, 0))
        offset = TZ.offset_hours(start)
        self.assertEqual(TZ.posix_rule(offset), '<-11>11')
        self.assertEqual(wellbeing_day(before, offset), wellbeing_day(after, offset))

    def test_no_start_minute_crosses_a_day_start_within_the_proof_window(self):
        for minute in range(24 * 60):
            start = utc(0, 0) + minute * 60
            offset = TZ.offset_hours(start)
            with self.subTest(start=minute):
                self.assertTrue(-12 < offset <= 12)
                self.assertEqual((start // 60 + offset * 60) // 60 % 24, TZ.TARGET_HOUR)
                self.assertEqual(wellbeing_day(start, offset),
                                 wellbeing_day(start + PROOF_WINDOW, offset))

    def test_utc_sessions_do_cross_it(self):
        # The control: without the zone, a proof started in the hours
        # before 03:00 UTC crosses the boundary.
        crossing = [hour for hour in range(24)
                    if wellbeing_day(utc(hour, 0), 0) != wellbeing_day(utc(hour, 0) + PROOF_WINDOW, 0)]
        self.assertEqual(crossing, [0, 1, 2, 21, 22, 23])

    def test_libc_reads_the_rule_as_the_target_hour(self):
        for hour in range(24):
            start = utc(hour, 7)
            rule = TZ.posix_rule(TZ.offset_hours(start))
            with self.subTest(rule=rule):
                shown = subprocess.run(['date', '-d', f'@{start}', '+%H:%M'], check=True,
                                       capture_output=True, text=True,
                                       env=dict(os.environ, TZ=rule)).stdout.strip()
                self.assertEqual(shown, f'{TZ.TARGET_HOUR}:07')
                local = subprocess.run(
                    [sys.executable, '-c', f'import time; print(time.localtime({start}).tm_hour)'],
                    check=True, capture_output=True, text=True,
                    env=dict(os.environ, TZ=rule)).stdout.strip()
                self.assertEqual(local, str(TZ.TARGET_HOUR))


def bracket_doc(desaturation, brightness):
    return {'animation_clock_manual': True, 'animation_clock_ms': 5000,
            'motion_policy': 'full', 'overview_open': False, 'overview_progress': 0,
            'layers': [],
            'windows': [{'id': 7, 'app_id': 'tuna-cursor-probe', 'x11_window_id': None,
                         'application_window_id': 70, 'workspace': 1, 'rect': [0, 0, 32, 32]}],
            'wellbeing_effects': {'desaturation': desaturation, 'brightness': brightness,
                                  'animating': True}}


class DivergenceStillFails(unittest.TestCase):
    OWNER = {'pid': os.getpid(), 'uid': os.getuid(), 'start_ticks': 4242, 'exe': '/bin/probe',
             'id': 7, 'application_window_id': 70, 'app_id': 'tuna-cursor-probe',
             'rect': [0, 0, 32, 32]}
    REGIONS = [{'name': 'probe', 'kind': 'texture', 'rect': [8, 8, 16, 16]}]

    def test_settled_bracket_passes(self):
        FRAME.check_bracket(bracket_doc(1.0, 0.8), bracket_doc(1.0, 0.8),
                            self.OWNER, self.REGIONS, 'both-fade-2000')

    def test_gray_lost_mid_bracket_fails(self):
        # The pinned zone removes the day turn as a cause; the bracket
        # check that caught it is unchanged and still fails on it.
        with self.assertRaisesRegex(AssertionError, 'effect sample desaturation changed mid-bracket'):
            FRAME.check_bracket(bracket_doc(1.0, 0.8), bracket_doc(0.0, 0.8),
                                self.OWNER, self.REGIONS, 'both-fade-2000')


if __name__ == '__main__':
    unittest.main()
