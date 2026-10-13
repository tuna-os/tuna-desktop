#!/usr/bin/env python3
"""Unit tests for tuna-wellbeing-frame-proof.py (synthetic frames only).

Runtime gates capture real frames in the nested harness; these tests pin
the math (EASE_OUT_QUAD 3s ramps, 1.0/0.6 endpoints, immediate off),
the no-skip rule, owned-region proof (exact known interior RGB, stable
bound geometry, settled overview, layer exclusion), frame bracketing,
provenance fail-closed behavior and the negative controls. Malformed
geometry/image inputs must raise HarnessError (broken harness), never
a pass and never a pixel verdict. No cargo, no compositor required.
"""
import importlib.util
import json
import os
import sys
import tempfile
import time
import unittest
from pathlib import Path

sys.dont_write_bytecode = True
from PIL import Image  # noqa: E402

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location(
    'wb_frame', HERE / 'tuna-wellbeing-frame-proof.py')
mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(mod)

PURPLE = (143, 63, 208)
RED = (224, 27, 36)
APP = 'tuna-cursor-probe'
RECT = [0, 0, 32, 32]


def solid(color, size=(32, 32)):
    img = Image.new('RGB', size)
    img.paste(color, (0, 0, size[0], size[1]))
    return img


def transformed(color, desat, bright):
    return tuple(round(c) for c in mod.transform(color, desat, bright))


def owner(**over):
    record = {'pid': os.getpid(), 'uid': os.getuid(), 'start_ticks': 4242,
              'exe': '/bin/probe', 'id': 7, 'application_window_id': 70,
              'app_id': APP, 'rect': list(RECT)}
    record.update(over)
    return record


def proc(boot='boot-1', pid=4242, start=777, exe='e' * 64):
    return {'boot_id': boot,
            'processes': {name: {'pid': pid, 'uid': 1000,
                                 'start_ticks': start, 'exe_sha256': exe}
                          for name in ('probe', 'compositor', 'shell')}}


def cstate(path, clock_ms, desat, bright, animating, manual=True, motion='full',
           rect=None, overview_open=False, overview_progress=0, layers=None,
           app_id=APP, x11=None, wid=7, app_window=70):
    path.write_text(json.dumps({
        'animation_clock_manual': manual,
        'animation_clock_ms': clock_ms,
        'motion_policy': motion,
        'overview_open': overview_open,
        'overview_progress': overview_progress,
        'layers': [] if layers is None else layers,
        'windows': [{'id': wid, 'app_id': app_id, 'x11_window_id': x11,
                     'application_window_id': app_window,
                     'workspace': 1, 'rect': list(RECT) if rect is None else rect}],
        'wellbeing_effects': {'desaturation': desat, 'brightness': bright,
                              'animating': animating},
    }))


class PinnedBudgets(unittest.TestCase):
    def test_tolerances_are_pinned_no_widening(self):
        self.assertEqual(mod.TOLERANCE, 2)
        self.assertEqual(mod.OWNED_RGB_TOLERANCE, 1)
        self.assertEqual(mod.OWNED_RGB, (143, 63, 208))
        self.assertEqual(mod.CHROMA_MIN, 20)


class FrameMath(unittest.TestCase):
    def test_ease_out_quad_endpoints_and_midpoint(self):
        self.assertEqual(mod.ease_out_quad(0.0), 0.0)
        self.assertEqual(mod.ease_out_quad(1.0), 1.0)
        self.assertAlmostEqual(mod.ease_out_quad(0.5), 0.75)
        self.assertAlmostEqual(mod.ease_out_quad(1 / 3), 5 / 9)

    def test_expected_samples_match_gnome_3s_ramps(self):
        d, b = mod.expected_sample(0, 3000, True, True)
        self.assertEqual((d, b), (0.0, 1.0))
        d, b = mod.expected_sample(1500, 3000, True, True)
        self.assertAlmostEqual(d, 0.75)
        self.assertAlmostEqual(b, 0.7)
        d, b = mod.expected_sample(3000, 3000, True, True)
        self.assertAlmostEqual(d, 1.0)
        self.assertAlmostEqual(b, 0.6)
        d, b = mod.expected_sample(1000, 3000, True, False)
        self.assertAlmostEqual(d, 5 / 9)
        self.assertEqual(b, 1.0)
        d, b = mod.expected_sample(3000, 3000, False, True)
        self.assertEqual(d, 0.0)
        self.assertAlmostEqual(b, 0.6)

    def test_gray_endpoint_is_achromatic_and_fade_endpoint_is_06(self):
        gray = mod.transform(PURPLE, 1.0, 1.0)
        self.assertLess(max(gray) - min(gray), 1e-6)
        faded = mod.transform(RED, 0.0, 0.6)
        for got, want in zip(faded, (134.4, 16.2, 21.6)):
            self.assertAlmostEqual(got, want)

    def test_intermediate_rgb_is_independently_expected(self):
        # 1500ms gray-only on the saturated purple producer.
        d, b = mod.expected_sample(1500, 3000, True, False)
        rgb = transformed(PURPLE, d, b)
        gray = sum(c * w for c, w in zip(PURPLE, (.299, .587, .114)))
        manual = tuple(round((c + (gray - c) * 0.75) * 1.0) for c in PURPLE)
        self.assertEqual(rgb, manual)
        self.assertEqual(manual, (113, 93, 130))

    def test_verify_frame_accepts_exact_effect_and_rejects_neutral(self):
        regions = [{'name': 'probe', 'kind': 'texture', 'rect': [8, 8, 16, 16]}]
        before = solid(PURPLE)
        after = solid(PURPLE)
        after.paste(transformed(PURPLE, 1.0, 1.0), (8, 8, 24, 24))
        mod.verify_frame(before, after, regions, 1.0, 1.0)
        with self.assertRaises(AssertionError):
            mod.verify_frame(before, solid(PURPLE), regions, 1.0, 1.0)

    def test_neutral_fixture_cannot_prove_desaturation(self):
        regions = [{'name': 'probe', 'kind': 'texture', 'rect': [0, 0, 16, 16]}]
        with self.assertRaises(mod.HarnessError):
            mod.verify_frame(solid((80, 80, 80)), solid((48, 48, 48)), regions, 1.0, 0.6)

    def test_pixel_mismatch_is_effect_mismatch_assertion(self):
        regions = [{'name': 'probe', 'kind': 'texture', 'rect': [8, 8, 16, 16]}]
        self.assertTrue(issubclass(mod.EffectMismatch, AssertionError))
        with self.assertRaises(mod.EffectMismatch):
            mod.verify_frame(solid(PURPLE), solid(PURPLE), regions, 1.0, 1.0)

    def test_cursor_region_bypasses_effect(self):
        regions = [{'name': 'cursor', 'kind': 'cursor', 'rect': [0, 0, 16, 16]}]
        mod.verify_frame(solid(RED), solid(RED), regions, 1.0, 0.6,
                         required_kinds=frozenset(('cursor',)))
        with self.assertRaises(AssertionError):
            mod.verify_frame(solid(RED), solid((40, 40, 40)), regions, 1.0, 0.6,
                             required_kinds=frozenset(('cursor',)))


