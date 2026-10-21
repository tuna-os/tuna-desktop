#!/usr/bin/env python3
"""Captured restore overlap and strict opacity controls, without GUI input."""
import copy
from unittest.mock import Mock
import sys
import unittest
sys.dont_write_bytecode = True
from minimize_body_pixel import (unobscured_body_point, require_rgb, require_restore_bracket,
                                 switcher_absent, wait_clear_restore, clear_capture, require_destroy_bracket,
                                 wait_clock_frames, minimize_quarter, minimize_capture)


class BodyPixel(unittest.TestCase):
    def test_actual_ltr_failed_probe_is_replaced_by_clear_body_probe(self):
        body = [321.0570888343116, 198.3968164335105, 411.6116523516815, 288.1281566461771]
        covered = {'layers': [{'namespace': 'tuna-shell-switcher', 'rect': [548, 308, 183, 183]}]}
        clear = {'layers': []}
        self.assertEqual(unobscured_body_point(body, (1280, 800), [covered, clear]), (445, 429))
        self.assertEqual(unobscured_body_point(body, (1280, 800), [clear, covered]), (445, 429))
        # Exact 3aa captured RGB at original(609,429) and clear(445,429).
        with self.assertRaises(AssertionError):
            require_rgb((71, 129, 130), (52, 101, 164), 'covered')
        require_rgb((52, 101, 164), (52, 101, 164), 'clear')

    def test_rtl_original_probe_stays_clear(self):
        weight = 2 ** -2.5
        body = [390 + (1280 - 390) * weight, 241 * (1 - weight),
                500 * (1 - weight), 350 * (1 - weight)]
        scene = {'layers': [{'namespace': 'tuna-shell-switcher', 'rect': [548, 308, 183, 183]}]}
        self.assertEqual(unobscured_body_point(body, (1280, 800), [scene, scene]), (835, 429))

    def test_original_ltr_probe_is_preferred_when_clear(self):
        body = [321.0570888343116, 198.3968164335105, 411.6116523516815, 288.1281566461771]
        self.assertEqual(unobscured_body_point(body, (1280, 800), [{'layers': []}] * 2), (609, 429))

    def test_entirely_occluded_probe_fails_ltr_and_rtl(self):
        scene = {'layers': [{'namespace': 'tuna-shell-switcher', 'rect': [0, 0, 1280, 800]}]}
        for body in ([321.057, 198.397, 411.612, 288.128], [547.331, 198.397, 411.612, 288.128]):
            with self.subTest(body=body), self.assertRaisesRegex(AssertionError, 'no unobscured'):
                unobscured_body_point(body, (1280, 800), [scene, scene])

    def test_missing_overlay_body_and_scene_geometry_fail(self):
        for scene in [{}, {'layers': [{'namespace': 'tuna-shell-switcher'}]},
                      {'layers': [{'namespace': 'tuna-shell-switcher', 'rect': [0, 0, 0, 1]}]}]:
            with self.assertRaises(AssertionError):
                unobscured_body_point([390, 241, 500, 350], (1280, 800), [scene, {'layers': []}])
        for body in [None, [0, 0, 0, 1], [0, float('nan'), 500, 350], [1500, 900, 500, 350]]:
            with self.assertRaises(AssertionError):
                unobscured_body_point(body, (1280, 800), [{'layers': []}] * 2)

    def test_capture_brackets_bind_icon_clock_timing_and_window_geometry(self):
        reference = {'animation_clock_ms': 1651,
                     'window_lifecycle': {'restoring': [{'window': 1, 'elapsed_ms': 100,
                                                        'duration_ms': 400, 'icon': [0, 0, 0, 0]}]},
                     'windows': [{'id': 1, 'rect': [390, 241, 500, 350]}]}
        require_restore_bracket(reference, copy.deepcopy(reference), 1, [390, 241, 500, 350])
        for field, value in [('icon', [1280, 0, 0, 0]), ('elapsed_ms', 101), ('duration_ms', 401)]:
            scene = copy.deepcopy(reference)
            scene['window_lifecycle']['restoring'][0][field] = value
            with self.subTest(field=field), self.assertRaises(AssertionError):
                require_restore_bracket(reference, scene, 1, [390, 241, 500, 350])
        scene = copy.deepcopy(reference)
        scene['animation_clock_ms'] += 1
        with self.assertRaises(AssertionError):
            require_restore_bracket(reference, scene, 1, [390, 241, 500, 350])
        scene = copy.deepcopy(reference)
        scene['windows'][0]['rect'][0] += 1
        with self.assertRaises(AssertionError):
            require_restore_bracket(reference, scene, 1, [390, 241, 500, 350])

    def test_half_opacity_still_fails_and_tolerance_is_four(self):
        expected = (52, 101, 164)
        with self.assertRaises(AssertionError):
            require_rgb((29, 80, 143), expected, 'half opaque over blue wallpaper')
        require_rgb((56, 97, 168), expected, 'four units allowed')
        with self.assertRaises(AssertionError):
            require_rgb((57, 101, 164), expected, 'five units refused')


