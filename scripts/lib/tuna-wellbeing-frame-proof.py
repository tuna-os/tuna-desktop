#!/usr/bin/env python3
"""Wellbeing framebuffer proofs for the nested GTK shell (#354).

Production path only: settings -> tick/deadline -> 0.35 wire ->
compositor FrameColor. Samples the EXISTING deterministic saturated
native test-client region (tuna-cursor-client 320x200 solid 0xff8f3fd0,
same offsets as the native tuna-wellbeing-proof), with owned geometry
from the bound probe owner record and no overlapping layer.

Hardening (w3-354pixel-proof4f):

- owned-region proof: the baseline must show the exact known interior
  RGB within the proven solid-pixel budget (+-1 per channel, the
  cursor-pixels.py convention through this same nested scrot path),
  geometry must match the bound owner on every capture, every capture
  requires overview_progress==0 settled, and no layer may intersect
  the sampled region.
- frame bracketing: every capture carries a CSTATE snapshot taken
  BEFORE and AFTER its scrot; the manual clock reading, the effect
  sample and the owned geometry must be identical across the bracket
  or the capture is discarded and the gate fails.
- provenance: every capture carries a capture-time /proc snapshot
  (exe byte hash + starttime + boot id); the gate requires agreement
  across captures and requires the admitted binary hashes to equal
  the live processes' exe bytes. No path-only hashing.
- neutral control: geometry/image inputs are validated BEFORE any
  effect assertion; malformed inputs raise HarnessError (exit 2,
  broken harness), never a pass and never a pixel verdict.
  Pixel/effect verdicts raise AssertionError (exit 1) as before.

Gatefix hardening (issue #354 stays OPEN): pixel mismatches raise
EffectMismatch (an AssertionError) and ONLY pixel mismatches do; an
achromatic or unowned baseline is an invalid fixture (HarnessError),
and the neutral control validates its saturated baseline and known
ownership before any verdict. Brackets require a full typed finite
clock/effect schema on BOTH states (no None == None). Every capture
carries process snapshots from BEFORE and AFTER its scrot; any
replacement or exe-swap across the sandwich fails. Provenance
receipts are labeled live self-consistency under outer CI build
admission, never standalone attestation. Locked captures travel a
separate state-only + process evidence path (no pixels, no pixel
bracket, no pixel label; state-only labels in gate output and
receipt); unlocked gates keep full bracketing. Locked observation
is strict at capture time (locked-state: full typed locked presence
on BOTH sides plus the full five-key redaction set plus the process
sandwich), not just at provenance close. The lock CSTATE schema
receipt is PENDING (see LOCK_SCHEMA_RECEIPT_PENDING).

Semantics match GNOME 51 source (docs/wellbeing-effect-protocol.md):
gray entry eases DesaturateEffect factor 0->1.0 over 3000 ms
EASE_OUT_QUAD; fade entry eases brightness 1.0->0.6 over 3000 ms
EASE_OUT_QUAD; disable is immediate (no fade-out).

Every gate records source commit, binary hashes, capture timestamps
and animation-clock readings; missing or stale captures fail closed.
Pixel tolerance is fixed at 2 (no widening); logs never qualify.
"""
import argparse
import hashlib
import json
import math
import sys
import time
from pathlib import Path

sys.dont_write_bytecode = True

try:
    from PIL import Image
except ImportError:  # pragma: no cover - proof env always has PIL
    Image = None

TOLERANCE = 2
OWNED_RGB_TOLERANCE = 1
OWNED_RGB = (143, 63, 208)
CHROMA_MIN = 20
RAMP_MS = 3000
MAX_STEP_MS = 1000
CSTATE_EPS = 1e-3
WEIGHTS = (.299, .587, .114)
KINDS = ('texture', 'solid', 'cursor')


class HarnessError(Exception):
    """Malformed geometry/image/capture input: broken harness, not a verdict."""


class EffectMismatch(AssertionError):
    """Pixel verdict: the frame differs from the expected effect (exit 1)."""


ATTESTATION_LABEL = ('live self-consistency '
                     '(outer CI build admission required; '
                     'never standalone attestation)')

# Lock-schema receipt status: PENDING. No exact current-schema locked
# CSTATE receipt exists on disk (archived locked snapshots predate the
# wellbeing_effects/animation_clock keys the current writer always
# emits); the state-only separation below is implemented against the
# source-validated redaction shape and flagged for nested-CI
# observation. See require_locked_state.
LOCK_SCHEMA_RECEIPT_PENDING = True


def digest(path):
    h = hashlib.sha256()
    with Path(path).open('rb') as stream:
        for chunk in iter(lambda: stream.read(65536), b''):
            h.update(chunk)
    return h.hexdigest()


def transform(rgb, desaturation, brightness):
    gray = sum(c * w for c, w in zip(rgb, WEIGHTS))
    return tuple((c + (gray - c) * desaturation) * brightness for c in rgb)


def ease_out_quad(t):
    t = min(1.0, max(0.0, t))
    return 1.0 - (1.0 - t) * (1.0 - t)


def expected_sample(elapsed_ms, duration_ms=RAMP_MS, grayscale=True, fade=True):
    if duration_ms <= 0:
        progress = 1.0
    else:
        progress = ease_out_quad(elapsed_ms / duration_ms)
    if grayscale == 'settled':
        desaturation = 1.0
    elif grayscale:
        desaturation = progress
    else:
        desaturation = 0.0
    if fade == 'settled':
        brightness = 0.6
    elif fade:
        brightness = (1.0 - 0.4 * progress)
    else:
        brightness = 1.0
    return desaturation, brightness


def ramp_animating(elapsed_ms, duration_ms, grayscale, fade):
    gray_live = grayscale is True and elapsed_ms < duration_ms
    fade_live = fade is True and elapsed_ms < duration_ms
    return bool(gray_live or fade_live)


