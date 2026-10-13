"""Explicit synthetic validator controls; no guest or framebuffer qualification."""
import copy
import hashlib
import importlib.machinery
import importlib.util
import json
import socket
import tempfile
import threading
from types import SimpleNamespace
from pathlib import Path
import sys
import time
import unittest
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'lib'))
import vm_strip_acceptance as proof


def sample():
    nonce = 'a' * 32
    identity = {'pid': 20, 'uid': 1000, 'start_ticks': 99, 'boot_id': 'boot',
                'exe_inode': 3, 'exe_device': 4, 'exe_sha256': 'b' * 64, 'script_sha256': 'c' * 64}
    core = {'tuna-compositor': {'pid': 10, 'uid': 1000, 'start_ticks': 10},
            'tuna-shell-gtk': {'pid': 11, 'uid': 1000, 'start_ticks': 11}}
    owner = {'fixture_mode': 'baseline', 'nonce': nonce, 'boot_id': 'boot', 'client': identity, 'core': core, 'launch_state': 'known'}
    roles = {}
    windows = []
    for index, role in enumerate(proof.ROLES):
        app = f'org.tuna.VmStrip.n{nonce}.{role}'
        roles[role] = {'app_id': app, 'mapped': True, 'paint_count': 1, 'width': 240, 'height': 160,
                       'painted_width': 240, 'painted_height': 160, 'color': list(proof.COLORS[role])}
        windows.append({'id': index + 1, 'app_id': app, 'rect': [16 + index * 256, 48, 240, 160]})
    return {'owner': owner, 'fixture': {'nonce': nonce, 'identity': copy.deepcopy(identity), 'backend': 'wayland',
                                      'schema': 1, 'fixture_mode': 'baseline', 'windows': roles, 'sequence': 5},
            'inventory': {'processes': [{'executable': name, **value} for name, value in core.items()],
                          'state': {'locked': False, 'overview_open': False, 'session_mode': 'scroll',
                                    'rendered_frames': 20, 'strip_animating': False, 'windows': windows}}}


