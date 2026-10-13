#!/usr/bin/env python3
"""No GUI or input injection: settled bounds and absolute deadline regressions."""
import importlib.util
from pathlib import Path
import sys
import tempfile
import time
import json
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
spec = importlib.util.spec_from_file_location('settled', Path(__file__).with_name('grid-target-settled.py'))
settled = importlib.util.module_from_spec(spec)
spec.loader.exec_module(settled)


class Clock:
    def __init__(self):
        self.now = 0.0

    def __call__(self):
        return self.now

    def sleep(self, seconds):
        self.now += seconds


def tree(bounds, pid=42):
    return [{'process_id': pid}, {'role': 'push button', 'name': 'AAA Move Two',
                                 'showing': True, 'bounds': bounds}]


class SettledTarget(unittest.TestCase):
    def bounds(self, nodes):
        return settled.target_bounds(nodes, 'AAA Move Two', 42, (0, 232), (1280, 800))

    def test_actual_moving_target_179_then_158_never_accepts_early(self):
        clock = Clock()
        samples = iter([self.bounds(tree([179, 36, 113, 113])),
                        self.bounds(tree([158, 36, 113, 113])),
                        self.bounds(tree([158, 36, 113, 113]))])
        observed = []
        result = settled.wait_for_target(lambda remaining: next(samples), timeout=1,
                                         clock=clock, sleep=clock.sleep, record=observed.append)
        self.assertEqual(result, (158, 36, 113, 113))
        self.assertEqual(len(observed), 3)
        self.assertGreaterEqual(clock.now, 0.4)
        # Snapshot179+4=183 would land25px inside settled158, creating a
        # folder. The admitted158+4=162 stays inside the20px reorder edge.
        self.assertGreater(179 + 4 - result[0], 20)
        self.assertLess(result[0] + 4 - 158, 20)

    def test_partial_missing_duplicate_and_wrong_owner_never_qualify(self):
        for nodes in [tree([1270, 36, 113, 113]), tree([-1, 36, 113, 113]),
                      tree([158, 600, 113, 113]), [], tree([158, 36, 113, 113], pid=7),
                      tree([158, 36, 113, 113]) + tree([158, 36, 113, 113])[1:]]:
            self.assertIsNone(self.bounds(nodes))

    def test_missing_sample_resets_stability(self):
        clock = Clock()
        samples = iter([(158, 36, 113, 113), None, (158, 36, 113, 113), (158, 36, 113, 113)])
        self.assertEqual(settled.wait_for_target(lambda _: next(samples), timeout=2,
                                               clock=clock, sleep=clock.sleep), (158, 36, 113, 113))
        self.assertGreaterEqual(clock.now, 0.6)

    def test_moving_and_missing_fail_at_absolute_deadline(self):
        for missing in [False, True]:
            clock = Clock()
            remaining_seen = []
            def capture(remaining):
                remaining_seen.append(remaining)
                return None if missing else (round(clock.now * 100), 36, 113, 113)
            with self.assertRaises(TimeoutError):
                settled.wait_for_target(capture, timeout=1, clock=clock, sleep=clock.sleep)
            self.assertAlmostEqual(clock.now, 1)
            self.assertTrue(all(0 < r <= 1 for r in remaining_seen))

    def test_capture_consuming_deadline_cannot_qualify(self):
        clock = Clock()
        def capture(remaining):
            clock.sleep(remaining)
            return (158, 36, 113, 113)
        with self.assertRaises(TimeoutError):
            settled.wait_for_target(capture, timeout=1, clock=clock, sleep=clock.sleep)
        self.assertEqual(clock.now, 1)

    def test_blocked_dump_is_killed_and_failed_receipt_cannot_reuse_old_tree(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            dump, out, receipt = root / 'dump.py', root / 'tree.json', root / 'receipt.json'
            dump.write_text('import time; time.sleep(2)\n')
            out.write_text(json.dumps(tree([158, 36, 113, 113])))
            argv = ['settled', '--dump', str(dump), '--out', str(out),
                    '--receipt', str(receipt), '--name', 'AAA Move Two', '--pid', '42',
                    '--origin', '0', '232', '--viewport', '1280', '800', '--timeout', '0.3']
            before = time.monotonic()
            with patch.object(sys, 'argv', argv):
                self.assertEqual(settled.main(), 1)
            self.assertLess(time.monotonic() - before, 1)
            self.assertFalse(json.loads(receipt.read_text())['qualified'])
            self.assertFalse(out.exists())


if __name__ == '__main__':
    unittest.main()