def validate_regions(regions, width, height, required_kinds=frozenset(('texture',))):
    """Validate geometry inputs BEFORE any effect assertion (HarnessError)."""
    if not isinstance(regions, list) or not regions:
        raise HarnessError(f'regions must be a non-empty list, got {regions!r:.80}')
    kinds = set()
    names = []
    for region in regions:
        if not isinstance(region, dict):
            raise HarnessError(f'region must be an object, got {region!r:.80}')
        for key in ('name', 'kind', 'rect'):
            if key not in region:
                raise HarnessError(f'region missing {key}: {region!r:.80}')
        if region['kind'] not in KINDS:
            raise HarnessError(f"unknown region kind {region['kind']!r}")
        rect = region['rect']
        if (not isinstance(rect, list) or len(rect) != 4
                or not all(type(v) is int for v in rect)):
            raise HarnessError(f"region {region['name']!r} rect must be 4 ints: {rect!r:.80}")
        x, y, w, h = rect
        if w < 2 or h < 2 or x < 0 or y < 0:
            raise HarnessError(f"region {region['name']!r} rect is degenerate: {rect!r}")
        if x + w > width or y + h > height:
            raise HarnessError(
                f"region {region['name']!r} rect {rect!r} exceeds image {width}x{height}")
        kinds.add(region['kind'])
        names.append(region['name'])
    if len(set(names)) != len(names):
        raise HarnessError(f'region names must be unique: {names!r}')
    missing = set(required_kinds) - kinds
    if missing:
        raise HarnessError(f'required kinds {sorted(missing)} missing from {sorted(kinds)}')


def open_pair(baseline_path, current_path):
    """Open both images BEFORE any effect assertion (HarnessError)."""
    if Image is None:
        raise HarnessError('PIL is required')
    try:
        baseline = Image.open(baseline_path).convert('RGB')
    except Exception as exc:
        raise HarnessError(f'baseline image unreadable {baseline_path}: {exc}')
    try:
        current = Image.open(current_path).convert('RGB')
    except Exception as exc:
        raise HarnessError(f'current image unreadable {current_path}: {exc}')
    if baseline.size != current.size:
        raise HarnessError(
            f'image size mismatch: baseline {baseline.size} != current {current.size}')
    return baseline, current


def verify_frame(baseline, current, regions, desaturation, brightness,
                 required_kinds=frozenset(('texture',))):
    if Image is None:
        raise HarnessError('PIL is required')
    if not (0 <= desaturation <= 1 and .6 - 1e-9 <= brightness <= 1):
        raise HarnessError(f'effect out of range: {desaturation} / {brightness}')
    if baseline.size != current.size:
        raise HarnessError(
            f'image size mismatch: baseline {baseline.size} != current {current.size}')
    validate_regions(regions, baseline.width, baseline.height, required_kinds)
    results = []
    for region in regions:
        x, y, width, height = region['rect']
        count, max_error, chroma = 0, 0, 0
        for py in range(y, y + height):
            for px in range(x, x + width):
                before = baseline.getpixel((px, py))[:3]
                after = current.getpixel((px, py))[:3]
                chroma = max(chroma, max(before) - min(before))
                if region['kind'] == 'cursor':
                    expected = before
                else:
                    expected = transform(before, desaturation, brightness)
                error = max(abs(a - e) for a, e in zip(after, expected))
                max_error = max(error, max_error)
                if error > TOLERANCE:
                    raise EffectMismatch(
                        (region['name'], px, py, before, after, expected, error))
                count += 1
        if chroma < CHROMA_MIN:
            raise HarnessError(
                f"{region['name']}: neutral fixture cannot prove desaturation "
                f'(baseline chroma {chroma} < {CHROMA_MIN}; invalid fixture)')
        results.append({'name': region['name'], 'kind': region['kind'],
                        'rect': region['rect'], 'pixels': count,
                        'maximum_error': max_error})
    return {'desaturation': desaturation, 'brightness': brightness, 'regions': results}


def check_owned_interior(image, regions, rgb=OWNED_RGB):
    """Prove the sampled region is the live owned probe interior (exact RGB)."""
    if Image is None:
        raise HarnessError('PIL is required')
    validate_regions(regions, image.width, image.height,
                     required_kinds=frozenset(('texture',)))
    if len(rgb) != 3 or not all(type(v) is int and 0 <= v <= 255 for v in rgb):
        raise HarnessError(f'owned RGB must be 3 bytes: {rgb!r}')
    checked, max_error = 0, 0
    for region in regions:
        if region['kind'] != 'texture':
            continue
        x, y, width, height = region['rect']
        for py in range(y, y + height):
            for px in range(x, x + width):
                got = image.getpixel((px, py))[:3]
                error = max(abs(a - b) for a, b in zip(got, rgb))
                max_error = max(error, max_error)
                assert error <= OWNED_RGB_TOLERANCE, \
                    (region['name'], px, py, got, rgb, 'not the owned probe interior')
                checked += 1
    assert checked, 'no texture pixels prove the owned interior'
    return {'rgb': list(rgb), 'pixels': checked, 'maximum_error': max_error}


def load_cstate(path):
    try:
        doc = json.loads(Path(path).read_text())
    except FileNotFoundError:
        raise HarnessError(f'cstate missing: {path}')
    except (OSError, json.JSONDecodeError) as exc:
        raise HarnessError(f'cstate unreadable {path}: {exc}')
    if not isinstance(doc, dict):
        raise HarnessError(f'cstate {path} is not an object')
    wb = doc.get('wellbeing_effects')
    if not isinstance(wb, dict):
        raise HarnessError(f'{path}: wellbeing_effects missing or malformed')
    return doc, wb


def check_cstate_sample(cstate_path, exp_desat, exp_bright, expect_animating):
    doc, wb = load_cstate(cstate_path)
    for key in ('desaturation', 'brightness', 'animating'):
        if key not in wb:
            raise HarnessError(f'{cstate_path}: wellbeing_effects.{key} missing')
    assert abs(wb['desaturation'] - exp_desat) <= CSTATE_EPS, \
        (cstate_path, wb['desaturation'], exp_desat)
    assert abs(wb['brightness'] - exp_bright) <= CSTATE_EPS, \
        (cstate_path, wb['brightness'], exp_bright)
    assert wb['animating'] == expect_animating, (cstate_path, wb['animating'], expect_animating)
    return doc, wb