class Tests(unittest.TestCase):
    def test_all_actual_role_allocation_and_process_fields_required(self):
        value = sample()
        self.assertEqual(set(proof.windows(value, copy.deepcopy(value['owner']))), set(proof.ROLES))
        for field, replacement in [('pid', 999), ('start_ticks', 999), ('boot_id', 'other'),
                                   ('exe_sha256', 'f' * 64), ('script_sha256', 'f' * 64)]:
            bad = copy.deepcopy(value)
            bad['fixture']['identity'][field] = replacement
            with self.subTest(field=field), self.assertRaises(RuntimeError):
                proof.windows(bad, copy.deepcopy(value['owner']))

    def test_malformed_flags_schema_sequence_and_counters_never_qualify(self):
        value = sample()
        for field, replacements in [('mapped', [0, 1, 'true']),
                                    ('paint_count', [True, False, -1, 0, 1.5]),
                                    ('width', [True, -1]), ('height', [True, -1]),
                                    ('painted_width', [True, -1]), ('painted_height', [True, -1])]:
            for replacement in replacements:
                bad = copy.deepcopy(value)
                bad['fixture']['windows']['alpha'][field] = replacement
                with self.subTest(field=field, value=replacement), self.assertRaises(RuntimeError):
                    proof.windows(bad, value['owner'])
        for container, field, replacements in [('fixture', 'schema', [True, False]),
                                               ('fixture', 'sequence', [True, 0, -1]),
                                               ('state', 'rendered_frames', [True, 0, -1]),
                                               ('state', 'locked', [0, 1]),
                                               ('state', 'overview_open', [0, 1])]:
            for replacement in replacements:
                bad = copy.deepcopy(value)
                target = bad['fixture'] if container == 'fixture' else bad['inventory']['state']
                target[field] = replacement
                with self.subTest(field=field, value=replacement), self.assertRaises(RuntimeError):
                    proof.windows(bad, value['owner'])

    def test_partial_map_or_allocation_cannot_be_completed_endpoint(self):
        value = sample()
        for role in proof.ROLES:
            for field, replacement in [('mapped', False), ('paint_count', 0), ('painted_width', 111), ('height', 10)]:
                bad = copy.deepcopy(value)
                bad['fixture']['windows'][role][field] = replacement
                with self.subTest(role=role, field=field), self.assertRaises(proof.NotReady):
                    proof.windows(bad, value['owner'])
        bad = copy.deepcopy(value)
        bad['inventory']['state']['windows'].pop()
        with self.assertRaises(proof.NotReady):
            proof.windows(bad, value['owner'])

    def test_wrong_core_or_restarted_native_window_is_terminal(self):
        value = sample()
        old = copy.deepcopy(value)
        bad = copy.deepcopy(value)
        bad['inventory']['processes'][0]['pid'] += 1
        with self.assertRaisesRegex(RuntimeError, 'process identity'):
            proof.windows(bad, value['owner'])
        bad = copy.deepcopy(value)
        bad['inventory']['state']['windows'][0]['id'] = 99
        bad['inventory']['state']['rendered_frames'] += 1
        with self.assertRaisesRegex(RuntimeError, 'remapped'):
            proof.endpoint(bad, value['owner'], mode='scroll', previous=old)

    def test_opposite_mode_stale_frame_animation_and_lock_never_pass(self):
        value = sample()
        for key, replacement in [('session_mode', 'gnome'), ('strip_animating', True),
                                 ('locked', True), ('overview_open', True)]:
            bad = copy.deepcopy(value)
            bad['inventory']['state'][key] = replacement
            with self.subTest(key=key), self.assertRaises(proof.NotReady):
                proof.endpoint(bad, value['owner'], mode='scroll')
        with self.assertRaises(proof.NotReady):
            proof.endpoint(value, value['owner'], mode='scroll', previous=copy.deepcopy(value))

    def test_actual_pixel_body_required_not_any_changed_frame(self):
        color = (44, 102, 164)
        good = (320, 240, bytes(color) * 320 * 240)
        self.assertTrue(proof.colored_frame(good, [0, 0, 240, 160], color))
        wrong = (320, 240, bytes((171, 65, 81)) * 320 * 240)
        with self.assertRaises(proof.NotReady):
            proof.colored_frame(wrong, [0, 0, 240, 160], color)
        with self.assertRaises(proof.NotReady):
            proof.colored_frame(good, [1000, 0, 240, 160], color)

    def test_partial_then_complete_poll_requires_three_real_stable_samples(self):
        value = sample()
        bad = copy.deepcopy(value)
        bad['fixture']['windows']['alpha']['mapped'] = False
        queue = [bad, value, value, value]
        def read(deadline):
            self.assertGreater(deadline, time.monotonic())
            return queue.pop(0)
        result, observations = proof.await_endpoint(read, value['owner'],
            lambda actual, original, previous: proof.endpoint(actual, original, mode='scroll'),
            deadline=time.monotonic() + 1)
        self.assertEqual(result, value)
        self.assertEqual(len(observations), 4)
        self.assertEqual(queue, [])

    def test_wrong_identity_does_not_retry_or_turn_into_readiness(self):
        value = sample()
        wrong = copy.deepcopy(value)
        wrong['owner']['nonce'] = 'f' * 32
        calls = []
        with self.assertRaises(RuntimeError):
            proof.await_endpoint(lambda _: (calls.append(1) or wrong), value['owner'],
                lambda actual, original, previous: proof.endpoint(actual, original, mode='scroll'),
                deadline=time.monotonic() + 1)
        self.assertEqual(calls, [1])

    def test_late_third_observation_cannot_pass_absolute_deadline(self):
        value = sample()
        clock = [0.0]
        calls = []
        def read(_deadline):
            calls.append(1)
            clock[0] = .01 if len(calls) < 3 else 2.0
            return value
        from unittest.mock import patch
        with patch.object(proof.time, 'monotonic', side_effect=lambda: clock[0]), patch.object(proof.time, 'sleep'):
            with self.assertRaisesRegex(TimeoutError, 'observation exceeded'):
                proof.await_endpoint(read, value['owner'],
                    lambda actual, original, previous: proof.endpoint(actual, original, mode='scroll'),
                    deadline=1.0)
        self.assertEqual(len(calls), 3)

    def test_late_validation_cannot_pass_absolute_deadline(self):
        value = sample()
        clock = [0.0]
        calls = []
        def predicate(actual, original, previous):
            calls.append(1)
            if len(calls) == 3:
                clock[0] = 2.0
            return proof.endpoint(actual, original, mode='scroll')
        from unittest.mock import patch
        with patch.object(proof.time, 'monotonic', side_effect=lambda: clock[0]), patch.object(proof.time, 'sleep'):
            with self.assertRaisesRegex(TimeoutError, 'validation exceeded'):
                proof.await_endpoint(lambda _: value, value['owner'], predicate, deadline=1.0)
        self.assertEqual(len(calls), 3)

    def test_actual_mode_chord_and_nonvacuous_vt_control_are_retained(self):
        import inspect
        source = inspect.getsource(proof.prove_installed_strip)
        self.assertEqual(source.count("chord('meta_l', 'shift', 't')"), 2)
        self.assertNotIn("chord('meta_l', 't')", source)
        self.assertIn("held = ready('scroll', extra=before_beta)", source)
        self.assertIn("bar[0] == found['beta']['rect'][0] - 4", source)
        self.assertIn("cancel control did not start after Beta", source)
        self.assertIn("with Image.open(png) as frame:", source)
        self.assertIn("colored_frame((png_image.width, png_image.height, png_image.tobytes())", source)
        self.assertIn("'same_acquisition_qualified': False", source)


