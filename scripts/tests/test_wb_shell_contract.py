#!/usr/bin/env python3
"""Structural contract for the WB shell proof's frozen-clock discipline.

Bracketed captures (owned-interior/verify) require identical typed
manual-clock samples, and real time reports animation_clock_ms null:
every direct bracketed capture site must run under a frozen clock (an
anim_clock freeze with no intervening clock-file removal), and every
freeze must be observed manual before its capture. Ramp/stepped
captures inherit their gate freeze via wb_step_ramp (unchanged) and
are out of scope here. Fail guards must use explicit if blocks: an
`A && B || fail` chain trips CI shellcheck SC2015.
"""
import json
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
PROOF = ROOT / 'scripts' / 'tuna-gtk-shell-proof'

BRACKETED = ['gray-baseline', 'fade-baseline', 'both-baseline',
             'reset-baseline', 'take-fade-baseline', 'take-fade-settled']

RM_CLOCK = 'rm -f "$TUNA_ANIMATION_CLOCK"'


class FrozenClockOrder(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.lines = PROOF.read_text().splitlines()

    def capture_index(self, label):
        found = [i for i, line in enumerate(self.lines)
                 if re.match(r'\s*wb_capture\s+%s\s*$' % label, line)]
        self.assertEqual(len(found), 1, f'{label}: capture site not unique')
        return found[0]

    def nearest_clock_op(self, index):
        # Gate-scoped: a freeze only counts inside the capture's own
        # gate (function bodies above the gate do not execute in order).
        for i in range(index - 1, -1, -1):
            stripped = self.lines[i].strip()
            if re.match(r'anim_clock\s', stripped):
                return ('freeze', i)
            if RM_CLOCK in stripped:
                return ('remove', i)
            if re.match(r'wb_gate_begin\b', stripped):
                return ('gate-start', i)
        return (None, -1)

    def test_bracketed_captures_run_frozen(self):
        for label in BRACKETED:
            with self.subTest(label=label):
                idx = self.capture_index(label)
                op, at = self.nearest_clock_op(idx)
                self.assertEqual(
                    op, 'freeze',
                    f'{label}: nearest clock op is {op} (line {at + 1}); '
                    f'want a freeze with no intervening clock removal')

    def test_freeze_observed_manual_before_capture(self):
        for label in BRACKETED:
            with self.subTest(label=label):
                idx = self.capture_index(label)
                op, at = self.nearest_clock_op(idx)
                self.assertEqual(op, 'freeze')
                window = self.lines[at:idx]
                self.assertTrue(
                    any("cstate '.animation_clock_manual'" in line
                        for line in window),
                    f'{label}: freeze at line {at + 1} never observed manual')


class FailChainShape(unittest.TestCase):
    def test_no_and_or_fail_chains(self):
        text = PROOF.read_text()
        # Join shell continuations into logical lines, keeping any
        # list operator: a line ending in && or || continues below.
        logical = re.sub(r'\\\n[ \t]*', '', text)
        logical = re.sub(r'(\|\||&&)\n[ \t]*', r'\1 ', logical)
        bad = []
        for line in logical.splitlines():
            # Single-quoted operators are not shell lists (awk/jq
            # programs); strip per line so stray apostrophes cannot
            # pair across lines.
            code = re.sub(r"'[^'\n]*'", "''", line)
            # One and-or list only: no separator may intervene.
            if re.search(r'&&[^;\n]*\|\|[^;\n]*\bfail\b', code):
                bad.append(line.strip())
        self.assertEqual(bad, [])


class FadeSettleFilter(unittest.TestCase):
    """The take-fade settle filter must admit the actual f32 0.6 bytes.

    The compositor stores effects as f32, so settled brightness
    serializes as 0.6000000238418579; CI 38399013279 proved exact f64
    equality rejects the settled frame forever (animating false, both
    jobs). The filter below is extracted from the proof itself, not
    copied, and must still reject genuinely unsettled values.
    """
    F32_06 = 0.6000000238418579

    @classmethod
    def setUpClass(cls):
        jq = shutil.which('jq')
        assert jq is not None, 'jq is required (GitHub ubuntu runners ship it)'
        cls.jq = jq
        match = re.search(r"cstate '([^']+)' \"fade settled\"",
                          PROOF.read_text())
        assert match is not None, 'fade-settled cstate line went missing'
        cls.filt = match.group(1)

    _MISSING = object()

    def run_filter(self, brightness, animating=False):
        effects = {'brightness': brightness}
        if animating is not self._MISSING:
            effects['animating'] = animating
        with tempfile.NamedTemporaryFile('w', suffix='.json',
                                         delete=True) as tmp:
            json.dump({'wellbeing_effects': effects}, tmp)
            tmp.flush()
            proc = subprocess.run([self.jq, '-e', self.filt, tmp.name],
                                  capture_output=True, text=True, timeout=60)
        return proc.returncode

    def test_settled_f32_admitted(self):
        self.assertEqual(self.run_filter(self.F32_06, animating=False), 0)

    def test_exact_f64_admitted(self):
        self.assertEqual(self.run_filter(0.6, animating=False), 0)

    def test_live_ramp_rejected(self):
        for value in (0.9, 0.7, 0.61, 0.59, 1.0):
            with self.subTest(brightness=value):
                self.assertNotEqual(
                    self.run_filter(value, animating=False), 0)

    def test_running_ramp_at_target_rejected(self):
        for value in (self.F32_06, 0.6):
            with self.subTest(brightness=value):
                self.assertNotEqual(
                    self.run_filter(value, animating=True), 0)

    def test_missing_animating_rejected(self):
        for value in (self.F32_06, 0.6):
            with self.subTest(brightness=value):
                self.assertNotEqual(
                    self.run_filter(value, animating=self._MISSING), 0)


class LiveActionCapture(unittest.TestCase):
    def test_actual_capture_clock_filter_refuses_live_missing_and_untyped_values(self):
        match = re.search(r"cstate '([^']+)' \"manual capture clock\"", PROOF.read_text())
        self.assertIsNotNone(match)
        good = {'animation_clock_manual': True, 'animation_clock_ms': 42}
        cases = [(good, True)]
        for key, value in [('animation_clock_manual', False), ('animation_clock_manual', 'true'),
                           ('animation_clock_ms', None), ('animation_clock_ms', True),
                           ('animation_clock_ms', '42'), ('animation_clock_ms', -1),
                           ('animation_clock_ms', 42.5)]:
            cases.append((dict(good, **{key: value}), False))
        cases.append(({'animation_clock_manual': True}, False))
        for value, accepted in cases:
            with self.subTest(value=value):
                proc = subprocess.run(['jq', '-e', match.group(1)],
                                      input=json.dumps(value), capture_output=True,
                                      text=True, timeout=5)
                self.assertEqual(proc.returncode == 0, accepted, proc.stderr)

    def test_live_action_sites_use_the_bracketed_capture(self):
        source = PROOF.read_text()
        for label in ('delay-redue', 'skip-redue', 'negative-baseline'):
            self.assertEqual(source.count('wb_capture_after_live_action ' + label), 1)
            self.assertNotRegex(source, re.compile(r'^wb_capture\s+' + re.escape(label) + r'\s*$', re.MULTILINE),
                                'unfrozen live-action capture')
        capture = source.split('wb_capture() {', 1)[1].split('\n}', 1)[0]
        self.assertLess(capture.index('"manual capture clock"'), capture.index('scrot'))
        self.assertIn('(.animation_clock_ms | floor)', capture)

    def test_actual_shell_wrapper_freezes_captures_and_restores(self):
        source = PROOF.read_text()
        match = re.search(r'^wb_capture_after_live_action\(\) \{\n.*?^\}',
                          source, re.MULTILINE | re.DOTALL)
        self.assertIsNotNone(match)
        script = r'''set -eu
TUNA_ANIMATION_CLOCK="$1"
WB_GATE=TEST
anim_clock() { printf '%s\n' "$1" > "$TUNA_ANIMATION_CLOCK"; printf 'freeze\n'; }
cstate() {
    case "$2" in
        "frozen capture clock") test -f "$TUNA_ANIMATION_CLOCK" ;;
        "real clock") test ! -e "$TUNA_ANIMATION_CLOCK" ;;
        *) return 2 ;;
    esac
}
wb_capture() { test -f "$TUNA_ANIMATION_CLOCK"; printf 'capture:%s\n' "$1"; }
fail() { exit 7; }
''' + match.group(0) + '\nwb_capture_after_live_action delay-redue\ntest ! -e "$TUNA_ANIMATION_CLOCK"\nprintf "restored\\n"\n'
        with tempfile.TemporaryDirectory() as directory:
            proc = subprocess.run(['bash', '-c', script, 'capture-test',
                                   str(Path(directory) / 'clock')],
                                  capture_output=True, text=True, timeout=5)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(proc.stdout.splitlines(), ['freeze', 'capture:delay-redue', 'restored'])


class DisableFrameBarrier(unittest.TestCase):
    """Execute the real helper against controlled compositor snapshots.

    A neutral state is published before its framebuffer is submitted. Do not
    let a snapshot of that old frame admit the subsequent single-shot capture.
    """
    @staticmethod
    def state(frame=40, clock=3000, manual=True, brightness=1.0, animating=False):
        return {'rendered_frames': frame, 'animation_clock_ms': clock,
                'animation_clock_manual': manual,
                'wellbeing_effects': {'desaturation': 0.0,
                                     'brightness': brightness,
                                     'animating': animating}}

    def run_helper(self, applied, submitted):
        source = PROOF.read_text()
        match = re.search(r'^wb_await_off\(\) \{\n.*?^\}',
                          source, re.MULTILINE | re.DOTALL)
        self.assertIsNotNone(match)
        script = r'''set -eu
SAMPLES="$1"
CSTATE="$SAMPLES/current.json"
stage=0
cstate() {
    stage=$((stage + 1))
    for sample in "$SAMPLES/$stage-"*.json; do
        cp "$sample" "$CSTATE"
        if jq -e "$1" "$CSTATE" >/dev/null 2>&1; then
            printf 'observed:%s:%s\n' "$stage" "$(basename "$sample")"
            return 0
        fi
    done
    return 1
}
fail() { printf '%s\n' "$2" >&2; exit 7; }
''' + match.group(0) + '\nwb_await_off TEST\nprintf "capture-ready\\n"\n'
        with tempfile.TemporaryDirectory() as directory:
            for stage, samples in enumerate((applied, submitted), 1):
                for index, sample in enumerate(samples):
                    (Path(directory) / f'{stage}-{index}.json').write_text(json.dumps(sample))
            return subprocess.run(['bash', '-c', script, 'off-test', directory],
                                  capture_output=True, text=True, timeout=5)

    def test_neutral_state_waits_for_a_new_submitted_frame(self):
        result = self.run_helper([self.state(brightness=0.6), self.state()],
                                 [self.state(), self.state(frame=41)])
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(),
                         ['observed:1:1-1.json', 'observed:2:2-1.json', 'capture-ready'])

    def test_stalled_counter_never_admits_capture(self):
        result = self.run_helper([self.state()], [self.state(), self.state()])
        self.assertEqual(result.returncode, 7)
        self.assertNotIn('capture-ready', result.stdout)

    def test_fade_out_cannot_settle_without_advancing_the_clock(self):
        result = self.run_helper([self.state(brightness=0.6, animating=True)],
                                 [self.state(frame=41)])
        self.assertEqual(result.returncode, 7)
        self.assertIn('fade-out found', result.stderr)

    def test_later_frame_must_remain_neutral_and_frozen(self):
        candidates = [self.state(frame=41, clock=3001),
                      self.state(frame=41, manual=False),
                      self.state(frame=41, brightness=0.8),
                      self.state(frame=41, animating=True),
                      self.state(frame='41'), self.state(frame=41.5)]
        for candidate in candidates:
            with self.subTest(candidate=candidate):
                result = self.run_helper([self.state()], [candidate])
                self.assertEqual(result.returncode, 7, result.stdout)
                self.assertNotIn('capture-ready', result.stdout)

    def test_applied_snapshot_requires_typed_clock_and_counter(self):
        for key in ('rendered_frames', 'animation_clock_ms'):
            for value in (None, True, '40', -1, 40.5):
                candidate = dict(self.state(), **{key: value})
                with self.subTest(key=key, value=value):
                    result = self.run_helper([candidate], [self.state(frame=41)])
                    self.assertEqual(result.returncode, 7, result.stdout)
                    self.assertIn('typed frozen clock', result.stderr)
        result = self.run_helper([self.state(manual=False)], [self.state(frame=41)])
        self.assertEqual(result.returncode, 7)

    def test_every_immediate_off_capture_uses_the_barrier(self):
        source = PROOF.read_text()
        for label in ('gray', 'fade', 'both'):
            self.assertRegex(source, 'wb_await_off G-WELLBEING-PIXEL-' + label.upper()
                             + r'\nwb_capture ' + label + '-off\n')


if __name__ == '__main__':
    unittest.main()