def region_rects(regions):
    rects = []
    for region in regions or []:
        rect = region.get('rect') if isinstance(region, dict) else None
        if (not isinstance(rect, list) or len(rect) != 4
                or not all(type(v) is int for v in rect)):
            raise HarnessError(f"region rect malformed: {rect!r:.80}")
        if region.get('kind') != 'cursor':
            rects.append(rect)
    if not rects:
        raise HarnessError('no sampled region to exclude layers from')
    return rects


def rects_overlap(a, b):
    return a[0] < b[0] + b[2] and b[0] < a[0] + a[2] and a[1] < b[1] + b[3] and b[1] < a[1] + a[3]


def rect_contains(outer, inner):
    return (outer[0] <= inner[0] and outer[1] <= inner[1]
            and outer[0] + outer[2] >= inner[0] + inner[2]
            and outer[1] + outer[3] >= inner[1] + inner[3])


def check_capture_context(doc, regions, label):
    """Settled overview + actual layer exclusion for the sampled region."""
    assert not doc.get('overview_open'), (label, 'overview open over the probe')
    assert doc.get('overview_progress') == 0, \
        (label, 'overview not settled', doc.get('overview_progress'))
    layers = doc.get('layers')
    if not isinstance(layers, list):
        raise HarnessError(f'{label}: cstate layers missing or malformed')
    rects = region_rects(regions)
    for layer in layers:
        rect = layer.get('rect') if isinstance(layer, dict) else None
        if (not isinstance(rect, list) or len(rect) != 4
                or not all(type(v) is int for v in rect)):
            raise HarnessError(f'{label}: layer rect malformed: {rect!r:.80}')
        for sampled in rects:
            assert not rects_overlap(rect, sampled), \
                (label, 'layer overlaps the sampled region',
                 layer.get('namespace'), rect, sampled)
    return True


def bound_window(doc, owner, label):
    matches = [w for w in doc.get('windows', [])
               if w.get('id') == owner.get('id')
               and w.get('application_window_id') == owner.get('application_window_id')]
    assert len(matches) == 1, \
        (label, f"bound probe identity missing or ambiguous ({len(matches)} matches)")
    window = matches[0]
    assert window.get('x11_window_id') is None, (label, 'bound probe is not native')
    assert window.get('app_id') == owner.get('app_id'), \
        (label, window.get('app_id'), owner.get('app_id'))
    return window


def require_bracket_rect(rect, label):
    """Typed finite nondegenerate geometry (no None, no bools, no 1px husks)."""
    if (not isinstance(rect, list) or len(rect) != 4
            or not all(type(v) is int for v in rect)):
        raise HarnessError(f'{label}: rect must be 4 ints: {rect!r:.80}')
    if rect[2] < 2 or rect[3] < 2:
        raise HarnessError(f'{label}: rect is degenerate: {rect!r}')
    return list(rect)


def require_bracket_state(doc, label, side):
    """Full typed finite clock/effect schema for one bracket side.

    Manual-clock bool, int ms, finite desaturation/brightness and bool
    animating must all be present and typed on BOTH states before any
    mid-bracket comparison; untyped, missing, NaN or infinite fields
    are harness errors (None == None never passes).
    """
    if not isinstance(doc, dict):
        raise HarnessError(f'{label}/{side}: cstate is not an object')
    manual = doc.get('animation_clock_manual')
    if type(manual) is not bool:
        raise HarnessError(
            f'{label}/{side}: animation_clock_manual must be a bool, '
            f'got {manual!r}')
    ms = doc.get('animation_clock_ms')
    if type(ms) is not int or ms < 0:
        raise HarnessError(
            f'{label}/{side}: animation_clock_ms must be a non-negative int, '
            f'got {ms!r}')
    wb = doc.get('wellbeing_effects')
    if not isinstance(wb, dict):
        raise HarnessError(
            f'{label}/{side}: wellbeing_effects missing or malformed')
    for key in ('desaturation', 'brightness'):
        value = wb.get(key)
        if isinstance(value, bool) or not isinstance(value, (int, float)) \
                or not math.isfinite(value):
            raise HarnessError(
                f'{label}/{side}: wellbeing_effects.{key} must be a finite '
                f'number, got {value!r}')
    if type(wb.get('animating')) is not bool:
        raise HarnessError(
            f'{label}/{side}: wellbeing_effects.animating must be a bool, '
            f'got {wb.get("animating")!r}')
    return manual, ms, wb


def check_bracket(before_doc, after_doc, owner, regions, label):
    """Identical manual clock + effect sample + owned geometry, or discard."""
    if not isinstance(owner, dict) or 'id' not in owner or 'rect' not in owner:
        raise HarnessError(f'{label}: bound owner record missing or malformed')
    owner_rect = require_bracket_rect(owner['rect'], f'{label}: bound owner')
    before_manual, before_ms, before_wb = require_bracket_state(
        before_doc, label, 'before')
    after_manual, after_ms, after_wb = require_bracket_state(
        after_doc, label, 'after')
    assert before_manual == after_manual, \
        (label, 'manual clock mode changed mid-bracket',
         before_manual, after_manual)
    assert before_ms == after_ms, \
        (label, 'animation clock changed mid-bracket', before_ms, after_ms)
    for key in ('desaturation', 'brightness', 'animating'):
        assert before_wb[key] == after_wb[key], \
            (label, f'effect sample {key} changed mid-bracket',
             before_wb[key], after_wb[key])
    before_window = bound_window(before_doc, owner, label)
    after_window = bound_window(after_doc, owner, label)
    before_rect = require_bracket_rect(
        before_window.get('rect'), f'{label}/before: owned window')
    after_rect = require_bracket_rect(
        after_window.get('rect'), f'{label}/after: owned window')
    assert before_rect == after_rect, \
        (label, 'owned geometry changed mid-bracket', before_rect, after_rect)
    assert after_rect == owner_rect, \
        (label, 'owned geometry left the bound rect', after_rect, owner_rect)
    for sampled in region_rects(regions):
        if not rect_contains(owner_rect, sampled):
            raise HarnessError(
                f'{label}: sampled region {sampled!r} outside the bound owned window')
    check_capture_context(before_doc, regions, f'{label}/before')
    check_capture_context(after_doc, regions, f'{label}/after')
    return after_doc, after_doc.get('wellbeing_effects')