class TransportAndCleanup(unittest.TestCase):
    def test_actual_qmp_event_and_partial_line_dribble_are_absolutely_bounded(self):
        loader = importlib.machinery.SourceFileLoader('installed_test_baseline',
            str(Path(__file__).resolve().parents[1] / 'tuna-baseline'))
        spec = importlib.util.spec_from_loader(loader.name, loader)
        module = importlib.util.module_from_spec(spec)
        loader.exec_module(module)
        for payload in (b'{"event":"TICK"}\n', b' '):
            with self.subTest(payload=payload):
                left, right = socket.socketpair()
                qmp = module.Qmp.__new__(module.Qmp)
                qmp.sock, qmp.file = left, left.makefile('rw')
                stop = threading.Event()
                def emit():
                    try:
                        right.recv(4096)
                        while not stop.wait(.005):
                            right.sendall(payload)
                    except OSError:
                        pass
                writer = threading.Thread(target=emit)
                writer.start()
                wrapped = proof.QmpDeadline(qmp, time.monotonic() + .12)
                began = time.monotonic()
                try:
                    with self.assertRaises(TimeoutError):
                        wrapped.command('screendump', filename='/unused')
                    self.assertLess(time.monotonic() - began, .6)
                    self.assertTrue(wrapped.failed)
                    with self.assertRaises(RuntimeError):
                        wrapped.command('query-status')
                finally:
                    stop.set()
                    right.shutdown(socket.SHUT_RDWR)
                    right.close()
                    writer.join(timeout=1)
                    qmp.file.close()
                    left.close()
                self.assertFalse(writer.is_alive())
                self.assertFalse(any(t.name == 'installed-strip-qmp-deadline' for t in threading.enumerate()))

    def test_lost_launch_ack_keeps_primary_error_and_independent_cleanup(self):
        root = Path(__file__).resolve().parents[2]
        image = {'schema': 1, 'commit': 'a' * 40, 'image_id': 'sha256:' + 'b' * 64,
                 'base_image': 'ghcr.io/tuna-os/marlin:gnome@sha256:' + 'c' * 64,
                 'package_sha256': 'd' * 64, 'member_sha256': {
                     '/usr/libexec/' + name: hashlib.sha256((root / 'packaging/marlin/vm-lane' / name).read_bytes()).hexdigest()
                     for name in ('tuna-vm-input', 'tuna-vm-lifecycle')}}
        original_error = RuntimeError('lost owned launch acknowledgment')
        calls = []
        class Original:
            def run(self, action, *args, timeout):
                calls.append(('original', action, timeout))
                raise original_error
        class Cleanup:
            def run(self, action, *args, timeout):
                calls.append(('cleanup', action, timeout))
                return {'cleanup_confirmed': False, 'launch_state': 'unknown'}
            def close(self):
                calls.append(('cleanup', 'close'))
        records = []
        with tempfile.TemporaryDirectory() as scratch:
            with self.assertRaises(RuntimeError) as caught:
                proof.prove_installed_strip(SimpleNamespace(cmd=lambda *_a, **_k: None),
                    Original(), Cleanup, scratch, image, lambda _: None,
                    lambda *values: records.append(values))
            self.assertIs(caught.exception, original_error)
            receipt = json.loads((Path(scratch) / 'strip-acceptance/receipt.json').read_text())
            self.assertFalse(receipt['completed'])
            self.assertFalse(receipt['cleanup_confirmed'])
            self.assertEqual(receipt['cleanup_error_type'], 'RuntimeError')
        self.assertEqual([item[:2] for item in calls],
                         [('original', 'strip-start'), ('cleanup', 'strip-stop'), ('cleanup', 'close')])
        self.assertFalse(records[-1][1])


if __name__ == '__main__':
    unittest.main()
