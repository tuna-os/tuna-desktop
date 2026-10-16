"""Real socket deadline controls; no VM or installed-session qualification."""
import json
from pathlib import Path
import socket
import sys
import tempfile
import threading
import time
import unittest
from unittest import mock
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
from vm_guest_agent import GuestAgent


class Tests(unittest.TestCase):
    def actor(self):
        left, right = socket.socketpair()
        actor = GuestAgent.__new__(GuestAgent)
        actor.socket = left
        actor.buffer = bytearray()
        actor.poisoned = False
        actor.pending_guest_pid = None
        self.addCleanup(right.close)
        self.addCleanup(actor.close)
        return actor, right

    def server(self, peer, function):
        stop = threading.Event()
        errors = []
        def run():
            try:
                peer.settimeout(.5)
                function(peer, stop)
            except (BrokenPipeError, ConnectionResetError, OSError):
                pass
            except BaseException as error:
                errors.append(error)
        thread = threading.Thread(target=run)
        thread.start()
        def finish():
            stop.set()
            peer.close()
            thread.join(2)
            self.assertFalse(thread.is_alive(), 'owned peer thread not retired')
            self.assertEqual(errors, [])
        self.addCleanup(finish)
        return stop

    def test_actual_partial_line_dribble_obeys_absolute_deadline(self):
        actor, peer = self.actor()
        def dribble(peer, stop):
            peer.recv(65536)
            peer.sendall(b'{"return":"')
            while not stop.wait(.005):
                peer.sendall(b'x')
        self.server(peer, dribble)
        began = time.monotonic()
        with self.assertRaises(TimeoutError):
            actor.command('guest-ping', deadline=began + .08)
        self.assertLess(time.monotonic() - began, .7)
        self.assertTrue(actor.poisoned)
        self.assertEqual(actor.socket.fileno(), -1)
        with self.assertRaisesRegex(RuntimeError, 'poisoned'):
            actor.command('guest-ping')

    def test_event_flood_is_not_a_command_reply(self):
        actor, peer = self.actor()
        def flood(peer, stop):
            peer.recv(65536)
            while not stop.wait(.002):
                peer.sendall(b'{"event":"noise"}\n')
        self.server(peer, flood)
        with self.assertRaisesRegex(RuntimeError, 'envelope'):
            actor.command('guest-ping', deadline=time.monotonic() + .1)
        self.assertTrue(actor.poisoned)

    def test_actual_silent_socket_times_out_and_closes(self):
        actor, _ = self.actor()
        with self.assertRaises(TimeoutError):
            actor.command('guest-ping', deadline=time.monotonic() + .05)
        self.assertTrue(actor.poisoned)

    def test_actual_bounded_reply_success_and_oversize_refusal(self):
        actor, peer = self.actor()
        def reply(peer, stop):
            request = json.loads(peer.recv(65536))
            peer.sendall(json.dumps({'return': {'actual': True}, 'id': request['id']}).encode() + b'\n')
        self.server(peer, reply)
        self.assertEqual(actor.command('guest-ping'), {'actual': True})
        actor.MAX_REPLY = 64
        peer.sendall(b'x' * 64)
        with self.assertRaisesRegex(RuntimeError, 'oversized'):
            actor.command('guest-ping')
        self.assertTrue(actor.poisoned)

    def test_run_includes_initial_exec_time_in_all_status_deadlines(self):
        actor, _ = self.actor()
        seen = []
        def command(name, **kwargs):
            seen.append(kwargs['deadline'])
            if name == 'guest-exec':
                time.sleep(.015)
                return {'pid': 51}
            return {'exited': True, 'exitcode': 0, 'out-data': 'e30='}
        with mock.patch.object(actor, 'command', side_effect=command):
            self.assertEqual(actor.run('inventory', timeout=.1), {})
        self.assertEqual(len(set(seen)), 1)
        self.assertIsNone(actor.pending_guest_pid)
        self.assertEqual(actor.guest_exec_state, 'none')

    def test_timed_out_exec_is_not_claimed_retired(self):
        actor, _ = self.actor()
        def command(name, **kwargs):
            return {'pid': 51} if name == 'guest-exec' else {'exited': False}
        with mock.patch.object(actor, 'command', side_effect=command):
            with self.assertRaisesRegex(TimeoutError, 'retirement unconfirmed'):
                actor.run('inventory', timeout=.03)
        self.assertEqual(actor.pending_guest_pid, 51)
        self.assertEqual(actor.guest_exec_state, 'known')
        self.assertTrue(actor.poisoned)

    def test_known_exec_pid_survives_transport_timeout(self):
        actor, _ = self.actor()
        with mock.patch.object(actor, 'command', side_effect=[{'pid': 51}, TimeoutError('transport')]):
            with self.assertRaises(TimeoutError):
                actor.run('inventory', timeout=.1)
        self.assertEqual(actor.pending_guest_pid, 51)

    def test_lost_initial_exec_ack_records_unknown_child_before_send(self):
        actor, _ = self.actor()
        def command(*args, **kwargs):
            self.assertEqual(actor.guest_exec_state, 'unknown')
            raise TimeoutError('lost acknowledgement')
        with mock.patch.object(actor, 'command', side_effect=command):
            with self.assertRaises(TimeoutError):
                actor.run('inventory', timeout=.1)
        self.assertEqual(actor.guest_exec_state, 'unknown')
        self.assertIsNone(actor.pending_guest_pid)
        self.assertTrue(actor.poisoned)
        with self.assertRaisesRegex(RuntimeError, 'retirement unconfirmed'):
            actor.run('inventory')

    def test_missing_or_stale_command_id_is_never_accepted(self):
        for result in ({'return': True}, {'return': True, 'id': 'stale:1'}):
            with self.subTest(result=result):
                actor, peer = self.actor()
                peer.sendall(json.dumps(result).encode() + b'\n')
                with self.assertRaisesRegex(RuntimeError, 'correlation'):
                    actor.command('guest-ping')
                self.assertTrue(actor.poisoned)

    def test_buffered_duplicate_cannot_satisfy_next_command(self):
        actor, peer = self.actor()
        def reply(peer, stop):
            request = json.loads(peer.recv(65536))
            response = json.dumps({'return': True, 'id': request['id']}).encode() + b'\n'
            peer.sendall(response + response)
        self.server(peer, reply)
        self.assertTrue(actor.command('guest-ping'))
        with self.assertRaisesRegex(RuntimeError, 'correlation'):
            actor.command('guest-ping')
        self.assertTrue(actor.poisoned)

    def test_constructor_resets_partial_prefix_at_fragmented_delimiter(self):
        for fragmented in (False, True):
            with self.subTest(fragmented=fragmented), tempfile.TemporaryDirectory() as directory:
                path = str(Path(directory) / 'qga.sock')
                listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
                listener.bind(path)
                listener.listen(1)
                listener.settimeout(1)
                errors = []
                def server():
                    try:
                        with listener.accept()[0] as peer:
                            peer.settimeout(1)
                            request = bytearray()
                            while not request.endswith(b'\n'):
                                request.extend(peer.recv(4096))
                            nonce = json.loads(request.lstrip(b'\xff'))['arguments']['id']
                            # Valid non-object stale lines are ignored before
                            # nonce; stale partial JSON must be discarded.
                            peer.sendall(b'[]\nnull\n17\n{"stale":')
                            if fragmented:
                                peer.sendall(b'\xff')
                                time.sleep(.01)
                                peer.sendall(json.dumps({'return': nonce}).encode() + b'\n')
                            else:
                                peer.sendall(b'\xff' + json.dumps({'return': nonce}).encode() + b'\n')
                    except BaseException as error:
                        errors.append(error)
                thread = threading.Thread(target=server)
                thread.start()
                actor = None
                try:
                    actor = GuestAgent(path)
                    self.assertFalse(actor.poisoned)
                    self.assertEqual(actor.guest_exec_state, 'none')
                finally:
                    if actor is not None:
                        actor.close()
                    listener.close()
                    thread.join(2)
                self.assertFalse(thread.is_alive())
                self.assertEqual(errors, [])


if __name__ == '__main__':
    unittest.main()
