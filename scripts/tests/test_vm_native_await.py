"""await_state tolerates single flapping reads within budget (round-1004 ev24).

A tree walk can fail once while its subject churns (an a11y re-render
mid-walk, a bus re-registration) with the app alive and focused; the
poll must retry that read instead of failing the run. Persistent
breakage still fails at the deadline, with the last read error
attached to the TimeoutError.
"""
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))

from vm_native_desktop import NativeDesktop


class StubDesktop:
    def __init__(self):
        self._deadline = None
        self.retained = {}

    def retain(self, label, value):
        self.retained[label] = value
        return label


def await_on(stub, read, predicate, timeout=10):
    return NativeDesktop.await_state(stub, 'probe-label', read, predicate, timeout=timeout)


class AwaitStateFlappingReads(unittest.TestCase):
    def test_flapping_read_recovers_within_budget(self):
        stub = StubDesktop()
        calls = []

        def read():
            calls.append(1)
            if len(calls) < 3:
                raise RuntimeError('accessibility owner changed during complete walk')
            return {'n': 3}

        self.assertEqual(await_on(stub, read, lambda value: value['n'] == 3), {'n': 3})
        self.assertEqual(len(calls), 3)

    def test_persistent_read_error_fails_with_last_error_attached(self):
        stub = StubDesktop()

        def read():
            raise RuntimeError('original native accessibility frame changed or departed')

        with self.assertRaisesRegex(TimeoutError, 'frame changed or departed'):
            await_on(stub, read, lambda value: True)
        self.assertIn('frame changed or departed',
                      stub.retained['failed-probe-label']['last_read_error'])

    def test_predicate_timeout_keeps_plain_message(self):
        stub = StubDesktop()
        with self.assertRaisesRegex(
                TimeoutError, '^actual readiness absolute deadline expired: probe-label$'):
            await_on(stub, lambda: {'n': 1}, lambda value: value['n'] == 2)
        self.assertIsNone(stub.retained['failed-probe-label']['last_read_error'])


if __name__ == '__main__':
    unittest.main()