class ClearRestoreCapture(unittest.TestCase):
    def reference(self):
        return {'animation_clock_ms':7775, 'layers':[],
                'windows':[{'id':7,'rect':[390,241,500,350]}],
                'window_lifecycle':{'restoring':[{'window':7,'elapsed_ms':100,
                    'duration_ms':400,'icon':[1280,0,0,0]}]}}

    def fading(self):
        scene=self.reference()
        scene['layers']=[{'namespace':'tuna-shell-switcher','rect':[559,324,161,164]}]
        return scene

    def test_fading_switcher_then_absent_keeps_exact_clock_and_actor(self):
        reference=self.reference();scenes=[self.fading(),reference];seen=[]
        def wait(predicate,label):
            for scene in scenes:
                seen.append(predicate(scene))
                if seen[-1]:return scene
            raise AssertionError('timed out')
        self.assertIs(wait_clear_restore(wait,reference,7,[390,241,500,350]),reference)
        self.assertEqual(seen,[False,True])
        self.assertEqual(reference['animation_clock_ms'],7775)

    def test_persistent_overlay_cannot_satisfy_existing_bounded_wait(self):
        calls=[]
        def bounded_wait(predicate,label):
            for _ in range(3):
                calls.append(predicate(self.fading()))
                if calls[-1]:return self.fading()
            raise AssertionError('timed out')
        with self.assertRaisesRegex(AssertionError,'timed out'):
            wait_clear_restore(bounded_wait,self.reference(),7,[390,241,500,350])
        self.assertEqual(calls,[False]*3)

    def test_reappearing_switcher_on_either_capture_side_fails(self):
        for scenes in ([self.fading(),self.reference()],[self.reference(),self.fading()]):
            capture=Mock(return_value='pixels')
            with self.assertRaisesRegex(AssertionError,'switcher present'):
                clear_capture(capture,Mock(side_effect=scenes),lambda scene:None)
        capture=Mock(return_value='unmodified pixels')
        self.assertEqual(clear_capture(capture,Mock(side_effect=[self.reference()]*2),lambda scene:None),'unmodified pixels')
        capture.assert_called_once()

    def test_clock_and_actor_changes_during_capture_fail(self):
        reference=self.reference()
        for change in ('clock','window','icon'):
            scene=copy.deepcopy(reference)
            if change=='clock':scene['animation_clock_ms']+=1
            if change=='window':scene['windows'][0]['id']=8
            if change=='icon':scene['window_lifecycle']['restoring'][0]['icon']=[0,0,0,0]
            with self.subTest(change=change),self.assertRaises(AssertionError):
                clear_capture(lambda:'pixels',Mock(side_effect=[reference,scene]),
                              lambda current:require_restore_bracket(reference,current,7,[390,241,500,350]))

    def test_retained_actor_brackets_reject_identity_and_clock_changes(self):
        reference={'animation_clock_ms':7775,'layers':[],'windows':[],
                   'window_lifecycle':{'destroying':[{'window':7,'rect':[580,198,379,288],'elapsed_ms':0}],
                                       'retained_destroy_bytes':1234}}
        require_destroy_bracket(reference,copy.deepcopy(reference),7)
        for change in ('clock','actor','geometry','bytes','live'):
            scene=copy.deepcopy(reference)
            if change=='clock':scene['animation_clock_ms']+=1
            if change=='actor':scene['window_lifecycle']['destroying'][0]['window']=8
            if change=='geometry':scene['window_lifecycle']['destroying'][0]['rect'][0]+=1
            if change=='bytes':scene['window_lifecycle']['retained_destroy_bytes']=0
            if change=='live':scene['windows']=[{'id':8}]
            with self.subTest(change=change),self.assertRaises(AssertionError):require_destroy_bracket(reference,scene,7)

    def test_exact_namespace_and_missing_inventory_fail_closed(self):
        self.assertTrue(switcher_absent({'layers':[{'namespace':'other-switcher'}]}))
        self.assertFalse(switcher_absent(self.fading()))
        for scene in ({},{'layers':None},{'layers':[{}]}):
            with self.assertRaises(AssertionError):switcher_absent(scene)

