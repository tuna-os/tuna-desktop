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
            'notification_ui': {'layers': [{'namespace': 'tuna-panel', 'rect': [0, 0, 1280, 32]}], 'control': None},
            'inventory': {'processes': [{'executable': name, **value} for name, value in core.items()],
                          'state': {'locked': False, 'overview_open': False, 'session_mode': 'scroll',
                                    'focused': 1, 'active_workspace': 0, 'seat_pressed_key_count': 0,
                                    'rendered_frames': 20, 'strip_animating': False, 'windows': windows}}}


class Tests(unittest.TestCase):
    def test_initial_focus_point_avoids_observed_staggered_fixture_overlap(self):
        # Recorded rectangles from exact CI runs (9c, and run 38471211286
        # where Delta covered Alpha's lower-right): synthetic geometry
        # controls, not a replacement for actual focus/input/framebuffer
        # qualification.
        layouts = [
            {'alpha': [630, 446, 320, 240], 'beta': [580, 396, 320, 240],
             'gamma': [480, 296, 320, 240], 'delta': [530, 346, 320, 240]},
            {'alpha': [580, 396, 320, 240], 'beta': [530, 346, 320, 240],
             'gamma': [480, 296, 320, 240], 'delta': [630, 446, 320, 240]},
        ]
        def inside(point, rect):
            x, y, w, h = rect
            return x <= point[0] < x + w and y <= point[1] < y + h
        for rects in layouts:
            covers = [rect for role, rect in rects.items() if role != 'alpha']
            point = proof.exposed_point(rects['alpha'], covers)
            self.assertTrue(inside(point, rects['alpha']))
            self.assertTrue(0 <= point[0] < 1280 and 0 <= point[1] < 800)
            for role in ('beta', 'gamma', 'delta'):
                self.assertFalse(inside(point, rects[role]), role)
        # Both fixed corners are covered in one of the recorded layouts.
        old = (layouts[0]['alpha'][0] + 12, layouts[0]['alpha'][1] + 20)
        self.assertTrue(inside(old, layouts[0]['delta']))
        alpha = layouts[1]['alpha']
        old = (alpha[0] + alpha[2] - 12, alpha[1] + alpha[3] - 20)
        self.assertTrue(inside(old, layouts[1]['delta']))
        with self.assertRaisesRegex(RuntimeError, 'no exposed actual client point'):
            proof.exposed_point(alpha, [[0, 0, 1280, 800]])
        source = Path(proof.__file__).read_text()
        self.assertIn("move(*exposed_point(alpha,", source)
        self.assertIn("sample['inventory']['state']['focused'] == found['alpha']['id']", source)

    def test_shared_application_id_never_substitutes_for_unique_role_map(self):
        value = sample()
        for window in value['inventory']['state']['windows']:
            window['app_id'] = f"org.tuna.VmStrip.n{value['owner']['nonce']}"
        with self.assertRaisesRegex(proof.NotReady, 'missing/ambiguous actual native map'):
            proof.windows(value, value['owner'])
        # Duplicated role IDs also remain ambiguous, regardless of distinct IDs.
        value = sample()
        duplicate = copy.deepcopy(value['inventory']['state']['windows'][0])
        duplicate['id'] = 999
        value['inventory']['state']['windows'].append(duplicate)
        with self.assertRaisesRegex(proof.NotReady, 'missing/ambiguous actual native map'):
            proof.windows(value, value['owner'])

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

    def test_alpha_is_carried_left_of_beta_by_observed_column_moves(self):
        # Recorded run 38714427347 strip order: Beta, Gamma(, Delta) and the
        # raised Alpha last (stacking order), here with the baseline roles.
        value = sample()
        state = value['inventory']['state']
        ids = {w['app_id'].rsplit('.', 1)[1]: w['id'] for w in state['windows']}
        order = [ids[role] for role in proof.ROLES[1:]] + [ids['alpha']]
        def lay(columns):
            for slot, window_id in enumerate(columns):
                next(w for w in state['windows'] if w['id'] == window_id)['rect'][0] = -1000 + slot * 256
        lay(order)
        gap = proof.beta_gap(value, value['owner'])
        self.assertEqual(gap, -(len(proof.ROLES) - 1))
        while gap != 1:
            index = order.index(ids['alpha'])
            order[index - 1], order[index] = order[index], order[index - 1]
            lay(order)
            want = proof.gap_after_move(gap, -1)
            gap = proof.beta_gap(value, value['owner'])
            self.assertEqual(gap, want)
        self.assertEqual(order, [ids[role] for role in proof.ROLES])
        # Moving right past Beta swaps sides; zero is never a column gap.
        self.assertEqual(proof.gap_after_move(1, 1), -1)
        self.assertEqual(proof.gap_after_move(3, 1), 2)
        for bad in [(0, -1), (2, 0), (2, 2)]:
            with self.subTest(bad=bad), self.assertRaises(RuntimeError):
                proof.gap_after_move(*bad)
        source = Path(proof.__file__).read_text()
        self.assertIn("chord('meta_l', 'shift', 'left' if step < 0 else 'right')", source)
        self.assertIn("beta_gap(sample, original, edge=edge) == want", source)

    def test_grip_press_is_observed_before_any_drag_motion(self):
        # Run 38726161191: the tablet carried the press to the next queued
        # point (x+12), past Alpha's edge, so GTK never saw the grip press.
        source = Path(proof.__file__).read_text()
        press = source.index("move(x, y)\n        button(True)")
        pressed = source.index("pressed = ready('scroll'", press)
        motion = source.index("move(x + (12 if delta > 0 else -12), y)", press)
        self.assertLess(pressed, motion)

    def test_super_drag_holds_super_in_the_seat_before_the_press(self):
        # Keyboard and tablet are separate guest devices with no mutual
        # ordering: the press must follow an observed seat-held Super, and
        # the drop and Super release are each observed in turn.
        source = Path(proof.__file__).read_text()
        start = source.index("super_key(True)\n")
        held = source.index("held_keys(sample) == 1 and unclaimed(sample)", start)
        press = source.index("button(True)", start)
        preview = source.index("before_beta(sample, found) and unclaimed(sample)", start)
        drop = source.index("button(False)", start)
        release = source.index("super_key(False)", start)
        released = source.index("held_keys(sample) == 0 and unclaimed(sample)", start)
        capture = source.index("capture('08-super-drag-reorder', returned)", start)
        self.assertLess(held, press)
        self.assertLess(press, preview)
        self.assertLess(preview, drop)
        self.assertLess(drop, release)
        self.assertLess(release, released)
        self.assertLess(released, capture)
        self.assertLess(source.index("capture('07-vt-cancel', returned)"), start)
        self.assertIn("('move_requests', 'resize_requests', 'delivered_buttons')", source)
        self.assertIn('"seat_pressed_key_count"', (Path(proof.__file__).resolve().parents[2]
                      / 'packaging/marlin/vm-lane/tuna-vm-lifecycle').read_text())

    def test_strip_insert_mirrors_the_compositor_drop_rule(self):
        column = lambda x, w=616: [[[x, 48, w, 736]]]
        # Same cases as the compositor's tests/windows.rs drop tests.
        self.assertEqual(proof.strip_insert(column(648) + column(1280), 853, 58), ('tile', 0, 0))
        self.assertEqual(proof.strip_insert(column(648) + column(1280), 1076, 413), ('column', 1))
        self.assertEqual(proof.strip_insert(column(648) + column(1280), 926, 113), ('tile', 0, 0))
        shared = [[16, 48, 405, 360], [16, 424, 405, 360]]
        self.assertEqual(proof.strip_insert([shared, *column(437)], 100, 410), ('tile', 0, 1))
        self.assertEqual(proof.strip_insert([shared, *column(437)], 899, 400), ('column', 2))
        # The stationary edge drop past the fully revealed last column.
        self.assertEqual(proof.strip_insert(column(-616) + column(16) + column(648), 1279, 68), ('column', 3))
        # A lifted slot keeps its place: both of its sides name it.
        self.assertEqual(proof.strip_insert(column(16) + column(1280), 700, 400), ('column', 1))
        self.assertEqual(proof.strip_insert(column(16) + column(1280), 1200, 400), ('column', 1))
        self.assertEqual(proof.strip_insert([], 600, 400), ('column', 0))

    def test_actual_drag_targets_choose_the_intended_gaps(self):
        # Geometry of the installed proof after its grip step widened (+53)
        # or narrowed (-53) Alpha, for the title reorder, VT cancellation
        # and Super+drag targets the proof computes.
        for alpha_w in (669, 563):
            with self.subTest(alpha_w=alpha_w):
                beta_x = 16 + alpha_w + 16
                state = {'windows': [
                    {'id': 1, 'workspace': 0, 'rect': [16, 48, alpha_w, 736]},
                    {'id': 2, 'workspace': 0, 'rect': [beta_x, 48, 616, 736]},
                    {'id': 3, 'workspace': 0, 'rect': [beta_x + 632, 48, 616, 736]},
                    {'id': 9, 'workspace': 1, 'rect': [16, 48, 616, 736]}]}
                value = {'inventory': {'state': state}}
                found = {'alpha': state['windows'][0], 'beta': state['windows'][1]}
                columns = proof.remaining_columns(value, found, 'alpha')
                self.assertEqual([tiles[0][0] for tiles in columns], [beta_x, beta_x + 632])
                target = (min(1260, beta_x + 3 * 616 // 4), 48 + 736 // 2)
                after = proof.drop_slot(value, found, 'alpha', 'beta') + 1
                self.assertEqual(proof.strip_insert(columns, *target), ('column', after))
                # The superseded centre-based target, at title height, would
                # now join Beta's column instead.
                old = (min(1260, beta_x + 616 // 2 + 30), 48 + 20)
                self.assertEqual(proof.strip_insert(columns, *old)[0], 'tile')
                # Reordered: [Beta, Alpha, Gamma] with Alpha focused and whole.
                offset = max(0, 632 + alpha_w + 32 - 1280)
                beta_x = 16 - offset
                alpha_x = beta_x + 632
                state['windows'][0]['rect'] = [alpha_x, 48, alpha_w, 736]
                state['windows'][1]['rect'] = [beta_x, 48, 616, 736]
                state['windows'][2]['rect'] = [alpha_x + alpha_w + 16, 48, 616, 736]
                columns = proof.remaining_columns(value, found, 'alpha')
                target = (max(20, beta_x + 616 // 4), 48 + 736 // 2)
                before = proof.drop_slot(value, found, 'alpha', 'beta')
                self.assertEqual(before, 0)
                self.assertEqual(proof.strip_insert(columns, *target), ('column', before))

    def test_column_chords_and_in_column_height_follow_the_super_drag(self):
        source = Path(proof.__file__).read_text()
        start = source.index("capture('08-super-drag-reorder', returned)")
        consume = source.index("chord('meta_l', 'bracket_right')", start)
        consumed = source.index("capture('09-column-consume', consumed)", consume)
        grip = source.index("grip = ready('scroll'", consumed)
        motion = source.index("move(x, y - 12)", consumed)
        height = source.index("capture('10-column-height', heights, 'beta')", motion)
        expel = source.index("chord('meta_l', 'bracket_right')", height)
        expelled = source.index("capture('11-column-expel', returned, 'beta')", expel)
        edge = source.index('if edge:\n            before_edge = returned', expelled)
        self.assertLess(grip, motion)
        self.assertLess(expelled, edge)


class EndpointEvidence(unittest.TestCase):
    def test_storage_failure_preserves_exact_primary_endpoint_exception(self):
        from unittest.mock import patch
        primary = TimeoutError('actual guest transport')
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'endpoint.json'
            with patch.object(Path, 'write_text', side_effect=OSError('disk full')):
                try:
                    try:
                        raise primary
                    finally:
                        secondary = proof.retain_endpoint_evidence(path, {'qualified': False}, primary)
                except TimeoutError as actual:
                    self.assertIs(actual, primary)
                else:
                    self.fail('primary exception swallowed')
        self.assertEqual(secondary, 'OSError')
        self.assertEqual(primary.__notes__, ['endpoint diagnostic persistence failed: OSError'])

    def test_successful_endpoint_storage_failure_cannot_pass(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'endpoint.json'
            with patch.object(Path, 'write_text', side_effect=OSError('disk full')):
                with self.assertRaises(OSError):
                    proof.retain_endpoint_evidence(path, {'qualified': True})

    def test_retained_evidence_matches_actual_observations(self):
        value = {'qualified': False, 'observations': [{'status': 'not-ready', 'sample': {'actual': 1}}]}
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary) / 'endpoint.json'
            self.assertIsNone(proof.retain_endpoint_evidence(path, value, TimeoutError()))
            self.assertEqual(json.loads(path.read_text()), value)

    def test_partial_reason_survives_later_transport_timeout(self):
        from unittest.mock import patch
        evidence = {}
        values = iter([{'actual': 1}, TimeoutError('transport')])
        def read(_):
            value = next(values)
            if isinstance(value, Exception):
                raise value
            return value
        def partial(*_):
            raise proof.NotReady('actual GTK allocation/paint differs from compositor')
        with patch.object(proof.time, 'monotonic', return_value=0), patch.object(proof.time, 'sleep'):
            with self.assertRaises(TimeoutError):
                proof.await_endpoint(read, None, partial, deadline=1, evidence=evidence)
        self.assertEqual(evidence['read_attempts'], 2)
        self.assertFalse(evidence['qualified'])
        self.assertEqual(evidence['observations'], [{'sample': {'actual': 1}, 'status': 'not-ready',
            'reason': 'actual GTK allocation/paint differs from compositor'}])

    def test_three_stable_observations_are_required_for_evidence_qualification(self):
        from unittest.mock import patch
        evidence = {}
        with patch.object(proof.time, 'monotonic', return_value=0), patch.object(proof.time, 'sleep'):
            _, samples = proof.await_endpoint(lambda _: {'actual': 1}, None,
                lambda *_: {'rect': [1, 2, 3, 4]}, deadline=1, evidence=evidence)
        self.assertTrue(evidence['qualified'])
        self.assertEqual(len(samples), 3)
        self.assertEqual([entry['stable_samples'] for entry in evidence['observations']], [1, 2, 3])

    def test_late_read_is_retained_without_validation_or_qualification(self):
        from unittest.mock import patch
        clock = [0]
        evidence = {}
        def read(_):
            clock[0] = 2
            return {'actual': 1}
        with patch.object(proof.time, 'monotonic', side_effect=lambda: clock[0]):
            with self.assertRaisesRegex(TimeoutError, 'observation exceeded'):
                proof.await_endpoint(read, None, lambda *_: self.fail('late sample validated'),
                                     deadline=1, evidence=evidence)
        self.assertFalse(evidence['qualified'])
        self.assertEqual(evidence['observations'], [{'sample': {'actual': 1}, 'status': 'late-read-refused'}])

    def test_terminal_identity_failure_is_not_labeled_readiness(self):
        from unittest.mock import patch
        evidence = {}
        def invalid(*_):
            raise RuntimeError('original identity changed')
        with patch.object(proof.time, 'monotonic', return_value=0):
            with self.assertRaisesRegex(RuntimeError, 'original identity changed'):
                proof.await_endpoint(lambda _: {'actual': 1}, None, invalid, deadline=1, evidence=evidence)
        self.assertFalse(evidence['qualified'])
        self.assertEqual(evidence['read_attempts'], 1)
        self.assertEqual(evidence['observations'][0]['status'], 'unvalidated')

    def test_oversized_sample_is_not_retained(self):
        from unittest.mock import patch
        evidence = {}
        with patch.object(proof.time, 'monotonic', return_value=0):
            with self.assertRaisesRegex(RuntimeError, 'byte bound'):
                proof.await_endpoint(lambda _: {'actual': 'x' * 65536}, None, lambda *_: {},
                                     deadline=1, evidence=evidence)
        self.assertEqual(evidence['observations'], [])
        self.assertFalse(evidence['qualified'])


class NotificationPreparation(unittest.TestCase):
    def setUp(self):
        self.value = sample()
        self.original = copy.deepcopy(self.value['owner'])
        self.sha = 'd' * 64
        shell = self.original['core']['tuna-shell-gtk']
        credentials = {'GetNameOwner': ':1.42', 'GetConnectionUnixProcessID': shell['pid'],
                       'GetConnectionUnixUser': shell['uid']}
        self.value['notification_ui']['layers'].append(
            {'namespace': 'tuna-shell-banners', 'rect': [384, 32, 512, 108]})
        self.value['notification_ui']['control'] = {
            'principal': {**shell, 'executable': 'tuna-shell-gtk'}, 'atspi_owner': ':1.42',
            'credentials_before': credentials, 'credentials_after': copy.deepcopy(credentials),
            'observation_only': True, 'observer_sha256': self.sha,
            'notification_banner': {'frames': [[0, 0, 512, 108]],
                                    'close_controls': [{'rect': [472, 12, 32, 32], 'focused': False}]}}
        self.focused = copy.deepcopy(self.value)
        self.focused['notification_ui']['control']['notification_banner']['close_controls'][0]['focused'] = True
        self.after = copy.deepcopy(self.value)
        self.after['notification_ui']['layers'].pop()
        self.after['notification_ui']['control'] = None

    def dismiss(self, *, before=None, observed=None, after=None, idle=True, clock=None, late_absence=False):
        from unittest.mock import patch
        self.keys, self.evidence = [], {}
        clock = clock or [0.0]
        observations = [0]
        def read(deadline):
            self.assertEqual(deadline, 1)
            if not self.keys or self.keys[-1] != ('ret',):
                return self.value
            if late_absence:
                clock[0] = 2
            return after if after is not None else self.after
        def observe(_deadline):
            observations[0] += 1
            return observed if observed is not None else self.value if observations[0] == 1 else self.focused
        with patch.object(proof.time, 'monotonic', side_effect=lambda: clock[0]), \
                patch.object(proof.time, 'sleep', side_effect=lambda duration: clock.__setitem__(0, clock[0] + duration)):
            return proof.dismiss_notification_banner(
                read, observe, lambda *keys: self.keys.append(keys), before if before is not None else self.value,
                self.original, self.sha, deadline=1, input_idle=idle, evidence=self.evidence)

    def test_owned_close_requires_actual_focused_control_before_return_and_real_absence(self):
        self.assertIs(self.dismiss(), self.after)
        self.assertEqual(self.keys, [('meta_l', 'n'), ('tab',), ('ret',)])
        self.assertTrue(self.evidence['activated'])
        self.assertTrue(self.evidence['confirmed_absent'])
        self.assertLess(len(json.dumps(self.evidence)), 2048)

    def test_no_banner_means_no_extra_observer_read_or_input(self):
        self.assertIs(self.dismiss(before=self.after), self.after)
        self.assertEqual(self.keys, [])
        self.assertEqual(self.evidence, {})

    def test_namespace_never_substitutes_for_original_process_bus_or_source(self):
        for mutation in ('principal', 'owner', 'before', 'after', 'source', 'observation'):
            value = copy.deepcopy(self.value)
            control = value['notification_ui']['control']
            if mutation == 'principal':
                control['principal']['pid'] += 1
            elif mutation == 'owner':
                control['atspi_owner'] = 'org.private.untrusted'
            elif mutation in ('before', 'after'):
                control['credentials_' + mutation]['GetConnectionUnixProcessID'] += 1
            elif mutation == 'source':
                control['observer_sha256'] = 'f' * 64
            else:
                control['observation_only'] = False
            with self.subTest(mutation=mutation), self.assertRaises(RuntimeError):
                self.dismiss(observed=value)
            self.assertEqual(self.keys, [])

    def test_authentication_unknown_overlay_and_ambiguous_controls_never_receive_input(self):
        for mutation in ('lock', 'overview', 'polkit', 'other', 'duplicate-layer', 'duplicate-frame',
                         'duplicate-close', 'close-outside', 'frame-size', 'panel-overlap', 'keys', 'gesture'):
            value = copy.deepcopy(self.value)
            ui, state = value['notification_ui'], value['inventory']['state']
            if mutation == 'lock':
                state['locked'] = True
            elif mutation == 'overview':
                state['overview_open'] = True
            elif mutation in ('polkit', 'other'):
                ui['layers'].append({'namespace': 'tuna-polkit' if mutation == 'polkit' else 'other',
                                     'rect': [900, 400, 100, 100]})
            elif mutation == 'duplicate-layer':
                ui['layers'].append(copy.deepcopy(ui['layers'][-1]))
            elif mutation == 'duplicate-frame':
                ui['control']['notification_banner']['frames'] *= 2
            elif mutation == 'duplicate-close':
                ui['control']['notification_banner']['close_controls'] *= 2
            elif mutation == 'close-outside':
                ui['control']['notification_banner']['close_controls'][0]['rect'][0] = 510
            elif mutation == 'frame-size':
                ui['control']['notification_banner']['frames'][0][2] = 511
            elif mutation == 'panel-overlap':
                ui['layers'][0]['rect'] = [0, 0, 1280, 100]
            elif mutation == 'keys':
                state['seat_pressed_key_count'] = 1
            else:
                state['tile_preview'] = {'window': 1}
            with self.subTest(mutation=mutation), self.assertRaises(RuntimeError):
                self.dismiss(before=value, observed=value)
            self.assertEqual(self.keys, [])
        with self.assertRaises(RuntimeError):
            self.dismiss(idle=False)
        self.assertEqual(self.keys, [])

    def test_stale_control_and_identity_geometry_mode_or_focus_changes_fail_closed(self):
        for mutation in ('identity', 'geometry', 'mode', 'focus', 'new-overlay'):
            value = copy.deepcopy(self.after)
            if mutation == 'identity':
                value['fixture']['identity']['start_ticks'] += 1
            elif mutation == 'geometry':
                value['inventory']['state']['windows'][0]['rect'][0] += 1
            elif mutation == 'mode':
                value['inventory']['state']['session_mode'] = 'gnome'
            elif mutation == 'focus':
                value['inventory']['state']['focused'] = 2
            else:
                value['notification_ui']['layers'].append({'namespace': 'other', 'rect': [0, 0, 40, 40]})
            with self.subTest(mutation=mutation), self.assertRaises(RuntimeError):
                self.dismiss(after=value)
            self.assertEqual(self.keys, [('meta_l', 'n'), ('tab',), ('ret',)])
            self.assertFalse(self.evidence['confirmed_absent'])
        value = copy.deepcopy(self.value)
        value['notification_ui']['layers'][-1]['rect'][0] += 1
        with self.assertRaises(RuntimeError):
            self.dismiss(observed=value)
        self.assertEqual(self.keys, [])

    def test_persistent_banner_cannot_be_claimed_dismissed_or_repeatedly_activated(self):
        with self.assertRaises(TimeoutError):
            self.dismiss(after=self.value)
        self.assertEqual(self.keys, [('meta_l', 'n'), ('tab',), ('ret',)])
        self.assertFalse(self.evidence['confirmed_absent'])

    def test_wrong_button_or_unfocused_close_never_receives_return(self):
        with self.assertRaisesRegex(RuntimeError, 'Close focused'):
            self.dismiss(observed=self.value)
        self.assertEqual(self.keys, [('meta_l', 'n'), ('tab',)])
        self.assertFalse(self.evidence['activated'])

    def test_late_observation_or_absence_does_not_extend_existing_deadline(self):
        from unittest.mock import patch
        clock, keys = [0.0], []
        def late(_):
            clock[0] = 2
            return self.value
        with patch.object(proof.time, 'monotonic', side_effect=lambda: clock[0]):
            with self.assertRaises(TimeoutError):
                proof.dismiss_notification_banner(lambda _: self.after, late, lambda *k: keys.append(k),
                    self.value, self.original, self.sha, deadline=1, input_idle=True, evidence={})
        self.assertEqual(keys, [])
        with self.assertRaises(TimeoutError):
            self.dismiss(late_absence=True)
        self.assertEqual(self.keys, [('meta_l', 'n'), ('tab',), ('ret',)])

    def test_focus_chord_release_must_be_observed_before_tab(self):
        from unittest.mock import patch
        clock, keys = [0.0], []
        held = copy.deepcopy(self.value)
        held['inventory']['state']['seat_pressed_key_count'] = 1
        with patch.object(proof.time, 'monotonic', side_effect=lambda: clock[0]), \
                patch.object(proof.time, 'sleep', side_effect=lambda duration: clock.__setitem__(0, clock[0] + duration)):
            with self.assertRaises(TimeoutError):
                proof.dismiss_notification_banner(lambda _: held, lambda _: self.value, lambda *k: keys.append(k),
                    self.value, self.original, self.sha, deadline=1, input_idle=True, evidence={})
        self.assertEqual(keys, [('meta_l', 'n')])

    def test_real_checker_still_refuses_occluded_or_wrong_body_pixels(self):
        color = proof.COLORS['beta']
        pixels = bytearray(bytes(color) * 1280 * 800)
        for y in range(36, 140):
            for x in range(384, 896):
                offset = (y * 1280 + x) * 3
                pixels[offset:offset + 3] = bytes((84, 84, 90))
        rect = [16, 48, 616, 300]
        with self.assertRaises(proof.NotReady):
            proof.colored_frame((1280, 800, pixels), rect, color)
        self.assertTrue(proof.colored_frame((1280, 800, bytes(color) * 1280 * 800), rect, color))
        with self.assertRaises(proof.NotReady):
            proof.colored_frame((1280, 800, bytes(proof.COLORS['alpha']) * 1280 * 800), rect, color)

    def test_lifecycle_projection_sanitizes_unknown_namespace_and_never_clears_backend(self):
        import ast
        root = Path(proof.__file__).resolve().parents[2]
        path = root / 'packaging/marlin/vm-lane/tuna-vm-lifecycle'
        tree = ast.parse(path.read_text())
        function = next(node for node in tree.body if isinstance(node, ast.FunctionDef)
                        and node.name == 'strip_notification_ui')
        scope = {}
        exec(compile(ast.Module(body=[function], type_ignores=[]), str(path), 'exec'), scope)
        summary = {'layers': [{'namespace': 'private namespace', 'rect': [0, 0, 100, 100]}]}
        self.assertEqual(scope['strip_notification_ui'](summary, self.original),
                         {'layers': [{'namespace': 'other', 'rect': [0, 0, 100, 100]}], 'control': None})
        with self.assertRaises(RuntimeError):
            scope['strip_notification_ui']({'layers': []} | {'locked': True}, self.original, observe_control=True)
        source = ast.get_source_segment(path.read_text(), function)
        self.assertNotIn('CloseNotification', source)
        self.assertNotIn('show-banners', source)

    def test_notification_observer_never_reads_body_text_and_retains_owned_bounds_only(self):
        import ast
        root = Path(proof.__file__).resolve().parents[2]
        path = root / 'packaging/marlin/vm-lane/tuna-vm-shell-focus'
        loader = importlib.machinery.SourceFileLoader('strip_notification_focus', str(path))
        spec = importlib.util.spec_from_loader(loader.name, loader)
        module = importlib.util.module_from_spec(spec)
        loader.exec_module(module)
        tree = ast.parse(path.read_text())
        observer = next(node for node in tree.body if isinstance(node, ast.FunctionDef) and node.name == 'observe')
        branch = next(node for node in ast.walk(observer) if isinstance(node, ast.If)
                      and isinstance(node.test, ast.Name) and node.test.id == 'notification'
                      and any(isinstance(child, ast.If) for child in node.body))
        loop = ast.For(target=ast.Name(id='_', ctx=ast.Store()), iter=ast.Tuple(elts=[ast.Constant(0)], ctx=ast.Load()),
                       body=[branch], orelse=[])
        compiled = compile(ast.fix_missing_locations(ast.Module(body=[loop], type_ignores=[])), str(path), 'exec')
        class Node:
            def get_name(self):
                if role not in ('frame', 'push button'):
                    raise AssertionError('private notification text must not be queried')
                return name
            def get_component_iface(self):
                return self
        component = SimpleNamespace(get_extents=lambda _node, coordinates: SimpleNamespace(x=0, y=0, width=512, height=108))
        for role, name, inside in [('label', 'private body', True), ('text', 'private body', True),
                                   ('frame', 'Notifications', False), ('frame', 'Private dialog', False),
                                   ('push button', 'Close', True), ('push button', 'Other action', True)]:
            scope = {'notification': True, 'role': role, 'in_notification': inside, 'node': Node(),
                     'accessibility_query': lambda method, *args: method(*args),
                     'Atspi': SimpleNamespace(Component=component, CoordType=SimpleNamespace(WINDOW='window'),
                                              StateType=SimpleNamespace(SENSITIVE='sensitive', FOCUSED='focused')),
                     'states': SimpleNamespace(contains=lambda _: True),
                     'notification_frames': [], 'notification_closes': []}
            exec(compiled, scope)
            self.assertEqual(len(scope['notification_frames']), int(role == 'frame' and name == 'Notifications'))
            self.assertEqual(len(scope['notification_closes']), int(role == 'push button' and name == 'Close' and inside))
        self.assertEqual(module.canonical_role('button'), 'push button')


class CaptureFailureEvidence(unittest.TestCase):
    """Synthetic capture failures exercise the real receipt and cleanup path."""

    def run_failure(self, *, start=0.0, acquire_error=None, mismatch=None,
                    post_error=None, late_post=False, late_pixels=False,
                    unknown_reason=False, retry_once=False, cleanup_confirmed=True,
                    banner=False, notification_key_expiry=False):
        from unittest.mock import patch
        from PIL import Image
        value = sample()
        value['inventory']['state']['session_mode'] = 'gnome'
        for window in value['inventory']['state']['windows']:
            window['workspace'] = 0
        root = Path(proof.__file__).resolve().parents[2]
        members = {
            '/usr/libexec/' + name: hashlib.sha256(
                (root / 'packaging/marlin/vm-lane' / name).read_bytes()).hexdigest()
            for name in ('tuna-vm-input', 'tuna-vm-lifecycle')}
        for name, identity in value['owner']['core'].items():
            identity['exe_sha256'] = members['/usr/bin/' + name] = 'e' * 64
        image = {'schema': 1, 'commit': 'a' * 40, 'image_id': 'sha256:' + 'b' * 64,
                 'base_image': 'ghcr.io/tuna-os/marlin:gnome@sha256:' + 'c' * 64,
                 'package_sha256': 'd' * 64, 'member_sha256': members}
        if banner:
            notification = NotificationPreparation()
            notification.setUp()
            value['notification_ui'] = copy.deepcopy(notification.value['notification_ui'])
            value['notification_ui']['control']['observer_sha256'] = hashlib.sha256(
                (root / 'packaging/marlin/vm-lane/tuna-vm-shell-focus').read_bytes()).hexdigest()
        clock = [0.0]
        calls, acquisitions, deadlines = [], [], []
        inputs = []
        notification_keys = []
        primary = RuntimeError('private raw exception body must not be retained')
        color = proof.COLORS['alpha']
        good = (1280, 800, bytes(color) * 1280 * 800)
        wrong = (1280, 800, bytes((0, 0, 0)) * 1280 * 800)

        class Agent:
            poisoned = False

            def run(self, action, *args, timeout):
                calls.append((action, timeout))
                if action == 'strip-start':
                    return {'owner': value['owner']}
                if action == 'strip-stop':
                    return {'cleanup_confirmed': cleanup_confirmed}
                if action == 'strip-notification-control':
                    observed = copy.deepcopy(value)
                    observed['notification_ui']['control']['notification_banner']['close_controls'][0]['focused'] = ['tab'] in notification_keys
                    return observed
                if action == 'strip-state':
                    if post_error == 'transport' and (not retry_once or reads[0] > 1):
                        raise primary
                    after = copy.deepcopy(value)
                    if banner and ['ret'] in notification_keys:
                        after['notification_ui']['layers'].pop()
                        after['notification_ui']['control'] = None
                    if post_error == 'identity':
                        after['fixture']['identity']['start_ticks'] += 1
                    if post_error == 'geometry':
                        after['inventory']['state']['windows'][0]['rect'][0] += 1
                    if late_post:
                        clock[0] = start + 10
                    return after
                raise AssertionError(action)

        class Transport:
            def __init__(self, _qmp, deadline):
                deadlines.append(deadline)
                self.deadline = deadline

            def command(self, command, **kwargs):
                if command in ('input-send-event', 'send-key'):
                    inputs.append((self.deadline, kwargs.get('events', kwargs.get('keys'))))
                    if command == 'send-key' and self.deadline != 120:
                        notification_keys.append([key['data'] for key in kwargs['keys']])
                    if notification_key_expiry and self.deadline != 120:
                        clock[0] = min(120, start + 10)
                        raise primary
                if command != 'screendump':
                    return None
                fmt = kwargs['format']
                acquisitions.append(fmt)
                if acquire_error == fmt:
                    raise primary
                path = Path(kwargs['filename'])
                if fmt == 'png':
                    Image.new('RGB', (1280, 800),
                              (0, 0, 0) if mismatch == 'png' else color).save(path)
                else:
                    path.write_bytes(b'synthetic PPM acquisition')

        def ready(*_args, **_kwargs):
            clock[0] = start
            return value, [value] * 3

        def sleep(duration):
            self.assertLessEqual(duration, .25)
            # Let the existing retry sleep reach the actual effective deadline.
            clock[0] = min(120.0, start + 10.0)

        def pixels(*args):
            if unknown_reason:
                raise proof.NotReady('private readiness body ' + 'x' * 10000)
            if late_pixels:
                clock[0] = start + 10
            return proof_colored_frame(*args)

        proof_colored_frame = proof.colored_frame
        reads = [0]

        def read_ppm(_path):
            reads[0] += 1
            return wrong if mismatch == 'ppm' and (not retry_once or reads[0] == 1) else good

        def retry_sleep(duration):
            if retry_once and reads[0] == 1:
                clock[0] += duration
            else:
                sleep(duration)

        records = []
        with tempfile.TemporaryDirectory() as scratch, \
                patch.object(proof, 'QmpDeadline', Transport), \
                patch.object(proof, 'await_endpoint', side_effect=ready), \
                patch.object(proof.time, 'monotonic', side_effect=lambda: clock[0]), \
                patch.object(proof.time, 'monotonic_ns', side_effect=lambda: int(clock[0] * 1e9)), \
                patch.object(proof.time, 'sleep', side_effect=retry_sleep), \
                patch.object(proof, 'colored_frame', side_effect=pixels):
            def reconnect():
                self.fail('healthy primary stream must perform cleanup')
            with self.assertRaises(BaseException) as caught:
                proof.prove_installed_strip(
                    object(), Agent(), reconnect, scratch, image, read_ppm,
                    lambda *entry: records.append(entry))
            receipt = json.loads((Path(scratch) / 'strip-acceptance/receipt.json').read_text())
        self.last_receipt, self.last_inputs = receipt, inputs
        self.assertFalse(receipt['completed'])
        self.assertEqual(receipt['cases'], [])
        self.assertEqual(receipt['cleanup_confirmed'], cleanup_confirmed)
        self.assertEqual(calls[-1], ('strip-stop', 10))
        self.assertFalse(records[-1][1])
        self.assertEqual(deadlines, [120.0, min(120.0, start + 10.0)])
        diagnostic = receipt['capture_failure']
        self.assertEqual(set(diagnostic), {
            'label', 'role', 'attempts', 'capture_begin_monotonic_ns',
            'failure_monotonic_ns', 'local_deadline_monotonic_ns',
            'capture_deadline_monotonic_ns', 'total_deadline_monotonic_ns',
            'stage', 'last_not_ready_reason', 'ppm_rgb_verified', 'png_rgb_verified'})
        self.assertEqual((diagnostic['label'], diagnostic['role']), ('01-floating', 'alpha'))
        self.assertEqual(diagnostic['capture_begin_monotonic_ns'], int(start * 1e9))
        self.assertEqual(diagnostic['local_deadline_monotonic_ns'], int((start + 10) * 1e9))
        self.assertEqual(diagnostic['capture_deadline_monotonic_ns'], int(min(120, start + 10) * 1e9))
        self.assertEqual(diagnostic['total_deadline_monotonic_ns'], 120_000_000_000)
        self.assertLess(len(json.dumps(diagnostic).encode()), 1024)
        self.assertNotIn('private', json.dumps(receipt))
        return diagnostic, caught.exception, primary, acquisitions, calls

    def test_notification_keys_join_all_commands_to_existing_capture_deadline(self):
        diagnostic, error, primary, _, calls = self.run_failure(banner=True, acquire_error='png')
        self.assertIs(error, primary)
        self.assertEqual(diagnostic['stage'], 'png-acquire')
        self.assertEqual([deadline for deadline, _ in self.last_inputs], [120, 120, 120, 10, 10, 10])
        self.assertEqual([action for action, _ in calls],
                         ['strip-start', 'strip-notification-control', 'strip-state', 'strip-state',
                          'strip-notification-control', 'strip-state', 'strip-stop'])
        self.assertTrue(self.last_receipt['notification_dismissals'][0]['confirmed_absent'])

    def test_expired_notification_focus_chord_never_sends_tab_or_return(self):
        diagnostic, error, primary, acquisitions, _ = self.run_failure(
            banner=True, notification_key_expiry=True)
        self.assertIs(error, primary)
        self.assertEqual(diagnostic['stage'], 'post-frame-read')
        self.assertEqual(diagnostic['attempts'], 0)
        self.assertEqual(acquisitions, [])
        self.assertEqual([deadline for deadline, _ in self.last_inputs], [120, 120, 120, 10])
        self.assertFalse(self.last_receipt['notification_dismissals'][0]['activated'])
        self.assertFalse(self.last_receipt['notification_dismissals'][0]['confirmed_absent'])

    def test_local_and_global_deadlines_are_distinguishable_after_pixel_mismatch(self):
        for start, failure_ns in ((0, 10_000_000_000), (112, 120_000_000_000)):
            with self.subTest(start=start):
                diagnostic, error, _, acquisitions, calls = self.run_failure(start=start, mismatch='ppm')
                self.assertIsInstance(error, TimeoutError)
                self.assertEqual(str(error), 'installed sample absolute deadline')
                self.assertEqual(diagnostic['failure_monotonic_ns'], failure_ns)
                self.assertEqual(diagnostic['stage'], 'retry-refresh')
                self.assertEqual(diagnostic['last_not_ready_reason'], 'fixture-color-mismatch')
                self.assertIs(diagnostic['ppm_rgb_verified'], False)
                self.assertIsNone(diagnostic['png_rgb_verified'])
                self.assertEqual(diagnostic['attempts'], 1)
                self.assertEqual(acquisitions, ['png', 'ppm'])
                self.assertNotIn('strip-state', [action for action, _ in calls])

    def test_png_mismatch_is_distinct_from_valid_ppm(self):
        diagnostic, error, _, _, _ = self.run_failure(mismatch='png')
        self.assertIsInstance(error, TimeoutError)
        self.assertIs(diagnostic['ppm_rgb_verified'], True)
        self.assertIs(diagnostic['png_rgb_verified'], False)
        self.assertEqual(diagnostic['last_not_ready_reason'], 'fixture-color-mismatch')

    def test_each_acquisition_failure_preserves_primary_and_cleanup(self):
        for fmt in ('png', 'ppm'):
            with self.subTest(format=fmt):
                diagnostic, error, primary, acquisitions, _ = self.run_failure(acquire_error=fmt)
                self.assertIs(error, primary)
                self.assertEqual(diagnostic['stage'], fmt + '-acquire')
                self.assertEqual(diagnostic['attempts'], 1)
                self.assertEqual(acquisitions, ['png'] if fmt == 'png' else ['png', 'ppm'])
                self.assertIsNone(diagnostic['last_not_ready_reason'])
                self.assertIsNone(diagnostic['ppm_rgb_verified'])
                self.assertIsNone(diagnostic['png_rgb_verified'])

    def test_post_frame_identity_geometry_and_transport_failure_remain_terminal(self):
        for failure in ('identity', 'geometry', 'transport'):
            with self.subTest(failure=failure):
                diagnostic, error, primary, _, calls = self.run_failure(post_error=failure)
                self.assertIsInstance(error, RuntimeError)
                if failure == 'transport':
                    self.assertIs(error, primary)
                self.assertEqual(diagnostic['stage'], 'post-frame-read')
                self.assertIs(diagnostic['ppm_rgb_verified'], True)
                self.assertIs(diagnostic['png_rgb_verified'], True)
                self.assertEqual([action for action, _ in calls], ['strip-start', 'strip-state', 'strip-stop'])

    def test_valid_pixels_cannot_bypass_local_deadline_or_late_post_read(self):
        for late_post in (False, True):
            with self.subTest(late_post=late_post):
                diagnostic, error, _, _, _ = self.run_failure(
                    late_post=late_post, late_pixels=not late_post)
                self.assertIsInstance(error, TimeoutError)
                self.assertEqual(diagnostic['stage'], 'post-frame-read' if late_post else 'png-pixels')
                self.assertEqual(diagnostic['failure_monotonic_ns'], 10_000_000_000)
                self.assertIs(diagnostic['ppm_rgb_verified'], True)
                self.assertIs(diagnostic['png_rgb_verified'], True)

    def test_retry_attempt_count_and_last_fixed_reason_survive_later_failure(self):
        diagnostic, error, primary, acquisitions, _ = self.run_failure(
            mismatch='ppm', retry_once=True, post_error='transport')
        self.assertIs(error, primary)
        self.assertEqual(diagnostic['attempts'], 2)
        self.assertEqual(diagnostic['stage'], 'post-frame-read')
        self.assertEqual(diagnostic['last_not_ready_reason'], 'fixture-color-mismatch')
        self.assertIs(diagnostic['ppm_rgb_verified'], True)
        self.assertIs(diagnostic['png_rgb_verified'], True)
        self.assertEqual(acquisitions, ['png', 'ppm', 'png', 'ppm'])

    def test_unknown_readiness_text_is_fixed_and_cleanup_failure_does_not_replace_primary(self):
        diagnostic, error, _, _, _ = self.run_failure(unknown_reason=True)
        self.assertIsInstance(error, TimeoutError)
        self.assertEqual(diagnostic['last_not_ready_reason'], 'other-not-ready')
        _, error, primary, _, _ = self.run_failure(acquire_error='png', cleanup_confirmed=False)
        self.assertIs(error, primary)


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
            self.assertEqual(receipt['cleanup_error'], 'owned fixture cleanup unconfirmed')
        self.assertEqual([item[:2] for item in calls],
                         [('original', 'strip-start'), ('cleanup', 'strip-stop'), ('cleanup', 'close')])
        self.assertFalse(records[-1][1])

    def test_healthy_primary_stream_cleans_up_without_a_second_stream(self):
        # QEMU's guest agent socket serves one client at a time; a second
        # stream beside a healthy primary never synchronizes (marlin-vm
        # 38757056152). An exit-code failure leaves the primary unpoisoned.
        root = Path(__file__).resolve().parents[2]
        image = {'schema': 1, 'commit': 'a' * 40, 'image_id': 'sha256:' + 'b' * 64,
                 'base_image': 'ghcr.io/tuna-os/marlin:gnome@sha256:' + 'c' * 64,
                 'package_sha256': 'd' * 64, 'member_sha256': {
                     '/usr/libexec/' + name: hashlib.sha256((root / 'packaging/marlin/vm-lane' / name).read_bytes()).hexdigest()
                     for name in ('tuna-vm-input', 'tuna-vm-lifecycle')}}
        original_error = RuntimeError('guest lifecycle strip-start failed (exit=1)')
        calls = []
        class Original:
            poisoned = False
            def run(self, action, *args, timeout):
                calls.append(action)
                if action == 'strip-start':
                    raise original_error
                return {'cleanup_confirmed': True}
            def close(self):
                calls.append('close')
        def reconnect():
            raise AssertionError('a second guest-agent stream beside a healthy primary')
        with tempfile.TemporaryDirectory() as scratch:
            with self.assertRaises(RuntimeError) as caught:
                proof.prove_installed_strip(SimpleNamespace(cmd=lambda *_a, **_k: None),
                    Original(), reconnect, scratch, image, lambda _: None, lambda *_values: None)
            self.assertIs(caught.exception, original_error)
            receipt = json.loads((Path(scratch) / 'strip-acceptance/receipt.json').read_text())
            self.assertTrue(receipt['cleanup_confirmed'])
            self.assertNotIn('cleanup_error_type', receipt)
        self.assertEqual(calls, ['strip-start', 'strip-stop'])


if __name__ == '__main__':
    unittest.main()
