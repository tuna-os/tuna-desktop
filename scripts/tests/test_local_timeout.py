#!/usr/bin/env python3
"""Local recipe deadlines kill command groups and release machine slots."""
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
TIMEOUT = ROOT / 'scripts/lib/tuna-timeout'
SEMAPHORE = ROOT / 'scripts/lib/tuna-semaphore'


class LocalTimeout(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='local-timeout-test.')
        self.addCleanup(self.tmp.cleanup)
        self.dir = Path(self.tmp.name)
        # Accelerate only the watchdog's minute. The worker below invokes
        # /bin/sleep directly and must be stopped by the group signal.
        sleeper = self.dir / 'sleep'
        sleeper.write_text('#!/bin/sh\ncase "$1" in 60) exec /bin/sleep 0.2;; *) exec /bin/sleep "$@";; esac\n')
        sleeper.chmod(0o755)
        self.env = dict(os.environ, PATH=f'{self.dir}:{os.environ["PATH"]}')

    def run_command(self, *args):
        return subprocess.run(args, env=self.env, text=True, capture_output=True, timeout=10)

    def test_status_preserved(self):
        result = self.run_command(str(TIMEOUT), '1', 'unit test', 'sh', '-c', 'exit 3')
        self.assertEqual(result.returncode, 3, result.stderr)

    def test_timeout_kills_child_group_releases_slot(self):
        pidfile = self.dir / 'child'
        slot = self.dir / 'slot'
        result = self.run_command(
            str(SEMAPHORE), str(slot), '1', str(TIMEOUT), '1', 'unit test',
            'bash', '-c', f'/bin/sleep 600 & echo $! >{pidfile}; wait')
        self.assertEqual(result.returncode, 124, result.stderr)
        self.assertIn('unit test TIMED OUT after 1 min', result.stderr)
        child = int(pidfile.read_text())
        try:
            state = Path(f'/proc/{child}/stat').read_text().rsplit(')', 1)[1].split()[0]
        except FileNotFoundError:
            state = 'gone'
        if state not in ('Z', 'gone'):
            os.kill(child, signal.SIGKILL)
        self.assertIn(state, ('Z', 'gone'))
        result = self.run_command(str(SEMAPHORE), str(slot), '1', 'true')
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_slot_wait_is_bounded(self):
        ready = self.dir / 'ready'
        slot = self.dir / 'slot'
        holder = subprocess.Popen(
            [str(SEMAPHORE), str(slot), '1', 'sh', '-c', f'touch {ready}; exec /bin/sleep 30'],
            env=self.env, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL, start_new_session=True)
        def cleanup():
            os.killpg(holder.pid, signal.SIGKILL)
            holder.wait(timeout=5)
        self.addCleanup(cleanup)
        end = time.monotonic() + 5
        while not ready.exists():
            self.assertLess(time.monotonic(), end, 'holder did not take slot')
            time.sleep(0.01)
        env = dict(self.env, TUNA_SLOT_WAIT='0')
        result = subprocess.run([str(SEMAPHORE), str(slot), '1', 'true'],
                                env=env, capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 75, result.stderr)
        self.assertIn('no slot', result.stderr)


if __name__ == '__main__':
    unittest.main()
