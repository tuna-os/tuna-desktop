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
        with self.assertRaises(AssertionError):
            mod.verify_frame(solid((80, 80, 80)), solid((48, 48, 48)), regions, 1.0, 0.6)

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
            caps.append({'label': f'c{ms}', 'image': str(png),
                         'cstate_before': str(before), 'cstate_after': str(after),
                         'clock_ms': ms, 'epoch': time.time(), 'proc': proc()})
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
                caps.append({'label': f's{ms}', 'image': str(png),
                             'cstate_before': str(before), 'cstate_after': str(after),
                             'clock_ms': ms, 'epoch': time.time(), 'proc': proc()})
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
                 'proc': snap}]
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


if __name__ == '__main__':
    unittest.main()
