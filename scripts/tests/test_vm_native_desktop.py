"""Native desktop evidence retention and readiness verdicts; no VM or
installed-session qualification."""
import base64
import json
from pathlib import Path
import sys
import tempfile
import time
import unittest
from unittest import mock
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
import vm_guest_agent
from vm_guest_agent import GuestAgent
import vm_native_desktop
from vm_native_desktop import NativeDesktop


METADATA = {'schema': 1, 'installed_signed_release_qualified': False,
            'commit': 'a' * 40, 'package_sha256': 'b' * 64,
            'image_id': 'c' * 64, 'base_image': 'example@sha256:' + 'd' * 64,
            'member_sha256': {}}


class FakeClock:
    """monotonic()/sleep() for both driver modules: time passes only when
    the driver sleeps or the fake guest answers."""
    time_ns = staticmethod(time.time_ns)

    def __init__(self):
        self.now = 1000.0

    def monotonic(self):
        return self.now

    def sleep(self, seconds):
        assert seconds >= 0
        self.now += seconds


class FakeGuest:
    """A QGA peer on the fake clock. Each guest-exec child runs `runtime`
    seconds and prints its observation; each reply takes LATENCY, and a reply
    the request deadline cannot fit times out mid-read like the real socket."""
    LATENCY = 0.05

    def __init__(self, clock, runtime):
        self.clock, self.runtime = clock, runtime
        self.children, self.starts = {}, []
        self.next_pid, self.connections, self.observed = 6194, 0, 0

    def agent(self, deadline):
        """GuestAgent(path, deadline=deadline) over this peer."""
        actor = GuestAgent.__new__(GuestAgent)
        actor._caller_deadline, actor._path = deadline, None
        actor.buffer, actor.poisoned = bytearray(), False
        actor.pending_guest_pid, actor.guest_exec_state = None, 'none'
        actor.command_nonce, actor.command_sequence = 'tuna-qga-test', 0
        pending = []

        def connect(deadline):
            actor._remaining(deadline)
            self.connections += 1
            actor.poisoned = False
            pending.clear()

        def send(data, deadline):
            if actor.poisoned:
                raise RuntimeError('guest-agent transport poisoned')
            actor._remaining(deadline)
            pending.append(json.loads(data))

        def read(deadline):
            request = pending.pop()
            if self.clock.now + self.LATENCY > deadline:
                self.clock.now = max(self.clock.now, deadline)
                raise TimeoutError('absolute guest-agent deadline')
            self.clock.now += self.LATENCY
            return {'return': self.reply(request['execute'], request['arguments']),
                    'id': request['id']}

        actor._connect, actor._send, actor._read = connect, send, read
        actor._connect(min(self.clock.now + 5, deadline))
        return actor

    def reply(self, name, arguments):
        if name == 'guest-exec':
            self.next_pid += 1
            self.children[self.next_pid] = self.clock.now + self.runtime
            self.starts.append(self.clock.now)
            return {'pid': self.next_pid}
        assert name == 'guest-exec-status'
        if self.clock.now < self.children[arguments['pid']]:
            return {'exited': False}
        del self.children[arguments['pid']]  # QGA reaps an exited child once reported
        self.observed += 1
        value = {'locked': False, 'ime_ready': False, 'observation': self.observed}
        return {'exited': True, 'exitcode': 0,
                'out-data': base64.b64encode(json.dumps(value).encode()).decode()}


class FakeGuestCase(unittest.TestCase):
    def setUp(self):
        self.clock = FakeClock()
        for module in (vm_guest_agent, vm_native_desktop):
            patcher = mock.patch.object(module, 'time', self.clock)
            patcher.start()
            self.addCleanup(patcher.stop)
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.out = Path(tmp.name)

    def desktop(self, guest):
        return NativeDesktop(qmp=None, agent_factory=guest.agent, out=self.out,
                             metadata=METADATA, action='core-apps', source_members={})

    def retained(self, suffix):
        [path] = self.out.glob('*-' + suffix + '.json')
        return json.loads(path.read_text())


