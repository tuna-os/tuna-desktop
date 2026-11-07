"""await_state tolerates single flapping reads within budget (round-1004 ev24).

A tree walk can fail once while its subject churns (an a11y re-render
mid-walk, a bus re-registration) with the app alive and focused; the
poll must retry that read instead of failing the run. Persistent
breakage still fails at the deadline, with the last read error
attached to the TimeoutError.
"""
import sys
from pathlib import Path

import pytest

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


def test_flapping_read_recovers_within_budget():
    stub = StubDesktop()
    calls = []

    def read():
        calls.append(1)
        if len(calls) < 3:
            raise RuntimeError('accessibility owner changed during complete walk')
        return {'n': 3}

    assert await_on(stub, read, lambda value: value['n'] == 3) == {'n': 3}
    assert len(calls) == 3


def test_persistent_read_error_fails_with_last_error_attached():
    stub = StubDesktop()

    def read():
        raise RuntimeError('original native accessibility frame changed or departed')

    with pytest.raises(TimeoutError, match='frame changed or departed'):
        await_on(stub, read, lambda value: True)
    assert 'frame changed or departed' in stub.retained['failed-probe-label']['last_read_error']


def test_predicate_timeout_keeps_plain_message():
    stub = StubDesktop()
    with pytest.raises(TimeoutError, match='^actual readiness absolute deadline expired: probe-label$'):
        await_on(stub, lambda: {'n': 1}, lambda value: value['n'] == 2)
    assert stub.retained['failed-probe-label']['last_read_error'] is None