def check_fresh(path, gate_start_epoch, label):
    p = Path(path)
    assert p.exists(), f'{label}: missing {path}'
    mtime = p.stat().st_mtime
    assert mtime + 1 >= gate_start_epoch, f'{label}: stale {path} (mtime {mtime} < gate {gate_start_epoch})'
    return mtime


def validate_capture(cap):
    if not isinstance(cap, dict):
        raise HarnessError(f'capture must be an object: {cap!r:.120}')
    for key in ('label', 'image', 'cstate_before', 'cstate_after', 'clock_ms',
                'proc', 'proc_before'):
        if key not in cap:
            raise HarnessError(f'capture missing {key}: {cap!r:.120}')
    if type(cap['clock_ms']) is not int:
        raise HarnessError(f"capture {cap.get('label')}: clock_ms must be an int")
    validate_proc_snapshot(cap['proc'], f"{cap.get('label')}/after")
    validate_proc_snapshot(cap['proc_before'], f"{cap.get('label')}/before")


def validate_proc_snapshot(proc, label):
    if not isinstance(proc, dict):
        raise HarnessError(f'{label}: proc snapshot must be an object')
    if 'boot_id' not in proc or not proc['boot_id']:
        raise HarnessError(f'{label}: proc snapshot boot_id missing')
    processes = proc.get('processes')
    if not isinstance(processes, dict) or not processes:
        raise HarnessError(f'{label}: proc snapshot processes missing')
    for name, entry in processes.items():
        if not isinstance(entry, dict):
            raise HarnessError(f'{label}: proc {name} must be an object')
        for key in ('pid', 'exe_sha256', 'start_ticks'):
            if key not in entry:
                raise HarnessError(f'{label}: proc {name} missing {key}')


def proc_identity(pid, label):
    root = Path('/proc') / str(pid)
    try:
        stat = (root / 'stat').read_text().rsplit(')', 1)[1].split()
        exe_bytes = (root / 'exe').read_bytes()
    except FileNotFoundError:
        raise HarnessError(f'{label}: capture-time process {pid} is gone')
    except OSError as exc:
        raise HarnessError(f'{label}: cannot read /proc/{pid}: {exc}')
    return {'pid': pid, 'uid': root.stat().st_uid,
            'start_ticks': int(stat[19]),
            'exe_sha256': hashlib.sha256(exe_bytes).hexdigest()}


def proc_snapshot(pids):
    try:
        boot_id = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
    except OSError as exc:
        raise HarnessError(f'boot id unreadable: {exc}')
    if not boot_id:
        raise HarnessError('boot id empty')
    processes = {}
    for name, pid in pids.items():
        processes[name] = proc_identity(pid, name)
    return {'boot_id': boot_id, 'processes': processes}


def check_proc_agreement(captures):
    first = captures[0]['proc']
    for cap in captures[1:]:
        proc = cap['proc']
        assert proc['boot_id'] == first['boot_id'], \
            (cap['label'], 'boot id changed mid-gate')
        assert set(proc['processes']) == set(first['processes']), \
            (cap['label'], 'capture-time process set changed')
        for name, entry in first['processes'].items():
            live = proc['processes'][name]
            assert live['pid'] == entry['pid'] \
                and live['start_ticks'] == entry['start_ticks'], \
                (cap['label'], f'{name} lifetime changed mid-gate')
            assert live['exe_sha256'] == entry['exe_sha256'], \
                (cap['label'], f'{name} binary bytes changed mid-gate')
    return {'boot_id': first['boot_id'],
            'processes': {name: {'pid': e['pid'], 'start_ticks': e['start_ticks'],
                                 'exe_sha256': e['exe_sha256']}
                          for name, e in first['processes'].items()}}


def check_proc_sandwich(proc_before, proc_after, label):
    """Lifetime sandwich: no replacement or exe-swap across one capture."""
    validate_proc_snapshot(proc_before, f'{label}/before')
    validate_proc_snapshot(proc_after, f'{label}/after')
    assert proc_after['boot_id'] == proc_before['boot_id'], \
        (label, 'boot id changed mid-capture')
    assert set(proc_after['processes']) == set(proc_before['processes']), \
        (label, 'capture-time process set changed mid-capture')
    for name, entry in proc_before['processes'].items():
        live = proc_after['processes'][name]
        assert live['pid'] == entry['pid'] \
            and live['start_ticks'] == entry['start_ticks'], \
            (label, f'{name} lifetime changed mid-capture')
        assert live['exe_sha256'] == entry['exe_sha256'], \
            (label, f'{name} binary bytes changed mid-capture')
    return True