class ReadinessVerdict(FakeGuestCase):
    """#1013: a poll the readiness deadline cuts short reports readiness."""

    def await_ime(self, desktop):
        return desktop.await_state('ime-ready', lambda: desktop.guest('desktop', 'ime-state'),
                                   lambda value: value['ime_ready'], timeout=10)

    def test_doomed_poll_reports_readiness_and_keeps_the_driver(self):
        # 4 s observations in a 10 s budget: the third poll starts with too
        # little budget left, as run 41719196333's IME poll did.
        guest = FakeGuest(self.clock, runtime=4)
        desktop = self.desktop(guest)
        began = self.clock.now
        with self.assertRaisesRegex(TimeoutError, r'^actual readiness absolute deadline expired: ime-ready$') as caught:
            self.await_ime(desktop)
        self.assertLess(began + 10 - guest.starts[-1], guest.runtime)
        self.assertRegex(str(caught.exception.__cause__),
                         r'^guest lifecycle core-apps timed out\b.*; child retired late \(exit=0\)')
        self.assertIsNone(desktop._uncertain)
        self.assertEqual(guest.children, {})
        failed = self.retained('failed-ime-ready')
        self.assertEqual(failed['last_observation']['observation'], 2)
        self.assertEqual(failed['readiness_timeout_sec'], 10)
        self.assertIn('child retired late', failed['late_observation_error'])
        poll = self.retained('failed-guest-desktop-ime-state')
        self.assertEqual((poll['guest_exec_state'], poll['pending_guest_pid']), ('none', None))
        # Retirement is observed, never waited for past the grace bound.
        self.assertLessEqual(self.clock.now, began + 10 + GuestAgent.RETIREMENT_GRACE)
        # The driver still serves the next operation instead of demanding VM
        # retirement; the late child's own output (observation 3) is discarded.
        self.assertEqual(desktop.guest('desktop', 'state')['observation'], 4)

    def test_unretired_child_still_poisons_the_driver(self):
        guest = FakeGuest(self.clock, runtime=100)
        desktop = self.desktop(guest)
        began = self.clock.now
        with self.assertRaisesRegex(TimeoutError, 'child retirement unconfirmed after 10s grace'):
            self.await_ime(desktop)
        self.assertAlmostEqual(self.clock.now, began + 10 + GuestAgent.RETIREMENT_GRACE)
        self.assertEqual(desktop._uncertain['guest_exec_state'], 'known')
        self.assertIn(desktop._uncertain['pending_guest_pid'], guest.children)
        self.assertEqual(list(self.out.glob('*-failed-ime-ready.json')), [])
        with self.assertRaisesRegex(RuntimeError, 'VM retirement required'):
            desktop.guest('desktop', 'state')

    def test_timeout_with_budget_left_is_not_a_readiness_verdict(self):
        desktop = self.desktop(FakeGuest(self.clock, runtime=1))
        def hung():
            self.clock.sleep(2)
            raise TimeoutError('guest-agent acquisition deadline')
        with self.assertRaisesRegex(TimeoutError, '^guest-agent acquisition deadline$'):
            desktop.await_state('ime-ready', hung, lambda value: True, timeout=10)
        self.assertEqual(list(self.out.glob('*-failed-ime-ready.json')), [])

    def test_ready_state_within_budget_is_unchanged(self):
        guest = FakeGuest(self.clock, runtime=1)
        desktop = self.desktop(guest)
        value = desktop.await_state('ime-ready', lambda: desktop.guest('desktop', 'ime-state'),
                                    lambda value: value['observation'] == 2, timeout=10)
        self.assertEqual(value['observation'], 2)
        self.assertEqual(self.retained('ime-ready')['observed'], value)