class ManualClockCapture(unittest.TestCase):
    def scene(self, clock=2652, frame=408, alpha=45/255):
        return {'animation_clock_ms': clock, 'rendered_frames': frame,
                'window_lifecycle': {'minimizing': [{'window': 1, 'alpha': alpha,
                    'duration_ms': 400, 'icon': [572, 706, 64, 64], 'live': True,
                    'monitor_sized': False, 'rect': [540, 624, 141, 114]}]}}

    def waiter(self, scenes, evidence):
        stream = iter(scenes)
        def wait(predicate, label):
            for scene in stream:
                if predicate(scene):
                    evidence[label] = scene
                    return scene
            raise AssertionError('timed out: ' + label)
        return wait

    def test_new_clock_with_stale_lifecycle_and_pre_poll_frames_cannot_settle(self):
        # Actual failure: frame 404 at old clock, then frame 406 acknowledges
        # the new clock but still publishes the full-size/full-alpha actor.
        old = self.scene(2552, 404, 1)
        unrelated = self.scene(2552, 405, 1)
        ack = self.scene(2652, 406, 1)
        ack['window_lifecycle']['minimizing'][0]['rect'] = [390, 241, 500, 350]
        settled = self.scene()
        evidence = {}
        result = wait_clock_frames(self.waiter([old, unrelated, ack,
                                  self.scene(frame=407), settled], evidence), 2652, 'clock-1050')
        self.assertIs(result, settled)
        self.assertIs(evidence['clock-1050-ack'], ack)
        self.assertIs(evidence['clock-1050'], settled)
        self.assertFalse(minimize_quarter(ack, 1, [572, 706, 64, 64]))
        self.assertTrue(minimize_quarter(result, 1, [572, 706, 64, 64]))
        with self.assertRaisesRegex(AssertionError, 'timed out'):
            wait_clock_frames(self.waiter([old, unrelated, ack, self.scene(frame=407)], {}),
                              2652, 'incomplete')

    def test_wrong_clock_cannot_acknowledge_or_settle(self):
        with self.assertRaisesRegex(AssertionError, 'timed out'):
            wait_clock_frames(self.waiter([self.scene(2653, 999)], {}), 2652, 'wrong')
        with self.assertRaisesRegex(AssertionError, 'clock changed'):
            wait_clock_frames(self.waiter([self.scene(frame=406), self.scene(2653, 999)], {}),
                              2652, 'changed')

    def test_quarter_requires_exact_single_identity_icon_duration_and_alpha(self):
        for change in ('missing', 'duplicate', 'window', 'icon', 'duration_ms', 'alpha'):
            scene = self.scene()
            actors = scene['window_lifecycle']['minimizing']
            if change == 'missing': actors.clear()
            elif change == 'duplicate': actors.append(copy.deepcopy(actors[0]))
            else: actors[0][change] = {'window': 2, 'icon': [0, 0, 64, 64],
                                     'duration_ms': 401, 'alpha': 1}[change]
            with self.subTest(change=change):
                self.assertFalse(minimize_quarter(scene, 1, [572, 706, 64, 64]))

    def test_both_capture_brackets_reject_clock_identity_and_presentation_changes(self):
        reference = self.scene()
        changes = {'window': 2, 'rect': [541, 624, 141, 114], 'alpha': 45/255 + 1e-6,
                   'icon': [573, 706, 64, 64], 'duration_ms': 401,
                   'live': False, 'monitor_sized': True}
        for field in ('clock', *changes):
            scene = copy.deepcopy(reference)
            if field == 'clock': scene['animation_clock_ms'] += 1
            else: scene['window_lifecycle']['minimizing'][0][field] = changes[field]
            for scenes in ([scene, reference], [reference, scene]):
                with self.subTest(field=field, before=scenes[0] is scene), self.assertRaises(AssertionError):
                    minimize_capture(lambda: 'pixels', Mock(side_effect=scenes), reference,
                                     1, [572, 706, 64, 64])

    def test_valid_capture_preserves_pixels_and_corruption_still_fails(self):
        reference = self.scene()
        read = Mock(side_effect=[reference, copy.deepcopy(reference)])
        capture = Mock(return_value=(52, 101, 164))
        pixels = minimize_capture(capture, read, reference, 1, [572, 706, 64, 64])
        capture.assert_called_once()
        self.assertEqual([call.args for call in read.call_args_list], [('before',), ('after',)])
        require_rgb(pixels, (52, 101, 164), 'valid bracket')
        corrupted = minimize_capture(lambda: (57, 101, 164), lambda _: reference,
                                     reference, 1, [572, 706, 64, 64])
        with self.assertRaises(AssertionError):
            require_rgb(corrupted, (52, 101, 164), 'corrupted pixel after valid bracket')


if __name__ == '__main__':
    unittest.main()