def verify_ramp(baseline_path, captures_path, regions, grayscale, fade,
                start_clock_ms, owner, duration_ms=RAMP_MS, gate_start_epoch=0.0,
                motion_required='full'):
    if Image is None:
        raise HarnessError('PIL is required')
    if not isinstance(owner, dict) or 'id' not in owner or 'rect' not in owner:
        raise HarnessError('ramp requires the bound owner record')
    try:
        captures = json.loads(Path(captures_path).read_text())
    except (OSError, json.JSONDecodeError) as exc:
        raise HarnessError(f'captures unreadable {captures_path}: {exc}')
    if not isinstance(captures, list):
        raise HarnessError(f'captures must be a list: {captures_path}')
    assert len(captures) >= 3, \
        'ramp needs baseline-adjacent entry, intermediate and endpoint captures'
    for cap in captures:
        validate_capture(cap)
        check_proc_sandwich(cap['proc_before'], cap['proc'], cap['label'])
    ordered = sorted(captures, key=lambda c: c['clock_ms'])
    clocks = [c['clock_ms'] for c in ordered]
    assert clocks[0] == start_clock_ms, (clocks[0], start_clock_ms)
    assert clocks[-1] == start_clock_ms + duration_ms, (clocks[-1], start_clock_ms + duration_ms)
    for prev, cur in zip(clocks, clocks[1:]):
        assert 0 < cur - prev <= MAX_STEP_MS, \
            f'clock step {prev}->{cur} skips the {duration_ms}ms ramp'
    mids = [c for c in ordered if start_clock_ms < c['clock_ms'] < start_clock_ms + duration_ms]
    assert mids, 'ramp needs at least one intermediate sample (not just endpoints)'
    assert any(start_clock_ms + 1000 <= c['clock_ms'] <= start_clock_ms + 2000 for c in mids), \
        'ramp needs an intermediate near the 1500ms midpoint'
    baseline, _probe = open_pair(baseline_path, baseline_path)
    check_fresh(baseline_path, gate_start_epoch, 'baseline')
    validate_regions(regions, baseline.width, baseline.height)
    receipts = []
    for cap in ordered:
        elapsed = cap['clock_ms'] - start_clock_ms
        exp_desat, exp_bright = expected_sample(elapsed, duration_ms, grayscale, fade)
        check_fresh(cap['image'], gate_start_epoch, cap['label'])
        check_fresh(cap['cstate_before'], gate_start_epoch, cap['label'])
        check_fresh(cap['cstate_after'], gate_start_epoch, cap['label'])
        before_doc, _ = load_cstate(cap['cstate_before'])
        after_doc, _ = load_cstate(cap['cstate_after'])
        check_bracket(before_doc, after_doc, owner, regions, cap['label'])
        for side, doc in (('before', before_doc), ('after', after_doc)):
            wb = doc.get('wellbeing_effects') or {}
            assert abs(wb['desaturation'] - exp_desat) <= CSTATE_EPS, \
                (cap['label'], side, wb['desaturation'], exp_desat)
            assert abs(wb['brightness'] - exp_bright) <= CSTATE_EPS, \
                (cap['label'], side, wb['brightness'], exp_bright)
            assert wb['animating'] == ramp_animating(elapsed, duration_ms, grayscale, fade), \
                (cap['label'], side, wb['animating'])
            assert doc.get('motion_policy') == motion_required, \
                (cap['label'], side, doc.get('motion_policy'))
            assert doc.get('animation_clock_manual') is True, \
                (cap['label'], side, 'manual clock required')
            assert doc.get('animation_clock_ms') == cap['clock_ms'], \
                (cap['label'], side, doc.get('animation_clock_ms'), cap['clock_ms'])
        with Image.open(cap['image']) as cur_img:
            result = verify_frame(baseline, cur_img.convert('RGB'), regions,
                                  exp_desat, exp_bright)
        result.update({'label': cap['label'], 'clock_ms': cap['clock_ms'],
                       'elapsed_ms': elapsed, 'epoch': cap.get('epoch'),
                       'baseline_sha256': digest(baseline_path),
                       'current_sha256': digest(cap['image']),
                       'cstate_before_sha256': digest(cap['cstate_before']),
                       'cstate_after_sha256': digest(cap['cstate_after'])})
        receipts.append(result)
    return {'grayscale': grayscale, 'fade': fade, 'start_clock_ms': start_clock_ms,
            'duration_ms': duration_ms, 'captures': receipts}


def verify_immediate_off(baseline_path, off_image, cstate_before, cstate_after,
                         regions, off_clock_ms, owner, gate_start_epoch=0.0):
    if Image is None:
        raise HarnessError('PIL is required')
    if not isinstance(owner, dict) or 'id' not in owner or 'rect' not in owner:
        raise HarnessError('immediate-off requires the bound owner record')
    baseline, current = open_pair(baseline_path, off_image)
    validate_regions(regions, baseline.width, baseline.height)
    check_fresh(off_image, gate_start_epoch, 'immediate-off')
    check_fresh(cstate_before, gate_start_epoch, 'immediate-off')
    check_fresh(cstate_after, gate_start_epoch, 'immediate-off')
    before_doc, _ = load_cstate(cstate_before)
    after_doc, _ = load_cstate(cstate_after)
    check_bracket(before_doc, after_doc, owner, regions, 'immediate-off')
    for side, doc in (('before', before_doc), ('after', after_doc)):
        wb = doc.get('wellbeing_effects') or {}
        for key in ('desaturation', 'brightness', 'animating'):
            if key not in wb:
                raise HarnessError(f'immediate-off/{side}: wellbeing_effects.{key} missing')
        assert abs(wb['desaturation'] - 0.0) <= CSTATE_EPS, (side, wb['desaturation'])
        assert abs(wb['brightness'] - 1.0) <= CSTATE_EPS, (side, wb['brightness'])
        assert wb['animating'] is False, (side, wb['animating'])
        assert doc.get('animation_clock_ms') == off_clock_ms, \
            (side, doc.get('animation_clock_ms'), off_clock_ms)
    result = verify_frame(baseline, current, regions, 0.0, 1.0)
    result.update({'baseline_sha256': digest(baseline_path),
                   'current_sha256': digest(off_image),
                   'cstate_before_sha256': digest(cstate_before),
                   'cstate_after_sha256': digest(cstate_after),
                   'clock_ms': off_clock_ms})
    return result


def baseline_chroma(baseline, regions):
    """Strongest channel spread across the sampled baseline pixels."""
    chroma = 0
    for region in regions:
        if region['kind'] == 'cursor':
            continue
        x, y, width, height = region['rect']
        for py in range(y, y + height):
            for px in range(x, x + width):
                before = baseline.getpixel((px, py))[:3]
                chroma = max(chroma, max(before) - min(before))
    return chroma


def negative_neutral_must_fail(baseline_path, neutral_path, regions, desaturation, brightness):
    baseline, current = open_pair(baseline_path, neutral_path)
    validate_regions(regions, baseline.width, baseline.height)
    chroma = baseline_chroma(baseline, regions)
    if chroma < CHROMA_MIN:
        raise HarnessError(
            f'negative control baseline is achromatic (chroma {chroma} < '
            f'{CHROMA_MIN}): invalid fixture, cannot prove the assertion bites')
    try:
        check_owned_interior(baseline, regions)
    except AssertionError as exc:
        raise HarnessError(
            f'negative control baseline is not the known owned interior: {exc}')
    try:
        verify_frame(baseline, current, regions, desaturation, brightness)
    except EffectMismatch:
        return True
    raise AssertionError('negative control FAILED: neutral frame passed the effect assertion')