class LateRetirement(FakeGuestCase):
    """The agent confirms a timed-out child's exit; the operation still fails."""

    def run_once(self, guest, timeout):
        actor = guest.agent(self.clock.now + 30)
        with self.assertRaises(TimeoutError) as caught:
            actor.run('core-apps', 'desktop', 'ime-state', '{}', timeout=timeout)
        self.assertTrue(actor.poisoned)
        return actor, str(caught.exception)

    def test_poll_exhausting_the_deadline_retires_on_the_same_connection(self):
        guest = FakeGuest(self.clock, runtime=1)
        actor, message = self.run_once(guest, timeout=.27)
        self.assertEqual(message, 'guest lifecycle core-apps timed out; '
                                  'child retired late (exit=0) within 10s grace')
        self.assertEqual((actor.guest_exec_state, actor.pending_guest_pid), ('none', None))
        self.assertEqual((guest.connections, guest.children), (1, {}))

    def test_reply_cut_mid_read_retires_on_a_fresh_connection(self):
        guest = FakeGuest(self.clock, runtime=1)
        actor, message = self.run_once(guest, timeout=.23)
        self.assertRegex(message, r'^guest lifecycle core-apps timed out awaiting guest-exec-status '
                                  r'for pid 6195 \(absolute guest-agent deadline\); '
                                  r'child retired late \(exit=0\) within 10s grace$')
        self.assertEqual((actor.guest_exec_state, actor.pending_guest_pid), ('none', None))
        # The poisoned stream is never reused; guest-sync on a new one drops the stale reply.
        self.assertEqual((guest.connections, guest.children), (2, {}))

    def test_grace_is_bounded_and_keeps_an_unretired_child(self):
        guest = FakeGuest(self.clock, runtime=100)
        began = self.clock.now
        actor, message = self.run_once(guest, timeout=.27)
        self.assertRegex(message, r'child retirement unconfirmed after 10s grace \(TimeoutError\)$')
        self.assertAlmostEqual(self.clock.now, began + .27 + GuestAgent.RETIREMENT_GRACE)
        self.assertEqual((actor.guest_exec_state, actor.pending_guest_pid), ('known', 6195))
        self.assertIn(6195, guest.children)
        with self.assertRaisesRegex(RuntimeError, 'retirement unconfirmed'):
            actor.run('core-apps', 'desktop', 'state', '{}')


class FailureMessage(unittest.TestCase):
    def desktop(self, out):
        metadata = {'schema': 1, 'installed_signed_release_qualified': False,
                    'commit': 'a' * 40, 'package_sha256': 'b' * 64,
                    'image_id': 'c' * 64, 'base_image': 'example@sha256:' + 'd' * 64,
                    'member_sha256': {}}
        return NativeDesktop(qmp=None, agent_factory=None, out=out, metadata=metadata,
                             action='autotz', source_members={})

    def test_failed_guest_record_carries_bounded_message(self):
        with tempfile.TemporaryDirectory() as tmp:
            desktop = self.desktop(tmp)
            agent = mock.Mock()
            agent.run.side_effect = RuntimeError('guest lifecycle autotz failed (exit=1)')
            agent.guest_exec_state = 'none'
            agent.pending_guest_pid = None
            desktop._agent_factory = lambda deadline: agent
            with self.assertRaises(RuntimeError):
                desktop.guest('autotz', 'watch-start')
            retained = json.loads(Path(tmp, '00001-failed-guest-autotz-watch-start.json').read_text())
            self.assertEqual(retained['error_type'], 'RuntimeError')
            self.assertEqual(retained['message'], 'guest lifecycle autotz failed (exit=1)')
            self.assertTrue(retained['not_replayed'])

    def test_failed_message_is_bounded_not_dropped(self):
        with tempfile.TemporaryDirectory() as tmp:
            desktop = self.desktop(tmp)
            agent = mock.Mock()
            agent.run.side_effect = RuntimeError('y' * 5000)
            agent.guest_exec_state = 'none'
            agent.pending_guest_pid = None
            desktop._agent_factory = lambda deadline: agent
            with self.assertRaises(RuntimeError):
                desktop.guest('autotz', 'snapshot')
            retained = json.loads(Path(tmp, '00001-failed-guest-autotz-snapshot.json').read_text())
            self.assertEqual(retained['message'], 'y' * 2000)


if __name__ == '__main__':
    unittest.main()