class NeutralControl(unittest.TestCase):
    """Malformed inputs are harness errors, never passes or pixel verdicts."""

    def regions(self, **over):
        region = {'name': 'probe', 'kind': 'texture', 'rect': [8, 8, 16, 16]}
        region.update(over)
        return [region]

    def test_rect_must_be_4_ints(self):
        with self.assertRaises(mod.HarnessError):
            mod.verify_frame(solid(PURPLE), solid(PURPLE),
                             self.regions(rect=[8, 8, 16]), 0.0, 1.0)
        with self.assertRaises(mod.HarnessError):
            mod.verify_frame(solid(PURPLE), solid(PURPLE),
                             self.regions(rect=[8.0, 8, 16, 16]), 0.0, 1.0)
        with self.assertRaises(mod.HarnessError):
            mod.verify_frame(solid(PURPLE), solid(PURPLE),
                             self.regions(rect=[True, 8, 16, 16]), 0.0, 1.0)

    def test_rect_must_fit_and_be_non_degenerate(self):
        with self.assertRaises(mod.HarnessError):
            mod.verify_frame(solid(PURPLE), solid(PURPLE),
                             self.regions(rect=[24, 24, 16, 16]), 0.0, 1.0)
        with self.assertRaises(mod.HarnessError):
            mod.verify_frame(solid(PURPLE), solid(PURPLE),
                             self.regions(rect=[-1, 8, 16, 16]), 0.0, 1.0)
        with self.assertRaises(mod.HarnessError):
            mod.verify_frame(solid(PURPLE), solid(PURPLE),
                             self.regions(rect=[8, 8, 1, 16]), 0.0, 1.0)

    def test_names_unique_kinds_known_required_present(self):
        with self.assertRaises(mod.HarnessError):
            mod.verify_frame(solid(PURPLE), solid(PURPLE),
                             self.regions() + self.regions(), 0.0, 1.0)
        with self.assertRaises(mod.HarnessError):
            mod.verify_frame(solid(PURPLE), solid(PURPLE),
                             self.regions(kind='bogus'), 0.0, 1.0)
        with self.assertRaises(mod.HarnessError):
            mod.verify_frame(solid(PURPLE), solid(PURPLE), [], 0.0, 1.0)
        with self.assertRaises(mod.HarnessError):
            mod.verify_frame(
                solid(PURPLE), solid(PURPLE),
                [{'name': 'c', 'kind': 'cursor', 'rect': [0, 0, 16, 16]}],
                0.0, 1.0)

    def test_effect_out_of_range_is_harness_error(self):
        with self.assertRaises(mod.HarnessError):
            mod.verify_frame(solid(PURPLE), solid(PURPLE),
                             self.regions(), 1.5, 1.0)
        with self.assertRaises(mod.HarnessError):
            mod.verify_frame(solid(PURPLE), solid(PURPLE),
                             self.regions(), 1.0, 0.5)

    def test_size_mismatch_is_harness_error(self):
        with self.assertRaises(mod.HarnessError):
            mod.verify_frame(solid(PURPLE), solid(PURPLE, (16, 16)),
                             self.regions(), 0.0, 1.0)

    def test_unreadable_image_is_harness_error(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            missing = tmp / 'gone.png'
            with self.assertRaises(mod.HarnessError):
                mod.open_pair(missing, missing)
            broken = tmp / 'broken.png'
            broken.write_text('not a png')
            with self.assertRaises(mod.HarnessError):
                mod.open_pair(broken, broken)

    def test_malformed_cstate_is_harness_error(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            with self.assertRaises(mod.HarnessError):
                mod.load_cstate(tmp / 'gone.json')
            broken = tmp / 'broken.json'
            broken.write_text('{oops')
            with self.assertRaises(mod.HarnessError):
                mod.load_cstate(broken)
            no_wb = tmp / 'no-wb.json'
            no_wb.write_text(json.dumps({'windows': []}))
            with self.assertRaises(mod.HarnessError):
                mod.check_cstate_sample(no_wb, 0.0, 1.0, False)

    def test_negative_neutral_propagates_harness_error(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            base = tmp / 'base.png'
            solid(PURPLE).save(base)
            with self.assertRaises(mod.HarnessError):
                mod.negative_neutral_must_fail(
                    base, base, self.regions(rect=[0, 0, 99, 99]), 1.0, 0.6)

    def test_harness_error_is_not_a_pixel_assertion(self):
        self.assertFalse(issubclass(mod.HarnessError, AssertionError))


class OwnedInterior(unittest.TestCase):
    def regions(self):
        return [{'name': 'probe', 'kind': 'texture', 'rect': [8, 8, 16, 16]}]

    def test_exact_known_rgb_passes(self):
        receipt = mod.check_owned_interior(solid(PURPLE), self.regions())
        self.assertEqual(receipt['pixels'], 256)
        self.assertEqual(receipt['maximum_error'], 0)

    def test_rounding_within_1_passes(self):
        img = solid(PURPLE)
        img.putpixel((9, 9), (144, 63, 207))
        receipt = mod.check_owned_interior(img, self.regions())
        self.assertEqual(receipt['maximum_error'], 1)

    def test_overlapped_region_rejected(self):
        # An overlapping popup paints foreign pixels over the sample.
        img = solid(PURPLE)
        img.paste((10, 10, 10), (8, 8, 24, 24))
        with self.assertRaises(AssertionError):
            mod.check_owned_interior(img, self.regions())

    def test_drift_beyond_1_rejected(self):
        img = solid(PURPLE)
        img.putpixel((9, 9), (145, 63, 208))
        with self.assertRaises(AssertionError):
            mod.check_owned_interior(img, self.regions())

    def test_malformed_regions_are_harness_error(self):
        with self.assertRaises(mod.HarnessError):
            mod.check_owned_interior(solid(PURPLE), [])


class Bracketing(unittest.TestCase):
    def docs(self, tmp, clock=10000, desat=0.0, bright=1.0, anim=False,
             rect=None, **kw):
        before, after = tmp / 'b.json', tmp / 'a.json'
        cstate(before, clock, desat, bright, anim, rect=rect, **kw)
        cstate(after, clock, desat, bright, anim, rect=rect, **kw)
        return json.loads(before.read_text()), json.loads(after.read_text())

    def regions(self):
        return [{'name': 'probe', 'kind': 'texture', 'rect': [8, 8, 16, 16]}]

    def test_identical_bracket_passes(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after = self.docs(tmp)
            mod.check_bracket(before, after, owner(), self.regions(), 't')

    def test_mid_bracket_clock_change_fails(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after = self.docs(tmp)
            after['animation_clock_ms'] = 10500
            with self.assertRaises(AssertionError):
                mod.check_bracket(before, after, owner(), self.regions(), 't')

    def test_mid_bracket_manual_mode_change_fails(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after = self.docs(tmp)
            after['animation_clock_manual'] = False
            with self.assertRaises(AssertionError):
                mod.check_bracket(before, after, owner(), self.regions(), 't')

    def test_mid_bracket_effect_change_fails(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after = self.docs(tmp)
            after['wellbeing_effects']['brightness'] = 0.9
            with self.assertRaises(AssertionError):
                mod.check_bracket(before, after, owner(), self.regions(), 't')

    def test_mid_bracket_geometry_change_fails(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after = self.docs(tmp)
            after['windows'][0]['rect'] = [1, 0, 32, 32]
            with self.assertRaises(AssertionError):
                mod.check_bracket(before, after, owner(), self.regions(), 't')

    def test_geometry_outside_bound_rect_fails(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after = self.docs(tmp, rect=[64, 64, 32, 32])
            with self.assertRaises(AssertionError):
                mod.check_bracket(before, after, owner(), self.regions(), 't')

    def test_bound_identity_must_match(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after = self.docs(tmp, wid=9, app_window=90)
            with self.assertRaises(AssertionError):
                mod.check_bracket(before, after, owner(), self.regions(), 't')

    def test_sampled_region_must_stay_inside_owned_window(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after = self.docs(tmp)
            outside = [{'name': 'probe', 'kind': 'texture', 'rect': [24, 24, 16, 16]}]
            with self.assertRaises(mod.HarnessError):
                mod.check_bracket(before, after, owner(), outside, 't')

    def test_missing_owner_is_harness_error(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after = self.docs(tmp)
            with self.assertRaises(mod.HarnessError):
                mod.check_bracket(before, after, None, self.regions(), 't')

    def test_overview_must_be_settled(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after = self.docs(tmp, overview_open=True)
            with self.assertRaises(AssertionError):
                mod.check_bracket(before, after, owner(), self.regions(), 't')
            before, after = self.docs(tmp, overview_progress=0.5)
            with self.assertRaises(AssertionError):
                mod.check_bracket(before, after, owner(), self.regions(), 't')

    def test_layer_overlapping_sample_fails(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            layers = [{'namespace': 'tuna-shell-switcher', 'rect': [0, 0, 32, 32]}]
            before, after = self.docs(tmp, layers=layers)
            with self.assertRaises(AssertionError):
                mod.check_bracket(before, after, owner(), self.regions(), 't')

    def test_layer_clear_of_sample_passes(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            layers = [{'namespace': 'panel', 'rect': [0, 0, 32, 4]}]
            before, after = self.docs(tmp, layers=layers)
            mod.check_bracket(before, after, owner(), self.regions(), 't')

    def test_malformed_layer_is_harness_error(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after = self.docs(tmp, layers=[{'namespace': 'x'}])
            with self.assertRaises(mod.HarnessError):
                mod.check_bracket(before, after, owner(), self.regions(), 't')


class RampRules(unittest.TestCase):
    def build_ramp(self, tmp, clocks, grayscale=True, fade=False, start=10000):
        regions = [{'name': 'probe', 'kind': 'texture', 'rect': [8, 8, 16, 16]}]
        (tmp / 'regions.json').write_text(json.dumps(regions))
        (tmp / 'owner.json').write_text(json.dumps(owner()))
        base = tmp / 'base.png'
        solid(PURPLE).save(base)
        caps = []
        for ms in clocks:
            elapsed = ms - start
            d, b = mod.expected_sample(elapsed, 3000, grayscale, fade)
            img = solid(PURPLE)
            img.paste(transformed(PURPLE, d, b), (8, 8, 24, 24))
            png = tmp / f'c{ms}.png'
            img.save(png)
            anim = elapsed < 3000 if (grayscale is True or fade is True) else False
            if grayscale == 'settled':
                anim = elapsed < 3000 if fade is True else False
            before, after = tmp / f'c{ms}-before.json', tmp / f'c{ms}-after.json'
            cstate(before, ms, d, b, anim)
            cstate(after, ms, d, b, anim)
            snap = proc()
            caps.append({'label': f'c{ms}', 'image': str(png),
                         'cstate_before': str(before), 'cstate_after': str(after),
                         'clock_ms': ms, 'epoch': time.time(), 'proc': snap,
                         'proc_before': json.loads(json.dumps(snap))})
        caps_path = tmp / 'caps.json'
        caps_path.write_text(json.dumps(caps))
        return base, caps_path, regions

    def test_full_ramp_passes_with_intermediates(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            gate_start = time.time() - 1
            base, caps, regions = self.build_ramp(
                tmp, [10000, 10500, 11000, 11500, 12000, 12500, 13000])
            receipt = mod.verify_ramp(base, caps, regions, True, False, 10000,
                                      owner(), gate_start_epoch=gate_start)
            self.assertEqual(len(receipt['captures']), 7)

    def test_ramp_requires_owner(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            gate_start = time.time() - 1
            base, caps, regions = self.build_ramp(tmp, [10000, 11500, 13000])
            with self.assertRaises(mod.HarnessError):
                mod.verify_ramp(base, caps, regions, True, False, 10000,
                                None, gate_start_epoch=gate_start)

    def test_ramp_rejects_legacy_single_cstate(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            gate_start = time.time() - 1
            base, caps, regions = self.build_ramp(tmp, [10000, 11500, 13000])
            legacy = json.loads(caps.read_text())
            for cap in legacy:
                del cap['cstate_before']
                cap['cstate'] = cap.pop('cstate_after')
            caps.write_text(json.dumps(legacy))
            with self.assertRaises(mod.HarnessError):
                mod.verify_ramp(base, caps, regions, True, False, 10000,
                                owner(), gate_start_epoch=gate_start)

    def test_skipped_ramp_fails_closed(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            gate_start = time.time() - 1
            base, caps, regions = self.build_ramp(tmp, [10000, 13000])
            with self.assertRaises(AssertionError):
                mod.verify_ramp(base, caps, regions, True, False, 10000,
                                owner(), gate_start_epoch=gate_start)

    def test_endpoints_only_without_midpoint_fails(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            gate_start = time.time() - 1
            base, caps, regions = self.build_ramp(
                tmp, [10000, 10200, 12800, 13000])
            with self.assertRaises(AssertionError):
                mod.verify_ramp(base, caps, regions, True, False, 10000,
                                owner(), gate_start_epoch=gate_start)

    def test_wrong_intermediate_fails_assertion_not_identity(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            gate_start = time.time() - 1
            base, caps, regions = self.build_ramp(
                tmp, [10000, 11500, 13000])
            # Corrupt the midpoint frame to neutral: same source, same
            # helper, only pixels differ -> assertion failure.
            solid(PURPLE).save(tmp / 'c11500.png')
            with self.assertRaises(AssertionError):
                mod.verify_ramp(base, caps, regions, True, False, 10000,
                                owner(), gate_start_epoch=gate_start)

    def test_immediate_off_requires_neutral_at_same_clock(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            gate_start = time.time() - 1
            regions = [{'name': 'probe', 'kind': 'texture', 'rect': [8, 8, 16, 16]}]
            base = tmp / 'base.png'
            solid(PURPLE).save(base)
            off = tmp / 'off.png'
            solid(PURPLE).save(off)
            before, after = tmp / 'off-before.json', tmp / 'off-after.json'
            cstate(before, 13000, 0.0, 1.0, False)
            cstate(after, 13000, 0.0, 1.0, False)
            mod.verify_immediate_off(base, off, before, after, regions, 13000,
                                     owner(), gate_start_epoch=gate_start)
            faded = tmp / 'faded.png'
            img = solid(PURPLE)
            img.paste(transformed(PURPLE, 1.0, 1.0), (8, 8, 24, 24))
            img.save(faded)
            with self.assertRaises(AssertionError):
                mod.verify_immediate_off(base, faded, before, after, regions,
                                         13000, owner(), gate_start_epoch=gate_start)

    def test_immediate_off_rejects_bracket_drift(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            gate_start = time.time() - 1
            regions = [{'name': 'probe', 'kind': 'texture', 'rect': [8, 8, 16, 16]}]
            base = tmp / 'base.png'
            solid(PURPLE).save(base)
            off = tmp / 'off.png'
            solid(PURPLE).save(off)
            before, after = tmp / 'off-before.json', tmp / 'off-after.json'
            cstate(before, 13000, 0.0, 1.0, False)
            cstate(after, 13500, 0.0, 1.0, False)
            with self.assertRaises(AssertionError):
                mod.verify_immediate_off(base, off, before, after, regions,
                                         13000, owner(), gate_start_epoch=gate_start)

    def test_negative_neutral_must_fail(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            regions = [{'name': 'probe', 'kind': 'texture', 'rect': [8, 8, 16, 16]}]
            base = tmp / 'base.png'
            solid(PURPLE).save(base)
            neutral = tmp / 'neutral.png'
            solid(PURPLE).save(neutral)
            self.assertTrue(mod.negative_neutral_must_fail(
                base, neutral, regions, 1.0, 0.6))
            effected = tmp / 'effected.png'
            img = solid(PURPLE)
            img.paste(transformed(PURPLE, 1.0, 0.6), (8, 8, 24, 24))
            img.save(effected)
            with self.assertRaises(AssertionError):
                mod.negative_neutral_must_fail(base, effected, regions, 1.0, 0.6)

    def test_settled_gray_with_fade_ramp(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            gate_start = time.time() - 1
            regions = [{'name': 'probe', 'kind': 'texture', 'rect': [8, 8, 16, 16]}]
            base = tmp / 'base.png'
            solid(PURPLE).save(base)
            caps = []
            for ms in (20000, 20500, 21000, 21500, 22000, 22500, 23000):
                elapsed = ms - 20000
                d, b = mod.expected_sample(elapsed, 3000, 'settled', True)
                self.assertEqual(d, 1.0)
                img = solid(PURPLE)
                img.paste(transformed(PURPLE, d, b), (8, 8, 24, 24))
                png = tmp / f's{ms}.png'
                img.save(png)
                before, after = tmp / f's{ms}-before.json', tmp / f's{ms}-after.json'
                cstate(before, ms, d, b, elapsed < 3000)
                cstate(after, ms, d, b, elapsed < 3000)
                snap = proc()
                caps.append({'label': f's{ms}', 'image': str(png),
                             'cstate_before': str(before), 'cstate_after': str(after),
                             'clock_ms': ms, 'epoch': time.time(), 'proc': snap,
                             'proc_before': json.loads(json.dumps(snap))})
            caps_path = tmp / 'caps.json'
            caps_path.write_text(json.dumps(caps))
            receipt = mod.verify_ramp(base, caps_path, regions, 'settled', True, 20000,
                                      owner(), gate_start_epoch=gate_start)
            self.assertEqual(len(receipt['captures']), 7)
            last = receipt['captures'][-1]
            self.assertAlmostEqual(last['desaturation'], 1.0)
            self.assertAlmostEqual(last['brightness'], 0.6)


class BracketedCLIVerdicts(unittest.TestCase):
    def baseline(self, tmp):
        regions = [{'name': 'probe', 'kind': 'texture', 'rect': [8, 8, 16, 16]}]
        regions_path = tmp / 'regions.json'
        regions_path.write_text(json.dumps(regions))
        owner_path = tmp / 'owner.json'
        owner_path.write_text(json.dumps(owner()))
        base = tmp / 'base.png'
        solid(PURPLE).save(base)
        before, after = tmp / 'b.json', tmp / 'a.json'
        cstate(before, 10000, 0.0, 1.0, False)
        cstate(after, 10000, 0.0, 1.0, False)
        return base, regions_path, owner_path, before, after

    def test_owned_interior_cli_with_bracket_passes(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            gate_start = time.time() - 1
            base, regions, owner_path, before, after = self.baseline(tmp)
            code = mod.main(['owned-interior', '--image', str(base),
                             '--regions', str(regions),
                             '--receipt', str(tmp / 'r.json'),
                             '--cstate-before', str(before),
                             '--cstate-after', str(after),
                             '--owner', str(owner_path),
                             '--gate-start', str(gate_start)])
            self.assertEqual(code, 0)

    def test_partial_bracket_is_harness_error_exit_2(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            base, regions, _, before, _ = self.baseline(tmp)
            code = mod.main(['owned-interior', '--image', str(base),
                             '--regions', str(regions),
                             '--receipt', str(tmp / 'r.json'),
                             '--cstate-before', str(before)])
            self.assertEqual(code, 2)

    def test_verify_cli_with_bracket_passes(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            gate_start = time.time() - 1
            base, regions, owner_path, before, after = self.baseline(tmp)
            code = mod.main(['verify', '--baseline', str(base),
                             '--current', str(base),
                             '--regions', str(regions),
                             '--desaturation', '0', '--brightness', '1',
                             '--receipt', str(tmp / 'r.json'),
                             '--cstate-before', str(before),
                             '--cstate-after', str(after),
                             '--owner', str(owner_path),
                             '--gate-start', str(gate_start)])
            self.assertEqual(code, 0)


class ProcSnapshot(unittest.TestCase):
    def test_live_snapshot_binds_exe_bytes_start_and_boot(self):
        snap = mod.proc_snapshot({'probe': os.getpid()})
        self.assertTrue(snap['boot_id'])
        entry = snap['processes']['probe']
        self.assertEqual(entry['pid'], os.getpid())
        self.assertGreater(entry['start_ticks'], 0)
        self.assertEqual(len(entry['exe_sha256']), 64)

    def test_dead_pid_is_harness_error(self):
        with self.assertRaises(mod.HarnessError):
            mod.proc_snapshot({'probe': 2**31 - 1})

    def test_agreement_rejects_starttime_change(self):
        first = {'label': 'a', 'proc': proc(start=777)}
        second = {'label': 'b', 'proc': proc(start=778)}
        with self.assertRaises(AssertionError):
            mod.check_proc_agreement([first, second])

    def test_agreement_rejects_exe_swap(self):
        first = {'label': 'a', 'proc': proc(exe='e' * 64)}
        second = {'label': 'b', 'proc': proc(exe='f' * 64)}
        with self.assertRaises(AssertionError):
            mod.check_proc_agreement([first, second])

    def test_agreement_rejects_boot_change(self):
        first = {'label': 'a', 'proc': proc(boot='boot-1')}
        second = {'label': 'b', 'proc': proc(boot='boot-2')}
        with self.assertRaises(AssertionError):
            mod.check_proc_agreement([first, second])


class ProvenanceRules(unittest.TestCase):
    REGIONS = [{'name': 'probe', 'kind': 'texture', 'rect': [8, 8, 16, 16]}]

    def build(self, tmp, tamper=None):
        gate_start = time.time() - 1
        blob = tmp / 'bin'
        blob.write_bytes(b'proof-binary')
        img = tmp / 'shot.png'
        solid(PURPLE).save(img)
        before, after = tmp / 'shot-before.json', tmp / 'shot-after.json'
        cstate(before, 10000, 0.0, 1.0, False)
        cstate(after, 10000, 0.0, 1.0, False)
        exe = mod.digest(blob)
        snap = proc(exe=exe)
        if tamper:
            tamper(snap)
        caps = [{'label': 's', 'image': str(img),
                 'cstate_before': str(before), 'cstate_after': str(after),
                 'clock_ms': 10000, 'epoch': time.time(), 'iso': 't',
                 'proc': snap,
                 'proc_before': json.loads(json.dumps(snap))}]
        binaries = {'compositor': str(blob), 'shell': str(blob),
                    'probe': str(blob)}
        return gate_start, binaries, caps

    def record(self, tmp, binaries, caps):
        return mod.record_provenance('G-TEST', '0ac3ec7bce96', binaries, caps,
                                     owner(), self.REGIONS, tmp / 'prov.json',
                                     time.time() - 1)

    def test_provenance_binds_live_bytes(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            _, binaries, caps = self.build(tmp)
            receipt = self.record(tmp, binaries, caps)
            self.assertEqual(receipt['gate'], 'G-TEST')
            self.assertEqual(len(receipt['captures']), 1)

    def test_provenance_requires_owner_and_regions(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            gate_start, binaries, caps = self.build(tmp)
            with self.assertRaises(mod.HarnessError):
                mod.record_provenance('G-TEST', '0ac3ec7bce96', binaries, caps,
                                      None, self.REGIONS, tmp / 'prov.json', gate_start)
            with self.assertRaises(mod.HarnessError):
                mod.record_provenance('G-TEST', '0ac3ec7bce96', binaries, caps,
                                      owner(), [], tmp / 'prov.json', gate_start)

    def test_provenance_fails_closed_on_missing_or_stale(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            _, binaries, caps = self.build(tmp)
            bad = dict(binaries, compositor=str(tmp / 'gone'))
            with self.assertRaises(AssertionError):
                self.record(tmp, bad, caps)
            with self.assertRaises(AssertionError):
                mod.record_provenance('G-TEST', '', binaries, caps, owner(),
                                      self.REGIONS, tmp / 'prov.json', time.time() - 1)

    def test_provenance_rejects_path_only_hash(self):
        # The admitted path hashes to other bytes than the live process
        # executes: not the same binary, however plausible the path.
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            _, binaries, caps = self.build(tmp)
            other = tmp / 'other'
            other.write_bytes(b'different-binary')
            bad = dict(binaries, shell=str(other))
            with self.assertRaises(AssertionError):
                self.record(tmp, bad, caps)

    def test_provenance_rejects_missing_proc(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            _, binaries, caps = self.build(tmp)
            del caps[0]['proc']
            with self.assertRaises(mod.HarnessError):
                self.record(tmp, binaries, caps)

    def test_provenance_rejects_mid_gate_replacement(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            _, binaries, caps = self.build(tmp)
            second = json.loads(json.dumps(caps[0]))
            second['label'] = 's2'
            second['proc'] = proc(exe=caps[0]['proc']['processes']['probe']['exe_sha256'])
            second['proc']['processes']['probe']['start_ticks'] = 999999
            with self.assertRaises(AssertionError):
                self.record(tmp, binaries, caps + [second])

    def test_provenance_rejects_bracket_drift(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            _, binaries, caps = self.build(tmp)
            drifted = tmp / 'drifted.json'
            cstate(drifted, 10500, 0.0, 1.0, False)
            caps[0]['cstate_after'] = str(drifted)
            with self.assertRaises(AssertionError):
                self.record(tmp, binaries, caps)

    def test_provenance_rejects_unsettled_overview(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            _, binaries, caps = self.build(tmp)
            open_ov = tmp / 'overview.json'
            cstate(open_ov, 10000, 0.0, 1.0, False, overview_progress=1)
            caps[0]['cstate_before'] = str(open_ov)
            caps[0]['cstate_after'] = str(open_ov)
            with self.assertRaises(AssertionError):
                self.record(tmp, binaries, caps)

    def test_provenance_receipt_is_labeled_live_self_consistency(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            _, binaries, caps = self.build(tmp)
            receipt = self.record(tmp, binaries, caps)
            self.assertEqual(receipt['attestation'], mod.ATTESTATION_LABEL)
            self.assertIn('outer CI build admission', receipt['attestation'])
            self.assertIn('never standalone attestation',
                          receipt['attestation'])

    def test_provenance_rejects_mid_capture_replacement(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            _, binaries, caps = self.build(tmp)
            caps[0]['proc']['processes']['probe']['start_ticks'] = 999999
            with self.assertRaises(AssertionError):
                self.record(tmp, binaries, caps)

    def test_provenance_rejects_capture_without_before_snapshot(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            _, binaries, caps = self.build(tmp)
            del caps[0]['proc_before']
            with self.assertRaises(mod.HarnessError):
                self.record(tmp, binaries, caps)


class MismatchOnlyNeutral(unittest.TestCase):
    REGIONS = [{'name': 'probe', 'kind': 'texture', 'rect': [8, 8, 16, 16]}]

    def test_achromatic_baseline_is_invalid_fixture(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            gray = tmp / 'gray.png'
            solid((80, 80, 80)).save(gray)
            with self.assertRaises(mod.HarnessError):
                mod.negative_neutral_must_fail(
                    gray, gray, self.REGIONS, 1.0, 0.6)

    def test_unowned_baseline_is_invalid_fixture(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            red = tmp / 'red.png'
            solid(RED).save(red)
            with self.assertRaises(mod.HarnessError):
                mod.negative_neutral_must_fail(
                    red, red, self.REGIONS, 1.0, 0.6)

    def test_saturated_owned_neutral_still_fails_the_effect(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            base = tmp / 'base.png'
            solid(PURPLE).save(base)
            self.assertTrue(mod.negative_neutral_must_fail(
                base, base, self.REGIONS, 1.0, 0.6))

    def test_fixture_error_is_not_swallowed_as_mismatch(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            gray = tmp / 'gray.png'
            solid((80, 80, 80)).save(gray)
            try:
                mod.negative_neutral_must_fail(
                    gray, gray, self.REGIONS, 1.0, 0.6)
            except mod.EffectMismatch:
                self.fail('invalid fixture must not report EffectMismatch')
            except mod.HarnessError:
                pass


class BracketSchema(unittest.TestCase):
    def docs(self, tmp):
        before, after = tmp / 'b.json', tmp / 'a.json'
        cstate(before, 10000, 0.0, 1.0, False)
        cstate(after, 10000, 0.0, 1.0, False)
        return json.loads(before.read_text()), json.loads(after.read_text())

    def regions(self):
        return [{'name': 'probe', 'kind': 'texture', 'rect': [8, 8, 16, 16]}]

    def test_untyped_clock_is_harness_error_not_equal(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            for manual, ms in ((None, None), ('true', 10000), (True, '10000'),
                               (True, 10000.0), (1, 10000), (True, True),
                               (True, -5)):
                before, after = self.docs(tmp)
                before['animation_clock_manual'] = manual
                after['animation_clock_manual'] = manual
                before['animation_clock_ms'] = ms
                after['animation_clock_ms'] = ms
                with self.assertRaises(mod.HarnessError,
                                       msg=f'manual={manual!r} ms={ms!r}'):
                    mod.check_bracket(before, after, owner(), self.regions(),
                                      't')

    def test_missing_clock_is_harness_error(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after = self.docs(tmp)
            del before['animation_clock_ms']
            with self.assertRaises(mod.HarnessError):
                mod.check_bracket(before, after, owner(), self.regions(), 't')

    def test_untyped_effect_is_harness_error(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            for key, bad in (('desaturation', 'none'), ('brightness', 'high'),
                             ('desaturation', None), ('brightness', None),
                             ('brightness', True), ('desaturation', False),
                             ('brightness', float('nan')),
                             ('desaturation', float('inf')),
                             ('animating', 1), ('animating', 'yes')):
                before, after = self.docs(tmp)
                before['wellbeing_effects'][key] = bad
                after['wellbeing_effects'][key] = bad
                with self.assertRaises(mod.HarnessError,
                                       msg=f'{key}={bad!r}'):
                    mod.check_bracket(before, after, owner(), self.regions(),
                                      't')

    def test_missing_effect_field_is_harness_error(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            for key in ('desaturation', 'brightness', 'animating'):
                before, after = self.docs(tmp)
                del before['wellbeing_effects'][key]
                del after['wellbeing_effects'][key]
                with self.assertRaises(mod.HarnessError, msg=key):
                    mod.check_bracket(before, after, owner(), self.regions(),
                                      't')

    def test_degenerate_geometry_is_harness_error(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            for bad_rect in ([0, 0, 1, 32], [0, 0, 32, 0], [0, 0, -4, 32],
                             [0, 0, 32], 'rect', None, [0, 0, True, 32]):
                before, after = self.docs(tmp)
                before['windows'][0]['rect'] = bad_rect
                after['windows'][0]['rect'] = bad_rect
                with self.assertRaises(mod.HarnessError,
                                       msg=f'rect={bad_rect!r}'):
                    mod.check_bracket(before, after, owner(), self.regions(),
                                      't')

    def test_untyped_owner_rect_is_harness_error(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after = self.docs(tmp)
            with self.assertRaises(mod.HarnessError):
                mod.check_bracket(before, after, owner(rect=[0, 0, 1, 1]),
                                  self.regions(), 't')

    def test_typed_identical_bracket_passes(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            before, after = self.docs(tmp)
            mod.check_bracket(before, after, owner(), self.regions(), 't')


class ProcSandwich(unittest.TestCase):
    def test_held_sandwich_passes(self):
        snap = proc()
        self.assertTrue(mod.check_proc_sandwich(
            json.loads(json.dumps(snap)), snap, 's'))

    def test_mid_capture_replacement_fails(self):
        with self.assertRaises(AssertionError):
            mod.check_proc_sandwich(proc(start=777), proc(start=778), 's')

    def test_mid_capture_exe_swap_fails(self):
        with self.assertRaises(AssertionError):
            mod.check_proc_sandwich(proc(exe='e' * 64), proc(exe='f' * 64),
                                    's')

    def test_mid_capture_boot_change_fails(self):
        with self.assertRaises(AssertionError):
            mod.check_proc_sandwich(proc(boot='boot-1'), proc(boot='boot-2'),
                                    's')

    def test_malformed_sandwich_is_harness_error(self):
        with self.assertRaises(mod.HarnessError):
            mod.check_proc_sandwich({}, proc(), 's')
        with self.assertRaises(mod.HarnessError):
            mod.check_proc_sandwich(proc(), {'boot_id': 'x'}, 's')

    def test_sandwich_cli_holds_and_fails(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            snap = proc()
            held = tmp / 'held.json'
            held.write_text(json.dumps(snap))
            code = mod.main(['proc-sandwich', '--before', str(held),
                             '--after', str(held), '--label', 's'])
            self.assertEqual(code, 0)
            swapped = tmp / 'swapped.json'
            swapped.write_text(json.dumps(proc(exe='f' * 64)))
            with self.assertRaises(AssertionError):
                mod.main(['proc-sandwich', '--before', str(held),
                          '--after', str(swapped), '--label', 's'])


class StateOnlyLocked(unittest.TestCase):
    REGIONS = [{'name': 'probe', 'kind': 'texture', 'rect': [8, 8, 16, 16]}]

    def locked_doc(self, locked=True, pointer=None, cursor=None,
                   fullscreen=None, wallpaper=None, motion_count=None,
                   brightness=1.0, wb=True, manual=False, ms=None):
        doc = {'animation_clock_manual': manual, 'animation_clock_ms': ms,
               'motion_policy': 'full', 'locked': locked,
               'pointer_position': pointer, 'cursor': cursor,
               'pointer_fullscreen_blocked': fullscreen,
               'wallpaper_diagnostics': wallpaper,
               'native_relative_motion_count': motion_count,
               'wellbeing_effects': {'desaturation': 0.0,
                                     'brightness': brightness,
                                     'animating': False}}
        if not wb:
            del doc['wellbeing_effects']
        return doc

    def build(self, tmp, before_doc=None, after_doc=None, extra_cap=None):
        blob = tmp / 'bin'
        blob.write_bytes(b'proof-binary')
        snap = proc(exe=mod.digest(blob))
        before = tmp / 'locked-before.json'
        after = tmp / 'locked-after.json'
        before.write_text(json.dumps(
            self.locked_doc() if before_doc is None else before_doc))
        after.write_text(json.dumps(
            self.locked_doc() if after_doc is None else after_doc))
        caps = [{'label': 'take-lock-locked',
                 'cstate_before': str(before), 'cstate_after': str(after),
                 'epoch': time.time(), 'iso': 't', 'proc': snap,
                 'proc_before': json.loads(json.dumps(snap))}]
        if extra_cap:
            caps[0].update(extra_cap)
        binaries = {'compositor': str(blob), 'shell': str(blob),
                    'probe': str(blob)}
        return binaries, caps

    def record(self, tmp, binaries, state_caps, pixel_caps=None):
        return mod.record_provenance(
            'G-TEST', '0ac3ec7bce96', binaries,
            [] if pixel_caps is None else pixel_caps,
            owner(), self.REGIONS, tmp / 'prov.json', time.time() - 1,
            state_captures=state_caps)

    def test_locked_state_only_passes_without_pixels(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            binaries, caps = self.build(tmp)
            receipt = self.record(tmp, binaries, caps)
            self.assertEqual(receipt['captures'], [])
            self.assertEqual(len(receipt['state_only_captures']), 1)
            entry = receipt['state_only_captures'][0]
            self.assertEqual(entry['label'], 'take-lock-locked')
            self.assertEqual(entry['evidence'], 'state-only')
            self.assertEqual(entry['lock_schema_receipt'], 'PENDING')
            self.assertNotIn('image', entry)
            self.assertNotIn('clock_ms', entry)

    def test_lock_schema_receipt_flagged_pending(self):
        self.assertTrue(mod.LOCK_SCHEMA_RECEIPT_PENDING)

    def test_unlocked_state_is_a_verdict_failure(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            binaries, caps = self.build(
                tmp, after_doc=self.locked_doc(locked=False))
            with self.assertRaises(AssertionError):
                self.record(tmp, binaries, caps)

    def test_untyped_locked_is_harness_error(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            binaries, caps = self.build(
                tmp, after_doc=self.locked_doc(locked=1))
            with self.assertRaises(mod.HarnessError):
                self.record(tmp, binaries, caps)

    def test_missing_effects_is_harness_error(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            binaries, caps = self.build(
                tmp, after_doc=self.locked_doc(wb=False))
            with self.assertRaises(mod.HarnessError):
                self.record(tmp, binaries, caps)

    def test_unredacted_pointer_is_harness_error(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            binaries, caps = self.build(
                tmp, after_doc=self.locked_doc(pointer=[10, 10]))
            with self.assertRaises(mod.HarnessError):
                self.record(tmp, binaries, caps)

    def test_unredacted_cursor_is_harness_error(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            binaries, caps = self.build(
                tmp, after_doc=self.locked_doc(cursor={'theme': 'x'}))
            with self.assertRaises(mod.HarnessError):
                self.record(tmp, binaries, caps)

    def test_unredacted_fullscreen_blocked_is_harness_error(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            binaries, caps = self.build(
                tmp, after_doc=self.locked_doc(fullscreen=True))
            with self.assertRaises(mod.HarnessError):
                self.record(tmp, binaries, caps)

    def test_unredacted_wallpaper_is_harness_error(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            binaries, caps = self.build(
                tmp, after_doc=self.locked_doc(
                    wallpaper={'outputs': []}))
            with self.assertRaises(mod.HarnessError):
                self.record(tmp, binaries, caps)

    def test_missing_redaction_key_is_harness_error(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            for key in ('pointer_position', 'cursor',
                        'pointer_fullscreen_blocked',
                        'wallpaper_diagnostics',
                        'native_relative_motion_count'):
                doc = self.locked_doc()
                del doc[key]
                binaries, caps = self.build(tmp, after_doc=doc)
                with self.assertRaises(mod.HarnessError, msg=key):
                    self.record(tmp, binaries, caps)

    def test_leaked_motion_count_is_harness_error(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            binaries, caps = self.build(
                tmp, after_doc=self.locked_doc(motion_count=3))
            with self.assertRaises(mod.HarnessError):
                self.record(tmp, binaries, caps)

    def test_missing_clock_key_is_harness_error(self):
        # A missing ms must not become None and pass under real time.
        for manual, ms in ((False, None), (True, 5)):
            doc = self.locked_doc(manual=manual, ms=ms)
            del doc['animation_clock_ms']
            with self.assertRaises(mod.HarnessError,
                                   msg=f'manual={manual}'):
                mod.require_locked_state(doc, 'locked')
        doc = self.locked_doc()
        del doc['animation_clock_manual']
        with self.assertRaises(mod.HarnessError):
            mod.require_locked_state(doc, 'locked')

    def test_untyped_clock_manual_is_harness_error(self):
        for bad in (1, 0, 'false', None):
            with self.assertRaises(mod.HarnessError,
                                   msg=f'manual={bad!r}'):
                mod.require_locked_state(self.locked_doc(manual=bad),
                                         'locked')

    def test_clock_ms_matches_manual_mode(self):
        # Real time reports a null ms; a manual clock needs an int ms.
        mod.require_locked_state(
            self.locked_doc(manual=False, ms=None), 'locked')
        mod.require_locked_state(
            self.locked_doc(manual=True, ms=5), 'locked')
        for bad_ms in (None, -1, True, 1.5, '5'):
            with self.assertRaises(mod.HarnessError,
                                   msg=f'manual ms={bad_ms!r}'):
                mod.require_locked_state(
                    self.locked_doc(manual=True, ms=bad_ms), 'locked')
        with self.assertRaises(mod.HarnessError):
            mod.require_locked_state(
                self.locked_doc(manual=False, ms=5), 'locked')

    def locked_state_args(self, tmp):
        _, caps = self.build(tmp)
        proc_before = tmp / 'proc-before.json'
        proc_after = tmp / 'proc-after.json'
        proc_before.write_text(json.dumps(caps[0]['proc_before']))
        proc_after.write_text(json.dumps(caps[0]['proc']))
        return ['locked-state',
                '--cstate-before', str(tmp / 'locked-before.json'),
                '--cstate-after', str(tmp / 'locked-after.json'),
                '--proc-before', str(proc_before),
                '--proc-after', str(proc_after),
                '--label', 'take-lock-locked']

    def test_locked_state_cli_observes_strict(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            self.assertEqual(mod.main(self.locked_state_args(tmp)), 0)

    def test_locked_state_cli_rejects_unredacted(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            _, caps = self.build(
                tmp, after_doc=self.locked_doc(pointer=[1, 2]))
            proc_before = tmp / 'proc-before.json'
            proc_after = tmp / 'proc-after.json'
            proc_before.write_text(json.dumps(caps[0]['proc_before']))
            proc_after.write_text(json.dumps(caps[0]['proc']))
            self.assertEqual(mod.main(['locked-state',
                                       '--cstate-before',
                                       str(tmp / 'locked-before.json'),
                                       '--cstate-after',
                                       str(tmp / 'locked-after.json'),
                                       '--proc-before', str(proc_before),
                                       '--proc-after', str(proc_after),
                                       '--label', 'take-lock-locked']), 2)

    def test_locked_state_cli_rejects_unlocked(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            _, caps = self.build(
                tmp, after_doc=self.locked_doc(locked=False))
            proc_before = tmp / 'proc-before.json'
            proc_after = tmp / 'proc-after.json'
            proc_before.write_text(json.dumps(caps[0]['proc_before']))
            proc_after.write_text(json.dumps(caps[0]['proc']))
            with self.assertRaises(AssertionError):
                mod.main(['locked-state',
                          '--cstate-before',
                          str(tmp / 'locked-before.json'),
                          '--cstate-after', str(tmp / 'locked-after.json'),
                          '--proc-before', str(proc_before),
                          '--proc-after', str(proc_after),
                          '--label', 'take-lock-locked'])

    def test_pixel_keys_forbidden_in_state_path(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            binaries, caps = self.build(tmp, extra_cap={'image': 'x.png'})
            with self.assertRaises(mod.HarnessError):
                self.record(tmp, binaries, caps)
            binaries, caps = self.build(tmp, extra_cap={'clock_ms': 3})
            with self.assertRaises(mod.HarnessError):
                self.record(tmp, binaries, caps)

    def test_state_sandwich_break_fails(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            binaries, caps = self.build(tmp)
            caps[0]['proc']['processes']['shell']['start_ticks'] = 424242
            with self.assertRaises(AssertionError):
                self.record(tmp, binaries, caps)

    def test_state_capture_missing_before_snapshot_errors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            binaries, caps = self.build(tmp)
            del caps[0]['proc_before']
            with self.assertRaises(mod.HarnessError):
                self.record(tmp, binaries, caps)

    def test_no_captures_at_all_errors(self):
        with tempfile.TemporaryDirectory() as td:
            tmp = Path(td)
            blob = tmp / 'bin'
            blob.write_bytes(b'proof-binary')
            binaries = {'compositor': str(blob), 'shell': str(blob),
                        'probe': str(blob)}
            with self.assertRaises(mod.HarnessError):
                mod.record_provenance('G-TEST', '0ac3ec7bce96', binaries, [],
                                      owner(), self.REGIONS, tmp / 'prov.json',
                                      time.time() - 1, state_captures=[])


if __name__ == '__main__':
    unittest.main()