def validate_state_capture(cap):
    """State-only captures carry no pixels and no pixel bracket."""
    if not isinstance(cap, dict):
        raise HarnessError(f'state capture must be an object: {cap!r:.120}')
    for key in ('label', 'cstate_before', 'cstate_after', 'proc', 'proc_before'):
        if key not in cap:
            raise HarnessError(f'state capture missing {key}: {cap!r:.120}')
    for key in ('image', 'clock_ms'):
        if key in cap:
            raise HarnessError(
                f"state capture {cap.get('label')}: {key!r} is pixel evidence, "
                f'forbidden in the state-only path')
    validate_proc_snapshot(cap['proc'], f"{cap.get('label')}/after")
    validate_proc_snapshot(cap['proc_before'], f"{cap.get('label')}/before")


def require_locked_state(doc, label):
    """Locked CSTATE shape: locked true, typed clock, typed effects, redacted.

    Implemented against the source-validated redaction shape (the
    snapshot emitter always writes animation_clock_manual/ms,
    wellbeing_effects, cursor, pointer_position,
    pointer_fullscreen_blocked, wallpaper_diagnostics and
    native_relative_motion_count; the last five are null while locked,
    and the clock ms is null unless the clock is manual). No exact
    current-schema locked receipt has been observed yet
    (LOCK_SCHEMA_RECEIPT_PENDING): nested-CI observation pending.
    """
    if not isinstance(doc, dict):
        raise HarnessError(f'{label}: cstate is not an object')
    locked = doc.get('locked')
    if type(locked) is not bool:
        raise HarnessError(f'{label}: locked must be an actual bool, got {locked!r}')
    assert locked is True, (label, 'state-only locked capture is not locked')
    for key in ('animation_clock_manual', 'animation_clock_ms'):
        if key not in doc:
            raise HarnessError(
                f'{label}: locked snapshot must carry {key}')
    manual = doc.get('animation_clock_manual')
    if type(manual) is not bool:
        raise HarnessError(
            f'{label}: animation_clock_manual must be a bool, got {manual!r}')
    ms = doc.get('animation_clock_ms')
    if manual:
        if type(ms) is not int or ms < 0:
            raise HarnessError(
                f'{label}: animation_clock_ms must be a non-negative int '
                f'under a manual clock, got {ms!r}')
    elif ms is not None:
        raise HarnessError(
            f'{label}: animation_clock_ms must be null under real time, '
            f'got {ms!r}')
    wb = doc.get('wellbeing_effects')
    if not isinstance(wb, dict):
        raise HarnessError(f'{label}: wellbeing_effects missing or malformed')
    for key in ('desaturation', 'brightness'):
        value = wb.get(key)
        if isinstance(value, bool) or not isinstance(value, (int, float)) \
                or not math.isfinite(value):
            raise HarnessError(
                f'{label}: wellbeing_effects.{key} must be a finite number, '
                f'got {value!r}')
    if type(wb.get('animating')) is not bool:
        raise HarnessError(
            f'{label}: wellbeing_effects.animating must be a bool, '
            f'got {wb.get("animating")!r}')
    for key in ('pointer_position', 'cursor', 'pointer_fullscreen_blocked',
                'wallpaper_diagnostics', 'native_relative_motion_count'):
        if key not in doc:
            raise HarnessError(
                f'{label}: locked snapshot must carry {key} (null while locked)')
        if doc[key] is not None:
            raise HarnessError(
                f'{label}: locked snapshot must redact {key}, got {doc[key]!r:.60}')
    return wb


