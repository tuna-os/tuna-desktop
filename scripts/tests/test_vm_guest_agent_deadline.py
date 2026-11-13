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
from vm_guest_agent import GuestAgent, valid_autotz_error


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

    def test_reboot_transmits_once_without_reply_and_retires_transport(self):
        actor, peer = self.actor()
        received = bytearray()
        retired = threading.Event()
        def silent(peer, stop):
            while True:
                data = peer.recv(65536)
                if not data:
                    retired.set()
                    return
                received.extend(data)
        self.server(peer, silent)
        with mock.patch.object(actor, '_send', wraps=actor._send) as send, \
                mock.patch.object(actor, '_read', wraps=actor._read) as read, \
                mock.patch.object(actor, 'command', wraps=actor.command) as command:
            self.assertIsNone(actor.request_reboot(timeout=.1))
            send.assert_called_once()
            read.assert_not_called()
            command.assert_not_called()
        self.assertTrue(retired.wait(1), 'reboot transport did not close')
        self.assertEqual(bytes(received),
                         b'{"execute": "guest-shutdown", "arguments": {"mode": "reboot"}}\n')
        self.assertTrue(actor.poisoned)
        self.assertEqual(actor.socket.fileno(), -1)
        self.assertIsNone(actor.pending_guest_pid)
        self.assertEqual(getattr(actor, 'guest_exec_state', 'none'), 'none')
        for operation in (actor.request_reboot, lambda: actor.command('guest-ping')):
            with self.subTest(operation=operation):
                with self.assertRaisesRegex(RuntimeError, 'poisoned'):
                    operation()
        self.assertEqual(bytes(received).count(b'\n'), 1)

    def test_reboot_send_failure_propagates_and_retires_transport(self):
        actor, peer = self.actor()
        peer.close()
        with mock.patch.object(actor, '_send', wraps=actor._send) as send:
            with self.assertRaises(BrokenPipeError):
                actor.request_reboot()
            send.assert_called_once()
        self.assertTrue(actor.poisoned)
        self.assertEqual(actor.socket.fileno(), -1)
        self.assertIsNone(actor.pending_guest_pid)
        with self.assertRaisesRegex(RuntimeError, 'poisoned'):
            actor.request_reboot()

    def test_reboot_expired_absolute_deadline_sends_nothing_and_retires(self):
        actor, peer = self.actor()
        # The deadline expires between its creation and the real _send check.
        with mock.patch('vm_guest_agent.time.monotonic', side_effect=[10., 11.]), \
                mock.patch.object(actor, '_send', wraps=actor._send) as send:
            with self.assertRaisesRegex(TimeoutError, 'absolute guest-agent deadline'):
                actor.request_reboot(timeout=.5)
            send.assert_called_once_with(
                b'{"execute": "guest-shutdown", "arguments": {"mode": "reboot"}}\n', 10.5)
        self.assertTrue(actor.poisoned)
        self.assertEqual(actor.socket.fileno(), -1)
        peer.settimeout(.5)
        self.assertEqual(peer.recv(65536), b'')
        with self.assertRaisesRegex(RuntimeError, 'poisoned'):
            actor.request_reboot()

    def test_reboot_refuses_owned_exec_without_sending_or_clearing_ownership(self):
        for state, pid in (('known', 51), ('unknown', None), ('none', 51)):
            with self.subTest(state=state, pid=pid):
                actor, peer = self.actor()
                actor.guest_exec_state = state
                actor.pending_guest_pid = pid
                actor.buffer.extend(b'owned transport data')
                with mock.patch.object(actor, '_send', wraps=actor._send) as send:
                    with self.assertRaisesRegex(RuntimeError, 'retirement unconfirmed'):
                        actor.request_reboot()
                    send.assert_not_called()
                self.assertEqual(actor.guest_exec_state, state)
                self.assertEqual(actor.pending_guest_pid, pid)
                self.assertEqual(actor.buffer, b'owned transport data')
                self.assertFalse(actor.poisoned)
                self.assertNotEqual(actor.socket.fileno(), -1)
                peer.setblocking(False)
                with self.assertRaises(BlockingIOError):
                    peer.recv(65536)

    def test_reboot_refuses_poisoned_actor_without_sending_or_clearing_ownership(self):
        actor, peer = self.actor()
        actor.guest_exec_state = 'known'
        actor.pending_guest_pid = 51
        actor.close()
        with mock.patch.object(actor, '_send', wraps=actor._send) as send:
            with self.assertRaisesRegex(RuntimeError, 'poisoned'):
                actor.request_reboot()
            send.assert_not_called()
        self.assertEqual(actor.guest_exec_state, 'known')
        self.assertEqual(actor.pending_guest_pid, 51)
        self.assertEqual(actor.socket.fileno(), -1)
        peer.settimeout(.5)
        self.assertEqual(peer.recv(65536), b'')

    def test_ordinary_shutdown_command_still_requires_a_reply(self):
        actor, peer = self.actor()
        received = bytearray()
        retired = threading.Event()
        def silent(peer, stop):
            while True:
                data = peer.recv(65536)
                if not data:
                    retired.set()
                    return
                received.extend(data)
        self.server(peer, silent)
        with self.assertRaises(TimeoutError):
            actor.command('guest-shutdown', mode='reboot', deadline=time.monotonic() + .05)
        self.assertTrue(retired.wait(1), 'timed-out ordinary transport did not close')
        request = json.loads(received)
        self.assertEqual(set(request), {'execute', 'arguments', 'id'})
        self.assertEqual(request['execute'], 'guest-shutdown')
        self.assertEqual(request['arguments'], {'mode': 'reboot'})
        self.assertIsInstance(request['id'], str)
        self.assertEqual(bytes(received).count(b'\n'), 1)
        self.assertTrue(actor.poisoned)
        self.assertEqual(actor.socket.fileno(), -1)

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

    def test_caller_deadline_bounds_actual_nested_exec_socket(self):
        actor, peer = self.actor()
        def exec_then_stall(peer, stop):
            request = json.loads(peer.recv(65536))
            peer.sendall(json.dumps({'return': {'pid': 51}, 'id': request['id']}).encode() + b'\n')
            peer.recv(65536)
            stop.wait(.4)
        self.server(peer, exec_then_stall)
        began = time.monotonic()
        actor._caller_deadline = began + .05
        with self.assertRaises(TimeoutError):
            actor.run('explicit-sync', 'state', '{}', timeout=30)
        self.assertLess(time.monotonic() - began, .7)
        self.assertEqual(actor.pending_guest_pid, 51)
        self.assertEqual(actor.guest_exec_state, 'known')
        self.assertTrue(actor.poisoned)

    def test_caller_deadline_caps_longer_nested_command(self):
        actor, _ = self.actor()
        began = time.monotonic()
        actor._caller_deadline = began + .05
        with self.assertRaises(TimeoutError):
            actor.command('guest-ping', deadline=began + 30)
        self.assertLess(time.monotonic() - began, .7)
        self.assertTrue(actor.poisoned)

    def test_autotz_diagnostics_reject_private_or_unbounded_fields(self):
        value = {'domain': 'autotz', 'operation': 'snapshot',
                 'exception_type': 'RuntimeError',
                 'sites': [{'module': 'autotz.py', 'line': 42, 'function': 'dispatch'}]}
        self.assertTrue(valid_autotz_error(value))
        self.assertTrue(valid_autotz_error({**value, 'message': 'bounded guest detail'}))
        self.assertTrue(valid_autotz_error({**value, 'message': 'x' * 2000}))
        for key, invalid in (('traceback', 'private'), ('operation', '../private'),
                             ('sites', value['sites'] * 5), ('errno', True),
                             ('message', 'x' * 2001), ('message', 42)):
            with self.subTest(key=key):
                self.assertFalse(valid_autotz_error({**value, key: invalid}))

    def test_reference_invalid_operation_never_starts_guest_child(self):
        actor, _ = self.actor()
        for arguments, timeout in (
                (("exec", "{}"), 30), (("observe", "[]"), 30),
                (("observe", "{}"), 31), (("observe", "{}"), 0),
                (("observe", "{}"), float("nan")), (("observe",), 30),
                (("prepare", "{}"), 251), (("prepare", "{}"), 0)):
            with self.subTest(arguments=arguments, timeout=timeout):
                with mock.patch.object(actor, "command") as command:
                    with self.assertRaises(ValueError):
                        actor.run("gnome-reference", *arguments, timeout=timeout)
                    command.assert_not_called()
                self.assertIsNone(actor.pending_guest_pid)
                self.assertFalse(actor.poisoned)

    def test_reference_prepare_headroom_starts_child_within_cap(self):
        actor, _ = self.actor()

        def command(name, **kwargs):
            if name == 'guest-exec':
                return {'pid': 51}
            return {'exited': True, 'exitcode': 0, 'out-data': 'e30='}

        with mock.patch.object(actor, 'command', side_effect=command) as started:
            self.assertEqual(actor.run("gnome-reference", "prepare", "{}", timeout=250), {})
        started.assert_called()
        self.assertIsNone(actor.pending_guest_pid)

    def test_reference_overview_starts_child_within_default_cap(self):
        actor, _ = self.actor()

        def command(name, **kwargs):
            if name == 'guest-exec':
                return {'pid': 52}
            return {'exited': True, 'exitcode': 0, 'out-data': 'e30='}

        with mock.patch.object(actor, 'command', side_effect=command) as started:
            self.assertEqual(actor.run("gnome-reference", "overview", "{}", timeout=30), {})
        started.assert_called()
        self.assertIsNone(actor.pending_guest_pid)
        with mock.patch.object(actor, 'command') as refused:
            with self.assertRaises(ValueError):
                actor.run("gnome-reference", "overview", "{}", timeout=31)
            refused.assert_not_called()

    def test_reference_timeout_preserves_original_child_for_retirement(self):
        actor, _ = self.actor()
        def command(name, **kwargs):
            if name == "guest-exec":
                self.assertEqual(kwargs["path"], "/usr/libexec/tuna-vm-gnome-reference")
                self.assertEqual(kwargs["arg"], ["observe", '{"nonce":"owned"}'])
                return {"pid": 52}
            return {"exited": False}
        with mock.patch.object(actor, "command", side_effect=command):
            with self.assertRaisesRegex(TimeoutError, "retirement unconfirmed"):
                actor.run("gnome-reference", "observe", '{"nonce":"owned"}', timeout=.03)
        self.assertEqual(actor.pending_guest_pid, 52)
        self.assertEqual(actor.guest_exec_state, "known")
        self.assertTrue(actor.poisoned)

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