def record_provenance(gate, source_commit, binaries, captures, owner,
                      regions, output, gate_start_epoch=0.0, extra=None,
                      state_captures=None):
    assert source_commit and len(source_commit) >= 12, 'source commit required'
    if not isinstance(owner, dict) or 'id' not in owner or 'rect' not in owner:
        raise HarnessError('provenance requires the bound owner record')
    if not isinstance(regions, list) or not regions:
        raise HarnessError('provenance requires the sampled regions')
    if not isinstance(binaries, dict) or not binaries:
        raise HarnessError('provenance binaries must be a non-empty object')
    hashes = {}
    for name, path in binaries.items():
        assert Path(path).exists(), f'provenance binary missing: {name}={path}'
        hashes[name] = digest(path)
    assert hashes, 'at least one binary hash required'
    if not isinstance(captures, list):
        raise HarnessError('provenance captures must be a list')
    state_captures = list(state_captures or [])
    combined = captures + state_captures
    if not combined:
        raise HarnessError('provenance needs at least one capture')
    for cap in captures:
        validate_capture(cap)
        check_proc_sandwich(cap['proc_before'], cap['proc'], cap['label'])
    for cap in state_captures:
        validate_state_capture(cap)
        check_proc_sandwich(cap['proc_before'], cap['proc'], cap['label'])
    check_proc_agreement(combined)
    live = combined[0]['proc']['processes']
    for name in ('compositor', 'shell', 'probe'):
        assert name in live, f'capture-time {name} process missing from proc snapshot'
        assert name in hashes, f'admitted {name} binary missing from binaries'
        assert hashes[name] == live[name]['exe_sha256'], \
            (f'admitted {name} binary is not the live process bytes: '
             f'{hashes[name][:12]} != {live[name]["exe_sha256"][:12]}')
    stamped = []
    for cap in captures:
        check_fresh(cap['image'], gate_start_epoch, cap['label'])
        check_fresh(cap['cstate_before'], gate_start_epoch, cap['label'])
        check_fresh(cap['cstate_after'], gate_start_epoch, cap['label'])
        before_doc, _ = load_cstate(cap['cstate_before'])
        after_doc, after_wb = load_cstate(cap['cstate_after'])
        check_bracket(before_doc, after_doc, owner, regions, cap['label'])
        stamped.append({'label': cap['label'], 'image': str(cap['image']),
                        'image_sha256': digest(cap['image']),
                        'cstate_before': str(cap['cstate_before']),
                        'cstate_before_sha256': digest(cap['cstate_before']),
                        'cstate_after': str(cap['cstate_after']),
                        'cstate_after_sha256': digest(cap['cstate_after']),
                        'clock_ms': cap['clock_ms'],
                        'epoch': cap.get('epoch'), 'iso': cap.get('iso'),
                        'animation_clock_manual': after_doc.get('animation_clock_manual'),
                        'animation_clock_ms': after_doc.get('animation_clock_ms'),
                        'wellbeing_effects': after_wb,
                        'motion_policy': after_doc.get('motion_policy'),
                        'proc': cap['proc'],
                        'proc_before': cap['proc_before']})
    state_stamped = []
    for cap in state_captures:
        check_fresh(cap['cstate_before'], gate_start_epoch, cap['label'])
        check_fresh(cap['cstate_after'], gate_start_epoch, cap['label'])
        before_doc, _ = load_cstate(cap['cstate_before'])
        after_doc, after_wb = load_cstate(cap['cstate_after'])
        require_locked_state(before_doc, f"{cap['label']}/before")
        require_locked_state(after_doc, f"{cap['label']}/after")
        state_stamped.append({'label': cap['label'], 'evidence': 'state-only',
                              'lock_schema_receipt': 'PENDING',
                              'cstate_before': str(cap['cstate_before']),
                              'cstate_before_sha256': digest(cap['cstate_before']),
                              'cstate_after': str(cap['cstate_after']),
                              'cstate_after_sha256': digest(cap['cstate_after']),
                              'epoch': cap.get('epoch'), 'iso': cap.get('iso'),
                              'animation_clock_manual':
                                  after_doc.get('animation_clock_manual'),
                              'animation_clock_ms':
                                  after_doc.get('animation_clock_ms'),
                              'wellbeing_effects': after_wb,
                              'motion_policy': after_doc.get('motion_policy'),
                              'proc': cap['proc'],
                              'proc_before': cap['proc_before']})
    receipt = {'gate': gate, 'source_commit': source_commit,
               'attestation': ATTESTATION_LABEL,
               'binary_sha256': hashes, 'gate_start_epoch': gate_start_epoch,
               'recorded_epoch': time.time(), 'captures': stamped,
               'state_only_captures': state_stamped}
    if extra:
        receipt.update(extra)
    Path(output).write_text(json.dumps(receipt, indent=2) + '\n')
    return receipt


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    sub = parser.add_subparsers(dest='command', required=True)

    p = sub.add_parser('verify')
    p.add_argument('--baseline', type=Path, required=True)
    p.add_argument('--current', type=Path, required=True)
    p.add_argument('--regions', type=Path, required=True)
    p.add_argument('--desaturation', type=float, required=True)
    p.add_argument('--brightness', type=float, required=True)
    p.add_argument('--receipt', type=Path, required=True)
    p.add_argument('--cstate-before', type=Path, default=None)
    p.add_argument('--cstate-after', type=Path, default=None)
    p.add_argument('--owner', type=Path, default=None)
    p.add_argument('--gate-start', type=float, default=0.0)

    p = sub.add_parser('owned-interior')
    p.add_argument('--image', type=Path, required=True)
    p.add_argument('--regions', type=Path, required=True)
    p.add_argument('--rgb', default='143,63,208')
    p.add_argument('--receipt', type=Path, required=True)
    p.add_argument('--cstate-before', type=Path, default=None)
    p.add_argument('--cstate-after', type=Path, default=None)
    p.add_argument('--owner', type=Path, default=None)
    p.add_argument('--gate-start', type=float, default=0.0)

    p = sub.add_parser('ramp')
    p.add_argument('--baseline', type=Path, required=True)
    p.add_argument('--captures', type=Path, required=True)
    p.add_argument('--regions', type=Path, required=True)
    p.add_argument('--grayscale', choices=('true', 'false', 'settled'), required=True)
    p.add_argument('--fade', choices=('true', 'false', 'settled'), required=True)
    p.add_argument('--start-clock-ms', type=int, required=True)
    p.add_argument('--owner', type=Path, required=True)
    p.add_argument('--duration-ms', type=int, default=RAMP_MS)
    p.add_argument('--gate-start', type=float, default=0.0)
    p.add_argument('--receipt', type=Path, required=True)

    p = sub.add_parser('immediate-off')
    p.add_argument('--baseline', type=Path, required=True)
    p.add_argument('--current', type=Path, required=True)
    p.add_argument('--cstate-before', type=Path, required=True)
    p.add_argument('--cstate-after', type=Path, required=True)
    p.add_argument('--regions', type=Path, required=True)
    p.add_argument('--owner', type=Path, required=True)
    p.add_argument('--clock-ms', type=int, required=True)
    p.add_argument('--gate-start', type=float, default=0.0)
    p.add_argument('--receipt', type=Path, required=True)

    p = sub.add_parser('negative-neutral')
    p.add_argument('--baseline', type=Path, required=True)
    p.add_argument('--neutral', type=Path, required=True)
    p.add_argument('--regions', type=Path, required=True)
    p.add_argument('--desaturation', type=float, required=True)
    p.add_argument('--brightness', type=float, required=True)

    p = sub.add_parser('proc-snapshot')
    p.add_argument('--probe', type=int, required=True)
    p.add_argument('--compositor', type=int, required=True)
    p.add_argument('--shell', type=int, required=True)

    p = sub.add_parser('proc-sandwich')
    p.add_argument('--before', type=Path, required=True)
    p.add_argument('--after', type=Path, required=True)
    p.add_argument('--label', default='capture')

    p = sub.add_parser('locked-state')
    p.add_argument('--cstate-before', type=Path, required=True)
    p.add_argument('--cstate-after', type=Path, required=True)
    p.add_argument('--proc-before', type=Path, required=True)
    p.add_argument('--proc-after', type=Path, required=True)
    p.add_argument('--label', default='locked')

    p = sub.add_parser('provenance')
    p.add_argument('--gate', required=True)
    p.add_argument('--source-commit', required=True)
    p.add_argument('--binaries', type=Path, required=True)
    p.add_argument('--captures', type=Path, required=True)
    p.add_argument('--owner', type=Path, required=True)
    p.add_argument('--regions', type=Path, required=True)
    p.add_argument('--gate-start', type=float, default=0.0)
    p.add_argument('--output', type=Path, required=True)
    p.add_argument('--state-captures', type=Path, default=None)

    args = parser.parse_args(argv)
    try:
        if args.command == 'verify':
            baseline, current = open_pair(args.baseline, args.current)
            regions = json.loads(args.regions.read_text())
            validate_regions(regions, baseline.width, baseline.height)
            bracket = [args.cstate_before, args.cstate_after, args.owner]
            if any(v is not None for v in bracket):
                if not all(v is not None for v in bracket):
                    raise HarnessError(
                        'verify bracket needs --cstate-before, --cstate-after and --owner together')
                check_fresh(args.current, args.gate_start, 'verify')
                check_fresh(args.cstate_before, args.gate_start, 'verify')
                check_fresh(args.cstate_after, args.gate_start, 'verify')
                before_doc, _ = load_cstate(args.cstate_before)
                after_doc, _ = load_cstate(args.cstate_after)
                check_bracket(before_doc, after_doc,
                              json.loads(args.owner.read_text()), regions, 'verify')
            receipt = verify_frame(baseline, current, regions,
                                   args.desaturation, args.brightness)
            receipt.update({'baseline_sha256': digest(args.baseline),
                            'current_sha256': digest(args.current)})
            args.receipt.write_text(json.dumps(receipt, indent=2) + '\n')
            print(json.dumps(receipt))
        elif args.command == 'owned-interior':
            if Image is None:
                raise HarnessError('PIL is required')
            try:
                with Image.open(args.image) as img:
                    image = img.convert('RGB')
            except Exception as exc:
                raise HarnessError(f'image unreadable {args.image}: {exc}')
            regions = json.loads(args.regions.read_text())
            bracket = [args.cstate_before, args.cstate_after, args.owner]
            if any(v is not None for v in bracket):
                if not all(v is not None for v in bracket):
                    raise HarnessError(
                        'owned-interior bracket needs --cstate-before, '
                        '--cstate-after and --owner together')
                check_fresh(args.image, args.gate_start, 'owned-interior')
                check_fresh(args.cstate_before, args.gate_start, 'owned-interior')
                check_fresh(args.cstate_after, args.gate_start, 'owned-interior')
                before_doc, _ = load_cstate(args.cstate_before)
                after_doc, _ = load_cstate(args.cstate_after)
                check_bracket(before_doc, after_doc,
                              json.loads(args.owner.read_text()),
                              regions, 'owned-interior')
            rgb = tuple(int(v) for v in args.rgb.split(','))
            receipt = check_owned_interior(image, regions, rgb)
            receipt.update({'image_sha256': digest(args.image)})
            args.receipt.write_text(json.dumps(receipt, indent=2) + '\n')
            print(json.dumps(receipt))
        elif args.command == 'ramp':
            def flag(value):
                return {'true': True, 'false': False, 'settled': 'settled'}[value]
            receipt = verify_ramp(args.baseline, args.captures,
                                  json.loads(args.regions.read_text()),
                                  flag(args.grayscale), flag(args.fade),
                                  args.start_clock_ms,
                                  json.loads(args.owner.read_text()),
                                  args.duration_ms, args.gate_start)
            args.receipt.write_text(json.dumps(receipt, indent=2) + '\n')
            print(json.dumps({'captures': len(receipt['captures'])}))
        elif args.command == 'immediate-off':
            receipt = verify_immediate_off(
                args.baseline, args.current, args.cstate_before, args.cstate_after,
                json.loads(args.regions.read_text()), args.clock_ms,
                json.loads(args.owner.read_text()), args.gate_start)
            args.receipt.write_text(json.dumps(receipt, indent=2) + '\n')
            print(json.dumps(receipt))
        elif args.command == 'negative-neutral':
            negative_neutral_must_fail(args.baseline, args.neutral,
                                       json.loads(args.regions.read_text()),
                                       args.desaturation, args.brightness)
            print('negative control passed: neutral frame correctly failed the effect assertion')
        elif args.command == 'proc-snapshot':
            snapshot = proc_snapshot({'probe': args.probe,
                                      'compositor': args.compositor,
                                      'shell': args.shell})
            print(json.dumps(snapshot))
        elif args.command == 'proc-sandwich':
            try:
                before = json.loads(args.before.read_text())
                after = json.loads(args.after.read_text())
            except (OSError, json.JSONDecodeError) as exc:
                raise HarnessError(f'sandwich snapshot unreadable: {exc}')
            check_proc_sandwich(before, after, args.label)
            print(json.dumps({'label': args.label, 'sandwich': 'held'}))
        elif args.command == 'locked-state':
            before_doc, _ = load_cstate(args.cstate_before)
            after_doc, after_wb = load_cstate(args.cstate_after)
            require_locked_state(before_doc, f"{args.label}/before")
            require_locked_state(after_doc, f"{args.label}/after")
            try:
                proc_before = json.loads(args.proc_before.read_text())
                proc_after = json.loads(args.proc_after.read_text())
            except (OSError, json.JSONDecodeError) as exc:
                raise HarnessError(f'sandwich snapshot unreadable: {exc}')
            check_proc_sandwich(proc_before, proc_after, args.label)
            print(json.dumps({'label': args.label, 'evidence': 'state-only',
                              'locked': True,
                              'wellbeing_effects': after_wb,
                              'sandwich': 'held'}))
        elif args.command == 'provenance':
            state_caps = []
            if args.state_captures is not None:
                try:
                    state_caps = json.loads(args.state_captures.read_text())
                except (OSError, json.JSONDecodeError) as exc:
                    raise HarnessError(
                        f'state captures unreadable {args.state_captures}: {exc}')
            receipt = record_provenance(args.gate, args.source_commit,
                                        json.loads(args.binaries.read_text()),
                                        json.loads(args.captures.read_text()),
                                        json.loads(args.owner.read_text()),
                                        json.loads(args.regions.read_text()),
                                        args.output, args.gate_start,
                                        state_captures=state_caps)
            print(json.dumps({'gate': receipt['gate'],
                              'captures': len(receipt['captures']),
                              'state_only_captures':
                                  len(receipt['state_only_captures'])}))
    except HarnessError as exc:
        print(f'harness error: {exc}', file=sys.stderr)
        return 2
    return 0


if __name__ == '__main__':
    sys.exit(main() or 0)
